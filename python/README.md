# Python

Python 3.14+ is the specified language for *research, prototyping,
experiments, analysis and tooling* (see `adr/0003-python-for-experimentation.md`
and `docs/LANGUAGE_POLICY.md`). It is intentionally excluded from the core
runtime.

Subdirectories:

- `experiments/` — reproducible experiment harnesses and notebooks
- `analysis/` — log/telemetry analysis and visualization
- `evaluation/` — offline evaluation of agent behaviour and fitness
- `tools/` — developer utilities

Tooling standard: `ruff` for linting and `pytest` for tests, wired into CI
only when Python code lands.