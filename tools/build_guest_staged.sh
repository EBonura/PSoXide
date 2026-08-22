#!/bin/sh
# Build the MIPS guest (engine/examples/editor-playtest) from a canonical
# staging path so the artifact does not depend on where the repository is
# checked out.
#
# Why this exists: cargo derives per-crate metadata from the absolute
# workspace path. Two worktrees with byte-identical sources AND byte-identical
# cooked `generated/` content produced same-size guest binaries differing in
# 312,194 bytes, because the differing metadata hash reorders codegen
# (docs/quake-psoxide-convergence-handoff.md section 0.20.2). Gameplay was
# unaffected, but VRAM pins in the replay gates became per-checkout, which
# makes them useless as shared regression evidence.
#
# The fix is the one Quake already uses (host/quake-build/main.rs): mirror the
# guest source closure into a fixed path, build there with an isolated Cargo
# home, and copy the artifact back. The stage is a single canonical directory
# rather than a content-addressed one on purpose: the cooked `generated/` tree
# changes on every Play, and content addressing would mean a full LTO rebuild
# each time. One stage plus cargo's own fingerprints keeps the edit loop
# incremental while still building at a fixed path.
#
# Environment:
#   PSOXIDE_GUEST_STAGE=0          build in-tree, exactly as before (escape hatch)
#   PSOXIDE_GUEST_STAGE_ROOT=<dir> override the canonical root
#   PSOXIDE_GUEST_CARGO_HOME=<dir> override the isolated Cargo home
#
# Every argument is forwarded verbatim to `cargo build --release`; the
# Makefile passes the target/build-std flags and the feature selection, and
# lets its own recipe shell do the quote removal.
#
# Run as: make build-editor-playtest
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GUEST_DIR="engine/examples/editor-playtest"
EXE_RELATIVE="build/examples/mipsel-sony-psx/release/editor-playtest.exe"
# The linker script is reached relative to the guest crate directory, so the
# stage has to mirror the repository layout rather than flatten it.
RUSTFLAGS_VALUE="-Zunstable-options -Cpanic=immediate-abort -Clink-arg=-T../../../sdk/psoxide.ld -Clink-arg=--oformat=binary"

# Everything the guest link closure reads. Directories are mirrored with
# --delete so a removed source file cannot survive in the stage.
CLOSURE="rust-toolchain.toml
crates/psx-hw
editor/crates/psxed-format
sdk/psoxide.ld
sdk/Cargo.toml
sdk/crates
engine/Cargo.toml
engine/crates
$GUEST_DIR"

if [ "${PSOXIDE_GUEST_STAGE:-1}" = "0" ]; then
    echo "[guest-build] PSOXIDE_GUEST_STAGE=0: building in-tree (artifact is checkout-specific)"
    cd "$ROOT/$GUEST_DIR"
    CARGO_TARGET_DIR="$ROOT/build/examples" RUSTFLAGS="$RUSTFLAGS_VALUE" \
        exec cargo build --release "$@"
fi

STAGE_ROOT="${PSOXIDE_GUEST_STAGE_ROOT:-/tmp/psoxide-psx-guest-v1}"
STAGE="$STAGE_ROOT/stage"
LOCK="$STAGE_ROOT/lock"
GUEST_CARGO_HOME="${PSOXIDE_GUEST_CARGO_HOME:-$STAGE_ROOT/cargo-home}"

mkdir -p "$STAGE_ROOT"

# mkdir is the portable atomic lock: macOS has no flock(1). A concurrent
# build from another checkout waits rather than corrupting the shared stage.
waited=0
until mkdir "$LOCK" 2>/dev/null; do
    if [ "$waited" -ge 600 ]; then
        echo "[guest-build] stage lock $LOCK held for 10 minutes; remove it if stale" >&2
        exit 1
    fi
    [ "$waited" -eq 0 ] && echo "[guest-build] waiting for the canonical stage lock"
    sleep 1
    waited=$((waited + 1))
done
trap 'rmdir "$LOCK" 2>/dev/null || true' EXIT HUP INT TERM

for entry in $CLOSURE; do
    source="$ROOT/$entry"
    [ -e "$source" ] || { echo "[guest-build] missing guest source: $source" >&2; exit 1; }
    destination="$STAGE/$entry"
    if [ -d "$source" ]; then
        mkdir -p "$destination"
        # Cargo fingerprints use mtimes. `rsync -a` preserved a checkout's
        # older source time, so changing the canonical stage from a newer
        # worktree to older-dated DIFFERENT content could leave a newer guest
        # artifact falsely fresh. Compare content, retain untouched files, but
        # give every changed destination the current copy time.
        rsync -rlp --checksum --delete --exclude '/target/' "$source/" "$destination/"
    else
        mkdir -p "$(dirname "$destination")"
        rsync -lp --checksum "$source" "$destination"
    fi
done

# The repository root is a workspace whose members are the HOST crates; the
# guest closure needs only two of them, and staging the rest would mean
# staging the emulator and editor as well. Rewrite the member list and keep
# every other line byte-identical, so the inherited [workspace.package],
# [workspace.lints] and [workspace.dependencies] values cannot drift from the
# real manifest the way a hand-copied stub would.
python3 - "$ROOT/Cargo.toml" "$STAGE/Cargo.toml" <<'PY'
import sys

source, destination = sys.argv[1], sys.argv[2]
text = open(source, encoding="utf-8").read()
start = text.index("members = [")
end = text.index("]", start) + 1
staged = (
    text[:start]
    + 'members = [\n    "crates/psx-hw",\n    "editor/crates/psxed-format",\n]'
    + text[end:]
)
try:
    if open(destination, encoding="utf-8").read() == staged:
        sys.exit(0)
except OSError:
    pass
open(destination, "w", encoding="utf-8").write(staged)
PY

mkdir -p "$GUEST_CARGO_HOME"
cd "$STAGE/$GUEST_DIR"
CARGO_HOME="$GUEST_CARGO_HOME" \
CARGO_TARGET_DIR="$STAGE/build/examples" \
RUSTFLAGS="$RUSTFLAGS_VALUE" \
    cargo build --release "$@"

staged_exe="$STAGE/$EXE_RELATIVE"
[ -f "$staged_exe" ] || { echo "[guest-build] staged build produced no $EXE_RELATIVE" >&2; exit 1; }
mkdir -p "$ROOT/$(dirname "$EXE_RELATIVE")"
cp "$staged_exe" "$ROOT/$EXE_RELATIVE"
echo "[guest-build] canonical stage $STAGE"
echo "[guest-build] $(cd "$ROOT" && shasum -a 256 "$EXE_RELATIVE")"
