//! GTE register accessors.
//!
//! These are **macros**, not functions, because the GTE register
//! index is encoded as a 5-bit immediate field in the instruction --
//! you can't pass it as a runtime value without either stamping 32
//! separate functions or generating code at build time. Macros keep
//! the call site readable (`write_data!(0, value)`) while letting the
//! assembler emit the exact encoding.
//!
//! Every macro emits a hand-built `.word` rather than the mnemonic
//! (`mtc2`, `mfc2`, `ctc2`, `cfc2`). The `mipsel-sony-psx` LLVM
//! target gates the coprocessor-2 instruction class behind a CPU
//! feature that isn't enabled, so passing the mnemonic through the
//! assembler fails with "instruction requires a CPU feature not
//! currently enabled". Emitting the raw 32-bit encoding sidesteps
//! that gate; the hardware still decodes it the same way.
//!
//! Encoding (per PSX-SPX, "MIPS Coprocessor 2 Opcodes"):
//!
//! ```text
//!   bits 31..26 = 0x12 (COP2)
//!   bits 25..21 = rs           -- 0=MFC2, 2=CFC2, 4=MTC2, 6=CTC2
//!   bits 20..16 = rt           -- CPU general-purpose register
//!   bits 15..11 = rd           -- COP2 data/control register index
//!   bits 10..0  = 0
//! ```
//!
//! We pin the CPU-side register to `$t0` (`$8`) via `in("$8")` /
//! `out("$8")` operands, so the encoding becomes
//! `opcode_base | 8<<16 | rd<<11`. The compiler is responsible for
//! shuttling values into/out of `$t0`, which keeps call sites
//! unobstructed.
//!
//! Register index tables (mirroring `emulator-core::gte`):
//!
//! | Data regs       | Purpose                                   |
//! |-----------------|-------------------------------------------|
//! | 0..=5           | V0, V1, V2 input vectors (packed XY / Z)  |
//! | 6               | RGBC                                      |
//! | 7               | OTZ (output average Z)                    |
//! | 8               | IR0 (scalar accumulator)                  |
//! | 9..=11          | IR1, IR2, IR3 (vector accumulators)       |
//! | 12..=15         | SXY FIFO (12..=14 slots, 15 = SXYP push)  |
//! | 16..=19         | SZ FIFO (16=SZ0 oldest … 19=SZ3 newest)   |
//! | 20..=22         | RGB FIFO                                  |
//! | 24              | MAC0                                      |
//! | 25..=27         | MAC1, MAC2, MAC3                          |
//! | 28 / 29         | IRGB (write) / ORGB (read) 5:5:5 view     |
//! | 30 / 31         | LZCS / LZCR                               |
//!
//! | Control regs    | Purpose                                   |
//! |-----------------|-------------------------------------------|
//! | 0..=4           | RT rotation matrix                        |
//! | 5..=7           | TR translation                            |
//! | 8..=12          | LLM light-direction matrix                |
//! | 13..=15         | BK background colour (light bias)         |
//! | 16..=20         | LCM light-colour matrix                   |
//! | 21..=23         | FC far colour                             |
//! | 24 / 25         | OFX / OFY screen offsets                  |
//! | 26              | H projection plane                        |
//! | 27 / 28         | DQA / DQB depth-cue coefficients          |
//! | 29 / 30         | ZSF3 / ZSF4 AVSZ weights                  |
//! | 31              | FLAG (error/saturation bits)              |

