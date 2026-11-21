# Flowerly OS — Evolution World

The Evolution World is an isolated, high-autonomy sandbox in which candidate
system generations are built, booted and tested. It runs on QEMU/KVM, separate
from the Stable World the user depends on, so aggressive or destructive
experiments cannot harm the real environment. Evolution is evidence-driven:
failed experiments are kept as useful history rather than discarded.

See [ADR-0007](adr/0007-kvm-evolution-world.md).

## Pipeline

```text
source change
   │
   ▼
build candidate  ──►  candidate generation manifest (GenerationId, source_commit)
   │
   ▼
KVM boot (QEMU, Evolution World)
   │
   ▼
health check      ──►  required services answer structured status
   │
   ▼
integration test  ──►  boot target, storage, networking, agentic services
   │
   ▼
shutdown
   │
   ▼
artifacts         ──►  test report, benchmark data, logs (ArtifactId)
   │
   ▼
experiment result ──►  Experiment record with metrics
   │
   ▼
promotion decision ──►  promote to STABLE or reject
```

One command, `just vm-test`, builds a Flowerly lab candidate, boots it under
QEMU/KVM, verifies what the guest reports, and records success or failure in a
private `flowerlyd` ([Evolution Lab](evolution-lab.md)). It is implemented
(0.0.6, #412); real KVM acceptance of the lab (#413) is still to come.

A precursor is now implemented as the nightly Arch-kernel system test. It boots
the official Arch Linux cloud image under QEMU/KVM, performs a full
`pacman -Syu`, reboots into the installed Arch kernel, and runs the complete
Flowerly build/test/runtime smoke suite inside that guest. It validates the real
rolling substrate while the Flowerly generation image builder is being built;
it does not yet claim to boot a Flowerly generation.

The `evolutiond` daemon remains a stub that exposes a health contract
(`evolution.experiment-status` returns `idle`). The Evolution Lab
(`evolutiond lab`, 0.0.6) is a foreground command that boots a candidate disk
under QEMU/KVM, checks the guest's report, and records the candidate as
`TESTED` or `REJECTED`; it never promotes or activates. See
[evolution-lab.md](evolution-lab.md).

## Experiment record

An experiment ties a hypothesis to a candidate generation, a baseline generation,
a test plan, metrics, a result and a decision. It is stored durably and links back
to the motivating goal.

## Provenance requirements

A test result must be traceable back through the pipeline. Evolution must record
provenance so any promoted change answers: which source commit, which generation,
which experiment, which build, which VM run, and which test output produced the
decision. Concretely, `provenanced` records edges such as `produced`,
`tested_by`, `caused_by` and `promoted_to` connecting experiments, artifacts,
generations and goals. See [provenance.md](provenance.md).

## Implementation status and requirements

- **Implemented:** `evolutiond` stub; the `Generation` and experiment storage
  models that the pipeline will populate; `evolutiond lab` (QEMU boots, checks,
  benchmarks, the fail-closed `TESTED`/`REJECTED` policy and verifiable results),
  tested locally without a real KVM run yet.
- **Implemented precursor:** nightly/manual official-Arch VM harness with
  KVM-preferred/QEMU-TCG fallback, rolling upgrade + reboot, whole-project
  verification, daemon smoke test and retained evidence artifacts.
- **Implemented, not yet KVM-verified:** the in-guest lab probe, the
  Flowerly lab candidate builder and `just vm-test` (0.0.6, #411, #412).
- **Planned:** real KVM acceptance of the lab (#413) and multidimensional
  fitness evaluation.
- **Environment requirement:** the heavyweight nightly test uses the
  `flowerly-runner` with QEMU; KVM is preferred when available and TCG is a
  correctness fallback.
