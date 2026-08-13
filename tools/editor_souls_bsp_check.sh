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
# The last stage builds the SAME sources a second time with a different code
# layout and replays the canonical tape on it. Gameplay must be identical
# while the presented frame count is free to differ: that is the standing
# proof that the souls loop depends on the fixed simulation clock and not on
# how many visual frames a build managed to present.
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
#
# `route-ticks` is the EMULATOR's host clock (a fixed cycle budget per tick),
# so it moves with guest speed and is pinned for the canonical build only.
# The build-independent clocks are the guest's own: pad polls and sim ticks,
# which the cross-layout stage below compares directly.
EXPECT_ROUTE_TICKS=2999
EXPECT_PAD_POLLS=3000
EXPECT_SIM_TICKS=2999
EXPECT_ATTACK_STARTS=6
EXPECT_MELEE_HITS=4
EXPECT_DUPLICATE_REJECTIONS=44
EXPECT_STAGGERS=1
EXPECT_ENEMY_DEATHS=1
EXPECT_HITS_TAKEN=4
EXPECT_WEAPON_ATTACHMENTS=2
EXPECT_CHECKPOINT_ACTIVATIONS=1
EXPECT_DOOR_ACTIVATIONS=1
EXPECT_PVS_SUPPRESSIONS=2910
EXPECT_LIQUID_EVENTS=6
EXPECT_PLAYER_DEATHS=1
# Post-respawn evidence: the player respawns at the checkpoint (x ~2048,
# far from the 1024 spawn) and walks the confirmation leg east into the
# door that reset closed, wall-stopping at x 3862 on the route line.
EXPECT_PLAYER_X_BIASED=1003862
EXPECT_PLAYER_Z_BIASED=1001536
# The two image hashes pin the CANONICAL build's final presented frame. They
# are deliberately NOT part of the cross-layout comparison: the run stops on
# tape exhaustion, so the last frame a build presented belongs to whichever
# simulation tick its own cadence reached, and the player's idle clip is still
# advancing there. Gameplay is what must match across layouts, and it does.
#
# History (read before "fixing" a red gate by re-pinning): until 2026-08-11
# both souls tapes were indexed on the emulator's VIDEO-FRAME clock while the
# guest samples the pad once per fixed simulation tick. Those clocks are not
# phase-locked and their drift tracks guest execution cost, so the authored
# 70-frame doorway retreat reached one guest as 71 held ticks and another as
# 70. One tick of extra backward movement is the whole difference between the
# fourth heavy swing reaching the Mantis and missing it, so the same sources
# scored melee 4 / kill / lava death on one build and melee 3 / no kill /
# death-by-enemy on another. Both tapes are now on the PAD-POLL clock (sample
# N lands on poll N), which is the guest's own input clock and therefore
# frame-rate independent; the retreat carries the 71 ticks the route needs.
# Re-pinned 2026-08-12 at the integration head. The editor's trigger-anchor
# fix moved the slice's Lift Door origin by one unit (y 257 to 256), which
# is a real content change and therefore a different final frame. Every
# simulation-side counter is unchanged (melee 4, enemy death 1, lava 6,
# player death 1, checkpoint 1, door 1, attachments 2) and both canonical
# replays agree, so this is a content re-pin, not a determinism failure.
EXPECT_VRAM_HASH=0xbab02327df64003d
EXPECT_DISPLAY_HASH=0x3c7a6bd9154f23de

EXPECT_NEG_ROUTE_TICKS=898
EXPECT_NEG_PAD_POLLS=900
EXPECT_NEG_WEAPON_ATTACHMENTS=1
EXPECT_NEG_PVS_SUPPRESSIONS=810
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

pad_polls() {
    awk '/^route-ticks=/ { for (i=1;i<=NF;i++) if ($i ~ /^port1-polls=/) { sub("port1-polls=","",$i); print $i; exit } }' "$1"
}

assert_eq() {
    [ "$2" = "$3" ] || fail "$1: got '$2', expected '$3'"
}

