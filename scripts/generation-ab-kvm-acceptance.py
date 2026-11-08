#!/usr/bin/env python3
"""Opt-in REAL OVMF/KVM A/B acceptance on Flowerly-built raw images.

Requires both roots and the data partition in a disposable GPT disk. The
guest's root identity, Flowerly IPC health, post-bless and persistent marker
are observed from each serial log. A guest is SIGKILLed to simulate power
loss. This is NOT a production updater or complete 0.0.5 acceptance.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
SCENARIOS = (
    "stage-complete",
    "stage-interrupted",
    "arm-before-rename",
    "arm-after-rename",
    "boot-before-bless",
    "unhealthy-b-fallback",
    "data-survives",
    "tampered-b-refused",
    "healthy-b-activation",
    "rollback-to-a",
    "damaged-entry-fallback",
    "measure-stage-and-boot",
)
# Seconds a damaged slot B entry is watched; the guest never reaches the
# Flowerly witness, the loader just has to burn one boot-count try per boot.
QUIET_SECONDS = 45
BOOT_SHA = re.compile(r"FLOWERLY_GENERATION_USER_MARKER_SHA256=([0-9a-f]{64})")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
TOKEN = "I_APPROVE_GENERATION_AB_KVM_EXECUTION"


def sha_file(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as source:
        while part := source.read(1048576):
            value.update(part)
    return value.hexdigest()


def module(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / file)
    if spec is None or spec.loader is None:
        raise ValueError("missing integration helper: " + file)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


def validate_source(args, *, actual_execution=False):
    if not COMMIT.fullmatch(args.approved_sha):
        raise ValueError("exact lowercase commit SHA required")
    sha = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
    if sha != args.approved_sha:
        raise ValueError("approved SHA does not match checked-out source")
    if actual_execution:
        clean = subprocess.run(
            ["git", "-C", str(ROOT), "status", "--porcelain"],
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        if clean:
            raise ValueError("dirty git checkout must not execute KVM")
        if os.geteuid() == 0:
            raise ValueError("do not run the KVM acceptance as root")
        if os.environ.get("FLOWERLY_GENERATION_AB_KVM") != TOKEN:
            raise ValueError("explicit Generation KVM environment approval missing")
        if not os.access("/dev/kvm", os.R_OK | os.W_OK):
            raise ValueError("usable real /dev/kvm required; no TCG fallback")
    for name in (
        "disk",
        "manifest",
        "kernel_b",
        "initramfs_b",
        "ovmf_code",
        "ovmf_vars",
    ):
        path = getattr(args, name)
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError("missing or unsafe input: " + name)
    if args.output.exists() or args.output.is_symlink():
        raise ValueError("refusing to overwrite existing evidence directory")
    if args.output.parent.is_symlink():
        raise ValueError("unsafe evidence parent")
    if not 30 <= args.timeout <= 600 or not 1 <= args.tries <= 9:
        raise ValueError("unsafe timeout / tries configuration")
    if (
        not UUID.fullmatch(args.root_a)
        or not UUID.fullmatch(args.root_b)
        or args.root_a == args.root_b
    ):
        raise ValueError("invalid or duplicate root UUIDs")
    manifest = json.loads(args.manifest.read_text())
    verifier = module("generation_ab_verify", "verify-ab-slot-layout.py")
    verifier.verify(args.disk, manifest)
    identities = {p["name"]: p["partuuid"] for p in manifest["partitions"]}
    if (
        identities.get("root-a") != args.root_a
        or identities.get("root-b") != args.root_b
    ):
        raise ValueError("approved booted root UUIDs do not match the disk")
    host = (args.unstaged_disk, args.unstaged_manifest, args.candidate_b)
    if any(host) and not all(host):
        raise ValueError(
            "--unstaged-disk, --unstaged-manifest and --candidate-b go together"
        )
    for path in filter(None, host):
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError("missing or unsafe host-scenario input")
    if actual_execution:
        for program in (
            "qemu-system-x86_64",
            "sfdisk",
            "mcopy",
            "mdir",
            "mtype",
            "mdel",
        ):
            if not shutil.which(program, path=os.environ["PATH"] + ":/usr/sbin:/sbin"):
                raise ValueError("missing real host tool: " + program)
    return manifest


def observe_serial(log, slot, *, bless, timeout):
    """Observe a real guest, not a mocked count or host-side assertion."""
    text = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
    if (
        "Kernel panic" in text
        or "Failed to start Flowerly OS Unified Supervisor" in text
    ):
        raise ValueError("guest kernel panic or Flowerly supervisor failure")
    seen = all(
        phrase in text
        for phrase in ("FLOWERLY_GENERATION_ROOT_VERIFIED", '"slot": "' + slot + '"')
    )
    marker = BOOT_SHA.findall(text)
    if not seen or len(set(marker)) != 1:
        return None
    blessed = "FLOWERLY_GENERATION_BOOT_BLESSED" in text
    ping = "FLOWERLY_GENERATION_IPC_VERIFIED" in text
    unhealthy = "injected unhealthy Generation boot" in text
    if bless and blessed and ping:
        return marker[0]
    if not bless and unhealthy and not blessed and not ping:
        return marker[0]
    return None


def verify_running_qemu(pid, command):
    """Read *running host process* evidence, not QEMU's requested config."""
    if type(pid) is not int or pid <= 0 or not isinstance(command, list):
        raise ValueError("invalid QEMU process identity")
    proc = Path("/proc") / str(pid)
    actual_raw = (proc / "cmdline").read_bytes()
    actual_argv = [
        arg.decode("utf-8", errors="strict")
        for arg in actual_raw.split(bytes([0]))
        if arg
    ]
    if actual_argv != command or not all(
        arg in actual_argv
        for arg in ("-net", "none", "-no-reboot", "-nodefaults", "q35,accel=kvm")
    ):
        raise ValueError("running QEMU command differs from approved KVM firmware boot")
    if any(value in actual_argv for value in ("-kernel", "-initrd", "-nic", "-netdev")):
        raise ValueError(
            "direct-kernel or networked QEMU is not valid firmware evidence"
        )
    fd_directory = proc / "fd"
    devices = []
    for fd in fd_directory.iterdir():
        try:
            devices.append(os.readlink(fd))
        except FileNotFoundError:
            continue
    if "/dev/kvm" not in devices:
        raise ValueError("no open /dev/kvm descriptor in actual QEMU process")
    return {
        "kvm_fd_verified": True,
        "qemu_cmdline_sha256": hashlib.sha256(actual_raw).hexdigest(),
    }


