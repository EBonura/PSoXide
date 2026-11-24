#!/bin/sh
# Build hello-boot (a program on the scene core with boot-only memory) and its two discs, and check
# that psoxide-boot.ld refuses a resident reference into `.boot_inner` and boot-only code in a
# section without the `.text.` prefix, and that a boot-time stack (BOOT_STACK_OVERRIDE) below the
# boot crates' own need and a link without psoxide-core.ld are refused. reserve.bin runs .boot partly
# inside the stack reserve.
#
#   sh sdk/examples/hello-boot/build.sh OUT_DIR      (from the repository root)
#
# Needs psoxide-pgo and mkisopsx built (HOST_BIN, default target/release) and a guest target dir
# (GUEST_TARGET, default build/examples). OUT_DIR gets hello-boot.exe and its map, plain.bin (the
# BIOS boots it) and scenes.bin (the same exe as a scene disc's boot program). Run either headless:
# the TTY ends with `hello-boot: PASS`.
set -eu
OUT="${1:?usage: build.sh OUT_DIR}"
ROOT="$(pwd)"
HOST_BIN="${HOST_BIN:-$ROOT/target/release}"
GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
CRATE=sdk/examples/hello-boot
mkdir -p "$OUT"
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort","-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'
# The order CORE-ABI gives: the RAM size, the core, the boot-only memory, then psoxide.ld.
LINK='"-Clink-arg=-Tram.ld","-Clink-arg=-T../../psoxide-core.ld","-Clink-arg=-T../../psoxide-boot.ld","-Clink-arg=-T../../psoxide.ld"'
EXTRA=""  # a further -T file before the scripts, for the boot-stack builds below
guest() { # extra cargo arguments
    # Cargo does not see the linker scripts change: relink every time.
    touch "$CRATE/src/main.rs"
    "$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --profile "$CRATE/pgo.prof" --variant off -- \
        build --release --target mipsel-sony-psx -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
        --target-dir "$GUEST_TARGET" "$@" --config "target.mipsel-sony-psx.rustflags=[$FLAGS$EXTRA,$LINK]"
}

# Resident code calling into `.boot_inner`: the link must fail, on that reference.
if guest --features cross-ref >"$OUT/cross-ref.log" 2>&1; then
    echo "hello-boot: FAIL: the cross-ref build linked" >&2
    exit 1
fi
grep -q "prohibited cross reference from '.text' to '.text.boot_inner" "$OUT/cross-ref.log" || {
    echo "hello-boot: FAIL: the cross-ref build failed for another reason ($OUT/cross-ref.log)" >&2
    exit 1
}
echo "hello-boot: the cross-ref build is refused, as it must be"
# Boot-only code without the `.text.` prefix: refused too.
if guest --features unprefixed >"$OUT/unprefixed.log" 2>&1; then
    echo "hello-boot: FAIL: the unprefixed build linked" >&2
    exit 1
fi
grep -q "a boot-only section without a .text./.rodata./.data. prefix" "$OUT/unprefixed.log" || {
    echo "hello-boot: FAIL: the unprefixed build failed for another reason ($OUT/unprefixed.log)" >&2
    exit 1
}
echo "hello-boot: the unprefixed build is refused, as it must be"
# psx-core's image linked without psoxide-core.ld: refused too (it would be placed as an orphan).
LINK_ALL="$LINK"
LINK='"-Clink-arg=-Tram.ld","-Clink-arg=-T../../psoxide-boot.ld","-Clink-arg=-T../../psoxide.ld"'
if guest >"$OUT/no-core-script.log" 2>&1; then
    echo "hello-boot: FAIL: the build without psoxide-core.ld linked" >&2
    exit 1
fi
grep -q "linked without psoxide-core.ld" "$OUT/no-core-script.log" || {
    echo "hello-boot: FAIL: the build without psoxide-core.ld failed for another reason ($OUT/no-core-script.log)" >&2
    exit 1
}
echo "hello-boot: the build without psoxide-core.ld is refused, as it must be"
LINK="$LINK_ALL"

guest
cp "$GUEST_TARGET/mipsel-sony-psx/release/hello-boot.exe" "$OUT/hello-boot.exe"
map=$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)
cp "$map" "$OUT/hello-boot.map"
MKISO="$HOST_BIN/mkisopsx"
"$MKISO" --diag --exe "$OUT/hello-boot.exe" --volume HELLOBOOT --out "$OUT/plain.bin"
cat >"$OUT/boot.scenes" <<MANIFEST
program PSX.EXE  -
scene   START    PSX.EXE    start
MANIFEST
# The program's row must cover .boot: its stored core image comes after it (psoxide.ld).
"$MKISO" --diag --exe "$OUT/hello-boot.exe" --scenes "$OUT/boot.scenes" \
    --core-image sdk/crates/psx-core/prebuilt/psx-core.bin --volume HELLOBOOT --out "$OUT/scenes.bin"

# Boot code inside the stack reserve, as a program with no heap to spare runs it: the boot-time
# stack stated (BOOT_STACK_OVERRIDE) below the boot crates' own need is refused...
EXTRA=',"-Clink-arg=-Tboot-stack-small.ld"'
if guest >"$OUT/boot-stack-small.log" 2>&1; then
    echo "hello-boot: FAIL: the boot-stack-small build linked" >&2
    exit 1
fi
grep -q "BOOT_STACK_OVERRIDE is less than the boot crates' own stack" "$OUT/boot-stack-small.log" || {
    echo "hello-boot: FAIL: the boot-stack-small build failed for another reason ($OUT/boot-stack-small.log)" >&2
    exit 1
}
echo "hello-boot: the boot-stack-small build is refused, as it must be"
# ...and one that states enough stack for the boot crates, runs .boot partly inside the reserve and
# passes (reserve.bin: the splash's stack stays clear of the code it runs).
EXTRA=',"-Clink-arg=-Tboot-stack-reserve.ld"'
guest
cp "$GUEST_TARGET/mipsel-sony-psx/release/hello-boot.exe" "$OUT/reserve.exe"
map=$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)
cp "$map" "$OUT/reserve.map"
"$MKISO" --diag --exe "$OUT/reserve.exe" --volume HELLOBOOT --out "$OUT/reserve.bin"
