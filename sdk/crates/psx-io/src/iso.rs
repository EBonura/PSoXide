// SPDX-License-Identifier: GPL-2.0-or-later
//! Find a file on an ISO 9660 disc by name: the one lookup every game and tool on the SDK uses.
//!
//! Two layers, one implementation. [`root_directory`] and [`find_in_directory`] read the disc's
//! records out of a sector the caller has already read; a program that has its own way of reading
//! sectors (a held stream, a poll that keeps the pad serviced, a host file) reads the volume
//! descriptor, the root directory and the sectors after it itself and asks these two. [`find`] is
//! the loop around them for a program that wants to hand over a sector read and a path: it reads
//! the volume descriptor, takes the root directory from it and walks the names down, searching
//! every sector of each directory. It owns no memory and touches no hardware: the caller supplies
//! the sector buffer and the read.
//!
//! Names are compared byte for byte, up to the `;version` the disc appends. ISO 9660 writes
//! file names in upper case, and so must the names passed in.
//!
//! ```
//! use psx_io::iso::{find, Sector};
//! let mut sector: Sector = [0; psx_io::cd::reader::SECTOR_WORDS];
//! // A read that always fails finds nothing.
//! assert_eq!(find(&mut sector, |_lba, _into| false, None, &["MUSIC.XA"]), None);
//! ```

use crate::cd::reader::SECTOR_WORDS;

/// LBA of the primary volume descriptor.
pub const PVD_LBA: u32 = 16;

/// The bytes of a sector.
pub const BYTES: usize = SECTOR_WORDS * 4;

/// One sector, as the CD reader fills it.
pub type Sector = [u32; SECTOR_WORDS];

/// `(first sector, length in bytes)` of a file or a directory.
pub type Extent = (u32, u32);

/// The sectors of a directory or file of `bytes` bytes (the last may be partly used).
#[must_use]
pub const fn sectors(bytes: u32) -> u32 {
    bytes.div_ceil(BYTES as u32)
}

/// A sector's words as its bytes.
#[must_use]
pub fn as_bytes(sector: &Sector) -> &[u8; BYTES] {
    // SAFETY: a `[u32; N]` viewed as its own `4 * N` bytes; alignment only shrinks.
    unsafe { &*(&raw const *sector).cast::<[u8; BYTES]>() }
}

/// The root directory of the volume, from the volume descriptor `sector` (sector 16). `None` when
/// the sector is not a volume descriptor.
#[must_use]
pub fn root_directory(sector: &[u8; BYTES]) -> Option<Extent> {
    (sector[..6] == *b"\x01CD001").then(|| extent(sector, 156))
}

/// The entry named `name` in the directory sector `sector`. `None` when the sector holds no such
/// entry; a directory of several sectors ([`sectors`]) is searched by asking each in turn.
///
/// `name` (a `&str` or any bytes) is a file or directory name as ISO 9660 writes it: upper case,
/// letters, digits, `_` and `.`, no `;`. The name on the disc may carry a `;version` after it; a
/// name that only starts with `name` is a different name. The scan ends at the first record that is empty (the zero padding
/// after the last record of a sector).
#[must_use]
pub fn find_in_directory(
    sector: &[u8; BYTES],
    name: &(impl AsRef<[u8]> + ?Sized),
) -> Option<Extent> {
    let name = hidden(name.as_ref());
    let len = name.len();
    if len.wrapping_sub(1) >= usize::from(u8::MAX) {
        // No name, or one longer than any record holds; and the sums below cannot wrap.
        return None;
    }
    // The last offset a record can start at and still hold `name` inside the sector.
    let last = BYTES - 33 - len;
    let mut found = None;
    let mut at = 0;
    while at <= last && sector[at] != 0 {
        // A name on the disc shorter than `name` fails the compare (a record's name is followed by
        // a pad byte or the next record's length, never by the rest of a file name); one longer
        // must go on with `;`. The byte after the name is read through a mask: a damaged record
        // cannot reach past the sector.
        if same(&sector[at + 33..at + 33 + len], name)
            && (usize::from(sector[at + 32]) == len
                || sector[(at + 33 + len) & (BYTES - 1)] == b';')
        {
            found = Some(extent(sector, at));
            break;
        }
        at += usize::from(sector[at]);
    }
    found
}

