# scripts

Developer and repository-maintenance scripts.

- `dev-runtime.sh` — start/stop/status for the local daemon runtime (`just serve`)
- `check-docs.sh` — required-documentation checker for CI
- `check-image-inputs.sh` — validate the declarative Arch image/package inputs
- `arch-kernel-vm-test.sh` — boot/update/reboot an official Arch VM and run the
  whole-project guest verification used by the nightly system test
- `check-daemon-self-tests.sh` — execute every daemon's advertised
  `--self-test` against built binaries
- `check-cli-contracts.sh` — exercise real CLI-to-daemon contracts that unit
  tests cannot catch, starting with Goal creation defaults
- `bootstrap-github.sh` — deterministic GitHub project setup (labels,
  milestones, issues, project board); created during repository bootstrap

Scripts must be inspectable one-liners wired to real commands — never opaque
build wrappers hiding behaviour the `justfile` should expose.