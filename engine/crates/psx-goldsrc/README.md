# GoldSrc runtime contracts

`Hsfx<N, HEALTH, SUIT>` owns resident-bank metadata, map dialogue, 15 rotating one-shot voices, charger voice16 and seven owner-addressed map voices. Each game owns its sole state instance and authored IDs. There is no dynamic dispatch, allocation or second audio driver.

All calls must be serialized with map replacement and the caller's SPU users. Playback preserves the previous SDK register sequence, gain arithmetic, stop-before-upload ordering and valid-prefix behavior. A truncated HSFX directory still panics as in the frozen source; this extraction does not claim to harden malformed inputs.

Validation from the editor root:

```sh
cargo test --manifest-path engine/Cargo.toml -p psx-goldsrc
python3 engine/crates/psx-goldsrc/tests/hsfx_oracle.py
```

The oracle compiles frozen HL70072 and CS988f implementations and the actual shared source, using the actual pinned SDK Voice/OneShot register-lowering code. Only MMIO/transfer endpoints are replaced with recorders. Synthetic banks exercise both resident capacities, truncation, invalid payloads, SPU bounds, voice rotation, dialogue duration, map-loop ownership and stop-before-replacement. Frozen code is test-only and has no production callers.
