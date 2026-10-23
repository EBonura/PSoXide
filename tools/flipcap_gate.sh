#!/bin/sh
# Headless gate for the presentation cap: run hello-flipcap in the emulator and
# print its verdict. It fails if any capped flip came sooner than 2 VBlanks after
# the one before, or the opt-out does not flip every VBlank.
#
# Run as: sh tools/flipcap_gate.sh <frontend> <hello-flipcap.cue> [steps]
# Build the disc with `make hello-flipcap-disc`.
set -eu
FRONTEND="${1:?usage: flipcap_gate.sh <frontend> <hello-flipcap.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-600000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "flipcap-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if grep -q '^FLIPCAP PASS' "$WORK/out.txt"; then
    grep '^FLIPCAP' "$WORK/out.txt"
    echo "flipcap-gate: PASS"
else
    grep '^FLIPCAP' "$WORK/out.txt" >&2 || cat "$WORK/out.txt" >&2
    echo "flipcap-gate: FAIL" >&2
    exit 1
fi
