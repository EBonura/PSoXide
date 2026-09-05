#!/bin/sh
# Cook and build this project's normal-spawn native review disc.
# Requires the repository Rust toolchain, Python 3, rsync and MIPS binutils.
set -eu
PROJECT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
REPO=$(CDPATH= cd -- "$PROJECT/../../.." && pwd)
OUT="$PROJECT/baked/cortex_ignition_tech_demo_0_4b"
cd "$REPO"
cargo run -p psxed-project --example cook_cortex_polish -- "$PROJECT"
cargo build --release -p mkisopsx
# Use a disposable source closure so cooking this project never overwrites
# another editor session's generated guest fixtures.
SRC=$(mktemp -d "${TMPDIR:-/tmp}/cortex04b-build.XXXXXX")
trap 'rm -rf "$SRC"' EXIT HUP INT TERM
mkdir -p "$SRC/engine/examples" "$SRC/sdk" "$SRC/editor/crates" "$SRC/crates" "$SRC/tools"
cp "$REPO/Cargo.toml" "$REPO/rust-toolchain.toml" "$SRC/"
cp "$REPO/engine/Cargo.toml" "$SRC/engine/"
cp "$REPO/sdk/Cargo.toml" "$REPO/sdk/psoxide.ld" "$SRC/sdk/"
cp "$REPO/tools/build_guest_staged.sh" "$REPO/tools/hazard_patch.py" "$SRC/tools/"
ln -s "$REPO/engine/crates" "$SRC/engine/crates"
ln -s "$REPO/sdk/crates" "$SRC/sdk/crates"
ln -s "$REPO/editor/crates/psxed-format" "$SRC/editor/crates/psxed-format"
ln -s "$REPO/crates/psx-hw" "$SRC/crates/psx-hw"
mkdir -p "$SRC/engine/examples/editor-playtest"
rsync -a --exclude generated --exclude target "$REPO/engine/examples/editor-playtest/" "$SRC/engine/examples/editor-playtest/"
rsync -a "$PROJECT/baked/generated/" "$SRC/engine/examples/editor-playtest/generated/"
PSOXIDE_GUEST_STAGE_ROOT="${PSOXIDE_GUEST_STAGE_ROOT:-/tmp/cortex04b-guest}" \
PSOXIDE_GUEST_LINK_MAP="$OUT.map" \
sh "$SRC/tools/build_guest_staged.sh" --target mipsel-sony-psx -Zjson-target-spec \
  -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem \
  --features 'cd-stream-bench emulator-telemetry'
sh "$REPO/tools/guest_symbol_gate.sh" "$OUT.map"
cp "$SRC/build/examples/mipsel-sony-psx/release/editor-playtest.exe" "$OUT.exe"
"${CARGO_TARGET_DIR:-$REPO/target}/release/mkisopsx" --exe "$OUT.exe" --out "$OUT.bin" --volume CORTEX04B \
  --world-pack-rooms-dir "$PROJECT/baked/generated/stream_chunks" \
  --world-pack-order-file "$PROJECT/baked/generated/world_pack_order.txt" \
  --ui-pack-dir "$PROJECT/baked/generated/ui_stream_chunks" \
  --ui-pack-order-file "$PROJECT/baked/generated/ui_pack_order.txt" \
  --cdda-track-list "$PROJECT/baked/generated/cdda_tracks.txt"
python3 "$REPO/tools/hazard_scan.py" "$OUT.exe"
shasum -a 256 "$OUT.exe" "$OUT.bin" "$OUT.cue"