/// The file or directory `path` names, down from `from` (the root directory when `None`), as
/// `(first sector, length in bytes)`: `&["MUSIC.XA"]` for a file in the root, `&["GAME",
/// "DATA.BIN"]` one directory down. An empty `path` is `from` itself, so `find(.., None, &[])`
/// is the root directory, which a program that looks several files up can keep and pass as
/// `from` the next time.
///
/// `read(lba, sector)` fills `sector` with sector `lba` and says whether it could; `find` calls
/// it once for the volume descriptor and once for each sector of each directory it searches,
/// and the sector it last read stays in `sector`. `None` when a name is not found or a read
/// fails.
///
/// Always inlined: it is the one loop around the sector read, and a program calls it from one
/// place (a program with several calls wraps them in a function of its own).
#[inline(always)]
pub fn find(
    sector: &mut Sector,
    mut read: impl FnMut(u32, &mut Sector) -> bool,
    from: Option<Extent>,
    path: &[&str],
) -> Option<Extent> {
    let (mut lba, mut left, mut next, mut name) = match from {
        // The volume descriptor stands for the root: the first "directory" of the walk.
        None => (PVD_LBA, 1, 0, ""),
        Some(dir) => {
            let Some(&first) = path.first() else {
                return Some(dir);
            };
            (dir.0, sectors(dir.1), 1, first)
        }
    };
    // SAFETY: an empty assembly block that returns its operand. The compiler is not told which
    // sector is read first: told, it copies the read for the volume descriptor and again for
    // the directories.
    #[cfg(target_arch = "mips")]
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) lba, options(pure, nomem, nostack, preserves_flags))
    };
    while left != 0 {
        if !read(lba, sector) {
            return None;
        }
        let s = as_bytes(sector);
        let hit = if next == 0 {
            root_directory(s)
        } else {
            find_in_directory(s, name)
        };
        match hit {
            Some((first, size)) => {
                let Some(entry) = path.get(next) else {
                    return Some((first, size));
                };
                (name, next) = (entry, next + 1);
                (lba, left) = (first, sectors(size));
            }
            None if next == 0 => return None,
            None => (lba, left) = (lba.wrapping_add(1), left - 1),
        }
    }
    None
}

/// The most root-directory sectors [`find_in_root`] searches. A root of about a hundred files fits
/// in four; a descriptor that claims more is damaged or not ours, and the walk stays bounded.
pub const ROOT_SECTORS_MAX: u32 = 4;

/// Read the volume descriptor and the first sector of the root directory into `sector`,
/// leaving the root sector there: the single-sector start of a lookup for a caller that walks
/// the entries itself with [`find_in_directory`] (several names, a size check on each hit,
/// its own failure policy). `read(lba, sector)` is the read [`find`] takes. `false` when a read
/// fails or the volume descriptor is not one. Always inlined: the caller's read stays its own
/// function, so the call adds nothing the hand-written two reads did not have.
///
/// ```
/// use psx_io::iso::{read_root_sector, Sector};
/// let mut sector: Sector = [0; psx_io::cd::reader::SECTOR_WORDS];
/// assert!(!read_root_sector(&mut sector, |_lba, _into| false));
/// ```
#[inline(always)]
pub fn read_root_sector(
    sector: &mut Sector,
    mut read: impl FnMut(u32, &mut Sector) -> bool,
) -> bool {
    if !read(PVD_LBA, sector) {
        return false;
    }
    let Some((root, _)) = root_directory(as_bytes(sector)) else {
        return false;
    };
    read(root, sector)
}

