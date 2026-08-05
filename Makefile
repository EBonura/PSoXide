# PSoXide development commands.
#
# Cargo workspaces:
#   root   - no_std shared crates (psx-hw, psx-iso, psx-trace)
#   editor - host-side editor/content pipeline crates
#   emu    - host-side emulator/frontend crates
#   engine - PSX runtime engine crates
#   sdk    - MIPS target SDK crates
#
# Standalone tool crates live under tools/* and are gated explicitly.
#
# SDK and engine examples are compiled individually with explicit PSX
# cargo flags from this Makefile.

.PHONY: help check test canaries fmt lint lint-policy-guard runtime-numeric-guard clean run run-release editor-ui-screenshot pgo web validate validate-repeat validate-bless \
        test-sdk \
        psxed assets \
	examples hello-tri hello-tri-disc hello-input hello-input-disc hello-ot hello-ot-disc \
	hello-tex hello-tex-disc hello-gte hello-gte-disc hello-audio hello-audio-disc \
	hello-cdda hello-cdda-disc \
	hello-pack hello-pack-fixture hello-pack-disc verify-hello-pack \
	cdda-read-contention cdda-read-contention-disc \
	run-tri run-input run-ot run-tex run-gte run-audio run-cdda probe-cdda-audio \
	showcase-text showcase-text-disc run-showcase-text \
	game-pong game-pong-disc run-game-pong \
	game-magikaaaaaarp-pong game-magikaaaaaarp-pong-disc magikaaaaaarp-pong-spectrum run-game-magikaaaaaarp-pong probe-magikaaaaaarp-pong-audio duckstation-magikaaaaaarp-pong \
	cortex-ignition-v1-project-disc cortex-ignition-v1-project-disc-boot-trace cortex-ignition-v1-hardware-diagnostic-disc cortex-ignition-v1-preburn-local cortex-ignition-v1-preburn-struct cortex-ignition-v1-preburn-disc-reads cortex-ignition-v1-preburn-internal cortex-ignition-v1-preburn-cdda-audio cortex-ignition-v1-preburn-bios-cdrom cortex-ignition-v1-preburn-boot-flow cortex-ignition-v1-preburn-streaming-guard cortex-ignition-v1-emulator-inventory cortex-ignition-v1-external-emulators cortex-ignition-v1-bringup-report cortex-ignition-v1-burn-candidate duckstation-cortex-ignition-v1 duckstation-cortex-ignition-v1-bios mednafen-cortex-ignition-v1-bios retroarch-cortex-ignition-v1-bios ares-cortex-ignition-v1-bios \
	game-breakout game-breakout-disc run-game-breakout \
        game-invaders game-invaders-disc run-game-invaders \
        showcase-3d showcase-3d-disc run-showcase-3d \
        showcase-model showcase-model-disc run-showcase-model \
        showcase-lights showcase-lights-disc run-showcase-lights \
	showcase-fog showcase-fog-disc run-showcase-fog \
	showcase-particles showcase-particles-disc run-showcase-particles \
	hardware-tests hardware-tests-disc run-hardware-tests \
	hwtest-capture hwtest-diff hwtest-baseline hwtest-silicon hwtest-verify-code hwtest-audio hwtest-audio-chain \
	hello-engine hello-engine-disc run-hello-engine \
	cook-playtest build-editor-playtest profile-demo3 profile-demo3-forward \
	profile-demo3-paced20 profile-demo3-paced20-forward profile-demo3-disc-stream \
	profile-demo3-disc-stream-forward profile-demo7-camera-sweep

help:
	@echo "PSoXide targets:"
	@echo ""
	@echo "  Emulator / host:"
	@echo "    make check        - cargo check on all workspaces and tools"
	@echo "    make test         - fast unit tests (all workspaces/tools, excludes canaries)"
	@echo "    make canaries     - commercial-game canary tests (Milestones D-K)"
	@echo "    make fmt          - format all code"
	@echo "    make lint         - clippy -D warnings"
	@echo "    make lint-policy-guard"
	@echo "                      - assert Cargo workspace lint policy stays in sync"
	@echo "    make runtime-numeric-guard"
	@echo "                      - reject floats/wide ints in PS1 runtime code"
	@echo "    make clean        - cargo clean all workspaces"
	@echo "    make run          - launch the desktop frontend with fast incremental builds"
	@echo "    make run-release  - launch the fully optimised desktop frontend"
	@echo "    make editor-ui-screenshot"
	@echo "                      - render the exact editor view to PNG without opening a window"
	@echo "    make pgo          - PGO-optimised frontend build (PGO_GAME=<.cue/.exe>, PSOXIDE_BIOS set; ~2x faster core)"
	@echo "    make web          - serve the wasm web build locally (release, :8080)"
	@echo "    make validate     - run exact-hash validation matrix"
	@echo "    make validate-repeat"
	@echo "                      - run exact-hash validation 3 times for determinism"
	@echo "    make validate-bless"
	@echo "                      - update exact-hash validation baselines"
	@echo "    make test-sdk     - build every SDK example + run Milestone-C regression suite"
	@echo "    make profile-demo3 - cook/build demo3 BIN and dump streamed screenshot/profile"
	@echo "    make profile-demo3-forward - streamed demo3 profile while holding forward"
	@echo "    make profile-demo3-paced20 - alias for streamed 20Hz visual cadence telemetry"
	@echo "    make profile-demo3-paced20-forward - streamed paced20 profile while holding forward"
	@echo "    make profile-demo3-disc-stream - build/play demo3 from BIN and measure CD streaming"
	@echo "    make profile-demo3-disc-stream-forward - same, while holding forward"
	@echo "    make profile-demo7-camera-sweep - streamed demo7 deterministic camera sweep profile"
	@echo ""
	@echo "  SDK examples (build burnable .cue + .bin discs):"
	@echo "    make examples     - build every public example disc"
	@echo "    make psxed        - build the content-pipeline CLI"
	@echo "    make assets       - cook source assets via psxed"
	@echo "    make hello-tri-disc    - build the direct-GP0 triangle demo disc"
	@echo "    make hello-input-disc  - build the pad-poll demo disc"
	@echo "    make hello-ot-disc     - build the DMA linked-list demo disc"
	@echo "    make hello-tex-disc    - build the CLUT texture demo disc"
	@echo "    make hello-gte-disc    - build the GTE perspective-transform demo disc"
	@echo "    make hello-audio-disc  - build the imported SPU sample demo disc"
	@echo "    make hello-cdda-disc   - build the CD-DA playback demo disc"
	@echo "    make hello-pack-disc   - build the WORLD.PAK CD-streaming smoke-test disc"
	@echo "    make verify-hello-pack - run hello-pack headless + assert ALL PASS"
	@echo "    make showcase-text"
	@echo "                      - build the text / font capabilities showcase disc"
	@echo "    make game-pong-disc - build the Pong mini-game disc"
	@echo "    make game-magikaaaaaarp-pong"
	@echo "                      - build the magikAAAAArp Pong mini-game"
	@echo "    make game-magikaaaaaarp-pong-disc"
	@echo "                      - build magikAAAAArp Pong as a CD-DA disc"
	@echo "    make magikaaaaaarp-pong-spectrum"
	@echo "                      - bake the GONCHAROV spectrum visualizer asset"
	@echo "    make game-breakout-disc - build the Breakout mini-game disc"
	@echo "    make game-invaders-disc - build the Space Invaders mini-game disc"
	@echo "    make showcase-3d-disc    - build the 3D geometry showcase disc"
	@echo "    make showcase-model-disc - build the animated native-model demo disc"
	@echo "    make showcase-lights-disc - build the 4-point-light demo disc"
	@echo "    make showcase-fog-disc   - build the fog / full-GTE-pipeline demo disc"
	@echo "    make showcase-particles-disc - build the particle-pool demo disc"
	@echo "    make hardware-tests-disc - build the visual PS1 hardware test suite"
	@echo "    make run-tri      - build + boot hello-tri as a disc"
	@echo "    make run-input    - build + boot hello-input as a disc"
	@echo "    make run-ot       - build + boot hello-ot as a disc"
	@echo "    make run-tex      - build + boot hello-tex as a disc"
	@echo "    make run-gte      - build + boot hello-gte as a disc"
	@echo "    make run-audio    - build + boot hello-audio as a disc"
	@echo "    make run-cdda     - build + boot hello-cdda with a mixed-mode disc"
	@echo "    make probe-cdda-audio - render hello-cdda audio to a WAV + silence check"
	@echo "    make probe-magikaaaaaarp-pong-audio"
	@echo "                      - render magikAAAAArp Pong CD-DA to a WAV + silence check"
	@echo "    make duckstation-magikaaaaaarp-pong"
	@echo "                      - boot magikAAAAArp Pong in DuckStation and assert TTY markers"
	@echo "    make duckstation-cortex-ignition-v1"
	@echo "                      - build cortex_ignition_v1's project disc and assert DuckStation TTY markers"
	@echo "    make cortex-ignition-v1-hardware-diagnostic-disc"
	@echo "                      - build cortex_ignition_v1 with TV-visible boot color checkpoints"
	@echo "    make cortex-ignition-v1-preburn-local"
	@echo "                      - run local structural/headless/audio/CD probes before burning"
	@echo "    make cortex-ignition-v1-preburn-streaming-guard"
	@echo "                      - fail if CD-DA plus room-streaming telemetry is absent or red"
	@echo "    make duckstation-cortex-ignition-v1-bios"
	@echo "                      - assert cortex_ignition_v1 through DuckStation's full BIOS/logo path"
	@echo "                      - assert cortex_ignition_v1 through PCSX-Redux's BIOS disc boot"
	@echo "    make cortex-ignition-v1-emulator-inventory"
	@echo "                      - list locally available external PS1 emulators"
	@echo "    make cortex-ignition-v1-external-emulators"
	@echo "                      - run DuckStation/Redux plus optional Mednafen/RetroArch/ares local gates"
	@echo "    make cortex-ignition-v1-bringup-report"
	@echo "                      - summarize latest cortex_ignition_v1 bringup logs"
	@echo "    make cortex-ignition-v1-burn-candidate"
	@echo "                      - run preburn + emulator matrix and fail on WARN/MISSING report rows"
	@echo "    make run-showcase-text"
	@echo "                      - build + boot the text capabilities showcase disc"
	@echo "    make run-game-pong     - build + boot the Pong mini-game disc"
	@echo "    make run-game-magikaaaaaarp-pong"
	@echo "                      - build + boot magikAAAAArp Pong with CD-DA"
	@echo "    make run-game-breakout - build + boot the Breakout mini-game disc"
	@echo "    make run-game-invaders - build + boot the Space Invaders mini-game disc"
	@echo "    make run-showcase-3d - build + boot the 3D geometry showcase disc"
	@echo "    make run-showcase-model - build + boot the animated model demo disc"
	@echo "    make run-showcase-lights - build + boot the 4-point-light demo disc"
	@echo "    make run-showcase-fog - build + boot the fog demo disc"
	@echo "    make run-showcase-particles - build + boot the particle demo disc"
	@echo "    make run-hardware-tests - build + boot the visual hardware test suite"

