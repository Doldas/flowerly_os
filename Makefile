# Flowerly OS — developer command interface.
#
# `just` is the primary interface (see justfile); these Make targets mirror the
# same commands for environments without `just`.

CARGO ?= cargo
TARGET ?= target

.PHONY: fmt fmt-check lint check build test verify run serve stop status release image vm vm-test

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

lint: fmt-check
	$(CARGO) clippy --workspace --all-targets -- -D warnings

check:
	$(CARGO) check --workspace --all-targets

build:
	$(CARGO) build --workspace --release

test:
	$(CARGO) test --workspace

verify: lint check
	$(CARGO) test --workspace
	$(CARGO) build --workspace --release

run: serve

serve: build
	./scripts/dev-runtime.sh start

stop:
	./scripts/dev-runtime.sh stop

status:
	./scripts/dev-runtime.sh status

release: build
	@echo "release build complete; see docs/RELEASES.md for tagging"

image:
	@echo "image target is a placeholder — not implemented yet"
	@exit 1

vm:
	@echo "vm target is a placeholder — not implemented yet"
	@exit 1

vm-test:
	@echo "vm-test target is a placeholder — not implemented yet"
	@exit 1