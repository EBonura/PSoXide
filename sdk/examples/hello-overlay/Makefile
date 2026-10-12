ROOT := $(abspath ../../..)
OUT ?= $(ROOT)/build/hello-overlay
TARGET := mipsel-sony-psx
TOOLS := $(OUT)/target/debug
GUEST := $(OUT)/target/$(TARGET)/release/hello-overlay.exe

.PHONY: all
all:
	cargo build --manifest-path "$(ROOT)/Cargo.toml" --locked -p psoxide-hazard --bins --target-dir "$(OUT)/target"
	cargo build --locked --release --target $(TARGET) -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem --target-dir "$(OUT)/target" --config 'target.$(TARGET).rustflags=["-Clink-arg=-T$(ROOT)/sdk/psoxide.ld","-Clink-arg=-T$(ROOT)/sdk/examples/hello-overlay/overlay.ld","-Clink-arg=--emit-relocs","-Clink-arg=-Map=$(OUT)/sample.map"]'
	"$(TOOLS)/overlay-example-pack" "$(GUEST)" "$(OUT)/hello-overlay.exe"
	"$(TOOLS)/hazard-scan" "$(OUT)/hello-overlay.exe.guard" --map "$(OUT)/sample.map" $$(cat "$(OUT)/hello-overlay.exe.module-args") $$(cat "$(OUT)/hello-overlay.exe.code-args")
	"$(TOOLS)/stack-guard" "$(OUT)/hello-overlay.exe.guard" "$(OUT)/sample.map" $$(cat "$(OUT)/hello-overlay.exe.module-args") --root overlay_typed_call --budget 32768 --forbid __psx_rt_flush_i_cache
	cargo run --manifest-path "$(ROOT)/Cargo.toml" --locked -p mkisopsx --target-dir "$(OUT)/target" -- --exe "$(OUT)/hello-overlay.exe" --out "$(OUT)/hello-overlay.bin" --volume PSOXIDESDK