run:
	cd emu && cargo run -p frontend

EDITOR_UI_PROJECT ?= editor/projects/cortex_ignition_v1
EDITOR_UI_VIEW ?= animation
EDITOR_UI_RESOURCE ?= Stand To Roll
EDITOR_UI_OUT ?= /tmp/psoxide-editor-ui.png
EDITOR_UI_FRAME_SELECTED ?= 0

editor-ui-screenshot:
	cargo run -p frontend -- dump-editor-ui \
		--project "$(EDITOR_UI_PROJECT)" \
		--view "$(EDITOR_UI_VIEW)" \
		$(if $(strip $(EDITOR_UI_RESOURCE)),--resource "$(EDITOR_UI_RESOURCE)",) \
		$(if $(filter 1 true yes,$(EDITOR_UI_FRAME_SELECTED)),--frame-selected,) \
		--out "$(EDITOR_UI_OUT)"

run-release:
	cd emu && cargo run -p frontend --release

# CI and the web deploy deliberately skip PGO: rustc's PGO does not
# target wasm (browsers JIT-optimize at runtime instead), and no CI
# pipeline ships native binaries -- fast local builds come from this
# target.
#
# Profile-guided release build. Interpreters are the canonical PGO
# winner: measured 8.1s -> 3.9s (-52%) on the 500M-instruction ALTTP
# benchmark, identical VRAM/display hashes. Trains on PGO_GAME (any
# .cue/.exe; pick something representative of what you'll run) plus a
# short BIOS boot, then rebuilds with the merged profile. The .profdata
# is toolchain-specific scratch, never committed. Plain `make run` /
# cargo builds are unaffected (no default RUSTFLAGS).
PGO_DIR := target/pgo-profiles
pgo:
ifndef PGO_GAME
	$(error PGO_GAME is required, e.g. make pgo PGO_GAME=path/to/game.cue)
endif
	rm -rf $(PGO_DIR)
	cd emu && RUSTFLAGS="-Cprofile-generate=$(abspath $(PGO_DIR))" \
		cargo build -p frontend --release --features mcp
	target/release/frontend launch --path "$(PGO_GAME)" --steps 500000000
	target/release/frontend launch --path "$(PGO_GAME)" --steps 150000000 --bios-boot
	"$$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-profdata merge \
		-o $(PGO_DIR)/merged.profdata $(PGO_DIR)/*.profraw
	cd emu && RUSTFLAGS="-Cprofile-use=$(abspath $(PGO_DIR))/merged.profdata" \
		cargo build -p frontend --release --features mcp
	@echo "PGO build ready: target/release/frontend"

# Serve the emulator-only wasm build locally (optimized) at http://127.0.0.1:8080.
# `env -u NO_COLOR` works around trunk choking on the NO_COLOR=1 shell env.
web:
	@-lsof -ti tcp:8080 | xargs kill -9 2>/dev/null || true
	cd emu/crates/frontend && env -u NO_COLOR trunk serve --release

validate:
	cd emu && cargo run -p frontend --release -- validate --manifest ../validation/suite.ron

validate-repeat:
	cd emu && cargo run -p frontend --release -- validate --manifest ../validation/suite.ron --repeat 3

validate-bless:
	cd emu && cargo run -p frontend --release -- validate --manifest ../validation/suite.ron --bless

check:
	cargo check --workspace --all-features
	cd engine && cargo check --workspace --all-features
	cd sdk && cargo check --workspace --all-features

test:
	cargo test --workspace
	cd engine && cargo test --workspace
	cd sdk && cargo test --workspace

canaries:
	cargo test --workspace -- --ignored

fmt:
	cargo fmt --all
	cd engine && cargo fmt --all
	cd sdk && cargo fmt --all

lint:
	$(PSOXIDE_DEV) lint-policy-guard
	$(PSOXIDE_DEV) runtime-numeric-guard
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd engine && cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd sdk && cargo clippy --workspace --all-targets --all-features -- -D warnings

lint-policy-guard:
	$(PSOXIDE_DEV) lint-policy-guard

runtime-numeric-guard:
	$(PSOXIDE_DEV) runtime-numeric-guard

clean:
	cargo clean
	cd engine && cargo clean
	cd sdk && cargo clean
	rm -rf build

# Milestone-C regression suite - every SDK example side-loaded into
# the emulator, multi-signal state pinned. Depends on `examples` so
# every .exe referenced by the tests exists before we run them; the
# tests themselves skip gracefully when an .exe is missing, but
# gating on `examples` here surfaces build breaks up-front.
test-sdk: examples
	cd emu && cargo test -p emulator-core --release --test sdk_milestones -- --ignored --nocapture

# --- SDK examples ---------------------------------------------------------

