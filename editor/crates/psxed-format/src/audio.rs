//! On-disk layout for cooked SPU audio (`.psau` files).
//!
//! PSAU is a thin wrapper around one-shot PS1 SPU ADPCM blocks. The
//! host pipeline owns WAV parsing, resampling, and ADPCM encoding; the
//! runtime only needs these offsets to upload the byte stream into SPU
//! RAM and configure a voice.

/// ASCII magic identifying the `.psau` format.
pub const MAGIC: [u8; 4] = *b"PSAU";

/// Current audio format revision.
pub const VERSION: u16 = 1;

/// Raw PS1 SPU ADPCM: 16 bytes decode to 28 mono samples.
pub const CODEC_SPU_ADPCM: u8 = 1;

/// Audio-specific flags stored in the shared
/// [`AssetHeader`](crate::AssetHeader).
pub mod flags {
    /// Sample is mono and can be uploaded to one SPU voice.
    pub const MONO: u16 = 1 << 0;
    /// Last ADPCM block carries an end flag and does not loop.
    pub const ONE_SHOT: u16 = 1 << 1;
}

/// Byte layout of the audio payload header after the common
/// [`AssetHeader`](crate::AssetHeader).
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
pub struct AudioHeader {
    /// Codec identifier, currently [`CODEC_SPU_ADPCM`].
    pub codec: u8,
    /// Channel count after cooking. Version 1 stores mono only.
    pub channel_count: u8,
    /// Alignment padding; writers set to zero, readers ignore.
    pub _pad: u16,
    /// Playback sample rate in Hz.
    pub sample_rate_hz: u32,
    /// Audible sample count before encoder padding.
    pub sample_count: u32,
    /// Number of 16-byte SPU ADPCM blocks following this header.
    pub adpcm_block_count: u32,
    /// Reserved loop-start block index. `u32::MAX` means no loop.
    pub loop_start_block: u32,
}

impl AudioHeader {
    /// Size of the audio header in bytes (always 20).
    pub const SIZE: usize = 20;

    /// Sentinel for non-looping samples.
    pub const NO_LOOP: u32 = u32::MAX;
}
