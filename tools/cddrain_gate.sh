#!/bin/sh
# Headless gate for psx-cdstream::drain: run hello-cddrain in the emulator, which
# reads CDTEST.BIN with the polled reader while the CD interrupt pops the
# sectors (one read, a partial last sector, thirty back to back, a cancel, the
# watchdog) and checks every byte, then print its report.
#
# Run as: sh tools/cddrain_gate.sh <frontend> <hello-cddrain.cue> [steps]
# Build the disc with `make hello-cddrain-disc`; the frontend is the headless
# PSoXide emulator. The run stops when the guest prints its verdict (--stop-on-tty);
# <steps> (default 3 billion CPU instructions) is only a hang guard, and a run that
# reaches it without a verdict fails.
set -eu
FRONTEND="${1:?usage: cddrain_gate.sh <frontend> <hello-cddrain.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" \
    --stop-on-tty 'hello-cddrain: ALL PASS' --stop-on-tty 'hello-cddrain: FAIL' >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cddrain-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
    cat "$WORK/out.txt" >&2
    echo "cddrain-gate: FAIL: no verdict within the step ceiling (hang guard)" >&2
    exit 1
fi
if grep -q '^hello-cddrain: ALL PASS' "$WORK/out.txt"; then
    sed -n '/^psx-cdstream drain on the emulator/,/^hello-cddrain:/p' "$WORK/out.txt"
    echo "cddrain-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "cddrain-gate: FAIL" >&2
    exit 1
fi
