//! GTE (COP2) instruction encodings.
//!
//! The geometry engine is driven by two kinds of instruction: register
//! moves (`mfc2`, `mtc2`, `cfc2`, `ctc2`) and function commands (`RTPS`,
//! `MVMVA`, ...), which are COP2 instructions with bit 25 set. Both are
//! built here as `const fn`s so a guest can emit them as `.word`s the
//! assembler need not know, and a host tool can decode or generate the same
//! words.
//!
//! ```text
//!   bits 31..26 = 0x12 (COP2)
//!   bit  25     = 1 for a function command; 0 for a register move
//!   bit  19     = sf, fraction shift (commands)
//!   bits 18..17 = mx, MVMVA matrix select
//!   bits 16..15 = vx, MVMVA vector select
//!   bits 14..13 = cv, MVMVA translation select
//!   bit  10     = lm, IR lower-bound mode
//!   bits  5..0  = function number
//! ```
//!
//! Reference: nocash PSX-SPX "GTE Opcode Summary" and "GTE Coprocessor
//! Opcodes".

/// The COP2 major opcode, `010010`, in bits 31..26.
pub const COP2: u32 = 0x12 << 26;

/// Every function command starts with this prefix: COP2 with bit 25 set.
pub const COMMAND: u32 = COP2 | (1 << 25);

/// `sf`: shift the product right by 12 (clear: no shift).
pub const SF: u32 = 1 << 19;
/// `lm`: clamp IR1..IR3 at 0 instead of -0x8000.
pub const LM: u32 = 1 << 10;

/// Whether `word` is a GTE function command (top seven bits `0100101`).
///
/// An interrupt taken on such an instruction leaves EPC on it, so a handler
/// that returns to EPC runs the command twice; it steps over the word
/// instead.
#[inline(always)]
pub const fn is_command(word: u32) -> bool {
    word & 0xFE00_0000 == COMMAND
}

/// `mfc2 rt, rd`: copy GTE data register `rd` into CPU register `rt`.
#[inline(always)]
pub const fn mfc2(rt: u32, rd: u32) -> u32 {
    COP2 | (rt & 31) << 16 | (rd & 31) << 11
}

/// `cfc2 rt, rd`: copy GTE control register `rd` into CPU register `rt`.
#[inline(always)]
pub const fn cfc2(rt: u32, rd: u32) -> u32 {
    COP2 | 2 << 21 | (rt & 31) << 16 | (rd & 31) << 11
}

/// `mtc2 rt, rd`: copy CPU register `rt` into GTE data register `rd`.
#[inline(always)]
pub const fn mtc2(rt: u32, rd: u32) -> u32 {
    COP2 | 4 << 21 | (rt & 31) << 16 | (rd & 31) << 11
}

/// `ctc2 rt, rd`: copy CPU register `rt` into GTE control register `rd`.
#[inline(always)]
pub const fn ctc2(rt: u32, rd: u32) -> u32 {
    COP2 | 6 << 21 | (rt & 31) << 16 | (rd & 31) << 11
}

/// `MVMVA` matrix select (`mx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    /// The rotation matrix.
    Rotation = 0,
    /// The light matrix.
    Light = 1,
    /// The light-color matrix.
    Color = 2,
}

/// `MVMVA` vector select (`vx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vector {
    /// V0.
    V0 = 0,
    /// V1.
    V1 = 1,
    /// V2.
    V2 = 2,
    /// The IR1..IR3 vector.
    Ir = 3,
}

/// `MVMVA` translation select (`cv`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Translation {
    /// The translation vector TR.
    Tr = 0,
    /// The background color BK.
    Bk = 1,
    /// The far color FC. psx-spx documents this choice as hardware-bugged.
    Fc = 2,
    /// No translation.
    None = 3,
}

/// `MVMVA` with the given selects and `sf` set (`lm` clear).
#[inline(always)]
pub const fn mvmva(mx: Matrix, vx: Vector, cv: Translation) -> u32 {
    COMMAND | SF | (mx as u32) << 17 | (vx as u32) << 15 | (cv as u32) << 13 | 0x12
}

/// Function commands as the SDK issues them: `sf` set where the command
/// takes it, `lm` clear.
pub mod command {
    use super::{COMMAND, SF};

