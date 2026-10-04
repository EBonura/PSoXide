# Known issues

Defects found while working on something else, written down with what was
measured, so they are not lost. Each says whether it is fixed.

## psx-asset: `hma1::seg` multiplies position by factor in 32 bits

Open. Found 2026-10-04 while pinning the HMA1 key-width check.

`seg` in `sdk/crates/psx-asset/src/hma1.rs` maps a clip position to a
segment with `(pos_q8 * factor) >> 15`, where `pos_q8` and `factor` are both
`u32`. `factor` is a Q15 rate factor read from the clip header; the cooker
writes about `0x7FFF` (1.0) for a rate with one key per source frame.

- With `factor = 0x7FFF` the product stops fitting 32 bits above
  `pos_q8 = 131,076` (`4,294,967,295 / 32,767`), which is 512.0 source frames
  (`pos_q8` counts 256 per frame). A smaller factor moves the threshold up in
  proportion.
- Measured: `Model::decode` at `pos_q8 = u32::MAX` on a validated blob panics
  in a debug build with "attempt to multiply with overflow" at `hma1.rs:122`.
  The test `every_key_byte_count_is_either_rejected_or_decodes_without_overflow`
  therefore stops at 131,075.
- In a release (guest) build the multiply wraps. By reading the code (not
  measured on a guest), the result then still lands in `0..s` or is clamped by
  `if i >= s`, so every key read stays inside the range `tracks_fit` validated.
  The harm is not memory safety but a wrong frame: a clip longer than 512 source
  frames, played past that point, samples an unrelated segment instead of its
  tail.
- Not measured: whether any shipped clip is that long, and what a 64-bit
  multiply costs. `seg` is called per bone per rate per frame, so the fix
  (widen to `u64`, or clamp `pos_q8` to `segments << 8` first, which costs one
  compare and no multiply) wants a bench on a game that decodes many bones
  before it lands.
