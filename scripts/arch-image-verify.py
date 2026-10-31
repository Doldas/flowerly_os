#!/usr/bin/env python3
"""Arch cloud image signature and digest verification (#263, ADR-0022).

    keyring --lock <arch-image.json> --keyring <file>
    verify  --lock <arch-image.json> --keyring <file> --image <qcow2> --signature <sig> --out <json>

Trust comes from the lock, never from GnuPG's trust database, a keyserver
or the words "Good signature". The lock pins one primary key
(`primary_key_fingerprint`) and the subkeys allowed to sign images with it
(`signing_subkey_fingerprints`). Arch's arch-boxes key is a certify-only
primary with a signing subkey, so the signer is a subkey, not the primary.

`keyring` checks the keyring file alone (no image needed): imported into a
fresh, throwaway GnuPG home, its colon listing must hold exactly one primary
key, the pinned one, usable (not revoked, expired, invalid or disabled),
holding every approved signing subkey, each signing-capable and usable. It
may hold other subkeys of that primary. No other primary key and no secret
key material is accepted. Run before anything is downloaded.

`verify` additionally:
- hashes the image and requires the pinned SHA-256;
- verifies the detached signature with GnuPG, reading only its
  machine-readable status (`--status-file`), never its human-readable
  output: exactly one signature, exactly one GOODSIG and one well-formed
  VALIDSIG naming an approved subkey of the pinned primary as signer, the
  GOODSIG key id agreeing with it, a binary-document signature with an
  acceptable hash, not dated in the future; any BADSIG, ERRSIG, EXPSIG,
  EXPKEYSIG, REVKEYSIG, KEYEXPIRED, KEYREVOKED, SIGEXPIRED, NO_PUBKEY or
  FAILURE status rejects it;
- writes the facts (fingerprints, digests, outcome, problems; never key
  material) to --out. Exit 0 only when every check holds.

`remote-provider-kvm-evidence.py` re-derives E3 from these facts.
"""
import argparse
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time

FPR = re.compile(r"^[0-9A-F]{40}$")
SHA64 = re.compile(r"^[0-9a-f]{64}$")
# Colon-listing record types GnuPG documents (doc/DETAILS). Anything else is
# output this parser does not understand, so the listing is rejected.
KNOWN_RECORDS = {"pub", "sub", "fpr", "fp2", "grp", "uid", "uat", "sig", "rev", "rvk", "rvs", "tru",
                 "spk", "tfs", "pkd", "cfg"}
SECRET_RECORDS = {"sec", "ssb", "crt", "crs"}
UNUSABLE_VALIDITY = {"i": "invalid", "d": "disabled", "r": "revoked", "e": "expired"}
# Status keywords that reject a verification outright, whatever else says.
REJECTING_STATUS = {"BADSIG", "ERRSIG", "EXPSIG", "EXPKEYSIG", "REVKEYSIG", "KEYEXPIRED", "KEYREVOKED",
                    "SIGEXPIRED", "NO_PUBKEY", "FAILURE", "BADARMOR", "NODATA", "UNEXPECTED",
                    "DECRYPTION_FAILED", "ERROR"}
# OpenPGP hash algorithm ids accepted for an image signature: SHA-2 and SHA-3.
ACCEPTED_HASHES = {8, 9, 10, 11, 12, 14}
FUTURE_SKEW_SECONDS = 300


class VerifyError(Exception):
    pass


# --- parsers (pure, unit-tested) -------------------------------------------------

