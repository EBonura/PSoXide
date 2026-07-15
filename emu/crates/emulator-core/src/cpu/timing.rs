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

/// One issue cycle applied to each instruction retirement.
///
/// The R3000A issues ordinary cached instructions at one cycle each. Memory
/// and execution-unit stalls are charged separately by the CPU/bus model.
const BIAS: u32 = 1;

pub(super) fn cycle_cost(_instr: u32) -> u32 {
    BIAS
}