PSX_TARGET := mipsel-sony-psx
EXAMPLE_TARGET_DIR := $(CURDIR)/build/examples
EXAMPLE_OUT := build/examples/$(PSX_TARGET)/release
PSX_BUILD_FLAGS := --target $(PSX_TARGET) -Zjson-target-spec -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem
SDK_EXAMPLE_CARGO_ENV := CARGO_TARGET_DIR=$(EXAMPLE_TARGET_DIR) RUSTFLAGS="-Clink-arg=-T../../psoxide.ld -Clink-arg=--oformat=binary"
ENGINE_EXAMPLE_CARGO_ENV := CARGO_TARGET_DIR=$(EXAMPLE_TARGET_DIR) RUSTFLAGS="-Clink-arg=-T../../../sdk/psoxide.ld -Clink-arg=--oformat=binary"
EDITOR_PLAYTEST_CARGO_ENV := CARGO_TARGET_DIR=$(EXAMPLE_TARGET_DIR) RUSTFLAGS="-Zunstable-options -Cpanic=immediate-abort -Clink-arg=-T../../../sdk/psoxide.ld -Clink-arg=--oformat=binary"
EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX := ../../engine/examples/editor-playtest/generated
CDDA_DEMO_TRACK ?= assets/audio/cdda/GONCHAROV.track02.cdda
GONCHAROV_WAV ?= assets/audio/cdda/GONCHAROV.wav
MAGIKAAAAARP_PONG_TRACK ?= assets/audio/cdda/GONCHAROV.track02.cdda
MAGIKAAAAARP_PONG_SPECTRUM := engine/examples/game-magikaaaaaarp-pong/assets/goncharov_spectrum_16x30hz.bin
DUCKSTATION_TIMEOUT ?= 45
DUCKSTATION_MAGIKARP_LOG ?= build/duckstation-harness/game-magikaaaaaarp-pong.log
DUCKSTATION_CORTEX_IGNITION_V1_LOG ?= build/duckstation-harness/cortex_ignition_v1.log
DUCKSTATION_CORTEX_IGNITION_V1_BIOS_LOG ?= build/duckstation-harness/cortex_ignition_v1-bios.log
MEDNAFEN_CORTEX_IGNITION_V1_LOG ?= build/external-emulator-smoke/cortex_ignition_v1-mednafen.log
RETROARCH_CORTEX_IGNITION_V1_LOG ?= build/external-emulator-smoke/cortex_ignition_v1-retroarch.log
RETROARCH_CORTEX_IGNITION_V1_SCREENSHOT ?= build/external-emulator-smoke/cortex_ignition_v1-retroarch.png
RETROARCH_CORTEX_IGNITION_V1_SCREENSHOT_FRAMES ?= 360
ARES_CORTEX_IGNITION_V1_LOG ?= build/external-emulator-smoke/cortex_ignition_v1-ares.log
EXTERNAL_EMULATOR_SMOKE_TIMEOUT ?= 12
REDUX_CORTEX_IGNITION_V1_BIOS ?= $(PSOXIDE_BIOS)
REDUX_CORTEX_IGNITION_V1_STEPS ?= 240000000
# The project was renamed cortex_ignition_v1 -> cortex_v1. Every path and the
# ISO volume id derive from these two, so a future rename is a two-line change
# rather than a hunt through the recipes.
CORTEX_IGNITION_V1_NAME ?= cortex_v1
CORTEX_IGNITION_V1_VOLUME ?= CORTEX_V1
CORTEX_IGNITION_V1_PROJECT ?= editor/projects/$(CORTEX_IGNITION_V1_NAME)
CORTEX_IGNITION_V1_CUE ?= $(CORTEX_IGNITION_V1_PROJECT)/baked/$(CORTEX_IGNITION_V1_NAME).cue
CORTEX_IGNITION_V1_BIN ?= $(CORTEX_IGNITION_V1_PROJECT)/baked/$(CORTEX_IGNITION_V1_NAME).bin
CORTEX_IGNITION_V1_PREBURN_OUT ?= build/preburn/$(CORTEX_IGNITION_V1_NAME)
# The streaming guard asserts that room streaming actually moved CD bytes, so
# the internal run has to REACH GAMEPLAY. At 90 visual frames it halted about
# three seconds in, still on the intro, and the guard failed with
# "no room stream requests observed" -- a stale budget reading as a defect.
# cortex_v1 reaches gameplay around visual frame 290.
CORTEX_IGNITION_V1_PREBURN_VISUAL_FRAMES ?= 900
CORTEX_IGNITION_V1_PREBURN_GUEST_FRAMES ?= 2400
CORTEX_IGNITION_V1_PREBURN_STEPS ?= 1200000000
CORTEX_IGNITION_V1_PREBURN_BIOS_STEPS ?= 120000000
# Real-BIOS boot path: BIOS POST (~213M cyc) + game load + intro before the menu
# CD-DA Play (~cyc 577M, ~step 250M). Budget must clear that, unlike the faster
# HLE internal launch above.
CORTEX_IGNITION_V1_PREBURN_BOOT_FLOW_STEPS ?= 340000000
CORTEX_IGNITION_V1_PREBURN_BOOT_FLOW_PAD1 ?= 0
CORTEX_IGNITION_V1_PREBURN_BOOT_FLOW_PULSES ?= 0x4000@974+25
CORTEX_IGNITION_V1_PREBURN_AUDIO_SECONDS ?= 6
CORTEX_IGNITION_V1_PREBURN_AUDIO_MIN_PEAK ?= 256
CORTEX_IGNITION_V1_PREBURN_FEATURES ?= cd-stream-bench emulator-telemetry
# Confirms through the intro and menu on the current boot flow. Edge-triggered,
# so several spaced pulses beat one long hold.
CORTEX_IGNITION_V1_PREBURN_PAD_PULSES ?= 0x4000@60+16,0x4000@120+16,0x4000@180+16,0x4000@240+16,0x4000@300+16
CORTEX_IGNITION_V1_PREBURN_INTERNAL_DISC_DIR ?= $(CORTEX_IGNITION_V1_PREBURN_OUT)/internal-disc
CORTEX_IGNITION_V1_PREBURN_INTERNAL_CUE ?= $(CORTEX_IGNITION_V1_PREBURN_INTERNAL_DISC_DIR)/$(CORTEX_IGNITION_V1_NAME).cue
CORTEX_IGNITION_V1_PREBURN_INTERNAL_BIN ?= $(CORTEX_IGNITION_V1_PREBURN_INTERNAL_DISC_DIR)/$(CORTEX_IGNITION_V1_NAME).bin
CORTEX_IGNITION_V1_BRINGUP_REPORT ?= $(CORTEX_IGNITION_V1_PREBURN_OUT)/BRINGUP_REPORT.md
PSOXIDE_DEV ?= cargo run --manifest-path tools/psoxide-dev/Cargo.toml --release --
PROFILE_DEMO3_FRAMES ?= 60
PROFILE_DEMO3_STEPS ?= 120000000
PROFILE_DEMO3_HW ?= /tmp/psoxide-demo3-hw-$(PROFILE_DEMO3_FRAMES).ppm
PROFILE_DEMO3_FORWARD_FRAMES ?= 240
PROFILE_DEMO3_FORWARD_STEPS ?= 480000000
PROFILE_DEMO3_FORWARD_HW ?= /tmp/psoxide-demo3-forward-hw-$(PROFILE_DEMO3_FORWARD_FRAMES).ppm
PROFILE_DEMO3_PACED20_VISUAL_FRAMES ?= 60
PROFILE_DEMO3_PACED20_GUEST_FRAMES ?= 720
PROFILE_DEMO3_PACED20_STEPS ?= 360000000
PROFILE_DEMO3_PACED20_HW ?= /tmp/psoxide-demo3-paced20-hw-$(PROFILE_DEMO3_PACED20_VISUAL_FRAMES).ppm
PROFILE_DEMO3_PACED20_FORWARD_VISUAL_FRAMES ?= 80
PROFILE_DEMO3_PACED20_FORWARD_GUEST_FRAMES ?= 1200
PROFILE_DEMO3_PACED20_FORWARD_STEPS ?= 480000000
PROFILE_DEMO3_PACED20_FORWARD_HW ?= /tmp/psoxide-demo3-paced20-forward-hw-$(PROFILE_DEMO3_PACED20_FORWARD_VISUAL_FRAMES).ppm
PROFILE_DEMO3_DISC_STREAM_VISUAL_FRAMES ?= 60
PROFILE_DEMO3_DISC_STREAM_GUEST_FRAMES ?= 720
PROFILE_DEMO3_DISC_STREAM_STEPS ?= 360000000
PROFILE_DEMO3_DISC_STREAM_HW ?= /tmp/psoxide-demo3-disc-stream-hw.ppm
PROFILE_DEMO3_DISC_STREAM_FORWARD_VISUAL_FRAMES ?= 80
PROFILE_DEMO3_DISC_STREAM_FORWARD_GUEST_FRAMES ?= 1200
PROFILE_DEMO3_DISC_STREAM_FORWARD_STEPS ?= 600000000
PROFILE_DEMO3_DISC_STREAM_FORWARD_HW ?= /tmp/psoxide-demo3-disc-stream-forward-hw.ppm
# Warp probe: read-only measurement of predicted affine texture error against
# what the depth-band subdivision rule actually decided, on real content.
# See docs/texture-warping-2026-07-27.md.
WARP_PROBE_PROJECT ?= projects/cortex_v3/project.ron
# Drop room-surface-profile for a clean A/B of stage timings: its per-stage
# cycle counters are themselves ~30% of room_surface_draw.
WARP_PROBE_FEATURES ?= cd-stream-bench room-surface-profile emulator-telemetry
WARP_PROBE_GUEST_FRAMES ?= 1200
WARP_PROBE_STEPS ?= 600000000
WARP_PROBE_LOG ?= /tmp/psoxide-warp-probe.csv
WARP_PROBE_HW ?= /tmp/psoxide-warp-probe-hw.ppm