def parse_keyring(listing):
    """GnuPG `--with-colons --with-subkey-fingerprints` listing -> list of
    primaries: {"fingerprint", "validity", "capabilities", "algorithm",
    "length", "subkeys": [{...same...}]}. Raises VerifyError on anything
    malformed or ambiguous: a fingerprint not directly after its key, a key
    without exactly one fingerprint, a subkey before any primary, duplicate
    fingerprints, secret key records or unknown record types."""
    primaries, current, expect_fpr, seen = [], None, None, set()
    for number, line in enumerate(listing.splitlines(), 1):
        if not line:
            continue
        fields = line.split(":")
        kind = fields[0]
        if kind in SECRET_RECORDS:
            raise VerifyError(f"line {number}: secret key material ({kind}) in the keyring")
        if kind not in KNOWN_RECORDS:
            raise VerifyError(f"line {number}: unknown record type {kind!r}")
        if expect_fpr is not None and kind != "fpr":
            raise VerifyError(f"line {number}: {expect_fpr['kind']} without a fingerprint")
        if kind in ("pub", "sub"):
            if len(fields) < 12:
                raise VerifyError(f"line {number}: truncated {kind} record")
            if not fields[2].isdigit() or not fields[3].isdigit():
                raise VerifyError(f"line {number}: malformed {kind} record")
            key = {"kind": kind, "fingerprint": None, "validity": fields[1], "length": int(fields[2]),
                   "algorithm": int(fields[3]), "keyid": fields[4], "capabilities": fields[11]}
            if kind == "pub":
                key["subkeys"] = []
                primaries.append(key)
                current = key
            else:
                if current is None:
                    raise VerifyError(f"line {number}: subkey before any primary key")
                current["subkeys"].append(key)
            expect_fpr = key
        elif kind == "fpr":
            if expect_fpr is None:
                raise VerifyError(f"line {number}: fingerprint not directly after a key")
            if len(fields) < 10 or not FPR.match(fields[9]):
                raise VerifyError(f"line {number}: fingerprint is not 40 uppercase hex")
            if fields[9] in seen:
                raise VerifyError(f"line {number}: duplicate fingerprint {fields[9]}")
            if not fields[9].endswith(expect_fpr["keyid"].upper()):
                raise VerifyError(f"line {number}: fingerprint does not match its key id")
            seen.add(fields[9])
            expect_fpr["fingerprint"] = fields[9]
            expect_fpr = None
    if expect_fpr is not None:
        raise VerifyError(f"{expect_fpr['kind']} without a fingerprint at end of listing")
    for key in primaries:
        key.pop("kind")
        for sub in key["subkeys"]:
            sub.pop("kind")
    return primaries


def key_problems(key, what):
    problems = []
    if key["validity"] in UNUSABLE_VALIDITY:
        problems.append(f"{what} {key['fingerprint']} is {UNUSABLE_VALIDITY[key['validity']]}")
    if key["algorithm"] in (1, 2, 3) and key["length"] < 2048:
        problems.append(f"{what} {key['fingerprint']} is RSA shorter than 2048 bits")
    if key["algorithm"] in (16, 17, 20):
        problems.append(f"{what} {key['fingerprint']} uses a deprecated algorithm ({key['algorithm']})")
    return problems


def check_keyring(primaries, primary_fpr, approved_subkeys):
    """Problems with the keyring's structure against the lock ([] = ok)."""
    problems = []
    if not FPR.match(primary_fpr or ""):
        return ["lock primary_key_fingerprint is not 40 uppercase hex"]
    if not approved_subkeys or not all(FPR.match(s or "") for s in approved_subkeys) \
            or len(set(approved_subkeys)) != len(approved_subkeys):
        return ["lock signing_subkey_fingerprints must be distinct 40 uppercase hex fingerprints"]
    if primary_fpr in approved_subkeys:
        return ["lock lists the primary key as a signing subkey"]
    if len(primaries) != 1:
        return [f"keyring must hold exactly one primary key, holds {len(primaries)}: "
                + ", ".join(p["fingerprint"] for p in primaries)]
    primary = primaries[0]
    if primary["fingerprint"] != primary_fpr:
        return [f"keyring primary {primary['fingerprint']} is not the pinned primary {primary_fpr}"]
    problems += key_problems(primary, "primary key")
    subkeys = {s["fingerprint"]: s for s in primary["subkeys"]}
    for fpr in approved_subkeys:
        sub = subkeys.get(fpr)
        if sub is None:
            problems.append(f"approved signing subkey {fpr} is not a subkey of the pinned primary")
            continue
        problems += key_problems(sub, "approved signing subkey")
        if "s" not in sub["capabilities"].lower():
            problems.append(f"approved signing subkey {fpr} is not signing-capable")
    return problems


def parse_status(text):
    """GnuPG status lines -> list of (keyword, [args]). Raises VerifyError
    on a line that is not a status line: the status channel carries
    nothing else."""
    out = []
    for number, line in enumerate(text.splitlines(), 1):
        if not line.startswith("[GNUPG:] "):
            raise VerifyError(f"status line {number} is not a GnuPG status line")
        parts = line[len("[GNUPG:] "):].split(" ")
        if not parts[0] or not re.fullmatch(r"[A-Z_0-9]+", parts[0]):
            raise VerifyError(f"status line {number} has no keyword")
        out.append((parts[0], parts[1:]))
    return out


def parse_validsig(args):
    """VALIDSIG <fpr> <date> <sig-ts> <expire-ts> <version> <reserved>
    <pubkey-algo> <hash-algo> <sig-class> <primary-fpr>."""
    if len(args) != 10:
        raise VerifyError(f"VALIDSIG has {len(args)} fields, expected 10")
    fpr, date, created, expires, version, _reserved, pk_algo, hash_algo, sig_class, primary = args
    if not FPR.match(fpr) or not FPR.match(primary):
        raise VerifyError("VALIDSIG fingerprints are not 40 uppercase hex")
    if not created.isdigit() or not (expires.isdigit()) or not pk_algo.isdigit() or not hash_algo.isdigit() \
            or not version.isdigit() or not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date) \
            or not re.fullmatch(r"[0-9a-fA-F]{2}", sig_class):
        raise VerifyError("VALIDSIG fields are malformed")
    return {"signing_fingerprint": fpr, "primary_fingerprint": primary, "created": int(created),
            "expires": int(expires), "version": int(version), "pubkey_algorithm": int(pk_algo),
            "hash_algorithm": int(hash_algo), "sig_class": sig_class.lower()}


