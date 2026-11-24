//! Faulty scene discs for the scene core's failure gates: one defect written into a good raw disc
//! image (2352-byte sectors), every sector it touches given a valid EDC and ECC again, so the
//! drive delivers the bytes and only the core's own checks can catch them.
//!
//! The table is the one at `table_lba` (22 on a disc, the member base plus 22 for a member nested
//! in a launcher); image LBAs are relative to its base (`table_lba - 22`), as the core reads them.

use alloc::vec::Vec;

use psx_scene_abi::{
    decode_header_checked, image_row_offset, scene_row_offset, table_image, word_fnv, Kind,
    HEADER_BYTES, LBA_ABSOLUTE, ROW_BYTES, SECTOR_BYTES, TABLE_LBA,
};

use crate::iso9660::protect_mode2_form1;

const RAW: usize = 2352;
const DATA: usize = 24;

/// One defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_enums,
    reason = "the gate names each defect; a new one is a new gate case"
)]
pub enum Fault {
    /// The table's magic broken (a program's table read refuses it, 0x21).
    TableMagic,
    /// One byte of image `row`'s first loaded sector changed (its word FNV fails, 0x14).
    ImageByte(u8),
    /// The disc cut right after image `row`'s first loaded sector (its read fails, 0x13).
    Short(u8),
    /// Scene `scene` needs image `bit`, which the table does not have (refused, 0x22).
    Needs {
        /// The scene row.
        scene: u16,
        /// The image bit set in its required-images word.
        bit: u8,
    },
    /// Program row `row` carries another core's hash (refused, 0x23).
    CoreHash(u8),
}

/// Why a fault could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::exhaustive_enums, reason = "the gate reports every variant")]
pub enum FaultError {
    /// The disc is not whole raw sectors, or too short for its table.
    NotRaw,
    /// The table at `table_lba` does not decode.
    Table(psx_scene_abi::TableError),
    /// No such image row, or not the kind the fault needs, or its sectors are not on the disc.
    Row(u8),
    /// No such scene row.
    Scene(u16),
}

/// The first sector image `row` loads, absolute on the disc.
fn first_loaded(table: &[u8], header: &psx_scene_abi::Header, row: u8, base: u32) -> Option<u32> {
    let image = table_image(table, header, row)?;
    let lba = if image.lba & LBA_ABSOLUTE != 0 {
        image.lba & !LBA_ABSOLUTE
    } else {
        base + image.lba
    };
    Some(if image.kind == Kind::Program {
        lba + 1
    } else {
        lba
    })
}

