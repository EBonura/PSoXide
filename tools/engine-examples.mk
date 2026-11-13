# Engine examples, the hardware-test suite build and its disc images (moved from PSoXide-editor).
# Included by the SDK Makefile and by the editor Makefile (the editor hydrates this file).


PSX_TARGET := mipsel-sony-psx
EXAMPLE_TARGET_DIR := $(CURDIR)/build/examples
EXAMPLE_OUT := build/examples/$(PSX_TARGET)/release
PSX_BUILD_FLAGS := --target $(PSX_TARGET) -Zjson-target-spec -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem
EDITOR_PLAYTEST_PSX_BUILD_FLAGS := --target $(PSX_TARGET) -Zjson-target-spec -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
SDK_EXAMPLE_CARGO_ENV := CARGO_TARGET_DIR="$(EXAMPLE_TARGET_DIR)" RUSTFLAGS="-Cllvm-args=-disable-mips-df-backward-search -Clink-arg=-T../../psoxide.ld -Clink-arg=--oformat=binary"
# Engine examples are their own Cargo workspaces whose SDK crates sit outside the workspace root, so Cargo
# hashes their absolute path and the same commit built from two directories gave two different executables
# (hardware-tests: two layouts, same size). psoxide-rustc takes the checkout out of that hash; see
# tools/psoxide-pgo/README.md ("One guest from any directory").
REPRO_TARGET := $(CURDIR)/build/repro-rustc
REPRO_RUSTC := $(REPRO_TARGET)/release/psoxide-rustc
REPRO_ENV := RUSTC_WRAPPER="$(REPRO_RUSTC)" PSOXIDE_REPRO_ROOT="$(CURDIR)"
ENGINE_EXAMPLE_CARGO_ENV := $(REPRO_ENV) CARGO_TARGET_DIR="$(EXAMPLE_TARGET_DIR)" RUSTFLAGS="-Cllvm-args=-disable-mips-df-backward-search -Clink-arg=-T../../../sdk/psoxide.ld -Clink-arg=--oformat=binary"

CDDA_DEMO_TRACK ?= assets/audio/cdda/GONCHAROV.track02.cdda
# Reconstructed from the CD-DA track on demand rather than tracked: the .cdda
# already holds the identical PCM, so committing both cost 39 MB for one song.
GONCHAROV_WAV ?= build/audio/GONCHAROV.wav
MAGIKAAAAARP_PONG_TRACK ?= assets/audio/cdda/GONCHAROV.track02.cdda
MAGIKAAAAARP_PONG_SPECTRUM := engine/examples/game-magikaaaaaarp-pong/assets/goncharov_spectrum_16x30hz.bin
PSOXIDE_DEV ?= cargo run --manifest-path tools/psoxide-dev/Cargo.toml --release --
PSOXIDE_HWTEST ?= cargo run -q --manifest-path tools/psoxide-hwtest/Cargo.toml --release --

ENGINE_DATA_DISC_EXAMPLES := \
	showcase-text game-pong game-breakout game-invaders \
	showcase-3d showcase-model showcase-lights showcase-fog showcase-particles \
	hello-engine
ENGINE_EXAMPLE_DISCS := $(addsuffix -disc,$(ENGINE_DATA_DISC_EXAMPLES)) game-magikaaaaaarp-pong-disc

define build_engine_data_disc
$(1)-disc: $(1)
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$$(EXAMPLE_OUT)/$(1).exe \
		--out ../../$$(EXAMPLE_OUT)/$(1).bin \
		--volume PSOXIDE
endef

# engine/ examples live outside sdk/examples/ - the engine is its
# own domain and its demos exercise the engine framework.
hello-engine:
	cd engine/examples/hello-engine && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)


showcase-text:
	cd engine/examples/showcase-text && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

game-pong:
	cd engine/examples/game-pong && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

game-magikaaaaaarp-pong:
	cd engine/examples/game-magikaaaaaarp-pong && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

$(GONCHAROV_WAV): $(CDDA_DEMO_TRACK) tools/wav_from_cdda.py
	python3 tools/wav_from_cdda.py $(CDDA_DEMO_TRACK) -o $@

magikaaaaaarp-pong-spectrum: $(GONCHAROV_WAV)
	$(PSOXIDE_DEV) bake-spectrum $(GONCHAROV_WAV) \
		-o $(MAGIKAAAAARP_PONG_SPECTRUM) \
		--fps 30 --bands 16 --seconds 233

game-magikaaaaaarp-pong-disc: game-magikaaaaaarp-pong
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.exe \
		--out ../../$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.bin \
		--volume MAGIKARP \
		--cdda-track ../../$(MAGIKAAAAARP_PONG_TRACK)


game-breakout:
	cd engine/examples/game-breakout && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

