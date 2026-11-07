#!/usr/bin/env python3
"""Opt-in REAL OVMF/KVM A→failed-B→A acceptance on Flowerly-built raw images.

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
    "stage-complete", "stage-interrupted", "arm-before-rename",
    "arm-after-rename", "boot-before-bless", "unhealthy-b-fallback",
    "data-survives", "tampered-b-refused",
)
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
    sha = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"],
                         text=True, capture_output=True, check=True).stdout.strip()
    if sha != args.approved_sha:
        raise ValueError("approved SHA does not match checked-out source")
    if actual_execution:
        clean = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain"],
                               text=True, capture_output=True, check=True).stdout
        if clean:
            raise ValueError("dirty git checkout must not execute KVM")
        if os.geteuid() == 0:
            raise ValueError("do not run the KVM acceptance as root")
        if os.environ.get("FLOWERLY_GENERATION_AB_KVM") != TOKEN:
            raise ValueError("explicit Generation KVM environment approval missing")
        if not os.access("/dev/kvm", os.R_OK | os.W_OK):
            raise ValueError("usable real /dev/kvm required; no TCG fallback")
    for name in ("disk", "manifest", "kernel_b", "initramfs_b",
                 "ovmf_code", "ovmf_vars"):
        path = getattr(args, name)
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError("missing or unsafe input: " + name)
    if args.output.exists() or args.output.is_symlink():
        raise ValueError("refusing to overwrite existing evidence directory")
    if args.output.parent.is_symlink():
        raise ValueError("unsafe evidence parent")
    if not 30 <= args.timeout <= 600 or not 1 <= args.tries <= 9:
        raise ValueError("unsafe timeout / tries configuration")
    if not UUID.fullmatch(args.root_a) or not UUID.fullmatch(args.root_b) or args.root_a == args.root_b:
        raise ValueError("invalid or duplicate root UUIDs")
    manifest = json.loads(args.manifest.read_text())
    verifier = module("generation_ab_verify", "verify-ab-slot-layout.py")
    verifier.verify(args.disk, manifest)
    identities = {p["name"]: p["partuuid"] for p in manifest["partitions"]}
    if identities.get("root-a") != args.root_a or identities.get("root-b") != args.root_b:
        raise ValueError("approved booted root UUIDs do not match the disk")
    if actual_execution:
        for program in ("qemu-system-x86_64", "sfdisk", "mcopy", "mdir", "mtype"):
            if not shutil.which(program, path=os.environ["PATH"] + ":/usr/sbin:/sbin"):
                raise ValueError("missing real host tool: " + program)
    return manifest


def observe_serial(log, slot, *, bless, timeout):
    """Observe a real guest, not a mocked count or host-side assertion."""
    text = log.read_text(encoding="utf-8", errors="replace") if log.exists() else ""
    if "Kernel panic" in text or "Failed to start Flowerly OS Unified Supervisor" in text:
        raise ValueError("guest kernel panic or Flowerly supervisor failure")
    seen = all(phrase in text for phrase in (
        "FLOWERLY_GENERATION_ROOT_VERIFIED", '"slot": "' + slot + '"'
    ))
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


# These parsers establish internal evidence consistency; they do NOT prove
# the serial output originated from an authenticated guest (that requires
# externally trusted QEMU execution and artifact custody).
BOOT_JSON = re.compile(
    r'(?m)^\s*(?:\[\s*[\d.]+\]\s+[\w.-]+\[\d+\]:\s*)?'
    r'(\{"boot_id":\s*"[0-9a-f-]+",\s*"root_partuuid":\s*"[0-9a-f-]+",\s*'
    r'"slot":\s*"root-[ab]"\})\s*\r?
    """Start one real firmware boot; kill guest after the observed milestone."""
    log = report / (label + ".serial.log")
    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        raise ValueError("qemu-system-x86_64 not installed")
    command = [
        qemu, "-machine", "q35,accel=kvm", "-cpu", "host", "-smp", "2",
        "-m", "2048", "-nodefaults", "-display", "none",
        "-serial", "stdio", "-monitor", "none", "-net", "none",
        "-no-reboot",
        "-drive", "if=pflash,format=raw,readonly=on,file=" + str(args.ovmf_code),
        "-drive", "if=pflash,format=raw,file=" + str(vars_file),
        "-drive", "if=virtio,format=raw,cache=none,file=" + str(disk),
    ]
    started = time.monotonic()
    with log.open("xb") as output:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while time.monotonic() - started <= args.timeout:
                marker = observe_serial(log, slot, bless=bless, timeout=args.timeout)
                if marker is not None:
                    # Permit systemd and ext4 a brief flush window before the
                    # deliberate abrupt QEMU kill (simulated host power loss).
                    time.sleep(2)
                    return {"slot": slot, "marker_sha256": marker,
                            "seconds": round(time.monotonic() - started, 3),
                            "serial": log.name, "observed_health": "BLESSED" if bless else "REJECTED",
                            "power_loss": "QEMU_SIGKILL"}
                if process.poll() is not None:
                    raise ValueError(f"QEMU exited before {label} witness: {process.returncode}")
                time.sleep(0.5)
            raise ValueError(f"timeout waiting for {label} boot identity/health")
        finally:
            if process.poll() is None:
                process.kill()  # intentionally abrupt, not a clean shutdown
            process.wait(timeout=15)


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
    try:
        qemu = shutil.which("qemu-system-x86_64")
        if not qemu:
            raise ValueError("real QEMU not installed")
        result["qemu_version"] = subprocess.run(
            [qemu, "--version"], capture_output=True, text=True,
            check=True).stdout.splitlines()[0][:160]
        disk = args.output / "running-disk.img"
        vars_file = args.output / "OVMF_VARS.fd"
        shutil.copyfile(args.disk, disk)
        shutil.copyfile(args.ovmf_vars, vars_file)
        a = qemu_boot(args, disk, vars_file, args.output,
                      "root-a", bless=True, label="boot-a")
        result["boots"].append(a)
        # The armed image is a NEW file based on A's current disk. It carries
        # A's user-data changes forward rather than rolling them back.
        arm = module("generation_arm_efi", "generation-arm-esp-image.py")
        reconstructed = args.output / "post-a-pristine-roots.img"
        # A's ext4 root can receive incidental journaling writes. A Generation
        # root is immutable, so retain its original verified bytes and carry
        # forward *only* the live ESP boot-counting state and user-data volume.
        result["mutable_sha256_from_boot_a"] = reconcile_mutable_partitions(
            args.disk, disk, reconstructed, manifest)
        result["root_reconstruction"] = "TEST_ONLY_PRESERVE_VERIFIED_ROOT_SLICES"
        armed_disk = args.output / "armed-b.img"
        armed_manifest = args.output / "armed-b.manifest.json"
        arm.arm_image(reconstructed, args.manifest, args.kernel_b, args.initramfs_b,
                      armed_disk, armed_manifest, tries=args.tries,
                      failure_switch=True)
        # Retry unhealthy B using the SAME EFI variables and writable image.
        # systemd-boot must exhaust B's boot-count and select last healthy A.
        for index in range(args.tries):
            attempt = qemu_boot(args, armed_disk, vars_file, args.output,
                                "root-b", bless=False, label=f"unhealthy-b-{index+1}")
            result["boots"].append(attempt)
            if attempt["marker_sha256"] != a["marker_sha256"]:
                raise ValueError("persistent data marker changed on failed B boot")
        result["scenarios"]["boot-before-bless"] = "PASS"
        last = qemu_boot(args, armed_disk, vars_file, args.output,
                         "root-a", bless=True, label="auto-fallback-a")
        result["boots"].append(last)
        if last["marker_sha256"] != a["marker_sha256"]:
            raise ValueError("user data changed on automatic fallback to A")
        result["scenarios"]["data-survives"] = "PASS"
        result["scenarios"]["unhealthy-b-fallback"] = "PASS"
        result["sequence_verified"] = True
        result["status"] = "SEQUENCE_OBSERVED_S7_INCOMPLETE"
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        result["status"] = "FAILED_OR_INCONCLUSIVE"
        result["error"] = str(error)[:350]
        raise
    finally:
        for name in ("armed-b.img", "running-disk.img", "OVMF_VARS.fd"):
            candidate = args.output / name
            if candidate.is_file():
                result[name + "_final_sha256"] = sha_file(candidate)
        file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
        # Hashes of serial evidence and the *final* disk may differ from the
        # initial manifest because systemd-boot and user data are mutable.
        hashes = {p.name: sha_file(p) for p in args.output.glob("*.serial.log")}
        (args.output / "serial-sha256.json").write_text(
            json.dumps(hashes, indent=2, sort_keys=True) + "\n")
    result["audit"] = audit_evidence(result, args.output)
    file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--approved-sha", required=True)
    for name in ("disk", "manifest", "kernel-b", "initramfs-b",
                 "ovmf-code", "ovmf-vars", "output"):
        parser.add_argument("--" + name, dest=name.replace("-", "_"),
                            type=Path, required=True)
    parser.add_argument("--root-a", required=True)
    parser.add_argument("--root-b", required=True)
    parser.add_argument("--tries", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    try:
        if args.execute:
            print("Starting explicitly authorized real KVM A→B→A acceptance", flush=True)
            result = run(args)
            print(json.dumps({"status": result["status"],
                              "sequence_verified": result["sequence_verified"],
                              "release_acceptance": False}, sort_keys=True))
        else:
            validate_source(args)
            print("VALIDATED ONLY; A→B→A KVM NOT_RUN. --execute requires "
                  "explicit approved SHA, clean nonroot checkout and /dev/kvm.")
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Generation A/B KVM refusal/failure: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

)
MARKER_LINE = re.compile(
    r'(?m)^.*FLOWERLY_GENERATION_USER_MARKER_SHA256=([0-9a-f]{64})\s*\r?
    """Start one real firmware boot; kill guest after the observed milestone."""
    log = report / (label + ".serial.log")
    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        raise ValueError("qemu-system-x86_64 not installed")
    command = [
        qemu, "-machine", "q35,accel=kvm", "-cpu", "host", "-smp", "2",
        "-m", "2048", "-nodefaults", "-display", "none",
        "-serial", "stdio", "-monitor", "none", "-net", "none",
        "-no-reboot",
        "-drive", "if=pflash,format=raw,readonly=on,file=" + str(args.ovmf_code),
        "-drive", "if=pflash,format=raw,file=" + str(vars_file),
        "-drive", "if=virtio,format=raw,cache=none,file=" + str(disk),
    ]
    started = time.monotonic()
    with log.open("xb") as output:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while time.monotonic() - started <= args.timeout:
                marker = observe_serial(log, slot, bless=bless, timeout=args.timeout)
                if marker is not None:
                    # Permit systemd and ext4 a brief flush window before the
                    # deliberate abrupt QEMU kill (simulated host power loss).
                    time.sleep(2)
                    return {"slot": slot, "marker_sha256": marker,
                            "seconds": round(time.monotonic() - started, 3),
                            "serial": log.name, "observed_health": "BLESSED" if bless else "REJECTED",
                            "power_loss": "QEMU_SIGKILL"}
                if process.poll() is not None:
                    raise ValueError(f"QEMU exited before {label} witness: {process.returncode}")
                time.sleep(0.5)
            raise ValueError(f"timeout waiting for {label} boot identity/health")
        finally:
            if process.poll() is None:
                process.kill()  # intentionally abrupt, not a clean shutdown
            process.wait(timeout=15)


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
        "disk_sha256_initial": sha_file(args.disk),
        "ovmf_code_sha256": sha_file(args.ovmf_code),
        "ovmf_vars_template_sha256": sha_file(args.ovmf_vars),
        "manifest_sha256": sha_file(args.manifest),
        "root_a_partuuid": args.root_a,
        "root_b_partuuid": args.root_b,
        "status": "INCOMPLETE",
    }
    file = args.output / "acceptance.json"
    try:
        qemu = shutil.which("qemu-system-x86_64")
        if not qemu:
            raise ValueError("real QEMU not installed")
        result["qemu_version"] = subprocess.run(
            [qemu, "--version"], capture_output=True, text=True,
            check=True).stdout.splitlines()[0][:160]
        disk = args.output / "running-disk.img"
        vars_file = args.output / "OVMF_VARS.fd"
        shutil.copyfile(args.disk, disk)
        shutil.copyfile(args.ovmf_vars, vars_file)
        a = qemu_boot(args, disk, vars_file, args.output,
                      "root-a", bless=True, label="boot-a")
        result["boots"].append(a)
        # The armed image is a NEW file based on A's current disk. It carries
        # A's user-data changes forward rather than rolling them back.
        arm = module("generation_arm_efi", "generation-arm-esp-image.py")
        reconstructed = args.output / "post-a-pristine-roots.img"
        # A's ext4 root can receive incidental journaling writes. A Generation
        # root is immutable, so retain its original verified bytes and carry
        # forward *only* the live ESP boot-counting state and user-data volume.
        result["mutable_sha256_from_boot_a"] = reconcile_mutable_partitions(
            args.disk, disk, reconstructed, manifest)
        result["root_reconstruction"] = "TEST_ONLY_PRESERVE_VERIFIED_ROOT_SLICES"
        armed_disk = args.output / "armed-b.img"
        armed_manifest = args.output / "armed-b.manifest.json"
        arm.arm_image(reconstructed, args.manifest, args.kernel_b, args.initramfs_b,
                      armed_disk, armed_manifest, tries=args.tries,
                      failure_switch=True)
        # Retry unhealthy B using the SAME EFI variables and writable image.
        # systemd-boot must exhaust B's boot-count and select last healthy A.
        for index in range(args.tries):
            attempt = qemu_boot(args, armed_disk, vars_file, args.output,
                                "root-b", bless=False, label=f"unhealthy-b-{index+1}")
            result["boots"].append(attempt)
            if attempt["marker_sha256"] != a["marker_sha256"]:
                raise ValueError("persistent data marker changed on failed B boot")
        result["scenarios"]["boot-before-bless"] = "PASS"
        last = qemu_boot(args, armed_disk, vars_file, args.output,
                         "root-a", bless=True, label="auto-fallback-a")
        result["boots"].append(last)
        if last["marker_sha256"] != a["marker_sha256"]:
            raise ValueError("user data changed on automatic fallback to A")
        result["scenarios"]["data-survives"] = "PASS"
        result["scenarios"]["unhealthy-b-fallback"] = "PASS"
        result["sequence_verified"] = True
        result["status"] = "SEQUENCE_OBSERVED_S7_INCOMPLETE"
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        result["status"] = "FAILED_OR_INCONCLUSIVE"
        result["error"] = str(error)[:350]
        raise
    finally:
        for name in ("armed-b.img", "running-disk.img", "OVMF_VARS.fd"):
            candidate = args.output / name
            if candidate.is_file():
                result[name + "_final_sha256"] = sha_file(candidate)
        file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
        # Hashes of serial evidence and the *final* disk may differ from the
        # initial manifest because systemd-boot and user data are mutable.
        hashes = {p.name: sha_file(p) for p in args.output.glob("*.serial.log")}
        (args.output / "serial-sha256.json").write_text(
            json.dumps(hashes, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--approved-sha", required=True)
    for name in ("disk", "manifest", "kernel-b", "initramfs-b",
                 "ovmf-code", "ovmf-vars", "output"):
        parser.add_argument("--" + name, dest=name.replace("-", "_"),
                            type=Path, required=True)
    parser.add_argument("--root-a", required=True)
    parser.add_argument("--root-b", required=True)
    parser.add_argument("--tries", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    try:
        if args.execute:
            print("Starting explicitly authorized real KVM A→B→A acceptance", flush=True)
            result = run(args)
            print(json.dumps({"status": result["status"],
                              "sequence_verified": result["sequence_verified"],
                              "release_acceptance": False}, sort_keys=True))
        else:
            validate_source(args)
            print("VALIDATED ONLY; A→B→A KVM NOT_RUN. --execute requires "
                  "explicit approved SHA, clean nonroot checkout and /dev/kvm.")
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Generation A/B KVM refusal/failure: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

)


def parse_boot_evidence(path, *, expected_slot, expected_partuuid, expect_bless):
    """Validate exactly one guest identity and one matching health outcome."""
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 16*1024*1024:
        raise ValueError("missing, symlinked or oversized serial log")
    text = path.read_text(encoding="utf-8", errors="replace")
    if "Kernel panic" in text or "Failed to start Flowerly OS Unified Supervisor" in text:
        raise ValueError("guest reported kernel panic / Flowerly failure")
    if text.count("FLOWERLY_GENERATION_ROOT_VERIFIED") != 1:
        raise ValueError("missing or repeated root verification marker")
    matches = BOOT_JSON.findall(text)
    if len(matches) != 1:
        raise ValueError("missing or repeated guest root identity")
    try:
        boot = json.loads(matches[0])
    except (ValueError, TypeError) as error:
        raise ValueError("invalid guest root identity JSON") from error
    if boot.get("slot") != expected_slot or boot.get("root_partuuid") != expected_partuuid:
        raise ValueError("guest selected wrong slot or PARTUUID")
    boot_id = boot.get("boot_id")
    if not isinstance(boot_id, str) or not UUID.fullmatch(boot_id):
        raise ValueError("invalid boot UUID evidence")
    marker = BOOT_SHA.findall(text)
    if len(marker) != 1:
        raise ValueError("user data marker missing or repeated")
    blessed = text.count("FLOWERLY_GENERATION_BOOT_BLESSED")
    healthy = text.count("FLOWERLY_GENERATION_IPC_VERIFIED")
    injected = text.count("injected unhealthy Generation boot")
    if expect_bless:
        if (blessed, healthy, injected) != (1, 1, 0):
            raise ValueError("guest did not explicitly pass IPC and boot blessing")
    elif (blessed, healthy, injected) != (0, 0, 1):
        raise ValueError("unhealthy guest was blessed or failed to reject boot")
    return {"boot_id": boot_id, "slot": expected_slot,
            "root_partuuid": expected_partuuid, "marker_sha256": marker[0],
            "blessed": expect_bless, "log_sha256": sha_file(path)}


def audit_evidence(report, directory):
    """Re-read every serial artifact; reject stale, missing or inconsistent data.

    This is a consistency gate over evidence produced by a trusted KVM run,
    not a substitute for the owner's approval or the eight S7 scenarios.
    """
    if not isinstance(report, dict) or report.get("schema") != "flowerly-generation-kvm-sequence/v1":
        raise ValueError("unknown Generation KVM evidence schema")
    if report.get("release_acceptance") is not False:
        raise ValueError("KVM sequence cannot independently certify release")
    source = report.get("source_sha")
    if not isinstance(source, str) or not COMMIT.fullmatch(source):
        raise ValueError("missing exact source SHA")
    required = ("disk_sha256_initial", "ovmf_code_sha256", "ovmf_vars_template_sha256",
                "manifest_sha256")
    if any(not isinstance(report.get(k), str) or not re.fullmatch(r"[0-9a-f]{64}", report[k])
           for k in required):
        raise ValueError("missing or malformed artifact provenance")
    a_uuid, b_uuid = report.get("root_a_partuuid"), report.get("root_b_partuuid")
    if not isinstance(a_uuid, str) or not isinstance(b_uuid, str) or (
        not UUID.fullmatch(a_uuid) or not UUID.fullmatch(b_uuid) or a_uuid == b_uuid
    ):
        raise ValueError("invalid pinned root identities")
    scenarios = report.get("scenarios")
    if not isinstance(scenarios, dict) or set(scenarios) != set(SCENARIOS) or (
        any(value not in ("PASS", "FAIL", "NOT_RUN", "INCONCLUSIVE")
            for value in scenarios.values())
    ):
        raise ValueError("invalid or incomplete power-loss scenario catalog")
    # No scenario lacking a measured real run may silently become PASS.
    measured = {"boot-before-bless", "unhealthy-b-fallback", "data-survives"}
    if any(value == "PASS" for key, value in scenarios.items() if key not in measured):
        raise ValueError("unexecuted power-loss scenario falsely marked PASS")
    boots = report.get("boots")
    if not isinstance(boots, list) or len(boots) < 3 or len(boots) > 12:
        raise ValueError("no complete A/B/A boot sequence")
    if report.get("sequence_verified") is not True or report.get("status") != "SEQUENCE_OBSERVED_S7_INCOMPLETE":
        raise ValueError("sequence report not complete")
    if any(scenarios[key] != "PASS" for key in measured):
        raise ValueError("missing expected observed sequence scenarios")
    if len(boots) != 2 + sum(b.get("slot") == "root-b" for b in boots if isinstance(b, dict)):
        raise ValueError("unexpected boot sequence count")
    if boots[0].get("slot") != "root-a" or boots[-1].get("slot") != "root-a":
        raise ValueError("A→B→A order not observed")
    if any(boot.get("slot") != "root-b" for boot in boots[1:-1]):
        raise ValueError("unexpected non-B slot among failed boots")
    tries = report.get("boot_tries")
    if type(tries) is not int or tries < 1 or tries > 9 or len(boots) != tries + 2:
        raise ValueError("B tries do not match observed boot count")
    directory = Path(directory)
    seen_ids, log_names, markers = set(), set(), set()
    serial_hashes = {}
    for i, boot in enumerate(boots):
        if not isinstance(boot, dict):
            raise ValueError("malformed boot evidence item")
        slot = "root-b" if 0 < i < len(boots)-1 else "root-a"
        expected_partuuid = b_uuid if slot == "root-b" else a_uuid
        label = boot.get("serial")
        if not isinstance(label, str) or not re.fullmatch(r"[a-z0-9-]+\.serial\.log", label):
            raise ValueError("unsafe or missing serial filename")
        if label in log_names:
            raise ValueError("same serial log reused for multiple boots")
        log_names.add(label)
        expected_label = "boot-a.serial.log" if i == 0 else (
            "auto-fallback-a.serial.log" if i == len(boots)-1 else f"unhealthy-b-{i}.serial.log"
        )
        if label != expected_label:
            raise ValueError("serial log named for wrong boot attempt")
        parsed = parse_boot_evidence(
            directory / label, expected_slot=slot,
            expected_partuuid=expected_partuuid, expect_bless=slot == "root-a")
        if parsed["boot_id"] in seen_ids:
            raise ValueError("same guest boot ID reused across boots")
        seen_ids.add(parsed["boot_id"])
        if boot.get("marker_sha256") != parsed["marker_sha256"] or boot.get("slot") != slot:
            raise ValueError("reported guest boot does not match its serial")
        if boot.get("observed_health") != ("BLESSED" if slot == "root-a" else "REJECTED"):
            raise ValueError("claimed health verdict contradicts serial")
        if boot.get("power_loss") != "QEMU_SIGKILL":
            raise ValueError("missing QEMU kill evidence")
        markers.add(parsed["marker_sha256"])
        serial_hashes[label] = parsed["log_sha256"]
    if len(markers) != 1:
        raise ValueError("persistent user-data marker drift across A/B/A")
    digest_file = directory / "serial-sha256.json"
    if digest_file.is_symlink() or not digest_file.is_file():
        raise ValueError("missing sealed serial SHA listing")
    try:
        recorded = json.loads(digest_file.read_text())
    except (OSError, ValueError) as error:
        raise ValueError("invalid sealed serial listing") from error
    if recorded != serial_hashes:
        raise ValueError("serial digests do not match recorded artifact manifest")
    if set(p.name for p in directory.glob("*.serial.log")) != log_names:
        raise ValueError("unaccounted serial logs in evidence directory")
    return {"status": "INTERNALLY_CONSISTENT_NOT_RELEASE_ACCEPTED",
            "boots": len(boots), "persistent_marker_sha256": next(iter(markers)),
            "source_sha": source}


def qemu_boot(args, disk, vars_file, report, slot, *, bless, label):
    """Start one real firmware boot; kill guest after the observed milestone."""
    log = report / (label + ".serial.log")
    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        raise ValueError("qemu-system-x86_64 not installed")
    command = [
        qemu, "-machine", "q35,accel=kvm", "-cpu", "host", "-smp", "2",
        "-m", "2048", "-nodefaults", "-display", "none",
        "-serial", "stdio", "-monitor", "none", "-net", "none",
        "-no-reboot",
        "-drive", "if=pflash,format=raw,readonly=on,file=" + str(args.ovmf_code),
        "-drive", "if=pflash,format=raw,file=" + str(vars_file),
        "-drive", "if=virtio,format=raw,cache=none,file=" + str(disk),
    ]
    started = time.monotonic()
    with log.open("xb") as output:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while time.monotonic() - started <= args.timeout:
                marker = observe_serial(log, slot, bless=bless, timeout=args.timeout)
                if marker is not None:
                    # Permit systemd and ext4 a brief flush window before the
                    # deliberate abrupt QEMU kill (simulated host power loss).
                    time.sleep(2)
                    return {"slot": slot, "marker_sha256": marker,
                            "seconds": round(time.monotonic() - started, 3),
                            "serial": log.name, "observed_health": "BLESSED" if bless else "REJECTED",
                            "power_loss": "QEMU_SIGKILL"}
                if process.poll() is not None:
                    raise ValueError(f"QEMU exited before {label} witness: {process.returncode}")
                time.sleep(0.5)
            raise ValueError(f"timeout waiting for {label} boot identity/health")
        finally:
            if process.poll() is None:
                process.kill()  # intentionally abrupt, not a clean shutdown
            process.wait(timeout=15)


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
        "disk_sha256_initial": sha_file(args.disk),
        "ovmf_code_sha256": sha_file(args.ovmf_code),
        "ovmf_vars_template_sha256": sha_file(args.ovmf_vars),
        "manifest_sha256": sha_file(args.manifest),
        "root_a_partuuid": args.root_a,
        "root_b_partuuid": args.root_b,
        "status": "INCOMPLETE",
    }
    file = args.output / "acceptance.json"
    try:
        qemu = shutil.which("qemu-system-x86_64")
        if not qemu:
            raise ValueError("real QEMU not installed")
        result["qemu_version"] = subprocess.run(
            [qemu, "--version"], capture_output=True, text=True,
            check=True).stdout.splitlines()[0][:160]
        disk = args.output / "running-disk.img"
        vars_file = args.output / "OVMF_VARS.fd"
        shutil.copyfile(args.disk, disk)
        shutil.copyfile(args.ovmf_vars, vars_file)
        a = qemu_boot(args, disk, vars_file, args.output,
                      "root-a", bless=True, label="boot-a")
        result["boots"].append(a)
        # The armed image is a NEW file based on A's current disk. It carries
        # A's user-data changes forward rather than rolling them back.
        arm = module("generation_arm_efi", "generation-arm-esp-image.py")
        reconstructed = args.output / "post-a-pristine-roots.img"
        # A's ext4 root can receive incidental journaling writes. A Generation
        # root is immutable, so retain its original verified bytes and carry
        # forward *only* the live ESP boot-counting state and user-data volume.
        result["mutable_sha256_from_boot_a"] = reconcile_mutable_partitions(
            args.disk, disk, reconstructed, manifest)
        result["root_reconstruction"] = "TEST_ONLY_PRESERVE_VERIFIED_ROOT_SLICES"
        armed_disk = args.output / "armed-b.img"
        armed_manifest = args.output / "armed-b.manifest.json"
        arm.arm_image(reconstructed, args.manifest, args.kernel_b, args.initramfs_b,
                      armed_disk, armed_manifest, tries=args.tries,
                      failure_switch=True)
        # Retry unhealthy B using the SAME EFI variables and writable image.
        # systemd-boot must exhaust B's boot-count and select last healthy A.
        for index in range(args.tries):
            attempt = qemu_boot(args, armed_disk, vars_file, args.output,
                                "root-b", bless=False, label=f"unhealthy-b-{index+1}")
            result["boots"].append(attempt)
            if attempt["marker_sha256"] != a["marker_sha256"]:
                raise ValueError("persistent data marker changed on failed B boot")
        result["scenarios"]["boot-before-bless"] = "PASS"
        last = qemu_boot(args, armed_disk, vars_file, args.output,
                         "root-a", bless=True, label="auto-fallback-a")
        result["boots"].append(last)
        if last["marker_sha256"] != a["marker_sha256"]:
            raise ValueError("user data changed on automatic fallback to A")
        result["scenarios"]["data-survives"] = "PASS"
        result["scenarios"]["unhealthy-b-fallback"] = "PASS"
        result["sequence_verified"] = True
        result["status"] = "SEQUENCE_OBSERVED_S7_INCOMPLETE"
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        result["status"] = "FAILED_OR_INCONCLUSIVE"
        result["error"] = str(error)[:350]
        raise
    finally:
        for name in ("armed-b.img", "running-disk.img", "OVMF_VARS.fd"):
            candidate = args.output / name
            if candidate.is_file():
                result[name + "_final_sha256"] = sha_file(candidate)
        file.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
        # Hashes of serial evidence and the *final* disk may differ from the
        # initial manifest because systemd-boot and user data are mutable.
        hashes = {p.name: sha_file(p) for p in args.output.glob("*.serial.log")}
        (args.output / "serial-sha256.json").write_text(
            json.dumps(hashes, indent=2, sort_keys=True) + "\n")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--approved-sha", required=True)
    for name in ("disk", "manifest", "kernel-b", "initramfs-b",
                 "ovmf-code", "ovmf-vars", "output"):
        parser.add_argument("--" + name, dest=name.replace("-", "_"),
                            type=Path, required=True)
    parser.add_argument("--root-a", required=True)
    parser.add_argument("--root-b", required=True)
    parser.add_argument("--tries", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    try:
        if args.execute:
            print("Starting explicitly authorized real KVM A→B→A acceptance", flush=True)
            result = run(args)
            print(json.dumps({"status": result["status"],
                              "sequence_verified": result["sequence_verified"],
                              "release_acceptance": False}, sort_keys=True))
        else:
            validate_source(args)
            print("VALIDATED ONLY; A→B→A KVM NOT_RUN. --execute requires "
                  "explicit approved SHA, clean nonroot checkout and /dev/kvm.")
    except (ValueError, OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Generation A/B KVM refusal/failure: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
