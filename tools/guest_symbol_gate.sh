#!/bin/sh
# Fail when a guest linker map contains 64-bit integer or soft-float helpers.
#
# The PS1 has no 64-bit ALU and no FPU: every `i64`/`u64` divide or remainder
# in guest code becomes a call into compiler-builtins (`__divdi3`,
# `u64_div_rem`, ...) that costs hundreds of cycles, and every `f32`/`f64`
# operation becomes a software routine (`__mulsf3`, `__addsf3`, `__floatsisf`,
# ...). Project rule (2026-09-01): no 64-bit arithmetic and no float in guest
# code; the cooker is the only place either is allowed.
# Inline 64-bit multiplies, adds and shifts do not show up here; this gate
# catches the expensive half and proves a removed site is really gone.
#
# A project whose product is 64-bit arithmetic (PSXcel's i64 cells) may opt out of
# the 64-bit INTEGER helpers only, by setting GUEST_GATE_ALLOW=int64:
#   __divdi3 __udivdi3 __moddi3 __umoddi3 __muldi3 __mulodi4 __ashldi3 __ashrdi3
#   __lshrdi3 and compiler-builtins' u64_div_rem / i64_div_rem.
# Soft-float, every 128-bit helper (__multi3, __muloti4, u128_div_rem, ...) and any
# other name stay banned; any other GUEST_GATE_ALLOW value is an error.
#
# Run as: [GUEST_GATE_ALLOW=int64] sh tools/guest_symbol_gate.sh <link.map>
# Produce the map with PSOXIDE_GUEST_LINK_MAP=<path> make build-editor-playtest
set -eu
MAP="${1:?usage: guest_symbol_gate.sh <link.map>}"
PATTERN='__(u?div|u?mod)[dt]i3|specialized_div_rem::(u|i)(64|128)_div_rem|__(mul|ashl|ashr|lshr)[dt]i3|__mulo[dt]i4|__(add|sub|mul|div)[sd]f3|__(eq|ne|lt|le|gt|ge|unord)[sd]f2|__float(un)?(si|di)[sd]f|__fix(uns)?[sd]f(si|di)|__extendsfdf2|__truncdfsf2'
HITS="$(grep -E "$PATTERN" "$MAP" | grep -vE '\.o\)?:\(' | awk '{print $NF}' | sort -u || true)"
case "${GUEST_GATE_ALLOW:-}" in
    "") ;;
    int64)
        # Whole-symbol match, so `__muloti4` and `u128_div_rem` are never let through.
        ALLOWED='^(__(u?div|u?mod|mul|ashl|ashr|lshr)di3|__mulodi4|.*specialized_div_rem::(u|i)64_div_rem)$'
        HITS="$(printf '%s\n' "$HITS" | grep -vE "$ALLOWED" || true)"
        ;;
    *)
        echo "guest-symbol-gate: unknown GUEST_GATE_ALLOW value '$GUEST_GATE_ALLOW' (only int64)" >&2
        exit 2
        ;;
esac
if [ -n "$HITS" ]; then
    echo "guest-symbol-gate: FAIL: 64-bit or soft-float helpers linked into the guest:" >&2
    echo "$HITS" | sed 's/^/  /' >&2
    exit 1
fi
echo "guest-symbol-gate: PASS ($MAP${GUEST_GATE_ALLOW:+, allowing $GUEST_GATE_ALLOW})"
