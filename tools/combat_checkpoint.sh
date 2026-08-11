#!/bin/sh
# Combat checkpoint regression gate for the tracked BSP combat fixture.
#
# From clean inputs: regenerates the fixture (and fails if the generator
# disagrees with the tracked copy), recooks into a wiped generated/ tree,
# builds the real MIPS guest and disc, replays the canonical combat tape
# twice and the door-occlusion tape once, then asserts the authored combat
# counters, the one-hit-per-swing totals, the post-kill traversal position,
# and byte-identical hashes across the two runs and against the pinned
# expectations below.
#
# Run as: make combat-checkpoint
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/psoxide-combat-checkpoint"
FIXTURE="editor/projects/brush-combat-fixture"
GENERATED="engine/examples/editor-playtest/generated"
CUE="build/examples/mipsel-sony-psx/release/editor-playtest.cue"

# Pinned expectations for the canonical tape. Any intentional content or
# engine change that shifts these must update them in the same commit, with
# the replay evidence in the commit message.
EXPECT_MELEE_HITS=4
EXPECT_STAGGERS=1
EXPECT_DEATHS=1
EXPECT_HITS_TAKEN=3
EXPECT_PLAYER_X_BIASED=1003625
# PXBSP-native world-content pass: static brush geometry, the live enemy,
# both weapons, and the HUD are present without a synthetic PSXW room.
EXPECT_VRAM_HASH=0x007fb6683f98d82b
EXPECT_DISPLAY_HASH=0x0739fbd9575fdbb8

fail() {
    echo "combat-checkpoint: FAIL: $1" >&2
    exit 1
}

counter() {
    # Zero counters are omitted from the profile dump; missing means 0.
    awk -v name="$2" '$0 ~ name { for (i=1;i<=NF;i++) if ($i ~ /^total=/) { sub("total=","",$i); print $i; exit } }' "$1" | head -1
}

counter_or_zero() {
    value="$(counter "$1" "$2")"
    echo "${value:-0}"
}

assert_eq() {
    [ "$2" = "$3" ] || fail "$1: got '$2', expected '$3'"
}

cd "$ROOT"
mkdir -p "$OUT"

echo "combat-checkpoint: regenerating fixture"
rm -rf "$OUT/fixture-regen"
(cd editor && cargo run -p psxed-project --bin gen_brush_combat_fixture --quiet -- "$OUT/fixture-regen")
diff -r "$OUT/fixture-regen" "$FIXTURE" >/dev/null 2>&1 \
    || fail "generator output differs from the tracked fixture; regenerate and commit"

echo "combat-checkpoint: clean cook"
rm -rf "$GENERATED"
# The generated tree also carries the TRACKED placeholder manifest; put it
# back so a checkpoint run leaves a clean worktree.
git checkout -- "$GENERATED"
(cd editor && cargo run -p psxed-project --bin cook-playtest --quiet -- projects/brush-combat-fixture/project.ron >/dev/null)

echo "combat-checkpoint: MIPS guest"
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry" >/dev/null

echo "combat-checkpoint: disc"
(cd tools/mkisopsx && cargo run --release --quiet -- \
    --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
    --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
    --volume PSOXIDE --cdtest-sectors 32 \
    --world-pack-rooms-dir "../../$GENERATED/stream_chunks" \
    --world-pack-order-file "../../$GENERATED/world_pack_order.txt" \
    --ui-pack-dir "../../$GENERATED/ui_stream_chunks" \
    --ui-pack-order-file "../../$GENERATED/ui_pack_order.txt" \
    --cdda-track-list "../../$GENERATED/cdda_tracks.txt" >/dev/null)

if [ ! -x target/release/frontend ]; then
    echo "combat-checkpoint: building frontend"
    (cd emu && cargo build -p frontend --release --quiet)
fi

