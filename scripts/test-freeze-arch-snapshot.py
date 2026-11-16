#!/usr/bin/env python3
"""Offline tests for scripts/freeze-arch-snapshot.py (#49, ADR-0020).

No network and no real Arch key: synthetic repository databases, a fake
fetcher serving them, and throwaway GnuPG keys. Covered: the network refusal
without approval and for any URL outside the snapshot tree or with a
redirect; database parsing; transitive resolution including virtual
providers (unique, reviewed choice, ambiguity and unknown refused); the trust
policy shape; revoked signers; a database/archive hash mismatch; a detached
signature that differs from the database's; a keyring package that is not
the pinned one; and a full freeze whose manifest has the canonical form the
Rust checker enforces.
"""

import base64
import hashlib
import importlib.util
import io
import json
import os
import pathlib
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location(
    "freeze", ROOT / "scripts/freeze-arch-snapshot.py"
)
freeze = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(freeze)
GPG = shutil.which("gpg")
APPROVE = {freeze.APPROVAL_ENV: freeze.APPROVAL}


def desc(name, version, filename, data, depends=(), provides=(), signature=b""):
    lines = [
        "%FILENAME%",
        filename,
        "",
        "%NAME%",
        name,
        "",
        "%VERSION%",
        version,
        "",
        "%CSIZE%",
        str(len(data)),
        "",
        "%SHA256SUM%",
        hashlib.sha256(data).hexdigest(),
        "",
        "%ARCH%",
        "x86_64",
        "",
    ]
    if signature:
        lines += ["%PGPSIG%", base64.b64encode(signature).decode(), ""]
    for key, values in (("DEPENDS", depends), ("PROVIDES", provides)):
        if values:
            lines += [f"%{key}%", *values, ""]
    return "\n".join(lines)


def database(path, entries):
    with tarfile.open(path, "w:gz") as archive:
        for name, text in entries.items():
            raw = text.encode()
            info = tarfile.TarInfo(f"{name}/desc")
            info.size = len(raw)
            archive.addfile(info, io.BytesIO(raw))


def parsed(**packages):
    return {
        name: {
            "name": name,
            "depends": spec.get("depends", []),
            "provides": spec.get("provides", []),
        }
        for name, spec in packages.items()
    }


class NetworkTests(unittest.TestCase):
    def test_nothing_is_fetched_without_the_approval_string(self):
        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.dict(os.environ, {}, clear=False),
        ):
            os.environ.pop(freeze.APPROVAL_ENV, None)
            with self.assertRaisesRegex(freeze.Refused, "needs FLOWERLY_ARCHIVE_FETCH"):
                freeze.fetch(
                    freeze.ARCHIVE + "2026/11/09/core/os/x86_64/core.db",
                    pathlib.Path(tmp) / "x",
                    10,
                )
            os.environ[freeze.APPROVAL_ENV] = "yes"
            with self.assertRaises(freeze.Refused):
                freeze.fetch(
                    freeze.ARCHIVE + "2026/11/09/core/os/x86_64/core.db",
                    pathlib.Path(tmp) / "x",
                    10,
                )

    def test_only_exact_snapshot_urls_are_fetched(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict(os.environ, APPROVE):
            for url in (
                "https://geo.mirror.pkgbuild.com/core/os/x86_64/core.db",
                "http://archive.archlinux.org/repos/2026/11/09/core/os/x86_64/core.db",
                "https://archive.archlinux.org.evil.example/repos/x",
                "https://archive.archlinux.org/repos/../packages/a",
                "https://archive.archlinux.org/repos/2026/11/09/core.db?x=1",
            ):
                with self.assertRaisesRegex(
                    freeze.Refused, "not an exact snapshot URL"
                ):
                    freeze.fetch(url, pathlib.Path(tmp) / "x", 10)

    def test_redirects_are_not_followed(self):
        handler = freeze._NoRedirect()
        self.assertIsNone(
            handler.redirect_request(None, None, 302, "Found", {}, "https://elsewhere/")
        )

    def test_an_oversized_download_is_cut_off(self):
        class Reply:
            status = 200

            def __init__(self):
                self.blocks = [b"x" * 8, b"x" * 8]

            def read(self, _):
                return self.blocks.pop(0) if self.blocks else b""

            def __enter__(self):
                return self

            def __exit__(self, *_):
                return False

        opener = mock.Mock()
        opener.open.return_value = Reply()
        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.dict(os.environ, APPROVE),
            self.assertRaisesRegex(freeze.Refused, "larger than 10"),
        ):
            freeze.fetch(
                freeze.ARCHIVE + "2026/11/09/core/os/x86_64/a",
                pathlib.Path(tmp) / "x",
                10,
                opener,
            )


