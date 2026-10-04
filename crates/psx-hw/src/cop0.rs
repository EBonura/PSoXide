//! System control coprocessor (COP0) register fields.

/// `Cause.BD`: the exception was taken in the delay slot of the branch at EPC.
pub const CAUSE_BD: u32 = 1 << 31;

/// The `rfe` instruction word: restore the pre-exception interrupt and
/// mode bits. An exception handler puts it in the delay slot of its final
/// `jr`.
pub const RFE: u32 = 0x4200_0010;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfe_is_coprocessor_0_function_0x10() {
        // COP0 major opcode 0x10 in bits 31..26, the CO bit (25) set, function 0x10.
        assert_eq!(RFE, 0x10 << 26 | 1 << 25 | 0x10);
    }
}
