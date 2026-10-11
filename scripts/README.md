# scripts

Developer and repository-maintenance scripts.

- `dev-runtime.sh` — start/stop/status for the local daemon runtime (`just serve`)
- `check-docs.sh` — required-documentation checker for CI
- `check-image-inputs.sh` — validate the declarative Arch image/package inputs
- `arch-kernel-vm-test.sh` — boot/update/reboot an official Arch VM and run the
  whole-project guest verification used by the nightly system test
- `bootstrap-github.sh` — deterministic GitHub project setup (labels,
  milestones, issues, project board); created during repository bootstrap

Scripts must be inspectable one-liners wired to real commands — never opaque
build wrappers hiding behaviour the `justfile` should expose.