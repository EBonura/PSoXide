#!/bin/sh
# Build hello-modular: programs P and Q on the scene core, their overlays, and two member discs
# whose scenes declare the images they need (`needs=`).
#
#   sh sdk/examples/hello-modular/build.sh OUT_DIR      (from the repository root)
#
# Needs psoxide-pgo, psoxide-hazard (psx-exe, hazard-scan, check-mfc0) and mkisopsx built
# (HOST_BIN, default target/release) and a guest target dir (GUEST_TARGET, default build/examples).
# OUT_DIR gets p.exe, q.exe (with their ELFs and maps), the overlay files, member.bin (the walk:
# the TTY ends with `hello-modular: PASS`) and soak.bin (P and Q switch to each other SOAK times,
# default 300: `hello-modular: SOAK PASS entries ...`). A launcher nests either with psx-nest.
set -eu
OUT="${1:?usage: build.sh OUT_DIR}"
ROOT="$(pwd)"
HOST_BIN="${HOST_BIN:-$ROOT/target/release}"
GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
SOAK="${SOAK:-300}"
CRATE=sdk/examples/hello-modular
MKISO="$HOST_BIN/mkisopsx"
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort","-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'
mkdir -p "$OUT/world"

# WORLD.PAK: TAILED's tail is its header sector.
printf "world" >"$OUT/world/chunk_1.bin"

build() { # name ram_script [cargo args]
    name=$1
    ram=$2
    shift 2
    link="\"-Clink-arg=-T$ram\",\"-Clink-arg=-T../../psoxide-core.ld\",\"-Clink-arg=-T../../psoxide-overlay.ld\",\"-Clink-arg=-T${MAIN_LD:-../../psoxide.ld}\"${ORPHAN-,\"-Clink-arg=--orphan-handling=error\"}"
    # Cargo does not see the linker scripts change: relink every time.
    touch "$CRATE/src/main.rs"
    "$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --profile "$CRATE/pgo.prof" --variant off -- \
        build --release --target mipsel-sony-psx -Zbuild-std=core \
        -Zbuild-std-features=compiler-builtins-mem --target-dir "$GUEST_TARGET" \
        --config "target.mipsel-sony-psx.rustflags=[$FLAGS,$link]" "$@" || return 1
    built="$GUEST_TARGET/mipsel-sony-psx/release/hello-modular"
    cp "$built.exe" "$OUT/$name.exe"
    cp "$built.elf" "$OUT/$name.elf"
    cp "$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)" "$OUT/$name.map"
}

overlay() { # program overlay-number file: write the overlay, scan it, print its load= option
    load=$("$HOST_BIN/psx-exe" --overlay ".overlay.$2" "$OUT/$1.elf" "$OUT/$3")
    "$HOST_BIN/hazard-scan" --raw "${load#load=}" "$OUT/$3" >&2
    "$HOST_BIN/check-mfc0" --raw "${load#load=}" "$OUT/$3" >&2
    echo "$load"
}

symbol() { # map name
    awk -v n="$2" '$NF == n { print "0x" $1; exit }' "$OUT/$1.map"
}

build p ram.ld
build q ram-host.ld --features role-q

# A program on its own copy of the main script (the way a psyq-style program brings its own) links
# the same EXE; a copy that leaves out the entry sections (`.text_entry.*`) is refused.
cp sdk/psoxide.ld "$CRATE/own.ld"
MAIN_LD=own.ld build own ram.ld >"$OUT/own.log" 2>&1 || { rm -f "$CRATE/own.ld"; cat "$OUT/own.log" >&2; exit 1; }
cmp -s "$OUT/own.exe" "$OUT/p.exe" || { rm -f "$CRATE/own.ld"; echo "hello-modular: the own-script EXE differs" >&2; exit 1; }
echo "hello-modular: the own-script build links the same EXE"
grep -v 'text_entry .text_entry' sdk/psoxide.ld >"$CRATE/own.ld"
if MAIN_LD=own.ld build forgot ram.ld >"$OUT/forgot.log" 2>&1; then
    rm -f "$CRATE/own.ld"
    echo "hello-modular: the build whose script forgets the entry sections linked, it must not" >&2
    exit 1