PROFILE_DEMO7_CAMERA_SWEEP_VISUAL_FRAMES ?= 240
PROFILE_DEMO7_CAMERA_SWEEP_GUEST_FRAMES ?= 1600
PROFILE_DEMO7_CAMERA_SWEEP_STEPS ?= 600000000
PROFILE_DEMO7_CAMERA_SWEEP_HW ?= /tmp/psoxide-demo7-camera-sweep-hw.ppm
PROFILE_DEMO7_CAMERA_SWEEP_HASH_LOG ?= /tmp/psoxide-demo7-camera-sweep-visual.csv
DATA_DISC_EXAMPLES := \
	hello-tri hello-input hello-ot hello-tex hello-gte hello-audio \
	showcase-text game-pong game-breakout game-invaders \
	showcase-3d showcase-model showcase-lights showcase-fog showcase-particles \
	hello-engine hello-memcard hello-i64probe
PUBLIC_EXAMPLE_DISCS := $(addsuffix -disc,$(DATA_DISC_EXAMPLES)) hello-cdda-disc hello-pack-disc game-magikaaaaaarp-pong-disc

define build_data_disc
$(1)-disc: $(1)
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$$(EXAMPLE_OUT)/$(1).exe \
		--out ../../$$(EXAMPLE_OUT)/$(1).bin \
		--volume PSOXIDE
endef

hello-tri:
	cd sdk/examples/hello-tri && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

hello-input:
	cd sdk/examples/hello-input && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

# Non-destructive memory-card hardware diagnostic (full scan + confirmed
# write/read/power-cycle persistence test).
hello-memcard:
	cd sdk/examples/hello-memcard && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)
	cp $(EXAMPLE_OUT)/hello-memcard.exe emu/crates/frontend/assets/examples/hello-memcard.exe

# Software 64-bit integer correctness probe (signed __divdi3 is broken on-target;
# unsigned works -- see the example's docs).
hello-i64probe:
	cd sdk/examples/hello-i64probe && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

# psx-pack CD-streaming smoke test: SectorReader + load_chunk stream WORLD.PAK
# chunks off the disc (table straddle, FNV checksums, HLZC decompression).
hello-pack:
	cd sdk/examples/hello-pack && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

HELLO_PACK_FIXTURE_DIR := build/hello-pack-fixture
HELLO_PACK_STEPS ?= 400000000
HELLO_PACK_HW ?= /tmp/hello-pack.ppm
HELLO_PACK_LOG ?= /tmp/hello-pack-run.log

# 86 room_<id>.psxc chunks so the pack table spans two sectors (entry 84
# straddles the boundary); ids 84/85 carry the patterns the guest verifies.
hello-pack-fixture:
	python3 tools/hello_pack_fixture.py $(HELLO_PACK_FIXTURE_DIR)

hello-pack-disc: hello-pack hello-pack-fixture
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/hello-pack.exe \
		--out ../../$(EXAMPLE_OUT)/hello-pack.bin \
		--volume PSOXIDE \
		--world-pack-rooms-dir ../../$(HELLO_PACK_FIXTURE_DIR) \
		--world-pack-compress-rooms

# Headless end-to-end check: boot the disc, let the guest stream + verify the
# fixture chunks, then assert its TTY verdict (the dump shows the banner too).
# --embedded-playtest = no-BIOS HLE fast boot, whose A(3Ch) putchar prints to
# stdout; the default warm real-BIOS boot routes putchar into the kernel's
# dummy TTY device and the verdict line never reaches the host.
verify-hello-pack: hello-pack-disc
	cd emu && cargo run -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/hello-pack.cue \
		--embedded-playtest \
		--steps $(HELLO_PACK_STEPS) \
		--dump-hw $(HELLO_PACK_HW) \
		--dump-hash | tee $(HELLO_PACK_LOG)
	grep -q "hello-pack: ALL PASS" $(HELLO_PACK_LOG)

hello-ot:
	cd sdk/examples/hello-ot && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

# engine/ examples live outside sdk/examples/ - the engine is its
# own domain and its demos exercise the engine framework.
hello-engine:
	cd engine/examples/hello-engine && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

hello-tex: assets
	cd sdk/examples/hello-tex && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

hello-gte:
	cd sdk/examples/hello-gte && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

hello-audio:
	cd sdk/examples/hello-audio && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

hello-cdda:
	cd sdk/examples/hello-cdda && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

hello-cdda-disc: hello-cdda
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/hello-cdda.exe \
		--out ../../$(EXAMPLE_OUT)/hello-cdda.bin \
		--volume PSOXIDE \
		--cdda-track ../../$(CDDA_DEMO_TRACK)