/// `disc` with `fault` written into it, the table read at `table_lba`.
///
/// # Errors
///
/// A [`FaultError`] naming what does not fit the disc.
pub fn apply(disc: &[u8], table_lba: u32, fault: Fault) -> Result<Vec<u8>, FaultError> {
    if !disc.len().is_multiple_of(RAW) || disc.len() / RAW <= table_lba as usize {
        return Err(FaultError::NotRaw);
    }
    let mut out = disc.to_vec();
    let at = table_lba as usize * RAW;
    let base = table_lba.wrapping_sub(TABLE_LBA);
    let table: Vec<u8> = out[at + DATA..at + DATA + SECTOR_BYTES].to_vec();
    let header = decode_header_checked(&table).map_err(FaultError::Table)?;
    let sector_of = |lba: u32, out: &[u8]| -> Result<usize, FaultError> {
        let start = lba as usize * RAW;
        (start + RAW <= out.len())
            .then_some(start)
            .ok_or(FaultError::Row(0))
    };
    // Rewrite the table's rows' checksum after a row change, then protect the sector.
    let reseal = |out: &mut Vec<u8>| {
        let rows = usize::from(header.scene_count) + usize::from(header.image_count);
        let sum =
            word_fnv(&out[at + DATA + HEADER_BYTES..at + DATA + HEADER_BYTES + rows * ROW_BYTES]);
        out[at + DATA + 0x14..at + DATA + 0x18].copy_from_slice(&sum.to_le_bytes());
        protect_mode2_form1(&mut out[at..at + RAW]);
    };
    match fault {
        Fault::TableMagic => {
            out[at + DATA] ^= 0xff;
            protect_mode2_form1(&mut out[at..at + RAW]);
        }
        Fault::ImageByte(row) => {
            let lba = first_loaded(&table, &header, row, base).ok_or(FaultError::Row(row))?;
            let s = sector_of(lba, &out).map_err(|_| FaultError::Row(row))?;
            out[s + DATA + 64] ^= 0x5a;
            protect_mode2_form1(&mut out[s..s + RAW]);
        }
        Fault::Short(row) => {
            let lba = first_loaded(&table, &header, row, base).ok_or(FaultError::Row(row))?;
            let s = sector_of(lba, &out).map_err(|_| FaultError::Row(row))?;
            out.truncate(s + RAW);
        }
        Fault::Needs { scene, bit } => {
            if scene >= header.scene_count || bit >= 32 {
                return Err(FaultError::Scene(scene));
            }
            let w = at + DATA + scene_row_offset(scene) + 0x0c;
            let needs = u32::from_le_bytes([out[w], out[w + 1], out[w + 2], out[w + 3]]) | 1 << bit;
            out[w..w + 4].copy_from_slice(&needs.to_le_bytes());
            reseal(&mut out);
        }
        Fault::CoreHash(row) => {
            match table_image(&table, &header, row) {
                Some(image) if image.kind == Kind::Program => {}
                _ => return Err(FaultError::Row(row)),
            }
            let w = at + DATA + image_row_offset(header.scene_count, row) + 0x10;
            out[w] ^= 1;
            reseal(&mut out);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use psx_scene_abi::{encode_table, table_scene, ImageRow, SceneRow, SCENE_FLAG_START};

    /// A disc of `sectors` raw sectors, every one Mode 2 Form 1 with data byte 0 = its LBA, and
    /// a table at 22: one Program (header at 23, two sectors), one Overlay at 26.
    fn disc(sectors: usize) -> Vec<u8> {
        let mut out = vec![0u8; sectors * RAW];
        for (lba, s) in out.chunks_exact_mut(RAW).enumerate() {
            s[DATA] = lba as u8;
            s[16..24].copy_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
            protect_mode2_form1(s);
        }
        let program = ImageRow {
            lba: 23,
            size: 4096,
            load_addr: 0x8001_0000,
            checksum: 0,
            core_hash: 0x1234,
            evicts: 0,
            member_base: 0,
            kind: Kind::Program,
            class: None,
            owner: 0,
            cdda_delta: 0,
        };
        let overlay = ImageRow {
            lba: 26,
            core_hash: 0,
            kind: Kind::Overlay,
            class: Some(psx_scene_abi::Class::Swap),
            ..program
        };
        let scene = SceneRow {
            id: 0,
            image: 0,
            flags: SCENE_FLAG_START,
            entry: 0x8001_0000,
            partner_credit: 0,
            min_block_version: 2,
            needs: 0,
            name: SceneRow::name_bytes("START"),
        };
        let mut table = [0u8; SECTOR_BYTES];
        encode_table(&[scene], &[program, overlay], 0, &mut table).unwrap();
        let at = 22 * RAW;
        out[at + DATA..at + DATA + SECTOR_BYTES].copy_from_slice(&table);
        protect_mode2_form1(&mut out[at..at + RAW]);
        out
    }

    fn table_of(disc: &[u8], lba: usize) -> Vec<u8> {
        disc[lba * RAW + DATA..lba * RAW + DATA + SECTOR_BYTES].to_vec()
    }

    #[test]
    fn each_fault_changes_what_it_names_and_reseals_the_sector() {
        let good = disc(40);
        let magic = apply(&good, 22, Fault::TableMagic).unwrap();
        assert!(decode_header_checked(&table_of(&magic, 22)).is_err());
        // The Program's first loaded sector is its header's next one, 24.
        let byte = apply(&good, 22, Fault::ImageByte(0)).unwrap();
        assert_ne!(byte[24 * RAW + DATA + 64], good[24 * RAW + DATA + 64]);
        assert_ne!(byte[24 * RAW..25 * RAW], good[24 * RAW..25 * RAW]);
        let mut resealed = byte[24 * RAW..25 * RAW].to_vec();
        protect_mode2_form1(&mut resealed);
        assert_eq!(
            resealed,
            byte[24 * RAW..25 * RAW],
            "EDC and ECC match the new bytes"
        );
        // The Overlay loads from 26.
        let short = apply(&good, 22, Fault::Short(1)).unwrap();
        assert_eq!(short.len(), 27 * RAW);
        let needs = apply(&good, 22, Fault::Needs { scene: 0, bit: 9 }).unwrap();
        let t = table_of(&needs, 22);
        let h = decode_header_checked(&t).unwrap();
        assert_eq!(table_scene(&t, &h, 0).unwrap().needs, 1 << 9);
        let hash = apply(&good, 22, Fault::CoreHash(0)).unwrap();
        let t = table_of(&hash, 22);
        let h = decode_header_checked(&t).unwrap();
        assert_eq!(table_image(&t, &h, 0).unwrap().core_hash, 0x1235);
        assert_eq!(
            apply(&good, 22, Fault::CoreHash(1)),
            Err(FaultError::Row(1))
        );
        assert_eq!(
            apply(&good, 22, Fault::ImageByte(5)),
            Err(FaultError::Row(5))
        );
        assert_eq!(
            apply(&good[..100], 22, Fault::TableMagic),
            Err(FaultError::NotRaw)
        );
    }
}
