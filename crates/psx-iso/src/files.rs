//! Reading files and listing the files of a disc's ISO 9660 tree.
//!
//! The directory walk behind boot discovery ([`crate::load_boot_exe_from_disc`]) made public,
//! so a host tool that needs a file's bytes or a disc's inventory (a cooker reading the
//! original game's data, an audit of what a disc holds) reads it through the one reader the
//! emulator boots from instead of carrying its own.
//!
//! Paths are case-insensitive and may use `/` or `\` and a `cdrom:` prefix; a file's `;1`
//! version suffix is optional. Everything reads through [`Disc::read_sector_user`], so it works
//! on any image the disc loader accepts (a BIN, a CUE with several tracks). A file in a CD-XA
//! form 2 sector is read as its 2048 user bytes; use [`Disc::find_file`] and
//! [`Disc::read_sector_raw`] for the raw 2352-byte (or 2336-byte) records of an interleaved file.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::boot::{lookup_path, read_directory, read_extent, root_directory, DirEntry};
use crate::{BootError, Disc};

/// The deepest directory nesting [`Disc::list_files`] follows before it reports a malformed disc.
const MAX_DEPTH: usize = 16;

/// A file on a disc: where its first sector is and how many bytes it holds.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscFile {
    /// Path from the root without a leading slash, `;version` removed, as the disc spells it
    /// (`WIPEOUT/TRACK02/TRACK.TRV`).
    pub path: String,
    /// First sector of the file's extent.
    pub lba: u32,
    /// Size in bytes.
    pub size: u32,
}

/// Why a disc file could not be found, read or listed.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileError {
    /// The volume descriptor, a directory or a path component was not usable; the inner error
    /// says which.
    Disc(BootError),
    /// The path names a directory where a file was wanted.
    IsDirectory(String),
    /// A sector of the file's extent is not on the image.
    Unreadable {
        /// First sector of the file.
        lba: u32,
        /// Size in bytes.
        size: u32,
    },
    /// A directory lists an ancestor (or itself) as a child, so a listing would not end.
    DirectoryCycle(String),
    /// Directories nest deeper than a real disc does.
    TooDeep(String),
}

impl From<BootError> for FileError {
    fn from(value: BootError) -> Self {
        Self::Disc(value)
    }
}

impl Disc {
    /// Where `path` is on the disc, without reading its data.
    ///
    /// # Errors
    ///
    /// [`FileError::Disc`] with [`BootError::FileNotFound`] when a component is missing,
    /// [`FileError::IsDirectory`] when `path` is a directory.
    pub fn find_file(&self, path: &str) -> Result<DiscFile, FileError> {
        let root = root_directory(self)?;
        let entry = lookup_path(self, &root, path)?;
        if entry.is_dir() {
            return Err(FileError::IsDirectory(path.to_string()));
        }
        Ok(DiscFile {
            path: path.trim_start_matches(['\\', '/']).to_string(),
            lba: entry.extent_lba,
            size: entry.size,
        })
    }

    /// The bytes of the file at `path` (`size` bytes from its first sector).
    ///
    /// # Errors
    ///
    /// As [`find_file`](Self::find_file), and [`FileError::Unreadable`] when the image ends
    /// inside the file.
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FileError> {
        let file = self.find_file(path)?;
        read_extent(self, file.lba, file.size).map_err(|_| FileError::Unreadable {
            lba: file.lba,
            size: file.size,
        })
    }

    /// Every file on the disc, in directory order, each directory's contents where the directory
    /// is listed (depth first). Directories themselves are not listed.
    ///
    /// # Errors
    ///
    /// [`FileError::Disc`] for a malformed directory record or an unreadable directory,
    /// [`FileError::DirectoryCycle`] or [`FileError::TooDeep`] for a tree that does not end.
    pub fn list_files(&self) -> Result<Vec<DiscFile>, FileError> {
        let root = root_directory(self)?;
        let mut found = Vec::new();
        let mut visiting = BTreeSet::new();
        walk(self, &root, "", 0, &mut visiting, &mut found)?;
        Ok(found)
    }
}