fi
grep -q "is being placed in '.text_entry" "$OUT/forgot.log" || {
    rm -f "$CRATE/own.ld"
    echo "hello-modular: the forgot-entry build failed for another reason ($OUT/forgot.log)" >&2
    exit 1
}
# ... and without --orphan-handling=error, psoxide-core.ld's own check refuses it.
if ORPHAN="" MAIN_LD=own.ld build forgot ram.ld >"$OUT/forgot-quiet.log" 2>&1; then
    rm -f "$CRATE/own.ld"
    echo "hello-modular: the forgot-entry build without the orphan check linked, it must not" >&2
    exit 1
fi
rm -f "$CRATE/own.ld"
grep -q "__core_boot is outside .text" "$OUT/forgot-quiet.log" || {
    echo "hello-modular: the quiet forgot-entry build failed for another reason ($OUT/forgot-quiet.log)" >&2
    exit 1
}
echo "hello-modular: the build whose script forgets the entry sections is refused, as it must be"
SVC=$(overlay p 1 svc.ovl)
VOL=$(overlay p 2 vol.ovl)
TAILED=$(overlay p 3 tailed.ovl)
HOSTED=$(overlay q 1 hosted.ovl)
HOST_AT=${HOSTED#load=}

# CARRIED: P's 2 KiB `CARRIED` static, cut from P's EXE (header sector, then the payload from
# 0x80010000), so the overlay file is byte for byte what P's payload holds there.
CARRIED_AT=$(symbol p CARRIED)
dd if="$OUT/p.exe" of="$OUT/carried.ovl" bs=2048 skip=$(( (CARRIED_AT - 0x80010000) / 2048 + 1 )) \
    count=1 2>/dev/null
PPLAY=$(symbol p hello_modular_pplay)
PVOL=$(symbol p hello_modular_pvol)
echo "hello-modular: CARRIED at $CARRIED_AT, PPLAY $PPLAY, PVOL $PVOL, $SVC $VOL $TAILED $HOSTED"

disc() { # name [extra scene line]
    cat >"$OUT/$1.scenes" <<MANIFEST
program PSX.EXE     -            reserve=0x80010000-0x80140000 carries=CARRIED.OVL
program Q.EXE       q.exe        reserve=$HOST_AT-$(printf '0x%x' $((HOST_AT + 0x10000)))
overlay CARRIED.OVL carried.ovl  owner=PSX.EXE load=$CARRIED_AT class=swap
overlay SVC.OVL     svc.ovl      owner=PSX.EXE $SVC class=swap
overlay VOL.OVL     vol.ovl      owner=PSX.EXE $VOL class=volatile
overlay TAILED.OVL  tailed.ovl   owner=PSX.EXE $TAILED class=swap place=before-world tail=1
overlay HOSTED.OVL  hosted.ovl   owner=Q.EXE $HOSTED class=swap host
scene   PSTART      PSX.EXE      start needs=SVC.OVL,CARRIED.OVL
scene   PPLAY       PSX.EXE      entry=$PPLAY needs=TAILED.OVL,CARRIED.OVL
scene   PVOL        PSX.EXE      entry=$PVOL needs=VOL.OVL
scene   QSTART      Q.EXE        start needs=HOSTED.OVL
${2:-}
MANIFEST
    "$MKISO" --diag --exe "$OUT/p.exe" --scenes "$OUT/$1.scenes" \
        --core-image sdk/crates/psx-core/prebuilt/psx-core.bin --volume HELLOMOD \
        --world-pack-extra-dir "$OUT/world" --out "$OUT/$1.bin"
}

disc member
disc soak "scene   SOAK$SOAK     PSX.EXE      entry=$PPLAY"
