//! `psoxide-hwtest radius-fill OUT SECTORS`: the filler file of the radius disc.
//!
//! `RADIUS.DAT` pads the hardware-test image out to about 78 minutes so the
//! CD RADIUS SWEEP step has outer-radius sectors to read. The words come from
//! the guest's own `filler.rs`, compiled in here, so the two cannot disagree.
//! The file is written as raw 2336-byte CD-XA sectors (the form `mkisopsx
//! --xa-file` takes): an eight-byte subheader marking a plain Form 1 data
//! sector, the 2048 data bytes, and the 280 bytes `mkisopsx` overwrites with
//! the sector's EDC and ECC. It lands as an ordinary data file.

use std::io::{BufWriter, Write};

use crate::util::{Error, Result};

#[path = "../../../engine/examples/hardware-tests/src/filler.rs"]
#[allow(dead_code)]
pub mod filler;

/// Bytes in one raw sector as `mkisopsx --xa-file` reads it.
pub const RAW_SECTOR: usize = 2336;
const SUBHEADER: [u8; 8] = [0, 0, 0x08, 0, 0, 0, 0x08, 0];

/// One raw sector of the file: sector `index`.
pub fn raw_sector(index: u32) -> [u8; RAW_SECTOR] {
    let mut out = [0u8; RAW_SECTOR];
    out[..8].copy_from_slice(&SUBHEADER);
    for word in 0..filler::SECTOR_WORDS {
        let at = 8 + word as usize * 4;
        out[at..at + 4].copy_from_slice(&filler::word(index, word).to_le_bytes());
    }
    out
}

/// The checksum of a group of sectors read in order, as the guest folds it
/// (FNV-1a over every word, the 32-bit result folded to 16 bits).
pub fn group_fold(first_sector: u32, sectors: u32) -> u16 {
    let mut hash = filler::FNV_START;
    for sector in first_sector..first_sector + sectors {
        for word in 0..filler::SECTOR_WORDS {
            hash = filler::fnv_step(hash, filler::word(sector, word));
        }
    }
    ((hash ^ (hash >> 16)) & 0xFFFF) as u16
}

pub fn run(args: &[String], out: &mut dyn Write) -> Result<i32> {
    let [path, sectors] = args else {
        return Err(Error("usage: radius-fill OUT SECTORS".into()));
    };
    let sectors: u32 = sectors
        .parse()
        .map_err(|_| Error(format!("bad sector count {sectors:?}")))?;
    if sectors == 0 {
        return Err(Error("sector count must be positive".into()));
    }
    let file = std::fs::File::create(path).map_err(|e| Error(format!("{path}: {e}")))?;
    let mut file = BufWriter::new(file);
    for index in 0..sectors {
        file.write_all(&raw_sector(index))
            .map_err(|e| Error(format!("{path}: {e}")))?;
    }
    file.flush().map_err(|e| Error(format!("{path}: {e}")))?;
    writeln!(out, "wrote {path}: {sectors} sectors, {} bytes", sectors as u64 * RAW_SECTOR as u64)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sector_is_subheader_data_and_room_for_the_parity() {
        let sector = raw_sector(5);
        assert_eq!(&sector[..8], &SUBHEADER);
        let word = u32::from_le_bytes(sector[8 + 4..8 + 8].try_into().unwrap());
        assert_eq!(word, filler::word(5, 1));
        assert!(sector[8 + 2048..].iter().all(|&b| b == 0));
    }

    #[test]
    fn the_group_fold_matches_a_manual_fold() {
        let mut hash = filler::FNV_START;
        for sector in 3..5 {
            for word in 0..filler::SECTOR_WORDS {
                hash = filler::fnv_step(hash, filler::word(sector, word));
            }
        }
        assert_eq!(group_fold(3, 2), ((hash ^ (hash >> 16)) & 0xFFFF) as u16);
    }
}
