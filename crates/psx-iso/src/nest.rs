//! A member scene disc nested whole inside an outer scene disc (a launcher's), and the rule the
//! outer table's row for it must keep.
//!
//! The member's sectors go after the outer disc's, unchanged but for their addresses, from a
//! 16-sector aligned `base`. One of the outer table's Program rows becomes the member's boot
//! program: the member's image row 0 with its LBA moved by `base` and `member_base = base`. The
//! scene core enters it like any program and the member then reads its own table at `base + 22`.
//!
//! The loaded mask the core writes on that entry is `(1 << position) | word5` (psx-core's `land`:
//! `position` is the row's index in the OUTER table, word 5 the row's carried word). The member
//! runs in its own numbering, so the nested row's word 5 is the member's whole entry mask: bit 0
//! (its boot program, its own image 0) and the overlays that program carries. The outer position
//! must not name one of the member's images (at least the member's image count) and must be below
//! 32 (the core shifts by the position, which wraps at 32), so the extra bit it sets can never
//! answer "loaded" for a member image: every load and switch checks the image index against the
//! member's own table first. [`check_nested_row`] is that rule; [`nest`] keeps it.

use alloc::vec::Vec;

use psx_scene_abi::{
    decode_header_checked, encode_table, table_image, table_scene, ImageRow, Kind, SceneRow,
    SECTOR_BYTES, TABLE_LBA,
};

use crate::iso9660::protect_mode2_form1;

/// A raw sector's bytes.
const RAW: usize = 2352;
/// Where the user data starts in a raw Mode 2 Form 1 sector.
const DATA: usize = 24;

/// Why a member cannot be nested.
#[derive(Debug, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_enums,
    reason = "the disc builders match every variant to report it"
)]
pub enum NestError {
    /// A disc image is not whole raw 2352-byte sectors, or has no table sector.
    NotRaw(&'static str),
    /// A table did not decode (outer or member).
    Table(&'static str, psx_scene_abi::TableError),
    /// The position is not an outer Program row.
    NotProgram(u8),
    /// The rule [`check_nested_row`] refuses.
    Rule(&'static str),
    /// The member's table names no scene that starts its image 0.
    NoStart,
}

/// The rule for an outer table row that boots a nested member: `position` is the row's index in
/// the outer table, `member_images` the member table's image count, `word5` the row's carried
/// word.
///
/// # Errors
///
/// A [`NestError::Rule`] naming the broken part.
pub fn check_nested_row(position: u8, member_images: u16, word5: u32) -> Result<(), NestError> {
    if position >= 32 {
        return Err(NestError::Rule("the outer position must be below 32"));
    }
    if u16::from(position) < member_images {
        return Err(NestError::Rule(
            "the outer position names one of the member's images; it must be at least the \
             member's image count",
        ));
    }
    if word5 & 1 == 0 {
        return Err(NestError::Rule(
            "word 5 must hold bit 0, the member's boot program in its own numbering",
        ));
    }
    Ok(())
}

/// Table bytes (2048) of a raw disc image.
fn table_bytes<'a>(disc: &'a [u8], which: &'static str) -> Result<&'a [u8], NestError> {
    let at = TABLE_LBA as usize * RAW + DATA;
    disc.get(at..at + SECTOR_BYTES)
        .ok_or(NestError::NotRaw(which))
}

/// Rewrite a raw sector's header address for `lba` (BCD minute, second, frame, 150 frames lead-in).
fn set_msf(sector: &mut [u8], lba: usize) {
    let frames = lba + 150;
    let bcd = |v: usize| (((v / 10) << 4) | (v % 10)) as u8;
    sector[12] = bcd(frames / 75 / 60);
    sector[13] = bcd(frames / 75 % 60);
    sector[14] = bcd(frames % 75);
}

/// Nest `member` (a raw scene disc) inside `outer` (a raw scene disc) at outer image row
/// `position`, which must be a Program row; every outer scene that starts that row starts the
/// member. With `entry_bit` false the row's word 5 is the member's carried word alone, without
/// bit 0: a disc that breaks the rule on purpose (its overlays are refused, 0x25), for tests.
///
/// # Errors
///
/// A [`NestError`] naming what is wrong with either disc or the position.
pub fn nest(
    outer: &[u8],
    position: u8,
    member: &[u8],
    entry_bit: bool,
) -> Result<Vec<u8>, NestError> {
    if !outer.len().is_multiple_of(RAW) {
        return Err(NestError::NotRaw("outer"));
    }
    if !member.len().is_multiple_of(RAW) {
        return Err(NestError::NotRaw("member"));
    }
    let otable = table_bytes(outer, "outer")?;
    let mtable = table_bytes(member, "member")?;
    let oh = decode_header_checked(otable).map_err(|e| NestError::Table("outer", e))?;
    let mh = decode_header_checked(mtable).map_err(|e| NestError::Table("member", e))?;
    let mut images: Vec<ImageRow> = (0..oh.image_count)
        .filter_map(|i| table_image(otable, &oh, i as u8))
        .collect();
    let mut scenes: Vec<SceneRow> = (0..oh.scene_count)
        .filter_map(|i| table_scene(otable, &oh, i))
        .collect();
    images
        .get(usize::from(position))
        .filter(|row| row.kind == Kind::Program)
        .ok_or(NestError::NotProgram(position))?;
    let boot = table_image(mtable, &mh, 0).ok_or(NestError::NoStart)?;
    let entry = (0..mh.scene_count)
        .filter_map(|i| table_scene(mtable, &mh, i))
        .find(|s| s.image == 0 && s.starts_program())
        .ok_or(NestError::NoStart)?
        .entry;

    let base = (outer.len() / RAW).div_ceil(16) * 16;
    let word5 = if entry_bit {
        boot.evicts | 1
    } else {
        boot.evicts & !1
    };
    if entry_bit {
        check_nested_row(position, mh.image_count, word5)?;
    }
    images[usize::from(position)] = ImageRow {
        lba: boot.lba + base as u32,
        member_base: base as u32,
        evicts: word5,
        ..boot
    };
    for scene in &mut scenes {
        if scene.image == position && scene.starts_program() {
            scene.entry = entry;
        }
    }
    let mut table = [0u8; SECTOR_BYTES];
    encode_table(&scenes, &images, oh.boot_scene, &mut table)
        .map_err(|e| NestError::Table("outer", e))?;

    let mut out = outer.to_vec();
    let at = TABLE_LBA as usize * RAW;
    out[at + DATA..at + DATA + SECTOR_BYTES].copy_from_slice(&table);
    protect_mode2_form1(&mut out[at..at + RAW]);
    out.resize(base * RAW, 0);
    for (i, sector) in member.chunks_exact(RAW).enumerate() {
        let mut s = sector.to_vec();
        set_msf(&mut s, base + i);
        out.extend_from_slice(&s);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule_refuses_each_broken_part() {
        assert!(check_nested_row(3, 2, 0b101).is_ok());
        assert!(check_nested_row(1, 2, 1).is_err());
        assert!(check_nested_row(32, 2, 1).is_err());
        assert!(check_nested_row(3, 2, 0b100).is_err());
    }

    #[test]
    fn msf_is_bcd_with_the_lead_in() {
        let mut s = [0u8; RAW];
        set_msf(&mut s, 0);
        assert_eq!(&s[12..15], &[0x00, 0x02, 0x00]);
        set_msf(&mut s, 1024);
        // 1174 frames: 0 min, 15 s, 49 f.
        assert_eq!(&s[12..15], &[0x00, 0x15, 0x49]);
    }
}
