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
    let pvd = pvd.get(..190)?;
    if pvd[..6] != *b"\x01CD001" {
        return None;
    }
    Some(extent(&pvd[156..]))
}

/// Search one directory sector for `name` (compared without the `;1`
/// version suffix, ASCII case-insensitive). Returns `(lba, size_bytes)`. A directory of
/// several sectors is searched by calling this on each in turn.
///
/// `name` is a file name as ISO 9660 writes it (letters, digits, `_` and `.`, no version
/// suffix); a name with a `;` in it, or a character such as `@`, which the case fold equates
/// with another, is not supported. The record scan ends at the first record that is empty or
/// shorter than a name field (the zero padding after the last record of a sector).
///
/// The name's length is hidden from the optimiser on purpose: a lookup runs once per disc, and
/// with a constant `name` the compiler unrolls the comparison for it (648 bytes for an 8-byte
/// name, against 228 for the loop).
pub fn find_in_directory(sector: &[u8], name: &str) -> Option<(u32, u32)> {
    let name = name.as_bytes();
    let mut len = name.len();
    // SAFETY: an empty assembly block that returns its operand: the optimiser can no longer see
    // the length, so it keeps the comparison a loop. The same pointer and length follow.
    let name = unsafe {
        core::arch::asm!("/* {0} */", inout(reg) len, options(pure, nomem, nostack, preserves_flags));
        core::slice::from_raw_parts(name.as_ptr(), len)
    };
    let mut rest = sector;
    loop {
        let rec = rest.get(..usize::from(*rest.first()?))?;
        let (&name_len, raw) = rec.get(32..)?.split_first()?;
        if let Some(raw) = raw.get(..usize::from(name_len)) {
            if names_file(raw, name) {
                return Some(extent(rec));
            }
        }
        rest = &rest[rec.len()..];
    }
}

/// Whether a record's name, up to its `;` version suffix, is `name` apart from ASCII case.
fn names_file(raw: &[u8], name: &[u8]) -> bool {
    let Some((head, tail)) = raw.split_at_checked(name.len()) else {
        return false;
    };
    head.iter().zip(name).all(|(&a, &b)| same_letter(a, b))
        && matches!(tail.first(), None | Some(&b';'))
}

/// `a` and `b` are the same character apart from ASCII case. Exact for the characters a file name
/// holds (letters, digits, `_`, `.`, `;`); it also equates `@` with the backtick, `[` with `{` and the
/// like, which no ISO 9660 name contains.
fn same_letter(a: u8, b: u8) -> bool {
    (a ^ b) & 0xdf == 0
}

