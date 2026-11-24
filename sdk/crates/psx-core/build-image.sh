#!/bin/sh
# Build the scene core's image and place it where `psx-core`'s `link` feature embeds it.
#
#   sh build-image.sh                 rebuild prebuilt/psx-core.bin (its map goes to the target dir)
#   sh build-image.sh --check         rebuild into the target dir and fail unless the bytes equal
#                                     the committed prebuilt/psx-core.bin
#   sh build-image.sh --alt OUT.bin   a second image with different bytes (feature `alt`), for the
#                                     hash-mismatch tests only
#
# CARGO_TARGET_DIR (default: image/target) holds the build. Opt-level z, panic as a bare `break`,
# no core::fmt, LTO, and LLVM's backward delay-slot search off: the image has no hazard
# trampolines to patch through, so `hazard-scan --raw 0x801FDF00` must find it clean as built.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$HERE/image/target}"
MODE=build
OUT="$HERE/prebuilt/psx-core.bin"
FEATURES=""
case "${1:-}" in
    --check) MODE=check; OUT="$TARGET_DIR/psx-core.check.bin" ;;
    --alt) OUT="${2:?--alt needs an output path}"; FEATURES="--features alt" ;;
    "") ;;
    *) echo "usage: build-image.sh [--check | --alt OUT.bin]" >&2; exit 2 ;;
esac
# The map holds absolute build paths: it stays in the target dir, never next to the image.
MAP="$TARGET_DIR/$(basename "${OUT%.bin}").map"
# LLVM's delay-slot filler: every search on (the SDK examples' flags), which fills most slots. The
# image has no hazard trampolines to be patched through, so the scan below must find it clean.
DELAY_FLAGS='"-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'

cd "$HERE/image"
# shellcheck disable=SC2086
cargo build -q --release --locked --target mipsel-sony-psx -Zbuild-std=core \
    -Zbuild-std-features=compiler-builtins-mem --target-dir "$TARGET_DIR" $FEATURES \
    --config "target.mipsel-sony-psx.rustflags=[\"-Zunstable-options\",\"-Cpanic=immediate-abort\",${DELAY_FLAGS},\"-Clink-arg=-Timage.ld\",\"-Clink-arg=-Map=$MAP\",\"-Clink-arg=--oformat=binary\"]"
cp "$TARGET_DIR/mipsel-sony-psx/release/psx-core-image.exe" "$OUT"
# Word-pad: the program side hashes and copies whole words.
SIZE=$(wc -c <"$OUT" | tr -d ' ')
PAD=$(( (4 - SIZE % 4) % 4 ))
if [ "$PAD" -ne 0 ]; then head -c "$PAD" /dev/zero >>"$OUT"; fi
SIZE=$(wc -c <"$OUT" | tr -d ' ')
echo "psx-core image: $SIZE bytes -> $OUT"
# HAZARD_BIN names a directory with psoxide-hazard's tools (cargo build -p psoxide-hazard): the
# image must scan clean, load delays and coprocessor delays both.
if [ -n "${HAZARD_BIN:-}" ]; then
    "$HAZARD_BIN/hazard-scan" --raw 0x801FDF00 "$OUT"
    "$HAZARD_BIN/check-mfc0" --raw 0x801FDF00 "$OUT"
    # Every slot's stack depth, derived from this image, must be the one psx-scene-abi publishes.
    "$HAZARD_BIN/core-stack" "$OUT" "$MAP"
fi
if [ "$MODE" = check ]; then
    if cmp -s "$OUT" "$HERE/prebuilt/psx-core.bin"; then
        echo "psx-core image: rebuild matches prebuilt/psx-core.bin"
    else
        echo "psx-core image: FAIL: the rebuild differs from prebuilt/psx-core.bin" >&2
        exit 1
    fi
fi
