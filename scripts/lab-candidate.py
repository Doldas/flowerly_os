#!/usr/bin/env python3
"""Evolution Lab candidate (ADR-0024, #412): describe a build, prepare a run.

``describe OUT --source-commit SHA40 --input-manifest-sha256 SHA64 [--lane
reproducibility|upstream]`` runs at the end of build-lab-candidate.sh. It
binds what was built (each slot's identity document, root volume, the kernel
and initramfs on the ESP) to what that slot's probe will report:
``facts-<a|b>.json`` from ``lab-probe.py --offline`` and the kernel release
under ``usr/lib/modules``. It writes ``candidate.json`` and refuses anything
missing or inconsistent, so a plan can never expect facts the build did not
produce.

The lane (ADR-0024 D4) says what the package set is. In the reproducibility
lane it is the committed manifest's frozen set, and the plan claims it. In
the upstream lane it is whatever the live mirror resolved when the inputs
were built: ``resolved-packages.txt`` (``lab-probe.py --package-list``) is
that list, its SHA-256 is the identity's input manifest hash and every
slot's package hash, and the plan records the set without claiming it.

``prepare CANDIDATE RUN --scenario good|bad|rollback ...`` copies the built
disk into a new run directory and boots the copy once as slot A with
``evolutiond lab run``, unrecorded: the initial boot an installed machine has,
whose witness creates the persistent data marker and which the guest requires
before any boot of slot B. Only once that boot passes does it arm slot B with
boot counting (with the injected health failure for ``bad`` and
``rollback``), hash the armed disk and write the ``flowerly-lab-plan/v1``
plan for the run. Each run consumes its own copy; the built disk is never
booted.

Standard library only.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

CANDIDATE_SCHEMA = "flowerly-lab-candidate/v1"
PLAN_SCHEMA = "flowerly-lab-plan/v1"
IDENTITY_SCHEMA = "flowerly-generation-root/v2"
SLOTS = {"root-a": "a", "root-b": "b"}
ACTIVE, TARGET = "root-a", "root-b"
BINARIES = ("agenticctl", "evolutiond", "flowerlyd", "flowerly-slot-broker")
CHECKS = (
    "identity",
    "root",
    "kernel",
    "packages",
    "binaries",
    "health",
    "integration",
    "shutdown",
)
FAIL_HEALTH = "flowerly.test.fail_health=1"
SCENARIOS = ("good", "bad", "rollback")
LANES = ("reproducibility", "upstream")
RESOLVED = "resolved-packages.txt"
MAX_RESOLVED = 8 * 1024 * 1024
# evolutiond's MAX_BOOTS; a rollback run boots B once per try, then A.
MAX_BOOTS = 8
MAX_TRIES = 9
INITIAL_BOOT = {"label": "initial", "subject": "candidate", "slot": ACTIVE}
INITIAL_HYPOTHESIS = (
    "the baseline in slot A boots as the machine's first boot, reports what "
    "was built and passes its checks, before the candidate is armed"
)
ROLLBACK_HYPOTHESIS = (
    "a candidate that fails its health gate is left unblessed and, once its "
    "boot-count tries are spent, the loader falls back to the baseline, "
    "which boots healthy"
)
SHA256 = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
PARTUUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
BUILD_ID = re.compile(r"[A-Za-z0-9_.-]{1,64}")
RELEASE = re.compile(r"[A-Za-z0-9._+-]{1,64}")
GENERATION = re.compile(r"[A-Za-z0-9_-]{1,128}")


class Refused(Exception):
    """The build or the request is not what a plan may be written from."""


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def regular(path, what):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise Refused(f"{what} {path} is missing or not a regular file")
    return path


def load_json(path, what, limit=64 * 1024):
    path = regular(path, what)
    raw = path.read_bytes()
    if len(raw) > limit:
        raise Refused(f"{what} {path} is larger than {limit} bytes")
    try:
        return raw, json.loads(raw)
    except ValueError as error:
        raise Refused(f"{what} {path} is not JSON: {error}") from None


def describe_slot(out, slot, source_commit, manifest_sha256, vmlinuz_sha256):
    letter = SLOTS[slot]
    raw, identity = load_json(out / f"identity-{letter}.json", f"{slot} identity")
    _, facts = load_json(out / f"facts-{letter}.json", f"{slot} probe facts")
    if not isinstance(identity, dict) or not isinstance(facts, dict):
        raise Refused(f"{slot}: identity and facts must be JSON objects")
    expected = {
        "schema": IDENTITY_SCHEMA,
        "slot": slot,
        "source_commit": source_commit,
        "input_manifest_sha256": manifest_sha256,
    }
    for key, value in expected.items():
        if identity.get(key) != value:
            raise Refused(
                f"{slot} identity {key} is {identity.get(key)!r}, not {value!r}"
            )
    partuuid, build_id = identity.get("root_partuuid"), identity.get("build_id")
    if not isinstance(partuuid, str) or not PARTUUID.fullmatch(partuuid):
        raise Refused(f"{slot} identity root_partuuid is not a lowercase UUID")
    if not isinstance(build_id, str) or not BUILD_ID.fullmatch(build_id):
        raise Refused(f"{slot} identity build_id is malformed")
    # The probe hashes the document baked into the slot; it must be this one.
    if facts.get("identity_sha256") != hashlib.sha256(raw).hexdigest():
        raise Refused(f"{slot}: the baked identity is not identity-{letter}.json")
    if facts.get("build_id") != build_id:
        raise Refused(f"{slot}: the baked build_id is not {build_id}")
    packages = facts.get("packages_sha256")
    if not isinstance(packages, str) or not SHA256.fullmatch(packages):
        raise Refused(f"{slot}: the probe could not hash the package set")
    binaries = facts.get("binaries")
    if not isinstance(binaries, dict) or sorted(binaries) != sorted(BINARIES):
        raise Refused(f"{slot}: the probe did not find exactly {', '.join(BINARIES)}")
    if not all(isinstance(v, str) and SHA256.fullmatch(v) for v in binaries.values()):
        raise Refused(f"{slot}: a binary hash is malformed")
    release_file = regular(out / f"kernel-release-{letter}", f"{slot} kernel release")
    release = release_file.read_text().strip()
    if not RELEASE.fullmatch(release):
        raise Refused(f"{slot}: kernel release {release!r} is malformed")
    volume = regular(out / f"root-{letter}.ext4", f"{slot} root volume")
    return {
        "partuuid": partuuid,
        "build_id": build_id,
        "identity_sha256": facts["identity_sha256"],
        "rootfs_sha256": sha256_file(volume),
        "kernel_release": release,
        "vmlinuz_sha256": vmlinuz_sha256,
        "packages_sha256": packages,
        "binaries": dict(sorted(binaries.items())),
    }


def resolved_sha256(path):
    """SHA-256 of an upstream lane's resolved package list."""
    path = regular(path, "resolved package list")
    if path.stat().st_size > MAX_RESOLVED:
        raise Refused(f"{path} is larger than {MAX_RESOLVED} bytes")
    return sha256_file(path)


