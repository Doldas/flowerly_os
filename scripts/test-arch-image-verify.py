#!/usr/bin/env python3
"""Offline tests for scripts/arch-image-verify.py (#263).

Real GnuPG operations on disposable, synthetic, test-only keys generated in
throwaway homes under /tmp; no network, no keyserver, no real Arch key, no
image. Plus parser unit tests and mutation tests of GnuPG's status output.

Covered: one signing subkey; several legitimate subkeys; a signature by an
unapproved subkey of the pinned primary, by the primary itself, by a wrong
primary key; an unrelated extra primary key; a keyring without the pinned
primary; secret key material; missing, corrupt and wrong-data signatures;
digest mismatch; revoked and expired keys; malformed and ambiguous keyring
listings; malformed, missing, duplicated and conflicting VALIDSIG/GOODSIG;
forged human-readable success without a valid status.
"""
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
TOOL = ROOT / "scripts/arch-image-verify.py"
GPG = shutil.which("gpg")


def load():
    spec = importlib.util.spec_from_file_location("arch_image_verify", TOOL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


v = load()


# --- synthetic keys -----------------------------------------------------------------

class Home:
    """A throwaway GnuPG home that generates and uses test-only keys."""

    def __init__(self, scratch, name):
        self.dir = scratch / name
        self.dir.mkdir(mode=0o700)

    def gpg(self, *args, faked=None, stdin=None):
        argv = [GPG, "--homedir", str(self.dir), "--batch", "--no-tty", "--pinentry-mode", "loopback",
                "--passphrase", ""]
        if faked:
            argv += ["--faked-system-time", faked]
        result = subprocess.run(argv + list(args), capture_output=True, text=True, input=stdin, timeout=120)
        assert result.returncode == 0, (args, result.stderr)
        return result

    def keys(self):
        listing = self.gpg("--with-colons", "--fixed-list-mode", "--with-subkey-fingerprints",
                           "--list-keys").stdout
        return v.parse_keyring(listing)

    def primary(self, uid, usage="cert", faked=None):
        before = {k["fingerprint"] for k in self.keys()}
        self.gpg("--quick-gen-key", uid, "ed25519", usage, "never", faked=faked)
        return next(k["fingerprint"] for k in self.keys() if k["fingerprint"] not in before)

    def add_subkey(self, primary, usage="sign", expire="2y", algo="ed25519", faked=None):
        before = self.subkeys(primary)
        self.gpg("--quick-add-key", primary, algo, usage, expire, faked=faked)
        return next(s for s in self.subkeys(primary) if s not in before)

    def subkeys(self, primary):
        return [s["fingerprint"] for k in self.keys() if k["fingerprint"] == primary for s in k["subkeys"]]

    def export(self, path, *fprs, secret=False):
        out = self.gpg("--armor", "--export-secret-keys" if secret else "--export", *fprs).stdout
        path.write_text(out)
        return path

    def sign(self, signer, data, out, faked=None):
        out.unlink(missing_ok=True)
        self.gpg("--local-user", signer + "!", "--detach-sign", "--output", str(out), str(data), faked=faked)
        return out

    def revoke_primary(self, primary):
        cert = (self.dir / "openpgp-revocs.d" / f"{primary}.rev").read_text().replace(":-----BEGIN", "-----BEGIN")
        self.gpg("--import", stdin=cert)

    def revoke_subkey(self, primary, index):
        self.gpg("--command-fd", "0", "--edit-key", primary, stdin=f"key {index}\nrevkey\ny\n0\n\ny\nsave\n")

    def close(self):
        subprocess.run(["gpgconf", "--homedir", str(self.dir), "--kill", "all"], capture_output=True)


def lock(path, primary, subkeys, data):
    path.write_text(json.dumps({"primary_key_fingerprint": primary, "signing_subkey_fingerprints": subkeys,
                                "sha256": hashlib.sha256(data.read_bytes()).hexdigest()}))
    return path


def outcome(lock_path, keyring, image, sig, gpg="gpg"):
    facts = v.verify(lock_path, keyring, image, sig, gpg=gpg)
    assert facts["signature_verified"] is (not facts["verification_problems"]), facts
    return facts


def failed(facts, *needles):
    assert facts["signature_verified"] is False, facts
    text = " | ".join(facts["verification_problems"])
    for needle in needles:
        assert needle in text, (needle, text)
    return text


# --- end to end with real GnuPG -----------------------------------------------------

def check_real_gnupg(scratch):
    image = scratch / "image.qcow2"
    image.write_bytes(os.urandom(4096) + b"synthetic test image, not an Arch image")
    other = scratch / "other.bin"
    other.write_bytes(b"different bytes")
    arch, wrong = Home(scratch, "arch"), Home(scratch, "wrong")
    try:
        # Arch-like: certify-only primary with signing subkeys and an encryption subkey.
        primary = arch.primary("synthetic-arch-boxes <test@example.invalid>")
        s1 = arch.add_subkey(primary)
        s2 = arch.add_subkey(primary)
        enc = arch.add_subkey(primary, usage="encr", algo="cv25519")
        keyring = arch.export(scratch / "keyring.asc", primary)
        sig1 = arch.sign(s1, image, scratch / "image.s1.sig")
        sig2 = arch.sign(s2, image, scratch / "image.s2.sig")

        # 1. one approved signing subkey (keyring may hold more subkeys).
        one = lock(scratch / "lock-one.json", primary, [s1], image)
        facts = outcome(one, keyring, image, sig1)
        assert facts["signature_verified"], facts["verification_problems"]
        assert facts["signature_primary_fingerprint"] == primary and facts["signature_signing_fingerprint"] == s1
        assert facts["keyring_primary_fingerprints"] == [primary]
        assert set(facts["keyring_subkey_fingerprints"]) == {s1, s2, enc}
        assert "BEGIN PGP" not in json.dumps(facts)

        # 2. multiple legitimate signing subkeys, signature by either.
        both = lock(scratch / "lock-both.json", primary, [s1, s2], image)
        for sig, signer in ((sig1, s1), (sig2, s2)):
            facts = outcome(both, keyring, image, sig)
            assert facts["signature_verified"] and facts["signature_signing_fingerprint"] == signer, facts

        # 3. unexpected signing subkey: a real subkey of the primary, not approved.
        failed(outcome(one, keyring, image, sig2), "not an approved signing subkey")
        # an approved fingerprint that is the encryption subkey: not signing-capable.
        failed(outcome(lock(scratch / "lock-enc.json", primary, [enc], image), keyring, image, sig1),
               "not signing-capable")
        # an approved fingerprint that is not a subkey of this primary at all.
        failed(outcome(lock(scratch / "lock-ghost.json", primary, ["F" * 40], image), keyring, image, sig1),
               "not a subkey of the pinned primary")

        # 4. wrong primary key: signed by an unrelated key not in the keyring.
        w_primary = wrong.primary("synthetic-wrong <wrong@example.invalid>")
        w_sub = wrong.add_subkey(w_primary)
        wrong_sig = wrong.sign(w_sub, image, scratch / "image.wrong.sig")
        failed(outcome(one, keyring, image, wrong_sig), "NO_PUBKEY", "exactly one VALIDSIG, got 0")
        # keyring holding only the wrong key against the pinned primary.
        wrong_ring = wrong.export(scratch / "wrong.asc", w_primary)
        failed(outcome(one, wrong_ring, image, wrong_sig), "is not the pinned primary")
        # lock pinned to the wrong primary but the signer subkey claimed from ours.
        failed(outcome(lock(scratch / "lock-mixed.json", w_primary, [s1], image), wrong_ring, image, sig1),
               "not a subkey of the pinned primary")

        # 5. unrelated additional primary key in the keyring.
        combined = scratch / "combined.asc"
        combined.write_text(keyring.read_text() + wrong_ring.read_text())
        failed(outcome(one, combined, image, sig1), "exactly one primary key, holds 2")

        # 6. primary that can sign, signing with the primary itself: not an approved subkey.
        signing_primary = wrong.primary("synthetic-signing-primary <sp@example.invalid>", usage="sign,cert")
        sp_sub = wrong.add_subkey(signing_primary)
        sp_ring = wrong.export(scratch / "sp.asc", signing_primary)
        sp_sig = wrong.sign(signing_primary, image, scratch / "image.sp.sig")
        sp_lock = lock(scratch / "lock-sp.json", signing_primary, [sp_sub], image)
        failed(outcome(sp_lock, sp_ring, image, sp_sig), "not an approved signing subkey")
        failed(outcome(lock(scratch / "lock-sp2.json", signing_primary, [signing_primary], image),
                       sp_ring, image, sp_sig), "primary key as a signing subkey")

        # 7. missing, corrupt and wrong-data signatures; digest mismatch.
        failed(outcome(one, keyring, image, scratch / "absent.sig"))
        corrupt = scratch / "corrupt.sig"
        data = bytearray(sig1.read_bytes())
        data[-5] ^= 0xFF
        corrupt.write_bytes(bytes(data))
        failed(outcome(one, keyring, image, corrupt))
        failed(outcome(one, keyring, other, sig1), "BADSIG", "SHA-256 differs")
        mismatch = lock(scratch / "lock-mismatch.json", primary, [s1], other)
        failed(outcome(mismatch, keyring, image, sig1), "image SHA-256 differs from the pinned SHA-256")
        bad_lock = scratch / "lock-badsha.json"
        bad_lock.write_text(json.dumps({"primary_key_fingerprint": primary, "signing_subkey_fingerprints": [s1],
                                        "sha256": None}))
        failed(outcome(bad_lock, keyring, image, sig1), "lock sha256")
        failed(outcome(one, scratch / "absent-keyring.asc", image, sig1), "not a readable OpenPGP key file")
        garbage = scratch / "garbage.asc"
        garbage.write_text("not a key\n")
        failed(outcome(one, garbage, image, sig1))

        # 8. secret key material is refused, not imported.
        secret = arch.export(scratch / "secret.asc", primary, secret=True)
        failed(outcome(one, secret, image, sig1), "secret key material")

        # 9. revoked signing subkey; revoked primary.
        revoked_home = Home(scratch, "revoked")
        try:
            r_primary = revoked_home.primary("synthetic-revoked <rev@example.invalid>")
            r_sub = revoked_home.add_subkey(r_primary)
            r_sig = revoked_home.sign(r_sub, image, scratch / "image.rsub.sig")
            revoked_home.revoke_subkey(r_primary, 1)
            r_ring = revoked_home.export(scratch / "revoked-sub.asc", r_primary)
            r_lock = lock(scratch / "lock-rsub.json", r_primary, [r_sub], image)
            failed(outcome(r_lock, r_ring, image, r_sig), "is revoked")
            revoked_home.revoke_primary(r_primary)
            r_ring = revoked_home.export(scratch / "revoked-primary.asc", r_primary)
            failed(outcome(r_lock, r_ring, image, r_sig), "primary key", "is revoked")
        finally:
            revoked_home.close()

        # 10. expired signing subkey: made and used two years ago, expired after a day.
        expired_home = Home(scratch, "expired")
        try:
            past = "20240101T000000"
            e_primary = expired_home.primary("synthetic-expired <exp@example.invalid>", faked=past)
            e_sub = expired_home.add_subkey(e_primary, expire="1d", faked=past)
            e_sig = expired_home.sign(e_sub, image, scratch / "image.exp.sig", faked=past)
            e_ring = expired_home.export(scratch / "expired.asc", e_primary)
            e_lock = lock(scratch / "lock-exp.json", e_primary, [e_sub], image)
            failed(outcome(e_lock, e_ring, image, e_sig), "is expired")
            # The status check alone also refuses it, independent of the listing.
            with v.Gpg(e_ring) as home:
                _, status = home.verify(e_sig, image)
            _, problems = v.check_status(v.parse_status(status), e_primary, [e_sub], [e_sub])
            assert any("EXPKEYSIG" in p or "KEYEXPIRED" in p for p in problems), problems
        finally:
            expired_home.close()

        # 11. forged human-readable success: a gpg that claims success for --verify
        # in words and exit status, but writes no status.
        fake = scratch / "fake-gpg"
        fake.write_text(f"""#!/bin/sh
for a in "$@"; do
  if [ "$a" = --verify ]; then
    echo 'gpg: Good signature from "synthetic-arch-boxes <test@example.invalid>" [ultimate]' >&2
    echo '[GNUPG:] GOODSIG {s1[-16:]} forged' >&2
    exit 0
  fi
done
exec {GPG} "$@"
""")
        fake.chmod(0o755)
        failed(outcome(one, keyring, image, wrong_sig, gpg=str(fake)), "exactly one VALIDSIG, got 0")
        # and one that also prints a forged status line on stdout rather than the status channel.
        fake.write_text(fake.read_text().replace("forged' >&2", f"forged'\n    echo '[GNUPG:] VALIDSIG {s1} "
                                                 f"2026-01-01 1 0 4 0 22 10 00 {primary}'"))
        failed(outcome(one, keyring, image, wrong_sig, gpg=str(fake)), "exactly one GOODSIG, got 0")
        # a valid status with a failing exit status is still a failure.
        failing = scratch / "failing-gpg"
        failing.write_text(f"#!/bin/sh\n{GPG} \"$@\"\nfor a in \"$@\"; do [ \"$a\" = --verify ] && exit 2; done\n"
                           "exit 0\n")
        failing.chmod(0o755)
        failed(outcome(one, keyring, image, sig1, gpg=str(failing)), "gpg --verify exited 2")

        # 12. CLI: keyring and verify subcommands, exit statuses, --out never overwrites.
        result = subprocess.run([sys.executable, "-I", TOOL, "keyring", "--lock", one, "--keyring", keyring],
                                capture_output=True, text=True)
        assert result.returncode == 0 and json.loads(result.stdout)["primaries"] == [primary], result
        result = subprocess.run([sys.executable, "-I", TOOL, "keyring", "--lock", one, "--keyring", combined],
                                capture_output=True, text=True)
        assert result.returncode == 1 and not json.loads(result.stdout)["ok"], result
        out = scratch / "facts.json"
        result = subprocess.run([sys.executable, "-I", TOOL, "verify", "--lock", one, "--keyring", keyring,
                                 "--image", image, "--signature", sig1, "--out", out], capture_output=True, text=True)
        assert result.returncode == 0 and json.loads(out.read_text())["signature_verified"], result
        result = subprocess.run([sys.executable, "-I", TOOL, "verify", "--lock", one, "--keyring", keyring,
                                 "--image", image, "--signature", sig2, "--out", scratch / "facts2.json"],
                                capture_output=True, text=True)
        assert result.returncode == 1, result
        result = subprocess.run([sys.executable, "-I", TOOL, "verify", "--lock", one, "--keyring", keyring,
                                 "--image", image, "--signature", sig1, "--out", out], capture_output=True, text=True)
        assert result.returncode != 0 and "FileExistsError" in result.stderr, result
        return {"primary": primary, "s1": s1, "status": real_status(keyring, image, sig1)}
    finally:
        arch.close()
        wrong.close()


def real_status(keyring, image, sig):
    with v.Gpg(keyring) as home:
        code, status = home.verify(sig, image)
    assert code == 0
    return status


# --- parsers ------------------------------------------------------------------------

A, B, C = "A" * 40, "B" * 40, "C" * 40


def listing(*records):
    return "\n".join(records) + "\n"


def pub(fpr, validity="-", caps="cC", kind="pub", algo=22, length=255):
    return f"{kind}:{validity}:{length}:{algo}:{fpr[-16:]}:1650000000:::-:::{caps}:::::ed25519:::0:"


def fpr(value):
    return f"fpr:::::::::{value}:"


def check_keyring_parser():
    good = listing("tru::1:1700000000:0:3:1:5", pub(A), fpr(A), "uid:-::::1650000000::X::arch::::::::::0:",
                   pub(B, kind="sub", caps="s"), fpr(B), pub(C, kind="sub", caps="e", algo=18), fpr(C))
    keys = v.parse_keyring(good)
    assert [k["fingerprint"] for k in keys] == [A] and [s["fingerprint"] for s in keys[0]["subkeys"]] == [B, C]
    assert v.check_keyring(keys, A, [B]) == []
    bad_listings = {
        "fpr missing": listing(pub(A), pub(B, kind="sub", caps="s"), fpr(B)),
        "fpr at end missing": listing(pub(A), fpr(A), pub(B, kind="sub", caps="s")),
        "fpr twice": listing(pub(A), fpr(A), fpr(A)),
        "fpr after uid": listing(pub(A), "uid:-::::::X::arch::::::::::0:", fpr(A)),
        "subkey first": listing(pub(B, kind="sub", caps="s"), fpr(B)),
        "duplicate fingerprint": listing(pub(A), fpr(A), pub(A, kind="sub", caps="s"), fpr(A)),
        "lowercase fpr": listing(pub(A), fpr(A.lower())),
        "short fpr": listing(pub(A), fpr("A" * 39)),
        "fpr not matching keyid": listing(pub(A), fpr(B)),
        "truncated pub": listing("pub:-:255:22", fpr(A)),
        "non-numeric length": listing(pub(A).replace(":255:", ":x:"), fpr(A)),
        "secret key": listing(pub(A, kind="sec"), fpr(A)),
        "secret subkey": listing(pub(A), fpr(A), pub(B, kind="ssb", caps="s"), fpr(B)),
        "unknown record": listing(pub(A), fpr(A), "zzz:1:2:3"),
        "human text": "pub   ed25519 2022-05-29 [C]\n      " + A + "\n",
    }
    for name, text in bad_listings.items():
        try:
            v.parse_keyring(text)
        except v.VerifyError:
            continue
        raise AssertionError(f"keyring listing accepted: {name}")
    assert v.parse_keyring("") == []

    def problems(text, primary=A, subkeys=(B,)):
        return " | ".join(v.check_keyring(v.parse_keyring(text), primary, list(subkeys)))
    two = listing(pub(A), fpr(A), pub(B, kind="sub", caps="s"), fpr(B), pub(C), fpr(C))
    assert "exactly one primary key, holds 2" in problems(two)
    assert "exactly one primary key, holds 0" in problems("")
    assert "not the pinned primary" in problems(listing(pub(C), fpr(C), pub(B, kind="sub", caps="s"), fpr(B)))
    assert "not a subkey" in problems(listing(pub(A), fpr(A)))
    assert "not signing-capable" in problems(listing(pub(A), fpr(A), pub(B, kind="sub", caps="e"), fpr(B)))
    for validity, word in (("r", "revoked"), ("e", "expired"), ("i", "invalid"), ("d", "disabled")):
        assert word in problems(listing(pub(A, validity=validity), fpr(A), pub(B, kind="sub", caps="s"), fpr(B)))
        assert word in problems(listing(pub(A), fpr(A), pub(B, validity=validity, kind="sub", caps="s"), fpr(B)))
    assert "RSA shorter" in problems(listing(pub(A, algo=1, length=1024), fpr(A),
                                             pub(B, kind="sub", caps="s"), fpr(B)))
    assert "deprecated algorithm" in problems(listing(pub(A), fpr(A),
                                                      pub(B, kind="sub", caps="s", algo=17, length=3072), fpr(B)))
    ok = listing(pub(A), fpr(A), pub(B, kind="sub", caps="s"), fpr(B))
    for primary, subkeys, needle in ((None, [B], "primary_key_fingerprint"), ("a" * 40, [B], "primary_key"),
                                     (A, [], "signing_subkey_fingerprints"), (A, [B, B], "distinct"),
                                     (A, ["b" * 40], "signing_subkey"), (A, [A], "primary key as a signing")):
        assert needle in problems(ok, primary, subkeys), (primary, subkeys)


def status(*lines):
    return "".join(f"[GNUPG:] {line}\n" for line in lines)


def validsig(signer=B, primary=A, created="1700000000", expires="0", hash_algo="10", sig_class="00", extra=()):
    return " ".join(["VALIDSIG", signer, "2023-11-14", created, expires, "4", "0", "22", hash_algo, sig_class,
                     primary, *extra])


GOOD = ("NEWSIG", f"KEY_CONSIDERED {A} 0", "SIG_ID abc 2023-11-14 1700000000",
        f"GOODSIG {B[-16:]} synthetic <x@example.invalid>", validsig())


def check_status_parser():
    def problems(text, approved=(B,), keyring=(B,), now=1800000000):
        _, found = v.check_status(v.parse_status(text), A, list(approved), list(keyring), now)
        return " | ".join(found)
    assert problems(status(*GOOD)) == ""
    cases = {
        "missing VALIDSIG": (status(*GOOD[:-1]), "exactly one VALIDSIG, got 0"),
        "missing GOODSIG": (status(*GOOD[:3], GOOD[4]), "exactly one GOODSIG, got 0"),
        "missing NEWSIG": (status(*GOOD[1:]), "exactly one NEWSIG, got 0"),
        "two signatures": (status(*GOOD, *GOOD), "exactly one NEWSIG, got 2"),
        "two VALIDSIG": (status(*GOOD, validsig()), "exactly one VALIDSIG, got 2"),
        "VALIDSIG 9 fields": (status(*GOOD[:4], validsig().rsplit(" ", 1)[0]), "9 fields"),
        "VALIDSIG 11 fields": (status(*GOOD[:4], validsig(extra=("x",))), "11 fields"),
        "VALIDSIG lowercase": (status(*GOOD[:4], validsig(signer=B.lower())), "not 40 uppercase hex"),
        "VALIDSIG bad timestamp": (status(*GOOD[:4], validsig(created="soon")), "malformed"),
        "VALIDSIG bad class": (status(*GOOD[:4], validsig(sig_class="zz")), "malformed"),
        "wrong primary": (status(*GOOD[:4], validsig(primary=C)), "not the pinned primary"),
        "unapproved signer": (status(*GOOD[:3], f"GOODSIG {C[-16:]} x", validsig(signer=C)),
                              "not an approved signing subkey"),
        "signer not in keyring": (status(*GOOD), "not a subkey of the pinned primary"),
        "GOODSIG other key": (status(*GOOD[:3], f"GOODSIG {C[-16:]} x", GOOD[4]), "GOODSIG key id"),
        "GOODSIG empty": (status(*GOOD[:3], "GOODSIG", GOOD[4]), "GOODSIG key id"),
        "text signature": (status(*GOOD[:4], validsig(sig_class="01")), "not a binary document"),
        "SHA-1": (status(*GOOD[:4], validsig(hash_algo="2")), "hash algorithm 2"),
        "future": (status(*GOOD[:4], validsig(created="1900000000")), "in the future"),
        "signature expired": (status(*GOOD[:4], validsig(expires="1750000000")), "signature has expired"),
    }
    for name, (text, needle) in cases.items():
        keyring = (B,) if name != "signer not in keyring" else ()
        found = problems(text, keyring=keyring)
        assert needle in found, (name, needle, found)
    for keyword in sorted(v.REJECTING_STATUS):
        assert f"GnuPG reported {keyword}" in problems(status(*GOOD, f"{keyword} x")), keyword
    for text in ("gpg: Good signature from x\n", status(*GOOD) + "Good signature\n", "[GNUPG:]  \n",
                 "[GNUPG:] lower case\n"):
        try:
            v.parse_status(text)
        except v.VerifyError:
            continue
        raise AssertionError(f"status accepted: {text!r}")
    assert problems("") != ""


def check_status_mutations(real):
    """Every single-line deletion and every single-field change of a real,
    valid GnuPG status either still names the same approved signer or is
    rejected; none turns into acceptance of a different identity."""
    primary, s1, text = real["primary"], real["s1"], real["status"]
    records = v.parse_status(text)
    validsig, problems = v.check_status(records, primary, [s1], [s1])
    assert problems == [] and validsig["signing_fingerprint"] == s1, problems
    lines = text.splitlines()
    essential = [i for i, line in enumerate(lines) if line.split(" ")[1] in ("NEWSIG", "GOODSIG", "VALIDSIG")]
    for i in essential:
        mutated = "\n".join(lines[:i] + lines[i + 1:]) + "\n"
        assert v.check_status(v.parse_status(mutated), primary, [s1], [s1])[1], f"deleting line {i} accepted"
    index = next(i for i, line in enumerate(lines) if " VALIDSIG " in line)
    fields = lines[index].split(" ")
    replacements = {2: [primary, "0" * 40], 4: ["9999999999"], 9: ["2", "1"], 10: ["01"], 11: [s1, "0" * 40]}
    for position, values in replacements.items():
        for value in values:
            mutated_fields = list(fields)
            mutated_fields[position] = value
            mutated = "\n".join(lines[:index] + [" ".join(mutated_fields)] + lines[index + 1:]) + "\n"
            _, found = v.check_status(v.parse_status(mutated), primary, [s1], [s1])
            assert found, (position, value)
    # Approved structure changed under an otherwise valid status.
    assert v.check_status(records, primary, ["0" * 40], ["0" * 40])[1]
    assert v.check_status(records, "0" * 40, [s1], [s1])[1]


def main():
    assert GPG, "gpg is required: these tests verify real signatures on synthetic keys"
    check_keyring_parser()
    check_status_parser()
    with tempfile.TemporaryDirectory(prefix="fl-gpg-", dir="/tmp") as scratch:
        real = check_real_gnupg(pathlib.Path(scratch))
    check_status_mutations(real)
    print("arch image verification, offline: real GnuPG on synthetic keys accepts only the pinned primary's "
          "approved signing subkeys with the pinned digest; wrong, extra, unapproved, revoked, expired and "
          "secret keys, bad or missing signatures, digest mismatch and forged success are refused; keyring "
          "and status parsers reject malformed, ambiguous and conflicting input.")


if __name__ == "__main__":
    main()
