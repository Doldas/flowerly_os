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


def verify_running_qemu(pid, command):
    """Read *running host process* evidence, not QEMU's requested config."""
    if type(pid) is not int or pid <= 0 or not isinstance(command, list):
        raise ValueError("invalid QEMU process identity")
    proc = Path("/proc") / str(pid)
    actual_raw = (proc / "cmdline").read_bytes()
    actual_argv = [arg.decode("utf-8", errors="strict")
                   for arg in actual_raw.split(bytes([0])) if arg]
    if actual_argv != command or not all(arg in actual_argv for arg in (
        "-net", "none", "-no-reboot", "-nodefaults", "q35,accel=kvm"
    )):
        raise ValueError("running QEMU command differs from approved KVM firmware boot")
    if any(value in actual_argv for value in ("-kernel", "-initrd", "-nic", "-netdev")):
        raise ValueError("direct-kernel or networked QEMU is not valid firmware evidence")
    fd_directory = proc / "fd"
    devices = []
    for fd in fd_directory.iterdir():
        try:
            devices.append(os.readlink(fd))
        except FileNotFoundError:
            continue
    if "/dev/kvm" not in devices:
        raise ValueError("no open /dev/kvm descriptor in actual QEMU process")
    return {"kvm_fd_verified": True,
            "qemu_cmdline_sha256": hashlib.sha256(actual_raw).hexdigest()}


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
                    host_observed = verify_running_qemu(process.pid, command)
                    # Permit systemd and ext4 a brief flush window before the
                    # deliberate abrupt QEMU kill (simulated host power loss).
                    time.sleep(2)
                    return {"slot": slot, "marker_sha256": marker,
                            **host_observed,
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


def verify_immutable_roots(disk_path, manifest):
    """Prove guest boot did not modify either pinned root filesystem image.

    Never 'repair' a writeable root from the pristine fixture and then call
    the run immutable: that would conceal a real boot-chain regression.
    """
    parts = manifest.get("partitions", [])
    if not isinstance(parts, list) or [p.get("name") for p in parts] != [
        "esp", "root-a", "root-b", "data"
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
            if type(offset) is not int or type(size) is not int or (
                size <= 0 or offset < 0 or offset + size > os.fstat(fd).st_size
            ):
                raise ValueError("invalid pinned root partition extent")
            if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
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
                raise ValueError("guest changed pinned " + part["name"] +
                                 " root image; refuse immutable-generation claim")
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
        result["root_sha256_after_boot_a"] = verify_immutable_roots(disk, manifest)
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
        result["root_sha256_after_fallback"] = verify_immutable_roots(
            armed_disk, json.loads(armed_manifest.read_text()))
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
    if result["sequence_verified"]:
        auditor = module("generation_ab_evidence_audit", "generation-ab-evidence-audit.py")
        try:
            result["audit"] = auditor.audit(result, args.output)
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
