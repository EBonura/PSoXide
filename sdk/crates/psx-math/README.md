# psx-math

Fixed-point math for the PS1 SDK: Q-format helpers, sine and cosine tables,
32-bit scalar kernels, integer-to-text, and the one wide-arithmetic type.
Everything is `no_std`, with no floats and no 64-bit division.

## Wide arithmetic: `Wide`, `WideU`, `Mask64`

The R3000 is a 32-bit machine with no 64-bit ALU, but a few 64-bit things are
cheap and never call a compiler helper: `mult` and `multu` write the full
32x32 product to HI:LO in one instruction, a 64-bit add or subtract is two
adds and a carry, a compare is two word compares, and a shift is a few word
shifts. The expensive half (divide, remainder, 128-bit, soft float) is what
`tools/guest_symbol_gate.sh` fails a link for.

`psx_math::Wide` (signed) and `psx_math::WideU` (unsigned) are the **only**
way guest code holds a 64-bit quantity: a dot product of two world-sized
vectors, a squared distance, the cross product of two screen edges, a CORDIC
accumulator. A game that uses them has no `i64` or `u64` token in its own
source, so a per-project source guard (`no64_guard`, or the editor's
`runtime-numeric-guard`) can fail the build on every occurrence, and this
module is the single place the rule is enforced: its 64-bit tokens carry
`psx-numeric-allow` markers and are all in the private `widen*` / `signed` /
`unsigned` helpers, the two newtype fields and `MAX` / `MIN`.

```rust
use psx_math::Wide;

// Squared distance of two world points, then a root: 3 mult, 2 adds, isqrt_u64.
let d2 = Wide::mul(dx, dx).mul_add(dy, dy).mul_add(dz, dz);
let dist = d2.isqrt();

// The sign of a 2D cross product that can exceed i32.
let facing = Wide::mul(bx - ax, cy - ay).mul_sub(cx - ax, by - ay);
if facing.is_negative() { /* back-facing */ }

// A Q12 dot product of a vector with a unit vector, back in 32 bits.
let along = Wide::dot3(v, unit).shr_i32(12);
```

### What is in the API

| Group | `Wide` | `WideU` |
| --- | --- | --- |
| Build | `of`, `of_u`, `from_parts(hi, lo)`, `ZERO`, `ONE`, `MAX`, `MIN`, `Default` | `of`, `from_parts(hi, lo)`, `ZERO`, `MAX`, `Default` |
| Products | `mul` (one `mult`), `mul_add`, `mul_sub`, `dot3`, `times(i32)`, `mul_w(Wide)` (low 64 bits) | `mul` (one `multu`), `mul_add`, `times(u32)` |
| Add, subtract | `+`, `-`, unary `-`, `add_w`, `sub_w`, `plus(i32)`, `negate`, `abs` (all wrapping) | `+`, `-`, `add`, `sub` (wrapping), `saturating_add` |
| Shift (count modulo 64) | `<<` / `shl_by`, `>>` / `sar` (arithmetic), `lshr` (logical) | `<<` / `shl_by`, `>>` / `shr_by` |
| Compare | `Ord`, `PartialOrd`, `is_zero`, `is_negative`, `is_positive`, `cmp_i32` | `Ord`, `PartialOrd`, `is_zero` |
| To 32 bits, truncating | `low`, `low_u32`, `high`, `shr_i32(n)` (shift, then truncate) | `low`, `high`, `shr_u32(n)` |
| To 32 bits, checked | `to_i32`, `to_u32` (`Option`) | `to_u32` |
| To 32 bits, saturating | `saturate_i32`, `saturate_u32` | `saturate_u32` |
| Root | `isqrt` (0 for a negative value) | `isqrt` |

`Mask64` is a set of up to 64 flags in two `u32` words (`set`, `clear`,
`assign`, `test`), for what used to be a `u64` bit mask. It does no arithmetic.

### What is deliberately missing

There is no division and no remainder on a `Wide`, no 128-bit anything and no
conversion to a float, and there will not be: each would link a software
routine the gate rejects. To divide a wide product by a 32-bit value use
`psx_math::int32::mul_div_i32` / `mul_div_u32` (a native `multu`, then `divu`
or the helper-free `div_u64_by_u32`). To scale a product by a power of two use
`mul_shr_i32` or `Wide::shr_i32`. If a quantity needs more than that,
restructure it into 32-bit steps.

Arithmetic wraps like the release-mode `i64` / `u64` it replaces; shift counts
are taken modulo 64 (a count of 64 shifts by zero). `Debug` prints the two
words in hex, because decimal formatting of 64 bits would link `__udivdi3`.

### Proof

- Host tests (`cargo test -p psx-math wide`): edge values and a 200,000-case
  pseudo-random stream for every operation against `i128` / `u128` oracles,
  every shift count 0..63 and past it, and each extraction flavour.
- `sdk/examples/hello-wide` runs every operation on the target (or the
  emulator) against a reference written in 32-bit arithmetic only, and prints
  `WIDE PASS`. `make hello-wide-gate [FRONTEND=...]` builds it, runs
  `tools/guest_symbol_gate.sh` on its link map, and with a frontend runs the
  guest and checks the verdict.
- The internal value is a plain `i64` / `u64` and every method is
  `#[inline(always)]` or `const`, so a call compiles to the same instructions
  as the bare expression. Games that migrated from per-game copies measured
  identical code for the converted functions.

## The rest of the crate

- `sincos`: Q0.12 angle type, 256-entry Q1.12 sine table, interpolated
  `sin_q12` / `cos_q12`, and the octant-linear `atan2_q12`.
- `int32`: saturating 32-bit helpers and the helper-free wide forms
  (`mul_div_i32`, `mul_shr_i32`, `mul_div_u32`, `isqrt_u64`, `div_u64_by_u32`).
- `fmt`: integer-to-decimal ASCII without `core::fmt`.
- `color`, `clip_lanes`, `attributed_clip`: color helpers and clipping kernels.
- Feature `asm-kernels`: hand-scheduled R3000 versions of `clip_lanes8_plane`,
  bit-identical to the portable definition.
