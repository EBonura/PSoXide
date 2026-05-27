# PSoXide development commands.
#
# Cargo workspaces:
#   root   - no_std shared crates (psx-hw, psx-iso, psx-trace)
#   editor - host-side editor/content pipeline crates
#   emu    - host-side emulator/frontend/parity crates
#   engine - PSX runtime engine crates
#   sdk    - MIPS target SDK crates
#
# Standalone tool crates live under tools/* and are gated explicitly.
#
# SDK examples live under sdk/examples/ and are compiled individually
# with cargo build in their own directory so they can use their own
# .cargo/config.toml for the mipsel-sony-psx target.

.PHONY: help check test canaries fmt lint lint-policy-guard runtime-numeric-guard clean fetch-opcode oracle-smoke oracle-side-load oracle-disc-smoke commercial-visual-guards tekken-mode-guard tekken-vs-guard tekken-fight-guard tekken-late-fight-guard parity run \
        test-sdk \
        psxed assets \
	examples hello-tri hello-tri-disc hello-input hello-input-disc hello-ot hello-ot-disc \
	hello-tex hello-tex-disc hello-gte hello-gte-disc hello-audio hello-audio-disc \
	hello-cdda hello-cdda-disc \
	run-tri run-input run-ot run-tex run-gte run-audio run-cdda probe-cdda-audio \
	showcase-text showcase-text-disc run-showcase-text \
	game-pong game-pong-disc run-game-pong \
	game-magikaaaaaarp-pong game-magikaaaaaarp-pong-disc magikaaaaaarp-pong-spectrum run-game-magikaaaaaarp-pong probe-magikaaaaaarp-pong-audio duckstation-magikaaaaaarp-pong \
	game-breakout game-breakout-disc run-game-breakout \
        game-invaders game-invaders-disc run-game-invaders \
        showcase-3d showcase-3d-disc run-showcase-3d \
        showcase-model showcase-model-disc run-showcase-model \
        showcase-lights showcase-lights-disc run-showcase-lights \
	showcase-fog showcase-fog-disc run-showcase-fog \
	showcase-particles showcase-particles-disc run-showcase-particles \
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
	@echo "    make run          - launch the desktop frontend (no EXE)"
	@echo "    make parity       - step both emulators and assert bit-identical traces"
	@echo "    make oracle-smoke - smoke: launch headless Redux and verify Lua runs"
	@echo "    make oracle-side-load - compare side-loaded SDK EXEs against Redux"
	@echo "    make oracle-disc-smoke - compare a local PSOXIDE_DISC checkpoint against Redux"
	@echo "    make commercial-visual-guards - run all local commercial visual guards"
	@echo "    make tekken-mode-guard - assert Tekken 3 mode-select coverage"
	@echo "    make tekken-vs-guard - assert Tekken 3 VS portrait coverage"
	@echo "    make tekken-fight-guard - assert Tekken 3 early-fight HUD/stage/fighter coverage"
	@echo "    make tekken-late-fight-guard - assert Tekken 3 late-fight sky/fighter coverage"
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

run:
	cd emu && cargo run -p frontend --release

check:
	cargo check --workspace --all-features
	cd editor && cargo check --workspace --all-features
	cd emu && cargo check --workspace --all-features
	cd engine && cargo check --workspace --all-features
	cd sdk && cargo check --workspace --all-features
	cd tools/mkisopsx && cargo check
	cd tools/psx-exe-pack && cargo check

test:
	cargo test --workspace
	cd editor && cargo test --workspace
	cd emu && cargo test --workspace
	cd engine && cargo test --workspace
	cd sdk && cargo test --workspace
	cd tools/mkisopsx && cargo test
	cd tools/psx-exe-pack && cargo test

canaries:
	cargo test --workspace -- --ignored
	cd emu && cargo test --workspace -- --ignored

fmt:
	cargo fmt --all
	cd editor && cargo fmt --all
	cd emu && cargo fmt --all
	cd engine && cargo fmt --all
	cd sdk && cargo fmt --all
	cd tools/mkisopsx && cargo fmt --all
	cd tools/psx-exe-pack && cargo fmt --all

lint:
	python3 tools/lint_policy_guard.py
	python3 tools/runtime_numeric_guard.py
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd editor && cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd emu && cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd engine && cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd sdk && cargo clippy --workspace --all-targets --all-features -- -D warnings
	cd tools/mkisopsx && cargo clippy --all-targets --all-features -- -D warnings
	cd tools/psx-exe-pack && cargo clippy --all-targets --all-features -- -D warnings

lint-policy-guard:
	python3 tools/lint_policy_guard.py

runtime-numeric-guard:
	python3 tools/runtime_numeric_guard.py

clean:
	cargo clean
	cd editor && cargo clean
	cd emu && cargo clean
	cd engine && cargo clean
	cd sdk && cargo clean
	cd tools/mkisopsx && cargo clean
	cd tools/psx-exe-pack && cargo clean
	rm -rf build

