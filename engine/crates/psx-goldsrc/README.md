# GoldSrc runtime contracts

`Hsfx<N, HEALTH, SUIT>` owns resident-bank metadata, map dialogue, 15 rotating one-shot voices, charger voice16 and seven owner-addressed map voices. Each game owns its sole state instance and authored IDs. There is no dynamic dispatch, allocation or second audio driver.

All calls must be serialized with map replacement and the caller's SPU users. Playback preserves the previous SDK register sequence, gain arithmetic, stop-before-upload ordering and valid-prefix behavior. A truncated HSFX directory still panics as in the frozen source; this extraction does not claim to harden malformed inputs.

Validation from the editor root:

```sh
cargo test --manifest-path engine/Cargo.toml -p psx-goldsrc
python3 engine/crates/psx-goldsrc/tests/hsfx_oracle.py
```

The oracle compiles frozen HL70072 and CS988f implementations and the actual shared source, using the actual pinned SDK Voice/OneShot register-lowering code. Only MMIO/transfer endpoints are replaced with recorders. Synthetic banks exercise both resident capacities, truncation, invalid payloads, SPU bounds, voice rotation, dialogue duration, map-loop ownership and stop-before-replacement. Frozen code is test-only and has no production callers.

`texture_animation` owns the 20 Hz-to-texture-chain clock, chain membership and
PVS-local change generations used by HL and CS. It borrows the ports' existing
state; display-row writes stay with their map decoders. The simulation driver is
outlined once per caller type, with no heap, vtable or additional resident array.
`model_variant::lookup` decodes their generated sorted three-byte variant rows;
map IDs, model IDs and generated tables stay in each game.

`tests/asset_contract.rs` compares the included frozen pre-extraction texture
driver across every u16 simulation tick, primary/alternate/empty/overlapping
chains and clock wrap. Its lookup oracle preserves the old function with only
generated table references changed to parameters. Original source and line/hash
provenance are retained under `tests/oracles/ASSET-PROVENANCE.json`.

## Shared gameplay modules

`pushable`, `semantic_input`, `route_follow`, `telemetry`, `visibility_logic`,
`hitbox_logic`, `pickup_logic`, `ladder_logic`, `ordering` and `ground_logic`
moved here verbatim from the two ports, which had carried identical or
near-identical copies. They hold no state and no statics; each port keeps its
policy. `ordering` takes the port's ordering-table length as the `OT_LEN`
const parameter (320 for HL, 512 for CS), and `ground_logic` leaves
`prop_spawn_floor_mode` / `prop_uses_occlusion_probe` to each game.
`telemetry` writes the emulator event ports only when the port forwards its
`emulator-telemetry` or `performance-telemetry` feature to this crate.
Small functions that were previously intra-crate carry `#[inline]` so the
games' LTO builds keep their previous inlining.
