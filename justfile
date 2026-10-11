# Flowerly OS — developer command runner.
#
# `just` is the primary interface. The Makefile mirrors the same targets for
# environments without `just`. Targets `image`, `vm` and `vm-test` are explicit
# placeholders: they fail loudly until they are implemented.

set dotenv-load

build_dir := env_var_or_default("CARGO_TARGET_DIR", "target")
bin_dir := build_dir + "/release"

# Lint-format the whole workspace.
fmt:
    cargo fmt --all

# Check formatting is clean.
fmt-check:
    cargo fmt --all -- --check

# Static analysis with clippy (warnings are errors).
lint: fmt-check
    cargo clippy --workspace --all-targets -- -D warnings

# Fast compile check of all targets.
check:
    cargo check --workspace --all-targets

# Fast compile check including tests.
check-tests:
    cargo check --workspace --all-targets --tests

# Build everything in release mode.
build:
    cargo build --workspace --release

# Run every test in the workspace.
test:
    cargo test --workspace

# Format, lint, check and run the full test suite.
verify: fmt-check lint test
    cargo build --workspace --release

# Start the local development runtime (all daemons) in release mode.
serve: build
    ./scripts/dev-runtime.sh start

# Stop the local development runtime.
servestop:
    ./scripts/dev-runtime.sh stop

# Alias for `serve`.
run: serve

# Alias for `servestop`.
stop: servestop

# Report the status of every running service.
status:
    ./scripts/dev-runtime.sh status

# Release build plus packaging guidance. See docs/RELEASES.md.
release: build
    @echo "release build complete"
    @echo "next steps: bump version, update CHANGELOG.md, tag v<version>, create GitHub release"

# [not implemented] build a distribution image (image/).
image:
    @echo "image target is a placeholder — not implemented yet"
    exit 1

# [not implemented] boot the current tree in the Evolution World (QEMU/KVM).
vm:
    @echo "vm target is a placeholder — not implemented yet"
    exit 1

# [not implemented] full Evolution World test cycle (build -> boot -> test -> measure).
vm-test:
    @echo "vm-test target is a placeholder — not implemented yet"
    exit 1