fetch-opcode:
	@if [ -z "$(BIOS)" ]; then echo "usage: make fetch-opcode BIOS=/path/to/bios.bin"; exit 2; fi
	cd emu && cargo run -p emulator-core --example fetch_first_opcode -- "$(BIOS)"

oracle-smoke:
	cd emu && cargo test -p parity-oracle --test smoke -- --ignored --nocapture

oracle-side-load: examples
	cd emu && cargo test -p parity-oracle --test side_loaded_exe --release -- --ignored --nocapture

oracle-disc-smoke:
	cd emu && cargo test -p parity-oracle --test commercial_disc_smoke -- --ignored --nocapture

commercial-visual-guards:
	cd emu && cargo run -p emulator-core --example commercial_visual_guard --release -- \
		--all \
		--out-dir $${PSOXIDE_VISUAL_GUARD_OUT:-/tmp/psoxide-commercial-guards}

tekken-mode-guard:
	cd emu && cargo run -p emulator-core --example commercial_visual_guard --release -- \
		--guard tekken3-mode-select \
		--out-dir $${PSOXIDE_TEKKEN_MODE_GUARD_OUT:-/tmp/tekken_mode_guard}

tekken-vs-guard:
	cd emu && cargo run -p emulator-core --example commercial_visual_guard --release -- \
		--guard tekken3-vs-portrait \
		--out-dir $${PSOXIDE_TEKKEN_GUARD_OUT:-/tmp/tekken_owner_guard}

tekken-fight-guard:
	cd emu && cargo run -p emulator-core --example commercial_visual_guard --release -- \
		--guard tekken3-early-fight \
		--out-dir $${PSOXIDE_TEKKEN_FIGHT_GUARD_OUT:-/tmp/tekken_fight_guard}

tekken-late-fight-guard:
	cd emu && cargo run -p emulator-core --example commercial_visual_guard --release -- \
		--guard tekken3-late-fight \
		--out-dir $${PSOXIDE_TEKKEN_LATE_FIGHT_GUARD_OUT:-/tmp/tekken_late_fight_guard}

parity:
	cd emu && cargo test -p emulator-core --release --features trace-cop2 --test parity -- --ignored --nocapture

# Milestone-C regression suite — every SDK example side-loaded into
# the emulator, multi-signal state pinned. Depends on `examples` so
# every .exe referenced by the tests exists before we run them; the
# tests themselves skip gracefully when an .exe is missing, but
# gating on `examples` here surfaces build breaks up-front.
test-sdk: examples
	cd emu && cargo test -p emulator-core --release --test sdk_milestones -- --ignored --nocapture

# --- SDK examples ---------------------------------------------------------

EXAMPLE_OUT := build/examples/mipsel-sony-psx/release
CDDA_DEMO_TRACK ?= assets/audio/cdda/GONCHAROV.track02.cdda
GONCHAROV_WAV ?= assets/audio/cdda/GONCHAROV.wav
MAGIKAAAAARP_PONG_TRACK ?= assets/audio/cdda/GONCHAROV.track02.cdda
MAGIKAAAAARP_PONG_SPECTRUM := engine/examples/game-magikaaaaaarp-pong/assets/goncharov_spectrum_16x30hz.bin
DUCKSTATION_TIMEOUT ?= 45
DUCKSTATION_MAGIKARP_LOG ?= build/duckstation-harness/game-magikaaaaaarp-pong.log
PYTHON ?= python3
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
PROFILE_DEMO7_CAMERA_SWEEP_VISUAL_FRAMES ?= 240
PROFILE_DEMO7_CAMERA_SWEEP_GUEST_FRAMES ?= 1600
PROFILE_DEMO7_CAMERA_SWEEP_STEPS ?= 600000000
PROFILE_DEMO7_CAMERA_SWEEP_HW ?= /tmp/psoxide-demo7-camera-sweep-hw.ppm
PROFILE_DEMO7_CAMERA_SWEEP_HASH_LOG ?= /tmp/psoxide-demo7-camera-sweep-visual.csv
DATA_DISC_EXAMPLES := \
	hello-tri hello-input hello-ot hello-tex hello-gte hello-audio \
	showcase-text game-pong game-breakout game-invaders \
	showcase-3d showcase-model showcase-lights showcase-fog showcase-particles \
	hello-engine
PUBLIC_EXAMPLE_DISCS := $(addsuffix -disc,$(DATA_DISC_EXAMPLES)) hello-cdda-disc game-magikaaaaaarp-pong-disc

define build_data_disc
$(1)-disc: $(1)
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$$(EXAMPLE_OUT)/$(1).exe \
		--out ../../$$(EXAMPLE_OUT)/$(1).bin \
		--volume PSOXIDE
endef

hello-tri:
	cd sdk/examples/hello-tri && cargo build --release

hello-input:
	cd sdk/examples/hello-input && cargo build --release

hello-ot:
	cd sdk/examples/hello-ot && cargo build --release

# engine/ examples live outside sdk/examples/ — the engine is its
# own domain and its demos exercise the engine framework.
hello-engine:
	cd engine/examples/hello-engine && cargo build --release