def check_status(records, primary_fpr, approved_subkeys, keyring_subkeys, now=None):
    """(validsig or None, problems) for one detached-signature verification."""
    now = time.time() if now is None else now
    keywords = [k for k, _ in records]
    problems = sorted({f"GnuPG reported {k}" for k in keywords if k in REJECTING_STATUS})
    count = {k: keywords.count(k) for k in ("NEWSIG", "GOODSIG", "VALIDSIG")}
    for keyword, n in count.items():
        if n != 1:
            problems.append(f"expected exactly one {keyword}, got {n}")
    validsig = None
    validsigs = [args for k, args in records if k == "VALIDSIG"]
    if len(validsigs) == 1:
        try:
            validsig = parse_validsig(validsigs[0])
        except VerifyError as err:
            problems.append(str(err))
    if validsig:
        signer, primary = validsig["signing_fingerprint"], validsig["primary_fingerprint"]
        if primary != primary_fpr:
            problems.append(f"signature's primary key {primary} is not the pinned primary {primary_fpr}")
        if signer not in approved_subkeys:
            problems.append(f"signature made by {signer}, which is not an approved signing subkey")
        if signer not in keyring_subkeys:
            problems.append(f"signer {signer} is not a subkey of the pinned primary in the keyring")
        if validsig["sig_class"] != "00":
            problems.append(f"signature class {validsig['sig_class']} is not a binary document signature")
        if validsig["hash_algorithm"] not in ACCEPTED_HASHES:
            problems.append(f"signature hash algorithm {validsig['hash_algorithm']} is not accepted")
        if validsig["created"] > now + FUTURE_SKEW_SECONDS:
            problems.append("signature is dated in the future")
        if validsig["expires"] and validsig["expires"] <= now:
            problems.append("signature has expired")
        goodsigs = [args for k, args in records if k == "GOODSIG"]
        if len(goodsigs) == 1 and (not goodsigs[0] or goodsigs[0][0].upper() != signer[-16:]):
            problems.append("GOODSIG key id does not match VALIDSIG's signer")
    return validsig, problems


# --- GnuPG (isolated, throwaway home) --------------------------------------------

class Gpg:
    """A fresh GnuPG home holding only the given keyring. No keyserver, no
    auto-retrieval, no options file, no agent. Use as a context manager.
    GnuPG's own trust model is left at its default and never consulted:
    `--trust-model always` makes gpg 2.4.4 skip creating the trustdb and
    then abort `--show-keys` on a revoked key."""

    def __init__(self, keyring, gpg="gpg"):
        self.keyring, self.gpg = pathlib.Path(keyring), shutil.which(gpg) or gpg

    def __enter__(self):
        self.home = pathlib.Path(tempfile.mkdtemp(prefix="flowerly-gpg-"))
        self.home.chmod(0o700)
        try:
            # Look before importing: secret material is refused, not imported.
            shown = self.run("--with-colons", "--fixed-list-mode", "--show-keys", "--", str(self.keyring))
            if shown.returncode != 0:
                raise VerifyError(f"keyring {self.keyring} is not a readable OpenPGP key file")
            parse_keyring(shown.stdout)
            result = self.run("--import", "--import-options", "import-minimal", "--", str(self.keyring))
            if result.returncode != 0 or "IMPORT_OK" not in self.last_status:
                raise VerifyError(f"keyring {self.keyring} could not be imported")
        except BaseException:
            self.__exit__()
            raise
        return self

    def __exit__(self, *exc):
        shutil.rmtree(self.home, ignore_errors=True)

    def run(self, *args):
        status = self.home / "status.txt"
        status.unlink(missing_ok=True)
        argv = [self.gpg, "--homedir", str(self.home), "--batch", "--no-tty", "--no-options",
                "--no-autostart", "--no-auto-key-retrieve", "--no-auto-key-locate",
                "--keyserver-options", "no-auto-key-retrieve",
                "--status-file", str(status), *args]
        try:
            result = subprocess.run(argv, capture_output=True, text=True, timeout=600,
                                    env={"PATH": "/usr/bin:/bin", "LC_ALL": "C", "GNUPGHOME": str(self.home)})
        except (OSError, subprocess.TimeoutExpired) as err:
            raise VerifyError(f"gpg could not run: {type(err).__name__}") from err
        self.last_status = status.read_text(errors="replace") if status.exists() else ""
        return result

    def listing(self, secret=False):
        result = self.run("--with-colons", "--fixed-list-mode", "--with-subkey-fingerprints",
                          "--list-secret-keys" if secret else "--list-keys")
        if result.returncode != 0 and not (secret and not result.stdout):
            raise VerifyError("gpg could not list the keyring")
        return result.stdout

    def verify(self, signature, data):
        result = self.run("--verify", "--", str(signature), str(data))
        return result.returncode, self.last_status


