# scripts

Developer and repository-maintenance scripts.

- `dev-runtime.sh` — start/stop/status for the local daemon runtime (`just serve`)
- `check-docs.sh` — required-documentation checker for CI
- `bootstrap-github.sh` — deterministic GitHub project setup (labels,
  milestones, issues, project board); created during repository bootstrap

Scripts must be inspectable one-liners wired to real commands — never opaque
build wrappers hiding behaviour the `justfile` should expose.