def describe(out, source_commit, manifest_sha256, lane="reproducibility"):
    out = Path(out)
    if not COMMIT.fullmatch(source_commit) or not SHA256.fullmatch(manifest_sha256):
        raise Refused("source commit or input manifest hash is malformed")
    if lane not in LANES:
        raise Refused(f"lane must be one of {', '.join(LANES)}")
    files = {
        "disk": "ab.img",
        "manifest": "ab.img.manifest.json",
        "kernel": "vmlinuz",
        "initramfs": "initramfs.img",
        "slot_tool": "slot-tool",
    }
    if lane == "upstream":
        files["resolved_packages"] = RESOLVED
    for key, name in files.items():
        regular(out / name, key)
    if lane == "upstream" and resolved_sha256(out / RESOLVED) != manifest_sha256:
        raise Refused(f"the input manifest hash is not the hash of {RESOLVED}")
    kernel_sha256 = sha256_file(out / files["kernel"])
    slots = {
        slot: describe_slot(out, slot, source_commit, manifest_sha256, kernel_sha256)
        for slot in SLOTS
    }
    if slots[ACTIVE]["partuuid"] == slots[TARGET]["partuuid"]:
        raise Refused("both slots claim the same PARTUUID")
    if slots[ACTIVE]["build_id"] == slots[TARGET]["build_id"]:
        raise Refused("both slots carry the same build_id")
    if lane == "upstream":
        # What each slot's probe will report is the list the build resolved.
        for slot, built in slots.items():
            if built["packages_sha256"] != manifest_sha256:
                raise Refused(f"{slot}: its package set is not {RESOLVED}")
    candidate = {
        "schema": CANDIDATE_SCHEMA,
        "lane": lane,
        "source_commit": source_commit,
        "input_manifest_sha256": manifest_sha256,
        "active": ACTIVE,
        "staged": TARGET,
        "files": files,
        "kernel_sha256": kernel_sha256,
        "initramfs_sha256": sha256_file(out / files["initramfs"]),
        "slots": slots,
    }
    write_json(out / "candidate.json", candidate)
    return candidate


