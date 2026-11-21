#!/usr/bin/env python3
"""Offline contract of the Evolution Lab guest probe (ADR-0024 D2, #411)."""

import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
PROBE = ROOT / "testbed/evolution-world/lab-probe.py"
UNIT = ROOT / "testbed/evolution-world/systemd/flowerly-lab-probe.service"
SYSUSERS = ROOT / "testbed/evolution-world/sysusers/flowerly-lab-probe.conf"
INSTALLER = ROOT / "scripts/prepare-lab-probe-rootfs.sh"
HOST_REPORT = ROOT / "src/evolutiond/src/lab/report.rs"
# Parsed by the host parser's own tests (report.rs); regenerate with
# FLOWERLY_REGENERATE_LAB_FIXTURE=1 after a deliberate report change.
CONSOLE_FIXTURE = ROOT / "testbed/evolution-world/fixtures/lab-report.console"
spec = importlib.util.spec_from_file_location("lab_probe", PROBE)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)

UUID_B = "8a3c4b6e-1d2f-4e5a-9b7c-0d1e2f3a4b5c"
IDENTITY = (
    '{"schema":"flowerly-generation-root/v2","slot":"root-b","root_partuuid":"'
    + UUID_B
    + '","source_commit":"'
    + "a" * 40
    + '","input_manifest_sha256":"'
    + "b" * 64
    + '","build_id":"lab-b","schema_version":16,"min_reader":16}\n'
)
PACKAGES = {"linux": "6.17.1.arch1-1", "bash": "5.3.3-2", "glibc": "2.42+r17-1"}
LAB = ["runuser", "-u", "flowerly-lab", "--"]
CTL = LAB + ["/usr/bin/agenticctl", "--runtime-dir", "/run/flowerly"]
GEN = "gen-01J9ZK"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write(path, data, mode=0o644):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data if isinstance(data, bytes) else data.encode())
    path.chmod(mode)
    return path


def make_root(base, packages=PACKAGES, identity=IDENTITY):
    root = Path(base) / "root"
    write(root / probe.IDENTITY, identity)
    local = root / probe.PACMAN_LOCAL
    write(local / "ALPM_DB_VERSION", "9\n")
    for name, version in packages.items():
        write(
            local / ("%s-%s" % (name, version)) / "desc",
            "%%NAME%%\n%s\n\n%%VERSION%%\n%s\n\n%%BASE%%\n%s\n" % (name, version, name),
        )
    for name, relative in probe.BINARIES.items():
        write(root / relative, ("binary " + name).encode(), 0o755)
    return root


def package_digest(packages=PACKAGES):
    lines = sorted("%s %s\n" % item for item in packages.items())
    return sha("".join(lines).encode())


def make_disk(base, partname="root-b", devname="vda3", links=(UUID_B,)):
    """A /sys and /dev pair for block device 254:3."""
    sys_root, dev_root = Path(base) / "sys", Path(base) / "dev"
    write(
        sys_root / "dev/block/254:3/uevent",
        "MAJOR=254\nMINOR=3\nDEVNAME=%s\nDEVTYPE=partition\nPARTN=3\nPARTNAME=%s\n"
        % (devname, partname),
    )
    write(dev_root / devname, b"")
    write(dev_root / "vda2", b"")
    by = dev_root / "disk/by-partuuid"
    by.mkdir(parents=True)
    (by / "11111111-2222-4333-8444-555555555555").symlink_to("../../vda2")
    for link in links:
        (by / link).symlink_to("../../" + devname)
    return sys_root, dev_root, os.makedev(254, 3)


SHOW_BOOT = (
    "FirmwareTimestampMonotonic=5000000\nLoaderTimestampMonotonic=2000000\n"
    "InitRDTimestampMonotonic=1500000\nUserspaceTimestampMonotonic=3000000\n"
    "FinishTimestampMonotonic=0\n"
)
SHOW_HEALTH = (
    "LoadState=loaded\nActiveState=inactive\nResult=success\n"
    "ExecMainStartTimestampMonotonic=4100000\nExecMainStatus=0\n"
)


