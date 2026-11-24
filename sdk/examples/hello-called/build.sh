#!/bin/sh
# Build hello-called: a called program (psx-rt feature `called-program`, linked at 0x80100000)
# and the caller that embeds it and calls it twice, as Hollow Knight calls its intro blob.
#
#   sh sdk/examples/hello-called/build.sh OUT_DIR      (from the repository root)
#
# Needs psoxide-pgo built (HOST_BIN, default target/release) and a guest target dir (GUEST_TARGET,
# default build/examples). OUT_DIR gets blob.exe and caller.exe; the caller's TTY ends with
# `hello-called: PASS` when its VRAM, display mode and loading frame survive both calls.
set -eu
OUT="${1:?usage: build.sh OUT_DIR}"
ROOT="$(pwd)"
HOST_BIN="${HOST_BIN:-$ROOT/target/release}"
GUEST_TARGET="${GUEST_TARGET:-$ROOT/build/examples}"
CRATE=sdk/examples/hello-called
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
FLAGS='"-Zunstable-options","-Cpanic=immediate-abort"'
build() { # ram_script [cargo args]
    ram=$1
    shift
    link="${ram:+\"-Clink-arg=-T$ram\",}\"-Clink-arg=-T../../psoxide.ld\",\"-Clink-arg=--orphan-handling=error\""
    touch "$CRATE/src/main.rs"
    "$HOST_BIN/psoxide-pgo" apply --crate "$CRATE" --variant off -- \
        build --release --target mipsel-sony-psx -Zbuild-std=core \
        -Zbuild-std-features=compiler-builtins-mem --target-dir "$GUEST_TARGET" \
        --config "target.mipsel-sony-psx.rustflags=[$FLAGS,$link]" "$@"
}
build blob.ld --features blob
cp "$GUEST_TARGET/mipsel-sony-psx/release/hello-called.exe" "$OUT/blob.exe"
HELLO_CALLED_BLOB="$OUT/blob.exe" build ""
cp "$GUEST_TARGET/mipsel-sony-psx/release/hello-called.exe" "$OUT/caller.exe"
echo "hello-called: $OUT/blob.exe $(wc -c <"$OUT/blob.exe") bytes, $OUT/caller.exe"
