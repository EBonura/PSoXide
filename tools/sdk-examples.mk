ROOT := $(CURDIR)
TARGET := mipsel-sony-psx
BUILD ?= $(ROOT)/build/examples
EXAMPLE ?= hello-tri
FRONTEND ?=
# LLVM's MIPS delay-slot filler searches backwards only by default. Searching
# the successor block and past calls as well fills most of the remaining
# slots: hl-psx measured 7% of executed instructions as delay-slot nops, and
# +2.3% rendered FPS with 10.9 KB less .text from these two switches. Every
# search can leave a load in a slot whose consumer runs inside the load delay,
# so the link is always followed by tools/hazard_patch.py, which reroutes
# those branches through psx-rt's HAZARD_TRAMPOLINES and rescans.
PSX_DELAY_SLOT_FLAGS := -Cllvm-args=-disable-mips-df-succbb-search=false -Cllvm-args=-disable-mips-df-forward-search=false

.PHONY: example disc hello-tri hello-tri-disc run-tri examples
examples:
	@set -e; for example in hello-tri hello-input hello-ot hello-gte hello-tex hello-memcard; do $(MAKE) -f tools/sdk-examples.mk disc EXAMPLE=$$example; done

example:
	@test -f "sdk/examples/$(EXAMPLE)/Cargo.toml"
	cd sdk/examples/$(EXAMPLE) && CARGO_TARGET_DIR="$(BUILD)" RUSTFLAGS="$(PSX_DELAY_SLOT_FLAGS) -Clink-arg=-T../../psoxide.ld -Clink-arg=--oformat=binary" cargo build --release --target $(TARGET) -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem
	python3 tools/hazard_patch.py "$(BUILD)/$(TARGET)/release/$(EXAMPLE).exe"
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