def write_json(path, value):
    with open(path, "x") as handle:
        json.dump(value, handle, sort_keys=True, indent=2)
        handle.write("\n")


def load_candidate(directory):
    directory = Path(directory)
    _, candidate = load_json(directory / "candidate.json", "candidate description")
    if not isinstance(candidate, dict) or candidate.get("schema") != CANDIDATE_SCHEMA:
        raise Refused(
            f"{directory}/candidate.json is not a {CANDIDATE_SCHEMA} description"
        )
    if candidate.get("active") != ACTIVE or candidate.get("staged") != TARGET:
        raise Refused("the candidate must have root-a active and root-b staged")
    slots = candidate.get("slots")
    if not isinstance(slots, dict) or sorted(slots) != sorted(SLOTS):
        raise Refused("the candidate must describe root-a and root-b")
    lane, files = candidate.get("lane"), candidate.get("files")
    if lane not in LANES:
        raise Refused(f"the candidate's lane must be one of {', '.join(LANES)}")
    if not isinstance(files, dict):
        raise Refused("the candidate names no files")
    for name in files.values():
        regular(directory / name, name)
    if (files.get("resolved_packages") == RESOLVED) != (lane == "upstream"):
        raise Refused(f"only an upstream candidate carries {RESOLVED}")
    if lane == "upstream" and (
        resolved_sha256(directory / RESOLVED) != candidate.get("input_manifest_sha256")
    ):
        raise Refused(f"{RESOLVED} is not the list the candidate was built from")
    return candidate


def boots(scenario, tries):
    if scenario in ("good", "bad"):
        return [{"label": "candidate", "subject": "candidate", "slot": TARGET}]
    candidate = [
        {"label": f"candidate-{n}", "subject": "candidate", "slot": TARGET}
        for n in range(1, tries + 1)
    ]
    return candidate + [{"label": "fallback", "subject": "baseline", "slot": ACTIVE}]


def expectation(built, generation, lane):
    """What a slot's probe must report, as built, for its Generation. The
    upstream lane records the package set it reports instead of claiming it."""
    expected = {
        "generation": generation,
        "partuuid": built["partuuid"],
        "identity_sha256": built["identity_sha256"],
        "build_id": built["build_id"],
        "kernel_release": built["kernel_release"],
        "vmlinuz_sha256": built["vmlinuz_sha256"],
        "packages_sha256": built["packages_sha256"],
        "binaries": built["binaries"],
    }
    if lane == "upstream":
        del expected["packages_sha256"]
    return expected