class FakeSystem:
    """Answers the probe's commands; every argv is recorded."""

    def __init__(self, **override):
        self.calls = []
        self.answers = {
            "show-boot": (0, SHOW_BOOT, ""),
            "show-health": (0, SHOW_HEALTH, ""),
            "failed": (0, "", ""),
            "is-active": (0, "", ""),
            "status": (0, '{"service":"flowerlyd"}\n', ""),
            "self-test": (0, "evolutiond self-test ok\n", ""),
            "create": (0, json.dumps({"id": GEN, "status": "CANDIDATE"}), ""),
            "reject": (0, json.dumps({"id": GEN, "status": "REJECTED"}), ""),
            "inspect": (
                0,
                json.dumps({"id": GEN, "status": "REJECTED", "parent": None}),
                "",
            ),
        }
        self.answers.update(override)

    def key(self, argv):
        if argv[:2] == ["systemctl", "show"]:
            return "show-health" if argv[-1] == probe.HEALTH_UNIT else "show-boot"
        if argv[:2] == ["systemctl", "list-units"]:
            return "failed"
        if argv[:2] == ["systemctl", "is-active"]:
            return "is-active"
        if argv[-1] == "--self-test":
            return "self-test"
        if argv[-1] == "status":
            return "status"
        return argv[len(CTL) + 1]

    def __call__(self, argv):
        self.calls.append(argv)
        answer = self.answers[self.key(argv)]
        if isinstance(answer, BaseException):
            raise answer
        return answer


def decode(lines):
    parts = [line.split(" ", 2) for line in lines[:-1]]
    body = base64.b64decode("".join(chunk for _, _, chunk in parts))
    done = re.fullmatch(
        r"FLOWERLY_LAB_REPORT_DONE parts=(\d+) sha256=([0-9a-f]{64})", lines[-1]
    )
    return body, done


def rust_fields(name):
    """The field names of `pub struct <name>` in the host's report parser."""
    source = HOST_REPORT.read_text()
    body = re.search(r"pub struct %s \{(.*?)\n\}" % name, source, re.S).group(1)
    return set(re.findall(r"^\s*pub (\w+):", body, re.M))


class FileFactTests(unittest.TestCase):
    def test_file_facts_of_a_root(self):
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d)
            facts = probe.file_facts(root)
        self.assertEqual(facts["identity_sha256"], sha(IDENTITY.encode()))
        self.assertEqual(facts["build_id"], "lab-b")
        self.assertEqual(facts["packages_sha256"], package_digest())
        self.assertEqual(facts["package_count"], 3)
        self.assertEqual(
            facts["binaries"],
            {name: sha(("binary " + name).encode()) for name in probe.BINARIES},
        )

    def test_package_digest_ignores_directory_order_and_other_fields(self):
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d)
            desc = root / probe.PACMAN_LOCAL / "bash-5.3.3-2" / "desc"
            desc.write_text(desc.read_text() + "%DESC%\nThe GNU shell\n")
            self.assertEqual(probe.packages(root), (package_digest(), 3))

    def test_one_unreadable_package_makes_the_set_unknown(self):
        for broken in (
            "%NAME%\nbash\n",
            "%VERSION%\n5.3\n",
            "%NAME%\nba sh\n\n%VERSION%\n5.3\n",
            b"\xff\xfe",
        ):
            with tempfile.TemporaryDirectory() as d:
                root = make_root(d)
                write(root / probe.PACMAN_LOCAL / "bash-5.3.3-2" / "desc", broken)
                self.assertEqual(probe.packages(root), (None, None), broken)
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d)
            (root / probe.PACMAN_LOCAL / "stray").mkdir()
            self.assertEqual(probe.packages(root), (None, None))
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(probe.packages(make_root(d, packages={})), (None, None))
            self.assertEqual(probe.packages(Path(d) / "missing"), (None, None))

    def test_identity_bounds(self):
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d, identity=IDENTITY.replace("lab-b", "../b"))
            digest, build_id = probe.identity(root)
            self.assertEqual(digest, sha(IDENTITY.replace("lab-b", "../b").encode()))
            self.assertIsNone(build_id)
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d, identity="not json")
            self.assertEqual(probe.identity(root)[1], None)
            self.assertIsNotNone(probe.identity(root)[0])
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d, identity=" " * (probe.MAX_IDENTITY + 1))
            self.assertEqual(probe.identity(root), (None, None))
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d)
            target = root / probe.IDENTITY
            target.rename(root / "elsewhere.json")
            target.symlink_to(root / "elsewhere.json")
            self.assertEqual(probe.identity(root), (None, None))

    def test_a_symlinked_or_missing_binary_is_not_reported(self):
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d)
            flowerlyd = root / probe.BINARIES["flowerlyd"]
            flowerlyd.unlink()
            flowerlyd.symlink_to(root / probe.BINARIES["agenticctl"])
            (root / probe.BINARIES["evolutiond"]).unlink()
            self.assertEqual(
                sorted(probe.binaries(root)), ["agenticctl", "flowerly-slot-broker"]
            )


