# Evolution Lab (`evolutiond lab`)

The Evolution Lab boots a candidate Generation under QEMU/KVM, checks what
the guest reports against what was built, and records the candidate as
`TESTED` or `REJECTED` with a complete Experiment record. The design is
ADR-0024 and the work plan is the 0.0.6 Evolution Lab plan, both proposed
in #415.

**Status (0.0.6, #410, #411):** the `evolutiond lab` commands, plan, report
and result formats, checks and promotion policy are implemented and covered by
unit tests and fake-QEMU tests. The test-profile [guest probe](#the-guest-probe)
is implemented and tested offline; a shared console fixture ties its output to
the host parser. The candidate build and `just vm-test` (#412) and real KVM
acceptance (#413) are separate steps. No KVM run has used this code yet.

## What the lab is and is not

- It runs in the **foreground as the invoking user** with access to
  `/dev/kvm`. It never escalates (no `sudo`, no setuid, no broker) and the
  daemon's `evolution.run_experiment` stays `unavailable` (#95).
- One QEMU process per boot: OVMF firmware with a **private copy** of its
  variable store, the candidate disk, `-net none`, `-monitor none`,
  `-nodefaults`, no host sharing, no direct kernel boot, `-no-reboot`, the
  serial console captured to a file. A KVM boot is verified on the running
  process: its `/proc/<pid>/cmdline` must equal the approved argv and it
  must hold `/dev/kvm` open (the Gate D check). TCG runs only with
  `--allow-tcg` and is never evidence.
- Each boot has a report deadline and a shutdown deadline. When one passes,
  or the guest floods its console (64 MiB) or sends a damaged report, the
  whole QEMU process group is killed.
- It recommends `generation.test` or `generation.reject` and applies the
  recommendation only on `record`. **It never activates**; activation stays a
  witnessed, human-requested Admin action (ADR-0023).

## Commands and exit status

```text
evolutiond lab run    --plan PLAN --out NEW_DIR [--qemu PATH] [--allow-tcg]
                      [--record] [--runtime-dir DIR]
evolutiond lab verify --dir DIR
evolutiond lab record --dir DIR [--runtime-dir DIR]
```

| Status | Meaning |
| --- | --- |
| 0 | The verdict is passed (and, with `record`, the candidate is `TESTED`) |
| 1 | The command could not do its work, or the evidence is not intact |
| 2 | Usage error |
| 3 | The verdict is failed (and, with `record`, the candidate is `REJECTED`) |

`--qemu` defaults to `FLOWERLY_LAB_QEMU`, then `qemu-system-x86_64` from
`PATH`. `--runtime-dir` defaults to `AGENTIC_RUNTIME_DIR`, then the user's
Flowerly runtime directory; `record` talks to `flowerlyd.sock` there.

`run` refuses before booting when the plan is invalid, the plan uses TCG
without `--allow-tcg`, the disk does not hash to the plan's `disk_sha256`
(a disk that was booted once has changed and is refused), or the output
directory exists. It always runs every planned boot, so a failed candidate
boot can be followed by the fallback boot it causes.

## The plan (`flowerly-lab-plan/v1`)

```json
{
  "schema": "flowerly-lab-plan/v1",
  "lane": "reproducibility",
  "hypothesis": "optional; at most 512 bytes",
  "candidate": {
    "generation": "gen-…",
    "disk": "/abs/candidate.img",
    "disk_sha256": "<64 hex>"
  },
  "baseline": "gen-… (optional, differs from the candidate)",
  "firmware": { "code": "/abs/OVMF_CODE.fd", "vars": "/abs/OVMF_VARS.fd" },
  "machine": { "accel": "kvm", "smp": 2, "memory_mib": 2048 },
  "limits": { "boot_seconds": 600, "shutdown_seconds": 120 },
  "expect": {
    "root-b": {
      "generation": "gen-…",
      "partuuid": "<lowercase UUID>",
      "identity_sha256": "<64 hex>",
      "build_id": "lab-…",
      "kernel_release": "6.17.1-arch1-1",
      "vmlinuz_sha256": "<64 hex>",
      "packages_sha256": "<64 hex>",
      "binaries": { "flowerlyd": "<64 hex>" }
    }
  },
  "checks": ["identity", "root", "kernel", "packages", "binaries",
             "health", "integration", "shutdown"],
  "boots": [{ "label": "candidate", "subject": "candidate", "slot": "root-b" }]
}
```

Bounds: at most 64 KiB; unknown keys are refused. Paths are absolute,
UTF-8, without commas or control characters. `smp` 1–16, `memory_mib`
512–65536, both limits 1–3600 seconds. `expect` describes one or two of
`root-a` and `root-b`; `build_id` is 1–64 of `[A-Za-z0-9_.-]`, as flowerlyd
bounds a root identity; `packages_sha256` is required in the reproducibility
lane and optional in the upstream lane. `checks` are distinct and include
`identity`, `root` and `health`. There are 1–8 boots with distinct labels of
`[a-z][a-z0-9-]`; each boot's slot must hold its subject's Generation
(`candidate` or `baseline`), and at least one boot boots the candidate.

## The probe report (`flowerly-lab-report/v1`)

The test-profile probe writes one report to the serial console, in the
Gate D format: base64 of the JSON in numbered lines, then a digest line.

```text
FLOWERLY_LAB_REPORT part=1/3 <base64>
FLOWERLY_LAB_REPORT part=2/3 <base64>
FLOWERLY_LAB_REPORT part=3/3 <base64>
FLOWERLY_LAB_REPORT_DONE parts=3 sha256=<hex of the JSON>
```

Console noise may surround the lines and an identical copy of a part is
accepted. A line counts only once its newline has arrived, because `lab run`
reads the serial log while the guest is still writing it. A part that differs from an earlier copy, a missing part, a second
digest line, more than 256 KiB, bad base64, a digest mismatch or another
schema refuses the report; the boot is stopped at once.

The JSON has `schema`, `facts` and `observations`; unknown keys are refused
and anything the probe could not determine is `null`. The probe does not
judge.

The guest cannot know the host's Generation id: the guest's own flowerlyd has
its own database. What ties a slot to its Generation is the identity
document baked into the slot (`/usr/lib/flowerly/generation-root.json`).
Its hash is `identity_sha256`, and its `build_id` is the one recorded in
that Generation's root identity (`generation.identity`).

| `facts` | |
| --- | --- |
| `slot`, `root_partuuid` | the booted root (`root-a`/`root-b`) and its PARTUUID |
| `identity_sha256`, `build_id` | the baked identity document's hash and its `build_id` |
| `kernel_release`, `vmlinuz_sha256` | the running kernel and its image |
| `packages_sha256`, `package_count` | hash of the sorted `name version` lines |
| `binaries` | Flowerly binary name → SHA-256 |
| `boot_phases_ms` | `firmware`, `loader`, `kernel`, `initrd`, `userspace` |
| `failed_units` | failed systemd units |

| `observations` | |
| --- | --- |
| `health_gate` | `passed`, `failed` or `absent` |
| `flowerlyd_active`, `status_ok` | `flowerlyd` is active and its status answers |
| `admin_roundtrip` | as a member of `flowerly-admin`, the probe created a Generation through `agenticctl`, rejected it as Admin (audited) and read the rejection back |
| `evolutiond_self_test` | `evolutiond --self-test` succeeded |

## The guest probe

The probe is `testbed/evolution-world/lab-probe.py`, run once per boot by
`flowerly-lab-probe.service`. It exists only in a lab candidate image:
`scripts/prepare-lab-probe-rootfs.sh ROOTFS --test-profile` adds it to a
rootfs that `prepare-generation-ab-rootfs.sh` already made an A/B test image,
and refuses any other rootfs, a symlinked destination or an existing
installation. The candidate build (#412) installs it in **both** slots: a
rollback boot falls back to slot A, which must report too.

Facts come from the booted system, not from what the baked identity claims.
The guest mounts a tmpfs over `/var` (`systemd.volatile=state`), which hides
the image's pacman database from `/`. The unit therefore binds the booted root
filesystem alone, without its submounts, read-only at `/run/flowerly-lab/root`,
and the probe reads the file facts (identity, binaries, packages) there:
the same tree that `--offline` reads before the boot.

| Fact | Where the probe reads it |
| --- | --- |
| `slot`, `root_partuuid` | the partition mounted at `/`: its `PARTNAME`, and the one `/dev/disk/by-partuuid` link to it |
| `identity_sha256`, `build_id` | `/usr/lib/flowerly/generation-root.json` (at most 4 KiB) |
| `kernel_release`, `vmlinuz_sha256` | the running kernel's release, and the booted slot's ESP copy `/efi/flowerly/<slot>/vmlinuz` |
| `packages_sha256`, `package_count` | the pacman database; any malformed entry makes both `null` |
| `binaries` | `agenticctl`, `evolutiond`, `flowerlyd`, `flowerly-slot-broker` |
| `boot_phases_ms` | systemd's boot timestamps; `kernel` runs to the initrd (or to userspace without one), `userspace` to the end of startup (or to now) |
| `failed_units` | `systemctl list-units --state=failed`; `(unknown)` when systemd cannot answer, so an unknown state never reads as none failed |

- `health_gate` is `passed` only when `flowerly-generation-health.service`
  ran on this boot and exited 0, and `absent` when the unit does not exist.
- `flowerlyd_active` is `systemctl is-active flowerlyd.service`; `status_ok`
  is `agenticctl status`.
- `admin_roundtrip` runs as the test-only account `flowerly-lab`, a member of
  `flowerly-admin` baked in by sysusers, never the `flowerly` service account:
  `agenticctl generation create`, then `generation reject` with a reason
  (Admin, audited), then `generation inspect` must show the same id
  `REJECTED`. It goes through `flowerlyd`'s authenticated socket, never the
  database, and the Generation exists only in the guest's test database.
- `evolutiond_self_test` is `evolutiond --self-test`, also as `flowerly-lab`.

Every command has a 60-second deadline. The unit runs as root with
`PrivateNetwork=yes`. It is ordered after the health gate and `flowerlyd` but
does not require them, because an unhealthy boot must still report. It powers
the guest off through both `SuccessAction` and `FailureAction`, so QEMU exits
even if the probe crashes; a crash before the report reads as no report.

`lab-probe.py --offline ROOT` prints the file facts (`identity_sha256`,
`build_id`, `packages_sha256`, `package_count`, `binaries`) of an unbooted
root tree, so the candidate build writes its plan with the probe's own
hashing.

`scripts/test-lab-probe.py` tests the probe offline against fake trees and a
fake systemd. It also proves that the probe emits exactly
`testbed/evolution-world/fixtures/lab-report.console`, which the host's own
tests parse and evaluate to every check passing.

## Checks and the decision

The host computes every check for every boot from the report and from what
it saw of the QEMU process. With no usable report every check fails; a KVM
boot whose process was not verified fails every check.

| Check | Passes when |
| --- | --- |
| `identity` | identity hash and `build_id` equal the slot's expectation |
| `root` | the booted slot and PARTUUID are the expected ones |
| `kernel` | kernel release and `vmlinuz` hash are the ones built |
| `packages` | reproducibility lane: the frozen package hash; upstream lane: a hash was reported (recorded, not claimed) |
| `binaries` | every expected binary is present with its hash (an empty expectation fails) |
| `health` | health gate `passed`, `flowerlyd` active, status answered, no failed units |
| `integration` | Admin round trip and `evolutiond --self-test` |
| `shutdown` | QEMU exited 0 on its own after the report, within the shutdown deadline |

The policy (ADR-0024 D3) is fixed: the verdict is **passed** only when every
boot has every planned check and each passed; anything else is **failed**.
A passed verdict recommends `generation.test`, a failed one
`generation.reject`. A baseline boot after a failed candidate boot is recorded
as a `fallback` (`after`, `boot`, `booted`, `healthy`): the rollback scenario.
Failed checks are listed as reasons `label: check failed: detail`.

## The result (`flowerly-lab-result/v1`)

`run` writes the output directory:

| File | |
| --- | --- |
| `plan.json` | the plan exactly as read |
| `ovmf-vars.fd` | the private firmware variable store (not evidence) |
| `N-label.serial.log` | each boot's console |
| `N-label.report.json` | each received report, decoded |
| `result.json` | the result, canonical JSON (sorted keys, 2-space indent, final newline) |
| `result.sha256` | `<sha256>  result.json`: the run's `evidence_sha256` |

`result.json` holds the `evolutiond` version, the plan hash, lane, candidate
and baseline, accelerator, the QEMU binary's path and hash, the firmware
hashes, the disk hash and sizes before the first boot, per boot the host
outcome (exit status, deadline, stop reason, times, verified process),
report state and checks, the metrics, the artifacts and the decision.

Metrics are measurements with units, never scores: per boot
`label.boot_to_report_s`, `label.shutdown_s` and `label.phase.<phase>_ms`,
plus `disk.bytes` and `disk.allocated_bytes`. Artifacts are the plan, every
serial log and every received report, each with `kind`, `name`, `sha256` and
`bytes`.

`verify` refuses a result that is not canonical or does not match
`result.sha256`, whose plan hash or header differs from `plan.json`, whose
reports differ from what its serial logs re-parse to, whose artifact files
differ, or whose checks, metrics or decision differ from a recomputation.

## Recording

`record` (or `run --record`) verifies the directory again, refuses a TCG
result, and then, through `flowerlyd` as the invoking Admin:

1. `generation.evidence` with `outcome` (`passed`/`failed`), the
   `evidence_sha256`, and the `experiment` object: `hypothesis` (the plan's,
   or a default), `baseline`, `test_plan` (the check names), `metrics`,
   `artifacts` and `lane` ([Experiment records](generation-ipc-v1-arguments.md#experiment-records-adr-0024-d5-006));
2. `generation.test` or `generation.reject` with the returned Experiment.

The candidate Generation must already exist with its identity; creating it
and setting its identity belongs to the candidate build (#412), which also
writes the plan, so the plan's `build_id` is the one the Generation's root
identity records. `generation.inspect` does not return the root identity, so
`record` cannot compare them itself. Recording the
same directory again returns the same Experiment from `generation.evidence`;
the second state change is refused by flowerlyd.