def lab_plan(
    lane, subject, baseline, expect, planned_boots, disk, disk_sha256, options
):
    return {
        "schema": PLAN_SCHEMA,
        "lane": lane,
        "candidate": {
            "generation": subject,
            "disk": str(disk),
            "disk_sha256": disk_sha256,
        },
        "baseline": baseline,
        "firmware": {
            "code": str(options.firmware_code),
            "vars": str(options.firmware_vars),
        },
        "machine": {
            "accel": options.accel,
            "smp": options.smp,
            "memory_mib": options.memory_mib,
        },
        "limits": {
            "boot_seconds": options.boot_seconds,
            "shutdown_seconds": options.shutdown_seconds,
        },
        "expect": expect,
        "checks": list(CHECKS),
        "boots": planned_boots,
    }


def plan(candidate, scenario, tries, disk, disk_sha256, generations, options):
    """The plan for one run; ``generations`` maps slot -> Generation id."""
    lane = candidate["lane"]
    expect = {
        slot: expectation(built, generations[slot], lane)
        for slot, built in candidate["slots"].items()
    }
    value = lab_plan(
        lane,
        generations[TARGET],
        generations[ACTIVE],
        expect,
        boots(scenario, tries),
        disk,
        disk_sha256,
        options,
    )
    if scenario == "rollback":
        value["hypothesis"] = ROLLBACK_HYPOTHESIS
    return value


def initial_plan(candidate, disk, disk_sha256, generation, options):
    """The unrecorded first boot of the copy, with slot A's Generation as its
    subject: the host's plan rules have every plan boot its subject."""
    lane = candidate["lane"]
    expect = {ACTIVE: expectation(candidate["slots"][ACTIVE], generation, lane)}
    value = lab_plan(
        lane,
        generation,
        None,
        expect,
        [dict(INITIAL_BOOT)],
        disk,
        disk_sha256,
        options,
    )
    value["hypothesis"] = INITIAL_HYPOTHESIS
    return value


def boot_argv(options, plan_path, out):
    argv = [
        str(options.evolutiond),
        "lab",
        "run",
        "--plan",
        str(plan_path),
        "--out",
        str(out),
        "--qemu",
        str(options.qemu),
    ]
    if options.accel == "tcg":
        argv.append("--allow-tcg")
    return argv


def arm_argv(candidate_dir, candidate, disk, manifest, scenario, tries):
    files = candidate["files"]
    argv = [
        str(candidate_dir / files["slot_tool"]),
        "slot",
        "arm",
        "--disk",
        str(disk),
        "--manifest",
        str(manifest),
        "--active-partuuid",
        candidate["slots"][ACTIVE]["partuuid"],
        "--slot",
        TARGET,
        "--kernel",
        str(candidate_dir / files["kernel"]),
        "--initramfs",
        str(candidate_dir / files["initramfs"]),
        "--tries",
        str(tries),
    ]
    if scenario in ("bad", "rollback"):
        argv += ["--extra-option", FAIL_HEALTH]
    return argv


def check_request(options):
    if options.scenario not in SCENARIOS:
        raise Refused(f"scenario must be one of {', '.join(SCENARIOS)}")
    limit = MAX_BOOTS - 1 if options.scenario == "rollback" else MAX_TRIES
    if not 1 <= options.tries <= limit:
        raise Refused(f"{options.scenario}: tries must be 1..={limit}")
    for slot, generation in (
        (TARGET, options.candidate_generation),
        (ACTIVE, options.baseline_generation),
    ):
        if not GENERATION.fullmatch(generation):
            raise Refused(f"{slot} Generation id {generation!r} is malformed")
    if options.candidate_generation == options.baseline_generation:
        raise Refused("the candidate and baseline Generations must differ")
    for what in ("firmware_code", "firmware_vars"):
        path = Path(getattr(options, what))
        if not path.is_absolute() or "," in str(path) or not path.is_file():
            raise Refused(f"{what} must be an existing absolute path without commas")
    for what in ("evolutiond", "qemu"):
        path = Path(getattr(options, what))
        if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
            raise Refused(f"{what} must be an absolute path to an executable")


