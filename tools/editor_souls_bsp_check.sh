#!/bin/sh
# Souls vertical-slice regression gate for the tracked BSP project at
# editor/projects/souls-bsp-vertical-slice.
#
# From clean inputs: re-authors the slice through the production editor
# command test (and fails if the export disagrees with the tracked copy),
# recooks into a wiped generated/ tree, builds the real MIPS guest and the
# disc, replays the canonical souls tape twice and the negative tape once,
# then asserts the full souls loop against the pinned expectations below:
# checkpoint touch, door opening, the doorway-pinch kill, lava death,
# checkpoint respawn with the world reset, and PVS suppression of the
# sealed crypt sentinel. No images are produced; guest telemetry and the
# VRAM/display hashes are the oracle.
#
# Run as: make editor-souls-bsp-check
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${TMPDIR:-/tmp}/psoxide-souls-bsp-check"
PROJECT="editor/projects/souls-bsp-vertical-slice"
GENERATED="engine/examples/editor-playtest/generated"
CUE="build/examples/mipsel-sony-psx/release/editor-playtest.cue"

# Pinned expectations for both tapes. Any intentional content or engine
# change that shifts these must update them in the same commit, with the
# replay evidence in the commit message.
EXPECT_ROUTE_TICKS=3000
EXPECT_ATTACK_STARTS=6
EXPECT_MELEE_HITS=4
EXPECT_DUPLICATE_REJECTIONS=44
EXPECT_STAGGERS=1
EXPECT_ENEMY_DEATHS=1
EXPECT_HITS_TAKEN=4
EXPECT_WEAPON_ATTACHMENTS=2
EXPECT_CHECKPOINT_ACTIVATIONS=1
EXPECT_DOOR_ACTIVATIONS=1
EXPECT_PVS_SUPPRESSIONS=2911
EXPECT_LIQUID_EVENTS=6
EXPECT_PLAYER_DEATHS=1
# Post-respawn evidence: the player respawns at the checkpoint (x ~2048,
# far from the 1024 spawn) and walks the confirmation leg east into the
# door that reset closed, wall-stopping at x 3862 on the route line.
EXPECT_PLAYER_X_BIASED=1003862
EXPECT_PLAYER_Z_BIASED=1001536
# KNOWN RED, and the pins below are deliberately NOT updated. Read this
# before "fixing" the gate.
#
# The guest is now checkout-path-reproducible (tools/build_guest_staged.sh),
# so the old "canonical per checkout" caveat is gone. What that exposed is
# worse: this tape's GAMEPLAY COUNTERS depend on guest code layout, not just
# the VRAM residue. Measured 2026-08-11 at identical sources and an identical
# cooked generated/ tree, three different guest binaries:
#
#   checkout A, in-tree build : melee 4, enemy deaths 1, lava 6, taken 4
#   checkout B, in-tree build : melee 3, enemy deaths 0, lava 0, taken 10
#   canonical staged build    : melee 3, enemy deaths 0, lava 0, taken 10
#                               (vram 0x6bd465ea8cee4049, display 0xddb73017e2e39d82,
#                                byte-identical across both canonical replays)
#
# Route ticks (3000) and pad polls (3001) are identical in all three, so the
# simulation clock and the input schedule are NOT diverging. Only the visual
# frame rate is, which means melee hit detection is render-rate dependent: a
# slower build tests fewer hit windows and a swing misses, the enemy survives,
# and the run ends in an enemy kill instead of the authored lava death.
#
# Re-pinning to the canonical build's numbers would record a run where the
# enemy never dies and the lava never fires, i.e. it would delete the souls
# loop this gate exists to prove. So the pins stay at the authored intent and
# the gate stays red until one of these lands (owner decision):
#
#   1. re-author souls-canonical.pxitape.csv with timing margin on the swings;
#   2. make melee hit detection tick-authoritative rather than per-visual-frame;
#   3. build replay gates with the `lockstep-visuals` feature, which fixes the
#      render cadence to the simulation. Measured on the canonical staged
#      build it restores the full loop (melee 4, enemy deaths 1, player
#      deaths 1, lava 5, vram 0xd612464ccbd4fb97, display 0x945a5772d6525f52)
#      but shifts every count, so it needs its own full re-pin.
#
# `PSOXIDE_GUEST_STAGE=0 make editor-souls-bsp-check` restores the old
# per-checkout build if you need the pre-staging behaviour to compare.
EXPECT_VRAM_HASH=0xc41710bde0e93e15
EXPECT_DISPLAY_HASH=0x3c7a6bd9154f23de

EXPECT_NEG_ROUTE_TICKS=900
EXPECT_NEG_WEAPON_ATTACHMENTS=1
EXPECT_NEG_PVS_SUPPRESSIONS=812
EXPECT_NEG_PLAYER_X_BIASED=1001024
EXPECT_NEG_PLAYER_Z_BIASED=1001536