def qemu_boot(
    args, disk, vars_file, report, slot, *, bless, label, lineage="main", quiet=False
):
    """Start one real firmware boot; kill guest after the observed milestone.

    With quiet=True the boot is expected NOT to reach the Flowerly witness
    (damaged loader entry): the guest is watched for QUIET_SECONDS and must
    have started the requested slot's kernel without any Flowerly marker.
    """
    log = report / (label + ".serial.log")
    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        raise ValueError("qemu-system-x86_64 not installed")
    command = [
        qemu,
        "-machine",
        "q35,accel=kvm",
        "-cpu",
        "host",
        "-smp",
        "2",
        "-m",
        "2048",
        "-nodefaults",
        "-display",
        "none",
        "-serial",
        "stdio",
        "-monitor",
        "none",
        "-net",
        "none",
        "-no-reboot",
        "-drive",
        "if=pflash,format=raw,readonly=on,file=" + str(args.ovmf_code),
        "-drive",
        "if=pflash,format=raw,file=" + str(vars_file),
        "-drive",
        "if=virtio,format=raw,cache=none,file=" + str(disk),
    ]
    started = time.monotonic()
    expect = "NO_WITNESS" if quiet else "BLESSED" if bless else "REJECTED"
    with log.open("xb") as output:
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            limit = QUIET_SECONDS if quiet else args.timeout
            while time.monotonic() - started <= limit:
                if quiet:
                    text = log.read_text(encoding="utf-8", errors="replace")
                    if "FLOWERLY_GENERATION_ROOT_VERIFIED" in text:
                        raise ValueError(
                            f"{label}: damaged entry unexpectedly reached the guest"
                        )
                else:
                    marker = observe_serial(
                        log, slot, bless=bless, timeout=args.timeout
                    )
                    if marker is not None:
                        observed_at = time.monotonic() - started
                        host_observed = verify_running_qemu(process.pid, command)
                        # Permit systemd and ext4 a brief flush window before the
                        # deliberate abrupt QEMU kill (simulated host power loss).
                        time.sleep(2)
                        return {
                            "label": label,
                            "slot": slot,
                            "expect": expect,
                            "lineage": lineage,
                            "marker_sha256": marker,
                            **host_observed,
                            "seconds": round(time.monotonic() - started, 3),
                            "seconds_to_observed": round(observed_at, 3),
                            "serial": log.name,
                            "observed_health": expect,
                            "power_loss": "QEMU_SIGKILL",
                        }
                if process.poll() is not None:
                    raise ValueError(
                        f"QEMU exited before {label} witness: {process.returncode}"
                    )
                time.sleep(0.5)
            if not quiet:
                raise ValueError(f"timeout waiting for {label} boot identity/health")
            host_observed = verify_running_qemu(process.pid, command)
            text = log.read_text(encoding="utf-8", errors="replace")
            wanted = "initrd=\\flowerly\\" + slot[-1] + "\\initramfs.img"
            if wanted not in text or "FLOWERLY_GENERATION_ROOT_VERIFIED" in text:
                raise ValueError(f"{label}: loader did not attempt the damaged slot")
            return {
                "label": label,
                "slot": slot,
                "expect": expect,
                "lineage": lineage,
                "marker_sha256": None,
                **host_observed,
                "seconds": round(time.monotonic() - started, 3),
                "seconds_to_observed": None,
                "serial": log.name,
                "observed_health": expect,
                "power_loss": "QEMU_SIGKILL",
            }
        finally:
            if process.poll() is None:
                process.kill()  # intentionally abrupt, not a clean shutdown
            process.wait(timeout=15)


