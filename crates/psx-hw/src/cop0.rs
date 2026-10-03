//! System control coprocessor (COP0) register fields.

/// `Cause.BD`: the exception was taken in the delay slot of the branch at EPC.
pub const CAUSE_BD: u32 = 1 << 31;
