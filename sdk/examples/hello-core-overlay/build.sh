#!/bin/sh
# Build hello-core-overlay (a program on the scene core with one fixed-address overlay,
# psoxide-overlay.ld), its overlay image and five scene discs.
#
#   sh sdk/examples/hello-core-overlay/build.sh OUT_DIR      (from the repository root)
#
# Needs psoxide-pgo, psoxide-hazard (psx-exe, hazard-scan, check-mfc0) and mkisopsx built
# (HOST_BIN, default target/release) and a guest target dir (GUEST_TARGET, default build/examples).
# OUT_DIR gets hello-core-overlay.exe, its ELF and map, menu.ovl, volatile.bin (the overlay is
# class=volatile: the TTY ends with `hello-core-overlay: PASS`) and swap.bin (class=swap, the old
# behaviour: the TTY ends with `hello-core-overlay: STALE`), before.bin (volatile, placed right
# before WORLD.PAK, header read after the overlay) and tail.bin (the same with tail=1: the
# header comes with the overlay). OUT_DIR/host gets the same files for the host build (feature
# `host`: the overlay runs over a static buffer in `.bss.overlay_host`) and host.bin (volatile,
# before WORLD.PAK, tail=1); OUT_DIR/hostboot the host build with boot-only memory too (feature
# `boot`, psoxide-boot.ld) and hostboot.bin. Every disc but swap ends with `hello-core-overlay: PASS`.
set -eu
OUT="${1:?usage: build.sh OUT_DIR}"
ROOT="$(pwd)"
HOST_BIN="${HOST_BIN:-$ROOT/target/release}"
GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
CRATE=sdk/examples/hello-core-overlay
MKISO="$HOST_BIN/mkisopsx"
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort","-Cllvm-args=-disable-mips-df-succbb-search=false","-Cllvm-args=-disable-mips-df-forward-search=false"'

# WORLD.PAK: one small chunk (the header sector names it) and one of 4 MiB, so the pack reaches
# past LBA 3024 (the far read).
mkdir -p "$OUT/world"
printf "menu art" >"$OUT/world/chunk_1.bin"
head -c 4194304 /dev/zero >"$OUT/world/chunk_2.bin"

build() { # out_dir ram_script [cargo args]
    dir=$1
    ram=$2
    shift 2
    mkdir -p "$dir"
    # The RAM size (and OVERLAY_VMA), the core, the overlays, then psoxide.ld.
    boot=""
    [ -n "${BOOTLD:-}" ] && boot="\"-Clink-arg=-T../../psoxide-boot.ld\","
    link="\"-Clink-arg=-T$ram\",\"-Clink-arg=-T../../psoxide-core.ld\",$boot\"-Clink-arg=-T../../psoxide-overlay.ld\",\"-Clink-arg=-T../../psoxide.ld\""
    # Cargo does not see the linker scripts change: relink every time.
    touch "$CRATE/src/main.rs"
    "$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --profile "$CRATE/pgo.prof" --variant off -- \
        build --release --target mipsel-sony-psx -Zbuild-std=core \
        -Zbuild-std-features=compiler-builtins-mem --target-dir "$GUEST_TARGET" \
        --config "target.mipsel-sony-psx.rustflags=[$FLAGS,$link]" "$@" || return 1
    built="$GUEST_TARGET/mipsel-sony-psx/release/hello-core-overlay"
    cp "$built.exe" "$dir/hello-core-overlay.exe"
    cp "$built.elf" "$dir/hello-core-overlay.elf"
    map=$(ls -t "$GUEST_TARGET"/mipsel-sony-psx/psoxide-pgo-maps/*.map | head -1)
    cp "$map" "$dir/hello-core-overlay.map"
    # The overlay as its own file, and its manifest option (load=0x...). Its code is not in the
    # EXE, so the hazard patcher never saw it: it must scan clean as built.
    LOAD=$("$HOST_BIN/psx-exe" --overlay .overlay.1 "$dir/hello-core-overlay.elf" "$dir/menu.ovl")
    echo "hello-core-overlay: $dir/menu.ovl $(wc -c <"$dir/menu.ovl") bytes, $LOAD"
    ADDR=${LOAD#load=}
    "$HOST_BIN/hazard-scan" --raw "$ADDR" "$dir/menu.ovl"
    "$HOST_BIN/check-mfc0" --raw "$ADDR" "$dir/menu.ovl"
}

disc() { # out_dir name class [placement options]
    # The owner's reserve: the overlay's range and room for its tail.
    reserve="$ADDR-$(printf '0x%x' $((ADDR + 0x40000)))"
    cat >"$1/$2.scenes" <<MANIFEST
program PSX.EXE  -         reserve=$reserve
overlay MENU.OVL menu.ovl  owner=PSX.EXE $LOAD class=$3 ${4:-}
scene   START    PSX.EXE   start
MANIFEST
    "$MKISO" --diag --exe "$1/hello-core-overlay.exe" --scenes "$1/$2.scenes" \
        --core-image sdk/crates/psx-core/prebuilt/psx-core.bin --volume HELLOOVL \
        --world-pack-extra-dir "$OUT/world" --out "$1/$2.bin"
}

build "$OUT" ram.ld
disc "$OUT" volatile volatile
disc "$OUT" swap swap
# The overlay right before the pack: the header is read after it (Half-Life on 16dda36a7)
disc "$OUT" before volatile place=before-world
# ... and carried as its tail, in the same read.
disc "$OUT" tail volatile "place=before-world tail=1"

# The overlay over a static: no OVERLAY_VMA in ram-host.ld.
build "$OUT/host" ram-host.ld --features host
disc "$OUT/host" host volatile "place=before-world tail=1 host"

# The core, boot-only memory and a host in one program (Counter-Strike's shape): links with no
# BOOT_VMA_OVERRIDE, the host starts at .data's end over the boot-only store.
BOOTLD=1 build "$OUT/hostboot" ram-host.ld --features boot
disc "$OUT/hostboot" hostboot volatile "place=before-world tail=1 host"
# ... and the same scripts with nothing boot-only in the program: an empty .boot links and makes a
# valid EXE (the stored size once came out as 0x7fff2000).
BOOTLD=1 build "$OUT/hostboot-empty" ram-host.ld --features host
disc "$OUT/hostboot-empty" hostboot-empty volatile "place=before-world tail=1 host"

# What must not link: OVERLAY_VMA inside .bss (only the host may be overlaid), and a host with
# OVERLAY_VMA as well. Each ram file sits in the crate (the link scripts are found from there).
refused() { # name ram_text [cargo args]
    name=$1
    printf '%s\n' "$2" >"$CRATE/ram-$name.ld"
    shift 2
    if build "$OUT/refused-$name" "ram-$name.ld" "$@" >"$OUT/refused-$name.log" 2>&1; then
        rm -f "$CRATE/ram-$name.ld"
        echo "hello-core-overlay: the $name build linked, it must not" >&2
        exit 1
    fi
    rm -f "$CRATE/ram-$name.ld"
    echo "hello-core-overlay: the $name build is refused, as it must be"
}
refused over-bss "RAM_SIZE_OVERRIDE = 0x1FE000; OVERLAY_VMA = ADDR(.bss);"
refused host-and-vma "RAM_SIZE_OVERRIDE = 0x1FE000; OVERLAY_VMA = 0x80100000;" --features host