def verify_immutable_roots(disk_path, manifest):
    """Prove guest boot did not modify either pinned root filesystem image.

    Never 'repair' a writeable root from the pristine fixture and then call
    the run immutable: that would conceal a real boot-chain regression.
    """
    parts = manifest.get("partitions", [])
    if not isinstance(parts, list) or [p.get("name") for p in parts] != [
        "esp",
        "root-a",
        "root-b",
        "data",
    ]:
        raise ValueError("cannot verify unknown A/B roots")
    image = Path(disk_path)
    if image.is_symlink() or not image.is_file():
        raise ValueError("unsafe guest disk when verifying root immutability")
    fd = os.open(image, os.O_RDONLY | os.O_NOFOLLOW)
    results = {}
    try:
        for part in parts[1:3]:
            offset, size = part.get("offset_bytes"), part.get("size_bytes")
            expected = part.get("sha256")
            if (
                type(offset) is not int
                or type(size) is not int
                or (size <= 0 or offset < 0 or offset + size > os.fstat(fd).st_size)
            ):
                raise ValueError("invalid pinned root partition extent")
            if not isinstance(expected, str) or not re.fullmatch(
                r"[0-9a-f]{64}", expected
            ):
                raise ValueError("missing expected pinned root SHA256")
            hash_value = hashlib.sha256()
            remaining = size
            while remaining:
                block = os.pread(fd, min(1048576, remaining), offset)
                if not block:
                    raise ValueError("short guest root partition read")
                hash_value.update(block)
                offset += len(block)
                remaining -= len(block)
            actual = hash_value.hexdigest()
            if actual != expected:
                raise ValueError(
                    "guest changed pinned "
                    + part["name"]
                    + " root image; refuse immutable-generation claim"
                )
            results[part["name"]] = actual
    finally:
        os.close(fd)
    return results


def reconcile_mutable_partitions(pristine_disk, booted_disk, out_disk, manifest):
    """Keep immutable pinned roots, carry A's real ESP and data writes forward."""
    shutil.copyfile(pristine_disk, out_disk)
    parts = manifest["partitions"]
    if [p["name"] for p in parts] != ["esp", "root-a", "root-b", "data"]:
        raise ValueError("cannot reconcile unknown disk geometry")
    copied = {}
    src = os.open(booted_disk, os.O_RDONLY | os.O_NOFOLLOW)
    dst = os.open(out_disk, os.O_RDWR | os.O_NOFOLLOW)
    try:
        for part in (parts[0], parts[3]):
            cursor = part["offset_bytes"]
            remaining = part["size_bytes"]
            digest = hashlib.sha256()
            while remaining:
                block = os.pread(src, min(1048576, remaining), cursor)
                if not block or os.pwrite(dst, block, cursor) != len(block):
                    raise OSError("short mutable partition copy")
                digest.update(block)
                cursor += len(block)
                remaining -= len(block)
            copied[part["name"]] = digest.hexdigest()
        os.fsync(dst)
    finally:
        os.close(src)
        os.close(dst)
    return copied