    /// Perspective transform of V0.
    pub const RTPS: u32 = COMMAND | SF | 0x01;
    /// Winding of the three screen XY entries.
    pub const NCLIP: u32 = COMMAND | 0x06;
    /// Outer product of IR with the rotation matrix diagonal.
    pub const OP: u32 = COMMAND | SF | 0x0C;
    /// Depth cue of the color in RGB.
    pub const DPCS: u32 = COMMAND | SF | 0x10;
    /// Interpolation of IR and the far color.
    pub const INTPL: u32 = COMMAND | SF | 0x11;
    /// Light, color and depth cue of V0.
    pub const NCDS: u32 = COMMAND | SF | 0x13;
    /// Color and depth cue of the light result.
    pub const CDP: u32 = COMMAND | SF | 0x14;
    /// Light, color and depth cue of V0, V1 and V2.
    pub const NCDT: u32 = COMMAND | SF | 0x16;
    /// Light and color of V0.
    pub const NCCS: u32 = COMMAND | SF | 0x1B;
    /// Color of the light result.
    pub const CC: u32 = COMMAND | SF | 0x1C;
    /// Light of V0.
    pub const NCS: u32 = COMMAND | SF | 0x1E;
    /// Light of V0, V1 and V2.
    pub const NCT: u32 = COMMAND | SF | 0x20;
    /// Square of IR1..IR3.
    pub const SQR: u32 = COMMAND | SF | 0x28;
    /// `SQR` without the fraction shift.
    pub const SQR_UNSHIFTED: u32 = COMMAND | 0x28;
    /// Depth cue of the color in RGB with IR.
    pub const DCPL: u32 = COMMAND | SF | 0x29;
    /// Depth cue of RGB0, RGB1 and RGB2.
    pub const DPCT: u32 = COMMAND | SF | 0x2A;
    /// Average of SZ1..SZ3 into OTZ.
    pub const AVSZ3: u32 = COMMAND | 0x2D;
    /// Average of SZ0..SZ3 into OTZ.
    pub const AVSZ4: u32 = COMMAND | 0x2E;
    /// Perspective transform of V0, V1 and V2.
    pub const RTPT: u32 = COMMAND | SF | 0x30;
    /// General-purpose interpolation.
    pub const GPF: u32 = COMMAND | SF | 0x3D;
    /// `GPF` without the fraction shift.
    pub const GPF_UNSHIFTED: u32 = COMMAND | 0x3D;
    /// General-purpose interpolation with accumulation.
    pub const GPL: u32 = COMMAND | SF | 0x3E;
    /// Light and color of V0, V1 and V2.
    pub const NCCT: u32 = COMMAND | SF | 0x3F;
    /// Rotate V0 by the rotation matrix and add TR.
    pub const ROTATE_TRANSLATE_V0: u32 = super::mvmva(
        super::Matrix::Rotation,
        super::Vector::V0,
        super::Translation::Tr,
    );
    /// Rotate V0 by the rotation matrix with the far color as translation.
    pub const ROTATE_V0_FAR_COLOR: u32 = super::mvmva(
        super::Matrix::Rotation,
        super::Vector::V0,
        super::Translation::Fc,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values from psx-spx "GTE Opcode Summary", the words the SDK emitted
    // before the builders existed.
    #[test]
    fn commands_match_the_documented_words() {
        assert_eq!(command::RTPS, 0x4A08_0001);
        assert_eq!(command::NCLIP, 0x4A00_0006);
        assert_eq!(command::OP, 0x4A08_000C);
        assert_eq!(command::DPCS, 0x4A08_0010);
        assert_eq!(command::INTPL, 0x4A08_0011);
        assert_eq!(command::NCDS, 0x4A08_0013);
        assert_eq!(command::CDP, 0x4A08_0014);
        assert_eq!(command::NCDT, 0x4A08_0016);
        assert_eq!(command::NCCS, 0x4A08_001B);
        assert_eq!(command::CC, 0x4A08_001C);
        assert_eq!(command::NCS, 0x4A08_001E);
        assert_eq!(command::NCT, 0x4A08_0020);
        assert_eq!(command::SQR, 0x4A08_0028);
        assert_eq!(command::SQR_UNSHIFTED, 0x4A00_0028);
        assert_eq!(command::DCPL, 0x4A08_0029);
        assert_eq!(command::DPCT, 0x4A08_002A);
        assert_eq!(command::AVSZ3, 0x4A00_002D);
        assert_eq!(command::AVSZ4, 0x4A00_002E);
        assert_eq!(command::RTPT, 0x4A08_0030);
        assert_eq!(command::GPF, 0x4A08_003D);
        assert_eq!(command::GPF_UNSHIFTED, 0x4A00_003D);
        assert_eq!(command::GPL, 0x4A08_003E);
        assert_eq!(command::NCCT, 0x4A08_003F);
        assert_eq!(command::ROTATE_TRANSLATE_V0, 0x4A08_0012);
        assert_eq!(command::ROTATE_V0_FAR_COLOR, 0x4A08_4012);
    }

    #[test]
    fn mvmva_places_each_select_in_its_field() {
        let word = mvmva(Matrix::Color, Vector::Ir, Translation::None);
        assert_eq!(word, 0x4A08_0012 | 2 << 17 | 3 << 15 | 3 << 13);
        assert!(is_command(word));
    }

    #[test]
    fn register_moves_encode_the_documented_fields() {
        // The words the guest GTE macros and the RTPT sequence emit.
        assert_eq!(mfc2(8, 0), 0x4808_0000);
        assert_eq!(mfc2(8, 31), 0x4808_F800);
        assert_eq!(mtc2(8, 0), 0x4888_0000);
        assert_eq!(mtc2(9, 1), 0x4889_0800);
        assert_eq!(cfc2(8, 5), 0x4848_2800);
        assert_eq!(ctc2(8, 5), 0x48C8_2800);
    }

    #[test]
    fn only_commands_are_commands() {
        assert!(is_command(command::RTPT));
        assert!(!is_command(mtc2(8, 0)));
        assert!(!is_command(0));
    }
}
