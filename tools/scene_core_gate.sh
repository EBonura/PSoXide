#!/bin/sh
# The scene core's gate: build the scene examples once, make the nested and faulty discs from them,
# and run them on the PSoXide emulator (and PCSX-Redux with OpenBIOS) several at a time.
#
#   PSOXIDE_EMULATOR=/path/to/psoxide-emulator sh tools/scene_core_gate.sh fast|full OUT_DIR
#
# fast (every commit): host tests, the core image check, every build and its link refusals, every
# disc on three configurations (real hardware with the retail entry state; MiSTer with OpenBIOS's;
# real hardware with a hostile entry state, which caught an entry-state bug the others did not).
# full (before landing): the same on four hardware profiles by four entry states, a long soak, and
# PCSX-Redux with OpenBIOS when PCSX_REDUX names its app's Contents directory.
#
# The cases (each expects a TTY verdict or a fatal screen's stage code):
# - hello-scene: the walk (program, in-program and overlay scenes, a program switch), a row of
#   another core (refused 0x23) and a window of another core (fatal 0x01); the launcher (the walk
#   with its program B at row 8) is the outer disc every nested case below starts from;
# - hello-core-overlay: volatile, swap (STALE), before-world, tail, .bss host, host with boot-only
#   memory, and an empty .boot, alone and (volatile, tail) nested, and nested without the entry bit
#   (refused 0x25);
# - hello-modular: a member of two programs whose scenes need images (carried, service, volatile,
#   tailed, hosted), alone and nested; its soak (P and Q switch to each other, with pad presses
#   while the core waits and the VBlank counter on), alone and nested; its entry-state line, which
#   must be the same in every run (a BIOS boot and a core switch leave the same machine);
# - hello-boot: the film player playing a real FILM.STR, alone and nested (no splash on the switch);
# - pgo-search: a layout search on hello-core-overlay through its ship relink (tools/pgo_search_gate.sh);
# - faults, every one the emulators can produce: the launcher's table broken (fatal 0x21), a program
#   image's byte (fatal 0x14, the switch's load), a needed overlay's byte on an in-program switch
#   (fatal 0x14) and on a program's start (refused 0x14), a disc cut short inside a program (fatal
#   0x13), a scene needing an image the table lacks (refused 0x22), a row of another core (0x23).
#
# Needs: cargo, the PSoXide emulator; HOST_TARGET (default target), GUEST_TARGET (default
# build/examples), GATE_JOBS (runs at once, default 4). Writes OUT_DIR/results.tsv and exits 1 on
# any unexpected verdict.
set -eu
TIER=${1:?usage: scene_core_gate.sh fast|full OUT_DIR}
OUT=${2:?usage: scene_core_gate.sh fast|full OUT_DIR}
case $TIER in fast | full) ;; *) echo "scene-core-gate: tier is fast or full" >&2; exit 2 ;; esac
: "${PSOXIDE_EMULATOR:?set PSOXIDE_EMULATOR to the emulator binary}"
export PSOXIDE_EMULATOR
ROOT=$(pwd)
HOST_TARGET=${HOST_TARGET:-$ROOT/target}
export HOST_BIN="$HOST_TARGET/release"
export GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
JOBS=${GATE_JOBS:-4}
mkdir -p "$OUT/runs"
OUT=$(cd "$OUT" && pwd)
: >"$OUT/results.tsv"
: >"$OUT/jobs"
START=$(date +%s)
mark() { echo "scene-core-gate: $1 ($(($(date +%s) - START)) s)"; }

# Host side.
cargo build --locked --release --target-dir "$HOST_TARGET" -p psoxide-hazard -p psoxide-pgo \
    -p mkisopsx -p psx-str-cook 2>&1 | tail -1
cargo test --locked --release --target-dir "$HOST_TARGET" -p psx-iso -p mkisopsx 2>&1 |
    grep -E "^test result|FAILED|panicked" | sort | uniq -c
cargo test --locked --manifest-path sdk/Cargo.toml --target-dir "$HOST_TARGET/sdk-test" \
    -p psx-scene-abi -p psx-core 2>&1 | grep -E "^test result|FAILED|panicked" | sort | uniq -c
CARGO_TARGET_DIR="$HOST_TARGET/core" HAZARD_BIN="$HOST_BIN" sh sdk/crates/psx-core/build-image.sh --check
mark "host side done"

