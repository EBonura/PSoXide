#!/bin/sh
# Headless gate for hello-cdstream-probe: run the whole measurement sequence in
# the emulator, answering every question with X, and check it reached its
# report with every read intact. The numbers are the emulator's model of the
# drive, not a console's.
#
# Run as: sh tools/cdstream_probe_gate.sh <frontend> <hello-cdstream-probe.cue> [steps]
#         sh tools/cdstream_probe_gate.sh --check <captured-stdout.txt>
# Build the disc with `make hello-cdstream-probe-disc`. The run is capped at
# <steps> CPU instructions (default 3.6 billion); the probe finishes its
# sequence well inside that and then flips through its report pages.
set -eu

check() {
    out="$1"
    grep -q '^hello-cdstream-probe: done' "$out" || {
        echo "cdstream-probe-gate: FAIL: the probe never reached its report" >&2
        return 1
    }
    payload=$(sed -n '/^hello-cdstream-probe: payload/{n;p;}' "$out")
    fail=0
    for key in F1 B1 F8192 SP2 SP1 C2 C1 LG PG15 PL TP D2 D2B RS D3 D4 PS SA SS M; do
        case ";$payload" in
            *";$key="*) ;;
            *) echo "cdstream-probe-gate: missing $key" >&2; fail=1 ;;
        esac
    done
    # Reads that must be intact: no bad reads in the seek table, no dropped
    # sectors, and the data reads after audio (their last field is the failure
    # code, 0 when done in full).
    case ";$payload" in *";ER=0;"*) ;; *) echo "cdstream-probe-gate: seek table had bad reads" >&2; fail=1 ;; esac
    for key in D2 D2B; do
        code=$(printf '%s' "$payload" | tr ';' '\n' | sed -n "s/^$key=//p" | cut -d, -f4)
        [ "$code" = "0" ] || { echo "cdstream-probe-gate: $key read failed (code $code)" >&2; fail=1; }
    done
    for key in SP2 SP1; do
        dropped=$(printf '%s' "$payload" | tr ';' '\n' | sed -n "s/^$key=//p" | cut -d, -f3)
        [ "$dropped" = "0" ] || { echo "cdstream-probe-gate: $key dropped $dropped sectors" >&2; fail=1; }
    done
    [ "$fail" = 0 ] || return 1
    sed -n '/^hello-cdstream-probe: payload/,/^hello-cdstream-probe: done/p' "$out"
    echo "cdstream-probe-gate: PASS"
}

if [ "${1:-}" = "--check" ]; then
    check "${2:?usage: cdstream_probe_gate.sh --check <captured-stdout.txt>}"
    exit $?
fi

FRONTEND="${1:?usage: cdstream_probe_gate.sh <frontend> <hello-cdstream-probe.cue> [steps]}"
CUE="${2:?missing cue}"
STEPS="${3:-3600000000}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Press CROSS for six frames every 150 route ticks: starts the run, answers
# every question yes, and (once the report is up) turns its pages.
PULSES=""
tick=30
while [ "$tick" -lt 20000 ]; do
    PULSES="$PULSES,0x4000@$tick+6"
    tick=$((tick + 150))
done
PULSES="${PULSES#,}"

"$FRONTEND" launch --path "$CUE" --steps "$STEPS" --digital-pad --pad-pulses "$PULSES" \
    >"$WORK/out.txt" 2>"$WORK/err.txt" || {
    echo "cdstream-probe-gate: FAIL: the emulator exited with an error" >&2
    cat "$WORK/err.txt" >&2
    exit 1
}
check "$WORK/out.txt"
