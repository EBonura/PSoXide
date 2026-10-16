#!/bin/sh
# Gate for psx_math::Wide / WideU / Mask64 and the division of them (psx_math::wide_div):
# build sdk/examples/hello-wide and sdk/examples/hello-wide-div with a link map each, fail
# if either map contains any 64-bit, 128-bit or soft-float helper
# (tools/guest_symbol_gate.sh, no opt-out), then, when a frontend is given, run each image
# headless and fail unless the guest reports `WIDE PASS` / `WIDEDIV PASS`: every operation
# against a reference built from 32-bit arithmetic only, on the target. hello-wide-div also
# prints the cycles per call of each division (`widediv: <op> cycles_per_call=N`).
#
# Run as: sh tools/wide_gate.sh [<frontend>]   (the headless PSoXide emulator)
# Needs the pinned nightly (rust-toolchain.toml) and its rust-src.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
TARGET=mipsel-sony-psx
FRONTEND="${1:-}"

# gate_example <example> <verdict-prefix> <steps>
gate_example() {
    EXAMPLE="$1"
    VERDICT="$2"
    STEPS="$3"
    # The normal example build: the PGO driver links, patches load-delay hazards
    # and writes the link map of every link under <build>/<target>/psoxide-pgo-maps.
    cd "$ROOT"
    make -s -f tools/sdk-examples.mk example EXAMPLE="$EXAMPLE" BUILD="$WORK/build" >"$WORK/build.log" 2>&1 || {
        cat "$WORK/build.log" >&2
        echo "wide-gate: FAIL: the $EXAMPLE build failed" >&2
        exit 1
    }
    MAP="$WORK/$EXAMPLE.map"
    cp "$(ls -t "$WORK"/build/$TARGET/psoxide-pgo-maps/*.map | head -n 1)" "$MAP"
    sh "$ROOT/tools/guest_symbol_gate.sh" "$MAP"
    if [ -z "$FRONTEND" ]; then
        return 0
    fi
    EXE="$WORK/build/$TARGET/release/$EXAMPLE.exe"
    mkdir -p "$WORK/config"
    "$FRONTEND" --config-dir "$WORK/config" launch --path "$EXE" \
        --steps "$STEPS" >"$WORK/$EXAMPLE.out.txt" 2>"$WORK/$EXAMPLE.err.txt" || {
        echo "wide-gate: FAIL: the emulator exited with an error on $EXAMPLE" >&2
        cat "$WORK/$EXAMPLE.err.txt" >&2
        exit 1
    }
    grep -E "^($VERDICT|  FAIL|widediv)" "$WORK/$EXAMPLE.out.txt" || true
    grep -q "^$VERDICT PASS" "$WORK/$EXAMPLE.out.txt" || {
        echo "wide-gate: FAIL: $EXAMPLE did not report $VERDICT PASS" >&2
        exit 1
    }
}

gate_example hello-wide WIDE "${WIDE_STEPS:-400000000}"
gate_example hello-wide-div WIDEDIV "${WIDEDIV_STEPS:-400000000}"
if [ -z "$FRONTEND" ]; then
    echo "wide-gate: link gate only (pass a frontend to run the guest checks)"
    exit 0
fi
echo "wide-gate: PASS"
