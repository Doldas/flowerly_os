#!/usr/bin/env python3
"""Offline contract of the Evolution Lab candidate build and run (#412)."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "scripts/lab-candidate.py"
BUILD = ROOT / "scripts/build-lab-candidate.sh"
VM_TEST = ROOT / "scripts/vm-test.sh"
PROBE = ROOT / "testbed/evolution-world/lab-probe.py"
HOST_PLAN = ROOT / "src/evolutiond/src/lab/plan.rs"
HEALTH_GATE = ROOT / "distro/overlay/usr/lib/flowerly/generation-health-gate.sh"
BAKER = ROOT / "scripts/prepare-generation-ab-rootfs.sh"
# Parsed by the host's own plan tests (plan.rs); regenerate with
# FLOWERLY_REGENERATE_LAB_FIXTURE=1 after a deliberate plan change.
PLAN_FIXTURE = ROOT / "testbed/evolution-world/fixtures/lab-plan-rollback.json"
INITIAL_FIXTURE = ROOT / "testbed/evolution-world/fixtures/lab-plan-initial.json"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


lab = load("lab_candidate", TOOL)

COMMIT = "c" * 40
MANIFEST = "d" * 64
UUID = {
    "root-a": "00010203-0405-4607-8809-0a0b0c0d0e0f",
    "root-b": "0f0e0d0c-0b0a-4908-8706-050403020100",
}
RELEASE = "6.17.1-arch1-1"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def write(path, data):
    path = Path(path)
    path.write_bytes(data if isinstance(data, bytes) else data.encode())
    return path


def identity(slot, **changes):
    value = {
        "schema": lab.IDENTITY_SCHEMA,
        "slot": slot,
        "root_partuuid": UUID[slot],
        "source_commit": COMMIT,
        "input_manifest_sha256": MANIFEST,
        "build_id": f"lab-{lab.SLOTS[slot]}",
        "schema_version": 16,
        "min_reader": 16,
    }
    value.update(changes)
    return json.dumps(value, separators=(",", ":"))


def make_build(base, identities=None, facts=None):
    """A finished build directory as build-lab-candidate.sh leaves it."""
    out = Path(base) / "build"
    out.mkdir()
    for slot, letter in lab.SLOTS.items():
        raw = (identities or {}).get(slot, identity(slot))
        write(out / f"identity-{letter}.json", raw)
        fact = {
            "identity_sha256": sha(raw.encode()),
            "build_id": f"lab-{letter}",
            "packages_sha256": "3" * 64,
            "package_count": 3,
            "binaries": {name: str(i) * 64 for i, name in enumerate(lab.BINARIES, 4)},
        }
        fact.update((facts or {}).get(slot, {}))
        write(out / f"facts-{letter}.json", json.dumps(fact))
        write(out / f"kernel-release-{letter}", RELEASE + "\n")
        write(out / f"root-{letter}.ext4", f"root volume {letter}")
    write(out / "vmlinuz", "kernel")
    write(out / "initramfs.img", "initramfs")
    write(out / "ab.img", b"\0" * 4096 + b"disk")
    write(out / "ab.img.manifest.json", "{}")
    write(out / "slot-tool", "#!/bin/false\n")
    return out


def firmware(base):
    code = write(Path(base) / "OVMF_CODE.fd", "code")
    vars_ = write(Path(base) / "OVMF_VARS.fd", "vars")
    return code, vars_


def tools(base):
    """Stand-ins for evolutiond and QEMU; the Runner answers their calls."""
    found = []
    for name in ("evolutiond", "qemu-system-x86_64"):
        path = write(Path(base) / name, "#!/bin/false\n")
        path.chmod(0o755)
        found.append(path)
    return found


def options(base, *extra, run="run"):
    code, vars_ = firmware(base)
    evolutiond, qemu = tools(base)
    argv = [
        "prepare",
        str(Path(base) / "build"),
        str(Path(base) / run),
        "--candidate-generation",
        "gen-candidate",
        "--baseline-generation",
        "gen-baseline",
        "--firmware-code",
        str(code),
        "--firmware-vars",
        str(vars_),
        "--evolutiond",
        str(evolutiond),
        "--qemu",
        str(qemu),
        *extra,
    ]
    return lab.parser().parse_args(argv)


class Runner:
    """Runs the copies for real; the initial boot and the arm only mark the
    copied disk, the boot after checking it is the disk its plan names."""

    def __init__(self, arm_status=0, boot_status=0):
        self.calls = []
        self.arm_status = arm_status
        self.boot_status = boot_status
        self.initial_plans = []

    def __call__(self, argv, check=False):
        self.calls.append(list(argv))
        if argv[0] == "cp":
            return subprocess.run(argv, check=check)
        if argv[1:3] == ["lab", "run"]:
            plan = json.loads(Path(argv[argv.index("--plan") + 1]).read_text())
            self.initial_plans.append(plan)
            disk = Path(plan["candidate"]["disk"])
            assert sha(disk.read_bytes()) == plan["candidate"]["disk_sha256"]
            with open(disk, "ab") as handle:
                handle.write(b"booted")
            return subprocess.CompletedProcess(argv, self.boot_status)
        disk = Path(argv[argv.index("--disk") + 1])
        with open(disk, "ab") as handle:
            handle.write(b"armed")
        return subprocess.CompletedProcess(argv, self.arm_status)

    def arm(self):
        (arm,) = [call for call in self.calls if call[1:3] == ["slot", "arm"]]
        return arm

    def boot(self):
        (boot,) = [call for call in self.calls if call[1:3] == ["lab", "run"]]
        return boot


# The fixture's build: fixed facts, so the plan is the same on every host.
FIXTURE_CANDIDATE = {
    "slots": {
        slot: {
            "partuuid": UUID[slot],
            "build_id": f"lab-{letter}",
            "identity_sha256": ("5" if letter == "a" else "1") * 64,
            "rootfs_sha256": "9" * 64,
            "kernel_release": RELEASE,
            "vmlinuz_sha256": "2" * 64,
            "packages_sha256": "3" * 64,
            "binaries": {
                name: ("6" if letter == "a" else "4") * 64
                for name in sorted(lab.BINARIES)
            },
        }
        for slot, letter in lab.SLOTS.items()
    }
}


def fixture_plan():
    request = lab.parser().parse_args(
        [
            "prepare",
            "/lab/build",
            "/lab/run",
            "--scenario",
            "rollback",
            "--candidate-generation",
            "gen-candidate",
            "--baseline-generation",
            "gen-baseline",
            "--firmware-code",
            "/fw/OVMF_CODE.fd",
            "--firmware-vars",
            "/fw/OVMF_VARS.fd",
            "--evolutiond",
            "/lab/bin/evolutiond",
            "--qemu",
            "/usr/bin/qemu-system-x86_64",
        ]
    )
    value = lab.plan(
        FIXTURE_CANDIDATE,
        request.scenario,
        request.tries,
        "/lab/run/candidate.img",
        "a" * 64,
        {"root-b": "gen-candidate", "root-a": "gen-baseline"},
        request,
    )
    return json.dumps(value, sort_keys=True, indent=2) + "\n"


def fixture_initial_plan():
    request = lab.parser().parse_args(
        [
            "prepare",
            "/lab/build",
            "/lab/run",
            "--candidate-generation",
            "gen-candidate",
            "--baseline-generation",
            "gen-baseline",
            "--firmware-code",
            "/fw/OVMF_CODE.fd",
            "--firmware-vars",
            "/fw/OVMF_VARS.fd",
            "--evolutiond",
            "/lab/bin/evolutiond",
            "--qemu",
            "/usr/bin/qemu-system-x86_64",
        ]
    )
    value = lab.initial_plan(
        FIXTURE_CANDIDATE, "/lab/run/candidate.img", "b" * 64, "gen-baseline", request
    )
    return json.dumps(value, sort_keys=True, indent=2) + "\n"


class Describe(unittest.TestCase):
    def describe(self, base, **build):
        out = make_build(base, **build)
        return out, lab.describe(out, COMMIT, MANIFEST)

    def test_binds_each_slot_to_its_probe_facts(self):
        with tempfile.TemporaryDirectory() as d:
            out, candidate = self.describe(d)
            written = json.loads((out / "candidate.json").read_text())
            self.assertEqual(written, candidate)
            self.assertEqual(candidate["schema"], lab.CANDIDATE_SCHEMA)
            self.assertEqual(
                (candidate["active"], candidate["staged"]), ("root-a", "root-b")
            )
            self.assertEqual(candidate["kernel_sha256"], sha(b"kernel"))
            self.assertEqual(candidate["initramfs_sha256"], sha(b"initramfs"))
            for slot, letter in lab.SLOTS.items():
                built = candidate["slots"][slot]
                raw = (out / f"identity-{letter}.json").read_bytes()
                self.assertEqual(built["identity_sha256"], sha(raw))
                self.assertEqual(built["partuuid"], UUID[slot])
                self.assertEqual(built["build_id"], f"lab-{letter}")
                self.assertEqual(built["kernel_release"], RELEASE)
                self.assertEqual(built["vmlinuz_sha256"], sha(b"kernel"))
                self.assertEqual(
                    built["rootfs_sha256"], sha(f"root volume {letter}".encode())
                )
                self.assertEqual(sorted(built["binaries"]), sorted(lab.BINARIES))
            # load_candidate reads back what describe wrote.
            self.assertEqual(lab.load_candidate(out), candidate)

    def test_refuses_an_inconsistent_build(self):
        bad_identities = {
            "wrong schema": {
                "root-b": identity("root-b", schema="flowerly-generation-root/v1")
            },
            "wrong slot": {"root-b": identity("root-a")},
            "other commit": {"root-a": identity("root-a", source_commit="e" * 40)},
            "other manifest": {
                "root-b": identity("root-b", input_manifest_sha256="e" * 64)
            },
            "uppercase partuuid": {
                "root-b": identity("root-b", root_partuuid=UUID["root-b"].upper())
            },
            "malformed build_id": {"root-b": identity("root-b", build_id="lab b")},
            "same partuuid": {
                "root-b": identity("root-b", root_partuuid=UUID["root-a"])
            },
            "not an object": {"root-a": "[]"},
        }
        for label, identities in bad_identities.items():
            # make_build bakes the facts from each document, so "same
            # partuuid" passes per slot and is refused only across slots.
            with self.subTest(label), tempfile.TemporaryDirectory() as d:
                with self.assertRaises(lab.Refused):
                    self.describe(d, identities=identities)
        bad_facts = {
            "baked another identity": {"identity_sha256": "0" * 64},
            "baked another build_id": {"build_id": "lab-x"},
            "no package hash": {"packages_sha256": None},
            "missing binary": {
                "binaries": {name: "4" * 64 for name in lab.BINARIES[1:]}
            },
            "extra binary": {
                "binaries": {
                    **{name: "4" * 64 for name in lab.BINARIES},
                    "sh": "4" * 64,
                }
            },
            "malformed binary hash": {
                "binaries": {name: "4" * 63 for name in lab.BINARIES}
            },
        }
        for label, facts in bad_facts.items():
            with self.subTest(label), tempfile.TemporaryDirectory() as d:
                with self.assertRaises(lab.Refused):
                    self.describe(d, facts={"root-b": facts})

    def test_refuses_two_slots_with_one_build_id(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaisesRegex(lab.Refused, "same build_id"):
                self.describe(
                    d,
                    identities={"root-b": identity("root-b", build_id="lab-a")},
                    facts={"root-b": {"build_id": "lab-a"}},
                )

    def test_refuses_missing_symlinked_or_malformed_files(self):
        def missing_vmlinuz(out):
            (out / "vmlinuz").unlink()

        def symlinked_volume(out):
            (out / "root-b.ext4").unlink()
            (out / "root-b.ext4").symlink_to(out / "root-a.ext4")

        def symlinked_facts(out):
            (out / "facts-a.json").rename(out / "elsewhere.json")
            (out / "facts-a.json").symlink_to(out / "elsewhere.json")

        def malformed_release(out):
            write(out / "kernel-release-b", "6.17 arch\n")

        def huge_facts(out):
            write(out / "facts-b.json", " " * (64 * 1024 + 1))

        def not_json(out):
            write(out / "facts-a.json", "{")

        for mutate in (
            missing_vmlinuz,
            symlinked_volume,
            symlinked_facts,
            malformed_release,
            huge_facts,
            not_json,
        ):
            with self.subTest(mutate.__name__), tempfile.TemporaryDirectory() as d:
                out = make_build(d)
                mutate(out)
                with self.assertRaises(lab.Refused):
                    lab.describe(out, COMMIT, MANIFEST)
                self.assertFalse((out / "candidate.json").exists())

    def test_refuses_malformed_arguments_and_an_existing_description(self):
        with tempfile.TemporaryDirectory() as d:
            out = make_build(d)
            for commit, manifest in (("c" * 39, MANIFEST), (COMMIT, "D" * 64)):
                with self.assertRaises(lab.Refused):
                    lab.describe(out, commit, manifest)
            lab.describe(out, COMMIT, MANIFEST)
            with self.assertRaises(FileExistsError):
                lab.describe(out, COMMIT, MANIFEST)

    def test_load_candidate_refuses_another_description(self):
        with tempfile.TemporaryDirectory() as d:
            out, candidate = self.describe(d)
            for change in (
                {"schema": "flowerly-lab-candidate/v0"},
                {"active": "root-b", "staged": "root-a"},
                {"slots": {"root-b": candidate["slots"]["root-b"]}},
            ):
                with self.subTest(change):
                    write(out / "candidate.json", json.dumps({**candidate, **change}))
                    with self.assertRaises(lab.Refused):
                        lab.load_candidate(out)
            write(out / "candidate.json", json.dumps(candidate))
            (out / "slot-tool").unlink()
            with self.assertRaisesRegex(lab.Refused, "slot-tool"):
                lab.load_candidate(out)


class Prepare(unittest.TestCase):
    def prepare(self, d, *extra, runner=None):
        make_build(d)
        lab.describe(Path(d) / "build", COMMIT, MANIFEST)
        runner = runner or Runner()
        request = options(d, *extra)
        path = lab.prepare(request, run=runner)
        return runner, path, json.loads(path.read_text())

    def test_good_arms_a_copy_and_plans_one_candidate_boot(self):
        with tempfile.TemporaryDirectory() as d:
            runner, path, plan = self.prepare(d)
            run = Path(d).resolve() / "run"
            self.assertEqual(path, run / "plan.json")
            self.assertEqual(os.stat(run).st_mode & 0o777, 0o700)
            disk = run / "candidate.img"
            # The plan hashes the copy as booted once and then armed; the
            # built disk is untouched.
            self.assertEqual(plan["candidate"]["disk"], str(disk))
            self.assertEqual(plan["candidate"]["disk_sha256"], sha(disk.read_bytes()))
            built = Path(d) / "build/ab.img"
            self.assertEqual(built.read_bytes(), b"\0" * 4096 + b"disk")
            self.assertEqual(disk.read_bytes(), built.read_bytes() + b"bootedarmed")
            self.assertEqual(
                runner.calls[0][:3], ["cp", "--sparse=always", "--reflink=auto"]
            )
            self.assertEqual(
                [call[1:3] for call in runner.calls[2:]],
                [["lab", "run"], ["slot", "arm"]],
            )
            arm = runner.arm()
            build = Path(d).resolve() / "build"
            self.assertEqual(
                arm,
                [
                    str(build / "slot-tool"),
                    "slot",
                    "arm",
                    "--disk",
                    str(disk),
                    "--manifest",
                    str(run / "candidate.img.manifest.json"),
                    "--active-partuuid",
                    UUID["root-a"],
                    "--slot",
                    "root-b",
                    "--kernel",
                    str(build / "vmlinuz"),
                    "--initramfs",
                    str(build / "initramfs.img"),
                    "--tries",
                    "2",
                ],
            )
            self.assertEqual(
                plan["boots"],
                [{"label": "candidate", "subject": "candidate", "slot": "root-b"}],
            )
            self.assertNotIn("hypothesis", plan)

    def test_the_copy_first_boots_as_slot_a_before_b_is_armed(self):
        # The guest's witness creates the persistent data marker only on the
        # first boot of root-a, as on an installed machine, and refuses a
        # first boot of root-b. Each run's copy boots once as A, unrecorded.
        with tempfile.TemporaryDirectory() as d:
            runner, _, plan = self.prepare(d)
            run = Path(d).resolve() / "run"
            initial = run / "initial"
            self.assertEqual(
                runner.boot(),
                [
                    str(Path(d) / "evolutiond"),
                    "lab",
                    "run",
                    "--plan",
                    str(initial / "plan.json"),
                    "--out",
                    str(initial / "result"),
                    "--qemu",
                    str(Path(d) / "qemu-system-x86_64"),
                ],
            )
            (written,) = runner.initial_plans
            self.assertEqual(json.loads((initial / "plan.json").read_text()), written)
            built = (Path(d) / "build/ab.img").read_bytes()
            self.assertEqual(
                written["candidate"],
                {
                    "generation": "gen-baseline",
                    "disk": str(run / "candidate.img"),
                    "disk_sha256": sha(built),
                },
            )
            self.assertIsNone(written["baseline"])
            self.assertEqual(written["expect"], {"root-a": plan["expect"]["root-a"]})
            self.assertEqual(
                written["boots"],
                [{"label": "initial", "subject": "candidate", "slot": "root-a"}],
            )
            self.assertEqual(written["hypothesis"], lab.INITIAL_HYPOTHESIS)
            for key in ("schema", "lane", "firmware", "machine", "limits", "checks"):
                self.assertEqual(written[key], plan[key], key)

    def test_a_failed_initial_boot_arms_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            runner = Runner(boot_status=3)
            with self.assertRaisesRegex(lab.Refused, "initial boot"):
                self.prepare(d, runner=runner)
            self.assertEqual([c for c in runner.calls if c[1:3] == ["slot", "arm"]], [])
            self.assertFalse((Path(d) / "run/plan.json").exists())

    def test_a_tcg_plan_lets_its_initial_boot_run_under_tcg(self):
        for accel, allowed in (("kvm", False), ("tcg", True)):
            with self.subTest(accel), tempfile.TemporaryDirectory() as d:
                runner, _, _ = self.prepare(d, "--accel", accel)
                self.assertEqual("--allow-tcg" in runner.boot(), allowed)

    def test_only_the_failing_scenarios_inject_the_health_failure(self):
        for scenario, injected in (("good", False), ("bad", True), ("rollback", True)):
            with self.subTest(scenario), tempfile.TemporaryDirectory() as d:
                runner, _, _ = self.prepare(d, "--scenario", scenario, "--tries", "3")
                arm = runner.arm()
                self.assertEqual(arm[arm.index("--tries") + 1], "3")
                self.assertEqual("--extra-option" in arm, injected)
                if injected:
                    self.assertEqual(arm[-2:], ["--extra-option", lab.FAIL_HEALTH])

    def test_the_plan_expects_what_each_slot_was_built_with(self):
        with tempfile.TemporaryDirectory() as d:
            _, _, plan = self.prepare(d, "--scenario", "rollback")
            candidate = json.loads((Path(d) / "build/candidate.json").read_text())
            self.assertEqual(plan["schema"], lab.PLAN_SCHEMA)
            self.assertEqual(plan["lane"], "reproducibility")
            self.assertEqual(plan["candidate"]["generation"], "gen-candidate")
            self.assertEqual(plan["baseline"], "gen-baseline")
            self.assertEqual(plan["expect"]["root-b"]["generation"], "gen-candidate")
            self.assertEqual(plan["expect"]["root-a"]["generation"], "gen-baseline")
            for slot, expected in plan["expect"].items():
                built = candidate["slots"][slot]
                for key in (
                    "partuuid",
                    "identity_sha256",
                    "build_id",
                    "kernel_release",
                    "vmlinuz_sha256",
                    "packages_sha256",
                    "binaries",
                ):
                    self.assertEqual(expected[key], built[key], (slot, key))
                # The root volume hash is the Generation's identity, not a check.
                self.assertNotIn("rootfs_sha256", expected)
            self.assertEqual(plan["checks"], list(lab.CHECKS))
            self.assertEqual(plan["hypothesis"], lab.ROLLBACK_HYPOTHESIS)
            self.assertEqual(
                plan["machine"], {"accel": "kvm", "smp": 2, "memory_mib": 2048}
            )
            self.assertEqual(
                plan["limits"], {"boot_seconds": 600, "shutdown_seconds": 120}
            )

    def test_refuses_to_reuse_a_run_directory(self):
        for kind in ("directory", "symlink"):
            with self.subTest(kind), tempfile.TemporaryDirectory() as d:
                make_build(d)
                lab.describe(Path(d) / "build", COMMIT, MANIFEST)
                run = Path(d) / "run"
                if kind == "directory":
                    run.mkdir()
                else:
                    run.symlink_to(Path(d) / "absent")
                runner = Runner()
                with self.assertRaisesRegex(lab.Refused, "reuse"):
                    lab.prepare(options(d), run=runner)
                self.assertEqual(runner.calls, [])

    def test_a_failed_arm_writes_no_plan(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaisesRegex(lab.Refused, "slot arm"):
                self.prepare(d, runner=Runner(arm_status=1))
            self.assertFalse((Path(d) / "run/plan.json").exists())

    def test_refuses_a_malformed_request(self):
        cases = {
            "rollback with no try": ("--scenario", "rollback", "--tries", "0"),
            "rollback beyond the boot limit": (
                "--scenario",
                "rollback",
                "--tries",
                "8",
            ),
            "good beyond the loader's tries": ("--tries", "10"),
            "one Generation for both": ("--baseline-generation", "gen-candidate"),
            "malformed Generation": ("--candidate-generation", "gen candidate"),
            "relative firmware": ("--firmware-code", "OVMF_CODE.fd"),
            "missing firmware": ("--firmware-vars", "/nonexistent/OVMF_VARS.fd"),
            "missing evolutiond": ("--evolutiond", "/nonexistent/evolutiond"),
            "relative QEMU": ("--qemu", "qemu-system-x86_64"),
        }
        for label, extra in cases.items():
            with self.subTest(label), tempfile.TemporaryDirectory() as d:
                make_build(d)
                lab.describe(Path(d) / "build", COMMIT, MANIFEST)
                runner = Runner()
                with self.assertRaises(lab.Refused):
                    lab.prepare(options(d, *extra), run=runner)
                self.assertEqual(runner.calls, [])
                self.assertFalse((Path(d) / "run").exists())
        with tempfile.TemporaryDirectory() as d:
            comma = Path(d) / "a,b"
            comma.mkdir()
            make_build(comma)
            lab.describe(comma / "build", COMMIT, MANIFEST)
            code, _ = firmware(comma)
            with self.assertRaisesRegex(lab.Refused, "firmware_code"):
                lab.prepare(options(comma, "--firmware-code", str(code)), run=Runner())

    def test_the_tries_bounds_fit_the_loader_and_the_boot_limit(self):
        self.assertEqual(len(lab.boots("rollback", lab.MAX_BOOTS - 1)), lab.MAX_BOOTS)
        for tries in (1, 2, 7):
            boots = lab.boots("rollback", tries)
            self.assertEqual(
                [b["label"] for b in boots],
                [f"candidate-{n}" for n in range(1, tries + 1)] + ["fallback"],
            )
            self.assertEqual({b["slot"] for b in boots[:-1]}, {"root-b"})
            self.assertEqual({b["subject"] for b in boots[:-1]}, {"candidate"})
            self.assertEqual(
                boots[-1],
                {"label": "fallback", "subject": "baseline", "slot": "root-a"},
            )
        for tries in (1, 9):
            self.assertEqual(len(lab.boots("bad", tries)), 1)


class HostContract(unittest.TestCase):
    def test_the_hosts_fixture_is_what_prepare_writes(self):
        text = fixture_plan()
        if os.environ.get("FLOWERLY_REGENERATE_LAB_FIXTURE") == "1":
            PLAN_FIXTURE.write_text(text)
        self.assertEqual(PLAN_FIXTURE.read_text(), text)
        self.assertIn("lab-plan-rollback.json", HOST_PLAN.read_text())

    def test_the_hosts_initial_fixture_is_what_prepare_boots_first(self):
        text = fixture_initial_plan()
        if os.environ.get("FLOWERLY_REGENERATE_LAB_FIXTURE") == "1":
            INITIAL_FIXTURE.write_text(text)
        self.assertEqual(INITIAL_FIXTURE.read_text(), text)
        self.assertIn("lab-plan-initial.json", HOST_PLAN.read_text())

    def test_the_plan_follows_the_hosts_rules(self):
        source = HOST_PLAN.read_text()
        self.assertIn(f"pub const MAX_BOOTS: usize = {lab.MAX_BOOTS};", source)
        self.assertIn(f'pub const PLAN_SCHEMA: &str = "{lab.PLAN_SCHEMA}";', source)
        names = re.findall(r'Self::\w+ => "(\w+)",', source)
        self.assertEqual(tuple(names), lab.CHECKS)
        label = re.compile(r"[a-z][a-z0-9-]{0,31}")
        for scenario in lab.SCENARIOS:
            for boot in lab.boots(scenario, lab.MAX_BOOTS - 1):
                self.assertRegex(boot["label"], label)
        for name in lab.BINARIES:
            self.assertRegex(name, label)

    def test_the_probe_and_the_gate_agree(self):
        probe = load("lab_probe", PROBE)
        self.assertEqual(sorted(probe.BINARIES), sorted(lab.BINARIES))
        self.assertEqual(probe.SLOTS, lab.SLOTS)
        self.assertIn(f" {lab.FAIL_HEALTH} ", HEALTH_GATE.read_text())

    def test_the_build_writes_the_identity_it_bakes(self):
        # describe compares the probe's hash of the baked generation-root.json
        # with identity-<letter>.json byte for byte. The rootfs script bakes it
        # with a heredoc, so the build's copy must end in the same newline.
        baked = re.search(
            r'generation-root\.json" <<EOF\n(.*)\nEOF\n', BAKER.read_text()
        ).group(1)
        written = re.search(
            r"identity\(\) \{[^\n]*\n\s*printf '([^']*)'", BUILD.read_text()
        ).group(1)
        self.assertEqual(re.sub(r"\$[A-Z_]+", "%s", baked) + r"\n", written)


class Command(unittest.TestCase):
    def run_main(self, argv):
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try:
                status = lab.main(argv)
            except SystemExit as exit_:
                status = exit_.code
        return status, stdout.getvalue(), stderr.getvalue()

    def test_describe_and_refusals_exit_as_documented(self):
        with tempfile.TemporaryDirectory() as d:
            out = make_build(d)
            args = ["describe", str(out), "--source-commit", COMMIT]
            status, stdout, _ = self.run_main(
                args + ["--input-manifest-sha256", MANIFEST]
            )
            self.assertEqual(status, 0)
            self.assertIn("candidate.json", stdout)
            status, _, stderr = self.run_main(
                args + ["--input-manifest-sha256", MANIFEST]
            )
            self.assertEqual(status, 1)
            self.assertIn("lab candidate:", stderr)
            status, _, _ = self.run_main(["describe", str(out)])
            self.assertEqual(status, 2)
            status, _, _ = self.run_main(
                ["prepare", str(out), str(Path(d) / "run"), "--scenario", "worse"]
            )
            self.assertEqual(status, 2)

    def test_the_scripts_parse_and_check_their_usage_first(self):
        for script in (BUILD, VM_TEST):
            subprocess.run(["bash", "-n", str(script)], check=True)
        # Usage is checked before /dev/kvm, the build or anything else.
        for argv in (
            ["--scenario", "worse"],
            ["--tries", "0"],
            ["--tries", "10"],
            ["--tries"],
            ["extra"],
        ):
            with self.subTest(argv):
                done = subprocess.run(
                    ["bash", str(VM_TEST), *argv],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(done.returncode, 2, done.stderr)
                self.assertIn("usage:", done.stderr)
        done = subprocess.run(
            ["bash", str(BUILD), "only-one"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(done.returncode, 1)
        self.assertIn("usage:", done.stderr)

    def test_vm_test_hands_prepare_the_tools_of_its_own_run(self):
        self.assertIn(
            '--evolutiond "$BIN/evolutiond" --qemu "$QEMU"', VM_TEST.read_text()
        )

    def test_the_runners_call_the_one_script(self):
        for runner in ("justfile", "Makefile"):
            text = (ROOT / runner).read_text()
            self.assertRegex(text, r"(?m)^vm-test:\n\s+bash \./scripts/vm-test\.sh$")

    def test_the_tool_uses_only_the_standard_library(self):
        imports = set(re.findall(r"(?m)^(?:import|from) (\w+)", TOOL.read_text()))
        self.assertLessEqual(
            imports,
            {"argparse", "hashlib", "json", "os", "pathlib", "re", "subprocess", "sys"},
        )


if __name__ == "__main__":
    unittest.main()