# Every build once (each refuses what must not link on the way).
export CARGO_TARGET_DIR="$HOST_TARGET/core"
failed() { echo "scene-core-gate: the $1 build failed ($OUT/build-$1.log)" >&2; tail -5 "$OUT/build-$1.log" >&2; exit 1; }
SOAK=20
[ "$TIER" = full ] && SOAK=300
sh sdk/examples/hello-scene/build.sh "$OUT/scene" >"$OUT/build-scene.log" 2>&1 || failed scene
sh sdk/examples/hello-core-overlay/build.sh "$OUT/ovl" >"$OUT/build-ovl.log" 2>&1 || failed ovl
SOAK=$SOAK sh sdk/examples/hello-modular/build.sh "$OUT/mod" >"$OUT/build-mod.log" 2>&1 || failed mod
sh sdk/examples/hello-boot/build.sh "$OUT/boot" >"$OUT/build-boot.log" 2>&1 || failed boot
grep -h -E "refused, as it must be|links the same EXE" "$OUT"/build-*.log | sort | uniq -c
# Every core program's ELF: __core_boot copies the window from __psx_core_store_lma, which must be
# where the core is stored (__psx_core_store_at). An empty boot region linked it 0 in Quake's
# journey build (11-24: fatal 0x01 at boot); the empty-.boot builds here are hostboot-empty and own.
NM=$(find "$(rustc --print sysroot)/lib/rustlib" -name llvm-nm -type f | head -1)
v=PASS
seen=0
for elf in "$OUT"/ovl/*.elf "$OUT"/ovl/*/*.elf "$OUT"/mod/*.elf; do
    [ -f "$elf" ] || continue
    at=$("$NM" "$elf" | awk '$3 == "__psx_core_store_at" { print $1 }')
    lma=$("$NM" "$elf" | awk '$3 == "__psx_core_store_lma" { print $1 }')
    seen=$((seen + 1))
    if [ -z "$at" ] || [ "$at" != "$lma" ]; then
        v=FAIL
        echo "scene-core-gate: $elf: __psx_core_store_lma ${lma:-missing}, stored at ${at:-missing}" >&2
    fi
done
[ "$seen" -gt 0 ] || v=FAIL
printf 'core-store-lma\t%s\t%s core ELFs\n' $v "$seen" >>"$OUT/results.tsv"
# A program whose code never names psx_core (hello-tri) linked with the core script: refused with
# the line to add, not lld's bare "symbol not found: __core_boot".
v=FAIL
touch sdk/examples/hello-tri/src/main.rs
if "$HOST_BIN/psoxide-pgo" apply --crate sdk/examples/hello-tri --variant off -- build --release \
    --target mipsel-sony-psx -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
    --target-dir "$GUEST_TARGET" --config 'target.mipsel-sony-psx.rustflags=["-Zunstable-options","-Cpanic=immediate-abort","-Clink-arg=-T../hello-modular/ram.ld","-Clink-arg=-T../../psoxide-core.ld","-Clink-arg=-T../../psoxide.ld"]' \
    >"$OUT/build-unnamed-core.log" 2>&1; then
    echo "scene-core-gate: a program that never names psx_core linked on the core" >&2
elif grep -q 'add `use psx_core as _;`' "$OUT/build-unnamed-core.log"; then
    v=PASS
fi
printf 'unnamed-core-refused\t%s\t%s\n' $v "$(grep -m1 '__core_boot is missing' "$OUT/build-unnamed-core.log" | cut -c1-120)" >>"$OUT/results.tsv"
mark "builds done"

# A layout search on a program of the core, through its ship relink (psoxide.ld keeps the core's
# entry code last; a search that moved it broke every searched game's relink, 11-24).
v=PASS
sh tools/pgo_search_gate.sh "$OUT/ovl" "$OUT/pgo-search" >"$OUT/pgo-search.log" 2>&1 || v=FAIL
printf 'pgo-search\t%s\t%s\n' $v "$(tail -1 "$OUT/pgo-search.log")" >>"$OUT/results.tsv"
mark "pgo search: $v"

# Derived discs.
NEST="$HOST_BIN/psx-nest"
FAULT="$HOST_BIN/psx-fault"
D=$OUT/discs
mkdir -p "$D"
cue() { printf 'FILE "%s" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n' "$(basename "$1")" >"${1%.bin}.cue"; }
L=$OUT/scene/launcher.bin
"$NEST" "$L" 8 "$OUT/mod/member.bin" "$D/n-member.bin" >/dev/null
"$NEST" "$L" 8 "$OUT/mod/soak.bin" "$D/n-soak.bin" >/dev/null
"$NEST" "$L" 8 "$OUT/boot/film.bin" "$D/n-film.bin" >/dev/null
for m in volatile tail; do
    "$NEST" "$L" 8 "$OUT/ovl/$m.bin" "$D/n-$m.bin" >/dev/null
done
"$NEST" "$L" 8 "$OUT/ovl/volatile.bin" "$D/n-volatile-nobit.bin" --no-entry-bit >/dev/null
for f in "table-magic|$L|22|table-magic" "tailed|$OUT/mod/member.bin|22|image-byte 5" \
    "svc|$OUT/mod/member.bin|22|image-byte 3" "needs|$OUT/mod/member.bin|22|needs 1 9" \
    "hash|$OUT/mod/member.bin|22|core-hash 1" "prog|$OUT/mod/member.bin|22|image-byte 0" \
    "short|$L|22|short 8"; do
    IFS='|' read -r name from lba what <<EOF
$f
EOF
    # shellcheck disable=SC2086 # `what` is the fault and its numbers
    "$FAULT" "$from" "$D/f-$name.bin" "$lba" $what >/dev/null
done
"$NEST" "$L" 8 "$D/f-prog.bin" "$D/f-prog-nested.bin" >/dev/null
# PCSX-Redux runs about ten times slower: its soak is 40 entries (the same discs, a shorter SOAK row).
sed "s/^scene   SOAK[0-9]*/scene   SOAK40/" "$OUT/mod/soak.scenes" >"$OUT/mod/soak40.scenes"
"$HOST_BIN/mkisopsx" --diag --exe "$OUT/mod/p.exe" --scenes "$OUT/mod/soak40.scenes" \
    --core-image sdk/crates/psx-core/prebuilt/psx-core.bin --volume HELLOMOD \
    --world-pack-extra-dir "$OUT/mod/world" --out "$D/soak40.bin" >/dev/null
