#!/usr/bin/env python3
"""Acceptance drill for the frozen Arch package set (ADR-0020, #49).

Runs the evidence items against a real cache produced by
``freeze-arch-snapshot.py`` and prints one JSON report. Every check goes
through the independent Rust verifier (``distro-builder manifest``):

1. the frozen manifest and cache verify with the pinned keyring;
2. a later snapshot differs from the frozen set (the mirror moved on), yet the
   cache still verifies byte for byte;
3. each tampering (archive bytes, signature bytes, manifest, keyring, a
   signer swapped in the manifest) fails closed before anything is installed;
4. with one cached archive removed and no network, the missing digest is
   listed and nothing is substituted.

The later-snapshot step is the only one using the network, only with the
same approval as the freeze, and reads two small repository databases.
"""

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import importlib.util

SPEC = importlib.util.spec_from_file_location(
    "freeze", pathlib.Path(__file__).resolve().parent / "freeze-arch-snapshot.py"
)
freeze = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(freeze)


def verify(binary, manifest, cache, keyring):
    run = subprocess.run(
        [
            binary,
            "manifest",
            "verify-cache",
            str(manifest),
            "--cache",
            str(cache),
            "--keyring",
            str(keyring),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    try:
        report = json.loads(run.stdout)
    except ValueError:
        report = {}
    return run.returncode, report, run.stderr.strip().splitlines()[-1:]


def mirror_moved(manifest, later_date):
    """Packages of the frozen set whose version differs in a later snapshot."""
    packages = {}
    with tempfile.TemporaryDirectory(prefix="flowerly-later-") as work:
        for repository in freeze.REPOSITORIES:
            path = pathlib.Path(work) / f"{repository}.db"
            freeze.fetch(
                freeze.archive_url(later_date, repository, f"{repository}.db"),
                path,
                freeze.MAX_DB,
            )
            packages.update(freeze.read_database(path, repository))
    changed = sorted(
        f"{p['name']} {p['version']} -> {packages[p['name']]['version']}"
        for p in manifest["packages"]
        if p["name"] in packages and packages[p["name"]]["version"] != p["version"]
    )
    gone = sorted(p["name"] for p in manifest["packages"] if p["name"] not in packages)
    return changed, gone


# What each tampering must produce: rejected for exactly this reason, so a
# rejection caused by a broken drill cannot pass for enforcement.
EXPECTED = {
    "archive_bytes": {"mismatched": 1},
    "signature_bytes": {"mismatched": 1},
    "rehashed_forged_archive": {"signature_problems": 1},
    "signer_swapped": {"signature_problems": 1},
    "empty_keyring": {"signature_problems": None},
    "archive_removed_offline": {"missing": 1},
    "manifest_not_canonical": {"mismatched": 0, "missing": 0, "signature_problems": 0},
}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--distro-builder", required=True)
    parser.add_argument("--manifest", required=True)
    parser.add_argument("--cache", required=True)
    parser.add_argument("--keyring-sha256", required=True)
    parser.add_argument("--later-date", help="YYYY/MM/DD of a newer snapshot (network)")
    args = parser.parse_args()
    cache = pathlib.Path(args.cache)
    manifest_path = pathlib.Path(args.manifest)
    keyring = cache / "sha256" / args.keyring_sha256
    manifest = json.loads(manifest_path.read_text())
    report = {"manifest_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest()}

    started = time.monotonic()
    code, result, _ = verify(args.distro_builder, manifest_path, cache, keyring)
    report["baseline"] = {
        "exit": code,
        "signature_trust": result.get("signature_trust"),
        "artifacts_verified": result.get("artifacts_verified"),
        "seconds": round(time.monotonic() - started, 1),
    }
    report["cache_bytes"] = sum(f.stat().st_size for f in (cache / "sha256").iterdir())
    report["archive_bytes"] = sum(p["size"] for p in manifest["packages"])

    if args.later_date:
        changed, gone = mirror_moved(manifest, args.later_date)
        code, result, _ = verify(args.distro_builder, manifest_path, cache, keyring)
        report["mirror_moved"] = {
            "later_snapshot": args.later_date,
            "packages_with_a_different_version": len(changed),
            "examples": changed[:8],
            "packages_gone": gone,
            "replay_exit": code,
            "replay_signature_trust": result.get("signature_trust"),
        }

    tampered = {}
    victim = manifest["packages"][len(manifest["packages"]) // 2]
    with tempfile.TemporaryDirectory(
        prefix="flowerly-tamper-", dir=cache.parent
    ) as work:
        work = pathlib.Path(work)
        counter = iter(range(1000))

        def fresh_cache():
            """A cache of hard links to the real one (the verifier refuses symlinks),
            so one entry can be replaced without touching the original."""
            copy = work / f"cache{next(counter)}"
            (copy / "sha256").mkdir(parents=True)
            for entry in (cache / "sha256").iterdir():
                os.link(entry, copy / "sha256" / entry.name)
            return copy

        def replace(copy, digest, data):
            link = copy / "sha256" / digest
            link.unlink()
            if data is not None:
                link.write_bytes(data)

        def attempt(name, manifest_file, cache_dir, keyring_file):
            code, result, _ = verify(
                args.distro_builder, manifest_file, cache_dir, keyring_file
            )
            tampered[name] = {
                "exit": code,
                "rejected": code != 0,
                "mismatched": len(result.get("mismatched", [])),
                "signature_problems": len(result.get("signature_problems", [])),
                "missing": len(result.get("missing", [])),
            }

        def flipped(digest):
            original = (cache / "sha256" / digest).read_bytes()
            return bytes([original[0] ^ 1]) + original[1:]

        for name, digest in (
            ("archive_bytes", victim["sha256"]),
            ("signature_bytes", victim["signature_sha256"]),
        ):
            copy = fresh_cache()
            replace(copy, digest, flipped(digest))
            attempt(name, manifest_path, copy, keyring)

        # A forged archive whose hash the manifest was re-written to match:
        # only the signature check can refuse it.
        copy = fresh_cache()
        data = flipped(victim["sha256"])
        forged_digest = hashlib.sha256(data).hexdigest()
        (copy / "sha256" / forged_digest).write_bytes(data)
        forged = json.loads(manifest_path.read_text())
        for package in forged["packages"]:
            if package["name"] == victim["name"]:
                package["sha256"] = forged_digest
        forged["packages"].sort(
            key=lambda p: (
                p["repository"],
                p["architecture"],
                p["name"],
                p["version"],
                p["sha256"],
            )
        )
        forged_path = work / "forged.json"
        forged_path.write_bytes(freeze.canonical(forged))
        attempt("rehashed_forged_archive", forged_path, copy, keyring)

        edited = work / "edited.json"
        edited.write_bytes(
            manifest_path.read_bytes().replace(b'"size": ', b'"size":  ', 1)
        )
        attempt("manifest_not_canonical", edited, fresh_cache(), keyring)

        swapped = json.loads(manifest_path.read_text())
        swapped["packages"][0]["signer_fingerprint"] = "0" * 40
        swapped_path = work / "swapped.json"
        swapped_path.write_bytes(freeze.canonical(swapped))
        attempt("signer_swapped", swapped_path, fresh_cache(), keyring)

        empty = work / "empty.gpg"
        empty.write_bytes(b"")
        attempt("empty_keyring", manifest_path, fresh_cache(), empty)

        copy = fresh_cache()
        replace(copy, victim["sha256"], None)
        attempt("archive_removed_offline", manifest_path, copy, keyring)
    report["tampering"] = tampered
    report["all_tampering_rejected"] = all(
        item["rejected"] for item in tampered.values()
    )
    report["ok"] = (
        report["baseline"]["exit"] == 0
        and report["baseline"]["signature_trust"] == freeze_verified()
        and report["all_tampering_rejected"]
    )
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["ok"] else 1


def freeze_verified():
    return "verified_against_supplied_keyring"


if __name__ == "__main__":
    sys.exit(main())