game-invaders:
	cd engine/examples/game-invaders && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

showcase-3d: assets
	cd engine/examples/showcase-3d && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

showcase-model:
	cd engine/examples/showcase-model && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

showcase-lights: assets
	cd engine/examples/showcase-lights && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

# showcase-fog uses two cooked textures (brick wall + cobblestone
# floor) on its corridor walls + floor, plus procedural geometry.
showcase-fog: assets
	cd engine/examples/showcase-fog && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

showcase-particles:
	cd engine/examples/showcase-particles && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

# Cargo features of the guest, e.g. HWTEST_FEATURES=console-diag.
HWTEST_FEATURES ?=

.PHONY: repro-rustc
repro-rustc:
	cargo build -q --release --manifest-path tools/psoxide-pgo/Cargo.toml --target-dir "$(REPRO_TARGET)" --bin psoxide-rustc
hello-engine showcase-text game-pong game-magikaaaaaarp-pong game-breakout game-invaders showcase-3d showcase-model showcase-lights showcase-fog showcase-particles hardware-tests: repro-rustc
.PHONY: hardware-tests hardware-tests-disc hwtest-radius-disc hwtest-verify-code hello-engine showcase-text game-pong game-breakout game-invaders showcase-3d showcase-model showcase-lights showcase-fog showcase-particles game-magikaaaaaarp-pong game-magikaaaaaarp-pong-disc magikaaaaaarp-pong-spectrum assets psxed engine-examples
hardware-tests:
	cd engine/examples/hardware-tests && PSX_GATE_PROGRAM=hardware-tests $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(EDITOR_PLAYTEST_PSX_BUILD_FLAGS) $(if $(HWTEST_FEATURES),--features "$(HWTEST_FEATURES)")
	cargo run -q --release --locked -p psoxide-hazard --bin hazard-scan -- $(EXAMPLE_OUT)/hardware-tests.exe

# --- hardware-test pipeline ----------------------------------------------

HWTEST_SUITE := $(shell sed -n 's/^const SUITE_VERSION: &str = "HWTEST v\(.*\)";/\1/p' engine/examples/hardware-tests/src/main.rs)
HWTEST_CODE_BASELINE := docs/hardware-refs/hwtest-machine-code-v$(HWTEST_SUITE).txt

# Audit the linked EXE: the instructions between each probe's markers must be
# the ones the source asked for, or its cycle count measures something else.
hwtest-verify-code: hardware-tests
	@test -f $(HWTEST_CODE_BASELINE) || { \
		echo "hwtest-verify-code: $(HWTEST_CODE_BASELINE) does not exist."; \
		echo "  The suite version bumped without a machine-code baseline."; \
		echo "  Review the spans, then pin them with:"; \
		echo "    $(PSOXIDE_HWTEST) verify-machine-code $(EXAMPLE_OUT)/hardware-tests.exe --baseline <previous version's file> | grep -v '^# drift' | cut -d, -f1-4 > $(HWTEST_CODE_BASELINE)"; \
		echo "  (--baseline carries the probe names over by id; name any probe_NN rows by hand.)"; \
		exit 2; }
	$(PSOXIDE_HWTEST) verify-machine-code $(EXAMPLE_OUT)/hardware-tests.exe \
		--baseline $(HWTEST_CODE_BASELINE) --fail-on-change


# The deterministic read region and the other data the disc carries.
# The CD-DA track is required, not decorative: read-while-CD-DA contention is
# the one CD failure no emulator reproduces. The tone is synthesised
# (tools/gen-cdda-tone.py) so the image stays redistributable and
# bit-reproducible.
HWTEST_CDDA := $(EXAMPLE_OUT)/hardware-tests-cdda.pcm

$(HWTEST_CDDA):
	@mkdir -p $(dir $@)
	python3 tools/gen-cdda-tone.py --seconds 10 --out $@

# HWSONGS.XA for the XA music loop: the SDK's four generated tone songs (6 s
# each, plain synthesis, nothing sampled) as the channels of one 37.8 kHz
# stereo single-speed file. The SDK's psx-audio-cook is not imported here, so
# PSOXIDE_SDK names a PSoXide checkout to run it from; the encode is
# deterministic, and to reuse a built file, copy it to this path.
PSOXIDE_SDK ?= $(CURDIR)
HWTEST_XA_DIR := $(EXAMPLE_OUT)/xa
HWTEST_XA := $(HWTEST_XA_DIR)/HWSONGS.XA
HWTEST_XA_COOK = cargo run -q --release --locked --manifest-path "$(PSOXIDE_SDK)/Cargo.toml" \
	--target-dir "$(CURDIR)/build/xa-tools" -p psx-audio-cook

