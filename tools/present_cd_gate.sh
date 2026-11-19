#!/bin/sh
# Headless gate for the one present path under CD traffic and cache flushes: run
# hello-present-cd in the emulator on the retail console model and on the FPGA
# core's model with GP0(1Fh) never raising GPUSTAT bit 24, and print each
# verdict. It fails on a tear, a part not kicked once, a frame not flipped once,
# a flip sooner than the cap, a recovered stall, a wrong sector, or a CD error,
# retry, timeout or dropped sector the same reads do not have with presenting
# off. Every run checks GPU linked-list nodes against the 16-word limit.
#
# Run as: sh tools/present_cd_gate.sh <frontend> <hello-present-cd.cue> [steps]
# Build the disc with `make hello-present-cd-disc`.
set -eu
FRONTEND="${1:?usage: present_cd_gate.sh <frontend> <hello-present-cd.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3000000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

status=0
for hardware in real mister,gpu-irq=never; do
    PSOXIDE_CHECK_GPU_LL_FIFO=1 "$FRONTEND" --hardware "$hardware" launch --path "$CUE" --steps "$STEPS" \
        --stop-on-tty 'PRESENT-CD PASS' --stop-on-tty 'PRESENT-CD FAIL' \
        >"$WORK/out.txt" 2>"$WORK/err.txt" || {
        echo "present-cd-gate: FAIL ($hardware): the emulator exited with an error" >&2
        cat "$WORK/err.txt" >&2
        status=1
        continue
    }
    if ! grep -q 'stopped-at=' "$WORK/out.txt"; then
        echo "present-cd-gate: FAIL ($hardware): no verdict within the step ceiling (hang guard)" >&2
        status=1
    elif grep -q '^PRESENT-CD PASS' "$WORK/out.txt"; then
        grep '^PRESENT-CD P' "$WORK/out.txt"
        echo "present-cd-gate: PASS ($hardware)"
    else
        grep '^PRESENT-CD' "$WORK/out.txt" >&2 || cat "$WORK/out.txt" >&2
        echo "present-cd-gate: FAIL ($hardware)" >&2
        status=1
    fi
done
exit "$status"
