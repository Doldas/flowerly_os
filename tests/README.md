# tests

Repository-level test suites beyond per-crate unit tests.

- `unit/` — cross-crate unit tests
- `integration/` — services started and driven over IPC (health/status contracts)
- `boot/` — boot tests against produced images (planned)
- `upgrade/` — upgrade/rollback tests across generations (planned)
- `generation/` — candidate generation build and boot (planned)
- `evolution/` — Evolution World experiment harness (planned)

At present the daemons carry `--self-test` and per-service integration tests
under `services/*/tests/`; run everything with `just test`.