#!/bin/sh
# Headless gate for the one present path: run hello-present-queue in the emulator
# on the retail console model and on the FPGA core's model with GP0(1Fh) never
# raising GPUSTAT bit 24, and print each verdict. It fails if any part was not
# kicked once, any frame was not flipped once and in order, a frame drew into
# the buffer on screen, a flip came sooner than the cap, a finished frame waited
# past its edge (retail model only: without bit 24 every flip waits for the
# draw-done fallback, one VBlank more by design), or the queue stalled. Every
# run checks GPU linked-list nodes against the 16-word limit.
#
# Run as: sh tools/present_gate.sh <frontend> <hello-present-queue.cue> [steps]
# Build the disc with `make hello-present-queue-disc`.
set -eu
FRONTEND="${1:?usage: present_gate.sh <frontend> <hello-present-queue.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

status=0
for hardware in real mister,gpu-irq=never; do
    PSOXIDE_CHECK_GPU_LL_FIFO=1 "$FRONTEND" --hardware "$hardware" launch --path "$CUE" --steps "$STEPS" \
        --stop-on-tty 'PRESENT-QUEUE PASS' --stop-on-tty 'PRESENT-QUEUE FAIL' \
        >"$WORK/out.txt" 2>"$WORK/err.txt" || {
        echo "present-gate: FAIL ($hardware): the emulator exited with an error" >&2
        cat "$WORK/err.txt" >&2
        status=1
        continue
    }
    if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
        echo "present-gate: FAIL ($hardware): no verdict within the step ceiling (hang guard)" >&2
        status=1
    elif grep -q '^PRESENT-QUEUE PASS' "$WORK/out.txt"; then
        grep '^PRESENT-QUEUE P' "$WORK/out.txt"
        echo "present-gate: PASS ($hardware)"
    else
        grep '^PRESENT-QUEUE' "$WORK/out.txt" >&2 || cat "$WORK/out.txt" >&2
        echo "present-gate: FAIL ($hardware)" >&2
        status=1
    fi
done
exit "$status"
