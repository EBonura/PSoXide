//! Per-instruction cycle-cost model.
//!
//! ## Provenance
//!
//! The cycle bias here is parity-matched against, and derived from,
//! PCSX-Redux's simple interpreter
//! (<https://github.com/grumpycoders/pcsx-redux>), Copyright (C) the
//! PCSX-Redux authors, GPL-2.0-or-later. Matching Redux's `BIAS` keeps
//! scheduler events landing on the same instruction. PSoXide is released
//! under GPL-2.0-or-later in part to honor this lineage; see `LICENSE`
//! and `docs/license-audit.md`.

/// One extra cycle bias applied to each instruction retirement.
///
/// Keeping this equal to Redux's `BIAS` is what makes the VBlank
/// scheduler line up on the same instruction as Redux and preserves
/// parity once it turns on.
const BIAS: u32 = 2;

pub(super) fn cycle_cost(_instr: u32) -> u32 {
    BIAS
}