# CD-DA + data-read contention conformance probe. The guest plays a CD-DA
# track, then issues the engine's exact read path (ReadN with no Pause/Stop)
# and records which CD-ROM IRQ the controller produces.
cdda-read-contention:
	cd sdk/examples/cdda-read-contention && $(SDK_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

cdda-read-contention-disc: cdda-read-contention
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/cdda-read-contention.exe \
		--out ../../$(EXAMPLE_OUT)/cdda-read-contention.bin \
		--volume PSOXIDE \
		--cdda-track ../../$(CDDA_DEMO_TRACK)

# Run the contention guest in PSoXide (always) and PCSX-Redux (when
# PSOXIDE_REDUX_BIN + PSOXIDE_BIOS are set) and diff the IRQ result.
showcase-text:
	cd engine/examples/showcase-text && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

game-pong:
	cd engine/examples/game-pong && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

game-magikaaaaaarp-pong:
	cd engine/examples/game-magikaaaaaarp-pong && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

magikaaaaaarp-pong-spectrum:
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

hardware-tests:
	cd engine/examples/hardware-tests && $(ENGINE_EXAMPLE_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS)

# --- hardware-test capture pipeline -------------------------------------
# The disc now boots side-effect free into its main menu. Headless capture
# selects "RUN ALL TESTS + CAPTURE" with a short Cross pulse, after which the
# suite mirrors every PX7 page to the debug TTY without QR scanning.
HWTEST_CAPTURE  := build/hwtest-capture.log
# Baselines are named by SUITE version, not by date: the suite version is what
# determines whether two captures are comparable, and re-baselining the same
# version should overwrite rather than accumulate files. The capture date lives
# in the file header.
HWTEST_SUITE := $(shell sed -n 's/^const SUITE_VERSION: &str = "HWTEST v\(.*\)";/\1/p' engine/examples/hardware-tests/src/main.rs)
HWTEST_BASELINE := docs/hardware-refs/px8-emulator-v$(HWTEST_SUITE).txt
HWTEST_STEPS    := 400000000

# Always run a source-built emulator. `cargo run` guarantees that; invoking a
# path under target/ by hand does not, and a stale binary silently produces a
# capture that describes an emulator nobody is running any more.
# Build diagnostics stay on stderr so only guest TTY output reaches the log.
# Side-loads the EXE (the HLE entry path the guest's TTY output depends on)
# while mounting the CUE with --disc, because the CD battery needs a disc in
# the drive. Against a driveless EXE every CD command burns its full poll
# budget timing out, which alone exhausts the instruction cap before the
# capture encodes; booting the CUE directly produces no guest TTY at all.
hwtest-capture: hardware-tests-disc
	@mkdir -p $(dir $(HWTEST_CAPTURE))
	cd emu && cargo run -q -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/hardware-tests.exe \
		--disc ../$(EXAMPLE_OUT)/hardware-tests.cue \
		--steps $(HWTEST_STEPS) --pad-pulses '0x4000@25+3' > ../$(HWTEST_CAPTURE)
	@echo "captured $$(grep -c 'px8' $(HWTEST_CAPTURE)) PX8 page(s) -> $(HWTEST_CAPTURE)"

HWTEST_CODE_BASELINE := docs/hardware-refs/hwtest-machine-code-v$(HWTEST_SUITE).txt

# Audit the linked EXE: the instructions between each probe's markers must be
# the ones the source asked for, or its cycle count measures something else.
hwtest-verify-code: hardware-tests
	python3 tools/verify-hwtest-machine-code.py $(EXAMPLE_OUT)/hardware-tests.exe \
		--baseline $(HWTEST_CODE_BASELINE) --fail-on-change

# CI gate: any observation, timing minimum, or precision value that moves
# against the baseline fails the build and is named in the output.
hwtest-diff: hwtest-verify-code hwtest-capture
	python3 tools/hwtest-report.py --baseline $(HWTEST_BASELINE) \
		--fail-on-change $(HWTEST_CAPTURE)

HWTEST_WAV        := build/hwtest-audio.wav
HWTEST_AUDIO_PAGES := build/hwtest-audio-pages.txt

# End-to-end check of the audio readout: the disc streams the whole payload as
# FSK and loops it, so this records the emulator's SPU output, decodes it back,
# and runs the recovered bytes through the SAME report pipeline a scanned QR
# capture uses. Needs a longer run than hwtest-capture because the payload
# takes ~11.6 s of audio to transmit once.
hwtest-audio: hardware-tests-disc
	@mkdir -p $(dir $(HWTEST_WAV))
	cd emu && cargo run -q -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/hardware-tests.exe \
		--disc ../$(EXAMPLE_OUT)/hardware-tests.cue \
		--steps 1200000000 --pad-pulses '0x4000@25+3,0x8000@1900+6' \
		--dump-audio ../$(HWTEST_WAV) > /dev/null
	python3 tools/hwtest-audio-decode.py $(HWTEST_WAV) --emit-pages $(HWTEST_AUDIO_PAGES)
	python3 tools/hwtest-report.py $(HWTEST_AUDIO_PAGES) > /dev/null
	@echo "audio link OK: payload recovered from audio and parsed as PX7"

# Robustness matrix for the audio link. A clean emulator recording proves the
# encoding; this degrades it the way a real capture chain does (resampling to
# 48/32/96 kHz, 20x gain range, clipping, DC offset, band-limiting, noise) and
# requires the decoder to still recover the identical payload. It cannot prove
# the real chain works, but it does stop the decoder being brittle against the
# damage a chain is known to introduce.
hwtest-audio-chain: hwtest-audio
	python3 tools/hwtest-audio-chaintest.py $(HWTEST_WAV)

# Deliberate re-baseline. Review the hwtest-diff output BEFORE running this:
# it overwrites the reference every later run is judged against.
hwtest-baseline: hwtest-capture
	@{ \
		echo "# PSoXide hardware-test capture baseline"; \
		echo "#"; \
		echo "# SOURCE: PSoXide EMULATOR, headless. This is NOT a silicon capture."; \
		echo "#   It detects emulator-side drift only. It is not hardware truth and"; \
		echo "#   must never be cited as a console measurement."; \
		echo "#"; \
		echo "# captured:  $$(date -u +%Y-%m-%d)"; \
		echo "# git:       $$(git describe --always --dirty)"; \
		echo "# guest exe: sha256:$$(shasum -a 256 $(EXAMPLE_OUT)/hardware-tests.exe | cut -c1-16)"; \
		echo "# emulator:  frontend launch --steps $(HWTEST_STEPS) (menu Cross pulse)"; \
		echo "# schema:    PX8 conformance, $$(grep -c 'px8' $(HWTEST_CAPTURE)) page(s)"; \
		echo "#"; \
		grep 'px8' $(HWTEST_CAPTURE) | sed 's/^hardware-tests: px8 //'; \
	} > $(HWTEST_BASELINE)
	@echo "re-baselined $(HWTEST_BASELINE)"

# Ingest a real console capture. Pass the OBS-decoded payload text:
#   make hwtest-silicon SILICON=captures/scph9902-2026-07-25.txt
# This is the comparison that actually matters; the emulator baseline above
# only guards against our own drift.
hwtest-silicon: hwtest-capture
	@test -n "$(SILICON)" || { echo "usage: make hwtest-silicon SILICON=<payload.txt>"; exit 2; }
	python3 tools/hwtest-report.py --baseline $(SILICON) $(HWTEST_CAPTURE)

# PA5 snapshots BIOS reverb state and selects reset variants before PA4's
# proven map-DMA trigger. PA3/PA2 retain prior fixtures and PA1 reads a deterministic 600-sector
# CDTEST.BIN at LBA 424.
# Keep the file explicit so every burn and emulator run uses identical inputs.
# The CD-DA track is required, not decorative: read-while-CD-DA contention is
# the one CD failure no emulator reproduces, and it cannot be probed without a
# real audio track on the disc. The tone is synthesised (tools/gen-cdda-tone.py)
# so the image stays redistributable and bit-reproducible.
HWTEST_CDDA := $(EXAMPLE_OUT)/hardware-tests-cdda.pcm

$(HWTEST_CDDA):
	@mkdir -p $(dir $@)
	python3 tools/gen-cdda-tone.py --seconds 10 --out $@

hardware-tests-disc: hardware-tests $(HWTEST_CDDA)
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/hardware-tests.exe \
		--out ../../$(EXAMPLE_OUT)/hardware-tests.bin \
		--volume PSOXIDE \
		--cdtest-sectors 500 \
		--cdda-track ../../$(HWTEST_CDDA)

$(foreach example,$(DATA_DISC_EXAMPLES),$(eval $(call build_data_disc,$(example))))

# Cook a project into editor-playtest/generated/. With no
# arguments cooks the embedded starter project; pass
# `PROJECT=<path/to/project.ron>` to cook a specific one.
# This target is **destructive** for ignored cooked outputs:
# it overwrites the cooked manifest/assets in generated/.
# Don't run it after the editor's Play action unless you want
# the editor's output replaced.
cook-playtest:
	cd editor && cargo run --release -p psxed-project --bin cook-playtest -- $(PROJECT)

# Build the editor-playtest example against whatever is in
# `generated/level_manifest.cooked.rs` if present, otherwise
# the tracked placeholder. Does NOT recook - that's the editor's
# job (or `make cook-playtest` if you want the starter). The playtest runtime is
# streaming-only, so the default build includes the CD streaming reader.
EDITOR_PLAYTEST_FEATURES ?= cd-stream-bench
EDITOR_PLAYTEST_CARGO_FEATURE_FLAGS ?= --features "$(EDITOR_PLAYTEST_FEATURES)"
EDITOR_PLAYTEST_HARDWARE_FEATURES ?= cd-stream-bench world-order-bucketed world-grid-visible ot-2048 vis-anchor-pvs-candidates

build-editor-playtest:
	cd engine/examples/editor-playtest && $(EDITOR_PLAYTEST_CARGO_ENV) cargo build --release $(PSX_BUILD_FLAGS) $(EDITOR_PLAYTEST_CARGO_FEATURE_FLAGS)

profile-demo3:
	$(MAKE) profile-demo3-disc-stream PROFILE_DEMO3_DISC_STREAM_HW=$(PROFILE_DEMO3_HW)

profile-demo3-forward:
	$(MAKE) profile-demo3-disc-stream-forward PROFILE_DEMO3_DISC_STREAM_FORWARD_HW=$(PROFILE_DEMO3_FORWARD_HW)

profile-demo3-paced20:
	$(MAKE) profile-demo3-disc-stream PROFILE_DEMO3_DISC_STREAM_HW=$(PROFILE_DEMO3_PACED20_HW)

profile-demo3-paced20-forward:
	$(MAKE) profile-demo3-disc-stream-forward PROFILE_DEMO3_DISC_STREAM_FORWARD_HW=$(PROFILE_DEMO3_PACED20_FORWARD_HW)

profile-demo3-disc-stream:
	$(MAKE) cook-playtest PROJECT=projects/demo_03/project.ron
	$(MAKE) build-editor-playtest
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
		--ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
		--ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
		--cdda-track-list $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/cdda_tracks.txt
	cd emu && cargo run -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/editor-playtest.cue \
		--embedded-playtest \
		--guest-visual-frames $(PROFILE_DEMO3_DISC_STREAM_VISUAL_FRAMES) \
		--guest-frames $(PROFILE_DEMO3_DISC_STREAM_GUEST_FRAMES) \
		--steps $(PROFILE_DEMO3_DISC_STREAM_STEPS) \
		--dump-hw $(PROFILE_DEMO3_DISC_STREAM_HW) \
		--dump-hash \
		--dump-guest-profile

profile-demo3-disc-stream-forward:
	$(MAKE) cook-playtest PROJECT=projects/demo_03/project.ron
	$(MAKE) build-editor-playtest
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
		--ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
		--ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
		--cdda-track-list $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/cdda_tracks.txt
	cd emu && cargo run -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/editor-playtest.cue \
		--embedded-playtest \
		--guest-visual-frames $(PROFILE_DEMO3_DISC_STREAM_FORWARD_VISUAL_FRAMES) \
		--guest-frames $(PROFILE_DEMO3_DISC_STREAM_FORWARD_GUEST_FRAMES) \
		--steps $(PROFILE_DEMO3_DISC_STREAM_FORWARD_STEPS) \
		--hold-forward \
		--dump-hw $(PROFILE_DEMO3_DISC_STREAM_FORWARD_HW) \
		--dump-hash \
		--dump-guest-profile

profile-demo7-camera-sweep:
	$(MAKE) cook-playtest PROJECT=projects/demo_07/project.ron
	PSXO_CAMERA_SWEEP=1 PSXO_PROFILE_MODELS=1 $(MAKE) build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench room-surface-profile"
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
		--ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
		--ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
		--cdda-track-list $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/cdda_tracks.txt
	cd emu && cargo run -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/editor-playtest.cue \
		--embedded-playtest \
		--guest-visual-frames $(PROFILE_DEMO7_CAMERA_SWEEP_VISUAL_FRAMES) \
		--guest-frames $(PROFILE_DEMO7_CAMERA_SWEEP_GUEST_FRAMES) \
		--steps $(PROFILE_DEMO7_CAMERA_SWEEP_STEPS) \
		--dump-hw $(PROFILE_DEMO7_CAMERA_SWEEP_HW) \
		--visual-hash-log $(PROFILE_DEMO7_CAMERA_SWEEP_HASH_LOG) \
		--visual-hash-interval 30 \
		--dump-hash \
		--dump-guest-profile

# Read-only: changes no geometry, emits counters only. Answers whether the
# closed-form warp criterion would actually cut primitives on real rooms, or
# whether cortex's content happens to make the depth-band rule good enough.
probe-warp:
	$(MAKE) cook-playtest PROJECT=$(WARP_PROBE_PROJECT)
	$(MAKE) build-editor-playtest EDITOR_PLAYTEST_FEATURES="$(WARP_PROBE_FEATURES)"
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
		--ui-pack-dir ../../engine/examples/editor-playtest/generated/ui_stream_chunks \
		--ui-pack-order-file ../../engine/examples/editor-playtest/generated/ui_pack_order.txt \
		--cdda-track-list $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/cdda_tracks.txt
	cd emu && cargo run -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/editor-playtest.cue \
		--embedded-playtest \
		--guest-frames $(WARP_PROBE_GUEST_FRAMES) \
		--steps $(WARP_PROBE_STEPS) \
		--hold-forward \
		--profile-log $(WARP_PROBE_LOG) \
		--dump-hw $(WARP_PROBE_HW) \
		--dump-hash

# --- Content pipeline (host-side editor tooling) ------------------------

PSXED := target/release/psxed

# Build the content-pipeline CLI. Independent host workspace -
# always builds fast, no MIPS toolchain needed.
psxed:
	cd editor && cargo build --release --bin psxed

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

examples: $(PUBLIC_EXAMPLE_DISCS)
	@echo ""
	@echo "Built public example discs:"
	@find $(EXAMPLE_OUT) -maxdepth 1 -type f \( -name '*.cue' -o -name '*.bin' \) ! -name 'editor-playtest.*' -print | sort | while IFS= read -r disc; do ls -la "$$disc"; done

# Frontend disc helpers. Public examples boot from CUE/BIN so the same
# artifact can be launched in emulators or burned to CD-R.

run-tri: hello-tri-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-tri.cue cargo run -p frontend --release

run-input: hello-input-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-input.cue cargo run -p frontend --release

run-ot: hello-ot-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-ot.cue cargo run -p frontend --release

run-tex: hello-tex-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-tex.cue cargo run -p frontend --release

run-gte: hello-gte-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-gte.cue cargo run -p frontend --release

run-audio: hello-audio-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-audio.cue cargo run -p frontend --release

run-cdda: hello-cdda-disc
	cd emu && PSOXIDE_AUTORUN=1 PSOXIDE_AUDIO_TRACE=1 PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-cdda.cue cargo run -p frontend --release

probe-cdda-audio: hello-cdda-disc
	cd emu && PSOXIDE_EXE=$(CURDIR)/$(EXAMPLE_OUT)/hello-cdda.exe PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-cdda.cue cargo run -p emulator-core --example probe_cdda_wav --release

run-showcase-text: showcase-text-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/showcase-text.cue cargo run -p frontend --release

run-game-pong: game-pong-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/game-pong.cue cargo run -p frontend --release

run-game-magikaaaaaarp-pong: game-magikaaaaaarp-pong-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.cue cargo run -p frontend --release

probe-magikaaaaaarp-pong-audio: game-magikaaaaaarp-pong-disc
	cd emu && PSOXIDE_EXE=$(CURDIR)/$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.exe PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.cue PSOXIDE_WAV=/tmp/psoxide_magikaaaaaarp_pong.wav PSOXIDE_AUDIO_SECONDS=6 cargo run -p emulator-core --example probe_cdda_wav --release

duckstation-magikaaaaaarp-pong: game-magikaaaaaarp-pong-disc
	$(PSOXIDE_DEV) duckstation-harness \
		--cue $(CURDIR)/$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.cue \
		--timeout $(DUCKSTATION_TIMEOUT) \
		--log $(CURDIR)/$(DUCKSTATION_MAGIKARP_LOG)

cortex-ignition-v1-project-disc:
	cd emu && cargo run -p frontend --release -- build-project-disc --project ../$(CORTEX_IGNITION_V1_PROJECT)

cortex-ignition-v1-project-disc-boot-trace:
	cd emu && EDITOR_PLAYTEST_FEATURES='cd-stream-bench boot-trace' cargo run -p frontend --release -- build-project-disc --project ../$(CORTEX_IGNITION_V1_PROJECT)

cortex-ignition-v1-hardware-diagnostic-disc:
	cd emu && EDITOR_PLAYTEST_CARGO_FEATURE_FLAGS='--no-default-features --features "$(EDITOR_PLAYTEST_HARDWARE_FEATURES) hardware-boot-visual"' cargo run -p frontend --release -- build-project-disc --project ../$(CORTEX_IGNITION_V1_PROJECT)

cortex-ignition-v1-preburn-local: cortex-ignition-v1-preburn-struct cortex-ignition-v1-preburn-disc-reads cortex-ignition-v1-preburn-internal cortex-ignition-v1-preburn-cdda-audio cortex-ignition-v1-preburn-bios-cdrom cortex-ignition-v1-preburn-boot-flow cortex-ignition-v1-preburn-streaming-guard
	@echo "cortex_ignition_v1 pre-burn local checks complete -> $(CORTEX_IGNITION_V1_PREBURN_OUT)"

cortex-ignition-v1-preburn-struct: cortex-ignition-v1-project-disc
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT)
	cd emu && cargo run -p frontend --release -- preburn-check \
		--cue "$(CORTEX_IGNITION_V1_CUE)" \
		--exe "$(EXAMPLE_OUT)/editor-playtest.exe" \
		--volume $(CORTEX_IGNITION_V1_VOLUME) \
		--require-file "SYSTEM.CNF;1" \
		--require-file "PSX.EXE;1" \
		--require-file "WORLD.PAK;1" \
		--require-file "UI.PAK;1" \
		--require-audio-track \
		--forbid-exe-string "UI DRAW OK" \
		--forbid-exe-string "PRESENT OK" \
		--forbid-exe-string "RENDER BEGIN" \
		--forbid-exe-string "psx-engine:" \
		--forbid-exe-string "psx-rt:" \
		--forbid-exe-string "editor-playtest:"