hello-tex: assets
	cd sdk/examples/hello-tex && cargo build --release

hello-gte:
	cd sdk/examples/hello-gte && cargo build --release

hello-audio:
	cd sdk/examples/hello-audio && cargo build --release

hello-cdda:
	cd sdk/examples/hello-cdda && cargo build --release

hello-cdda-disc: hello-cdda
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/hello-cdda.exe \
		--out ../../$(EXAMPLE_OUT)/hello-cdda.bin \
		--volume PSOXIDE \
		--cdda-track ../../$(CDDA_DEMO_TRACK)

showcase-text:
	cd engine/examples/showcase-text && cargo build --release

game-pong:
	cd engine/examples/game-pong && cargo build --release

game-magikaaaaaarp-pong:
	cd engine/examples/game-magikaaaaaarp-pong && cargo build --release

magikaaaaaarp-pong-spectrum:
	$(PYTHON) tools/bake_spectrum.py $(GONCHAROV_WAV) \
		-o $(MAGIKAAAAARP_PONG_SPECTRUM) \
		--fps 30 --bands 16 --seconds 233

game-magikaaaaaarp-pong-disc: game-magikaaaaaarp-pong
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.exe \
		--out ../../$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.bin \
		--volume MAGIKARP \
		--cdda-track ../../$(MAGIKAAAAARP_PONG_TRACK)

game-breakout:
	cd engine/examples/game-breakout && cargo build --release

game-invaders:
	cd engine/examples/game-invaders && cargo build --release

showcase-3d: assets
	cd engine/examples/showcase-3d && cargo build --release

showcase-model:
	cd engine/examples/showcase-model && cargo build --release

showcase-lights: assets
	cd engine/examples/showcase-lights && cargo build --release

# showcase-fog uses two cooked textures (brick wall + cobblestone
# floor) on its corridor walls + floor, plus procedural geometry.
showcase-fog: assets
	cd engine/examples/showcase-fog && cargo build --release

showcase-particles:
	cd engine/examples/showcase-particles && cargo build --release

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
# the tracked placeholder. Does NOT recook — that's the editor's
# job (or `make cook-playtest` if you want the starter). The playtest runtime is
# streaming-only, so the default build includes the CD streaming reader.
EDITOR_PLAYTEST_FEATURES ?= cd-stream-bench

build-editor-playtest:
	cd engine/examples/editor-playtest && cargo build --release --features "$(EDITOR_PLAYTEST_FEATURES)"

profile-demo3:
	$(MAKE) profile-demo3-disc-stream PROFILE_DEMO3_DISC_STREAM_HW=$(PROFILE_DEMO3_HW)

profile-demo3-forward:
	$(MAKE) profile-demo3-disc-stream-forward PROFILE_DEMO3_DISC_STREAM_FORWARD_HW=$(PROFILE_DEMO3_FORWARD_HW)

profile-demo3-paced20:
	$(MAKE) profile-demo3-disc-stream PROFILE_DEMO3_DISC_STREAM_HW=$(PROFILE_DEMO3_PACED20_HW)

profile-demo3-paced20-forward:
	$(MAKE) profile-demo3-disc-stream-forward PROFILE_DEMO3_DISC_STREAM_FORWARD_HW=$(PROFILE_DEMO3_PACED20_FORWARD_HW)

profile-demo3-disc-stream:
	$(MAKE) cook-playtest PROJECT=projects/demo3/project.ron
	$(MAKE) build-editor-playtest
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt
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
	$(MAKE) cook-playtest PROJECT=projects/demo3/project.ron
	$(MAKE) build-editor-playtest
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt
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
	$(MAKE) cook-playtest PROJECT=projects/demo7/project.ron
	PSXO_CAMERA_SWEEP=1 PSXO_PROFILE_MODELS=1 $(MAKE) build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench room-surface-profile"
	cd tools/mkisopsx && cargo run --release -- \
		--exe ../../$(EXAMPLE_OUT)/editor-playtest.exe \
		--out ../../$(EXAMPLE_OUT)/editor-playtest.bin \
		--volume PSOXIDE \
		--cdtest-sectors 32 \
		--world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
		--world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt
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

# --- Content pipeline (host-side editor tooling) ------------------------

PSXED := editor/target/release/psxed

# Build the content-pipeline CLI. Independent host workspace —
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
	    $(PSXED) tex "$(1)" -o "$(2)" --size $(3) --depth $(4) --resample lanczos3 ; \
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
	$(PYTHON) tools/duckstation_harness.py \
		--cue $(CURDIR)/$(EXAMPLE_OUT)/game-magikaaaaaarp-pong.cue \
		--timeout $(DUCKSTATION_TIMEOUT) \
		--log $(CURDIR)/$(DUCKSTATION_MAGIKARP_LOG)

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

run-hello-engine: hello-engine-disc
	cd emu && PSOXIDE_DISC=$(CURDIR)/$(EXAMPLE_OUT)/hello-engine.cue cargo run -p frontend --release
