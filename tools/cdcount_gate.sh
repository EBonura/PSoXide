#!/bin/sh
# Headless gate for psx_cdstream::counted: run hello-cdcount in the emulator on the retail console
# model, on the FPGA core's model, and on the FPGA core's model with GP0(1Fh) never raising
# GPUSTAT bit 24. The guest reads the handler's sector count while a read is in flight, bounds a
# last sector, parks, cancels and reads an unreadable sector, and prints a line per check.
#
# Run as: sh tools/cdcount_gate.sh <frontend> <hello-cdcount.cue> [steps]
# Build the disc with `make hello-cdcount-disc`; the frontend is the headless PSoXide
# emulator. Each run stops when the guest prints its verdict (--stop-on-tty); <steps>
# (default 3 billion CPU instructions) is only a hang guard, and a run that reaches it
# without a verdict fails.
set -eu
FRONTEND="${1:?usage: cdcount_gate.sh <frontend> <hello-cdcount.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

status=0
for hardware in real mister mister,gpu-irq=never; do
    PSOXIDE_CHECK_GPU_LL_FIFO=1 "$FRONTEND" --hardware "$hardware" launch --path "$CUE" --steps "$STEPS" \
        --stop-on-tty 'hello-cdcount: ALL PASS' --stop-on-tty 'hello-cdcount: FAIL' \
        >"$WORK/out.txt" 2>"$WORK/err.txt" || {
        echo "cdcount-gate: FAIL ($hardware): the emulator exited with an error" >&2
        cat "$WORK/err.txt" >&2
        status=1
        continue
    }
    if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
        cat "$WORK/out.txt" >&2
        echo "cdcount-gate: FAIL ($hardware): no verdict within the step ceiling (hang guard)" >&2
        status=1
    elif grep -q '^hello-cdcount: ALL PASS' "$WORK/out.txt"; then
        grep '^hello-cdcount:' "$WORK/out.txt"
        echo "cdcount-gate: PASS ($hardware)"
    else
        grep '^hello-cdcount:' "$WORK/out.txt" >&2 || cat "$WORK/out.txt" >&2
        echo "cdcount-gate: FAIL ($hardware)" >&2
        status=1
    fi
done
exit $status
