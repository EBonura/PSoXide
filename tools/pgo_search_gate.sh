#!/bin/sh
# A layout search on a program of the scene core, end to end: the search a game runs before its
# ship link, on hello-core-overlay (psoxide.ld keeps the core's entry code, `.text_entry.*`, last).
#
#   PSOXIDE_EMULATOR=/path/to/psoxide-emulator sh tools/pgo_search_gate.sh OVL_DIR WORK_DIR
#
# OVL_DIR is hello-core-overlay/build.sh's output (the scene-core gate's OUT/ovl). Steps: count the
# words the volatile disc runs (the seed layout), record a fetch trace of the same run, search
# (ROUNDS, default 40), relink the program with the searched layout through `psoxide-pgo apply`
# (which refuses a relink that does not follow its order file), check the relinked map with
# `place` and `followed`, press the volatile disc again with the relinked EXE and run it to its
# PASS line. Exits 1 on any failure. Needs HOST_BIN (psoxide-pgo, mkisopsx, psx-exe) and
# GUEST_TARGET as the scene-core gate sets them.
set -eu
OVL=${1:?usage: pgo_search_gate.sh OVL_DIR WORK_DIR}
W=${2:?usage: pgo_search_gate.sh OVL_DIR WORK_DIR}
: "${PSOXIDE_EMULATOR:?set PSOXIDE_EMULATOR to the emulator binary}"
ROOT=$(pwd)
HOST_BIN=${HOST_BIN:-$ROOT/target/release}
GUEST_TARGET=${GUEST_TARGET:-$ROOT/build/examples}
ROUNDS=${ROUNDS:-40}
PGO="$HOST_BIN/psoxide-pgo"
SCRIPT=$ROOT/sdk/psoxide.ld
CRATE=sdk/examples/hello-core-overlay
rm -rf "$W"
mkdir -p "$W/cfg"
W=$(cd "$W" && pwd)
fail() { echo "pgo-search-gate: FAIL: $1" >&2; exit 1; }
emu() { "$PSOXIDE_EMULATOR" --config-dir "$W/cfg" launch --hardware real --entry-state default \
    --steps 900000000 --stop-on-tty 'hello-core-overlay: PASS' --stop-on-tty 'hello-core-overlay: FAIL' "$@"; }

# 1. The seed: the words the unordered link runs.
emu --path "$OVL/volatile.cue" --pc-log-words --pc-line-log "$W/words.csv" >"$W/words.log" 2>&1 ||
    fail "word-count run ($W/words.log)"
"$PGO" layout "$OVL/hello-core-overlay.map" "$OVL/hello-core-overlay.exe" "$W/words.csv" \
    "$W/seed.layout" >"$W/seed.log" 2>&1 || fail "seed layout ($W/seed.log)"
# 2. One fetch trace of the same run.
emu --path "$OVL/volatile.cue" --fetch-trace-log "$W/run.trace" --fetch-trace-start-route-tick 0 \
    >"$W/trace.log" 2>&1 || fail "traced run ($W/trace.log)"
[ -s "$W/run.trace" ] || fail "empty fetch trace ($W/trace.log)"
# 3. The search.
"$PGO" search "$W/seed.layout" "$OVL/hello-core-overlay.map" "$OVL/hello-core-overlay.exe" \
    "$SCRIPT" "$W/searched.layout" --trace run "$OVL/hello-core-overlay.map" \
    "$OVL/hello-core-overlay.exe" "$W/run.trace" --rounds "$ROUNDS" --seed 1 --threads 2 \
    >"$W/search.log" 2>&1 || fail "search ($W/search.log)"
grep -q "^# psoxide-pgo searched: rounds $ROUNDS seed 1" "$W/searched.layout" ||
    fail "the searched layout has no search stamp"
# 4. The ship relink with it (apply refuses a relink that does not follow the order).
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort","-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'
link='"-Clink-arg=-Tram.ld","-Clink-arg=-T../../psoxide-core.ld","-Clink-arg=-T../../psoxide-overlay.ld","-Clink-arg=-T../../psoxide.ld","-Clink-arg=--orphan-handling=error"'
touch "$CRATE/src/main.rs"
"$PGO" apply --crate "$CRATE" --profile "$CRATE/pgo.prof" --layout "$W/searched.layout" \
    --variant off+order -- build --release --target mipsel-sony-psx -Zbuild-std=core \
    -Zbuild-std-features=compiler-builtins-mem --target-dir "$GUEST_TARGET" \
    --config "target.mipsel-sony-psx.rustflags=[$FLAGS,$link]" >"$W/ship.log" 2>&1 ||
    fail "the ship relink with the searched layout ($W/ship.log)"
built="$GUEST_TARGET/mipsel-sony-psx/release/hello-core-overlay"
cp "$built.exe" "$W/ship.exe"
cp "$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)" "$W/ship.map"
cmp -s "$W/ship.map" "$OVL/hello-core-overlay.map" && fail "the ship map equals the unordered one"
# 5. The order the layout gives the first link, followed by the ship map.
"$PGO" place "$W/searched.layout" "$OVL/hello-core-overlay.map" "$OVL/hello-core-overlay.exe" \
    "$SCRIPT" "$W/order.txt" >"$W/place.log" 2>&1 || fail "place ($W/place.log)"
grep -q "__core_boot\|__boot_entry" "$W/order.txt" && fail "the order file names the core's entry code"
"$PGO" followed "$W/order.txt" "$W/ship.map" >"$W/followed.log" 2>&1 ||
    fail "the ship map does not follow the searched layout ($W/followed.log)"
# 6. The relinked program still runs.
cp "$built.elf" "$W/ship.elf"
LOAD=$("$HOST_BIN/psx-exe" --overlay .overlay.1 "$W/ship.elf" "$W/menu.ovl")
ADDR=${LOAD#load=}
cat >"$W/ship.scenes" <<MANIFEST
program PSX.EXE  -         reserve=$ADDR-$(printf "0x%x" $((ADDR + 0x40000)))
overlay MENU.OVL menu.ovl  owner=PSX.EXE $LOAD class=volatile
scene   START    PSX.EXE   start
MANIFEST
(cd "$W" && "$HOST_BIN/mkisopsx" --diag --exe ship.exe --scenes ship.scenes \
    --core-image "$ROOT/sdk/crates/psx-core/prebuilt/psx-core.bin" --volume HELLOOVL \
    --world-pack-extra-dir "$OVL/world" --out ship.bin) >"$W/press.log" 2>&1 || fail "press ($W/press.log)"
printf 'FILE "ship.bin" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n' >"$W/ship.cue"
emu --path "$W/ship.cue" >"$W/run.log" 2>&1 || true
grep -a -q '^hello-core-overlay: PASS' "$W/run.log" || fail "the relinked program did not pass ($W/run.log)"
echo "pgo-search-gate: PASS: $(grep '^followed: ' "$W/followed.log")"