class BootFactTests(unittest.TestCase):
    def test_the_mounted_partition_names_slot_and_partuuid(self):
        with tempfile.TemporaryDirectory() as d:
            sys_root, dev_root, device = make_disk(d)
            self.assertEqual(
                probe.booted_root(device, str(sys_root), str(dev_root)),
                ("root-b", UUID_B),
            )

    def test_unknown_partitions_are_null(self):
        with tempfile.TemporaryDirectory() as d:
            sys_root, dev_root, device = make_disk(d, partname="data")
            self.assertEqual(
                probe.booted_root(device, str(sys_root), str(dev_root)), (None, UUID_B)
            )
        with tempfile.TemporaryDirectory() as d:
            sys_root, dev_root, device = make_disk(d, links=())
            self.assertEqual(
                probe.booted_root(device, str(sys_root), str(dev_root)),
                ("root-b", None),
            )
        with tempfile.TemporaryDirectory() as d:
            sys_root, dev_root, device = make_disk(
                d, links=(UUID_B, "99999999-2222-4333-8444-555555555555")
            )
            self.assertEqual(
                probe.booted_root(device, str(sys_root), str(dev_root)),
                ("root-b", None),
            )
        with tempfile.TemporaryDirectory() as d:
            sys_root, dev_root, device = make_disk(d)
            uevent = sys_root / "dev/block/254:3/uevent"
            uevent.write_text(
                uevent.read_text().replace("DEVNAME=vda3", "DEVNAME=../vda3")
            )
            self.assertEqual(
                probe.booted_root(device, str(sys_root), str(dev_root)),
                ("root-b", None),
            )
        with tempfile.TemporaryDirectory() as d:
            sys_root, dev_root, _ = make_disk(d)
            self.assertEqual(
                probe.booted_root(os.makedev(8, 1), str(sys_root), str(dev_root)),
                (None, None),
            )

    def test_the_kernel_image_is_the_slots_esp_copy(self):
        with tempfile.TemporaryDirectory() as d:
            write(Path(d) / "flowerly/a/vmlinuz", b"kernel a")
            write(Path(d) / "flowerly/b/vmlinuz", b"kernel b")
            self.assertEqual(
                probe.kernel("root-b", d, "6.17.1-arch1-1"),
                ("6.17.1-arch1-1", sha(b"kernel b")),
            )
            self.assertEqual(
                probe.kernel(None, d, "6.17.1-arch1-1"), ("6.17.1-arch1-1", None)
            )
            self.assertEqual(probe.kernel("root-a", d, "r")[1], sha(b"kernel a"))
            self.assertEqual(
                probe.kernel("root-b", Path(d) / "nothing", "r"), ("r", None)
            )

    def test_boot_phases_follow_systemd_analyze(self):
        system = FakeSystem()
        self.assertEqual(
            probe.boot_phases(system, now_us=10_000_000),
            {
                "firmware": 3000.0,
                "loader": 2000.0,
                "kernel": 1500.0,
                "initrd": 1500.0,
                "userspace": 7000.0,
            },
        )
        self.assertEqual(system.calls[0][:2], ["systemctl", "show"])
        finished = SHOW_BOOT.replace(
            "InitRDTimestampMonotonic=1500000", "InitRDTimestampMonotonic=0"
        ).replace("FinishTimestampMonotonic=0", "FinishTimestampMonotonic=8000000")
        self.assertEqual(
            probe.boot_phases(FakeSystem(**{"show-boot": (0, finished, "")}), now_us=1),
            {
                "firmware": 3000.0,
                "loader": 2000.0,
                "kernel": 3000.0,
                "userspace": 5000.0,
            },
        )
        no_firmware = "UserspaceTimestampMonotonic=1000\n"
        self.assertEqual(
            probe.boot_phases(
                FakeSystem(**{"show-boot": (0, no_firmware, "")}), now_us=3000
            ),
            {"kernel": 1.0, "userspace": 2.0},
        )
        self.assertEqual(
            probe.boot_phases(FakeSystem(**{"show-boot": (1, "", "")})), {}
        )
        self.assertEqual(
            probe.boot_phases(
                FakeSystem(**{"show-boot": (0, "LoaderTimestampMonotonic=x", "")})
            ),
            {},
        )


