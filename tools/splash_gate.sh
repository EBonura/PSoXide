#!/bin/sh
# Gate: the boot splash's call tree never reaches psx-rt's I-cache flush.
#
# A program that runs its title code from an overlay proves statically (stack-guard --forbid)
# that the overlay's call tree cannot reach `__psx_rt_flush_i_cache`, which runs with the
# scratchpad unmapped. `psx_splash::show` waits with `try_wait_vblank`, which never installs
# the VBlank counter (`install_vblank_counter` flushes the cache), so the proof holds for it.
# This builds sdk/examples/hello-splash and walks the tree of every `psx_splash::show`.
#
# The walk also stops at `DoubleBuffer::swap`, which calls psx-gpu's present hook through a
# register ("the callee cannot be bounded"); that is not what this gate is about, so the gate
# fails only when a tree is reported to REACH the forbidden function.
#
# Run as: sh tools/splash_gate.sh   Needs the pinned nightly (rust-toolchain.toml) and its rust-src.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
TARGET=mipsel-sony-psx
cd "$ROOT"
make -s -f tools/sdk-examples.mk example EXAMPLE=hello-splash BUILD="$WORK/build" >"$WORK/build.log" 2>&1 || {
    cat "$WORK/build.log" >&2
    echo "splash-gate: FAIL: the hello-splash build failed" >&2
    exit 1
}
MAP="$(ls -t "$WORK"/build/$TARGET/psoxide-pgo-maps/*.map | head -n 1)"
cargo build -q --release --locked -p psoxide-hazard --bin stack-guard
GUARD="$(ls "${CARGO_TARGET_DIR:-target}/release/stack-guard")"
"$GUARD" "$WORK/build/$TARGET/release/hello-splash.exe" "$MAP" --root 'psx_splash.*show' \
    --budget 32768 --forbid __psx_rt_flush_i_cache >"$WORK/guard.txt" 2>&1 || true
if grep -q 'reaches __psx_rt_flush_i_cache' "$WORK/guard.txt"; then
    cat "$WORK/guard.txt" >&2
    echo "splash-gate: FAIL: the splash's call tree reaches the I-cache flush" >&2
    exit 1
fi
if ! grep -q 'psx_splash::show' "$WORK/guard.txt"; then
    cat "$WORK/guard.txt" >&2
    echo "splash-gate: FAIL: the guard found no psx_splash::show tree to walk (the root pattern matched nothing)" >&2
    exit 1
fi
echo "splash-gate: PASS (no tree of psx_splash::show reaches __psx_rt_flush_i_cache)"
