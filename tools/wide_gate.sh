#!/bin/sh
# Gate for psx_math::Wide / WideU / Mask64: build sdk/examples/hello-wide with a
# link map, fail if the map contains any 64-bit, 128-bit or soft-float helper
# (tools/guest_symbol_gate.sh), then, when a frontend is given, run the image
# headless and fail unless the guest reports `WIDE PASS`: every operation
# against a reference built from 32-bit arithmetic only, on the target.
#
# Run as: sh tools/wide_gate.sh [<frontend>]   (the headless PSoXide emulator)
# Needs the pinned nightly (rust-toolchain.toml) and its rust-src.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
TARGET=mipsel-sony-psx

# The normal example build: the PGO driver links, patches load-delay hazards
# and writes the link map of every link under <build>/<target>/psoxide-pgo-maps.
cd "$ROOT"
make -s -f tools/sdk-examples.mk example EXAMPLE=hello-wide BUILD="$WORK/build" >"$WORK/build.log" 2>&1 || {
    cat "$WORK/build.log" >&2
    echo "wide-gate: FAIL: the build failed" >&2
    exit 1
}
MAP="$(ls -t "$WORK"/build/$TARGET/psoxide-pgo-maps/*.map | head -n 1)"
sh "$ROOT/tools/guest_symbol_gate.sh" "$MAP"
EXE="$WORK/build/$TARGET/release/hello-wide.exe"

FRONTEND="${1:-}"
if [ -z "$FRONTEND" ]; then
    echo "wide-gate: link gate only (pass a frontend to run the guest checks)"
    exit 0
fi
mkdir -p "$WORK/config"
"$FRONTEND" --config-dir "$WORK/config" launch --path "$EXE" \
    --steps "${WIDE_STEPS:-400000000}" >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "wide-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
grep -E '^(WIDE|  FAIL)' "$WORK/out.txt" || true
grep -q '^WIDE PASS' "$WORK/out.txt" || {
    echo "wide-gate: FAIL: the guest did not report WIDE PASS" >&2
    exit 1
}
echo "wide-gate: PASS"
