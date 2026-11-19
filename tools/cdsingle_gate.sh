#!/bin/sh
# Headless gate for psx-cdstream's single-transfer transport: run hello-cdsingle in the
# emulator, which streams CDTEST.BIN through it (one request at a time, a slow main loop
# with the cheap progress counter, cancel, park, the byte bound, the source closed between
# requests, a hand-off to polled code, a read the disc cannot satisfy) and checks every
# byte, then print its report.
#
# Run as: sh tools/cdsingle_gate.sh <frontend> <hello-cdsingle.cue> [steps]
# Build the disc with `make hello-cdsingle-disc`; the frontend is the headless
# PSoXide emulator. The run stops when the guest prints its verdict (--stop-on-tty);
# <steps> (default 3 billion CPU instructions) is only a hang guard, and a run that
# reaches it without a verdict fails.
set -eu
FRONTEND="${1:?usage: cdsingle_gate.sh <frontend> <hello-cdsingle.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" \
    --stop-on-tty 'hello-cdsingle: ALL PASS' --stop-on-tty 'hello-cdsingle: FAIL' >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cdsingle-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
    cat "$WORK/out.txt" >&2
    echo "cdsingle-gate: FAIL: no verdict within the step ceiling (hang guard)" >&2
    exit 1
fi
if grep -q '^hello-cdsingle: ALL PASS' "$WORK/out.txt"; then
    sed -n '/^psx-cdstream single transfer on the emulator/,/^hello-cdsingle:/p' "$WORK/out.txt"
    echo "cdsingle-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "cdsingle-gate: FAIL" >&2
    exit 1
fi