def prepare(options, run=subprocess.run):
    check_request(options)
    candidate_dir = Path(options.candidate).resolve()
    candidate = load_candidate(candidate_dir)
    run_dir = Path(options.run)
    if run_dir.exists() or run_dir.is_symlink():
        raise Refused(f"refusing to reuse {run_dir}")
    run_dir.mkdir(mode=0o700)
    run_dir = run_dir.resolve()
    if "," in str(run_dir):
        raise Refused("the run directory path must not contain a comma")
    disk = run_dir / "candidate.img"
    manifest = run_dir / "candidate.img.manifest.json"
    files = candidate["files"]
    copy = [
        "cp",
        "--sparse=always",
        "--reflink=auto",
        str(candidate_dir / files["disk"]),
        str(disk),
    ]
    copies = [copy, ["cp", str(candidate_dir / files["manifest"]), str(manifest)]]
    if candidate["lane"] == "upstream":
        # The run keeps the list its unclaimed package set was resolved as.
        copies.append(
            ["cp", str(candidate_dir / files["resolved_packages"]), str(run_dir)]
        )
    for argv in copies:
        if run(argv, check=False).returncode != 0:
            raise Refused(f"`{' '.join(argv[:3])} ...` failed")
    initial = run_dir / "initial"
    initial.mkdir(mode=0o700)
    write_json(
        initial / "plan.json",
        initial_plan(
            candidate, disk, sha256_file(disk), options.baseline_generation, options
        ),
    )
    # Arming B on a copy whose first boot did not pass would put the guest's
    # refusal in the candidate's record.
    if (
        run(
            boot_argv(options, initial / "plan.json", initial / "result"), check=False
        ).returncode
        != 0
    ):
        raise Refused(
            f"the initial boot of {ACTIVE} did not pass; see {initial}/result"
        )
    arm = arm_argv(
        candidate_dir, candidate, disk, manifest, options.scenario, options.tries
    )
    if run(arm, check=False).returncode != 0:
        raise Refused(f"`{' '.join(arm[:3])} ...` failed")
    generations = {
        TARGET: options.candidate_generation,
        ACTIVE: options.baseline_generation,
    }
    value = plan(
        candidate,
        options.scenario,
        options.tries,
        disk,
        sha256_file(disk),
        generations,
        options,
    )
    write_json(run_dir / "plan.json", value)
    return run_dir / "plan.json"


def parser():
    top = argparse.ArgumentParser(
        prog="lab-candidate.py", description=__doc__.split("\n")[0]
    )
    sub = top.add_subparsers(dest="command", required=True)
    d = sub.add_parser("describe", help="bind a finished build to its probe facts")
    d.add_argument("out")
    d.add_argument("--source-commit", required=True)
    d.add_argument("--input-manifest-sha256", required=True)
    d.add_argument("--lane", choices=LANES, default="reproducibility")
    p = sub.add_parser("prepare", help="arm a copy of the candidate and write its plan")
    p.add_argument("candidate")
    p.add_argument("run")
    p.add_argument("--scenario", choices=SCENARIOS, default="good")
    p.add_argument("--tries", type=int, default=2)
    p.add_argument("--candidate-generation", required=True)
    p.add_argument("--baseline-generation", required=True)
    p.add_argument("--firmware-code", required=True)
    p.add_argument("--firmware-vars", required=True)
    p.add_argument("--evolutiond", required=True, help="boots the initial A boot")
    p.add_argument("--qemu", required=True)
    p.add_argument("--accel", choices=("kvm", "tcg"), default="kvm")
    p.add_argument("--smp", type=int, default=2)
    p.add_argument("--memory-mib", type=int, default=2048)
    p.add_argument("--boot-seconds", type=int, default=600)
    p.add_argument("--shutdown-seconds", type=int, default=120)
    return top


def main(argv):
    options = parser().parse_args(argv)
    try:
        if options.command == "describe":
            describe(
                options.out,
                options.source_commit,
                options.input_manifest_sha256,
                options.lane,
            )
            print(f"described {os.path.join(options.out, 'candidate.json')}")
        else:
            print(prepare(options))
    except (Refused, OSError) as error:
        print(f"lab candidate: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
