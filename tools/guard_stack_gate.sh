#!/bin/sh
# Gate: the call tree of a direct GPU write holds no call through a register.
#
# Once the present path has armed the direct-access guard, the first direct port write runs it
# (psx_io::gpu::handoff::write_command_slow > run_armed_guard). On the console that guard is psx-rt's
# `wait_idle` (symbol `__psx_rt_wait_idle`), called directly at the address the linker script binds
# to `__psx_direct_access_guard`; a registered function pointer would make the tree unprovable
# ("calls through a register") for overlay and scratchpad-stack code, and a game, not the SDK,
# would find out. This builds sdk/examples/hello-present (it arms the guard and then writes
# directly) and walks the tree rooted at write_command_slow with stack-guard: it must be bounded,
# and it must reach the guard's wait (so the root really exercises it).
#
# Run as: sh tools/guard_stack_gate.sh   Needs the pinned nightly (rust-toolchain.toml) and its rust-src.
set -eu
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
TARGET=mipsel-sony-psx
cd "$ROOT"
make -s -f tools/sdk-examples.mk example EXAMPLE=hello-present BUILD="$WORK/build" >"$WORK/build.log" 2>&1 || {
    cat "$WORK/build.log" >&2
    echo "guard-stack-gate: FAIL: the hello-present build failed" >&2
    exit 1
}
MAP="$(ls -t "$WORK"/build/$TARGET/psoxide-pgo-maps/*.map | head -n 1)"
cargo build -q --release --locked -p psoxide-hazard --bin stack-guard
GUARD="$(ls "${CARGO_TARGET_DIR:-target}/release/stack-guard")"
if ! "$GUARD" "$WORK/build/$TARGET/release/hello-present.exe" "$MAP" \
    --root 'psx_io::gpu::handoff::write_command_slow' --budget 4096 >"$WORK/guard.txt" 2>&1; then
    cat "$WORK/guard.txt" >&2
    echo "guard-stack-gate: FAIL: the tree of a direct write is not bounded (a call through a register?)" >&2
    exit 1
fi
if ! grep -q '__psx_rt_wait_idle' "$WORK/guard.txt"; then
    cat "$WORK/guard.txt" >&2
    echo "guard-stack-gate: FAIL: the tree found does not reach the guard's wait (the root matched nothing useful)" >&2
    exit 1
fi
echo "guard-stack-gate: PASS (the tree of a direct write is bounded and reaches the guard)"