$(HWTEST_XA):
	@[ -n "$(PSOXIDE_SDK)" ] || { echo "HWSONGS.XA: set PSOXIDE_SDK to a PSoXide checkout (its psx-audio-cook encodes the songs)" >&2; exit 1; }
	@mkdir -p $(HWTEST_XA_DIR)
	$(HWTEST_XA_COOK) --example xa_demo_songs -- "$(HWTEST_XA_DIR)" 6
	$(HWTEST_XA_COOK) -- xa-encode $@ \
		$(HWTEST_XA_DIR)/song0_pad.wav $(HWTEST_XA_DIR)/song1_high.wav \
		$(HWTEST_XA_DIR)/song2_blips.wav $(HWTEST_XA_DIR)/song3_whistle.wav \
		--manifest $(HWTEST_XA_DIR)/songs.json

# HWCLOCK.XA for the XA clock drift case: one 190 s song (the pad of the
# generated set, alone in its file as file number 2) that the case plays for
# 180 s. 14,250 sectors, 33 MB: the length of the song is the point.
HWTEST_XA_CLOCK := $(HWTEST_XA_DIR)/HWCLOCK.XA

$(HWTEST_XA_CLOCK):
	@[ -n "$(PSOXIDE_SDK)" ] || { echo "HWCLOCK.XA: set PSOXIDE_SDK to a PSoXide checkout (its psx-audio-cook encodes the songs)" >&2; exit 1; }
	@mkdir -p $(HWTEST_XA_DIR)/clock
	$(HWTEST_XA_COOK) --example xa_demo_songs -- "$(HWTEST_XA_DIR)/clock" 190
	$(HWTEST_XA_COOK) -- xa-encode $@ $(HWTEST_XA_DIR)/clock/song0_pad.wav --file 2 \
		--manifest $(HWTEST_XA_DIR)/clock.json

# The boot EXE of the BIOS-free suite (v2.9 on) is 547 sectors, past the 542 that
# CDTEST.BIN at 460 sectors leaves, so the disc is always the console-diag build
# (the SDK stop panel; CDTEST_SECTORS 400 in main.rs), the same layout as the
# radius disc and as every v2.9 burn.
hardware-tests-disc: HWTEST_FEATURES = console-diag
hardware-tests-disc: hardware-tests $(HWTEST_CDDA) $(HWTEST_XA) $(HWTEST_XA_CLOCK)
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/hardware-tests.exe \
		--out ../../$(EXAMPLE_OUT)/hardware-tests.bin \
		--volume PSOXIDE \
		--cdtest-sectors 400 \
		--xa-file ../../$(HWTEST_XA) \
		--xa-file ../../$(HWTEST_XA_CLOCK) \
		--cdda-track ../../$(HWTEST_CDDA)

# --- radius disc (v2.8) ---------------------------------------------------
# The plain hardware-test image padded with RADIUS.DAT, deterministic filler
# (psoxide-hwtest radius-fill) that carries the data track out to about 78
# minutes, as full as the demo disc, so the CD RADIUS SWEEP step has outer
# radius to read. It is built with the SDK stop panel (console-diag), which
# moves CDTEST.BIN to 400 sectors (see CDTEST_SECTORS in main.rs). The filler
# goes last on the data track, after the XA files, so everything else keeps its
# place; the CD-DA track follows it, so on this disc it sits at the outer edge.
# 16,660 sectors of image plus the filler stay under the 359,844 blocks of an
# 80-minute CD-R; the recipe checks it.
HWTEST_RADIUS_DIR := build/hwtest-radius
HWTEST_RADIUS_FILL := $(HWTEST_RADIUS_DIR)/RADIUS.DAT
HWTEST_RADIUS_SECTORS ?= 334300
HWTEST_RADIUS_BLOCKS_MAX := 359844

$(HWTEST_RADIUS_FILL):
	@mkdir -p $(dir $@)
	$(PSOXIDE_HWTEST) radius-fill $@ $(HWTEST_RADIUS_SECTORS)

hwtest-radius-disc: HWTEST_FEATURES = console-diag
hwtest-radius-disc: hardware-tests $(HWTEST_CDDA) $(HWTEST_XA) $(HWTEST_XA_CLOCK) $(HWTEST_RADIUS_FILL)
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/hardware-tests.exe \
		--out ../../$(HWTEST_RADIUS_DIR)/hardware-tests-radius.bin \
		--volume PSOXIDE \
		--cdtest-sectors 400 \
		--xa-file ../../$(HWTEST_XA) \
		--xa-file ../../$(HWTEST_XA_CLOCK) \
		--xa-file ../../$(HWTEST_RADIUS_FILL) \
		--cdda-track ../../$(HWTEST_CDDA)
	@sectors=$$(( $$(stat -f %z $(HWTEST_RADIUS_DIR)/hardware-tests-radius.bin) / 2352 )); \
		echo "radius disc: $$sectors sectors ($$(( sectors / 4500 )) min $$(( sectors % 4500 / 75 )) s), limit $(HWTEST_RADIUS_BLOCKS_MAX)"; \
		[ $$sectors -le $(HWTEST_RADIUS_BLOCKS_MAX) ] || { echo "radius disc does not fit an 80-minute CD-R" >&2; exit 2; }


