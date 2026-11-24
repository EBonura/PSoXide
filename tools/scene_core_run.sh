#!/bin/sh
# One run of the scene-core gate (tools/scene_core_gate.sh starts these, several at once):
#
#   sh tools/scene_core_run.sh OUT N
#
# takes line N of OUT/jobs, `name|disc.cue|hardware|entry-state|steps|pads|expect` (`pads` either
# empty or `pads`: press pad 1 as $GATE_PADS says, emulator --pad-pulses syntax), runs the PSoXide
# emulator ($PSOXIDE_EMULATOR) headless on the disc and appends one line,
# `name<TAB>PASS|FAIL<TAB>what it saw`, to OUT/results.tsv. `expect` is either `tty:<prefix>` (the
# last `hello-` TTY line starts with it; the run stops at the first verdict line) or `fatal:<hex>`
# (the run goes to `steps` and the core's fatal screen in VRAM shows that stage code: red, eight
# bars, a tall white bar for a set bit, most significant first).
set -u
OUT=$1
IFS='|' read -r name disc hw es steps pads expect <<EOF
$(sed -n "${2}p" "$OUT/jobs")
EOF
log="$OUT/runs/$name.txt"
mkdir -p "$OUT/runs/cfg-$name"
set -- --config-dir "$OUT/runs/cfg-$name" launch --path "$disc" --hardware "$hw" --entry-state "$es" \
    --steps "$steps"
[ -n "$pads" ] && set -- "$@" --pad-pulses "$GATE_PADS"
case $expect in
tty:*)
    for stop in 'hello-scene: B PASS' 'hello-scene: B FAIL' 'hello-scene: O switch refused' \
        'hello-core-overlay: PASS' 'hello-core-overlay: STALE' 'hello-core-overlay: FAIL' \
        'hello-modular: PASS' 'hello-modular: FAIL' 'hello-modular: SOAK' 'hello-boot: PASS' \
        'hello-boot: FAIL'; do
        set -- "$@" --stop-on-tty "$stop"
    done
    ;;
fatal:*) set -- "$@" --dump-vram "$OUT/runs/$name.vram.ppm" ;;
esac
"$PSOXIDE_EMULATOR" "$@" >"$log" 2>&1
last=$(grep -a '^hello-' "$log" | tail -1)

# The colour of VRAM pixel X, Y in the dump (a 1024 x 512 P6 file): r g b.
ppm="$OUT/runs/$name.vram.ppm"
head=$(head -c 20 "$ppm" 2>/dev/null | awk 'NR <= 3 { n += length($0) + 1 } END { print n + 0 }')
pixel() {
    dd if="$ppm" bs=1 skip=$((head + ($2 * 1024 + $1) * 3)) count=3 2>/dev/null |
        od -An -tu1
}

verdict=FAIL
case $expect in
tty:*)
    seen=$last
    case $last in "${expect#tty:}"*) verdict=PASS ;; esac
    ;;
fatal:*)
    set -- $(pixel 8 8)
    if [ "${1:-0}" -gt 128 ] && [ "${2:-255}" -lt 64 ] && [ "${3:-255}" -lt 64 ]; then
        code=0
        bit=0
        while [ $bit -lt 8 ]; do
            set -- $(pixel $((40 + bit * 32)) 100)
            code=$((code * 2))
            [ "${1:-0}" -gt 200 ] && [ "${2:-0}" -gt 200 ] && code=$((code + 1))
            bit=$((bit + 1))
        done
        seen=$(printf 'fatal:%02x (last TTY: %s)' $code "$last")
        [ "$(printf 'fatal:%02x' $code)" = "$(printf 'fatal:%02x' "0x${expect#fatal:}")" ] && verdict=PASS
    else
        seen="no fatal screen (last TTY: $last)"
    fi
    ;;
esac
printf '%s\t%s\t%s\n' "$name" "$verdict" "$seen" >>"$OUT/results.tsv"
