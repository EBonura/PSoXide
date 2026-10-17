#!/bin/bash
# The panic=immediate-abort gate (tools/psoxide-size policy.rs, fault.rs).
#
#   tools/abort_gate.sh FRONTEND
#
# 1. hello-fault panics on purpose: the guest must record a CPU exception
#    (ExcCode 9, break) and `psoxide-size faults` must name main around EPC.
# 2. hello-present runs clean: cpu_exceptions=0.
# 3. hello-present is smaller than the same build with PSOXIDE_PANIC_MESSAGES=1.
set -euo pipefail
FRONTEND=${1:?usage: abort_gate.sh FRONTEND}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${ABORT_GATE_OUT:-$ROOT/build/abort-gate}
SIZE="$ROOT/target/release/psoxide-size"
cd "$ROOT"
cargo build --release --locked -p psoxide-size >/dev/null
rm -rf "$OUT"; mkdir -p "$OUT"

build() { # example builddir [env...]
    local example=$1 dir=$2; shift 2
    env "$@" make -f tools/sdk-examples.mk example EXAMPLE="$example" BUILD="$dir" >"$dir.log" 2>&1
    ls -t "$dir"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1
}
run_ram() { # exe ram
    "$FRONTEND" launch --path "$1" --guest-frames 120 --dump-ram "$2" --no-memcard >/dev/null 2>&1
}

fault_map=$(build hello-fault "$OUT/fault" PSOXIDE_PANIC_MESSAGES=0)
run_ram "$OUT/fault/mipsel-sony-psx/release/hello-fault.exe" "$OUT/fault.ram"
if "$SIZE" faults "$fault_map" "$OUT/fault.ram" >"$OUT/fault.txt"; then
    echo "FAIL: hello-fault panicked but no CPU exception was recorded"; cat "$OUT/fault.txt"; exit 1
fi
grep -q "ExcCode 9" "$OUT/fault.txt" || { echo "FAIL: not a break"; cat "$OUT/fault.txt"; exit 1; }
grep -q "in main," "$OUT/fault.txt" || { echo "FAIL: EPC not resolved to main"; cat "$OUT/fault.txt"; exit 1; }
echo "ok: panic is a recorded break: $(sed -n 2p "$OUT/fault.txt")"

clean_map=$(build hello-present "$OUT/abort" PSOXIDE_PANIC_MESSAGES=0)
run_ram "$OUT/abort/mipsel-sony-psx/release/hello-present.exe" "$OUT/abort.ram"
"$SIZE" faults "$clean_map" "$OUT/abort.ram" | grep -qx "cpu_exceptions=0" || { echo "FAIL: clean example took an exception"; exit 1; }
echo "ok: hello-present cpu_exceptions=0"

build hello-present "$OUT/msg" PSOXIDE_PANIC_MESSAGES=1 >/dev/null
a=$(stat -f%z "$OUT/abort/mipsel-sony-psx/release/hello-present.exe" 2>/dev/null || stat -c%s "$OUT/abort/mipsel-sony-psx/release/hello-present.exe")
m=$(stat -f%z "$OUT/msg/mipsel-sony-psx/release/hello-present.exe" 2>/dev/null || stat -c%s "$OUT/msg/mipsel-sony-psx/release/hello-present.exe")
[ "$a" -lt "$m" ] || { echo "FAIL: abort build ($a B) is not smaller than the message build ($m B)"; exit 1; }
echo "ok: hello-present $m B with messages, $a B with immediate-abort"