PSXED := target/release/psxed

# Build the content-pipeline CLI. Independent host workspace -
# always builds fast, no MIPS toolchain needed.
psxed:
	cargo build --release --locked -p psxed

# Cook source assets into the binary blobs examples embed via
# include_bytes!. Re-runs whenever an .obj changes. Targets go
# next to the source under `assets/` so a repo clone has the
# runtime input available without having to run the editor.
SHOWCASE_3D := engine/examples/showcase-3d
SHOWCASE_LIGHTS := engine/examples/showcase-lights
HELLO_TEX := sdk/examples/hello-tex
TEXTURE_ASSETS := assets/textures
MAGIKAAAAARP_PONG := engine/examples/game-magikaaaaaarp-pong

# Texture sources committed under example `vendor/` directories are
# small pre-cropped JPGs. Larger originals are intentionally not
# committed. Runtime examples consume the shared cooked blobs under
# `assets/textures/`, so `make assets` updates that canonical location.
# It still skips missing source files so local experiments with ignored
# high-res replacements do not break fresh clones or CI.
define cook_texture
	@if [ -f "$(1)" ]; then \
	    $(PSXED) tex "$(1)" -o "$(2)" --size $(3) --depth $(4) --resample lanczos3 $(5) ; \
	else \
	    echo "[psxed tex] skip: source $(1) not present (using committed $(2))" ; \
	fi
endef

assets: psxed
	@mkdir -p $(SHOWCASE_3D)/assets $(SHOWCASE_LIGHTS)/assets $(TEXTURE_ASSETS)
	@$(PSXED) obj $(SHOWCASE_3D)/vendor/suzanne.obj \
	    -o $(SHOWCASE_3D)/assets/suzanne.psxm \
	    --palette warm --decimate-grid 6 --compute-normals
	@$(PSXED) obj $(SHOWCASE_3D)/vendor/teapot.obj \
	    -o $(SHOWCASE_3D)/assets/teapot.psxm \
	    --palette cool --compute-normals
	@$(PSXED) obj $(SHOWCASE_LIGHTS)/vendor/cube.obj \
	    -o $(SHOWCASE_LIGHTS)/assets/cube.psxm \
	    --compute-normals --no-colors
	$(call cook_texture,$(HELLO_TEX)/vendor/brick-wall.jpg,$(TEXTURE_ASSETS)/brick-wall.psxt,64x64,4)
	$(call cook_texture,$(HELLO_TEX)/vendor/floor.jpg,$(TEXTURE_ASSETS)/floor.psxt,64x64,4)
	$(call cook_texture,$(MAGIKAAAAARP_PONG)/vendor/magikaaaaaarp_album.jpg,$(MAGIKAAAAARP_PONG)/assets/magikaaaaaarp_album.psxt,128x128,8)
	$(call cook_texture,$(MAGIKAAAAARP_PONG)/vendor/score_flyby.png,$(MAGIKAAAAARP_PONG)/assets/score_flyby.psxt,128x128,8,--transparent-index-zero)
	@$(MAKE) magikaaaaaarp-pong-spectrum

$(foreach example,$(ENGINE_DATA_DISC_EXAMPLES),$(eval $(call build_engine_data_disc,$(example))))

engine-examples: $(ENGINE_EXAMPLE_DISCS)

# Reproducibility check: build the same checkout from two directories and require byte-identical
# executables (tools/psoxide-pgo "One guest from any directory"). Two full guest builds per artifact, so
# it is not part of `make lint`; it is a step of every fleet repin's gate list. The gate clones the tree
# into its own temp directories, so run it on a clean build of the commit being checked.
.PHONY: repro-check repro-check-editor-playtest repro-check-hello-engine
REPRO_GATE = cargo run -q --release --locked -p psoxide-pgo --bin repro-gate --
repro-check: repro-check-editor-playtest repro-check-hello-engine
repro-check-editor-playtest:
	$(REPRO_GATE) --source "$(CURDIR)" \
		--build 'sh tools/build_guest_staged.sh $(EDITOR_PLAYTEST_PSX_BUILD_FLAGS)' \
		--artifact '{checkout}/$(EXAMPLE_OUT)/editor-playtest.exe'
repro-check-hello-engine:
	$(REPRO_GATE) --source "$(CURDIR)" \
		--build 'make hello-engine' \
		--artifact '{checkout}/$(EXAMPLE_OUT)/hello-engine.exe'