for RUN in 1 2; do
    echo "combat-checkpoint: canonical replay $RUN/2"
    ./target/release/frontend launch --path "$CUE" --embedded-playtest \
        --input-tape "$FIXTURE/combat-checkpoint.pxitape.csv" \
        --steps 6000000000 --dump-guest-profile --dump-hash \
        > "$OUT/main-$RUN.txt" 2>&1
done

echo "combat-checkpoint: door occlusion replay"
./target/release/frontend launch --path "$CUE" --embedded-playtest \
    --input-tape "$FIXTURE/door-blocks-damage.pxitape.csv" \
    --steps 6000000000 --dump-guest-profile --dump-hash \
    > "$OUT/door.txt" 2>&1

MAIN="$OUT/main-1.txt"
assert_eq "authored player melee hits" \
    "$(counter_or_zero "$MAIN" "player melee hits")" "$EXPECT_MELEE_HITS"
assert_eq "stagger enters" \
    "$(counter_or_zero "$MAIN" "game entity stagger enters")" "$EXPECT_STAGGERS"
assert_eq "enemy deaths" \
    "$(counter_or_zero "$MAIN" "game entity deaths")" "$EXPECT_DEATHS"
assert_eq "fallback hits taken" \
    "$(counter_or_zero "$MAIN" "player hits taken")" "$EXPECT_HITS_TAKEN"

PLAYER_X="$(awk '/player local x/ { for (i=1;i<=NF;i++) if ($i ~ /^latest=/) { sub("latest=","",$i); print $i; exit } }' "$MAIN")"
assert_eq "post-kill traversal x (biased)" "$PLAYER_X" "$EXPECT_PLAYER_X_BIASED"

VRAM_1="$(awk -F= '/^vram_fnv1a_64=/{print $2}' "$MAIN")"
DISPLAY_1="$(awk -F= '/^display_fnv1a_64=/{print $2; exit}' "$MAIN" | awk '{print $1}')"
VRAM_2="$(awk -F= '/^vram_fnv1a_64=/{print $2}' "$OUT/main-2.txt")"
DISPLAY_2="$(awk -F= '/^display_fnv1a_64=/{print $2; exit}' "$OUT/main-2.txt" | awk '{print $1}')"
assert_eq "run-to-run VRAM determinism" "$VRAM_1" "$VRAM_2"
assert_eq "run-to-run display determinism" "$DISPLAY_1" "$DISPLAY_2"
assert_eq "pinned VRAM hash" "$VRAM_1" "$EXPECT_VRAM_HASH"
assert_eq "pinned display hash" "$DISPLAY_1" "$EXPECT_DISPLAY_HASH"

# Door tape contract: exactly the scripted player attempts happened, the
# door's logic never fired (so it never opened), the enemy swung the pinned
# number of times, and not one connection crossed the closed door in either
# direction.
EXPECT_DOOR_PLAYER_ATTEMPTS=3
EXPECT_DOOR_ENEMY_SWINGS=1
DOOR="$OUT/door.txt"
assert_eq "door tape: player attack attempts" \
    "$(counter_or_zero "$DOOR" "player attack starts")" "$EXPECT_DOOR_PLAYER_ATTEMPTS"
assert_eq "door tape: door logic never fired" \
    "$(counter_or_zero "$DOOR" "logic records fired")" "0"
assert_eq "door tape: enemy attack attempts" \
    "$(counter_or_zero "$DOOR" "game entity attack enters")" "$EXPECT_DOOR_ENEMY_SWINGS"
assert_eq "door tape: player melee hits blocked" \
    "$(counter_or_zero "$DOOR" "player melee hits")" "0"
assert_eq "door tape: player hits taken blocked" \
    "$(counter_or_zero "$DOOR" "player hits taken")" "0"
DOOR_SWINGS="$EXPECT_DOOR_ENEMY_SWINGS"

echo "combat-checkpoint: PASS (melee $EXPECT_MELEE_HITS, stagger $EXPECT_STAGGERS, death $EXPECT_DEATHS, taken $EXPECT_HITS_TAKEN, door swings $DOOR_SWINGS blocked, vram $VRAM_1)"