/// Look several names up in the root directory in one pass: the volume descriptor, then up to
/// [`ROOT_SECTORS_MAX`] sectors of the root, each name searched in each sector. `out[i]` is the
/// extent of `names[i]`, or `None` (every slot is written, so stale entries cannot survive);
/// `names` and `out` are matched pairwise up to the shorter. The XA players, the launcher and the
/// games with a handful of files to find on the disc use this instead of walking the directory
/// by hand. `read(lba, sector)` is the same read [`find`] takes (a closure over
/// `SectorReader::read_at`, or a program's own held-session read).
///
/// `false` when the volume descriptor or a root sector could not be read, or the descriptor is
/// not one; names found before a failed read stay in `out`. Always inlined, like [`find`].
#[inline(always)]
pub fn find_in_root(
    sector: &mut Sector,
    mut read: impl FnMut(u32, &mut Sector) -> bool,
    names: &[&str],
    out: &mut [Option<Extent>],
) -> bool {
    for slot in out.iter_mut() {
        *slot = None;
    }
    if !read(PVD_LBA, sector) {
        return false;
    }
    let Some((root, size)) = root_directory(as_bytes(sector)) else {
        return false;
    };
    let mut at = 0;
    while at < sectors(size).min(ROOT_SECTORS_MAX) {
        if !read(root.wrapping_add(at), sector) {
            return false;
        }
        let bytes = as_bytes(sector);
        for (slot, name) in out.iter_mut().zip(names) {
            if slot.is_none() {
                *slot = find_in_directory(bytes, name);
            }
        }
        at += 1;
    }
    true
}

/// Whether two slices of one length hold the same bytes: a loop right here, so the function around
/// it stays a leaf that makes no call and saves no registers (the library's compare is a call).
#[inline(always)]
fn same(a: &[u8], b: &[u8]) -> bool {
    let mut differ = 0u32;
    for (x, y) in a.iter().zip(b) {
        differ |= u32::from(x ^ y);
    }
    differ == 0
}

/// `bytes` with its length hidden from the optimiser. A lookup runs once per disc, and with a name
/// it can see (every game passes a constant) the compiler unrolls the comparison for it, or, when
/// every call passes names of one length, specialises the function for that length.
#[inline(always)]
fn hidden(bytes: &[u8]) -> &[u8] {
    #[cfg(target_arch = "mips")]
    {
        let mut len = bytes.len();
        // SAFETY: an empty assembly block that returns its operand: the same length comes back,
        // only the optimiser can no longer see it. The pointer and the bytes are the caller's.
        unsafe {
            core::arch::asm!("/* {0} */", inout(reg) len, options(pure, nomem, nostack, preserves_flags));
            core::slice::from_raw_parts(bytes.as_ptr(), len)
        }
    }
    #[cfg(not(target_arch = "mips"))]
    bytes
}

/// `(lba, bytes)` of the directory record at `at` (also the root record inside the descriptor).
#[inline(always)]
fn extent(s: &[u8; BYTES], at: usize) -> Extent {
    (le32(s, at + 2), le32(s, at + 10))
}

