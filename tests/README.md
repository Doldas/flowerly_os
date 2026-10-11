# tests

Repository-level test suites beyond per-crate unit tests.

Present today:

- per-service integration tests under `services/*/tests/`, run by `just test`;
- `arch-kernel/`, a heavyweight system test that boots the official Arch Linux
  cloud image, performs a full rolling upgrade, reboots into the installed Arch
  kernel, and runs the complete Flowerly verification inside that VM.

The Arch-kernel suite is driven by
`.github/workflows/nightly-arch-kernel.yml`. It is deliberately nightly/manual
rather than per-PR because it downloads/boots a VM and compiles the whole
workspace inside the guest.

Planned suites additionally cover Flowerly generation-image boot, upgrade,
rollback and full Evolution World experiments.
