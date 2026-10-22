//! A cartridge image as the cook sees it: the DMA file table, Yaz0
//! decompression and file lookup by name.
//!
//! The ROM is the player's own; nothing read from it is ever written back or
//! committed. File names come from a `segments.csv` (one row per DMA entry,
//! first column the file name) because the cartridge itself carries none.

use crate::Error;
use std::path::Path;

/// File offset of the DMA table in the NTSC 1.2 image.
pub const DMA_TABLE_OFFSET_NTSC_12: usize = 0x7960;

/// Big-endian `u32` at `at`.
pub fn be_u32(b: &[u8], at: usize) -> Result<u32, Error> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(Error::Truncated(at))
}

/// Big-endian `u16` at `at`.
pub fn be_u16(b: &[u8], at: usize) -> Result<u16, Error> {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or(Error::Truncated(at))
}

struct DmaEntry {
    vrom_start: u32,
    vrom_end: u32,
    rom_start: u32,
    rom_end: u32,
}

/// A loaded ROM with its DMA table and file names.
pub struct Rom {
    data: Vec<u8>,
    dma: Vec<DmaEntry>,
    names: Vec<String>,
}

impl Rom {
    /// Load `rom` and name its files from `segments_csv`.
    pub fn load(rom: &Path, segments_csv: &Path) -> Result<Rom, Error> {
        let data = std::fs::read(rom).map_err(|e| Error::Io(format!("{}: {e}", rom.display())))?;
        let csv = std::fs::read_to_string(segments_csv)
            .map_err(|e| Error::Io(format!("{}: {e}", segments_csv.display())))?;
        let names = csv
            .lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap_or("").to_string())
            .collect();
        let mut dma = Vec::new();
        let mut at = DMA_TABLE_OFFSET_NTSC_12;
        while at + 16 <= data.len() {
            let vrom_end = be_u32(&data, at + 4)?;
            if vrom_end == 0 {
                break;
            }
            dma.push(DmaEntry {
                vrom_start: be_u32(&data, at)?,
                vrom_end,
                rom_start: be_u32(&data, at + 8)?,
                rom_end: be_u32(&data, at + 12)?,
            });
            at += 16;
        }
        Ok(Rom { data, dma, names })
    }

    /// The names of every file, in DMA order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The decompressed bytes of the file called `name`.
    pub fn file(&self, name: &str) -> Result<Vec<u8>, Error> {
        let idx = self
            .names
            .iter()
            .position(|n| n == name)
            .ok_or_else(|| Error::Missing(format!("file {name}")))?;
        self.file_at(idx)
    }

    /// The decompressed bytes of DMA entry `idx`.
    pub fn file_at(&self, idx: usize) -> Result<Vec<u8>, Error> {
        let e = self
            .dma
            .get(idx)
            .ok_or_else(|| Error::Missing(format!("DMA entry {idx}")))?;
        let start = e.rom_start as usize;
        if e.rom_end == 0 {
            let len = (e.vrom_end - e.vrom_start) as usize;
            self.data
                .get(start..start + len)
                .map(<[u8]>::to_vec)
                .ok_or(Error::Truncated(start))
        } else {
            let src = self
                .data
                .get(start..e.rom_end as usize)
                .ok_or(Error::Truncated(start))?;
            yaz0(src)
        }
    }
}

/// Decompress a Yaz0 stream.
pub fn yaz0(src: &[u8]) -> Result<Vec<u8>, Error> {
    if src.len() < 16 || &src[0..4] != b"Yaz0" {
        return Err(Error::Format("not a Yaz0 stream".into()));
    }
    let size = be_u32(src, 4)? as usize;
    let mut out = Vec::with_capacity(size);
    let mut pos = 16usize;
    let mut group = 0u8;
    let mut bits = 0u8;
    let byte = |p: usize| src.get(p).copied().ok_or(Error::Truncated(p));
    while out.len() < size {
        if bits == 0 {
            group = byte(pos)?;
            pos += 1;
            bits = 8;
        }
        if group & 0x80 != 0 {
            out.push(byte(pos)?);
            pos += 1;
        } else {
            let b0 = byte(pos)? as usize;
            let b1 = byte(pos + 1)? as usize;
            pos += 2;
            let dist = (((b0 & 0x0f) << 8) | b1) + 1;
            let len = match b0 >> 4 {
                0 => {
                    let l = byte(pos)? as usize + 0x12;
                    pos += 1;
                    l
                }
                n => n + 2,
            };
            if dist > out.len() {
                return Err(Error::Format("Yaz0 back reference before start".into()));
            }
            for _ in 0..len {
                let c = out[out.len() - dist];
                out.push(c);
            }
        }
        group <<= 1;
        bits -= 1;
    }
    out.truncate(size);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaz0_expands_literals_and_back_references() {
        // header, then: group byte 0b1110_0000 = three literals, then one
        // back reference of length 4 at distance 3 ("abcabca")
        let mut s = Vec::new();
        s.extend_from_slice(b"Yaz0");
        s.extend_from_slice(&7u32.to_be_bytes());
        s.extend_from_slice(&[0; 8]);
        s.push(0b1110_0000);
        s.extend_from_slice(b"abc");
        // length nibble 2 means 4 bytes; distance - 1 = 2
        s.extend_from_slice(&[0x20, 0x02]);
        assert_eq!(yaz0(&s).unwrap(), b"abcabca");
    }

    #[test]
    fn yaz0_rejects_other_data() {
        assert!(yaz0(b"not yaz0 at all, sixteen+ bytes").is_err());
    }

    #[test]
    fn big_endian_readers_check_bounds() {
        assert_eq!(be_u32(&[0, 0, 1, 0], 0).unwrap(), 256);
        assert!(be_u32(&[0, 0, 1], 0).is_err());
        assert_eq!(be_u16(&[1, 2], 0).unwrap(), 0x0102);
    }
}
