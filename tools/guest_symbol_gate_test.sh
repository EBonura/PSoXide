#!/bin/sh
# Self-test for guest_symbol_gate.sh: the default gate and the int64 opt-out.
# Run as: sh tools/guest_symbol_gate_test.sh
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="$HERE/guest_symbol_gate.sh"
DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT
fail=0

# map <name> <symbol...>: a minimal LLD-style map listing the symbols.
map() {
    name="$1"; shift
    : >"$DIR/$name.map"
    for sym in "$@"; do
        printf '80010000 80010000       40     4         %s\n' "$sym" >>"$DIR/$name.map"
    done
}
# expect <description> <exit status wanted> <env> <map>
expect() {
    desc="$1"; want="$2"; allow="$3"; m="$DIR/$4.map"
    got=0
    GUEST_GATE_ALLOW="$allow" sh "$GATE" "$m" >/dev/null 2>&1 || got=$?
    if [ "$got" != "$want" ]; then
        echo "FAIL: $desc (exit $got, wanted $want)"
        fail=1
    else
        echo "ok:   $desc"
    fi
}

map clean main psx_rt_start
map int64 main __divdi3 __udivdi3 __moddi3 __umoddi3 __muldi3 __mulodi4 \
    compiler_builtins::int::specialized_div_rem::u64_div_rem
map signed main __ashldi3 __ashrdi3 __lshrdi3 compiler_builtins::int::specialized_div_rem::i64_div_rem
map float main __mulsf3 __divdi3
map wide main __divdi3 __multi3
map checked128 main __muloti4
map div128 main compiler_builtins::int::specialized_div_rem::u128_div_rem
map eq main __eqsf2 __floatsisf

expect "clean map passes" 0 "" clean
expect "clean map passes with int64 allowed" 0 int64 clean
expect "64-bit integer helpers fail by default" 1 "" int64
expect "64-bit integer helpers pass with int64" 0 int64 int64
expect "64-bit shifts and signed divide pass with int64" 0 int64 signed
expect "64-bit shifts fail by default" 1 "" signed
expect "soft-float stays banned with int64" 1 int64 float
expect "128-bit multiply stays banned with int64" 1 int64 wide
expect "128-bit checked multiply stays banned with int64" 1 int64 checked128
expect "128-bit divide stays banned with int64" 1 int64 div128
expect "float compare and convert stay banned with int64" 1 int64 eq
expect "an unknown allow value is an error" 2 float clean
exit $fail
