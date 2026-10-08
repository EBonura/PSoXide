#!/bin/sh
# Headless gate for psx-cdstream: run hello-cdstream in the emulator, which
# streams CDTEST.BIN through the interrupt-driven transport (single reads,
# chaining, priority, abort and resume, a sustained stream, an audio lease, a
# read the disc cannot satisfy) and checks every byte, then print its report.
#
# Run as: sh tools/cdstream_gate.sh <frontend> <hello-cdstream.cue> [steps]
# Build the disc with `make hello-cdstream-disc`; the frontend is the headless
# PSoXide emulator. The run is capped at <steps> CPU instructions (default
# 3 billion); the program stops testing long before that and idles.
set -eu
FRONTEND="${1:?usage: cdstream_gate.sh <frontend> <hello-cdstream.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cdstream-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if grep -q '^hello-cdstream: ALL PASS' "$WORK/out.txt"; then
    sed -n '/^psx-cdstream on the emulator/,/^hello-cdstream:/p' "$WORK/out.txt"
    echo "cdstream-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "cdstream-gate: FAIL" >&2
    exit 1
fi
