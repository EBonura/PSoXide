ROOT := $(CURDIR)
TARGET := mipsel-sony-psx
BUILD ?= $(ROOT)/build/examples
EXAMPLE ?= hello-tri
FRONTEND ?=

.PHONY: example disc hello-tri hello-tri-disc run-tri examples
examples:
	@set -e; for example in hello-tri hello-input hello-ot hello-gte hello-tex hello-memcard; do $(MAKE) -f tools/sdk-examples.mk disc EXAMPLE=$$example; done

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
