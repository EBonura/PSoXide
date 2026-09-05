ROOT := $(CURDIR)
TARGET := mipsel-sony-psx
BUILD ?= $(ROOT)/build/examples
EXAMPLE ?= hello-tri
FRONTEND ?=

.PHONY: help check test fmt lint example disc hello-tri hello-tri-disc run-tri
help:
	@echo "make check | test | lint | hello-tri-disc"
	@echo "make disc EXAMPLE=hello-input; make run-tri FRONTEND=/path/to/frontend"
check:
	cargo check --locked --workspace --all-features
	cargo check --locked --manifest-path sdk/Cargo.toml --workspace --all-features
test:
	python3 -m unittest discover -s tools -p test_bootstrap_components.py
	cargo test --locked --workspace
	cargo test --locked --manifest-path sdk/Cargo.toml --workspace
fmt:
	cargo fmt --all
	cargo fmt --manifest-path sdk/Cargo.toml --all
lint:
	python3 tools/check_mfc0.py sdk
	cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
	cargo clippy --locked --manifest-path sdk/Cargo.toml --workspace --all-targets --all-features -- -D warnings
example:
	@test -f "sdk/examples/$(EXAMPLE)/Cargo.toml"
	cd sdk/examples/$(EXAMPLE) && CARGO_TARGET_DIR="$(BUILD)" RUSTFLAGS="-Cllvm-args=-disable-mips-df-backward-search -Clink-arg=-T../../psoxide.ld -Clink-arg=--oformat=binary" cargo build --release --target $(TARGET) -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem
	python3 tools/hazard_scan.py "$(BUILD)/$(TARGET)/release/$(EXAMPLE).exe"
disc: example
	cargo run --locked --release -p mkisopsx -- --exe "$(BUILD)/$(TARGET)/release/$(EXAMPLE).exe" --out "$(BUILD)/$(TARGET)/release/$(EXAMPLE).bin" --volume PSOXIDESDK
hello-tri:
	$(MAKE) example EXAMPLE=hello-tri
hello-tri-disc:
	$(MAKE) disc EXAMPLE=hello-tri
run-tri: hello-tri-disc
	@test -n "$(FRONTEND)" || (echo "Set FRONTEND to the PSoXide-emulator executable"; exit 1)
	"$(FRONTEND)" launch --path "$(BUILD)/$(TARGET)/release/hello-tri.cue"