class ObservationTests(unittest.TestCase):
    def test_health_gate_reads_the_oneshot_result(self):
        self.assertEqual(probe.health_gate(FakeSystem()), "passed")
        cases = {
            "failed": SHOW_HEALTH.replace(
                "ActiveState=inactive", "ActiveState=failed"
            ).replace("Result=success", "Result=exit-code"),
            "never ran": SHOW_HEALTH.replace("=4100000", "=0"),
            "nonzero": SHOW_HEALTH.replace("ExecMainStatus=0", "ExecMainStatus=1"),
            "still running": SHOW_HEALTH.replace(
                "ActiveState=inactive", "ActiveState=activating"
            ),
            "masked": SHOW_HEALTH.replace("LoadState=loaded", "LoadState=masked"),
        }
        for name, text in cases.items():
            self.assertEqual(
                probe.health_gate(FakeSystem(**{"show-health": (0, text, "")})),
                "failed",
                name,
            )
        absent = "LoadState=not-found\nActiveState=inactive\nResult=success\n"
        self.assertEqual(
            probe.health_gate(FakeSystem(**{"show-health": (0, absent, "")})), "absent"
        )
        self.assertIsNone(
            probe.health_gate(FakeSystem(**{"show-health": (None, "", "")}))
        )

    def test_failed_units(self):
        listing = (
            "flowerlyd.service loaded failed failed Flowerly daemon\n"
            "a\x1bbad.service loaded failed failed X\n"
            "boot-efi.mount loaded failed failed EFI\n"
        )
        self.assertEqual(
            probe.failed_units(FakeSystem(failed=(0, listing, ""))),
            ["(unprintable unit)", "boot-efi.mount", "flowerlyd.service"],
        )
        self.assertEqual(
            probe.failed_units(FakeSystem(failed=(1, "", ""))), ["(unknown)"]
        )
        many = "".join("u%03d.service loaded failed failed x\n" % i for i in range(100))
        self.assertEqual(len(probe.failed_units(FakeSystem(failed=(0, many, "")))), 64)

    def test_admin_roundtrip_creates_rejects_and_reads_back_as_the_lab_account(self):
        system = FakeSystem()
        self.assertTrue(probe.admin_roundtrip(system))
        self.assertEqual(
            system.calls,
            [
                CTL + ["generation", "create"],
                CTL + ["generation", "reject", GEN, "--reason", probe.ROUNDTRIP_REASON],
                CTL + ["generation", "inspect", GEN],
            ],
        )

    def test_admin_roundtrip_fails_closed(self):
        other = json.dumps({"id": "gen-other", "status": "REJECTED"})
        cases = {
            "create refused": {"create": (1, "", "service error")},
            "create not json": {"create": (0, "ok", "")},
            "created not a candidate": {
                "create": (0, json.dumps({"id": GEN, "status": "TESTED"}), "")
            },
            "created id unsafe": {
                "create": (0, json.dumps({"id": "../x", "status": "CANDIDATE"}), "")
            },
            "reject refused": {"reject": (1, "", "not an Admin")},
            "rejected another": {"reject": (0, other, "")},
            "inspect refused": {"inspect": (1, "", "")},
            "inspect still candidate": {
                "inspect": (0, json.dumps({"id": GEN, "status": "CANDIDATE"}), "")
            },
            "inspect another": {"inspect": (0, other, "")},
        }
        for name, override in cases.items():
            self.assertFalse(probe.admin_roundtrip(FakeSystem(**override)), name)

    def test_observations(self):
        self.assertEqual(
            probe.observations(FakeSystem()),
            {
                "health_gate": "passed",
                "flowerlyd_active": True,
                "status_ok": True,
                "admin_roundtrip": True,
                "evolutiond_self_test": True,
            },
        )
        seen = probe.observations(
            FakeSystem(
                **{
                    "is-active": (3, "", ""),
                    "status": (None, "", ""),
                    "self-test": (1, "", ""),
                }
            )
        )
        self.assertEqual(
            (seen["flowerlyd_active"], seen["status_ok"], seen["evolutiond_self_test"]),
            (False, None, False),
        )
        system = FakeSystem()
        probe.observations(system)
        for argv in system.calls:
            if "agenticctl" in " ".join(argv) or "evolutiond" in " ".join(argv):
                self.assertEqual(argv[:4], LAB, argv)

    def test_run_reports_timeouts_as_failure_and_missing_tools_as_unknown(self):
        with mock.patch.object(
            probe.subprocess, "run", side_effect=subprocess.TimeoutExpired("x", 1)
        ):
            self.assertEqual(probe.run(["x"])[0], 124)
        with mock.patch.object(
            probe.subprocess, "run", side_effect=FileNotFoundError("x")
        ):
            self.assertIsNone(probe.run(["x"])[0])