fail() {
    echo "editor-souls-bsp-check: FAIL: $1" >&2
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

gauge_latest() {
    awk -v name="$2" '$0 ~ name { for (i=1;i<=NF;i++) if ($i ~ /^latest=/) { sub("latest=","",$i); print $i; exit } }' "$1" | head -1
}

route_ticks() {
    awk -F'[= ]+' '/^route-ticks=/ { print $2; exit }' "$1"
}

assert_eq() {
    [ "$2" = "$3" ] || fail "$1: got '$2', expected '$3'"
}

cd "$ROOT"
mkdir -p "$OUT"

echo "editor-souls-bsp-check: re-authoring the slice through production commands"
rm -rf "$OUT/slice-regen"
(cd editor && PSOXIDE_SOULS_SLICE_PROJECT_OUT="$OUT/slice-regen" \
    cargo test -q -p psxed-ui \
    tests::project_workspace::souls_slice_project_is_authored_through_production_commands \
    -- --exact >/dev/null)
diff -r "$OUT/slice-regen" "$PROJECT" >/dev/null 2>&1 || fail \
    "authoring-test export differs from the tracked project; regenerate with
  rm -rf $PROJECT && (cd editor && PSOXIDE_SOULS_SLICE_PROJECT_OUT=/tmp/souls-slice-export cargo test -p psxed-ui tests::project_workspace::souls_slice_project_is_authored_through_production_commands -- --exact) && cp -R /tmp/souls-slice-export $PROJECT
and commit the result"

echo "editor-souls-bsp-check: clean cook"
rm -rf "$GENERATED"
# The generated tree also carries the TRACKED placeholder manifest; put it
# back so a gate run leaves a clean worktree.
git checkout -- "$GENERATED"
(cd editor && cargo run -p psxed-project --bin cook-playtest --quiet -- "projects/souls-bsp-vertical-slice/project.ron" >/dev/null)

echo "editor-souls-bsp-check: MIPS guest"
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry" >/dev/null

echo "editor-souls-bsp-check: disc"
# The slice cook emits UI packs (persistent gameplay assets live there); a
# disc missing them stalls on the loading screen forever, so their presence
# is asserted rather than assumed.
[ -d "$GENERATED/ui_stream_chunks" ] || fail "cook emitted no ui_stream_chunks; the disc recipe below no longer matches the cook"
[ -f "$GENERATED/ui_pack_order.txt" ] || fail "cook emitted no ui_pack_order.txt; the disc recipe below no longer matches the cook"
(cd tools/mkisopsx && cargo run --release --quiet -- \
    --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
    --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
    --volume PSOXIDE --cdtest-sectors 32 \
    --world-pack-rooms-dir "../../$GENERATED/stream_chunks" \
    --world-pack-order-file "../../$GENERATED/world_pack_order.txt" \
    --ui-pack-dir "../../$GENERATED/ui_stream_chunks" \
    --ui-pack-order-file "../../$GENERATED/ui_pack_order.txt" \
    --cdda-track-list "../../$GENERATED/cdda_tracks.txt" >/dev/null)

# Always rebuild: a replay gate must use the frontend built from the exact
# source under test. A stale binary silently drops newer telemetry labels
# (its counters print as "unknown") and can false-red or false-green the
# counter assertions. Incremental cargo makes this free when up to date.
echo "editor-souls-bsp-check: building frontend"
(cd emu && cargo build -p frontend --release --quiet)

for RUN in 1 2; do
    echo "editor-souls-bsp-check: canonical replay $RUN/2"
    ./target/release/frontend launch --path "$CUE" --embedded-playtest \
        --input-tape "$PROJECT/souls-canonical.pxitape.csv" \
        --steps 6000000000 --dump-guest-profile --dump-hash \
        > "$OUT/canon-$RUN.txt" 2>&1
done

echo "editor-souls-bsp-check: negative replay"
./target/release/frontend launch --path "$CUE" --embedded-playtest \
    --input-tape "$PROJECT/souls-negative.pxitape.csv" \
    --steps 6000000000 --dump-guest-profile --dump-hash \
    > "$OUT/negative.txt" 2>&1

MAIN="$OUT/canon-1.txt"
assert_eq "canonical route ticks" "$(route_ticks "$MAIN")" "$EXPECT_ROUTE_TICKS"
assert_eq "player attack starts" \
    "$(counter_or_zero "$MAIN" "player attack starts")" "$EXPECT_ATTACK_STARTS"
assert_eq "authored player melee hits" \
    "$(counter_or_zero "$MAIN" "player melee hits")" "$EXPECT_MELEE_HITS"
assert_eq "duplicate hit rejections" \
    "$(counter_or_zero "$MAIN" "player duplicate hit rejections")" "$EXPECT_DUPLICATE_REJECTIONS"
assert_eq "stagger enters" \
    "$(counter_or_zero "$MAIN" "game entity stagger enters")" "$EXPECT_STAGGERS"
assert_eq "enemy deaths" \
    "$(counter_or_zero "$MAIN" "game entity deaths")" "$EXPECT_ENEMY_DEATHS"
assert_eq "player hits taken" \
    "$(counter_or_zero "$MAIN" "player hits taken")" "$EXPECT_HITS_TAKEN"
assert_eq "weapon attachments (one per life, two lives)" \
    "$(counter_or_zero "$MAIN" "player weapon attachments")" "$EXPECT_WEAPON_ATTACHMENTS"
assert_eq "checkpoint activations" \
    "$(counter_or_zero "$MAIN" "player checkpoint activations")" "$EXPECT_CHECKPOINT_ACTIVATIONS"
assert_eq "door activations" \
    "$(counter_or_zero "$MAIN" "logic door activations")" "$EXPECT_DOOR_ACTIVATIONS"
assert_eq "pvs suppressions (sealed crypt sentinel)" \
    "$(counter_or_zero "$MAIN" "game entity pvs suppressions")" "$EXPECT_PVS_SUPPRESSIONS"
assert_eq "liquid damage events" \
    "$(counter_or_zero "$MAIN" "player liquid damage events")" "$EXPECT_LIQUID_EVENTS"
assert_eq "player deaths" \
    "$(counter_or_zero "$MAIN" "player deaths")" "$EXPECT_PLAYER_DEATHS"
assert_eq "post-respawn route x (biased)" \
    "$(gauge_latest "$MAIN" "player local x")" "$EXPECT_PLAYER_X_BIASED"
assert_eq "post-respawn route z (biased)" \
    "$(gauge_latest "$MAIN" "player local z")" "$EXPECT_PLAYER_Z_BIASED"

VRAM_1="$(awk -F= '/^vram_fnv1a_64=/{print $2}' "$MAIN")"
DISPLAY_1="$(awk -F= '/^display_fnv1a_64=/{print $2; exit}' "$MAIN" | awk '{print $1}')"
VRAM_2="$(awk -F= '/^vram_fnv1a_64=/{print $2}' "$OUT/canon-2.txt")"
DISPLAY_2="$(awk -F= '/^display_fnv1a_64=/{print $2; exit}' "$OUT/canon-2.txt" | awk '{print $1}')"
assert_eq "run-to-run VRAM determinism" "$VRAM_1" "$VRAM_2"
assert_eq "run-to-run display determinism" "$DISPLAY_1" "$DISPLAY_2"
assert_eq "pinned VRAM hash" "$VRAM_1" "$EXPECT_VRAM_HASH"
assert_eq "pinned display hash" "$DISPLAY_1" "$EXPECT_DISPLAY_HASH"

# Negative tape: nothing progresses, nothing fights, nothing dies, while
# the sealed sentinel keeps accruing suppressions; this is the proof the
# gate cannot false-green on a build that no-ops the souls runtime.
NEG="$OUT/negative.txt"
assert_eq "negative route ticks" "$(route_ticks "$NEG")" "$EXPECT_NEG_ROUTE_TICKS"
assert_eq "negative: checkpoint activations" \
    "$(counter_or_zero "$NEG" "player checkpoint activations")" "0"
assert_eq "negative: door activations" \
    "$(counter_or_zero "$NEG" "logic door activations")" "0"
assert_eq "negative: melee hits" \
    "$(counter_or_zero "$NEG" "player melee hits")" "0"
assert_eq "negative: attack starts" \
    "$(counter_or_zero "$NEG" "player attack starts")" "0"
assert_eq "negative: hits taken" \
    "$(counter_or_zero "$NEG" "player hits taken")" "0"
assert_eq "negative: player deaths" \
    "$(counter_or_zero "$NEG" "player deaths")" "0"
assert_eq "negative: liquid damage events" \
    "$(counter_or_zero "$NEG" "player liquid damage events")" "0"
assert_eq "negative: weapon attachments (single life)" \
    "$(counter_or_zero "$NEG" "player weapon attachments")" "$EXPECT_NEG_WEAPON_ATTACHMENTS"
assert_eq "negative: pvs suppressions" \
    "$(counter_or_zero "$NEG" "game entity pvs suppressions")" "$EXPECT_NEG_PVS_SUPPRESSIONS"
assert_eq "negative: player stayed at spawn x (biased)" \
    "$(gauge_latest "$NEG" "player local x")" "$EXPECT_NEG_PLAYER_X_BIASED"
assert_eq "negative: player stayed at spawn z (biased)" \
    "$(gauge_latest "$NEG" "player local z")" "$EXPECT_NEG_PLAYER_Z_BIASED"

echo "editor-souls-bsp-check: PASS (hits $EXPECT_MELEE_HITS, stagger $EXPECT_STAGGERS, kill $EXPECT_ENEMY_DEATHS, taken $EXPECT_HITS_TAKEN, checkpoint $EXPECT_CHECKPOINT_ACTIVATIONS, door $EXPECT_DOOR_ACTIVATIONS, lava $EXPECT_LIQUID_EVENTS, death $EXPECT_PLAYER_DEATHS, attach $EXPECT_WEAPON_ATTACHMENTS, pvs $EXPECT_PVS_SUPPRESSIONS, vram $VRAM_1)"