fn walk(
    disc: &Disc,
    directory: &DirEntry,
    prefix: &str,
    depth: usize,
    visiting: &mut BTreeSet<u32>,
    found: &mut Vec<DiscFile>,
) -> Result<(), FileError> {
    if depth > MAX_DEPTH {
        return Err(FileError::TooDeep(prefix.to_string()));
    }
    if !visiting.insert(directory.extent_lba) {
        return Err(FileError::DirectoryCycle(prefix.to_string()));
    }
    for entry in read_directory(disc, directory)? {
        let name = entry.identifier.split(';').next().unwrap_or_default();
        let mut path = String::from(prefix);
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(name);
        if entry.is_dir() {
            walk(disc, &entry, &path, depth + 1, visiting, found)?;
        } else {
            found.push(DiscFile {
                path,
                lba: entry.extent_lba,
                size: entry.size,
            });
        }
    }
    visiting.remove(&directory.extent_lba);
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{IsoBuilder, SECTOR_BYTES};

    /// An ISO 9660 directory record.
    fn record(lba: u32, size: u32, dir: bool, name: &[u8]) -> Vec<u8> {
        let mut len = 33 + name.len();
        len += len % 2;
        let mut r = vec![0u8; len];
        r[0] = len as u8;
        r[2..6].copy_from_slice(&lba.to_le_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    fn directory(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut sector = Vec::new();
        for e in entries {
            sector.extend_from_slice(e);
        }
        sector.resize(2048, 0);
        sector
    }

    /// A disc image from 2048-byte sectors, each wrapped as a raw mode 1 sector.
    fn disc(sectors: &[Vec<u8>]) -> Disc {
        let mut image = Vec::new();
        for (lba, user) in sectors.iter().enumerate() {
            let mut raw = vec![0u8; SECTOR_BYTES];
            raw[1..11].fill(0xff);
            // MSF of the sector with the 150-sector lead-in, BCD; only the first matters.
            let frame = lba as u32 + 150;
            let bcd = |v: u32| (((v / 10) << 4) | (v % 10)) as u8;
            raw[12] = bcd(frame / 75 / 60);
            raw[13] = bcd(frame / 75 % 60);
            raw[14] = bcd(frame % 75);
            raw[15] = 1;
            raw[16..16 + 2048].copy_from_slice(user);
            image.extend_from_slice(&raw);
        }
        Disc::from_bin(image)
    }

    fn pvd(root_lba: u32) -> Vec<u8> {
        let mut v = vec![0u8; 2048];
        v[0] = 1;
        v[1..6].copy_from_slice(b"CD001");
        v[6] = 1;
        v[156..156 + 34].copy_from_slice(&record(root_lba, 2048, true, &[0])[..34]);
        v
    }

    /// Sectors 0..=15 empty, 16 the volume descriptor, 17 root, 18 `GAME`, 19 `GAME/DATA`,
    /// 20 `A.TXT`, 21 and 22 `GAME/DATA/B.BIN`, 23 `GAME/C.DAT`.
    fn tree() -> Disc {
        let mut sectors = vec![vec![0u8; 2048]; 16];
        sectors.push(pvd(17));
        sectors.push(directory(&[
            record(17, 2048, true, &[0]),
            record(17, 2048, true, &[1]),
            record(18, 2048, true, b"GAME"),
            record(20, 5, false, b"A.TXT;1"),
        ]));
        sectors.push(directory(&[
            record(18, 2048, true, &[0]),
            record(17, 2048, true, &[1]),
            record(19, 2048, true, b"DATA"),
            record(23, 3, false, b"C.DAT;1"),
        ]));
        sectors.push(directory(&[
            record(19, 2048, true, &[0]),
            record(18, 2048, true, &[1]),
            record(21, 2048 + 7, false, b"B.BIN;1"),
        ]));
        let mut a = vec![0u8; 2048];
        a[..5].copy_from_slice(b"hello");
        sectors.push(a);
        let mut b1 = vec![0xb1u8; 2048];
        b1[0] = 1;
        sectors.push(b1);
        sectors.push(vec![0xb2u8; 2048]);
        let mut c = vec![0u8; 2048];
        c[..3].copy_from_slice(b"c!c");
        sectors.push(c);
        disc(&sectors)
    }

    #[test]
    fn lists_every_file_depth_first_in_directory_order() {
        let files = tree().list_files().unwrap();
        let listing: Vec<(&str, u32, u32)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.lba, f.size))
            .collect();
        assert_eq!(
            listing,
            vec![
                ("GAME/DATA/B.BIN", 21, 2055),
                ("GAME/C.DAT", 23, 3),
                ("A.TXT", 20, 5)
            ]
        );
    }

    #[test]
    fn reads_a_file_by_path_in_any_case_with_either_slash() {
        let d = tree();
        assert_eq!(d.read_file("A.TXT").unwrap(), b"hello");
        assert_eq!(d.read_file("a.txt;1").unwrap(), b"hello");
        assert_eq!(d.read_file("cdrom:\\game\\c.dat").unwrap(), b"c!c");
        let b = d.read_file("/GAME/DATA/B.BIN").unwrap();
        assert_eq!(b.len(), 2055);
        assert_eq!(b[0], 1);
        assert_eq!(b[1], 0xb1);
        assert_eq!(b[2048..], [0xb2; 7]);
    }

    #[test]
    fn find_file_reports_the_extent_without_reading_it() {
        let f = tree().find_file("game/data/b.bin").unwrap();
        assert_eq!((f.lba, f.size), (21, 2055));
    }

    #[test]
    fn missing_paths_and_directories_are_errors() {
        let d = tree();
        assert_eq!(
            d.read_file("GAME/NOPE.BIN"),
            Err(FileError::Disc(BootError::FileNotFound(
                "GAME/NOPE.BIN".into()
            )))
        );
        assert_eq!(
            d.read_file("GAME/DATA"),
            Err(FileError::IsDirectory("GAME/DATA".into()))
        );
        assert!(matches!(
            d.read_file("A.TXT/X"),
            Err(FileError::Disc(BootError::NotDirectory(_)))
        ));
    }

    #[test]
    fn a_file_that_runs_past_the_image_is_unreadable() {
        let mut sectors = vec![vec![0u8; 2048]; 16];
        sectors.push(pvd(17));
        sectors.push(directory(&[record(19, 3 * 2048, false, b"BIG.BIN;1")]));
        sectors.push(vec![0u8; 2048]);
        sectors.push(vec![7u8; 2048]);
        let d = disc(&sectors);
        assert_eq!(
            d.read_file("BIG.BIN"),
            Err(FileError::Unreadable {
                lba: 19,
                size: 3 * 2048
            })
        );
        // The listing still reports it: it is on the directory.
        assert_eq!(d.list_files().unwrap().len(), 1);
    }

    #[test]
    fn a_directory_that_lists_its_ancestor_ends_in_an_error() {
        let mut sectors = vec![vec![0u8; 2048]; 16];
        sectors.push(pvd(17));
        sectors.push(directory(&[record(18, 2048, true, b"LOOP")]));
        sectors.push(directory(&[record(17, 2048, true, b"BACK")]));
        let d = disc(&sectors);
        assert_eq!(
            d.list_files(),
            Err(FileError::DirectoryCycle("LOOP/BACK".into()))
        );
    }

    #[test]
    fn a_malformed_directory_record_is_an_error() {
        let mut sectors = vec![vec![0u8; 2048]; 16];
        sectors.push(pvd(17));
        let mut bad = vec![0u8; 2048];
        bad[0] = 3;
        sectors.push(bad);
        let d = disc(&sectors);
        assert_eq!(
            d.list_files(),
            Err(FileError::Disc(BootError::BadDirectoryRecord))
        );
    }

    #[test]
    fn an_image_built_by_the_builder_lists_and_reads_back() {
        let mut builder = IsoBuilder::new();
        builder.add_file("SYSTEM.CNF", b"BOOT = cdrom:\\PSX.EXE;1\r\n".to_vec());
        builder.add_file("DATA.BIN", vec![9u8; 5000]);
        let d = Disc::from_bin(builder.build_bin());
        let files = d.list_files().unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["SYSTEM.CNF", "DATA.BIN"]);
        assert_eq!(d.read_file("data.bin").unwrap(), vec![9u8; 5000]);
    }
}