/// `(lba, size_bytes)` of a directory record (also the root record inside the volume descriptor).
fn extent(rec: &[u8]) -> (u32, u32) {
    (le32(&rec[2..6]), le32(&rec[10..14]))
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

    // The lookup before it was made smaller, kept as the reference the new one must agree with.
    fn reference_find(sector: &[u8], name: &str) -> Option<(u32, u32)> {
        let le32 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
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

    fn reference_root(pvd: &[u8]) -> Option<(u32, u32)> {
        if pvd.len() < 190 || pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return None;
        }
        let rec = &pvd[156..190];
        let le32 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        Some((le32(&rec[2..6]), le32(&rec[10..14])))
    }

    const REAL: [(&[u8], &[u8]); 2] = [
        (
            include_bytes!("../tests/fixtures/present-cd-pvd.bin"),
            include_bytes!("../tests/fixtures/present-cd-root.bin"),
        ),
        (
            include_bytes!("../tests/fixtures/flipcap-pvd.bin"),
            include_bytes!("../tests/fixtures/flipcap-root.bin"),
        ),
    ];

    /// Names to try: in the directory, in another case, prefixes of one, one byte longer.
    const NAMES: [&str; 17] = [
        "SYSTEM.CNF",
        "system.cnf",
        "System.Cnf",
        "PSX.EXE",
        "psx.exe",
        "CDTEST.BIN",
        "cdtest.bin",
        "PSX",
        "PSX.EX",
        "PSX.EXEE",
        "SYSTEM.CN",
        "SYSTEM.CNFX",
        "CDTEST",
        "MUSIC.XA",
        "",
        ".",
        "NOPE.STR",
    ];

    #[test]
    fn real_volume_descriptors_and_root_directories_agree_with_the_reference() {
        for (pvd, root) in REAL {
            assert_eq!(root_directory(pvd), reference_root(pvd));
            let (lba, size) = root_directory(pvd).expect("a real descriptor");
            assert_eq!((lba, size), (20, 2048));
            for name in NAMES {
                assert_eq!(
                    find_in_directory(root, name),
                    reference_find(root, name),
                    "{name}"
                );
            }
        }
        // The files the images hold are found, whatever the case.
        let root = REAL[0].1;
        assert!(find_in_directory(root, "psx.exe").is_some());
        assert!(find_in_directory(root, "cdtest.bin").is_some());
        assert!(find_in_directory(root, "PSX").is_none());
        // A damaged or short descriptor is refused as before.
        for cut in [0, 5, 100, 189] {
            assert_eq!(root_directory(&REAL[0].0[..cut]), None);
        }
        let mut bad = [0u8; 2048];
        bad.copy_from_slice(REAL[0].0);
        for at in 0..6 {
            let mut copy = bad;
            copy[at] ^= 1;
            assert_eq!(root_directory(&copy), reference_root(&copy));
            assert_eq!(root_directory(&copy), None);
        }
    }

    /// One directory record for the tests (lba and size in the numbers' own little-endian fields).
    fn put(sector: &mut [u8; 2048], at: &mut usize, name: &[u8], lba: u32, size: u32) {
        let len = 33 + name.len() + (name.len() + 1) % 2;
        assert!(*at + len <= 2048);
        sector[*at] = len as u8;
        sector[*at + 2..*at + 6].copy_from_slice(&lba.to_le_bytes());
        sector[*at + 10..*at + 14].copy_from_slice(&size.to_le_bytes());
        sector[*at + 32] = name.len() as u8;
        sector[*at + 33..*at + 33 + name.len()].copy_from_slice(name);
        *at += len;
    }

    #[test]
    fn names_that_are_prefixes_of_other_names_find_only_their_own() {
        let mut dir = [0u8; 2048];
        let mut at = 0;
        for (n, name) in [
            &b"\0"[..],
            b"\x01",
            b"MUSIC.XAB;1",
            b"MUSIC.XA;1",
            b"MUSIC.XA2;1",
            b"MUSIC;1",
            b"MUSIC.X;1",
            b"MUSIC.XA",
        ]
        .iter()
        .enumerate()
        {
            put(&mut dir, &mut at, name, 100 + n as u32, 1000 + n as u32);
        }
        // The first MUSIC.XA is the one with the version suffix (index 3), the later bare one is hidden.
        assert_eq!(find_in_directory(&dir, "MUSIC.XA"), Some((103, 1003)));
        assert_eq!(find_in_directory(&dir, "music.xa"), Some((103, 1003)));
        assert_eq!(find_in_directory(&dir, "MUSIC.XAB"), Some((102, 1002)));
        assert_eq!(find_in_directory(&dir, "MUSIC.XA2"), Some((104, 1004)));
        assert_eq!(find_in_directory(&dir, "MUSIC"), Some((105, 1005)));
        assert_eq!(find_in_directory(&dir, "MUSIC.X"), Some((106, 1006)));
        assert_eq!(find_in_directory(&dir, "MUSIC."), None);
        assert_eq!(find_in_directory(&dir, "MUSIC.XA3"), None);
        for name in [
            "MUSIC.XA",
            "music.xa",
            "MUSIC.XAB",
            "MUSIC.XA2",
            "MUSIC",
            "MUSIC.X",
            "MUSIC.",
            "MUSIC.XA3",
            "MUS",
        ] {
            assert_eq!(
                find_in_directory(&dir, name),
                reference_find(&dir, name),
                "{name}"
            );
        }
    }

    #[test]
    fn a_directory_of_several_sectors_is_searched_sector_by_sector() {
        // 130 records over three sectors: a record never crosses a sector, the rest of a sector is zero.
        let mut sectors = [[0u8; 2048]; 3];
        let mut names = [[0u8; 12]; 130];
        let (mut s, mut at) = (0, 0);
        for (n, name) in names.iter_mut().enumerate() {
            let text = [
                b'A' + (n / 26) as u8,
                b'A' + (n % 26) as u8,
                b'.',
                b'B',
                b'I',
                b'N',
                b';',
                b'1',
            ];
            name[..8].copy_from_slice(&text);
            if at + 33 + 8 + 1 > 2048 {
                s += 1;
                at = 0;
            }
            put(
                &mut sectors[s],
                &mut at,
                &text,
                1000 + n as u32,
                2048 * n as u32,
            );
        }
        assert_eq!(s, 2, "the fixture spans three sectors");
        for (n, name) in names.iter().enumerate() {
            let want = core::str::from_utf8(&name[..6]).unwrap();
            let found = sectors
                .iter()
                .find_map(|sector| find_in_directory(sector, want));
            assert_eq!(found, Some((1000 + n as u32, 2048 * n as u32)), "{want}");
            for sector in &sectors {
                assert_eq!(
                    find_in_directory(sector, want),
                    reference_find(sector, want)
                );
            }
        }
        assert!(sectors
            .iter()
            .all(|sector| find_in_directory(sector, "ZZ.BIN").is_none()));
    }

    /// File-name characters (ISO 9660 d-characters, and lower case as a lenient writer leaves it).
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.";

    #[test]
    fn the_case_fold_is_exact_for_every_file_name_character() {
        for &a in CHARS.iter().chain(b";") {
            for &b in CHARS.iter().chain(b";") {
                assert_eq!(same_letter(a, b), a.eq_ignore_ascii_case(&b), "{a} {b}");
            }
        }
    }

    #[test]
    fn random_directories_agree_with_the_reference() {
        let mut state = 0x2545_f491_u32;
        let mut next = move |n: u32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) % n
        };
        for round in 0..3000 {
            let mut dir = [0u8; 2048];
            let mut at = 0;
            let mut names = [([0u8; 12], 0usize); 12];
            let count = next(12) as usize;
            for slot in names.iter_mut().take(count) {
                let len = 1 + next(11) as usize;
                for k in 0..len {
                    slot.0[k] = CHARS[next(CHARS.len() as u32) as usize];
                }
                slot.1 = len;
                let mut full = [0u8; 14];
                full[..len].copy_from_slice(&slot.0[..len]);
                let total = if next(2) == 0 { len } else { len + 2 };
                if total > len {
                    full[len] = b';';
                    full[len + 1] = b'1';
                }
                put(
                    &mut dir,
                    &mut at,
                    &full[..total],
                    next(1 << 20),
                    next(1 << 20),
                );
            }
            // Names to look for: ones in the directory (in any case), prefixes and extensions of them, strangers.
            for slot in names.iter().take(count) {
                let mut probe = slot.0;
                let len = slot.1;
                for c in probe.iter_mut().take(len) {
                    if next(3) == 0 {
                        *c = c.to_ascii_lowercase();
                    }
                }
                let cut = next(len as u32 + 1) as usize;
                let extended = {
                    let mut e = probe;
                    if len < 12 {
                        e[len] = b'X';
                    }
                    e
                };
                for text in [&probe[..len], &probe[..cut], &extended[..(len + 1).min(12)]] {
                    let name = core::str::from_utf8(text).unwrap();
                    assert_eq!(
                        find_in_directory(&dir, name),
                        reference_find(&dir, name),
                        "round {round} {name}"
                    );
                }
            }
        }
    }
}
