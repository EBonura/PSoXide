#!/bin/sh
# Cook the showcase assets, build the guest, run it headless in the emulator, and print its cycle table.
#
#   run.sh <outdir> [--golden]
#
# Needs a built frontend (default <repo>/target/release/frontend, override with FRONTEND). STEPS (default
# 120000000) bounds the run. `--golden` also rewrites the emulator-frame golden that
# psx-vector-cook's tests compare the host raster with. Writes <outdir>/hw.{ppm,png} (the last frame),
# <outdir>/ram.bin and the table of cycles per draw (the PSVGMEAS block).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
engine=$(cd "$here/../.." && pwd)
root=$(cd "$engine/.." && pwd)
out=${1:?usage: run.sh <outdir> [--golden]}
mkdir -p "$out"
out=$(cd "$out" && pwd)
frontend=${FRONTEND:-$root/target/release/frontend}
cook() { (cd "$engine" && cargo run -q -p psx-vector-cook --bin psx-svg-cook -- "$@"); }

cook cook examples/showcase-vector/assets/gallery.svg -o examples/showcase-vector/assets/gallery.psvg --quiet
cook cook examples/showcase-vector/assets/hud/stance_hud.svg -o examples/showcase-vector/assets/hud/stance_hud.psvg \
  --budget total=80 --quiet

(
  cd "$here"
  CARGO_TARGET_DIR="$root/build/examples" \
  RUSTFLAGS="-Zunstable-options -Cpanic=immediate-abort -Cllvm-args=-disable-mips-df-backward-search -Clink-arg=-T../../../sdk/psoxide.ld -Clink-arg=--oformat=binary" \
    cargo build --release --target mipsel-sony-psx -Zjson-target-spec -Zbuild-std=core,alloc \
    -Zbuild-std-features=compiler-builtins-mem 2>&1 | grep -v "MIPS-I support" | grep -E "^error" -A14 || true
)
(
  cd "$root"
  "$frontend" launch --path build/examples/mipsel-sony-psx/release/showcase-vector.exe \
    --steps "${STEPS:-120000000}" --dump-hw "$out/hw.ppm" --dump-ram "$out/ram.bin" --dump-hash 2>&1 \
    | grep -E "fnv|side-loaded"
)
cook png "$out/hw.ppm" -o "$out/hw.png"
cook meas "$out/ram.bin"
if [ "${2:-}" = "--golden" ]; then
  cook png "$out/hw.ppm" -o crates/psx-vector-cook/tests/data/showcase_gallery_emulator_176x114.ppm --crop 0,0,176,114
  echo "golden refreshed"
fi
