#!/bin/sh
# Cortex Ignition before/after benchmark on the tracked whole-level tape.
#
# Builds the frontend, cooks and builds the project disc with a linker map
# (in a private guest stage root so the editor's own Play stage is untouched),
# runs the 64-bit symbol gate, replays the poll-bound tape twice, checks the
# two replays are byte-identical, and prints one row of numbers. With
# CORTEX_BENCH_BASELINE=<dir of a previous run> it prints before/after.
#
# Run as: make cortex-bench            (or CORTEX_BENCH_OUT=... make cortex-bench)
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROJECT="${CORTEX_BENCH_PROJECT:-editor/projects/cortex-ignition-tech-demo-0.4/project.ron}"
TAPE="${CORTEX_BENCH_TAPE:-editor/archive/fixtures/cortex-0.4/whole-level.pxtape}"
OUT="${CORTEX_BENCH_OUT:-${TMPDIR:-/tmp}/psoxide-cortex-bench}"
STAGE_ROOT="${CORTEX_BENCH_STAGE_ROOT:-${TMPDIR:-/tmp}/psoxide-psx-guest-cortex-bench}"
# lockstep-visuals renders exactly once per two fixed ticks regardless of
# wall-clock vblanks, so two builds of different speed present the same guest
# states and their display/VRAM hashes are comparable. Without it a code-layout
# change alone moves which vblank a frame lands on and the hashes diverge
# (observed 2026-09-01: an inlining change diverged at route tick 978, in the
# menu, before any gameplay). Shipping cadence measurements use
# CORTEX_BENCH_FEATURES="cd-stream-bench".
FEATURES="${CORTEX_BENCH_FEATURES:-cd-stream-bench lockstep-visuals}"
STEPS="${CORTEX_BENCH_STEPS:-6000000000}"
FRONTEND="$ROOT/target/release/frontend"

fail() { echo "cortex-bench: FAIL: $1" >&2; exit 1; }

cd "$ROOT"
mkdir -p "$OUT"
[ -f "$TAPE" ] || fail "tape $TAPE not found"

echo "cortex-bench: frontend"
(cd emu && cargo build -p frontend --release --quiet)

echo "cortex-bench: disc ($FEATURES) with link map"
(cd emu && EDITOR_PLAYTEST_FEATURES="$FEATURES" \
    PSOXIDE_GUEST_STAGE_ROOT="$STAGE_ROOT" \
    PSOXIDE_GUEST_CARGO_HOME="${PSOXIDE_GUEST_CARGO_HOME:-/tmp/psoxide-psx-guest-v1/cargo-home}" \
    PSOXIDE_GUEST_LINK_MAP="$OUT/link.map" \
    "$FRONTEND" build-project-disc --project "../$PROJECT" > "$OUT/disc-build.txt" 2>&1) \
    || { tail -20 "$OUT/disc-build.txt" >&2; fail "disc build failed"; }
CUE="$(ls "$(dirname "$PROJECT")"/baked/*.cue | head -1)"
[ -f "$CUE" ] || fail "no baked cue under $(dirname "$PROJECT")/baked"
shasum -a 256 build/examples/mipsel-sony-psx/release/editor-playtest.exe | tee "$OUT/exe.sha256"

echo "cortex-bench: symbol gate"
sh tools/guest_symbol_gate.sh "$OUT/link.map" | tee "$OUT/symbol-gate.txt" || true

for RUN in 1 2; do
    echo "cortex-bench: replay $RUN/2"
    # The tape-end frame is the same guest state on every build (poll-bound
    # tape), so it doubles as the visual A/B pair; dump it at 4x.
    PSOXIDE_HW_DUMP_SCALE=4 "$FRONTEND" launch --path "$CUE" --embedded-playtest \
        --input-tape "$TAPE" --steps "$STEPS" --dump-hash \
        --route-log "$OUT/route-$RUN.csv" \
        --cpu-cycle-profile-log "$OUT/cycles-$RUN.csv" \
        --pc-line-log "$OUT/pcline-$RUN.csv" \
        --dump-hw "$OUT/final-$RUN.ppm" \
        > "$OUT/run-$RUN.txt" 2>&1 || fail "replay $RUN failed (see $OUT/run-$RUN.txt)"
done

python3 tools/cortex_bench_report.py "$OUT" ${CORTEX_BENCH_BASELINE:+--baseline "$CORTEX_BENCH_BASELINE"}
