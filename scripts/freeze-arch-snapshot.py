#!/usr/bin/env python3
"""Freeze the Arch package set for a Generation from one dated snapshot (#49).

Implements the acquisition half of ADR-0020. It reads the repository
databases of a dated Arch Linux Archive snapshot, resolves the transitive
closure of ``packages.base``, downloads every archive and detached signature
by exact snapshot URL, checks size, SHA-256 and signature, stores them by
digest and writes the canonical ``flowerly.generation-inputs.v1`` manifest.

The network is touched only by the ``freeze`` and ``keyring-digest``
commands, only with ``FLOWERLY_ARCHIVE_FETCH`` set to the approval string, and
only for https://archive.archlinux.org/repos/ (no redirects are followed, so
a live mirror can never be substituted). The independent check is the Rust
``distro-builder manifest verify-cache --keyring``; this script never decides
that its own output is good.

Trust anchor: ``archlinux-keyring`` from the same snapshot, accepted only if
its SHA-256 equals the digest pinned in the trust policy. A signer must be a
key of that keyring and must not be on the keyring's revoked list.
"""

import argparse
import base64
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request

APPROVAL_ENV = "FLOWERLY_ARCHIVE_FETCH"
APPROVAL = "I_APPROVE_ARCHIVE_ARCHLINUX_ORG_ONLY"
ARCHIVE = "https://archive.archlinux.org/repos/"
REPOSITORIES = ("core", "extra")
ARCHITECTURE = "x86_64"
# A choice of "-" says the image builder satisfies the virtual name itself
# (mkosi runs pacman with --assume-installed initramfs), so nothing is added.
ASSUMED = "-"
POLICY_SCHEMA = "flowerly.arch-trust-policy.v1"
KEYRING_PACKAGE = "archlinux-keyring"
KEYRING_MEMBER = "usr/share/pacman/keyrings/archlinux.gpg"
REVOKED_MEMBER = "usr/share/pacman/keyrings/archlinux-revoked"
FINGERPRINT = re.compile(r"[0-9A-F]{40}")
DATE = re.compile(r"(\d{4})/(\d{2})/(\d{2})")
# Big enough for linux-firmware; anything larger is not part of a base image.
MAX_ARCHIVE = 2 * 1024**3
MAX_DB = 64 * 1024**2


class Refused(Exception):
    """Fail closed: the message says why nothing was produced."""


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


# --- network -------------------------------------------------------------


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def _approved():
    if os.environ.get(APPROVAL_ENV) != APPROVAL:
        raise Refused(
            f"network access to archive.archlinux.org needs {APPROVAL_ENV}={APPROVAL}"
        )


