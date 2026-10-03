// SPDX-License-Identifier: GPL-2.0-or-later
//! STR video sectors: the 32-byte chunk header and frame reassembly.
//!
//! Every video sector's 2048 user bytes are a 32-byte header plus 2016
//! bytes of one frame's bitstream:
//!
//! | Bytes  | Field |
//! |--------|-------|
//! | 0..2   | `0x0160` |
//! | 2..4   | `0x8001` (MDEC video chunk) |
//! | 4..6   | chunk index within the frame |
//! | 6..8   | chunks in the frame |
//! | 8..12  | frame number (1-based) |
//! | 12..16 | frame bitstream size in bytes |
//! | 16..18 | width |
//! | 18..20 | height |
//! | 20..28 | copy of the BS frame header |
//!
//! Interleaved XA audio sectors never reach this layer: with the drive in
//! XA mode they go straight to the SPU and raise no data IRQ.

/// Bytes of chunk header at the start of each video sector.
pub const CHUNK_HEADER_BYTES: usize = 32;
/// Bitstream bytes carried per video sector.
pub const CHUNK_PAYLOAD_BYTES: usize = 2016;
/// Most chunks per frame [`FrameAssembler`] tracks (a 32-bit mask). At
/// 2016 bytes each that is 64 KB, far above any frame a 2x stream carries.
pub const MAX_CHUNKS: u16 = 32;

/// Parsed chunk header of one video sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Index of this chunk within its frame.
    pub index: u16,
    /// Chunks in the frame.
    pub count: u16,
    /// Frame number.
    pub frame: u32,
    /// Frame bitstream size in bytes.
    pub size: u32,
    /// Frame width in pixels.
    pub width: u16,
    /// Frame height in pixels.
    pub height: u16,
}

impl Chunk {
    /// Parse a sector's user data; `None` if it is not an STR video chunk.
    pub fn parse(sector: &[u8]) -> Option<Chunk> {
        if sector.len() < CHUNK_HEADER_BYTES {
            return None;
        }
        let h = |i: usize| u16::from_le_bytes([sector[i], sector[i + 1]]);
        let w =
            |i: usize| u32::from_le_bytes([sector[i], sector[i + 1], sector[i + 2], sector[i + 3]]);
        if h(0) != 0x0160 || h(2) != 0x8001 {
            return None;
        }
        let chunk = Chunk {
            index: h(4),
            count: h(6),
            frame: w(8),
            size: w(12),
            width: h(16),
            height: h(18),
        };
        if chunk.count == 0 || chunk.count > MAX_CHUNKS || chunk.index >= chunk.count {
            return None;
        }
        Some(chunk)
    }
}

/// A frame whose every chunk has arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Frame number from the chunk headers.
    pub number: u32,
    /// Bitstream bytes.
    pub size: u32,
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
}

/// Collects one frame's chunks into a caller-owned buffer.
///
/// Chunks may arrive in any order. A chunk from a new frame while the
/// current one is incomplete abandons it (a lost sector) and counts it in
/// [`dropped`](Self::dropped).
#[derive(Default)]
pub struct FrameAssembler {
    frame: u32,
    mask: u32,
    count: u16,
    /// Frames abandoned with chunks missing.
    pub dropped: u32,
}

impl FrameAssembler {
    /// Empty assembler.
    pub const fn new() -> Self {
        FrameAssembler {
            frame: 0,
            mask: 0,
            count: 0,
            dropped: 0,
        }
    }

    /// Add one sector (2048 user bytes). `buf` is the frame being filled
    /// and must hold `count * 2016` bytes. Returns the finished frame when
    /// this chunk completes it; the caller then hands `buf` to the decoder
    /// and passes a different buffer from now on.
    pub fn add(&mut self, sector: &[u8], buf: &mut [u8]) -> Option<Frame> {
        let chunk = Chunk::parse(sector)?;
        if chunk.frame != self.frame || chunk.count != self.count {
            if self.mask != 0 {
                self.dropped += 1;
            }
            self.frame = chunk.frame;
            self.count = chunk.count;
            self.mask = 0;
        }
        let at = chunk.index as usize * CHUNK_PAYLOAD_BYTES;
        let end = at + CHUNK_PAYLOAD_BYTES;
        if end > buf.len() || sector.len() < CHUNK_HEADER_BYTES + CHUNK_PAYLOAD_BYTES {
            return None;
        }
        buf[at..end]
            .copy_from_slice(&sector[CHUNK_HEADER_BYTES..CHUNK_HEADER_BYTES + CHUNK_PAYLOAD_BYTES]);
        self.mask |= 1 << chunk.index;
        let full = if chunk.count == 32 {
            u32::MAX
        } else {
            (1u32 << chunk.count) - 1
        };
        if self.mask != full {
            return None;
        }
        self.mask = 0;
        self.frame = 0;
        Some(Frame {
            number: chunk.frame,
            size: chunk.size,
            width: chunk.width,
            height: chunk.height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector(index: u16, count: u16, frame: u32, fill: u8) -> [u8; 2048] {
        let mut s = [fill; 2048];
        s[0..2].copy_from_slice(&0x0160u16.to_le_bytes());
        s[2..4].copy_from_slice(&0x8001u16.to_le_bytes());
        s[4..6].copy_from_slice(&index.to_le_bytes());
        s[6..8].copy_from_slice(&count.to_le_bytes());
        s[8..12].copy_from_slice(&frame.to_le_bytes());
        s[12..16].copy_from_slice(&5000u32.to_le_bytes());
        s[16..18].copy_from_slice(&320u16.to_le_bytes());
        s[18..20].copy_from_slice(&240u16.to_le_bytes());
        s
    }

    #[test]
    fn assembles_out_of_order_and_counts_drops() {
        let mut a = FrameAssembler::new();
        let mut buf = [0u8; 3 * CHUNK_PAYLOAD_BYTES];
        assert_eq!(a.add(&sector(2, 3, 1, 0xC2), &mut buf), None);
        assert_eq!(a.add(&sector(0, 3, 1, 0xC0), &mut buf), None);
        let f = a.add(&sector(1, 3, 1, 0xC1), &mut buf).unwrap();
        assert_eq!((f.number, f.size, f.width, f.height), (1, 5000, 320, 240));
        assert_eq!(buf[0], 0xC0);
        assert_eq!(buf[CHUNK_PAYLOAD_BYTES], 0xC1);
        assert_eq!(buf[2 * CHUNK_PAYLOAD_BYTES + 5], 0xC2);
        // Frame 2 loses a chunk; frame 3 completes and 2 counts as dropped.
        assert_eq!(a.add(&sector(0, 3, 2, 0), &mut buf), None);
        assert_eq!(a.add(&sector(0, 3, 3, 0), &mut buf), None);
        assert_eq!(a.add(&sector(1, 3, 3, 0), &mut buf), None);
        assert!(a.add(&sector(2, 3, 3, 0), &mut buf).is_some());
        assert_eq!(a.dropped, 1);
    }

    #[test]
    fn ignores_non_video_sectors() {
        let mut a = FrameAssembler::new();
        let mut buf = [0u8; CHUNK_PAYLOAD_BYTES];
        assert_eq!(a.add(&[0u8; 2048], &mut buf), None);
        assert_eq!(Chunk::parse(&sector(3, 3, 1, 0)), None);
    }
}
