





set dotenv-load

build_dir := env_var_or_default("CARGO_TARGET_DIR", "target")
bin_dir := build_dir + "/release"


fmt:
    cargo fmt --all


fmt-check:
    cargo fmt --all -- --check


lint: fmt-check
    cargo clippy --workspace --all-targets -- -D warnings


check:
    cargo check --workspace --all-targets


check-tests:
    cargo check --workspace --all-targets --tests


build:
    cargo build --workspace --release


test:
    cargo test --workspace


verify: fmt-check lint test
    cargo build --workspace --release


serve: build
    ./scripts/dev-runtime.sh start


servestop:
    ./scripts/dev-runtime.sh stop


run: serve


stop: servestop


status:
    ./scripts/dev-runtime.sh status


release: build
    @echo "release build complete"
    @echo "next steps: bump version, update CHANGELOG.md, tag v<version>, create GitHub release"


image-inputs:
    bash ./scripts/check-image-inputs.sh
    cargo run -p distro-builder -- validate


image: image-inputs
    @echo "Arch image inputs are ready, but bootable image production is not implemented yet."
    @echo "See distro/base-image/README.md and web-docs/docs/image-model.md."
    exit 1


vm:
    @echo "vm target is a placeholder — not implemented yet"
    exit 1


vm-test:
    @echo "vm-test target is a placeholder — not implemented yet"
    exit 1