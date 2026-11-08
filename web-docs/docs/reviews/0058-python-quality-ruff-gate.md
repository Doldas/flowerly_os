# Python quality gate: Ruff and syntax review

Status: **proposed, implemented on a draft PR; not active remotely until merged and the protected release ruleset is explicitly applied by an authorized maintainer.**

The repository already runs Rust `fmt-check`, Clippy and tests. Python tests exercise behavior, but tests alone do not flag unused imports, undefined names or newly introduced style problems. A dedicated `python-quality` CI job now performs:

1. **Syntax**: parse *every tracked Python source file* with `ast.parse`. A syntax error even in unchanged code fails.
2. **Lint**: run pinned `ruff==0.17.4` on every added, copied, modified or renamed Python file, using the default `E4,E7,E9,F` error/pyflakes rules. A regression cannot be hidden because the current file was previously exempt.
3. **Formatting**: `ruff format --check` on *newly added* Python files only. Existing files have legacy formatting and need a deliberate, separate baseline migration.
4. **Enforcement tests**: the offline regression verifies the correct diff selection (including spaces in file names), syntax rejection of an untouched file, missing base-SHA rejection, error propagation and all-files lint mode.

The checker requires a full Git checkout and an exact base reference. For pull requests, CI diffs the GitHub-provided base SHA against the checked-out merge commit. For release and master pushes it uses `github.event.before`. For a newly created push whose before SHA is all zeroes, it lint-checks all files. Unknown or missing references fail closed. The CI job runs on GitHub-hosted Ubuntu for both PRs and pushes, without `continue-on-error`.

Local commands:

```bash
python3 scripts/test-python-quality.py
python3 -m pip install ruff==0.17.4
python3 scripts/check-python-quality.py --base "$(git merge-base HEAD release/2)"
python3 scripts/check-python-quality.py --all
```

`bash scripts/preflight-docs.sh` also runs the gate's offline regression controls. Normal full Python unit/integration test execution remains in the existing Docs and Rust jobs.

## Release ruleset and owner action

The checked-in `.github/release-gates.json` and `scripts/release-gates.py` now require the status context **`python-quality`**, in addition to the prior Rust, Docs, security and CodeQL checks. Merely merging this change does **not** update GitHub's remote branch ruleset; the authorized repository admin must apply it using the existing authenticated `release-gates.py` procedure, verify actual enforcement, and review failed exact-head checks before merges. No release/master promotion is authorized by the new job.

## Follow-up: whole-repository formatting baseline

An explicit maintenance PR can run `ruff format` on the existing Python tree after reviewing any functional changes. After that migration, the formatting gate can be expanded from added files to all modified files. Stronger security/type rules (for example `B`, `UP`, `S`, optional mypy or pyright) should be enabled only after a measured error inventory, not by silently suppressing diagnostics.