/// Read a GTE data register (`MFC2`). Expands to a single MFC2 instruction
/// with the register index baked into the `.word` encoding, followed
/// by a NOP that fills the one-cycle load-delay slot.
///
/// **Why the NOP**: MIPS R3000 MFC2/CFC2 commit their result to the
/// CPU GPR at the END of the NEXT instruction -- not at the end of
/// MFC2 itself. Rust's inline-asm output binding (`out("$8") value`)
/// makes the compiler insert a move-from-$8 right after the `.word`
/// block, and that move runs in the delay slot → sees the
/// pre-MFC2 value of $8, not the coprocessor data. The emulator
/// models this correctly (see `cpu::committing_load`), so without
/// the NOP every `read_data!` call silently returns stale data.
///
/// Encoded as two `.word`s: the MFC2 then a 32-bit zero (the MIPS
/// canonical NOP = `SLL $0, $0, 0`).
#[doc(alias = "MFC2")]
#[macro_export]
macro_rules! read_data {
    ($reg:literal) => {{
        // Out-of-range indices would spill into other encoding fields.
        const { assert!(($reg as u32) < 32, "GTE register index must be 0..=31") };
        let value: u32;
        #[cfg(target_arch = "mips")]
        // SAFETY: MFC2 copies one GTE data register into $t0. The asm
        // declares $t0 as its only output, touches no memory or stack
        // (`nomem`, `nostack`), and its trailing NOP covers the load-delay
        // slot so the compiler's read of $t0 sees the committed value.
        // The const assert above keeps `$reg` in 0..=31, so the shifted
        // index stays in the rd field (bits 15..11).
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                ".word 0",
                instr = const $crate::encoding::mfc2(8, $reg as u32),
                out("$8") value,
                options(nostack, nomem, preserves_flags)
            );
        }
        #[cfg(not(target_arch = "mips"))]
        {
            value = $crate::host::read_data($reg);
        }
        value
    }};
}

/// Write a GTE data register (`MTC2`). Expands to a single MTC2 instruction.
#[doc(alias = "MTC2")]
#[macro_export]
macro_rules! write_data {
    ($reg:literal, $value:expr) => {{
        // Out-of-range indices would spill into other encoding fields.
        const { assert!(($reg as u32) < 32, "GTE register index must be 0..=31") };
        let _value: u32 = $value;
        #[cfg(target_arch = "mips")]
        // SAFETY: MTC2 copies $t0 into one GTE data register. The asm only
        // reads $t0 (declared as its input), writes no CPU register, and
        // touches no memory or stack (`nomem`, `nostack`); GTE state is not
        // Rust-visible memory.
        // The const assert above keeps `$reg` in 0..=31, so the shifted
        // index stays in the rd field (bits 15..11).
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                instr = const $crate::encoding::mtc2(8, $reg as u32),
                in("$8") _value,
                options(nostack, nomem, preserves_flags)
            );
        }
        #[cfg(not(target_arch = "mips"))]
        {
            $crate::host::write_data($reg, _value);
        }
    }};
}

/// Read a GTE control register (`CFC2`). Same load-delay concern as
/// [`read_data!`] -- emits a NOP after the CFC2 to give the result a
/// cycle to commit to $8 before Rust's compiler-inserted move-from-$8
/// runs.
#[doc(alias = "CFC2")]
#[macro_export]
macro_rules! read_control {
    ($reg:literal) => {{
        // Out-of-range indices would spill into other encoding fields.
        const { assert!(($reg as u32) < 32, "GTE register index must be 0..=31") };
        let value: u32;
        #[cfg(target_arch = "mips")]
        // SAFETY: CFC2 copies one GTE control register into $t0. The asm
        // declares $t0 as its only output, touches no memory or stack
        // (`nomem`, `nostack`), and its trailing NOP covers the load-delay
        // slot so the compiler's read of $t0 sees the committed value.
        // The const assert above keeps `$reg` in 0..=31, so the shifted
        // index stays in the rd field (bits 15..11).
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                ".word 0",
                instr = const $crate::encoding::cfc2(8, $reg as u32),
                out("$8") value,
                options(nostack, nomem, preserves_flags)
            );
        }
        #[cfg(not(target_arch = "mips"))]
        {
            value = $crate::host::read_control($reg);
        }
        value
    }};
}