class ResolutionTests(unittest.TestCase):
    def test_closure_follows_dependencies_and_ignores_version_operators(self):
        packages = parsed(
            base={"depends": ["glibc>=2.40", "bash"]},
            glibc={},
            bash={"depends": ["glibc"]},
            other={},
        )
        self.assertEqual(
            sorted(freeze.resolve(["base"], packages)), ["base", "bash", "glibc"]
        )

    def test_a_unique_provider_resolves_a_virtual_name(self):
        packages = parsed(
            app={"depends": ["libfoo.so"]}, foo={"provides": ["libfoo.so=1-64"]}
        )
        self.assertIn("foo", freeze.resolve(["app"], packages))

    def test_several_providers_are_never_guessed(self):
        packages = parsed(
            app={"depends": ["initramfs"]},
            mkinitcpio={"provides": ["initramfs"]},
            dracut={"provides": ["initramfs"]},
        )
        with self.assertRaisesRegex(freeze.Refused, "provider-choices"):
            freeze.resolve(["app"], packages)
        resolved = freeze.resolve(["app"], packages, {"initramfs": "dracut"})
        self.assertIn("dracut", resolved)
        self.assertNotIn("mkinitcpio", resolved)
        with self.assertRaisesRegex(freeze.Refused, "does not provide"):
            freeze.resolve(["app"], packages, {"initramfs": "app"})

    def test_a_name_the_builder_provides_adds_nothing(self):
        packages = parsed(
            app={"depends": ["initramfs"]},
            mkinitcpio={"provides": ["initramfs"], "depends": ["diffutils"]},
            dracut={"provides": ["initramfs"]},
            diffutils={},
        )
        resolved = freeze.resolve(["app"], packages, {"initramfs": freeze.ASSUMED})
        self.assertEqual(sorted(resolved), ["app"])

    def test_an_already_selected_provider_satisfies_a_virtual_name(self):
        packages = parsed(
            app={"depends": ["initramfs"]},
            mkinitcpio={"provides": ["initramfs"]},
            dracut={"provides": ["initramfs"]},
        )
        self.assertEqual(
            sorted(freeze.resolve(["app", "dracut"], packages)), ["app", "dracut"]
        )

    def test_an_unknown_dependency_is_refused(self):
        with self.assertRaisesRegex(freeze.Refused, "cannot resolve 'ghost'"):
            freeze.resolve(["app"], parsed(app={"depends": ["ghost"]}))

    def test_choice_and_request_files_are_strict(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = pathlib.Path(tmp)
            (tmp / "ok").write_text("# c\nlibx.so = iptables  # why\n")
            self.assertEqual(freeze.read_choices(tmp / "ok"), {"libx.so": "iptables"})
            (tmp / "dup").write_text("a = b\na = c\n")
            with self.assertRaises(freeze.Refused):
                freeze.read_choices(tmp / "dup")
            (tmp / "bad").write_text("no separator\n")
            with self.assertRaises(freeze.Refused):
                freeze.read_choices(tmp / "bad")
            (tmp / "unsorted").write_text("b\na\n")
            with self.assertRaisesRegex(freeze.Refused, "sorted"):
                freeze.read_requested(tmp / "unsorted")


class PolicyTests(unittest.TestCase):
    def test_the_committed_policy_is_well_formed(self):
        policy = freeze.load_policy(ROOT / "distro/base-image/arch/trust-policy.json")
        self.assertEqual(policy["keyring"]["name"], "archlinux-keyring")

    def test_malformed_policies_are_refused(self):
        good = json.loads(
            (ROOT / "distro/base-image/arch/trust-policy.json").read_text()
        )
        with tempfile.TemporaryDirectory() as tmp:
            for mutate in (
                lambda p: p.update(extra=1),
                lambda p: p.update(schema="other"),
                lambda p: p["keyring"].update(sha256="ABC"),
                lambda p: p["keyring"].update(name="not-the-keyring"),
                lambda p: p["keyring"].pop("version"),
            ):
                policy = json.loads(json.dumps(good))
                mutate(policy)
                path = pathlib.Path(tmp) / "p.json"
                path.write_text(json.dumps(policy))
                with self.assertRaises(freeze.Refused):
                    freeze.load_policy(path)


@unittest.skipUnless(GPG, "gpg is required")
class SignatureTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def gpg(self, home, *args, stdin=None):
        return subprocess.run(
            [
                GPG,
                "--homedir",
                str(home),
                "--batch",
                "--no-tty",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                *args,
            ],
            input=stdin,
            capture_output=True,
            check=True,
        ).stdout

    def key(self, name):
        home = self.dir / name
        home.mkdir(mode=0o700)
        self.gpg(
            home,
            "--quick-generate-key",
            f"{name} <{name}@example.org>",
            "ed25519",
            "sign",
            "1d",
        )
        fingerprint = next(
            line.split(":")[9]
            for line in self.gpg(home, "--list-keys", "--with-colons")
            .decode()
            .splitlines()
            if line.startswith("fpr")
        )
        keyring = self.dir / f"{name}.gpg"
        keyring.write_bytes(self.gpg(home, "--export"))
        return home, fingerprint, keyring

    def sign(self, home, data):
        path = self.dir / "data"
        path.write_bytes(data)
        signature = self.dir / "data.sig"
        signature.write_bytes(self.gpg(home, "--detach-sign", "-o", "-", str(path)))
        return path, signature

    def test_good_signature_returns_the_signers_primary_fingerprint(self):
        home, fingerprint, keyring = self.key("packager")
        data, signature = self.sign(home, b"payload")
        self.assertEqual(
            freeze.verify_signature(keyring, signature, data, set()), fingerprint
        )

    def test_a_revoked_signer_is_refused(self):
        home, fingerprint, keyring = self.key("packager")
        data, signature = self.sign(home, b"payload")
        with self.assertRaisesRegex(freeze.Refused, "revoked"):
            freeze.verify_signature(keyring, signature, data, {fingerprint})

    def test_a_key_outside_the_keyring_and_changed_bytes_are_refused(self):
        home, _, _ = self.key("packager")
        _, _, other_keyring = self.key("stranger")
        data, signature = self.sign(home, b"payload")
        with self.assertRaises(freeze.Refused):
            freeze.verify_signature(other_keyring, signature, data, set())
        _, _, keyring = self.dir / "x", None, self.dir / "packager.gpg"
        data.write_bytes(b"changed")
        with self.assertRaises(freeze.Refused):
            freeze.verify_signature(keyring, signature, data, set())


@unittest.skipUnless(GPG, "gpg is required")
class FreezeTests(unittest.TestCase):
    """The whole freeze against a fake archive and a synthetic keyring."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)
        home = self.dir / "gnupg"
        home.mkdir(mode=0o700)
        self.home = home
        self.gpg(
            "--quick-generate-key", "Packager <p@example.org>", "ed25519", "sign", "1d"
        )
        self.primary = next(
            line.split(":")[9]
            for line in self.gpg("--list-keys", "--with-colons").decode().splitlines()
            if line.startswith("fpr")
        )
        self.keyring = self.gpg("--export")
        self.files = {}
        self.build()

    def gpg(self, *args):
        return subprocess.run(
            [
                GPG,
                "--homedir",
                str(self.home),
                "--batch",
                "--no-tty",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                *args,
            ],
            capture_output=True,
            check=True,
        ).stdout

    def sign(self, data):
        path = self.dir / "to-sign"
        path.write_bytes(data)
        return self.gpg("--detach-sign", "-o", "-", str(path))

    def serve(self, repository, name, version, data, **spec):
        filename = f"{name}-{version}-x86_64.pkg.tar.zst"
        signature = self.sign(data)
        self.files[f"{repository}/os/x86_64/{filename}"] = data
        self.files[f"{repository}/os/x86_64/{filename}.sig"] = signature
        return desc(name, version, filename, data, signature=signature, **spec)

    def build(self):
        keyring_pkg = b"keyring-package-bytes"
        core = {
            "base-1-1": self.serve(
                "core", "base", "1-1", b"base-bytes", depends=["glibc"]
            ),
            "glibc-2-1": self.serve("core", "glibc", "2-1", b"glibc-bytes"),
            "archlinux-keyring-1-1": self.serve(
                "core", "archlinux-keyring", "1-1", keyring_pkg
            ),
        }
        database(self.dir / "core.db", core)
        database(self.dir / "extra.db", {})
        self.files["core/os/x86_64/core.db"] = (self.dir / "core.db").read_bytes()
        self.files["extra/os/x86_64/extra.db"] = (self.dir / "extra.db").read_bytes()
        self.pin = hashlib.sha256(keyring_pkg).hexdigest()
        policy = {
            "schema": freeze.POLICY_SCHEMA,
            "snapshot_date": "2026/11/09",
            "keyring": {
                "name": "archlinux-keyring",
                "version": "1-1",
                "filename": "archlinux-keyring-1-1-x86_64.pkg.tar.zst",
                "sha256": self.pin,
            },
        }
        self.policy = self.dir / "policy.json"
        self.policy.write_text(json.dumps(policy))
        (self.dir / "requested").write_text("base\n")
        (self.dir / "choices").write_text("")

    def fetcher(self, tamper=None):
        def fetch(url, destination, limit):
            key = url.removeprefix(freeze.ARCHIVE + "2026/11/09/")
            data = self.files[key]
            if tamper:
                data = tamper(key, data)
            pathlib.Path(destination).write_bytes(data)
            return hashlib.sha256(data).hexdigest()

        return fetch

    def args(self):
        out = self.dir / "out"
        out.mkdir(exist_ok=True)
        return mock.Mock(
            date="2026/11/09",
            policy=self.policy,
            requested=self.dir / "requested",
            choices=self.dir / "choices",
            cache=self.dir / "cache",
            out=out / "manifest.json",
            evidence=out / "evidence.json",
        )

    def run_freeze(self, tamper=None):
        with mock.patch.object(
            freeze, "extract_keyring", return_value=(self.keyring, set())
        ):
            return freeze.freeze(self.args(), self.fetcher(tamper))

    def test_a_clean_freeze_writes_the_canonical_manifest_and_content_addressed_cache(
        self,
    ):
        evidence = self.run_freeze()
        args = self.args()
        raw = args.out.read_bytes()
        manifest = json.loads(raw)
        self.assertEqual(
            raw, (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
        )
        self.assertEqual(hashlib.sha256(raw).hexdigest(), evidence["manifest_sha256"])
        self.assertEqual(
            [p["name"] for p in manifest["packages"]],
            ["archlinux-keyring", "base", "glibc"],
        )
        self.assertEqual(manifest["requested"], ["base"])
        self.assertTrue(
            all(p["signer_fingerprint"] == self.primary for p in manifest["packages"])
        )
        for package in manifest["packages"]:
            for digest in (package["sha256"], package["signature_sha256"]):
                self.assertTrue((args.cache / "sha256" / digest).is_file())

    def test_a_tampered_archive_fails_before_anything_is_stored(self):
        with self.assertRaisesRegex(
            freeze.Refused, "does not match the repository database"
        ):
            self.run_freeze(
                lambda key, data: (
                    b"evil" if key.endswith("glibc-2-1-x86_64.pkg.tar.zst") else data
                )
            )
        self.assertFalse((self.dir / "out/manifest.json").exists())

    def test_a_signature_that_differs_from_the_databases_is_refused(self):
        def swap(key, data):
            return (
                self.sign(b"other")
                if key.endswith("glibc-2-1-x86_64.pkg.tar.zst.sig")
                else data
            )

        with self.assertRaisesRegex(
            freeze.Refused, "differs from the database's PGPSIG"
        ):
            self.run_freeze(swap)

    def test_a_keyring_other_than_the_pinned_one_is_refused(self):
        def swap(key, data):
            return (
                b"not the keyring"
                if key.endswith("archlinux-keyring-1-1-x86_64.pkg.tar.zst")
                else data
            )

        with self.assertRaisesRegex(freeze.Refused, "pinned in the trust policy"):
            self.run_freeze(swap)

    def test_a_policy_for_another_date_is_refused(self):
        policy = json.loads(self.policy.read_text())
        policy["snapshot_date"] = "2026/11/10"
        self.policy.write_text(json.dumps(policy))
        with self.assertRaisesRegex(freeze.Refused, "trust policy is for"):
            self.run_freeze()

    def test_a_revoked_signer_stops_the_freeze(self):
        with (
            mock.patch.object(
                freeze, "extract_keyring", return_value=(self.keyring, {self.primary})
            ),
            self.assertRaisesRegex(freeze.Refused, "revoked"),
        ):
            freeze.freeze(self.args(), self.fetcher())


if __name__ == "__main__":
    unittest.main()
