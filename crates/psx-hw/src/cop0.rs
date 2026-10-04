//! System control coprocessor (COP0) register fields.

/// `Cause.BD`: the exception was taken in the delay slot of the branch at EPC.
pub const CAUSE_BD: u32 = 1 << 31;

/// The `rfe` instruction word: restore the pre-exception interrupt and
/// mode bits. An exception handler puts it in the delay slot of its final
/// `jr`.
pub const RFE: u32 = 0x4200_0010;