cortex-ignition-v1-preburn-disc-reads: cortex-ignition-v1-project-disc
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT)
	@(cd emu && PSOXIDE_DISC="../$(CORTEX_IGNITION_V1_BIN)" \
		cargo run -p emulator-core --example verify_disc_reads --release) > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/disc-reads.log" 2>&1; \
	status=$$?; cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/disc-reads.log"; exit $$status

cortex-ignition-v1-preburn-internal:
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT) $(CORTEX_IGNITION_V1_PREBURN_INTERNAL_DISC_DIR)
	cd emu && EDITOR_PLAYTEST_FEATURES='$(CORTEX_IGNITION_V1_PREBURN_FEATURES)' cargo run -p frontend --release -- build-project-disc --project ../$(CORTEX_IGNITION_V1_PROJECT)
	@cp "$(CORTEX_IGNITION_V1_CUE)" "$(CORTEX_IGNITION_V1_PREBURN_INTERNAL_CUE)"
	@cp "$(CORTEX_IGNITION_V1_BIN)" "$(CORTEX_IGNITION_V1_PREBURN_INTERNAL_BIN)"
	@$(MAKE) cortex-ignition-v1-project-disc
	@(cd emu && cargo run -p frontend --release -- launch \
		--path ../$(CORTEX_IGNITION_V1_PREBURN_INTERNAL_CUE) \
		--embedded-playtest \
		--pad-pulses "$(CORTEX_IGNITION_V1_PREBURN_PAD_PULSES)" \
		--guest-visual-frames $(CORTEX_IGNITION_V1_PREBURN_VISUAL_FRAMES) \
		--guest-frames $(CORTEX_IGNITION_V1_PREBURN_GUEST_FRAMES) \
		--steps $(CORTEX_IGNITION_V1_PREBURN_STEPS) \
		--dump-hw ../$(CORTEX_IGNITION_V1_PREBURN_OUT)/internal-hle.ppm \
		--dump-audio ../$(CORTEX_IGNITION_V1_PREBURN_OUT)/internal-hle.wav \
		--visual-hash-log ../$(CORTEX_IGNITION_V1_PREBURN_OUT)/visual-hashes.csv \
		--guest-hash-log ../$(CORTEX_IGNITION_V1_PREBURN_OUT)/guest-hashes.csv \
		--counter-log ../$(CORTEX_IGNITION_V1_PREBURN_OUT)/counters.csv \
		--profile-log ../$(CORTEX_IGNITION_V1_PREBURN_OUT)/profile.csv \
		--dump-hash \
		--dump-guest-profile) > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/internal-hle.log" 2>&1; \
	status=$$?; cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/internal-hle.log"; exit $$status