class ReportTests(unittest.TestCase):
    def full_report(self, d):
        root = make_root(d)
        write(Path(d) / "efi/flowerly/b/vmlinuz", b"kernel b")
        sys_root, dev_root, device = make_disk(d)
        return probe.probe(
            FakeSystem(),
            root=root,
            efi_root=Path(d) / "efi",
            sys_root=str(sys_root),
            dev_root=str(dev_root),
            device=device,
            release="6.17.1-arch1-1",
            now_us=10_000_000,
        )

    def test_report_keys_are_exactly_the_hosts_v1_fields(self):
        with tempfile.TemporaryDirectory() as d:
            report = self.full_report(d)
        self.assertEqual(set(report), rust_fields("ProbeReport"))
        self.assertEqual(set(report["facts"]), rust_fields("Facts"))
        self.assertEqual(set(report["observations"]), rust_fields("Observations"))
        self.assertIn(
            'REPORT_SCHEMA: &str = "%s"' % probe.SCHEMA, HOST_REPORT.read_text()
        )

    def test_a_good_boot_reports_every_fact(self):
        with tempfile.TemporaryDirectory() as d:
            report = self.full_report(d)
        facts = report["facts"]
        self.assertEqual((facts["slot"], facts["root_partuuid"]), ("root-b", UUID_B))
        self.assertEqual(facts["vmlinuz_sha256"], sha(b"kernel b"))
        self.assertEqual(facts["kernel_release"], "6.17.1-arch1-1")
        self.assertEqual(facts["build_id"], "lab-b")
        self.assertEqual(facts["failed_units"], [])
        self.assertEqual(len(facts["boot_phases_ms"]), 5)
        self.assertTrue(all(report["observations"].values()))

    def test_the_hosts_fixture_is_what_the_probe_emits(self):
        with tempfile.TemporaryDirectory() as d:
            text = "\n".join(probe.report_lines(self.full_report(d))) + "\n"
        if os.environ.get("FLOWERLY_REGENERATE_LAB_FIXTURE") == "1":
            CONSOLE_FIXTURE.parent.mkdir(parents=True, exist_ok=True)
            CONSOLE_FIXTURE.write_text(text)
        self.assertEqual(CONSOLE_FIXTURE.read_text(), text)
        self.assertIn("lab-report.console", HOST_REPORT.read_text())

    def test_report_lines_decode_to_the_report(self):
        with tempfile.TemporaryDirectory() as d:
            report = self.full_report(d)
        lines = probe.report_lines(report)
        body, done = decode(lines)
        self.assertEqual(json.loads(body), report)
        self.assertEqual(done.group(2), sha(body))
        self.assertEqual(int(done.group(1)), len(lines) - 1)
        self.assertGreater(len(lines), 2)
        for index, line in enumerate(lines[:-1], 1):
            self.assertRegex(
                line,
                r"^FLOWERLY_LAB_REPORT part=%d/%d [A-Za-z0-9+/=]{1,480}$"
                % (index, len(lines) - 1),
            )

    def test_console_emitter_writes_every_byte_then_drains(self):
        written, drained, echoed = [], [], []

        def short_write(fd, data):
            written.append(bytes(data[:7]))
            return min(7, len(data))

        emit = probe.console_emitter(
            "/dev/console",
            opener=lambda path, flags: 42,
            write=short_write,
            drain=drained.append,
            echo=echoed.append,
        )
        emit("FLOWERLY_LAB_REPORT_DONE parts=1 sha256=" + "0" * 64)
        self.assertEqual(
            b"".join(written),
            b"FLOWERLY_LAB_REPORT_DONE parts=1 sha256=" + b"0" * 64 + b"\n",
        )
        self.assertEqual(drained, [42])
        self.assertEqual(len(echoed), 1)

    def test_offline_mode_prints_the_file_facts(self):
        with tempfile.TemporaryDirectory() as d:
            root = make_root(d)
            out = io.StringIO()
            with mock.patch.object(probe.sys, "stdout", out):
                self.assertEqual(probe.main(["--offline", str(root)]), 0)
            self.assertEqual(json.loads(out.getvalue()), probe.file_facts(root))
        with mock.patch.object(probe.sys, "stderr", io.StringIO()):
            self.assertEqual(probe.main(["--bogus"]), 2)