# A replay gate's tape must be indexed on the guest's own input clock. The
# editor records live play on the video-frame clock, so a tape re-recorded
# rather than re-authored would silently put this gate back on the drifting
# clock described above; catch that here instead of three builds later.
assert_poll_clock() {
    head -1 "$1" | grep -q 'clock=pad_poll' || fail \
        "$1 is not a pad_poll tape. Replay gates need the guest's own input clock;
  convert an editor recording with 'frontend launch --input-tape <in> --input-tape-transcribe <out>'."
}

# Every counter/gauge whose value is a property of the SIMULATION, not of a
# build's render cadence. The cross-layout stage compares this whole list
# between two differently compiled guests.
GAMEPLAY_COUNTERS="player attack starts
player melee hits
player duplicate hit rejections
game entity stagger enters
game entity deaths
player hits taken
player weapon attachments
player checkpoint activations
logic door activations
game entity pvs suppressions
player liquid damage events
player deaths
sim ticks"

gameplay_fingerprint() {
    echo "$GAMEPLAY_COUNTERS" | while IFS= read -r name; do
        [ -n "$name" ] || continue
        echo "$name=$(counter_or_zero "$1" "$name")"
    done
    echo "player local x=$(gauge_latest "$1" "player local x")"
    echo "player local z=$(gauge_latest "$1" "player local z")"
    echo "port1 polls=$(pad_polls "$1")"
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

assert_poll_clock "$PROJECT/souls-canonical.pxitape.csv"
assert_poll_clock "$PROJECT/souls-negative.pxitape.csv"

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
assert_eq "canonical pad polls (guest input clock)" \
    "$(pad_polls "$MAIN")" "$EXPECT_PAD_POLLS"
assert_eq "canonical sim ticks (guest simulation clock)" \
    "$(counter_or_zero "$MAIN" "sim ticks")" "$EXPECT_SIM_TICKS"
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
assert_eq "negative pad polls (guest input clock)" \
    "$(pad_polls "$NEG")" "$EXPECT_NEG_PAD_POLLS"
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

# Frame-cadence lock. Combat timing must depend on the fixed simulation
# clock, not on how many visual frames the renderer manages to deliver. That
# is proved directly, by replaying the same authored swing under different
# render costs and requiring the active window to cover the same simulation
# ticks either way:
#   psx-game-runtime combat::tests::tick_authority
# Nothing else in this gate establishes it, so it is run here explicitly
# rather than being left to whoever remembers to run the crate's unit tests.
echo "editor-souls-bsp-check: frame-cadence lock"
cadence_log="$OUT/tick-authority.txt"
(cd "$ROOT/engine" && cargo test -p psx-game-runtime --lib combat::tests::tick_authority) \
    > "$cadence_log" 2>&1 \
    || { cat "$cadence_log"; fail "the frame-cadence lock (tick_authority) regressed"; }
# A filter that matches nothing still exits 0, so the count is asserted too:
# renaming or deleting these tests must break this gate, not silently empty it.
cadence_ran=$(sed -n 's/^test result: ok\. \([0-9]*\) passed.*/\1/p' "$cadence_log" | head -1)
[ "${cadence_ran:-0}" -eq 2 ] || fail "expected 2 tick_authority tests, ran ${cadence_ran:-0}.
  psx-game-runtime combat::tests::tick_authority is the only direct evidence
  that combat timing follows the simulation clock rather than the frame rate."

# Cross-layout reproducibility stage. The same sources and the same cooked
# world built twice: once through the canonical /tmp stage, once in-tree
# through the PSOXIDE_GUEST_STAGE=0 escape hatch, so the build runs from two
# different absolute paths.
#
# This stage used to REQUIRE the two images to differ, on the theory that
# cargo's path-derived crate metadata reorders codegen. Measured on
# 2026-08-12 that is not true here: the guest links with
# `-Clink-arg=--oformat=binary`, so the image carries no symbol table and no
# path metadata, and a forced full recompile in-tree (cargo reported
# "Compiling editor-playtest", 6.17s, not a cache hit) produced a
# byte-identical image to the staged build. Requiring a difference therefore
# only passed when the two builds were inconsistent with each other.
#
# What this stage proves is reproducibility across build layouts, and ONLY
# that. Two identical executables cannot perturb visual cadence, so the
# replay below is a second confirmation that the same image replays the same
# way, not independent evidence of cadence independence. That evidence is the
# tick_authority run above.
echo "editor-souls-bsp-check: second guest layout"
cp "$ROOT/build/examples/mipsel-sony-psx/release/editor-playtest.exe" "$OUT/layout-a.exe"
PSOXIDE_GUEST_STAGE=0 make build-editor-playtest \
    EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry" >/dev/null
cp "$ROOT/build/examples/mipsel-sony-psx/release/editor-playtest.exe" "$OUT/layout-b.exe"
if ! cmp -s "$OUT/layout-a.exe" "$OUT/layout-b.exe"; then
    fail "the staged and in-tree guest builds differ.
  The guest image is meant to be reproducible across build layouts; a
  difference means something outside the tracked source closure reached the
  build."
fi
(cd tools/mkisopsx && cargo run --release --quiet -- \
    --exe "$OUT/layout-b.exe" \
    --out "$OUT/layout-b.bin" \
    --volume PSOXIDE --cdtest-sectors 32 \
    --world-pack-rooms-dir "../../$GENERATED/stream_chunks" \
    --world-pack-order-file "../../$GENERATED/world_pack_order.txt" \
    --ui-pack-dir "../../$GENERATED/ui_stream_chunks" \
    --ui-pack-order-file "../../$GENERATED/ui_pack_order.txt" \
    --cdda-track-list "../../$GENERATED/cdda_tracks.txt" >/dev/null)

echo "editor-souls-bsp-check: canonical replay on the second layout"
./target/release/frontend launch --path "$OUT/layout-b.cue" --embedded-playtest \
    --input-tape "$PROJECT/souls-canonical.pxitape.csv" \
    --steps 6000000000 --dump-guest-profile --dump-hash \
    > "$OUT/canon-layout-b.txt" 2>&1

# Leave the build tree holding the canonical artifact again, so the .exe next
# to the disc this gate packed is the one it was packed from.
cp "$OUT/layout-a.exe" "$ROOT/build/examples/mipsel-sony-psx/release/editor-playtest.exe"

gameplay_fingerprint "$MAIN" > "$OUT/fingerprint-a.txt"
gameplay_fingerprint "$OUT/canon-layout-b.txt" > "$OUT/fingerprint-b.txt"
FRAMES_A="$(counter_or_zero "$MAIN" "visual frames")"
FRAMES_B="$(counter_or_zero "$OUT/canon-layout-b.txt" "visual frames")"
if ! diff -u "$OUT/fingerprint-a.txt" "$OUT/fingerprint-b.txt" >"$OUT/fingerprint.diff" 2>&1; then
    echo "--- souls loop differs between guest layouts ---" >&2
    cat "$OUT/fingerprint.diff" >&2
    fail "gameplay is not build-independent (layout A presented $FRAMES_A visual frames, layout B $FRAMES_B).
  Something in the gameplay path is reading the render cadence; see the clock
  history at the top of this file before touching the pins."
fi

echo "editor-souls-bsp-check: PASS (hits $EXPECT_MELEE_HITS, stagger $EXPECT_STAGGERS, kill $EXPECT_ENEMY_DEATHS, taken $EXPECT_HITS_TAKEN, checkpoint $EXPECT_CHECKPOINT_ACTIVATIONS, door $EXPECT_DOOR_ACTIVATIONS, lava $EXPECT_LIQUID_EVENTS, death $EXPECT_PLAYER_DEATHS, attach $EXPECT_WEAPON_ATTACHMENTS, pvs $EXPECT_PVS_SUPPRESSIONS, vram $VRAM_1; identical across two guest layouts presenting $FRAMES_A and $FRAMES_B visual frames)"
