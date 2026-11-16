#!/bin/sh
# Headless gate for the smallest psx-cdstream::single user: run hello-cdlean in the emulator,
# which streams a chunk with an exact byte bound through the transport, cancels one, and reads
# with the polled reader in between, then print its verdict.
#
# Run as: sh tools/cdlean_gate.sh <frontend> <hello-cdlean.cue> [steps]
# Build the disc with `make hello-cdlean-disc`; the frontend is the headless
# PSoXide emulator. The run stops when the guest prints its verdict (--stop-on-tty);
# <steps> (default 3 billion CPU instructions) is only a hang guard, and a run that
# reaches it without a verdict fails.
set -eu
FRONTEND="${1:?usage: cdlean_gate.sh <frontend> <hello-cdlean.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" \
    --stop-on-tty 'hello-cdlean: ALL PASS' --stop-on-tty 'hello-cdlean: FAIL' >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cdlean-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
    cat "$WORK/out.txt" >&2
    echo "cdlean-gate: FAIL: no verdict within the step ceiling (hang guard)" >&2
    exit 1
fi
if grep -q '^hello-cdlean: ALL PASS' "$WORK/out.txt"; then
    grep '^hello-cdlean:' "$WORK/out.txt"
    echo "cdlean-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "cdlean-gate: FAIL" >&2
    exit 1
fi
