#!/bin/sh
# Fail when a guest linker map contains 64-bit integer helpers.
#
# The PS1 has no 64-bit ALU: every `i64`/`u64` divide or remainder in guest
# code becomes a call into compiler-builtins (`__divdi3`, `u64_div_rem`, ...)
# that costs hundreds of cycles. Project rule (2026-09-01): no 64-bit
# arithmetic in guest code; the cooker is the only place it is allowed.
# Multiplies and shifts inline and do not show up here; this gate catches the
# expensive half and proves a removed site is really gone.
#
# Run as: sh tools/guest_symbol_gate.sh <link.map>
# Produce the map with PSOXIDE_GUEST_LINK_MAP=<path> make build-editor-playtest
set -eu
MAP="${1:?usage: guest_symbol_gate.sh <link.map>}"
PATTERN='__(u?div|u?mod)[dt]i3|specialized_div_rem::(u|i)64_div_rem|__(mul|ashl|ashr|lshr)di3'
HITS="$(grep -E "$PATTERN" "$MAP" | grep -vE '\.o\)?:\(' | awk '{print $NF}' | sort -u || true)"
if [ -n "$HITS" ]; then
    echo "guest-symbol-gate: FAIL: 64-bit helpers linked into the guest:" >&2
    echo "$HITS" | sed 's/^/  /' >&2
    exit 1
fi
echo "guest-symbol-gate: PASS ($MAP)"