def fetch(url, destination, limit, opener=None):
    """Download one https://archive.archlinux.org/repos/ URL, bounded and
    without redirects; returns its SHA-256."""
    _approved()
    if not url.startswith(ARCHIVE) or "?" in url or "#" in url or ".." in url:
        raise Refused(f"refusing to fetch {url}: not an exact snapshot URL")
    opener = opener or urllib.request.build_opener(_NoRedirect)
    digest = hashlib.sha256()
    size = 0
    try:
        with opener.open(url, timeout=120) as reply, open(destination, "wb") as out:
            if reply.status != 200:
                raise Refused(f"{url}: HTTP {reply.status}")
            for block in iter(lambda: reply.read(1 << 20), b""):
                size += len(block)
                if size > limit:
                    raise Refused(f"{url}: larger than {limit} bytes")
                digest.update(block)
                out.write(block)
    except urllib.error.HTTPError as error:
        raise Refused(f"{url}: HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise Refused(f"{url}: {error.reason}") from error
    return digest.hexdigest()


# --- repository databases --------------------------------------------------


def parse_desc(text):
    """The ``%KEY%`` sections of one pacman ``desc`` file."""
    sections, key = {}, None
    for line in text.splitlines():
        if line.startswith("%") and line.endswith("%") and len(line) > 2:
            key = line[1:-1]
            sections[key] = []
        elif line and key is not None:
            sections[key].append(line)
    return sections


def read_database(path, repository):
    """Every package of one ``.db`` as a dict; later duplicates are refused."""
    packages = {}
    with tarfile.open(path, "r:*") as archive:
        for member in archive:
            if not member.isfile() or not member.name.endswith("/desc"):
                continue
            if member.size > 1 << 20:
                raise Refused(f"{path}: oversized desc {member.name}")
            desc = parse_desc(
                archive.extractfile(member).read().decode("utf-8", errors="replace")
            )
            try:
                entry = {
                    "repository": repository,
                    "name": desc["NAME"][0],
                    "version": desc["VERSION"][0],
                    "architecture": desc["ARCH"][0],
                    "filename": desc["FILENAME"][0],
                    "size": int(desc["CSIZE"][0]),
                    "sha256": desc["SHA256SUM"][0],
                    "pgpsig": desc.get("PGPSIG", [""])[0],
                    "depends": desc.get("DEPENDS", []),
                    "provides": desc.get("PROVIDES", []),
                }
            except (KeyError, IndexError, ValueError) as error:
                raise Refused(f"{path}: incomplete desc {member.name}: {error}")
            if entry["name"] in packages:
                raise Refused(f"{path}: {entry['name']} appears twice")
            packages[entry["name"]] = entry
    return packages


def _bare(dependency):
    return re.split(r"[<>=]", dependency, maxsplit=1)[0]


def resolve(requested, packages, choices=None):
    """Transitive closure of ``requested`` over DEPENDS.

    A name resolves to the package of that name; otherwise to a provider.
    Several providers are never guessed: an already-selected provider wins,
    then a reviewed entry of ``choices`` (virtual name -> package, or ``-`` for
    a name the image builder provides itself), else the
    candidate must be unique, else resolution is refused so a person decides."""
    choices = choices or {}
    providers = {}
    for package in packages.values():
        for provided in package["provides"]:
            providers.setdefault(_bare(provided), set()).add(package["name"])
    chosen, queue = {}, list(requested)
    while queue:
        name = queue.pop(0)
        if name in chosen:
            continue
        if name in packages:
            package = packages[name]
        else:
            candidates = providers.get(name, set())
            selected = sorted(candidates & chosen.keys())
            if selected:
                continue
            if choices.get(name) == ASSUMED:
                continue
            if name in choices:
                if choices[name] not in candidates:
                    raise Refused(f"choice {choices[name]!r} does not provide {name!r}")
                candidates = {choices[name]}
            if len(candidates) != 1:
                raise Refused(
                    f"cannot resolve {name!r}: providers {sorted(candidates)}; "
                    "add a reviewed choice to the provider-choices file"
                )
            package = packages[next(iter(candidates))]
            if package["name"] in chosen:
                continue
        chosen[package["name"]] = package
        queue.extend(_bare(dependency) for dependency in package["depends"])
    return chosen


def read_requested(path):
    names = [line.strip() for line in pathlib.Path(path).read_text().splitlines()]
    names = [name for name in names if name and not name.startswith("#")]
    if names != sorted(set(names)):
        raise Refused(f"{path} must be sorted and unique")
    return names


# --- trust ---------------------------------------------------------------


def read_choices(path):
    """``virtual-name = package`` lines; the file is reviewed like code."""
    choices = {}
    for number, line in enumerate(pathlib.Path(path).read_text().splitlines(), 1):
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        virtual, separator, package = (part.strip() for part in line.partition("="))
        if not separator or not virtual or not package or virtual in choices:
            raise Refused(f"{path}:{number}: expected one 'virtual = package' per name")
        choices[virtual] = package
    return choices


def load_policy(path):
    policy = json.loads(pathlib.Path(path).read_text())
    expected = {"schema", "snapshot_date", "keyring"}
    if set(policy) != expected or policy["schema"] != POLICY_SCHEMA:
        raise Refused(f"{path}: not a {POLICY_SCHEMA} policy")
    keyring = policy["keyring"]
    if set(keyring) != {"name", "version", "filename", "sha256"} or not re.fullmatch(
        r"[0-9a-f]{64}", keyring["sha256"]
    ):
        raise Refused(f"{path}: malformed keyring pin")
    if keyring["name"] != KEYRING_PACKAGE:
        raise Refused(f"{path}: the pinned keyring must be {KEYRING_PACKAGE}")
    return policy


def extract_keyring(package, workdir):
    """Binary keyring and the revoked fingerprints out of the keyring
    package, using the system ``tar`` and ``gpg`` with a throwaway home."""
    armored = subprocess.run(
        ["tar", "--zstd", "-xOf", str(package), KEYRING_MEMBER],
        check=True,
        capture_output=True,
        env={"PATH": "/usr/bin:/bin"},
    ).stdout
    revoked_text = subprocess.run(
        ["tar", "--zstd", "-xOf", str(package), REVOKED_MEMBER],
        check=True,
        capture_output=True,
        env={"PATH": "/usr/bin:/bin"},
    ).stdout.decode()
    home = tempfile.mkdtemp(prefix="flowerly-gpg-", dir=workdir)
    binary = subprocess.run(
        ["gpg", "--homedir", home, "--batch", "--dearmor"],
        input=armored,
        check=True,
        capture_output=True,
        env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
    ).stdout
    revoked = {
        token.upper()
        for token in revoked_text.split()
        if FINGERPRINT.fullmatch(token.upper())
    }
    return binary, revoked


def verify_signature(keyring, signature, data, revoked):
    """Primary fingerprint of the signer if gpgv accepts, else Refused."""
    with tempfile.TemporaryDirectory(prefix="flowerly-gpgv-") as home:
        run = subprocess.run(
            [
                "gpgv",
                "--homedir",
                home,
                "--keyring",
                str(keyring),
                "--status-fd",
                "1",
                "--",
                str(signature),
                str(data),
            ],
            check=False,
            capture_output=True,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
        )
    records = [
        line[len("[GNUPG:] ") :].split(" ")
        for line in run.stdout.decode("utf-8", errors="replace").splitlines()
        if line.startswith("[GNUPG:] ")
    ]
    keywords = [record[0] for record in records]
    bad = {
        "BADSIG",
        "ERRSIG",
        "EXPSIG",
        "EXPKEYSIG",
        "REVKEYSIG",
        "NO_PUBKEY",
        "KEYEXPIRED",
        "KEYREVOKED",
        "SIGEXPIRED",
        "FAILURE",
        "ERROR",
    }
    if run.returncode != 0 or bad & set(keywords) or keywords.count("VALIDSIG") != 1:
        raise Refused(
            f"signature rejected by gpgv ({run.returncode}): {sorted(bad & set(keywords))}"
        )
    valid = next(record for record in records if record[0] == "VALIDSIG")
    primary = valid[-1].upper()
    if not FINGERPRINT.fullmatch(primary):
        raise Refused("gpgv gave no primary fingerprint")
    if primary in revoked or valid[1].upper() in revoked:
        raise Refused(f"signer {primary} is on the keyring's revoked list")
    return primary


# --- freezing ------------------------------------------------------------


def snapshot_base(date):
    if not DATE.fullmatch(date):
        raise Refused("--date must be YYYY/MM/DD")
    return f"{ARCHIVE}{date}"


def archive_url(date, repository, filename):
    return f"{snapshot_base(date)}/{repository}/os/{ARCHITECTURE}/{filename}"


def store(cache, source, digest):
    target = pathlib.Path(cache) / "sha256" / digest
    target.parent.mkdir(parents=True, exist_ok=True)
    if not target.exists():
        target.write_bytes(pathlib.Path(source).read_bytes())
    return target


def build_manifest(date, requested, frozen):
    return {
        "schema": "flowerly.generation-inputs.v1",
        "source": {
            "snapshot_url": snapshot_base(date) + "/",
            "snapshot_date": date.replace("/", "-"),
            "architecture": ARCHITECTURE,
        },
        "requested": requested,
        "packages": sorted(
            frozen,
            key=lambda p: (
                p["repository"],
                p["architecture"],
                p["name"],
                p["version"],
                p["sha256"],
            ),
        ),
    }


def canonical(manifest):
    return (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()


def freeze(args, fetcher=fetch):
    policy = load_policy(args.policy)
    date = args.date
    if policy["snapshot_date"] != date:
        raise Refused(f"the trust policy is for {policy['snapshot_date']}, not {date}")
    requested = read_requested(args.requested)
    cache = pathlib.Path(args.cache)
    cache.mkdir(parents=True, exist_ok=True)
    evidence = {"snapshot": date, "databases": {}}
    with tempfile.TemporaryDirectory(prefix="flowerly-freeze-") as work:
        work = pathlib.Path(work)
        packages = {}
        for repository in REPOSITORIES:
            path = work / f"{repository}.db"
            digest = fetcher(
                archive_url(date, repository, f"{repository}.db"), path, MAX_DB
            )
            evidence["databases"][repository] = digest
            for name, entry in read_database(path, repository).items():
                if name in packages:
                    raise Refused(
                        f"{name} is in both {packages[name]['repository']} and {repository}"
                    )
                packages[name] = entry
        keyring_entry = packages.get(KEYRING_PACKAGE)
        pin = policy["keyring"]
        if not keyring_entry or (
            keyring_entry["version"],
            keyring_entry["filename"],
        ) != (pin["version"], pin["filename"]):
            raise Refused("the snapshot's archlinux-keyring is not the pinned version")
        package = work / "keyring.pkg"
        digest = fetcher(
            archive_url(date, keyring_entry["repository"], keyring_entry["filename"]),
            package,
            MAX_ARCHIVE,
        )
        if digest != pin["sha256"] or keyring_entry["sha256"] != pin["sha256"]:
            raise Refused(
                "the keyring package does not match the digest pinned in the trust policy"
            )
        binary, revoked = extract_keyring(package, work)
        keyring = work / "keyring.bin"
        keyring.write_bytes(binary)
        evidence["keyring_sha256"] = hashlib.sha256(binary).hexdigest()
        evidence["revoked_fingerprints"] = len(revoked)

        selected = resolve(requested, packages, read_choices(args.choices))
        selected[KEYRING_PACKAGE] = keyring_entry
        frozen = []
        for name in sorted(selected):
            entry = selected[name]
            if entry["architecture"] not in ("any", ARCHITECTURE):
                raise Refused(
                    f"{name}: unexpected architecture {entry['architecture']}"
                )
            url = archive_url(date, entry["repository"], entry["filename"])
            archive_path, signature_path = work / "pkg", work / "pkg.sig"
            if name == KEYRING_PACKAGE:
                archive_path = package
            else:
                digest = fetcher(url, archive_path, MAX_ARCHIVE)
                if (
                    digest != entry["sha256"]
                    or archive_path.stat().st_size != entry["size"]
                ):
                    raise Refused(
                        f"{name}: archive does not match the repository database"
                    )
            signature_digest = fetcher(url + ".sig", signature_path, 1 << 20)
            if (
                entry["pgpsig"]
                and base64.b64decode(entry["pgpsig"]) != signature_path.read_bytes()
            ):
                raise Refused(
                    f"{name}: detached signature differs from the database's PGPSIG"
                )
            signer = verify_signature(keyring, signature_path, archive_path, revoked)
            store(cache, archive_path, entry["sha256"])
            store(cache, signature_path, signature_digest)
            frozen.append(
                {
                    "repository": entry["repository"],
                    "name": name,
                    "version": entry["version"],
                    "architecture": entry["architecture"],
                    "filename": entry["filename"],
                    "size": entry["size"],
                    "sha256": entry["sha256"],
                    "signature_sha256": signature_digest,
                    "signer_fingerprint": signer,
                }
            )
            print(f"frozen {name} {entry['version']}", file=sys.stderr)
        store(cache, keyring, evidence["keyring_sha256"])
    manifest = canonical(build_manifest(date, requested, frozen))
    pathlib.Path(args.out).write_bytes(manifest)
    evidence["manifest_sha256"] = hashlib.sha256(manifest).hexdigest()
    evidence["packages"] = len(frozen)
    pathlib.Path(args.evidence).write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n"
    )
    return evidence


def keyring_digest(args, fetcher=fetch):
    """Print the pin for a snapshot; a person reviews it into the policy."""
    date = args.date
    with tempfile.TemporaryDirectory(prefix="flowerly-freeze-") as work:
        db = pathlib.Path(work) / "core.db"
        fetcher(archive_url(date, "core", "core.db"), db, MAX_DB)
        entry = read_database(db, "core")[KEYRING_PACKAGE]
    print(
        json.dumps(
            {
                "schema": POLICY_SCHEMA,
                "snapshot_date": date,
                "keyring": {
                    "name": KEYRING_PACKAGE,
                    "version": entry["version"],
                    "filename": entry["filename"],
                    "sha256": entry["sha256"],
                },
            },
            indent=2,
            sort_keys=True,
        )
    )


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    pin = sub.add_parser("keyring-digest", help="print the keyring pin for a snapshot")
    pin.add_argument("--date", required=True)
    run = sub.add_parser("freeze", help="download, verify and write the manifest")
    run.add_argument("--date", required=True)
    run.add_argument("--policy", required=True)
    run.add_argument("--requested", required=True)
    run.add_argument("--choices", required=True)
    run.add_argument("--cache", required=True)
    run.add_argument("--out", required=True)
    run.add_argument("--evidence", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "keyring-digest":
            keyring_digest(args)
        else:
            freeze(args)
    except Refused as error:
        print(f"refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