/// Write a GTE control register (`CTC2`).
#[doc(alias = "CTC2")]
#[macro_export]
macro_rules! write_control {
    ($reg:literal, $value:expr) => {{
        // Out-of-range indices would spill into other encoding fields.
        const { assert!(($reg as u32) < 32, "GTE register index must be 0..=31") };
        let _value: u32 = $value;
        #[cfg(target_arch = "mips")]
        // SAFETY: CTC2 copies $t0 into one GTE control register. The asm
        // only reads $t0 (declared as its input), writes no CPU register,
        // and touches no memory or stack (`nomem`, `nostack`); GTE state is
        // not Rust-visible memory.
        // The const assert above keeps `$reg` in 0..=31, so the shifted
        // index stays in the rd field (bits 15..11).
        unsafe {
            core::arch::asm!(
                ".word {instr}",
                instr = const $crate::encoding::ctc2(8, $reg as u32),
                in("$8") _value,
                options(nostack, nomem, preserves_flags)
            );
        }
        #[cfg(not(target_arch = "mips"))]
        {
            $crate::host::write_control($reg, _value);
        }
    }};
}

/// Renamed to [`read_data!`].
#[deprecated(note = "renamed to `read_data!`")]
#[macro_export]
macro_rules! mfc2 {
    ($($t:tt)*) => { $crate::read_data!($($t)*) };
}

/// Renamed to [`write_data!`].
#[deprecated(note = "renamed to `write_data!`")]
#[macro_export]
macro_rules! mtc2 {
    ($($t:tt)*) => { $crate::write_data!($($t)*) };
}

/// Helper: pack two i16 values into one u32 for MTC2/CTC2 of XY-pair
/// registers (V0 X/Y, SXY slots, rotation matrix rows, …).
///
/// The low 16 bits hold `x`, the high 16 bits hold `y`.
#[inline(always)]
pub const fn pack_xy(x: i16, y: i16) -> u32 {
    ((y as u16 as u32) << 16) | (x as u16 as u32)
}

/// Inverse of [`pack_xy`] -- split a packed word back into two `i16`s.
#[inline(always)]
pub const fn unpack_xy(value: u32) -> (i16, i16) {
    (value as i16, (value >> 16) as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_xy_round_trips_positive_values() {
        assert_eq!(pack_xy(0x100, 0x200), 0x0200_0100);
        assert_eq!(unpack_xy(0x0200_0100), (0x100, 0x200));
    }

    #[test]
    fn pack_xy_preserves_negative_sign() {
        // -1 packs to 0xFFFF in the low or high half without
        // contaminating the opposite half.
        assert_eq!(pack_xy(-1, 5), 0x0005_FFFF);
        assert_eq!(pack_xy(5, -1), 0xFFFF_0005);
        assert_eq!(unpack_xy(0xFFFF_0005), (5, -1));
        assert_eq!(unpack_xy(0x0005_FFFF), (-1, 5));
    }

    #[test]
    fn pack_xy_extreme_values() {
        assert_eq!(pack_xy(i16::MIN, i16::MAX), 0x7FFF_8000);
        assert_eq!(unpack_xy(0x7FFF_8000), (i16::MIN, i16::MAX));
    }

    /// Verify the MFC2/MTC2/CFC2/CTC2 encoding tables we hand-coded
    /// into the macros. If these ever drift from PSX-SPX, every GTE
    /// operation silently targets the wrong register.
    #[test]
    fn cop2_opcode_encodings_match_psx_spx() {
        // COP2 prefix = 0x12 << 26 = 0x4800_0000.
        // MFC2: rs=0; MTC2: rs=4; CFC2: rs=2; CTC2: rs=6.
        // All macros pin rt=$8 which contributes `8 << 16 = 0x0008_0000`.
        assert_eq!(0x4808_0000u32, (0x12u32 << 26) | (8 << 16));
        assert_eq!(0x4888_0000u32, (0x12u32 << 26) | (4 << 21) | (8 << 16));
        assert_eq!(0x4848_0000u32, (0x12u32 << 26) | (2 << 21) | (8 << 16));
        assert_eq!(0x48C8_0000u32, (0x12u32 << 26) | (6 << 21) | (8 << 16));
    }
}
