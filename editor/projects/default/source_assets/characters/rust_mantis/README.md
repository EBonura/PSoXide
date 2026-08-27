# Rust Mantis texture source

`Diffuse_Enemy01_256px.png` is the Rust Mantis-owned copy of the revised
enemy texture delivered with the Tank Boss. The artwork is intentionally
duplicated at source level so the two characters can evolve independently.
The Rust Mantis model source remains at `../rust_mantis.glb`.

The cooked Rust Mantis pair is target-specific:

- `assets/models/rust_mantis/rust_mantis.psxt` is a 128 x 128, four-bank
  4bpp atlas.
- `assets/models/rust_mantis/rust_mantis.psxmdl` stores the matching
  per-face palette-bank assignments.

Do not replace the light atlas with the Tank Boss 8bpp PSXT directly. Rebuild
the 8bpp reference from this PNG, then run `tools/model_palette_banks.py` with
the Rust Mantis model so the model and four-bank atlas stay paired. Rebanking
does not change its geometry, skeleton, UVs, or animation compatibility.

## Selected animations

`Idle` uses `idle_look_v2/idle_look_01`, an authored sentry scan layered over
the planted `idle_v1/idle_01` body motion. The Rust Mantis slowly checks left
and right with long readable holds across the spine, neck, and head. It remains
locked against horizontal root drift, closes into a seamless eight-second
loop, and is cooked at 12 Hz to
`assets/animations/rust_mantis_starter/idle.psxanim` (97 frames). The first and
last cooked poses are identical.

`WalkBackward` uses locally selected MoMask candidate 2. It is cooked at 12 Hz
to `assets/animations/rust_mantis_starter/walk_bwd.psxanim` and bound in the
Rust Mantis Starter Animation Set. Generated candidates and review media are
kept outside version control under `local-assets/`.