cortex-ignition-v1-preburn-cdda-audio: cortex-ignition-v1-project-disc
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT)
	@(cd emu && PSOXIDE_EXE="../$(EXAMPLE_OUT)/editor-playtest.exe" \
		PSOXIDE_DISC="../$(CORTEX_IGNITION_V1_CUE)" \
		PSOXIDE_WAV="../$(CORTEX_IGNITION_V1_PREBURN_OUT)/cdda-probe.wav" \
		PSOXIDE_AUDIO_SECONDS="$(CORTEX_IGNITION_V1_PREBURN_AUDIO_SECONDS)" \
		PSOXIDE_MIN_PEAK="$(CORTEX_IGNITION_V1_PREBURN_AUDIO_MIN_PEAK)" \
		cargo run -p emulator-core --example probe_cdda_wav --release) > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/cdda-probe.log" 2>&1; \
	status=$$?; cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/cdda-probe.log"; exit $$status

cortex-ignition-v1-preburn-bios-cdrom: cortex-ignition-v1-project-disc
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT)
	@rm -f "$(CORTEX_IGNITION_V1_PREBURN_OUT)/bios-cdrom-probe.log"
	@if [ -f "$(REDUX_CORTEX_IGNITION_V1_BIOS)" ]; then \
		(cd emu && PSOXIDE_BIOS="$(REDUX_CORTEX_IGNITION_V1_BIOS)" \
			PSOXIDE_DISC="../$(CORTEX_IGNITION_V1_BIN)" \
			cargo run -p emulator-core --example cdrom_probe --release -- $(CORTEX_IGNITION_V1_PREBURN_BIOS_STEPS)) > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/bios-cdrom-probe.log" 2>&1; \
		status=$$?; cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/bios-cdrom-probe.log"; exit $$status; \
	else \
		echo "skip BIOS CD-ROM probe: REDUX_CORTEX_IGNITION_V1_BIOS not found ($(REDUX_CORTEX_IGNITION_V1_BIOS))" > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/bios-cdrom-probe.log"; \
		cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/bios-cdrom-probe.log"; \
	fi

cortex-ignition-v1-preburn-boot-flow: cortex-ignition-v1-project-disc
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT)
	@rm -f "$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.log"
	@if [ -f "$(REDUX_CORTEX_IGNITION_V1_BIOS)" ]; then \
		(cd emu && PSOXIDE_BIOS="$(REDUX_CORTEX_IGNITION_V1_BIOS)" \
			PSOXIDE_DISC="../$(CORTEX_IGNITION_V1_CUE)" \
			PSOXIDE_PAD1="$(CORTEX_IGNITION_V1_PREBURN_BOOT_FLOW_PAD1)" \
			PSOXIDE_PAD1_PULSES="$(CORTEX_IGNITION_V1_PREBURN_BOOT_FLOW_PULSES)" \
			PSOXIDE_VISIBLE_DUMP="../$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.ppm" \
			PSOXIDE_REQUIRE_CDDA=1 \
			PSOXIDE_REQUIRE_CDROM_READS=1 \
			PSOXIDE_MIN_PEAK="$(CORTEX_IGNITION_V1_PREBURN_AUDIO_MIN_PEAK)" \
			cargo run -p emulator-core --example probe_disc_pad_trace --release -- $(CORTEX_IGNITION_V1_PREBURN_BOOT_FLOW_STEPS)) > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.log" 2>&1; \
		status=$$?; cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.log"; exit $$status; \
	else \
		echo "skip BIOS boot-flow probe: REDUX_CORTEX_IGNITION_V1_BIOS not found ($(REDUX_CORTEX_IGNITION_V1_BIOS))" > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.log"; \
		cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.log"; \
	fi

cortex-ignition-v1-preburn-streaming-guard: cortex-ignition-v1-preburn-internal cortex-ignition-v1-preburn-cdda-audio cortex-ignition-v1-preburn-boot-flow
	@mkdir -p $(CORTEX_IGNITION_V1_PREBURN_OUT)
	@($(PSOXIDE_DEV) cortex-stream-guard \
		--profile $(CURDIR)/$(CORTEX_IGNITION_V1_PREBURN_OUT)/profile.csv \
		--cdda-log $(CURDIR)/$(CORTEX_IGNITION_V1_PREBURN_OUT)/cdda-probe.log \
		--boot-flow-log $(CURDIR)/$(CORTEX_IGNITION_V1_PREBURN_OUT)/boot-flow.log) > "$(CORTEX_IGNITION_V1_PREBURN_OUT)/streaming-guard.log" 2>&1; \
	status=$$?; cat "$(CORTEX_IGNITION_V1_PREBURN_OUT)/streaming-guard.log"; exit $$status

cortex-ignition-v1-emulator-inventory:
	$(PSOXIDE_DEV) emulator-inventory

cortex-ignition-v1-external-emulators: duckstation-cortex-ignition-v1-bios mednafen-cortex-ignition-v1-bios retroarch-cortex-ignition-v1-bios ares-cortex-ignition-v1-bios
	@echo "cortex_ignition_v1 external emulator matrix complete"

cortex-ignition-v1-bringup-report:
	$(PSOXIDE_DEV) cortex-bringup-report \
		--out $(CURDIR)/$(CORTEX_IGNITION_V1_BRINGUP_REPORT) \
		--preburn-dir $(CURDIR)/$(CORTEX_IGNITION_V1_PREBURN_OUT) \
		--duckstation-log $(CURDIR)/$(DUCKSTATION_CORTEX_IGNITION_V1_BIOS_LOG) \
		--external-dir $(CURDIR)/build/external-emulator-smoke