def prepared_root(base):
    root = Path(base) / "rootfs"
    for file in (
        "usr/lib/flowerly/generation-ab-test-profile",
        "usr/lib/flowerly/generation-root.json",
        "usr/lib/systemd/system/flowerly-generation-health.service",
        "etc/systemd/system/flowerlyd.service.d/50-multi-user.conf",
        "usr/lib/sysusers.d/flowerly.conf",
        "usr/bin/python3",
        "usr/bin/runuser",
    ):
        write(root / file, "")
    (root / "etc/systemd/system/multi-user.target.wants").mkdir(parents=True)
    return root


def install(root, *args):
    fake = Path(root).parent / "fake-sysusers"
    fake.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@" >> "$(dirname "$0")/sysusers.log"\n'
    )
    fake.chmod(0o755)
    env = dict(os.environ, FLOWERLY_SYSUSERS=str(fake))
    cmd = ["bash", str(INSTALLER), str(root)] + list(args or ["--test-profile"])
    return subprocess.run(cmd, capture_output=True, text=True, env=env)


class InstallerTests(unittest.TestCase):
    def test_installs_probe_unit_account_and_bakes_it(self):
        with tempfile.TemporaryDirectory() as d:
            root = prepared_root(d)
            result = install(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            installed = {
                "usr/lib/flowerly/lab-probe.py": (PROBE, 0o755),
                "usr/lib/systemd/system/flowerly-lab-probe.service": (UNIT, 0o644),
                "usr/lib/sysusers.d/flowerly-lab-probe.conf": (SYSUSERS, 0o644),
            }
            for path, (source, mode) in installed.items():
                target = root / path
                self.assertEqual(target.read_bytes(), source.read_bytes(), path)
                self.assertEqual(stat.S_IMODE(target.stat().st_mode), mode, path)
            wants = (
                root
                / "etc/systemd/system/multi-user.target.wants/flowerly-lab-probe.service"
            )
            self.assertEqual(
                os.readlink(wants), "/usr/lib/systemd/system/flowerly-lab-probe.service"
            )
            log = (Path(d) / "sysusers.log").read_text()
            self.assertEqual(log, "--root=%s\n" % root)

    def test_refuses_without_the_test_profile_flag(self):
        with tempfile.TemporaryDirectory() as d:
            root = prepared_root(d)
            for args in (["--shipped"], ["--test-profile", "extra"]):
                result = install(root, *args)
                self.assertNotEqual(result.returncode, 0, args)
            self.assertFalse((root / "usr/lib/flowerly/lab-probe.py").exists())

    def test_refuses_a_rootfs_that_is_not_an_ab_test_image(self):
        for missing in (
            "usr/lib/flowerly/generation-ab-test-profile",
            "etc/systemd/system/flowerlyd.service.d/50-multi-user.conf",
            "usr/lib/flowerly/generation-root.json",
        ):
            with tempfile.TemporaryDirectory() as d:
                root = prepared_root(d)
                (root / missing).unlink()
                result = install(root)
                self.assertNotEqual(result.returncode, 0, missing)
                self.assertIn("not a prepared A/B test rootfs", result.stderr)
                self.assertFalse((root / "usr/lib/flowerly/lab-probe.py").exists())

    def test_refuses_existing_paths_and_symlinked_destinations(self):
        with tempfile.TemporaryDirectory() as d:
            root = prepared_root(d)
            write(root / "usr/lib/sysusers.d/flowerly-lab-probe.conf", "u x - - /\n")
            result = install(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("refusing existing probe installation path", result.stderr)
            self.assertFalse((root / "usr/lib/flowerly/lab-probe.py").exists())
        with tempfile.TemporaryDirectory() as d:
            root = prepared_root(d)
            wants = root / "etc/systemd/system/multi-user.target.wants"
            wants.rmdir()
            (Path(d) / "outside").mkdir()
            wants.symlink_to(Path(d) / "outside")
            result = install(root)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("symlinked rootfs destination", result.stderr)
            self.assertEqual(list((Path(d) / "outside").iterdir()), [])

    def test_a_failed_bake_fails_the_installation(self):
        with tempfile.TemporaryDirectory() as d:
            root = prepared_root(d)
            fake = Path(d) / "failing"
            fake.write_text("#!/bin/sh\nexit 1\n")
            fake.chmod(0o755)
            env = dict(os.environ, FLOWERLY_SYSUSERS=str(fake))
            result = subprocess.run(
                ["bash", str(INSTALLER), str(root), "--test-profile"],
                capture_output=True,
                text=True,
                env=env,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("baking the probe account failed", result.stderr)


class UnitTests(unittest.TestCase):
    def test_unit_contract(self):
        text = UNIT.read_text()
        for line in (
            "ConditionPathExists=/usr/lib/flowerly/generation-ab-test-profile",
            "After=flowerly-generation-health.service flowerlyd.service flowerly-slot-broker.socket",
            "After=flowerly-g5-drill.service",
            "Before=getty.target serial-getty@ttyS0.service",
            "SuccessAction=poweroff",
            "FailureAction=poweroff",
            "Type=oneshot",
            "ExecStart=/usr/bin/python3 -I /usr/lib/flowerly/lab-probe.py",
            "StandardOutput=journal",
            "PrivateNetwork=yes",
            "WantedBy=multi-user.target",
        ):
            self.assertIn(line + "\n", text, line)
        # Ordering only: a failed health gate must not stop the report.
        self.assertNotRegex(text, r"(?m)^(Requires|BindsTo|Requisite)=")

    def test_the_probe_account_is_an_admin_and_not_the_service_account(self):
        entries = [
            line.split()
            for line in SYSUSERS.read_text().splitlines()
            if line and not line.startswith("#")
        ]
        self.assertEqual(entries[0][:2], ["u", "flowerly-lab"])
        self.assertIn(["m", "flowerly-lab", "flowerly-admin"], entries)
        self.assertIn(["m", "flowerly-lab", "flowerly-users"], entries)
        self.assertNotIn("flowerly", [entry[1] for entry in entries])
        self.assertEqual(probe.LAB_USER, "flowerly-lab")

    def test_the_probe_uses_only_the_standard_library(self):
        imports = set(re.findall(r"(?m)^(?:import|from) (\w+)", PROBE.read_text()))
        self.assertLessEqual(
            imports,
            {
                "base64",
                "hashlib",
                "json",
                "os",
                "pathlib",
                "re",
                "subprocess",
                "sys",
                "termios",
                "time",
            },
        )


if __name__ == "__main__":
    unittest.main()
