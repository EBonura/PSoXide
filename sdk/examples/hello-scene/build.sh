#!/bin/sh
# Build the scene-core P1 set: the core image and a second one, scene 0 and scene 1 on the core,
# scene 1 on the second image, scene 0 the old way, and the three test discs.
#
#   sh sdk/examples/hello-scene/build-p1.sh OUT_DIR      (from the repository root)
#
# Needs psoxide-pgo, psoxide-hazard and mkisopsx built (HOST_BIN, default target/release) and a guest
# target dir (GUEST_TARGET, default build/examples).
set -eu
OUT="${1:?usage: build-p1.sh OUT_DIR}"
ROOT="$(pwd)"
HOST_BIN="${HOST_BIN:-$ROOT/target/release}"
GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
CRATE=sdk/examples/hello-scene
mkdir -p "$OUT"
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort","-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'
CORE_LINK='"-Clink-arg=-Tram.ld","-Clink-arg=-T../../psoxide-core.ld",'

# A program on the core links an ELF (its core image runs at the window and is stored after .data,
# see sdk/psoxide.ld), which psoxide-pgo writes out as the EXE by load address; the plain one keeps
# --oformat=binary.
guest() { # name features link-scripts
    if [ -n "$3" ]; then format=""; else format=',"-Clink-arg=--oformat=binary"'; fi
    "$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --profile "$CRATE/pgo.prof" --variant off -- \
        build --release --target mipsel-sony-psx -Zbuild-std=core -Zbuild-std-features=compiler-builtins-mem \
        --target-dir "$GUEST_TARGET" $2 \
        --config "target.mipsel-sony-psx.rustflags=[$FLAGS,$3\"-Clink-arg=-T../../psoxide.ld\",\"-Clink-arg=--orphan-handling=error\"$format]"
    cp "$GUEST_TARGET/mipsel-sony-psx/release/hello-scene.exe" "$OUT/$1.exe"
    map=$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map 2>/dev/null | head -1 || true)
    [ -n "$map" ] && cp "$map" "$OUT/$1.map" || true
}

HAZARD_BIN="$HOST_BIN" sh sdk/crates/psx-core/build-image.sh
HAZARD_BIN="$HOST_BIN" sh sdk/crates/psx-core/build-image.sh --alt "$OUT/psx-core-alt.bin"
cp sdk/crates/psx-core/prebuilt/psx-core.bin "$OUT/psx-core.bin"

guest scene-a "" "$CORE_LINK"
guest scene-b "--features role-b" "$CORE_LINK"
PSX_CORE_IMAGE_OVERRIDE="$OUT/psx-core-alt.bin" guest scene-b-alt "--features role-b" "$CORE_LINK"
overlay() { # name features
    (cd "$CRATE/overlay" && cargo build -q --release --target mipsel-sony-psx -Zbuild-std=core \
        -Zbuild-std-features=compiler-builtins-mem --target-dir "$GUEST_TARGET/overlay" $2 \
        --config "target.mipsel-sony-psx.rustflags=[$FLAGS,\"-Clink-arg=-Toverlay.ld\",\"-Clink-arg=--oformat=binary\"]")
    cp "$GUEST_TARGET/overlay/mipsel-sony-psx/release/hello-scene-overlay.exe" "$OUT/$1.bin"
    "$HOST_BIN/hazard-scan" --raw 0x80100000 "$OUT/$1.bin"
}
overlay o1 ""
overlay o2 "--features second"

(cd "$CRATE/corehash" && cargo build -q --release --target-dir "$GUEST_TARGET/host")
HASH="$GUEST_TARGET/host/release/corehash"
H=$("$HASH" "$OUT/psx-core.bin")
HALT=$("$HASH" "$OUT/psx-core-alt.bin")
MENU=$(awk '$NF == "hello_scene_menu" { print "0x" $1; exit }' "$OUT/scene-a.map")
echo "core hash $H, second image $HALT, menu at $MENU"
MKISO="$HOST_BIN/mkisopsx"
walk() { # name B-exe B-options
    cat >"$OUT/$1.scenes" <<MANIFEST
# images (the index is the loaded-mask bit)
program PSX.EXE  -          reserve=0x80100000-0x80110000
overlay O1.BIN   o1.bin     owner=PSX.EXE load=0x80100000 class=swap
overlay O2.BIN   o2.bin     owner=PSX.EXE load=0x80100000 class=swap
program B.EXE    $2  $3
# scenes
scene   START    PSX.EXE    start
scene   MENU     PSX.EXE    entry=$MENU
scene   O1       O1.BIN     entry=0x80100000
scene   O2       O2.BIN     entry=0x80100000
scene   BSTART   B.EXE      start
MANIFEST
    "$MKISO" --diag --exe "$OUT/scene-a.exe" --scenes "$OUT/$1.scenes" --core-image "$OUT/psx-core.bin" \
        --volume PSXSCENE --out "$OUT/$1.bin"
}
# The walk: every scene and image, B on the one core.
walk walk scene-b.exe ""
# B carries the second core and its row says so: the core refuses the program switch (0x23).
walk walk-mism-row scene-b-alt.exe "core-hash=$HALT"
# The row lies (says the running core): the core loads B, B's __core_boot refuses (0x01).
walk walk-mism-win scene-b-alt.exe "core-hash=$H"
# The launcher: the walk with B at image row 8 (five spare one-sector overlays of A, never loaded,
# before it), so psx-nest can put a member disc of up to 8 images there (the nesting rule: the
# outer row's position is at least the member's image count).
head -c 2048 /dev/zero >"$OUT/spare.bin"
cat >"$OUT/launcher.scenes" <<MANIFEST
program PSX.EXE  -          reserve=0x80100000-0x80110000
overlay O1.BIN   o1.bin     owner=PSX.EXE load=0x80100000 class=swap
overlay O2.BIN   o2.bin     owner=PSX.EXE load=0x80100000 class=swap
overlay S1.BIN   spare.bin  owner=PSX.EXE load=0x80108000 class=swap
overlay S2.BIN   spare.bin  owner=PSX.EXE load=0x80108000 class=swap
overlay S3.BIN   spare.bin  owner=PSX.EXE load=0x80108000 class=swap
overlay S4.BIN   spare.bin  owner=PSX.EXE load=0x80108000 class=swap
overlay S5.BIN   spare.bin  owner=PSX.EXE load=0x80108000 class=swap
program B.EXE    scene-b.exe
scene   START    PSX.EXE    start
scene   MENU     PSX.EXE    entry=$MENU
scene   O1       O1.BIN     entry=0x80100000
scene   O2       O2.BIN     entry=0x80100000
scene   BSTART   B.EXE      start
MANIFEST
"$MKISO" --diag --exe "$OUT/scene-a.exe" --scenes "$OUT/launcher.scenes" --core-image "$OUT/psx-core.bin" \
    --volume PSXSCENE --out "$OUT/launcher.bin"
ls -la "$OUT"