#[inline(always)]
fn le32(s: &[u8; BYTES], at: usize) -> u32 {
    psx_math::le::read_u32(s, at)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;
    use std::{format, vec};

    /// A disc image under construction: sectors on demand, directories filled record by record.
    struct Image(Vec<[u8; BYTES]>);

    impl Image {
        fn new() -> Image {
            Image(vec![[0; BYTES]; 64])
        }

        /// A directory record at `at` of sector `lba`; returns where the next one goes.
        fn record(&mut self, lba: u32, at: usize, name: &[u8], file: Extent) -> usize {
            let len = 33 + name.len() + (name.len() + 1) % 2;
            let s = &mut self.0[lba as usize];
            assert!(at + len <= BYTES, "a record never crosses a sector");
            s[at] = len as u8;
            s[at + 2..at + 6].copy_from_slice(&file.0.to_le_bytes());
            s[at + 10..at + 14].copy_from_slice(&file.1.to_le_bytes());
            s[at + 32] = name.len() as u8;
            s[at + 33..at + 33 + name.len()].copy_from_slice(name);
            at + len
        }

        /// The volume descriptor, naming the root directory.
        fn volume(&mut self, root: Extent) {
            let s = &mut self.0[PVD_LBA as usize];
            s[0] = 1;
            s[1..6].copy_from_slice(b"CD001");
            s[156] = 34;
            s[158..162].copy_from_slice(&root.0.to_le_bytes());
            s[166..170].copy_from_slice(&root.1.to_le_bytes());
        }

        /// The end of the records already in sector `lba`.
        fn end(&self, lba: u32) -> usize {
            let s = &self.0[lba as usize];
            let mut at = 0;
            while s[at] != 0 {
                at += usize::from(s[at]);
            }
            at
        }

        /// A read of this image that logs the sectors it was asked for.
        fn reader<'a>(
            &'a self,
            log: &'a mut Vec<u32>,
        ) -> impl FnMut(u32, &mut Sector) -> bool + 'a {
            move |lba, into| {
                log.push(lba);
                let Some(bytes) = self.0.get(lba as usize) else {
                    return false;
                };
                for (word, chunk) in into.iter_mut().zip(bytes.chunks(4)) {
                    *word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                }
                true
            }
        }
    }

    /// Root at sector 20: SYSTEM.CNF, MUSIC.XA, NOVERSION.BIN and the directory GAME (sector 21),
    /// which holds A.BIN and the directory DATA (sector 22), which holds DEEP.DAT.
    fn standard() -> Image {
        let mut im = Image::new();
        im.volume((20, 2048));
        let at = im.record(20, 0, b"\0", (20, 2048));
        let at = im.record(20, at, b"\x01", (20, 2048));
        let at = im.record(20, at, b"SYSTEM.CNF;1", (30, 60));
        let at = im.record(20, at, b"MUSIC.XA;1", (40, 4_096_000));
        let at = im.record(20, at, b"GAME", (21, 2048));
        im.record(20, at, b"NOVERSION.BIN", (50, 10));
        let at = im.record(21, 0, b"\0", (21, 2048));
        let at = im.record(21, at, b"\x01", (20, 2048));
        let at = im.record(21, at, b"DATA", (22, 2048));
        im.record(21, at, b"A.BIN;1", (60, 7000));
        let at = im.record(22, 0, b"\0", (22, 2048));
        im.record(22, at, b"DEEP.DAT;1", (70, 9));
        im
    }

    fn lookup(im: &Image, path: &str) -> Option<Extent> {
        let names: Vec<&str> = path.split(['/', '\\']).filter(|n| !n.is_empty()).collect();
        let mut sector = [0; SECTOR_WORDS];
        let mut log = Vec::new();
        find(&mut sector, im.reader(&mut log), None, &names)
    }

    #[test]
    fn read_root_sector_leaves_the_first_root_sector_for_the_callers_own_walk() {
        let im = standard();
        let mut sector = [0; SECTOR_WORDS];
        let mut log = Vec::new();
        assert!(read_root_sector(&mut sector, im.reader(&mut log)));
        assert_eq!(log, [PVD_LBA, 20]);
        assert_eq!(
            find_in_directory(as_bytes(&sector), "MUSIC.XA"),
            Some((40, 4_096_000))
        );
        let blank = Image::new();
        assert!(!read_root_sector(
            &mut sector,
            blank.reader(&mut Vec::new())
        ));
    }

    #[test]
    fn find_in_root_looks_every_name_up_in_one_pass() {
        let im = standard();
        let mut sector = [0; SECTOR_WORDS];
        let mut log = Vec::new();
        let mut out = [Some((1, 1)); 4];
        let names = ["MUSIC.XA", "SYSTEM.CNF", "MISSING.BIN", "GAME"];
        assert!(find_in_root(
            &mut sector,
            im.reader(&mut log),
            &names,
            &mut out
        ));
        assert_eq!(
            out,
            [
                Some((40, 4_096_000)),
                Some((30, 60)),
                None,
                Some((21, 2048))
            ]
        );
        assert_eq!(log, [PVD_LBA, 20], "the descriptor and the one root sector");
    }

    #[test]
    fn find_in_root_agrees_with_find_for_each_name() {
        let im = standard();
        for name in [
            "MUSIC.XA",
            "SYSTEM.CNF",
            "NOVERSION.BIN",
            "GAME",
            "A.BIN",
            "NOPE",
        ] {
            let mut sector = [0; SECTOR_WORDS];
            let mut log = Vec::new();
            let mut out = [None];
            assert!(find_in_root(
                &mut sector,
                im.reader(&mut log),
                &[name],
                &mut out
            ));
            assert_eq!(out[0], lookup(&im, name), "{name}");
        }
    }

    #[test]
    fn find_in_root_fails_on_a_bad_descriptor_or_a_failed_read() {
        let im = standard();
        let mut sector = [0; SECTOR_WORDS];
        let mut out = [Some((9, 9))];
        assert!(!find_in_root(
            &mut sector,
            |_, _| false,
            &["MUSIC.XA"],
            &mut out
        ));
        assert_eq!(out, [None]);
        let blank = Image::new();
        let mut log = Vec::new();
        assert!(!find_in_root(
            &mut sector,
            blank.reader(&mut log),
            &["MUSIC.XA"],
            &mut out
        ));
        let mut calls = 0;
        let flaky = |lba: u32, into: &mut Sector| {
            calls += 1;
            calls == 1 && im.reader(&mut Vec::new())(lba, into)
        };
        assert!(!find_in_root(&mut sector, flaky, &["MUSIC.XA"], &mut out));
    }

    #[test]
    fn a_file_in_the_root() {
        let im = standard();
        assert_eq!(lookup(&im, "MUSIC.XA"), Some((40, 4_096_000)));
        assert_eq!(lookup(&im, "SYSTEM.CNF"), Some((30, 60)));
        assert_eq!(lookup(&im, "NOVERSION.BIN"), Some((50, 10)));
    }

    #[test]
    fn names_are_upper_case_and_the_version_on_the_disc_is_ignored() {
        let im = standard();
        assert_eq!(lookup(&im, "MUSIC.XA"), lookup(&im, "/MUSIC.XA"));
        assert_eq!(lookup(&im, "\\MUSIC.XA"), Some((40, 4_096_000)));
        assert_eq!(
            lookup(&im, "music.xa"),
            None,
            "ISO 9660 names are upper case"
        );
    }

    #[test]
    fn a_file_in_a_subdirectory() {
        let im = standard();
        assert_eq!(lookup(&im, "GAME/A.BIN"), Some((60, 7000)));
        assert_eq!(lookup(&im, "GAME\\A.BIN"), Some((60, 7000)));
        assert_eq!(lookup(&im, "//GAME//DATA/DEEP.DAT"), Some((70, 9)));
        assert_eq!(
            lookup(&im, "GAME/DEEP.DAT"),
            None,
            "DEEP.DAT is one directory deeper"
        );
        assert_eq!(lookup(&im, "GAME/DATA/A.BIN"), None);
        assert_eq!(lookup(&im, "A.BIN"), None, "not in the root");
        assert_eq!(
            lookup(&im, "GAME"),
            Some((21, 2048)),
            "a directory has an extent too"
        );
    }

    #[test]
    fn names_that_only_share_a_prefix_do_not_match() {
        let mut im = standard();
        let at = im.end(20);
        let at = im.record(20, at, b"MUSIC.XAB;1", (1, 1));
        im.record(20, at, b"MUS;1", (2, 2));
        for name in ["MUSIC.X", "MUSIC.XAB2", "MUSIC", "MUS.", "MUSIC.XA2"] {
            assert_eq!(lookup(&im, name), None, "{name:?}");
        }
        assert_eq!(
            lookup(&im, ""),
            Some((20, 2048)),
            "no name: the root directory itself"
        );
        assert_eq!(lookup(&im, "MUSIC.XAB"), Some((1, 1)));
        assert_eq!(lookup(&im, "MUS"), Some((2, 2)));
        assert_eq!(
            lookup(&im, "MUSIC.XA"),
            Some((40, 4_096_000)),
            "the earlier record wins"
        );
    }

    #[test]
    fn missing_files_and_bad_volumes() {
        let im = standard();
        assert_eq!(lookup(&im, "NOPE.BIN"), None);
        assert_eq!(lookup(&im, "GAME/NOPE"), None);
        let mut bad = standard();
        bad.0[PVD_LBA as usize][3] ^= 1;
        assert_eq!(lookup(&bad, "MUSIC.XA"), None);
        assert_eq!(lookup(&Image::new(), "MUSIC.XA"), None);
        // A root that lies past the end of the image: the read fails, the lookup says none.
        let mut far = standard();
        far.volume((1000, 2048));
        assert_eq!(lookup(&far, "MUSIC.XA"), None);
    }

    #[test]
    fn a_directory_of_several_sectors_is_searched_in_all_of_them() {
        let mut im = Image::new();
        im.volume((20, 3 * 2048));
        for (n, lba) in [20u32, 21, 22].into_iter().enumerate() {
            let mut at = 0;
            for k in 0..40 {
                let name = format!("F{n}{k:02}.BIN;1");
                at = im.record(
                    lba,
                    at,
                    name.as_bytes(),
                    (100 + 40 * n as u32 + k, 2048 * k),
                );
            }
        }
        assert_eq!(lookup(&im, "F000.BIN"), Some((100, 0)));
        assert_eq!(lookup(&im, "F139.BIN"), Some((100 + 40 + 39, 2048 * 39)));
        assert_eq!(lookup(&im, "F239.BIN"), Some((100 + 80 + 39, 2048 * 39)));
        assert_eq!(lookup(&im, "F300.BIN"), None);
        // Zero padding ends a sector's records; the next sector is still searched.
        assert_eq!(lookup(&im, "F200.BIN"), Some((180, 0)));
    }

    #[test]
    fn a_directory_size_that_is_not_a_whole_number_of_sectors_still_reads_the_last_one() {
        let mut im = Image::new();
        im.volume((20, 2049));
        im.record(21, 0, b"LATE.BIN;1", (9, 9));
        assert_eq!(lookup(&im, "LATE.BIN"), Some((9, 9)));
        im.volume((20, 2048));
        assert_eq!(lookup(&im, "LATE.BIN"), None, "the size says one sector");
    }

    #[test]
    fn a_walk_from_a_known_directory_skips_the_volume_descriptor() {
        let im = standard();
        let mut sector = [0; SECTOR_WORDS];
        let mut log = Vec::new();
        let root = find(&mut sector, im.reader(&mut log), None, &[]);
        assert_eq!(root, Some((20, 2048)));
        assert_eq!(log, [16]);
        log.clear();
        let found = find(&mut sector, im.reader(&mut log), root, &["MUSIC.XA"]);
        assert_eq!(found, Some((40, 4_096_000)));
        assert_eq!(log, [20]);
        log.clear();
        assert_eq!(find(&mut sector, im.reader(&mut log), root, &[]), root);
        assert!(log.is_empty());
        let found = find(
            &mut sector,
            im.reader(&mut log),
            root,
            &["GAME", "DATA", "DEEP.DAT"],
        );
        assert_eq!(found, Some((70, 9)));
        assert_eq!(log, [20, 21, 22]);
    }

    #[test]
    fn the_primitives_read_one_sector() {
        let im = standard();
        assert_eq!(root_directory(&im.0[PVD_LBA as usize]), Some((20, 2048)));
        assert_eq!(root_directory(&im.0[20]), None);
        assert_eq!(find_in_directory(&im.0[20], "SYSTEM.CNF"), Some((30, 60)));
        assert_eq!(find_in_directory(&im.0[20], "DEEP.DAT"), None);
        assert_eq!(find_in_directory(&im.0[22], "DEEP.DAT"), Some((70, 9)));
        // The name may be bytes: a program that splits a path by hand never makes a `str`.
        assert_eq!(
            find_in_directory(&im.0[20], &b"SYSTEM.CNF"[..]),
            Some((30, 60))
        );
        assert_eq!(find_in_directory(&im.0[20], &b"SYSTEM.CN"[..]), None);
        assert_eq!(find_in_directory(&im.0[20], &b"\xff\x00"[..]), None);
        assert_eq!(sectors(0), 0);
        assert_eq!(sectors(1), 1);
        assert_eq!(sectors(2048), 1);
        assert_eq!(sectors(2049), 2);
    }

    #[test]
    fn a_record_that_ends_the_sector_is_found_and_damaged_records_never_read_out_of_bounds() {
        // A record with an odd-length name that ends exactly at the end of the sector.
        let mut im = Image::new();
        im.volume((20, 2048));
        let mut at = 0;
        for _ in 0..41 {
            at = im.record(20, at, b"FILLER.BIN;1", (1, 1));
        }
        let last = "L".repeat(BYTES - at - 33);
        assert_eq!(im.record(20, at, last.as_bytes(), (5, 5)), BYTES);
        assert_eq!(lookup(&im, &last), Some((5, 5)));
        // Damaged: a record length that runs on, a name length past the sector.
        let mut dmg = standard();
        dmg.0[20][0x1f0] = 0xff;
        dmg.0[20][0x1f0 + 32] = 0xff;
        let _ = lookup(&dmg, "ZZZ");
        let _ = lookup(&dmg, "NOVERSION.BIN");
        dmg.0[20][0] = 0xff;
        dmg.0[20][32] = 0xff;
        assert_eq!(lookup(&dmg, "MUSIC.XA"), None);
        // A name of the full 255 bytes asked for: never found, never out of bounds.
        let long = "A".repeat(255);
        assert_eq!(lookup(&standard(), &long), None);
    }

    /// Volume descriptors and root directories of two real discs.
    const REAL: [(&[u8; BYTES], &[u8; BYTES]); 2] = [
        (
            include_bytes!("../tests/fixtures/present-cd-pvd.bin"),
            include_bytes!("../tests/fixtures/present-cd-root.bin"),
        ),
        (
            include_bytes!("../tests/fixtures/flipcap-pvd.bin"),
            include_bytes!("../tests/fixtures/flipcap-root.bin"),
        ),
    ];

    #[test]
    fn real_volume_descriptors_and_root_directories() {
        for (pvd, root) in REAL {
            assert_eq!(root_directory(pvd), Some((20, 2048)));
            for name in ["SYSTEM.CNF", "PSX.EXE", "CDTEST.BIN"] {
                let found = find_in_directory(root, name);
                assert_eq!(
                    found.is_some(),
                    name != "CDTEST.BIN" || root[..].windows(10).any(|w| w == name.as_bytes())
                );
            }
            for name in [
                "PSX",
                "PSX.EX",
                "PSX.EXEE",
                "SYSTEM.CN",
                "SYSTEM.CNFX",
                "MUSIC.XA",
                "",
                ".",
            ] {
                assert_eq!(find_in_directory(root, name), None, "{name:?}");
            }
            // A damaged descriptor is refused.
            for at in 0..6 {
                let mut bad = *pvd;
                bad[at] ^= 1;
                assert_eq!(root_directory(&bad), None);
            }
        }
        assert!(find_in_directory(REAL[0].1, "PSX.EXE").is_some());
        assert!(find_in_directory(REAL[0].1, "SYSTEM.CNF").is_some());
    }
}
