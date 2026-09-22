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
PSX_DELAY_SLOT_FLAGS := "-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"
# The example's cargo invocation. Flags go in through --config rather than
# RUSTFLAGS so the PGO driver can append its own (RUSTFLAGS would replace them).
EXAMPLE_CARGO = build --release --target $(TARGET) -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
	--target-dir "$(BUILD)" \
	--config 'target.$(TARGET).rustflags=[$(PSX_DELAY_SLOT_FLAGS),"-Clink-arg=-T../../psoxide.ld","-Clink-arg=--oformat=binary"]'
# Profile-guided builds (tools/psoxide-pgo/README.md). `example` applies
# PGO_PROFILE when it exists, as variant PGO_VARIANT, and builds plainly
# otherwise; either way the driver runs the hazard patcher and scanner.
PGO = cargo run -q --release --locked -p psoxide-pgo --
PGO_PROFILE ?= sdk/examples/$(EXAMPLE)/pgo.prof
PGO_VARIANT ?= default
# Extra collect arguments, placed after the tape: `--polls FROM..TO` keeps only
# gameplay samples from it, `--launch-arg ARG` passes ARG to the frontend.
PGO_ARGS ?=
PGO_VARIANTS ?= --variant off --variant default --variant hot=500 --variant hot=500+profi
GATE ?=

.PHONY: example disc hello-tri hello-tri-disc run-tri examples pgo-collect pgo-choose
examples:
	@set -e; for example in hello-tri hello-input hello-ot hello-gte hello-tex hello-memcard hello-spstack; do $(MAKE) -f tools/sdk-examples.mk disc EXAMPLE=$$example; done

# After the patched link, tools/stack_guard.py proves every scratchpad stack
# call tree fits its region. It needs the link map, which an example that uses
# psx_rt::scratchpad::ScratchpadStack writes next to its exe from build.rs
# (hello-spstack); without one it only checks the image never switches stacks.
example:
	@test -f "sdk/examples/$(EXAMPLE)/Cargo.toml"
	$(PGO) apply --crate "sdk/examples/$(EXAMPLE)" --profile "$(PGO_PROFILE)" \
		--variant "$$(test -f "$(PGO_PROFILE)" && echo "$(PGO_VARIANT)" || echo off)" -- $(EXAMPLE_CARGO)
	@exe="$(BUILD)/$(TARGET)/release/$(EXAMPLE).exe"; map="$(BUILD)/$(TARGET)/release/$(EXAMPLE).map"; \
		if [ -f "$$map" ]; then python3 tools/stack_guard.py "$$exe" "$$map"; else python3 tools/stack_guard.py "$$exe"; fi
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
hello-tri:
	$(MAKE) example EXAMPLE=hello-tri
hello-tri-disc:
	$(MAKE) disc EXAMPLE=hello-tri
run-tri: hello-tri-disc
	@test -n "$(FRONTEND)" || (echo "Set FRONTEND to the PSoXide-emulator executable"; exit 1)
	"$(FRONTEND)" launch --path "$(BUILD)/$(TARGET)/release/hello-tri.cue"
