#!/bin/sh
# Headless gate for the Pause protocol: run hello-cdpause in the emulator, whose
# CD model answers a Pause the way the console does (second response 1.2 to 3.9
# million clocks after the first, a Pause during a seek refused), and print its report.
#
# Run as: sh tools/cdpause_gate.sh <frontend> <hello-cdpause.cue> [steps]
# Build the disc with `make hello-cdpause-disc`. The frontend must include the
# emulator's silicon Pause model (PSoXide-emulator 33b58b6 or later); an older
# emulator completes a Pause in 7,000 clocks and check "1" fails.
set -eu
FRONTEND="${1:?usage: cdpause_gate.sh <frontend> <hello-cdpause.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cdpause-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if grep -q '^hello-cdpause: ALL PASS' "$WORK/out.txt"; then
    grep -E '^[0-9] ' "$WORK/out.txt"
    echo "cdpause-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "cdpause-gate: FAIL" >&2
    exit 1
fi
