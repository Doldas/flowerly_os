# Flowerly OS — Building

How to build, check, test and run Flowerly OS from source, and how CI runs the
same targets.

## Prerequisites

- **Rust:** the toolchain is pinned in `rust-toolchain.toml` (`stable`, with
  `rustfmt` and `clippy`); a `rustup` install picks it up automatically. The
  minimum supported Rust version is set intentionally in `Cargo.toml`.
- **just:** the primary developer command runner (https://github.com/casey/just).
- **Git** and build essentials; a C compiler is needed for the bundled SQLite.
- **Optional, for later work:** Go, Python 3 and QEMU/KVM. None are required to
  build or test the current Rust workspace.

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

### Runner caching and stale-run cancellation

CI is optimized for both GitHub-hosted and self-hosted runners:

- Rust CI caches Cargo registry/git data plus `target/`, keyed by the runner
  platform, toolchain file and `Cargo.lock`.
- CodeQL caches Cargo registry/git downloads but deliberately **does not cache
  `target/`**. CodeQL must observe the instrumented build; reusing compiled
  target artifacts can make analysis incomplete.
- Security jobs reuse Cargo registry/git metadata.
- GitHub Pages uses the built-in pip cache keyed by
  `requirements-docs.txt`.
- PR CI, CodeQL, security and PR-validation use concurrency groups with
  `cancel-in-progress: true`. A new commit therefore cancels stale work for
  the same PR instead of allowing obsolete jobs to accumulate in the runner
  queue.
- CodeQL and dependency security workflows skip documentation-only changes.
  Scheduled security analysis still runs independently.

`FLOWERLY_CACHE_VERSION` in the workflows is the manual cache-busting knob.
Increment it when cache layout or semantics change.

## Current status

The Rust workspace builds and tests the shared crates and services. `justfile`,
`Makefile`, systemd units and image/kernel work arrive with the repository
foundation and later sprints; the commands above are the intended interface.
