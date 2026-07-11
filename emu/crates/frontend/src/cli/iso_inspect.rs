//! Minimal ISO-9660 inspection for `preburn-check`: volume ID and
//! root-directory entries read from a mounted disc image.

use psx_iso::Disc;

#[derive(Clone, Debug)]
pub(super) struct IsoRootEntry {
    pub(super) identifier: String,
    extent_lba: u32,
    size: u32,
    flags: u8,
}

impl IsoRootEntry {
    pub(super) fn is_dir(&self) -> bool {
        self.flags & 0x02 != 0
    }
}

pub(super) fn iso_volume_id(disc: &Disc) -> Result<String, String> {
    let pvd = disc
        .read_sector_user(16)
        .ok_or_else(|| "missing ISO9660 primary volume descriptor at LBA 16".to_string())?;
    if pvd.len() < 72 || pvd[0] != 1 || &pvd[1..6] != b"CD001" || pvd[6] != 1 {
        return Err("LBA 16 is not an ISO9660 primary volume descriptor".to_string());
    }
    Ok(String::from_utf8_lossy(&pvd[40..72]).trim().to_string())
}

pub(super) fn iso_root_entries(disc: &Disc) -> Result<Vec<IsoRootEntry>, String> {
    let pvd = disc
        .read_sector_user(16)
        .ok_or_else(|| "missing ISO9660 primary volume descriptor at LBA 16".to_string())?;
    if pvd.len() < 190 || pvd[0] != 1 || &pvd[1..6] != b"CD001" || pvd[6] != 1 {
        return Err("LBA 16 is not an ISO9660 primary volume descriptor".to_string());
    }
    let root =
        parse_iso_dir_record(&pvd[156..]).ok_or_else(|| "bad root directory record".to_string())?;
    if !root.is_dir() {
        return Err("ISO9660 root record is not a directory".to_string());
    }
    let bytes = read_iso_extent(disc, root.extent_lba, root.size)?;
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let len = bytes[offset] as usize;
        if len == 0 {
            offset =
                ((offset / psx_iso::SECTOR_USER_DATA_BYTES) + 1) * psx_iso::SECTOR_USER_DATA_BYTES;
            continue;
        }
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| "bad ISO9660 directory record length".to_string())?;
        let entry = parse_iso_dir_record(&bytes[offset..end])
            .ok_or_else(|| "bad ISO9660 directory record".to_string())?;
        if entry.identifier != "\u{0}" && entry.identifier != "\u{1}" {
            entries.push(entry);
        }
        offset = end;
    }
    Ok(entries)
}

fn parse_iso_dir_record(record: &[u8]) -> Option<IsoRootEntry> {
    let len = *record.first()? as usize;
    if len < 34 || record.len() < len {
        return None;
    }
    let ident_len = *record.get(32)? as usize;
    let ident_start = 33usize;
    let ident_end = ident_start.checked_add(ident_len)?;
    if ident_end > len {
        return None;
    }
    Some(IsoRootEntry {
        extent_lba: u32::from_le_bytes(record.get(2..6)?.try_into().ok()?),
        size: u32::from_le_bytes(record.get(10..14)?.try_into().ok()?),
        flags: *record.get(25)?,
        identifier: String::from_utf8(record[ident_start..ident_end].to_vec()).ok()?,
    })
}

fn read_iso_extent(disc: &Disc, extent_lba: u32, size: u32) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(size as usize);
    let mut remaining = size as usize;
    let mut lba = extent_lba;
    while remaining > 0 {
        let sector = disc
            .read_sector_user(lba)
            .ok_or_else(|| format!("unreadable ISO9660 extent sector LBA {lba}"))?;
        let take = remaining.min(sector.len());
        bytes.extend_from_slice(&sector[..take]);
        remaining -= take;
        lba = lba.saturating_add(1);
    }
    Ok(bytes)
}

pub(super) fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
