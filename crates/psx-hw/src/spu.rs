//! SPU register addresses.
//!
//! Reference: nocash PSX-SPX "Sound Processing Unit (SPU)".

/// Base of the SPU register bank. Voice, volume and reverb registers sit at
/// fixed offsets from it.
pub const BASE: u32 = 0x1F80_1C00;
/// Control register.
pub const SPUCNT: u32 = 0x1F80_1DAA;
/// Status register.
pub const SPUSTAT: u32 = 0x1F80_1DAE;
/// Sound-RAM transfer address register (`address / 8`). Latches the SPU-RAM
/// cursor for FIFO and DMA uploads and downloads.
pub const TRANSFER_ADDR: u32 = 0x1F80_1DA6;
/// Sound-RAM transfer FIFO (manual-write port). Writes land in SPU RAM only
/// while the control register's transfer mode is manual write (bits 5..4 =
/// 01).
pub const TRANSFER_DATA: u32 = 0x1F80_1DA8;
/// Sound-RAM transfer control register (transfer type; 0x0004 is the normal
/// byte order every game uses).
pub const TRANSFER_CTRL: u32 = 0x1F80_1DAC;
/// Sound-RAM IRQ address register (`address / 8`).
pub const IRQ_ADDR: u32 = 0x1F80_1DA4;
/// Start of the reverb work area in sound RAM (`address / 8`).
pub const REVERB_WORK_BASE: u32 = 0x1F80_1DA2;

/// Per-voice registers: 24 voices, 16 bytes each from [`BASE`].
pub mod voice {
    /// Number of voices.
    pub const COUNT: u32 = 24;
    /// Distance between two voices' register blocks.
    pub const STRIDE: u32 = 0x10;
    /// Offset of the left volume register.
    pub const VOL_LEFT: u32 = 0x0;
    /// Offset of the right volume register.
    pub const VOL_RIGHT: u32 = 0x2;
    /// Offset of the pitch (sample rate) register.
    pub const PITCH: u32 = 0x4;
    /// Offset of the sample start address (`address / 8`).
    pub const START_ADDR: u32 = 0x6;
    /// Offset of the low half of the ADSR word.
    pub const ADSR_LO: u32 = 0x8;
    /// Offset of the high half of the ADSR word.
    pub const ADSR_HI: u32 = 0xA;
    /// Offset of the current envelope level (read only).
    pub const ADSR_LEVEL: u32 = 0xC;
    /// Offset of the loop repeat address (`address / 8`).
    pub const REPEAT_ADDR: u32 = 0xE;

    /// Address of voice `n`'s register block (`n` in `0..COUNT`).
    #[inline(always)]
    pub const fn base(n: u32) -> u32 {
        super::BASE + n * STRIDE
    }
}

/// Main volume, left.
pub const MAIN_VOL_LEFT: u32 = 0x1F80_1D80;
/// Main volume, right.
pub const MAIN_VOL_RIGHT: u32 = 0x1F80_1D82;
/// Reverb output volume, left.
pub const REVERB_VOL_LEFT: u32 = 0x1F80_1D84;
/// Reverb output volume, right.
pub const REVERB_VOL_RIGHT: u32 = 0x1F80_1D86;
/// CD audio input volume, left.
pub const CD_VOL_LEFT: u32 = 0x1F80_1DB0;
/// CD audio input volume, right.
pub const CD_VOL_RIGHT: u32 = 0x1F80_1DB2;

