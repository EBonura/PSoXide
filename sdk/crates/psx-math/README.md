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
| Reinterpret | `unsigned_abs` (exact, `MIN` gives 2^63), `cast_unsigned` (like `as u64`) | `cast_signed` (like `as i64`) |
| Root | `isqrt` (0 for a negative value) | `isqrt` |

`Mask64` is a set of up to 64 flags in two `u32` words, for what used to be a
`u64` bit mask: `set`, `clear`, `assign`, `test`, the set algebra `or`, `and`,
`and_not`, `is_empty`, the constructors `bit(n)`, `first(n)` (lowest `n` bits)
and `from_words(low, high)`, and `low` / `high` to store it. It does no
arithmetic.

### Division (`psx_math::wide_div`)

Dividing a wide value is its own module so the cost is visible at the call
site. Every operation is exact, uses only `multu`, 64-bit add / subtract /
compare / shift and 32-bit `divu`, and links no compiler helper (the guest
symbol gate passes with no opt-out). A zero divisor panics, like `/`. The
bodies are `#[inline(never)]`: one shared copy per image.

| Operation | Divisor | Result |
| --- | --- | --- |
| `WideU::div_rem_u32` | `u32` | full 64-bit quotient and the remainder |
| `WideU::quotient` | `WideU` | `floor(n / d)`, clamped to `u32` |
| `Wide::div_i32_wrap(d, Rounding)` | `i32` | the quotient as its low 32 bits, like `(n / d) as i32` |
| `Wide::div_i32_sat(d, Rounding)` | `i32` | the quotient clamped to the `i32` range |
| `Wide::div_wide_sat(d, Rounding)` | `Wide` | the same for a wide divisor |
| `WideU::mul_full` | `WideU` | the exact 128-bit product, `(high, low)` |
| `WideU::cmp_ratio(a, b, c, d)` | | order of `a / b` against `c / d` by comparing `a * d` with `c * b` |
| `WideU::scale_by_ratio(a, den)` | `WideU` | `floor(a * self / den)` for `self <= den` |
| `Wide::div_pow2(shift)` | `2^shift` | quotient truncated toward zero, still wide |

`Rounding` is `TowardZero` (Rust's `/`), `Floor` (toward negative infinity) or
`Nearest` (an exact half goes away from zero).

Cost on the R3000A (CPU cycles per call including the call and return, random
operands of every magnitude, measured by `sdk/examples/hello-wide-div` on the
emulator; the numerator "fits a word" rows take the single-`divu` path):

| Operation | Cycles |
| --- | ---: |
| `div_pow2` | 14 |
| `div_rem_u32`, numerator fits a word | 89 |
| `div_i32_wrap` / `div_i32_sat`, numerator fits a word | 153 to 167 |
| `mul_full` | 138 |
| `div_rem_u32`, 64-bit numerator | 300 |
| `quotient`, word or wide divisor | 310 to 322 |
| `div_i32_wrap` / `div_i32_sat`, 64-bit numerator | 421 to 430 |
| `cmp_ratio` | 441 |
| `div_wide_sat`, wide divisor | 476 |
| `scale_by_ratio`, wide denominator | 528 |

A 64-bit numerator over a word divisor is one `divu` for the high word plus the
two-digit `div_u64_by_u32` (about 180 cycles). A divisor wider than 32 bits adds
one `div_u64_by_u32` for the estimate (taken from the top 32 bits of the divisor)
and a 96-bit product to settle it; the estimate is never below the true quotient
and is one too high about one time in eight, so the settle loop runs at most
twice.

### What is deliberately missing

There is no 128-bit type and no conversion to a float, and there will not
be: each would link a software routine the gate rejects. To divide a product
of two words by a word use `psx_math::int32::mul_div_i32` / `mul_div_u32` (a
native `multu`, then `divu` or the helper-free `div_u64_by_u32`), which is
cheaper than going through a `Wide`. To scale a product by a power of two use
`mul_shr_i32` or `Wide::shr_i32`.

Arithmetic wraps like the release-mode `i64` / `u64` it replaces; shift counts
are taken modulo 64 (a count of 64 shifts by zero). `Debug` prints the two
words in hex, because decimal formatting of 64 bits would link `__udivdi3`.

### Proof

- Host tests (`cargo test -p psx-math wide`): edge values and a 200,000-case
  pseudo-random stream for every operation against `i128` / `u128` oracles,
  every shift count 0..63 and past it, and each extraction flavour.
- `sdk/examples/hello-wide` runs every operation on the target (or the
  emulator) against a reference written in 32-bit arithmetic only, and prints
  `WIDE PASS`. `sdk/examples/hello-wide-div` does the same for the division
  operations (a bit-by-bit restoring division over 32-bit words is the
  reference), prints `WIDEDIV PASS`, and times each operation.
  `make hello-wide-gate [FRONTEND=...]` builds both, runs
  `tools/guest_symbol_gate.sh` on their link maps, and with a frontend runs the
  guests and checks the verdicts.
- Host tests (`cargo test -p psx-math wide_div`): 1.5 million random cases per
  division operation against `i128` / `u128` oracles, every rounding mode, and
  the edge values around every word boundary, plus a bound on the settle loop.
- The internal value is a plain `i64` / `u64` and every method is
  `#[inline(always)]` or `const`, so a call compiles to the same instructions
  as the bare expression. Games that migrated from per-game copies measured
  identical code for the converted functions.

## The rest of the crate

- `sincos`: Q0.12 angle type, 256-entry Q1.12 sine table, interpolated
  `sin_q12` / `cos_q12`, and the octant-linear `atan2_q12`.
- `int32`: saturating 32-bit helpers and the helper-free wide forms
  (`mul_div_i32`, `mul_shr_i32`, `mul_div_u32`, `isqrt_u64`, `div_u64_by_u32`).
- `wide_div`: exact `Wide` / `WideU` division, 128-bit product and ratio compare.
- `fmt`: integer-to-decimal ASCII without `core::fmt`.
- `color`, `clip_lanes`, `attributed_clip`: color helpers and clipping kernels.
- Feature `asm-kernels`: hand-scheduled R3000 versions of `clip_lanes8_plane`,
  bit-identical to the portable definition.
