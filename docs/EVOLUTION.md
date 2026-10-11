# Flowerly OS — Evolution World

The Evolution World is an isolated, high-autonomy sandbox in which candidate
system generations are built, booted and tested. It runs on QEMU/KVM, separate
from the Stable World the user depends on, so aggressive or destructive
experiments cannot harm the real environment. Evolution is evidence-driven:
failed experiments are kept as useful history rather than discarded.

See [`../adr/0007-kvm-evolution-world.md`](../adr/0007-kvm-evolution-world.md).

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

One command, `just vm-test`, is intended to build a candidate, boot it, verify a
service, exit, and record success or failure. This workflow is **planned and not
implemented**. `evolutiond` currently ships as a stub that only exposes a health
contract (`evolution.experiment-status` returns `idle`).

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
generations and goals. See [`PROVENANCE.md`](PROVENANCE.md).

## Implementation status and requirements

- **Implemented:** `evolutiond` stub; the `Generation` and experiment storage
  models that the pipeline will populate.
- **Planned:** the KVM boot harness, `just vm-test`, health and integration test
  suites in `evolution/`, benchmarks, multidimensional fitness evaluation, and
  promotion/rejection policy.
- **Environment requirement:** KVM-capable hardware and QEMU. QEMU is not yet
  installed in the current development environment.
