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
# so the link is always followed by hazard-patch (tools/psoxide-hazard), which
# reroutes those branches through psx-rt's HAZARD_TRAMPOLINES and rescans. The
# PGO driver runs it, hazard-scan and stack-guard in-process with the link
# map it has the link write, so every jump table is proven from the map and
# every scratchpad stack call tree is proven to fit its region.
PSX_DELAY_SLOT_FLAGS := "-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"
# The example's cargo invocation. Flags go in through --config rather than
# RUSTFLAGS so the PGO driver can append its own (RUSTFLAGS would replace them).
EXAMPLE_CARGO = build --release --target $(TARGET) -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
	--target-dir "$(BUILD)" \
	--config 'target.$(TARGET).rustflags=[$(PSX_DELAY_SLOT_FLAGS),"-Clink-arg=-T../../psoxide.ld","-Clink-arg=--oformat=binary"]'
# Profile-guided builds (tools/psoxide-pgo/README.md). `example` applies
# PGO_PROFILE when it exists, as variant PGO_VARIANT, and builds plainly
# otherwise; either way the driver runs the patcher, scanner and stack guard.
PGO = cargo run -q --release --locked -p psoxide-pgo --
PGO_PROFILE ?= sdk/examples/$(EXAMPLE)/pgo.prof
PGO_VARIANT ?= default
# Extra collect arguments, placed after the tape: `--polls FROM..TO` keeps only
# gameplay samples from it, `--launch-arg ARG` passes ARG to the frontend.
PGO_ARGS ?=
PGO_VARIANTS ?= --variant off --variant default --variant hot=500 --variant hot=500+profi
GATE ?=

.PHONY: example disc hello-tri hello-tri-disc run-tri examples pgo-collect pgo-choose hello-xa-disc hello-xa-gate hello-cdstream-disc hello-cdstream-gate
examples:
	@set -e; for example in hello-tri hello-input hello-ot hello-gte hello-tex hello-memcard hello-spstack hello-gteirq hello-present hello-present-queue hello-asmprobe; do $(MAKE) -f tools/sdk-examples.mk disc EXAMPLE=$$example; done

example:
	@test -f "sdk/examples/$(EXAMPLE)/Cargo.toml"
	$(PGO) apply --crate "sdk/examples/$(EXAMPLE)" --profile "$(PGO_PROFILE)" \
		--variant "$$(test -f "$(PGO_PROFILE)" && echo "$(PGO_VARIANT)" || echo off)" -- $(EXAMPLE_CARGO)
disc: example
	cargo run --locked --release -p mkisopsx -- --exe "$(BUILD)/$(TARGET)/release/$(EXAMPLE).exe" --out "$(BUILD)/$(TARGET)/release/$(EXAMPLE).bin" --volume PSOXIDESDK
# make pgo-collect EXAMPLE=x TAPE=route.pxtape FRONTEND=frontend [PGO_ARGS="--polls 100..1400"]
pgo-collect:
	@test -n "$(FRONTEND)" -a -n "$(TAPE)" || (echo "Set FRONTEND and TAPE"; exit 1)
	$(PGO) collect --crate "sdk/examples/$(EXAMPLE)" --frontend "$(FRONTEND)" --tape "$(TAPE)" $(PGO_ARGS) \
		--out "$(PGO_PROFILE)" -- $(EXAMPLE_CARGO)
# make pgo-choose EXAMPLE=x GATE='script printing key=value lines for $$PSOXIDE_PGO_IMAGE'
# (`$$PSOXIDE_PGO measure` prints gameplay-window totals for one tape)
pgo-choose:
	@test -n '$(GATE)' || (echo "Set GATE"; exit 1)
	$(PGO) choose --crate "sdk/examples/$(EXAMPLE)" --profile "$(PGO_PROFILE)" --gate '$(GATE)' $(PGO_VARIANTS) \
		-- $(EXAMPLE_CARGO)
# hello-xa plays four generated songs from SONGS.XA: synthesise the WAVs, encode
# them as one interleaved XA file, build the example and put both on a disc.
XA_DIR := $(BUILD)/hello-xa-songs
hello-xa-disc:
	cargo run -q --release --locked -p psx-audio-cook --example xa_demo_songs -- "$(XA_DIR)"
	cargo run -q --release --locked -p psx-audio-cook -- xa-encode "$(XA_DIR)/SONGS.XA" \
		"$(XA_DIR)/song0_pad.wav" "$(XA_DIR)/song1_high.wav" "$(XA_DIR)/song2_blips.wav" "$(XA_DIR)/song3_whistle.wav" \
		--manifest "$(XA_DIR)/songs.json"
	$(MAKE) example EXAMPLE=hello-xa
	cargo run --locked --release -p mkisopsx -- --exe "$(BUILD)/$(TARGET)/release/hello-xa.exe" \
		--out "$(BUILD)/$(TARGET)/release/hello-xa.bin" --volume PSOXIDESDK --xa-file "$(XA_DIR)/SONGS.XA"
# Plays hello-xa headless and checks the audio capture: make hello-xa-gate FRONTEND=/path/to/frontend
hello-xa-gate: hello-xa-disc
	@test -n "$(FRONTEND)" || (echo "Set FRONTEND to the PSoXide-emulator executable"; exit 1)
	cargo build -q --release --locked -p psx-audio-cook
	sh tools/xa_gate.sh "$(FRONTEND)" "$(BUILD)/$(TARGET)/release/hello-xa.cue" target/release/psx-audio-cook
# hello-cdstream streams CDTEST.BIN, the deterministic file mkisopsx writes with
# --cdtest-sectors, through psx-cdstream and checks every byte. 960 sectors is
# nearly all the boot area has room for after the executable.
CDSTREAM_SECTORS ?= 960
hello-cdstream-disc:
	$(MAKE) example EXAMPLE=hello-cdstream
	cargo run --locked --release -p mkisopsx -- --exe "$(BUILD)/$(TARGET)/release/hello-cdstream.exe" \
		--out "$(BUILD)/$(TARGET)/release/hello-cdstream.bin" --volume PSOXIDESDK --cdtest-sectors $(CDSTREAM_SECTORS)
# Runs hello-cdstream headless and checks its verdict: make hello-cdstream-gate FRONTEND=/path/to/frontend
hello-cdstream-gate: hello-cdstream-disc
	@test -n "$(FRONTEND)" || (echo "Set FRONTEND to the PSoXide-emulator executable"; exit 1)
	sh tools/cdstream_gate.sh "$(FRONTEND)" "$(BUILD)/$(TARGET)/release/hello-cdstream.cue"
hello-tri:
	$(MAKE) example EXAMPLE=hello-tri
hello-tri-disc:
	$(MAKE) disc EXAMPLE=hello-tri
run-tri: hello-tri-disc
	@test -n "$(FRONTEND)" || (echo "Set FRONTEND to the PSoXide-emulator executable"; exit 1)
	"$(FRONTEND)" launch --path "$(BUILD)/$(TARGET)/release/hello-tri.cue"