def load_lock(path):
    lock = json.loads(pathlib.Path(path).read_text())
    primary = lock.get("primary_key_fingerprint")
    subkeys = lock.get("signing_subkey_fingerprints")
    if not isinstance(subkeys, list):
        subkeys = []
    return lock, primary, subkeys


def inspect_keyring(gpg, primary, subkeys):
    """(primaries, problems)."""
    if gpg.listing(secret=True).strip():
        return [], ["keyring contains secret key material"]
    primaries = parse_keyring(gpg.listing())
    return primaries, check_keyring(primaries, primary, subkeys)


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify(lock_path, keyring, image, signature, gpg="gpg", now=None):
    """The facts for host-facts.json's "image" section. Never raises for a
    verification failure: problems are recorded and signature_verified is
    false."""
    lock, primary, subkeys = load_lock(lock_path)
    facts = {"lock_primary_fingerprint": primary, "lock_signing_subkeys": subkeys,
             "lock_sha256": lock.get("sha256"), "sha256_expected": lock.get("sha256"), "sha256_actual": None,
             "keyring_primary_fingerprints": [], "keyring_subkey_fingerprints": [],
             "signature_primary_fingerprint": None, "signature_signing_fingerprint": None,
             "signature_status": [], "signature_verified": False, "verification_problems": []}
    problems = []
    try:
        facts["sha256_actual"] = sha256_file(image)
    except OSError as err:
        problems.append(f"image unreadable: {type(err).__name__}")
    if not SHA64.match(lock.get("sha256") or ""):
        problems.append("lock sha256 is not 64 lowercase hex")
    elif facts["sha256_actual"] != lock["sha256"]:
        problems.append("image SHA-256 differs from the pinned SHA-256")
    try:
        with Gpg(keyring, gpg) as home:
            primaries, keyring_problems = inspect_keyring(home, primary, subkeys)
            problems += keyring_problems
            facts["keyring_primary_fingerprints"] = [p["fingerprint"] for p in primaries]
            facts["keyring_subkey_fingerprints"] = [s["fingerprint"] for p in primaries for s in p["subkeys"]
                                                    if p["fingerprint"] == primary]
            code, status_text = home.verify(signature, image)
            records = parse_status(status_text)
            facts["signature_status"] = [k for k, _ in records if k not in ("KEY_CONSIDERED",)]
            validsig, status_problems = check_status(records, primary, subkeys,
                                                     facts["keyring_subkey_fingerprints"], now)
            problems += status_problems
            if code != 0:
                problems.append(f"gpg --verify exited {code}")
            if validsig:
                facts["signature_primary_fingerprint"] = validsig["primary_fingerprint"]
                facts["signature_signing_fingerprint"] = validsig["signing_fingerprint"]
    except (VerifyError, OSError, ValueError) as err:
        problems.append(str(err))
    facts["verification_problems"] = sorted(set(problems))
    facts["signature_verified"] = not facts["verification_problems"]
    return facts


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("keyring", "verify"):
        p = sub.add_parser(name)
        p.add_argument("--lock", required=True)
        p.add_argument("--keyring", required=True)
        if name == "verify":
            p.add_argument("--image", required=True)
            p.add_argument("--signature", required=True)
            p.add_argument("--out", required=True)
    args = parser.parse_args(argv[1:])
    if args.command == "keyring":
        _, primary, subkeys = load_lock(args.lock)
        try:
            with Gpg(args.keyring) as home:
                primaries, problems = inspect_keyring(home, primary, subkeys)
        except VerifyError as err:
            primaries, problems = [], [str(err)]
        print(json.dumps({"ok": not problems, "primaries": [p["fingerprint"] for p in primaries],
                          "problems": problems}, indent=2))
        return 0 if not problems else 1
    facts = verify(args.lock, args.keyring, args.image, args.signature)
    with open(args.out, "x") as out:
        out.write(json.dumps(facts, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"signature_verified": facts["signature_verified"],
                      "problems": facts["verification_problems"]}, indent=2))
    return 0 if facts["signature_verified"] else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