/// Voice-mask registers: one bit per voice, 16 in the low register and 8 in
/// the high one, the high register 2 bytes above the low.
pub mod mask {
    /// Offset of a mask's high half from its low half.
    pub const HIGH: u32 = 2;
    /// Key on (start the envelope), voices 0..=15.
    pub const KEY_ON_LO: u32 = 0x1F80_1D88;
    /// Key on, voices 16..=23.
    pub const KEY_ON_HI: u32 = KEY_ON_LO + HIGH;
    /// Key off (release the envelope), voices 0..=15.
    pub const KEY_OFF_LO: u32 = 0x1F80_1D8C;
    /// Key off, voices 16..=23.
    pub const KEY_OFF_HI: u32 = KEY_OFF_LO + HIGH;
    /// Pitch modulation by the previous voice, voices 0..=15.
    pub const PITCH_MOD_LO: u32 = 0x1F80_1D90;
    /// Pitch modulation, voices 16..=23.
    pub const PITCH_MOD_HI: u32 = PITCH_MOD_LO + HIGH;
    /// Noise instead of samples, voices 0..=15.
    pub const NOISE_LO: u32 = 0x1F80_1D94;
    /// Noise, voices 16..=23.
    pub const NOISE_HI: u32 = NOISE_LO + HIGH;
    /// Reverb enable, voices 0..=15.
    pub const REVERB_ENABLE_LO: u32 = 0x1F80_1D98;
    /// Reverb enable, voices 16..=23.
    pub const REVERB_ENABLE_HI: u32 = REVERB_ENABLE_LO + HIGH;
    /// ENDX, voices 0..=15: one sticky bit per voice, set when that voice
    /// decodes a block with the END flag. Write to clear.
    pub const ENDX_LO: u32 = 0x1F80_1D9C;
    /// ENDX, voices 16..=23.
    pub const ENDX_HI: u32 = ENDX_LO + HIGH;
}

/// Value of [`TRANSFER_CTRL`] every game uses: the normal byte order.
pub const TRANSFER_CTRL_NORMAL: u16 = 0x0004;

/// [`SPUCNT`] bits and fields.
pub mod control {
    /// Bit 15: the SPU is enabled.
    pub const ENABLE: u16 = 1 << 15;
    /// Bit 14: output is unmuted (clear: muted).
    pub const UNMUTE: u16 = 1 << 14;
    /// Bits 13..=8: noise clock, step in bits 9..=8 and shift in 13..=10.
    pub const NOISE_CLOCK_MASK: u16 = 0x3F00;
    /// Bit 7: reverb master enable.
    pub const REVERB_MASTER: u16 = 1 << 7;
    /// Bit 6: raise the SPU IRQ when the IRQ address is accessed.
    pub const IRQ_ENABLE: u16 = 1 << 6;
    /// Bits 5..=4: sound-RAM transfer mode.
    pub const TRANSFER_MODE_MASK: u16 = 0b11 << 4;
    /// Transfer mode 1: the CPU writes through the transfer FIFO.
    pub const TRANSFER_MANUAL_WRITE: u16 = 1 << 4;
    /// Transfer mode 2: DMA writes sound RAM.
    pub const TRANSFER_DMA_WRITE: u16 = 2 << 4;
    /// Bit 0: mix the CD audio input.
    pub const CD_AUDIO_ENABLE: u16 = 1 << 0;
    /// Bits 5..=0: the part of the register the SPU applies after a delay;
    /// [`status::MODE_MASK`] reads the applied value back.
    pub const MODE_MASK: u16 = 0x3F;
}

/// [`SPUSTAT`] bits and fields.
pub mod status {
    /// Bits 5..=0: the applied copy of the control register's low bits.
    pub const MODE_MASK: u16 = 0x3F;
    /// Bit 6: the SPU IRQ latch is set.
    pub const IRQ_FLAG: u16 = 1 << 6;
    /// Bit 10: a sound-RAM transfer is still draining.
    pub const TRANSFER_BUSY: u16 = 1 << 10;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_blocks_tile_the_bank_below_the_global_registers() {
        assert_eq!(voice::base(0), BASE);
        assert_eq!(voice::base(1), 0x1F80_1C10);
        // psx-spx: voice 23 is the last block, ending right before 1F801D80h.
        assert_eq!(voice::base(voice::COUNT - 1) + voice::STRIDE, MAIN_VOL_LEFT);
    }

    #[test]
    fn mask_halves_sit_two_bytes_apart() {
        assert_eq!(mask::KEY_ON_HI, 0x1F80_1D8A);
        assert_eq!(mask::KEY_OFF_HI, 0x1F80_1D8E);
        assert_eq!(mask::ENDX_HI, 0x1F80_1D9E);
        assert_eq!(mask::REVERB_ENABLE_HI, 0x1F80_1D9A);
    }

    #[test]
    fn global_registers_follow_psx_spx() {
        assert_eq!(REVERB_WORK_BASE, 0x1F80_1DA2);
        assert_eq!(IRQ_ADDR, 0x1F80_1DA4);
        assert_eq!(CD_VOL_RIGHT, 0x1F80_1DB2);
        assert_eq!(SPUCNT, 0x1F80_1DAA);
    }
}
