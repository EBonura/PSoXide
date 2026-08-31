#!/bin/sh
# Catch miscompilation of the guest on mipsel-sony-psx.
#
# Runs one fixed collision workload twice, natively and on the emulated
# console, and compares the resulting hash. Both sides call the same function,
# psx_bsp::toolchain_probe::compute_hash, so a difference in the hash is a
# difference in generated code and nothing else.
#
#   tools/toolchain_check.sh                 # current guest flags
#   GUEST_RUSTFLAGS_EXTRA=-Copt-level=z \
#     tools/toolchain_check.sh               # try a specific configuration
#
# Exits non-zero on a mismatch. Re-run after a toolchain bump and after
# changing guest codegen flags; see the module doc for what it has caught.
set -eu

ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
MAP="${TOOLCHAIN_CHECK_MAP:-$ROOT/engine/examples/editor-playtest/generated/brush_world.pxbsp}"
TARGET_DIR="${TOOLCHAIN_CHECK_TARGET_DIR:-/tmp/psoxide-toolchain-check}"
OUT="$TARGET_DIR/frames"

if [ ! -f "$MAP" ]; then
    echo "toolchain-check: no cooked map at $MAP" >&2
    echo "  cook a project first, or set TOOLCHAIN_CHECK_MAP" >&2
    exit 2
fi

echo "toolchain-check: map $(basename "$MAP")"

# The native reference. Stable across host optimisation levels by construction:
# it is ordinary Rust doing integer arithmetic.
ORACLE="$(cd "$ROOT/engine" && cargo run -q -p psx-bsp --example toolchain_oracle -- "$MAP" | tail -1)"
echo "  oracle $ORACLE"

# The guest. Built with the same flags the shipping guest uses, plus anything
# the caller wants to vary.
GUEST_FLAGS="-Zunstable-options -Cpanic=immediate-abort -Cllvm-args=-disable-mips-df-backward-search ${GUEST_RUSTFLAGS_EXTRA:-} -Clink-arg=-T../../../sdk/psoxide.ld -Clink-arg=--oformat=binary"
(
    cd "$ROOT/engine/examples/toolchain-probe"
    RUSTFLAGS="$GUEST_FLAGS" CARGO_TARGET_DIR="$TARGET_DIR/guest" \
        cargo build --release --target mipsel-sony-psx \
        -Zjson-target-spec -Zbuild-std=core,alloc \
        -Zbuild-std-features=compiler-builtins-mem >/dev/null 2>&1
)
EXE="$TARGET_DIR/guest/mipsel-sony-psx/release/toolchain-probe.exe"

rm -rf "$OUT"
mkdir -p "$OUT"
(
    cd "$ROOT/emu"
    cargo run -q -p frontend --release -- launch --path "$EXE" \
        --steps 1500000000 --route-screenshot-dir "$OUT" \
        --route-screenshot-interval 500 >/dev/null 2>&1
)

# The guest has no console, so it paints the hash as 32 cells along the top of
# the framebuffer and we read them back out of a screenshot.
GUEST="$(python3 - "$OUT" <<'PY'
import glob, sys
frames = sorted(glob.glob(sys.argv[1] + "/*.ppm"))
if not frames:
    print("no-frames")
    raise SystemExit
data = open(frames[-1], "rb").read()
pixels = data[data.index(b"255\n") + 4:]
WIDTH, ROW, CELL = 320, 10, 8
bits = "".join(
    "1" if pixels[((ROW * WIDTH) + (bit * CELL + CELL // 2)) * 3] > 128 else "0"
    for bit in range(32)
)
print(f"0x{int(bits, 2):08x}")
PY
)"
echo "  guest  $GUEST"

if [ "$GUEST" = "$ORACLE" ]; then
    echo "toolchain-check: OK"
else
    echo "toolchain-check: MISMATCH -- the guest miscompiles" >&2
    echo "  the emulated console and the host disagree on identical source." >&2
    echo "  Bisect with GUEST_RUSTFLAGS_EXTRA, e.g. -Copt-level=2 vs -Copt-level=z," >&2
    echo "  and see engine/crates/psx-bsp/src/toolchain_probe.rs." >&2
    exit 1
fi
