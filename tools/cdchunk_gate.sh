#!/bin/sh
# Headless gate for the smallest psx-cdstream::single user: run hello-cdchunk in the emulator,
# which streams a chunk with an exact byte bound through the transport, cancels one, and reads
# with the polled reader in between, then print its verdict.
#
# Run as: sh tools/cdchunk_gate.sh <frontend> <hello-cdchunk.cue> [steps]
# Build the disc with `make hello-cdchunk-disc`; the frontend is the headless
# PSoXide emulator. The run stops when the guest prints its verdict (--stop-on-tty);
# <steps> (default 3 billion CPU instructions) is only a hang guard, and a run that
# reaches it without a verdict fails.
set -eu
FRONTEND="${1:?usage: cdchunk_gate.sh <frontend> <hello-cdchunk.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" \
    --stop-on-tty 'hello-cdchunk: ALL PASS' --stop-on-tty 'hello-cdchunk: FAIL' >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cdchunk-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
    cat "$WORK/out.txt" >&2
    echo "cdchunk-gate: FAIL: no verdict within the step ceiling (hang guard)" >&2
    exit 1
fi
if grep -q '^hello-cdchunk: ALL PASS' "$WORK/out.txt"; then
    grep '^hello-cdchunk:' "$WORK/out.txt"
    echo "cdchunk-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "cdchunk-gate: FAIL" >&2
    exit 1
fi
