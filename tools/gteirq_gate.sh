#!/bin/sh
# Headless gate for psx-rt's dispatcher: run hello-gteirq in the emulator. It checks that a GTE
# command runs once under dispatched interrupts, the dispatcher's order against its reference,
# and that `$28` (which the compiler uses as a general register) survives a VBlank hook and a
# source handler that both overwrite it, then prints its verdict.
#
# Run as: sh tools/gteirq_gate.sh <frontend> <hello-gteirq.cue> [steps]
# Build the disc with `make hello-gteirq-disc`; <steps> (default 3 billion CPU instructions) is
# only a hang guard, and a run that reaches it without a verdict fails.
set -eu
FRONTEND="${1:?usage: gteirq_gate.sh <frontend> <hello-gteirq.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" \
    --stop-on-tty 'GTEIRQ PASS' --stop-on-tty 'GTEIRQ FAIL' >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "gteirq-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
    cat "$WORK/out.txt" >&2
    echo "gteirq-gate: FAIL: no verdict within the step ceiling (hang guard)" >&2
    exit 1
fi
if grep -q '^GTEIRQ PASS' "$WORK/out.txt"; then
    grep '^GTEIRQ PASS' "$WORK/out.txt"
    echo "gteirq-gate: PASS"
else
    cat "$WORK/out.txt" >&2
    echo "gteirq-gate: FAIL" >&2
    exit 1
fi
