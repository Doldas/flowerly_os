# 0003: Python for Experimentation and Tooling

Date: 2026-10-11
Status: Accepted

## Context

The evolution loop depends on experiments being cheap to write, run, measure and
discard. Evaluation harnesses, benchmarks, log and data analysis, migrations and
prototype agents all change rapidly and benefit from fast iteration and a rich
scientific and AI ecosystem. None of these belong on the trusted daemon surface.

## Decision

Python is the language for research, experiments, analysis, evaluation,
migration helpers and one-off tooling, under `python/`. Experiments must be easy
to write and run, so Python is the default there. A successful Python prototype
that becomes permanent infrastructure is reimplemented in Rust or Go once its
stable architecture is understood. Python is never used for the core runtime,
privileged operations, or security-critical paths.

## Consequences

- Fast iteration and access to strong ML/AI and data tooling.
- Experiments stay isolated from the trusted, long-lived daemon surface.
- Translation cost exists when a prototype graduates to Rust or Go.
- Dependency and reproducibility hygiene matters for experiments too; CI runs
  `ruff` and `pytest` only when Python code is present.
- The full policy is documented in `../docs/LANGUAGE_POLICY.md`.