cortex-ignition-v1-burn-candidate: cortex-ignition-v1-preburn-local cortex-ignition-v1-external-emulators
	$(PSOXIDE_DEV) cortex-bringup-report \
		--out $(CURDIR)/$(CORTEX_IGNITION_V1_BRINGUP_REPORT) \
		--preburn-dir $(CURDIR)/$(CORTEX_IGNITION_V1_PREBURN_OUT) \
		--duckstation-log $(CURDIR)/$(DUCKSTATION_CORTEX_IGNITION_V1_BIOS_LOG) \
		--external-dir $(CURDIR)/build/external-emulator-smoke \
		--fail-on-warn
	@echo "cortex_ignition_v1 burn candidate passed -> $(CORTEX_IGNITION_V1_BRINGUP_REPORT)"

duckstation-cortex-ignition-v1: cortex-ignition-v1-project-disc-boot-trace
	$(PSOXIDE_DEV) duckstation-harness \
		--cue $(CURDIR)/$(CORTEX_IGNITION_V1_CUE) \
		--timeout $(DUCKSTATION_TIMEOUT) \
		--log $(CURDIR)/$(DUCKSTATION_CORTEX_IGNITION_V1_LOG) \
		--no-default-expect \
		--expect "psx-rt: main" \
		--expect "editor-playtest: init ok" \
		--expect "psx-engine: scene init ok" \
		--expect "psx-engine: cdda setmode ok" \
		--expect "psx-engine: cdda demute ok" \
		--expect "psx-engine: cdda play ok"

duckstation-cortex-ignition-v1-bios: cortex-ignition-v1-project-disc-boot-trace
	$(PSOXIDE_DEV) duckstation-harness \
		--cue $(CURDIR)/$(CORTEX_IGNITION_V1_CUE) \
		--timeout $(DUCKSTATION_TIMEOUT) \
		--log $(CURDIR)/$(DUCKSTATION_CORTEX_IGNITION_V1_BIOS_LOG) \
		--bios-boot \
		--no-default-expect \
		--expect "psx-rt: main" \
		--expect "editor-playtest: init ok" \
		--expect "psx-engine: scene init ok" \
		--expect "psx-engine: cdda setmode ok" \
		--expect "psx-engine: cdda demute ok" \
		--expect "psx-engine: cdda play ok"

mednafen-cortex-ignition-v1-bios: cortex-ignition-v1-project-disc
	@if $(PSOXIDE_DEV) emulator-inventory --require mednafen >/dev/null 2>&1; then \
		$(PSOXIDE_DEV) external-emulator-smoke \
			--emulator mednafen \
			--cue $(CURDIR)/$(CORTEX_IGNITION_V1_CUE) \
			--bios "$(REDUX_CORTEX_IGNITION_V1_BIOS)" \
			--timeout $(EXTERNAL_EMULATOR_SMOKE_TIMEOUT) \
			--log $(CURDIR)/$(MEDNAFEN_CORTEX_IGNITION_V1_LOG); \
	else \
		echo "skip Mednafen cortex_ignition_v1 smoke: emulator unavailable"; \
		$(PSOXIDE_DEV) emulator-inventory; \
	fi

retroarch-cortex-ignition-v1-bios: cortex-ignition-v1-project-disc
	@if $(PSOXIDE_DEV) emulator-inventory --require retroarch >/dev/null 2>&1; then \
		$(PSOXIDE_DEV) external-emulator-smoke \
			--emulator retroarch \
			--cue $(CURDIR)/$(CORTEX_IGNITION_V1_CUE) \
			--bios "$(REDUX_CORTEX_IGNITION_V1_BIOS)" \
			--timeout $(EXTERNAL_EMULATOR_SMOKE_TIMEOUT) \
			--log $(CURDIR)/$(RETROARCH_CORTEX_IGNITION_V1_LOG) \
			--screenshot $(CURDIR)/$(RETROARCH_CORTEX_IGNITION_V1_SCREENSHOT) \
			--screenshot-frames $(RETROARCH_CORTEX_IGNITION_V1_SCREENSHOT_FRAMES) \
			--fail-on "Firmware is missing" \
			--fail-on "Failed to load content"; \
	else \
		echo "skip RetroArch cortex_ignition_v1 smoke: emulator/core unavailable"; \
		$(PSOXIDE_DEV) emulator-inventory; \
	fi

ares-cortex-ignition-v1-bios: cortex-ignition-v1-project-disc
	@if $(PSOXIDE_DEV) emulator-inventory --require ares >/dev/null 2>&1; then \
		$(PSOXIDE_DEV) external-emulator-smoke \
			--emulator ares \
			--cue $(CURDIR)/$(CORTEX_IGNITION_V1_CUE) \
			--bios "$(REDUX_CORTEX_IGNITION_V1_BIOS)" \
			--timeout $(EXTERNAL_EMULATOR_SMOKE_TIMEOUT) \
			--log $(CURDIR)/$(ARES_CORTEX_IGNITION_V1_LOG); \
	else \
		echo "skip ares cortex_ignition_v1 smoke: emulator unavailable"; \
		$(PSOXIDE_DEV) emulator-inventory; \
	fi

run-game-breakout: game-breakout-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/game-breakout.cue cargo run -p frontend --release

run-game-invaders: game-invaders-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/game-invaders.cue cargo run -p frontend --release

run-showcase-3d: showcase-3d-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/showcase-3d.cue cargo run -p frontend --release

run-showcase-model: showcase-model-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/showcase-model.cue cargo run -p frontend --release

run-showcase-lights: showcase-lights-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/showcase-lights.cue cargo run -p frontend --release

run-showcase-fog: showcase-fog-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/showcase-fog.cue cargo run -p frontend --release

run-showcase-particles: showcase-particles-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/showcase-particles.cue cargo run -p frontend --release

run-hardware-tests: hardware-tests-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hardware-tests.cue cargo run -p frontend --release

run-hello-engine: hello-engine-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-engine.cue cargo run -p frontend --release

# ---------------------------------------------------------------------------
# cortex_anim: the AI-generated locomotion/attack pack on Aletha.
#
# `cortex-anim-disc` cooks the project and burns the playtest ISO. The UI-pack
# flags are NOT optional: persistent gameplay assets live in that pack, and a
# disc built without them stalls on the loading screen with zero sectors read.
#
# `cortex-anim-shots` drives the disc headlessly through the menu and fires the
# whole moveset, writing one PNG-able PPM per CORTEX_ANIM_SHOT_INTERVAL ticks.
# Gameplay starts at route tick ~480; the press script is keyed to that.
CORTEX_ANIM_PROJECT ?= projects/cortex_anim/project.ron
CORTEX_ANIM_SHOT_DIR ?= /tmp/cortex-anim-shots
CORTEX_ANIM_SHOT_INTERVAL ?= 3
CORTEX_ANIM_STEPS ?= 450000000
# tick:button[:hold] -- menu, then walk, run, light (R1), heavy (R2), combo (L2).
CORTEX_ANIM_PRESS ?= 120:cross,200:cross,300:cross,420:cross,540:up:70,760:up:70,760:circle:70,980:r1:6,1150:r2:6,1430:l2:6

cortex-anim-disc:
	$(MAKE) cook-playtest PROJECT=$(CORTEX_ANIM_PROJECT)
	$(MAKE) build-editor-playtest
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/stream_chunks \
		--world-pack-order-file $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/world_pack_order.txt \
		--ui-pack-dir $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/ui_stream_chunks \
		--ui-pack-order-file $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/ui_pack_order.txt \
		--cdda-track-list $(EDITOR_PLAYTEST_GENERATED_FROM_MKISOPSX)/cdda_tracks.txt

cortex-anim-shots: cortex-anim-disc
	rm -rf $(CORTEX_ANIM_SHOT_DIR) && mkdir -p $(CORTEX_ANIM_SHOT_DIR)
	cd emu && cargo run -p frontend --release -- launch \
		--path ../$(EXAMPLE_OUT)/editor-playtest.cue \
		--embedded-playtest \
		--steps $(CORTEX_ANIM_STEPS) \
		--press "$(CORTEX_ANIM_PRESS)" \
		--route-screenshot-dir $(CORTEX_ANIM_SHOT_DIR) \
		--route-screenshot-interval $(CORTEX_ANIM_SHOT_INTERVAL)
	@echo "[cortex-anim-shots] wrote $$(ls $(CORTEX_ANIM_SHOT_DIR) | wc -l | tr -d ' ') frames to $(CORTEX_ANIM_SHOT_DIR)"