"$NEST" "$L" 8 "$D/soak40.bin" "$D/n-soak40.bin" >/dev/null
mark "discs made"

# The runs.
PADS=$(i=30; while [ $i -lt 6000 ]; do printf '0x4000@%d+6,' $i; i=$((i + 40)); done)
export GATE_PADS="${PADS%,}"
if [ "$TIER" = full ]; then
    CONFIGS="real|default real|openbios real|hostile:1 real|hostile:7 mister|default mister|openbios
        mister|hostile:1 mister|hostile:7 mister,gpu-irq=never|default mister,gpu-irq=never|openbios
        mister,gpu-irq=never|hostile:1 mister,gpu-irq=never|hostile:7
        real,cd-ack=strict,mdec-req=hidden|default real,cd-ack=strict,mdec-req=hidden|openbios
        real,cd-ack=strict,mdec-req=hidden|hostile:1 real,cd-ack=strict,mdec-req=hidden|hostile:7"
else
    CONFIGS="real|default mister|openbios real|hostile:7"
fi
job() { # name disc steps expect [pads]
    for c in $CONFIGS; do
        hw=${c%%|*}
        es=${c#*|}
        tag=$(printf '%s.%s' "$hw" "$es" | tr ',=:' '__-')
        printf '%s|%s|%s|%s|%s|%s|%s\n' "$1.$tag" "$2" "$hw" "$es" "$3" "${5:-}" "$4" >>"$OUT/jobs"
    done
}
S=$OUT/scene
O=$OUT/ovl
M=$OUT/mod
job walk "$S/walk.cue" 1500000000 'tty:hello-scene: B PASS'
job walk-mism-row "$S/walk-mism-row.cue" 1500000000 'tty:hello-scene: O switch refused 00000023'
job walk-mism-win "$S/walk-mism-win.cue" 700000000 'fatal:01'
for d in volatile before tail host/host hostboot/hostboot hostboot-empty/hostboot-empty; do
    job "ovl-${d##*/}" "$O/$d.cue" 900000000 'tty:hello-core-overlay: PASS'
done
job ovl-swap "$O/swap.cue" 900000000 'tty:hello-core-overlay: STALE'
job n-volatile "$D/n-volatile.cue" 2000000000 'tty:hello-core-overlay: PASS'
job n-tail "$D/n-tail.cue" 2000000000 'tty:hello-core-overlay: PASS'
job n-volatile-nobit "$D/n-volatile-nobit.cue" 2000000000 'tty:hello-core-overlay: FAIL load 00000025'
job member "$M/member.cue" 1500000000 'tty:hello-modular: PASS'
job n-member "$D/n-member.cue" 2500000000 'tty:hello-modular: PASS'
job soak "$M/soak.cue" 40000000000 'tty:hello-modular: SOAK PASS' pads
job n-soak "$D/n-soak.cue" 40000000000 'tty:hello-modular: SOAK PASS' pads
job film "$OUT/boot/film.cue" 3500000000 'tty:hello-boot: PASS'
job n-film "$D/n-film.cue" 3500000000 'tty:hello-boot: PASS'
job f-table "$D/f-table-magic.cue" 900000000 'fatal:21'
job f-prog "$D/f-prog-nested.cue" 1500000000 'fatal:14'
job f-short "$D/f-short.cue" 2500000000 'fatal:13'
job f-tailed "$D/f-tailed.cue" 1200000000 'fatal:14'
job f-svc "$D/f-svc.cue" 1200000000 'tty:hello-modular: FAIL require 00000014'
job f-needs "$D/f-needs.cue" 1200000000 'fatal:22'
job f-hash "$D/f-hash.cue" 1200000000 'tty:hello-modular: FAIL switch refused 00000023'
for f in "$D"/*.bin; do cue "$f"; done
seq 1 "$(wc -l <"$OUT/jobs")" | xargs -P "$JOBS" -n 1 sh tools/scene_core_run.sh "$OUT"
mark "$(wc -l <"$OUT/jobs" | tr -d ' ') emulator runs done"

# PCSX-Redux with OpenBIOS: a TTY verdict per disc, the run stopped once it is printed.
if [ "$TIER" = full ] && [ -n "${PCSX_REDUX:-}" ]; then
    R=$OUT/redux
    mkdir -p "$R/cfg"
    printf '%s\n' "local n = 0" \
        "L = PCSX.Events.createEventListener('GPU::Vsync', function()" \
        "  n = n + 1; if n >= 7200 then print('SCEND'); PCSX.quit(0) end end)" >"$R/quit.lua"
    for c in "walk|$S/walk.cue|hello-scene: B PASS" "n-member|$D/n-member.cue|hello-modular: PASS" \
        "n-soak40|$D/n-soak40.cue|hello-modular: SOAK PASS" "n-film|$D/n-film.cue|hello-boot: PASS" \
        "ovl-tail|$O/tail.cue|hello-core-overlay: PASS" "ovl-host|$O/host/host.cue|hello-core-overlay: PASS" \
        "n-volatile-nobit|$D/n-volatile-nobit.cue|hello-core-overlay: FAIL load 00000025"; do
        IFS='|' read -r name disc want <<EOF
$c
EOF
        (cd "$R/cfg" && exec "$PCSX_REDUX/MacOS/PCSX-Redux" -portable -cli -softgpu -stdout \
            -bios "$PCSX_REDUX/Resources/share/pcsx-redux/resources/openbios.bin" -iso "$disc" \
            -dofile "$R/quit.lua" -run) >"$R/$name.log" 2>&1 &
        pid=$!
        i=0
        while kill -0 $pid 2>/dev/null && [ $i -lt 600 ]; do
            grep -a -q -E '^hello-[a-z-]+: (PASS|FAIL|STALE|SOAK|B PASS|B FAIL)' "$R/$name.log" && break
            sleep 1
            i=$((i + 1))
        done
        kill $pid 2>/dev/null || true
        wait $pid 2>/dev/null || true
        last=$(grep -a -E '^hello-[a-z-]+: (PASS|FAIL|STALE|SOAK|B PASS|B FAIL)' "$R/$name.log" | tail -1)
        case $last in "$want"*) v=PASS ;; *) v=FAIL ;; esac
        printf 'redux-%s\t%s\t%s\n' "$name" "$v" "$last" >>"$OUT/results.tsv"
    done
    mark "PCSX-Redux runs done"
fi

# The loading frame, frame by frame (one screenshot per VBlank): program P sets its display up
# (psx-gpu's Gpu::new), "loads" for 20 VBlanks, then shows its first frame (0x204080 at (0, 256)).
# Launched (n-member): the launcher's last screen stays up until P's frame, never a black frame
# between. Booted alone (member): black from the entry state until P's frame, nothing else.
frames() { # disc: the colour at (8, 120) of each distinct frame in turn, as `r g b`
    rm -rf "$OUT/runs/shots"
    mkdir -p "$OUT/runs/shots/cfg"
    "$PSOXIDE_EMULATOR" --config-dir "$OUT/runs/shots/cfg" launch --path "$1" --hardware real \
        --entry-state default --steps 2500000000 --stop-on-tty 'hello-modular: PASS' \
        --stop-on-tty 'hello-modular: FAIL' --route-screenshot-dir "$OUT/runs/shots/s" \
        --route-screenshot-interval 1 >"$OUT/runs/shots/log" 2>&1 || true
    for f in $(cd "$OUT/runs/shots/s" && ls | sort); do
        f="$OUT/runs/shots/s/$f"
        h=$(head -c 20 "$f" | awk 'NR <= 3 { n += length($0) + 1 } END { print n + 0 }')
        w=$(head -2 "$f" | tail -1 | cut -d' ' -f1)
        dd if="$f" bs=1 skip=$((h + (120 * w + 8) * 3)) count=3 2>/dev/null | od -An -tu1 |
            awk 'NF >= 3 { print $1, $2, $3 }'
    done | uniq
    rm -rf "$OUT/runs/shots"
}
for c in "n-member|$D/n-member.cue|cover" "member|$M/member.cue|black"; do
    IFS='|' read -r name disc want <<EOF
$c
EOF
    seq=$(frames "$disc" | awk '$1 == 33 && $2 == 66 && $3 == 132 { print "P"; exit } { print }')
    printf '%s\n' "$seq" >"$OUT/runs/loading-frame-$name.seq"
    before=$(printf '%s\n' "$seq" | grep -v '^P$' || true)
    v=FAIL
    case $want in
    cover) # never black once something is shown, and P's frame comes
        printf '%s\n' "$seq" | grep -q '^P$' &&
            ! printf '%s\n' "$before" | awk 'seen && $0 == "0 0 0" { bad = 1 } $0 != "0 0 0" { seen = 1 } END { exit !bad }' &&
            v=PASS ;;
    black) # only black before P's frame
        printf '%s\n' "$seq" | grep -q '^P$' && [ "$before" = "0 0 0" ] && v=PASS ;;
    esac
    printf 'loading-frame-%s\t%s\t%s\n' "$name" $v "$(printf '%s' "$seq" | tr '\n' '/')" >>"$OUT/results.tsv"
done

# The boot stubs (__core_boot, __boot_entry) never leave $sp outside the program's stack: the
# lowest $sp of a whole boot-to-PASS run, BIOS included, stays at or above the stack reserve's floor
# (STACK_INIT on the core, 0x801FDF00, less the default 32 KB STACK_RESERVE: hello-boot sets none). A stub that built $sp in two steps
# showed a stack 57 KB or 2 MB deep (11-24).
for d in plain scenes; do
    log=$OUT/runs/stack-$d.csv
    "$PSOXIDE_EMULATOR" --config-dir "$OUT/runs/cfg-stack-$d" launch --path "$OUT/boot/$d.cue" \
        --hardware real --entry-state default --steps 3500000000 --stop-on-tty 'hello-boot: PASS' \
        --stop-on-tty 'hello-boot: FAIL' --stack-profile-log "$log" >"$OUT/runs/stack-$d.txt" 2>&1 || true
    floor=801f5f00
    low=$(awk -F, 'NR == 2 { sub(/^0x/, "", $5); print $5 }' "$log" 2>/dev/null)
    v=FAIL
    [ -n "$low" ] && [ -n "$floor" ] && [ "$((0x$low))" -ge "$((0x$floor))" ] && v=PASS
    printf 'boot-sp-%s\t%s\tlowest $sp 0x%s, floor 0x%s\n' "$d" $v "${low:-none}" "${floor:-none}" \
        >>"$OUT/results.tsv"
done

# Whole-matrix checks: one entry state everywhere; pad presses during the soak; the film played.
states=$(cat "$OUT"/runs/*.txt | grep -a '^hello-modular: entry-state' | sort -u | wc -l | tr -d ' ')
n=$(cat "$OUT"/runs/*.txt | grep -a -c '^hello-modular: entry-state' || true)
v=FAIL
[ "$states" = 1 ] && v=PASS
printf 'entry-state-same\t%s\t%s distinct line(s) in %s entries\n' $v "$states" "$n" >>"$OUT/results.tsv"
for f in "$OUT"/runs/soak.*.txt "$OUT"/runs/n-soak.*.txt; do
    p=$(grep -a '^hello-modular: soak pad presses' "$f" | awk '{ print $NF }')
    v=FAIL
    [ -n "$p" ] && [ "$p" != 00000000 ] && v=PASS
    printf 'pad-%s\t%s\tpresses %s\n' "$(basename "$f" .txt)" $v "${p:-none}" >>"$OUT/results.tsv"
done

sort "$OUT/results.tsv" >"$OUT/results.sorted"
pass=$(grep -c "	PASS	" "$OUT/results.sorted" || true)
fail=$(grep -c "	FAIL	" "$OUT/results.sorted" || true)
grep "	FAIL	" "$OUT/results.sorted" | cut -c1-200 || true
mark "$TIER tier: $pass passed, $fail failed"
[ "$fail" = 0 ]
