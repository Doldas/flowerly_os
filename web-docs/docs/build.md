# Flowerly OS — Building

How to build, check, test and run Flowerly OS from source, and how CI runs the
same targets.

## Prerequisites

- **Rust:** the toolchain is pinned in `rust-toolchain.toml` (`stable`, with
  `rustfmt` and `clippy`); a `rustup` install picks it up automatically. The
  minimum supported Rust version is set intentionally in `Cargo.toml`.
- **just:** the primary developer command runner (https://github.com/casey/just).
- **Git** and build essentials; a C compiler is needed for the bundled SQLite.
- **Optional:** Go and Python 3 for future components. QEMU/KVM plus OVMF and
  cloud-image tooling are only required to run the heavyweight Arch-kernel
  system test; they are not required for the normal Rust workspace.

## Developer commands

The canonical interface is `just`:

```text
just build    # build the workspace (debug)
just check    # cargo check across the workspace
just test     # run the test suite
just lint     # cargo fmt --check and cargo clippy
just run      # run a service (see the justfile for arguments)
```

Underlying them are ordinary Cargo invocations (`cargo build/check/test --workspace`,
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- --deny warnings`);
`just` does not hide build behavior behind opaque scripts.

## Makefile fallback

Where `just` is not available, a `Makefile` mirrors these targets (`make build`,
`make check`, `make test`, `make lint`, `make run`), invoking the same underlying
commands. `just` remains primary; the `Makefile` only keeps the project buildable
without extra tooling.

Services are foreground processes, so `cargo run -p flowd` or
`cargo run -p agenticctl -- status` works directly. They create Unix sockets
under `$XDG_RUNTIME_DIR/flowerly` and persist structured state in one local
SQLite `state.db` under the XDG state directory (normally
`$XDG_STATE_HOME/flowerly` or `$HOME/.local/state/flowerly`). All of this works
without a network or an AI provider.

Useful overrides:

```text
AGENTIC_RUNTIME_DIR   ephemeral sockets/runtime files
AGENTIC_STATE_DIR     durable Flowerly state directory
AGENTIC_DATABASE_PATH explicit database path for tests/recovery
```

`scripts/dev-runtime.sh` deliberately keeps its durable development state under
`target/flowerly-state` while its sockets/logs live in a separate runtime
directory.

## Arch image inputs

The initial distribution substrate is Arch Linux. The current repository does
not yet produce the 0.0.9 bootable image, but the image inputs are concrete
under `image/arch/`.

Validate them with:

```text
just image-inputs
```

This checks that the selected Arch packages and expected Flowerly binaries are
sorted, unique and consistent with the initial mkosi rootfs configuration.

`just image` deliberately still fails after validation because a successful
command named `image` must eventually produce a real image artifact. The
project will not report the scaffold as a completed bootable image.

The current mkosi prototype uses Arch Linux and a directory/rootfs output. A
later milestone adds Flowerly package installation, bootable disk output,
initramfs/UKI handling, exact resolved package manifests and KVM boot tests. See
[image-model.md](image-model.md).

## CI

GitHub Actions runs the same targets from a clean checkout, keeping local and CI
behavior aligned:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- --deny warnings
cargo test --workspace
cargo build --workspace --release
```

Go and Python checks (`go test`/`go vet`, `ruff`/`pytest`) run only when such code
is present. Documentation checks validate required docs and relative links;
security checks run `cargo audit` and `cargo deny`. A red CI cannot be
released; see [releases.md](releases.md).

### Nightly real-Arch kernel validation

`.github/workflows/nightly-arch-kernel.yml` is the heavyweight system check.
Once the workflow exists on the default branch, it runs every night at
02:17 UTC and selects:

- `master`;
- the highest-numbered `release/N` branch.

For each target it computes a fingerprint from Rust source, build tooling,
image inputs and system-test code. If that exact fingerprint has already passed
the Arch-kernel test, the VM work is skipped. Documentation-only commits
therefore do not cause another full VM build.

When work is required, the workflow uses the persistent `flowerly-runner` to:

```text
official Arch cloud image
        ↓ SHA256 verification
QEMU/KVM (TCG fallback)
        ↓
pacman -Syu
        ↓
reboot into installed Arch kernel
        ↓
copy exact checked-out Flowerly tree
        ↓
fmt + clippy + check + tests + release build
        ↓
docs/image validation
        ↓
start all Flowerly daemons and query them with agenticctl
```

The official base image is cached under the runner user's
`~/.cache/flowerly/arch-images`, keyed by its published checksum. Per-run
evidence contains the tested git commit, image checksum, running kernel/package
information, QEMU accelerator, guest report and serial log.

#### Arch VM root disk capacity

The official upstream cloud image has a small default virtual root disk. The
release VM script creates a **sparse 32 GiB QCOW2 overlay** by default, without
altering the SHA256-verified cached base image. To choose a larger disposable
guest disk, set `ARCH_VM_DISK_GB` to a whole number from **24 to 256**:

```sh
ARCH_VM_DISK_GB=48 bash scripts/arch-kernel-vm-test.sh
```

Expanding the virtual QCOW2 capacity is not sufficient on its own. Cloud-init
is configured to grow the root partition and resize the root filesystem at
first boot. **Before `pacman -Syu`**, the harness checks that `/` has at
least **16 GiB total** and **8 GiB free**, and writes evidence to
`report/guest-disk-before-upgrade.txt`. If the expansion fails, it prints
partition and filesystem diagnostics instead of attempting an upgrade that
cannot fit. On Arch, the cloud-init growpart module requires the optional
`cloud-guest-utils` package in the guest image.

Use `bash scripts/test-arch-vm-disk.sh` to validate configuration bounds,
the QEMU resize command and guest preflight logic without downloading or
booting a VM. The 32 GiB limit refers to **virtual disk capacity**, not
space immediately reserved on the host; actual QCOW2 host usage grows as
the guest writes data. A real Arch boot test on the trusted release runner is
still required for end-to-end confirmation.

The workflow also supports `workflow_dispatch` with `force=true`, giving a
manual final-validation path before tagging even if the same fingerprint passed
overnight.

Tagged releases go one step further: `.github/workflows/release.yml` performs a
fresh Arch-kernel VM run for the exact tag and will not publish the GitHub
Release unless that guest validation passes.

The heavyweight VM test is deliberately not a per-PR gate: it performs a full
rolling Arch upgrade, reboot and build inside a VM. Normal CI remains the fast
pull-request gate.

### Documentation Pages publication

Documentation builds run on both `master` and `release/**` when docs inputs
change. Release branches therefore validate the MkDocs site before promotion,
but they do **not** publish the production GitHub Pages site.

Only `master` may execute the `github-pages` deployment job. Manual
`workflow_dispatch` runs follow the same rule: a non-master ref can build and
validate documentation, while production publication is skipped.

This matches the release model and GitHub environment protection: unreleased
documentation is tested on the active release branch and becomes public only
after that release line is promoted to `master`.

### Runner caching and stale-run cancellation

CI is optimized for both GitHub-hosted and self-hosted runners:

- Rust CI caches Cargo registry/git data plus `target/`, keyed by the runner
  platform, toolchain file and `Cargo.lock`.
- CodeQL uses `github/codeql-action@v4` with Rust `build-mode: none`, so PR
  analysis does not perform a second full Cargo build. It still caches Cargo
  registry/git downloads for dependency metadata. `target/` is deliberately
  not part of the CodeQL cache because no compiled build output is required by
  this analysis mode.
- Security jobs reuse Cargo registry/git metadata.
- GitHub Pages uses the built-in pip cache keyed by
  `requirements-docs.txt`.
- PR CI, CodeQL, security and PR-validation use concurrency groups with
  `cancel-in-progress: true`. A new commit therefore cancels stale work for
  the same PR instead of allowing obsolete jobs to accumulate in the runner
  queue.
- CodeQL and dependency security workflows skip documentation-only changes.
  Scheduled security analysis still runs independently.
- CodeQL disables database artifact upload (`upload-database: false`) while
  still publishing code-scanning results, avoiding an unnecessary large
  database upload on every PR run.

`FLOWERLY_CACHE_VERSION` in the workflows is the manual cache-busting knob.
Increment it when cache layout or semantics change.

## Current status

The Rust workspace builds and tests the shared crates and services. `justfile`,
`Makefile`, systemd units and image/kernel work arrive with the repository
foundation and later sprints; the commands above are the intended interface.