def slice_hashes(disk, manifest):
    """SHA-256 of every manifest partition of a regular disk image."""
    hashes = {}
    fd = os.open(disk, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        for part in manifest["partitions"]:
            digest = hashlib.sha256()
            cursor, remaining = part["offset_bytes"], part["size_bytes"]
            while remaining:
                block = os.pread(fd, min(1048576, remaining), cursor)
                if not block:
                    raise ValueError("short partition read")
                digest.update(block)
                cursor += len(block)
                remaining -= len(block)
            hashes[part["name"]] = digest.hexdigest()
    finally:
        os.close(fd)
    return hashes


def refuses(callable_, *arguments, **keywords):
    """True when the real tool refuses (ValueError); anything else raises."""
    try:
        callable_(*arguments, **keywords)
    except ValueError as error:
        return str(error)[:160]
    return None


def host_stage_scenarios(args, out, result):
    """Stage tool behaviour on real disk images (no guest needed)."""
    stage_tool = module("generation_slot_stage", "generation-slot-stage-image.py")
    verifier = module("generation_ab_verify_host", "verify-ab-slot-layout.py")
    unstaged_manifest = json.loads(args.unstaged_manifest.read_text())
    candidate_sha = sha_file(args.candidate_b)
    checks = result.setdefault("host_checks", {})

    # stage-complete: a full stage succeeds and is timed; an oversized
    # candidate ("full disk") is refused before any byte is written.
    disk = out / "stage-complete.img"
    manifest_file = out / "stage-complete.manifest.json"
    shutil.copyfile(args.unstaged_disk, disk)
    shutil.copyfile(args.unstaged_manifest, manifest_file)
    started = time.monotonic()
    staged = stage_tool.stage(
        disk, manifest_file, args.candidate_b, candidate_sha, args.root_a, "root-b"
    )
    result["measurements"]["stage_seconds"] = round(time.monotonic() - started, 3)
    staged_manifest = json.loads(manifest_file.read_text())
    verifier.verify(disk, staged_manifest)
    before = sha_file(disk)
    oversized = out / "oversized-candidate.bin"
    with oversized.open("wb") as handle:
        handle.truncate(
            next(p for p in staged_manifest["partitions"] if p["name"] == "root-b")[
                "size_bytes"
            ]
            + 1048576
        )
    refusal = refuses(
        stage_tool.stage,
        disk,
        manifest_file,
        oversized,
        sha_file(oversized),
        args.root_a,
        "root-b",
    )
    oversized.unlink()
    checks["stage-complete"] = {
        "ok": staged["status"] == "STAGED_NOT_ARMED"
        and staged["candidate_sha256"] == candidate_sha
        and refusal is not None
        and sha_file(disk) == before,
        "staged_disk_sha256": staged["disk_sha256"],
        "oversized_refusal": refusal,
    }
    disk.unlink()

    # stage-interrupted: power loss in the middle of the inactive-slot write.
    disk = out / "stage-interrupted.img"
    manifest_file = out / "stage-interrupted.manifest.json"
    shutil.copyfile(args.unstaged_disk, disk)
    shutil.copyfile(args.unstaged_manifest, manifest_file)
    manifest_before = manifest_file.read_bytes()
    hashes_before = slice_hashes(disk, unstaged_manifest)
    interrupted = None
    try:
        stage_tool.stage(
            disk,
            manifest_file,
            args.candidate_b,
            candidate_sha,
            args.root_a,
            "root-b",
            inject_after_chunks=300,
        )
    except RuntimeError as error:
        interrupted = str(error)[:120]
    hashes_after = slice_hashes(disk, unstaged_manifest)
    unchanged = all(
        hashes_after[name] == hashes_before[name] for name in ("esp", "root-a", "data")
    )
    partial = hashes_after["root-b"] != hashes_before["root-b"]
    reverify = refuses(verifier.verify, disk, unstaged_manifest)
    checks["stage-interrupted"] = {
        "ok": interrupted is not None
        and unchanged
        and partial
        and reverify is not None
        and manifest_file.read_bytes() == manifest_before,
        "fault": interrupted,
        "reverification_refusal": reverify,
        "root_b_partially_written": partial,
    }
    result["_stage_interrupted"] = (disk, unstaged_manifest, hashes_after)
    return checks


def host_tamper_scenario(args, out, manifest, result):
    """A tampered slot B must be refused everywhere and change nothing."""
    verifier = module("generation_ab_verify_tamper", "verify-ab-slot-layout.py")
    arm = module("generation_arm_efi_tamper", "generation-arm-esp-image.py")
    stage_tool = module(
        "generation_slot_stage_tamper", "generation-slot-stage-image.py"
    )
    disk = out / "tampered-b.img"
    shutil.copyfile(args.disk, disk)
    root_b = next(p for p in manifest["partitions"] if p["name"] == "root-b")
    with disk.open("r+b") as handle:
        handle.seek(root_b["offset_bytes"] + 1048576)
        byte = handle.read(1)
        handle.seek(root_b["offset_bytes"] + 1048576)
        handle.write(bytes([(byte[0] ^ 0xFF) if byte else 0xFF]))
    esp_before = slice_hashes(disk, manifest)["esp"]
    verify_refusal = refuses(verifier.verify, disk, manifest)
    outputs = (out / "tampered-armed.img", out / "tampered-armed.manifest.json")
    arm_refusal = refuses(
        arm.arm_image,
        disk,
        args.manifest,
        args.kernel_b,
        args.initramfs_b,
        outputs[0],
        outputs[1],
    )
    digest_refusal = refuses(
        stage_tool.stage,
        disk,
        args.manifest,
        args.candidate_b,
        "0" * 63 + "1",
        args.root_a,
        "root-b",
    )
    result.setdefault("host_checks", {})["tampered-b-refused"] = {
        "ok": all((verify_refusal, arm_refusal, digest_refusal))
        and not any(path.exists() for path in outputs)
        and slice_hashes(disk, manifest)["esp"] == esp_before,
        "layout_verifier": verify_refusal,
        "arm": arm_refusal,
        "stage_digest": digest_refusal,
    }
    disk.unlink()


def run(args):
    manifest = validate_source(args, actual_execution=True)
    args.output.mkdir(mode=0o700)
    result = {
        "schema": "flowerly-generation-kvm-sequence/v1",
        "source_sha": args.approved_sha,
        "release_acceptance": False,
        "sequence_verified": False,
        "scenarios": {name: "NOT_RUN" for name in SCENARIOS},
        "boots": [],
        "host_checks": {},
        "measurements": {
            "slot_bytes": {p["name"]: p["size_bytes"] for p in manifest["partitions"]},
            "clean_shutdown": "NOT_MEASURED",
        },
        "s7_remaining": ["clean-shutdown-time", "production-slot-tool-power-loss"],
        "disk_sha256_initial": sha_file(args.disk),
        "ovmf_code_sha256": sha_file(args.ovmf_code),
        "ovmf_vars_template_sha256": sha_file(args.ovmf_vars),
        "manifest_sha256": sha_file(args.manifest),
        "root_a_partuuid": args.root_a,
        "root_b_partuuid": args.root_b,
        "status": "INCOMPLETE",
        "boot_tries": args.tries,
    }
    file = args.output / "acceptance.json"
    arm = module("generation_arm_efi", "generation-arm-esp-image.py")
    out = args.output

    def fresh_vars(name):
        target = out / f"OVMF_VARS-{name}.fd"
        shutil.copyfile(args.ovmf_vars, target)
        return target

    def boot(disk, vars_file, slot, label, *, bless, quiet=False, lineage="main"):
        record = qemu_boot(
            args,
            disk,
            vars_file,
            out,
            slot,
            bless=bless,
            label=label,
            quiet=quiet,
            lineage=lineage,
        )
        result["boots"].append(record)
        return record

    def arm_from(source, name, **keywords):
        disk = out / f"{name}.img"
        manifest_file = out / f"{name}.manifest.json"
        arm.arm_image(
            source,
            args.manifest,
            args.kernel_b,
            args.initramfs_b,
            disk,
            manifest_file,
            tries=args.tries,
            **keywords,
        )
        return disk, manifest_file

    def assert_data(record, reference, what):
        if record["marker_sha256"] != reference["marker_sha256"]:
            raise ValueError(f"persistent data marker changed: {what}")

    try:
        qemu = shutil.which("qemu-system-x86_64")
        if not qemu:
            raise ValueError("real QEMU not installed")
        result["qemu_version"] = subprocess.run(
            [qemu, "--version"], capture_output=True, text=True, check=True
        ).stdout.splitlines()[0][:160]
        disk = out / "running-disk.img"
        vars_file = out / "OVMF_VARS.fd"
        shutil.copyfile(args.disk, disk)
        shutil.copyfile(args.ovmf_vars, vars_file)
        a = boot(disk, vars_file, "root-a", "boot-a", bless=True)
        result["root_sha256_after_boot_a"] = verify_immutable_roots(disk, manifest)
        # Later disks are NEW files based on A's current disk. A Generation
        # root is immutable, so retain its verified bytes and carry forward
        # *only* the live ESP boot-counting state and the user-data volume.
        reconstructed = out / "post-a-pristine-roots.img"
        result["mutable_sha256_from_boot_a"] = reconcile_mutable_partitions(
            args.disk, disk, reconstructed, manifest
        )
        result["root_reconstruction"] = "TEST_ONLY_PRESERVE_VERIFIED_ROOT_SLICES"

        # --- unhealthy B: boot count runs out, loader falls back to A.
        armed_disk, armed_manifest = arm_from(
            reconstructed, "armed-b", failure_switch=True
        )
        for index in range(args.tries):
            attempt = boot(
                armed_disk,
                vars_file,
                "root-b",
                f"unhealthy-b-{index + 1}",
                bless=False,
            )
            assert_data(attempt, a, "failed B boot")
        last = boot(armed_disk, vars_file, "root-a", "auto-fallback-a", bless=True)
        result["root_sha256_after_fallback"] = verify_immutable_roots(
            armed_disk, json.loads(armed_manifest.read_text())
        )
        assert_data(last, a, "automatic fallback to A")
        for name in ("boot-before-bless", "data-survives", "unhealthy-b-fallback"):
            result["scenarios"][name] = "PASS"
        armed_disk.unlink()

        # --- healthy B: activation sticks across a reboot, then rollback to A.
        ok_disk, ok_manifest = arm_from(reconstructed, "armed-b-ok")
        ok_vars = fresh_vars("healthy-b")
        b1 = boot(ok_disk, ok_vars, "root-b", "healthy-b-1", bless=True)
        verify_immutable_roots(ok_disk, json.loads(ok_manifest.read_text()))
        assert_data(b1, a, "first healthy B boot")
        b2 = boot(ok_disk, ok_vars, "root-b", "healthy-b-2", bless=True)
        assert_data(b2, a, "second healthy B boot")
        result["scenarios"]["healthy-b-activation"] = "PASS"
        rolled_disk = out / "rollback-a.img"
        rolled_manifest = out / "rollback-a.manifest.json"
        arm.rollback_image(ok_disk, ok_manifest, rolled_disk, rolled_manifest)
        ok_disk.unlink()
        rb = boot(
            rolled_disk, fresh_vars("rollback"), "root-a", "rollback-a", bless=True
        )
        verify_immutable_roots(rolled_disk, json.loads(rolled_manifest.read_text()))
        assert_data(rb, a, "rollback to A")
        result["scenarios"]["rollback-to-a"] = "PASS"
        rolled_disk.unlink()

        # --- power loss during the arm sequence: loader stays on verified A.
        for fault, label, scenario in (
            ("tmp-entry", "arm-before-rename-a", "arm-before-rename"),
            ("entry-no-default", "arm-after-rename-a", "arm-after-rename"),
        ):
            torn_disk, torn_manifest = arm_from(reconstructed, label, fault=fault)
            record = boot(torn_disk, fresh_vars(label), "root-a", label, bless=True)
            verify_immutable_roots(torn_disk, json.loads(torn_manifest.read_text()))
            assert_data(record, a, scenario)
            result["scenarios"][scenario] = "PASS"
            torn_disk.unlink()

        # --- damaged (torn) B entry: tries burn down without a witness, then A.
        damaged_disk, damaged_manifest = arm_from(
            reconstructed, "damaged-entry", fault="torn-entry"
        )
        damaged_vars = fresh_vars("damaged")
        for index in range(args.tries):
            boot(
                damaged_disk,
                damaged_vars,
                "root-b",
                f"damaged-b-{index + 1}",
                bless=False,
                quiet=True,
            )
        back = boot(
            damaged_disk, damaged_vars, "root-a", "damaged-fallback-a", bless=True
        )
        verify_immutable_roots(damaged_disk, json.loads(damaged_manifest.read_text()))
        assert_data(back, a, "damaged entry fallback")
        result["scenarios"]["damaged-entry-fallback"] = "PASS"
        damaged_disk.unlink()

        # --- stage-level scenarios on real disk images.
        if args.unstaged_disk:
            checks = host_stage_scenarios(args, out, result)
            interrupted_disk, interrupted_manifest, hashes_after = result.pop(
                "_stage_interrupted"
            )
            host_tamper_scenario(args, out, manifest, result)
            boot(
                interrupted_disk,
                fresh_vars("stage-interrupted"),
                "root-a",
                "stage-interrupted-a",
                bless=True,
                lineage="stage-interrupted",
            )
            now = slice_hashes(interrupted_disk, interrupted_manifest)
            if (
                now["root-a"] != hashes_after["root-a"]
                or now["root-b"] != hashes_after["root-b"]
            ):
                raise ValueError("guest wrote a root slot after interrupted stage")
            interrupted_disk.unlink()
            for name, check in checks.items():
                result["scenarios"][name] = "PASS" if check["ok"] else "FAIL"
            result["scenarios"]["tampered-b-refused"] = (
                "PASS" if result["host_checks"]["tampered-b-refused"]["ok"] else "FAIL"
            )
            if "stage_seconds" in result["measurements"]:
                result["scenarios"]["measure-stage-and-boot"] = "PASS"
        result["sequence_verified"] = True
        result["status"] = "SEQUENCE_OBSERVED_S7_INCOMPLETE"
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        result["status"] = "FAILED_OR_INCONCLUSIVE"
        result["error"] = str(error)[:350]
        raise
    finally:
        result.pop("_stage_interrupted", None)
        for name in ("running-disk.img", "OVMF_VARS.fd"):
            candidate = out / name
            if candidate.is_file():
                result[name + "_final_sha256"] = sha_file(candidate)
        file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
        # Hashes of serial evidence and the *final* disk may differ from the
        # initial manifest because systemd-boot and user data are mutable.
        hashes = {p.name: sha_file(p) for p in out.glob("*.serial.log")}
        (out / "serial-sha256.json").write_text(
            json.dumps(hashes, indent=2, sort_keys=True) + "\n"
        )
    if result["sequence_verified"]:
        auditor = module(
            "generation_ab_evidence_audit", "generation-ab-evidence-audit.py"
        )
        try:
            result["audit"] = auditor.audit(result, out)
        except (ValueError, TypeError, KeyError, OSError) as error:
            result["status"] = "EVIDENCE_AUDIT_FAILED"
            result["sequence_verified"] = False
            result["error"] = f"serial evidence audit refused: {error}"[:350]
            file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
            raise ValueError(result["error"]) from error
        file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--approved-sha", required=True)
    for name in (
        "disk",
        "manifest",
        "kernel-b",
        "initramfs-b",
        "ovmf-code",
        "ovmf-vars",
        "output",
    ):
        parser.add_argument(
            "--" + name, dest=name.replace("-", "_"), type=Path, required=True
        )
    for name in ("unstaged-disk", "unstaged-manifest", "candidate-b"):
        parser.add_argument("--" + name, dest=name.replace("-", "_"), type=Path)
    parser.add_argument("--root-a", required=True)
    parser.add_argument("--root-b", required=True)
    parser.add_argument("--tries", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    try:
        if args.execute:
            print("Starting explicitly authorized real KVM A/B acceptance", flush=True)
            result = run(args)
            print(
                json.dumps(
                    {
                        "status": result["status"],
                        "sequence_verified": result["sequence_verified"],
                        "release_acceptance": False,
                    },
                    sort_keys=True,
                )
            )
        else:
            validate_source(args)
            print(
                "VALIDATED ONLY; A→B→A KVM NOT_RUN. --execute requires "
                "explicit approved SHA, clean nonroot checkout and /dev/kvm."
            )
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Generation A/B KVM refusal/failure: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
