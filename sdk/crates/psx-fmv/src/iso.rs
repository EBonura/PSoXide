// SPDX-License-Identifier: GPL-2.0-or-later
//! Minimal ISO9660 lookup: find a file in the disc's root directory.
//!
//! Enough for a movie player to open `MOVIE.STR` by name instead of by a
//! baked LBA: read the primary volume descriptor (LBA 16), take the root
//! directory extent from it, then scan that directory's records.

/// LBA of the primary volume descriptor.
pub const PVD_LBA: u32 = 16;

/// Root directory `(lba, size_bytes)` from the primary volume descriptor.
pub fn root_directory(pvd: &[u8]) -> Option<(u32, u32)> {
    if pvd.len() < 190 || pvd[0] != 1 || &pvd[1..6] != b"CD001" {
        return None;
    }
    let rec = &pvd[156..190];
    Some((le32(&rec[2..6]), le32(&rec[10..14])))
}

/// Search one directory sector for `name` (compared without the `;1`
/// version suffix, ASCII case-insensitive). Returns `(lba, size_bytes)`.
pub fn find_in_directory(sector: &[u8], name: &str) -> Option<(u32, u32)> {
    let mut at = 0;
    while at + 33 <= sector.len() {
        let len = sector[at] as usize;
        if len == 0 {
            break;
        }
        if at + len > sector.len() {
            break;
        }
        let rec = &sector[at..at + len];
        let name_len = rec[32] as usize;
        if 33 + name_len <= len {
            let raw = &rec[33..33 + name_len];
            let base = match raw.iter().position(|&b| b == b';') {
                Some(p) => &raw[..p],
                None => raw,
            };
            if base.eq_ignore_ascii_case(name.as_bytes()) {
                return Some((le32(&rec[2..6]), le32(&rec[10..14])));
            }
        }
        at += len;
    }
    None
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &[u8], lba: u32, size: u32) -> [u8; 48] {
        let mut r = [0u8; 48];
        let len = 33 + name.len() + (name.len() + 1) % 2;
        r[0] = len as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    #[test]
    fn finds_file_by_name() {
        let mut dir = [0u8; 2048];
        let mut at = 0;
        for (name, lba, size) in [
            (&b"\0"[..], 20, 2048),
            (b"PSX.EXE;1", 22, 90000),
            (b"MOVIE.STR;1", 70, 81920),
        ] {
            let r = record(name, lba, size);
            let len = r[0] as usize;
            dir[at..at + len].copy_from_slice(&r[..len]);
            at += len;
        }
        assert_eq!(find_in_directory(&dir, "movie.str"), Some((70, 81920)));
        assert_eq!(find_in_directory(&dir, "NOPE.STR"), None);
    }

    #[test]
    fn reads_root_from_pvd() {
        let mut pvd = [0u8; 2048];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[156 + 2..156 + 6].copy_from_slice(&20u32.to_le_bytes());
        pvd[156 + 10..156 + 14].copy_from_slice(&2048u32.to_le_bytes());
        assert_eq!(root_directory(&pvd), Some((20, 2048)));
    }
}
