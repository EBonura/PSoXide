#!/bin/sh
# Build hello-core-overlay (a program on the scene core with one fixed-address overlay,
# psoxide-overlay.ld), its overlay image and two scene discs.
#
#   sh sdk/examples/hello-core-overlay/build.sh OUT_DIR      (from the repository root)
#
# Needs psoxide-pgo, psoxide-hazard (psx-exe, hazard-scan, check-mfc0) and mkisopsx built
# (HOST_BIN, default target/release) and a guest target dir (GUEST_TARGET, default build/examples).
# OUT_DIR gets hello-core-overlay.exe, its ELF and map, menu.ovl, volatile.bin (the overlay is
# class=volatile: the TTY ends with `hello-core-overlay: PASS`) and swap.bin (class=swap, the old
# behaviour: the TTY ends with `hello-core-overlay: STALE`).
set -eu
OUT="${1:?usage: build.sh OUT_DIR}"
ROOT="$(pwd)"
HOST_BIN="${HOST_BIN:-$ROOT/target/release}"
GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
CRATE=sdk/examples/hello-core-overlay
mkdir -p "$OUT"
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort","-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'
# The RAM size and OVERLAY_VMA, the core, the overlays, then psoxide.ld.
LINK='"-Clink-arg=-Tram.ld","-Clink-arg=-T../../psoxide-core.ld","-Clink-arg=-T../../psoxide-overlay.ld","-Clink-arg=-T../../psoxide.ld"'
# Cargo does not see the linker scripts change: relink every time.
touch "$CRATE/src/main.rs"
"$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --profile "$CRATE/pgo.prof" --variant off -- \
    build --release --target mipsel-sony-psx -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
    --target-dir "$GUEST_TARGET" --config "target.mipsel-sony-psx.rustflags=[$FLAGS,$LINK]"
BUILT="$GUEST_TARGET/mipsel-sony-psx/release/hello-core-overlay"
cp "$BUILT.exe" "$OUT/hello-core-overlay.exe"
cp "$BUILT.elf" "$OUT/hello-core-overlay.elf"
map=$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)
cp "$map" "$OUT/hello-core-overlay.map"

# The overlay as its own file, and its manifest option (load=0x...). Its code is not in the EXE,
# so the hazard patcher never saw it: it must scan clean as built.
LOAD=$("$HOST_BIN/psx-exe" --overlay .overlay.1 "$OUT/hello-core-overlay.elf" "$OUT/menu.ovl")
echo "hello-core-overlay: menu.ovl $(wc -c <"$OUT/menu.ovl") bytes, $LOAD"
ADDR=${LOAD#load=}
"$HOST_BIN/hazard-scan" --raw "$ADDR" "$OUT/menu.ovl"
"$HOST_BIN/check-mfc0" --raw "$ADDR" "$OUT/menu.ovl"

MKISO="$HOST_BIN/mkisopsx"
disc() { # name class
    cat >"$OUT/$1.scenes" <<MANIFEST
program PSX.EXE  -         reserve=0x80100000-0x80140000
overlay MENU.OVL menu.ovl  owner=PSX.EXE $LOAD class=$2
scene   START    PSX.EXE   start
MANIFEST
    "$MKISO" --diag --exe "$OUT/hello-core-overlay.exe" --scenes "$OUT/$1.scenes" \
        --core-image sdk/crates/psx-core/prebuilt/psx-core.bin --volume HELLOOVL --out "$OUT/$1.bin"
}
disc volatile volatile
disc swap swap
