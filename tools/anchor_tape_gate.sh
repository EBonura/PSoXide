#!/bin/sh
# A speed window anchored after presses, end to end on the PSoXide emulator: hello-anchor (Start, a
# load of 20 or 50 VBlanks, then Cross counted from the load's end) built with both load lengths.
#
#   PSOXIDE_EMULATOR=/path/to/psoxide-emulator sh tools/anchor_tape_gate.sh OUT_DIR
#
# One tape is recorded from power-on on the short build (Start, then Cross 30 polls after the load
# ends), split at the poll the anchor (`LOADED eq 1`) first holds (`psoxide-test tape-anchor
# --before-out`), and replayed on each build the way the speed stage does: the presses before the
# anchor from power-on, the anchor found on that build, the rest joined from there (`psoxide-test
# tape-join`). Both builds must print the same `cross at loaded+N`. The open-loop tape (the recording
# fed from power-on) must not put Cross on the same frame on the long build: that is what the anchor
# fixes. Needs HOST_BIN (psoxide-pgo, psoxide-test) and GUEST_TARGET as the scene-core gate sets them.
set -eu
OUT=${1:?usage: anchor_tape_gate.sh OUT_DIR}
: "${PSOXIDE_EMULATOR:?set PSOXIDE_EMULATOR to the emulator binary}"
ROOT=$(pwd)
HOST_BIN=${HOST_BIN:-$ROOT/target/release}
GUEST_TARGET=${GUEST_TARGET:-$ROOT/build/examples}
CRATE=sdk/examples/hello-anchor
mkdir -p "$OUT/cfg"
OUT=$(cd "$OUT" && pwd)
fail() { echo "anchor-tape-gate: FAIL: $1" >&2; exit 1; }
# psoxide-test tape reads the frontend from its config: one for this run.
printf 'work = "%s"\n[tools]\nfrontend = "%s"\n' "$OUT/work" "$PSOXIDE_EMULATOR" >"$OUT/config.toml"
export PSOXIDE_TEST_CONFIG="$OUT/config.toml"
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort"'
for v in short long; do
    features=""
    [ "$v" = long ] && features="--features long-load"
    touch "$CRATE/src/main.rs"
    # shellcheck disable=SC2086 # features is empty or one flag and its value
    "$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --variant off -- build --release \
        --target mipsel-sony-psx -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
        --target-dir "$GUEST_TARGET" \
        --config "target.mipsel-sony-psx.rustflags=[$FLAGS,\"-Clink-arg=-T../../psoxide.ld\"]" \
        $features >"$OUT/build-$v.log" 2>&1 || fail "the $v build ($OUT/build-$v.log)"
    cp "$GUEST_TARGET/mipsel-sony-psx/release/hello-anchor.exe" "$OUT/$v.exe"
    cp "$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)" "$OUT/$v.map"
done
loaded() { awk '$NF == "LOADED" { print $1; exit }' "$OUT/$1.map"; }
emu() { # exe tape route-log
    "$PSOXIDE_EMULATOR" --config-dir "$OUT/cfg" launch --path "$OUT/$1.exe" --digital-pad \
        --steps 400000000 --stop-on-tty 'hello-anchor: PASS' --input-tape "$2" --input-tape-hold-last \
        --route-log "$3" --route-watch-u32 "0x$(loaded "$1")" 2>&1 || true
}
# The first port-1 poll at which LOADED reads 1 in a route log.
anchor_poll() {
    awk -F, -v col="ram_$2" 'NR == 1 { for (i = 1; i <= NF; i++) { if ($i == col) c = i; if ($i == "port1_polls") p = i }; next }
        c && $c == 1 { print $p; exit }' "$1"
}
shot() { sed -n 's/^hello-anchor: cross at loaded+\([0-9A-F]*\).*/\1/p' "$1" | head -1; }
# Record on the short build: Start at frame 40, Cross 30 frames after the load ends (frame ~92).
"$HOST_BIN/psoxide-test" tape --disc "$OUT/short.exe" --press "40:start:4,92:cross:4" \
    --out "$OUT/recorded.pxtape" --steps 400000000 --pad digital >"$OUT/tape.log" 2>&1 ||
    fail "recording the tape ($OUT/tape.log)"
emu short "$OUT/recorded.pxtape" "$OUT/rec.route.csv" >"$OUT/rec.log"
a=$(anchor_poll "$OUT/rec.route.csv" "$(loaded short)")
[ -n "$a" ] || fail "LOADED never held in the recording"
"$HOST_BIN/psoxide-test" tape-anchor --tape "$OUT/recorded.pxtape" --at-poll "$a" \
    --out "$OUT/after.pxtape" --before-out "$OUT/before.pxtape" >"$OUT/split.log" 2>&1 ||
    fail "splitting at poll $a ($OUT/split.log)"
for v in short long; do
    emu "$v" "$OUT/before.pxtape" "$OUT/$v-find.route.csv" >"$OUT/$v-find.log"
    p=$(anchor_poll "$OUT/$v-find.route.csv" "$(loaded "$v")")
    [ -n "$p" ] || fail "$v: LOADED never held with the anchor tape"
    "$HOST_BIN/psoxide-test" tape-join --before "$OUT/before.pxtape" --tape "$OUT/after.pxtape" \
        --at-poll "$p" --out "$OUT/$v-joined.pxtape" >"$OUT/$v-join.log" 2>&1 ||
        fail "$v: joining at poll $p ($OUT/$v-join.log)"
    emu "$v" "$OUT/$v-joined.pxtape" "$OUT/$v.route.csv" >"$OUT/$v.log"
    eval "anchor_$v=$p shot_$v=\$(shot \"\$OUT/\$v.log\")"
done
emu long "$OUT/recorded.pxtape" "$OUT/open.route.csv" >"$OUT/open.log"
open=$(shot "$OUT/open.log")
# shellcheck disable=SC2154 # set by the eval above
echo "anchor-tape-gate: anchor at poll $anchor_short (short) and $anchor_long (long); cross at loaded+$shot_short and +$shot_long anchored, +${open:-none} open-loop on the long build"
[ -n "$shot_short" ] && [ "$shot_short" = "$shot_long" ] || fail "the anchored shot moved between the builds"
[ "$anchor_long" != "$anchor_short" ] || fail "the long build's load did not move the anchor"
[ "${open:-none}" != "$shot_long" ] || fail "the open-loop tape landed on the same frame: the case proves nothing"
echo "anchor-tape-gate: PASS"
