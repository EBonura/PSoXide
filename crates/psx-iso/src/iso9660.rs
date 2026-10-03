//! Minimal ISO 9660 writer -- enough to produce a bootable PS1 disc
//! image from a PSX-EXE + a `SYSTEM.CNF`.
//!
//! This is **not** a full ISO 9660 implementation. Features deliberately
//! omitted:
//!
//! - Multi-level directory trees (everything lives in root).
//! - Joliet / Rock Ridge extensions (ASCII-only file names, 8.3).
//! - Multi-session / multi-track discs.
//! - Proper per-file metadata (creation / modification dates are fixed).
//! - Supplementary volume descriptors.
//!
//! What it **does** produce:
//!
//! - A 2048-byte-per-sector "cooked" image (`.iso`). Every emulator
//!   will boot this directly. Real PlayStation hardware requires a
//!   raw 2352-byte `.bin` with CD-ROM EDC/ECC -- conversion is a
//!   post-step (tools like `cdrdao` or `bchunk` do it). This crate
//!   focuses on the filesystem layout; upgrading to raw-sector output
//!   is purely a sector-encoding change in [`IsoBuilder::build`].
//!
//! Sector map produced:
//!
//! ```text
//!   0..=15    system area, zero-filled unless supplied by caller
//!   16        Primary Volume Descriptor (PVD)
//!   17        Volume Descriptor Set Terminator (VDST)
//!   18        L-path table (little-endian LBA fields)
//!   19        M-path table (big-endian LBA fields)
//!   20        Root directory extent
//!   21..      File data, one per file, each aligned to a sector
//! ```
//!
//! Reference: ECMA-119 (the published ISO 9660 standard), plus the
//! PSX-specific bits from PSX-SPX "CD-ROM File System".

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// One cooked sector = 2048 bytes of user data.
pub const SECTOR_SIZE: usize = 2048;

/// One raw CD-ROM sector = 2352 bytes:
///
/// - 12 sync bytes
/// - 4 header bytes
/// - 8 subheader bytes
/// - 2048 user-data bytes
/// - 280 EDC/ECC bytes
///
/// PSoXide's emulator CDROM path expects raw sectors; external
/// emulators accept either cooked .iso or raw .bin.
pub const RAW_SECTOR_SIZE: usize = 2352;

/// Fixed ISO 9660 date used for every volume / file timestamp. Games
/// don't read these, so any valid date works -- we use 2026-01-01.
const FIXED_VOLUME_DATE: &[u8; 17] = b"2026010100000000\x00";
/// 7-byte directory record date: year-since-1900, month, day, hour,
/// minute, second, GMT offset in 15-minute units. 126 = 2026.
const FIXED_DIR_DATE: [u8; 7] = [126, 1, 1, 0, 0, 0, 0];

/// A single file to embed. Filenames are normalised to 8.3 upper-case
/// with a `;1` version suffix during serialisation.
pub struct IsoFile {
    /// User-supplied filename. Normalised at build time.
    pub name: String,
    /// Raw bytes to store.
    pub content: Vec<u8>,
}

/// Raw CD-XA sector size without sync and header: 8-byte subheader plus
/// 2328 bytes (Form 1 data + EDC/ECC, or Form 2 data + EDC).
pub const XA_SECTOR_SIZE: usize = 2336;

enum IsoEntry {
    File(IsoFile),
    /// A file whose sectors carry their own subheaders (interleaved STR /
    /// XA audio). `file.content` holds the first 2048 bytes of each
    /// sector's data for the cooked image; `raw` is the 2336-byte form
    /// `build_bin` writes back.
    Xa {
        file: IsoFile,
        raw: Vec<u8>,
    },
    Padding {
        sectors: u32,
    },
}

impl IsoEntry {
    fn file(&self) -> Option<&IsoFile> {
        match self {
            IsoEntry::File(file) | IsoEntry::Xa { file, .. } => Some(file),
            IsoEntry::Padding { .. } => None,
        }
    }
}

/// Build a cooked ISO 9660 image from a set of root-directory files.
pub struct IsoBuilder {
    volume_id: String,
    system_id: String,
    system_area: Option<Vec<u8>>,
    entries: Vec<IsoEntry>,
}

impl Default for IsoBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IsoBuilder {
    /// New builder with empty file list and sensible PS1 defaults
    /// (`system_id = "PLAYSTATION"`, `volume_id = "UNTITLED"`).
    pub fn new() -> Self {
        Self {
            volume_id: String::from("UNTITLED"),
            system_id: String::from("PLAYSTATION"),
            system_area: None,
            entries: Vec::new(),
        }
    }

    /// Override the volume identifier. Will be upper-cased and
    /// truncated to 32 bytes (ISO 9660 d-characters).
    pub fn volume_id(mut self, id: &str) -> Self {
        self.volume_id = id.to_ascii_uppercase();
        self
    }

    /// Override the system identifier (written to PVD bytes 8..39).
    pub fn system_id(mut self, id: &str) -> Self {
        self.system_id = id.to_ascii_uppercase();
        self
    }

    /// Override the first 16 cooked ISO sectors. Real PlayStation
    /// boot media uses this area for the region/license/logo payload.
    /// The data must be exactly 16 * 2048 bytes.
    pub fn system_area(mut self, bytes: Vec<u8>) -> Result<Self, Vec<u8>> {
        if bytes.len() != 16 * SECTOR_SIZE {
            return Err(bytes);
        }
        self.system_area = Some(bytes);
        Ok(self)
    }

    /// Add a file to the root directory. Name is normalised later.
    pub fn add_file(&mut self, name: &str, content: Vec<u8>) -> &mut Self {
        self.entries.push(IsoEntry::File(IsoFile {
            name: name.to_ascii_uppercase(),
            content,
        }));
        self
    }

    /// Add a CD-XA file from raw 2336-byte sectors (subheader + 2328
    /// bytes), such as psxavenc's `-t str` / `-t xa` output. The raw
    /// build writes each sector's own subheader and recomputes EDC/ECC
    /// (Form 1) or EDC (Form 2, submode bit 5) at the file's final LBA,
    /// so interleaved video + XA-ADPCM audio streams play from the disc.
    /// The directory records `2048 * sectors` as the size, as for any
    /// Mode 2 file. Returns `None` if `raw` is not a whole number of
    /// sectors.
    pub fn add_xa_file(&mut self, name: &str, raw: Vec<u8>) -> Option<&mut Self> {
        if raw.is_empty() || !raw.len().is_multiple_of(XA_SECTOR_SIZE) {
            return None;
        }
        let mut content = Vec::with_capacity(raw.len() / XA_SECTOR_SIZE * SECTOR_SIZE);
        for sector in raw.chunks_exact(XA_SECTOR_SIZE) {
            content.extend_from_slice(&sector[8..8 + SECTOR_SIZE]);
        }
        self.entries.push(IsoEntry::Xa {
            file: IsoFile {
                name: name.to_ascii_uppercase(),
                content,
            },
            raw,
        });
        Some(self)
    }

    /// Reserve invisible file-area sectors. These sectors are not listed in the
    /// root directory, but they do consume LBAs, allowing later files to land at
    /// fixed positions without cluttering the ISO file tree.
    pub fn add_padding_sectors(&mut self, sectors: u32) -> &mut Self {
        if sectors != 0 {
            self.entries.push(IsoEntry::Padding { sectors });
        }
        self
    }

    /// Serialise the image. Output length is always a multiple of
    /// [`SECTOR_SIZE`].
    pub fn build(&self) -> Vec<u8> {
        // ---------- Layout plan --------------------------------------
        // Every file takes ceil(size / 2048) sectors starting at a
        // known LBA we can bake into the root directory record.

        // Sector allocation for fixed headers:
        let lba_l_path = 18u32;
        let lba_m_path = 19u32;
        let lba_root_dir = 20u32;
        let mut next_file_lba = 21u32;

        // Compute the root directory contents + pre-assigned LBAs.
        struct FilePlacement {
            entry_name: String, // "FOO.TXT;1"
            lba: u32,
            size: u32,
        }
        let mut placements: Vec<FilePlacement> = Vec::new();
        for entry in &self.entries {
            match entry {
                IsoEntry::File(file) | IsoEntry::Xa { file, .. } => {
                    let entry_name = iso_file_identifier(&file.name);
                    let size = file.content.len() as u32;
                    let sectors = size.div_ceil(SECTOR_SIZE as u32).max(1);
                    placements.push(FilePlacement {
                        entry_name,
                        lba: next_file_lba,
                        size,
                    });
                    next_file_lba += sectors;
                }
                IsoEntry::Padding { sectors } => {
                    next_file_lba += *sectors;
                }
            }
        }
        let total_sectors = next_file_lba;

        // ---------- Root directory extent ----------------------------
        let mut root_extent = Vec::with_capacity(SECTOR_SIZE);
        // "." entry (self).
        root_extent.extend_from_slice(&dir_record(lba_root_dir, SECTOR_SIZE as u32, &[0], true));
        // ".." entry (parent = self for root).
        root_extent.extend_from_slice(&dir_record(lba_root_dir, SECTOR_SIZE as u32, &[1], true));
        // File entries -- stored as ASCII bytes from the placement names.
        for p in &placements {
            root_extent.extend_from_slice(&dir_record(
                p.lba,
                p.size,
                p.entry_name.as_bytes(),
                false,
            ));
        }
        pad_to(&mut root_extent, SECTOR_SIZE);

        // ---------- Path tables (root is the only directory) --------
        let mut l_path = Vec::with_capacity(SECTOR_SIZE);
        encode_path_table_entry(&mut l_path, lba_root_dir, 1, &[0], false);
        pad_to(&mut l_path, SECTOR_SIZE);

        let mut m_path = Vec::with_capacity(SECTOR_SIZE);
        encode_path_table_entry(&mut m_path, lba_root_dir, 1, &[0], true);
        pad_to(&mut m_path, SECTOR_SIZE);
        let path_table_size = 10u32; // 8-byte header + 1 ID + 1 pad

        // ---------- Primary Volume Descriptor -----------------------
        let mut pvd = vec![0u8; SECTOR_SIZE];
        pvd[0] = 0x01; // volume descriptor type = PVD
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 0x01; // version
                       // pvd[7] = 0 (unused)
        fill_ascii(&mut pvd[8..40], &self.system_id);
        fill_ascii(&mut pvd[40..72], &self.volume_id);
        // pvd[72..80] = 0
        write_both_endian_u32(&mut pvd[80..88], total_sectors);
        // pvd[88..120] = 0 (unused)
        write_both_endian_u16(&mut pvd[120..124], 1); // volume set size
        write_both_endian_u16(&mut pvd[124..128], 1); // volume sequence
        write_both_endian_u16(&mut pvd[128..132], SECTOR_SIZE as u16);
        write_both_endian_u32(&mut pvd[132..140], path_table_size);
        write_le_u32(&mut pvd[140..144], lba_l_path);
        // pvd[144..148] = 0 (optional L-path table)
        write_be_u32(&mut pvd[148..152], lba_m_path);
        // pvd[152..156] = 0 (optional M-path table)

        // Root directory record -- 34 bytes embedded at offset 156.
        let root_record = dir_record(lba_root_dir, SECTOR_SIZE as u32, &[0], true);
        pvd[156..156 + 34].copy_from_slice(&root_record);

        // Volume set / publisher / prepare / app / file identifiers --
        // all blank-filled per spec.
        fill_ascii(&mut pvd[190..318], ""); // volume set id (128 bytes)
        fill_ascii(&mut pvd[318..446], ""); // publisher id
        fill_ascii(&mut pvd[446..574], ""); // data preparer id
        fill_ascii(&mut pvd[574..702], "PLAYSTATION"); // application id
        fill_ascii(&mut pvd[702..740], ""); // copyright file id
        fill_ascii(&mut pvd[740..776], ""); // abstract file id
        fill_ascii(&mut pvd[776..813], ""); // bibliographic file id

        pvd[813..813 + 17].copy_from_slice(FIXED_VOLUME_DATE); // creation
        pvd[830..830 + 17].copy_from_slice(FIXED_VOLUME_DATE); // modification
        pvd[847..847 + 17].copy_from_slice(b"0000000000000000\x00"); // expiration
        pvd[864..864 + 17].copy_from_slice(b"0000000000000000\x00"); // effective
        pvd[881] = 0x01; // file structure version

        // ---------- Volume Descriptor Set Terminator ----------------
        let mut vdst = vec![0u8; SECTOR_SIZE];
        vdst[0] = 0xFF;
        vdst[1..6].copy_from_slice(b"CD001");
        vdst[6] = 0x01;

        // ---------- Stitch it all together --------------------------
        let mut out = vec![0u8; (total_sectors as usize) * SECTOR_SIZE];
        if let Some(system_area) = &self.system_area {
            out[..16 * SECTOR_SIZE].copy_from_slice(system_area);
        }
        out[16 * SECTOR_SIZE..17 * SECTOR_SIZE].copy_from_slice(&pvd);
        out[17 * SECTOR_SIZE..18 * SECTOR_SIZE].copy_from_slice(&vdst);
        out[18 * SECTOR_SIZE..19 * SECTOR_SIZE].copy_from_slice(&l_path);
        out[19 * SECTOR_SIZE..20 * SECTOR_SIZE].copy_from_slice(&m_path);
        out[20 * SECTOR_SIZE..21 * SECTOR_SIZE].copy_from_slice(&root_extent);

        let mut placement_index = 0usize;
        for entry in &self.entries {
            let Some(file) = entry.file() else {
                continue;
            };
            let placement = &placements[placement_index];
            placement_index += 1;
            let start = placement.lba as usize * SECTOR_SIZE;
            let end = start + file.content.len();
            out[start..end].copy_from_slice(&file.content);
            // Remainder of the last sector stays zero (already is).
        }

        out
    }

    /// Serialise as a raw .bin image -- 2352-byte Mode-2 Form-1 sectors
    /// with sync pattern, BCD MSF header, subheader, and valid
    /// Mode-2/Form-1 EDC/ECC.
    ///
    /// Use this output for PSoXide's disc loader (`Disc::from_bin`)
    /// and for emulators expecting .bin/.cue.
    pub fn build_bin(&self) -> Vec<u8> {
        let cooked = self.build();
        let total_sectors = cooked.len() / SECTOR_SIZE;
        let mut bin = vec![0u8; total_sectors * RAW_SECTOR_SIZE];
        for lba in 0..total_sectors {
            let cooked_start = lba * SECTOR_SIZE;
            let raw_start = lba * RAW_SECTOR_SIZE;
            let sector = &mut bin[raw_start..raw_start + RAW_SECTOR_SIZE];
            // Sync pattern: 00 FF×10 00.
            sector[0] = 0x00;
            for b in &mut sector[1..=10] {
                *b = 0xFF;
            }
            sector[11] = 0x00;
            // Header: BCD MSF + mode byte. MSF starts at 00:02:00 for
            // LBA 0 (2-second pre-gap). One frame = one LBA.
            let absolute_frame = lba as u32 + 150; // 150 = 00:02:00 in frames
            let minute = (absolute_frame / 75 / 60) as u8;
            let second = ((absolute_frame / 75) % 60) as u8;
            let frame = (absolute_frame % 75) as u8;
            sector[12] = bin_to_bcd(minute);
            sector[13] = bin_to_bcd(second);
            sector[14] = bin_to_bcd(frame);
            sector[15] = 0x02; // Mode 2
                               // Subheader: FILE=0, CHAN=0, SUBMODE=0x08 (data), CI=0
                               // (repeated twice -- Mode 2 requires the sub-header pair).
            sector[16] = 0x00;
            sector[17] = 0x00;
            sector[18] = 0x08;
            sector[19] = 0x00;
            sector[20] = 0x00;
            sector[21] = 0x00;
            sector[22] = 0x08;
            sector[23] = 0x00;
            // User data -- copy the cooked sector's 2048 bytes.
            sector[24..24 + SECTOR_SIZE]
                .copy_from_slice(&cooked[cooked_start..cooked_start + SECTOR_SIZE]);
            encode_mode2_sector(sector);
        }
        // CD-XA files: put back each sector's own subheader and data, then
        // redo EDC/ECC for its form. File LBAs follow build()'s layout.
        let mut lba = 21usize;
        for entry in &self.entries {
            match entry {
                IsoEntry::Padding { sectors } => lba += *sectors as usize,
                IsoEntry::File(file) => {
                    lba += file.content.len().div_ceil(SECTOR_SIZE).max(1);
                }
                IsoEntry::Xa { file, raw } => {
                    for (i, xa) in raw.chunks_exact(XA_SECTOR_SIZE).enumerate() {
                        let start = (lba + i) * RAW_SECTOR_SIZE;
                        let sector = &mut bin[start..start + RAW_SECTOR_SIZE];
                        sector[16..].copy_from_slice(xa);
                        encode_mode2_sector(sector);
                    }
                    lba += file.content.len().div_ceil(SECTOR_SIZE).max(1);
                }
            }
        }
        bin
    }
}

// ---------- Mode 2 sector protection (ECMA-130) -------------------------
//
// ECMA-130 gives a Mode 2 sector two protection layers on top of the
// disc's own CIRC:
//
// * EDC: a 32-bit CRC over the subheader and user data. Form 1 covers
//   0x10..0x818 and stores the CRC at 0x818; Form 2 covers 0x10..0x92C
//   and stores it at 0x92C, the sector's last four bytes.
// * ECC, Form 1 only: a Reed-Solomon product code over GF(2^8). The 2340
//   bytes from the header (0x0C) to the end of the sector are read as
//   1170 16-bit words. The low and high bytes of each word belong to two
//   independent planes that use the same code. P parity protects 43
//   columns of 24 words; Q parity protects 26 diagonals of 43 words that
//   run through the data and the P parity. In Mode 2 the four header
//   bytes count as zero for both, so a sector's ECC doesn't depend on
//   where it sits on the disc.

/// Raw-sector offset of the four header bytes (BCD MSF, mode).
const HEADER_AT: usize = 0x0C;
/// Raw-sector offset of the first subheader copy; EDC coverage starts here.
const SUBHEADER_AT: usize = 0x10;
/// Raw-sector offset of the submode byte in the first subheader copy.
const SUBMODE_AT: usize = 0x12;
/// Submode bit 5: the sector is Form 2 (2324 data bytes, EDC only).
const SUBMODE_FORM2: u8 = 1 << 5;
/// Where the EDC goes in a Form 1 sector (after 2048 data bytes).
const FORM1_EDC_AT: usize = 0x818;
/// Where the EDC goes in a Form 2 sector (after 2324 data bytes).
const FORM2_EDC_AT: usize = 0x92C;

/// The EDC generator, as ECMA-130 writes it:
/// (x^16 + x^15 + x^2 + 1) * (x^16 + x^2 + x + 1). Each factor is
/// written here with bit n standing for x^n.
const EDC_GENERATOR: u64 = carryless_mul(0x1_8005, 0x1_0007);

/// The generator's low 32 coefficients, bit-reversed, for a CRC that
/// takes each byte least significant bit first.
const EDC_POLY_LSB_FIRST: u32 = (EDC_GENERATOR as u32).reverse_bits();
const _: () = assert!(EDC_POLY_LSB_FIRST == 0xD801_8001);

/// CRC of every byte value, for the byte-at-a-time EDC loop.
const EDC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < 256 {
        let mut crc = byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ EDC_POLY_LSB_FIRST
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};

/// Polynomial product over GF(2): `a * b` with XOR instead of add.
const fn carryless_mul(a: u64, b: u64) -> u64 {
    let mut product = 0;
    let mut bit = 0;
    while bit < 32 {
        if b & (1 << bit) != 0 {
            product ^= a << bit;
        }
        bit += 1;
    }
    product
}

/// ECMA-130 EDC of `bytes`: the CRC above, starting from zero, with no
/// final inversion.
fn edc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |crc, &byte| {
        (crc >> 8) ^ EDC_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize]
    })
}

/// GF(2^8) field polynomial x^8 + x^4 + x^3 + x^2 + 1, without its x^8
/// term: what a doubling that overflows bit 7 folds back in.
const GF_REDUCE: u8 = 0x1D;

/// Multiply a field element by alpha (x), the field's primitive element.
const fn gf_times_alpha(a: u8) -> u8 {
    if a & 0x80 != 0 {
        (a << 1) ^ GF_REDUCE
    } else {
        a << 1
    }
}

/// General GF(2^8) product, by shift and add.
const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 != 0 {
            product ^= a;
        }
        a = gf_times_alpha(a);
        b >>= 1;
    }
    product
}

/// The inverse of (alpha + 1), found by search. Solving the two parity
/// equations below divides by it.
const INV_ALPHA_PLUS_ONE: u8 = {
    let mut candidate = 1u8;
    while gf_mul(candidate, gf_times_alpha(1) ^ 1) != 1 {
        candidate += 1;
    }
    candidate
};

/// One of the two Reed-Solomon codes, described by where its symbols sit.
///
/// Vector `v` takes `len` data words; data word `i` of it is word
/// `(v * vector_step + i * symbol_step) % wrap` of the 1170-word area.
/// Its two parity words go to `parity_at + v` and
/// `parity_at + vectors + v`.
struct ParityCode {
    vectors: usize,
    len: usize,
    vector_step: usize,
    symbol_step: usize,
    wrap: usize,
    parity_at: usize,
}

/// P parity: 43 columns down 24 rows of 43 words (header,
/// subheader, user data and EDC), parity words in rows 24 and 25.
const P_CODE: ParityCode = ParityCode {
    vectors: 43,
    len: 24,
    vector_step: 1,
    symbol_step: 43,
    wrap: 43 * 24,
    parity_at: 43 * 24,
};

/// Q parity: 26 diagonals of 43 words over the data and P parity
/// (26 rows of 43 words), each diagonal stepping one row down and one
/// column right, parity words after the P parity.
const Q_CODE: ParityCode = ParityCode {
    vectors: 26,
    len: 43,
    vector_step: 43,
    symbol_step: 44,
    wrap: 43 * 26,
    parity_at: 43 * 26,
};

impl ParityCode {
    /// Compute and store both parity words of every vector, for both
    /// byte planes, in the 2340-byte area that starts at the header.
    ///
    /// For a codeword `c[0..n]` (the `n - 2` data symbols, then the two
    /// parity symbols) ECMA-130 requires
    /// `sum c[k] = 0` and `sum alpha^(n-1-k) * c[k] = 0`.
    /// With `s0` and `s1` those two sums over the data alone, and `p0`,
    /// `p1` the parity symbols at alpha^1 and alpha^0:
    /// `p0 + p1 = s0` and `alpha*p0 + p1 = s1`, so
    /// `p0 = (s0 + s1) / (alpha + 1)` and `p1 = s0 + p0`.
    fn encode(&self, area: &mut [u8]) {
        for plane in 0..2 {
            for v in 0..self.vectors {
                let mut s0 = 0u8;
                let mut horner = 0u8;
                for i in 0..self.len {
                    let word = (v * self.vector_step + i * self.symbol_step) % self.wrap;
                    let symbol = area[2 * word + plane];
                    s0 ^= symbol;
                    horner = gf_times_alpha(horner) ^ symbol;
                }
                // Horner weighted the last data symbol by alpha^0; in the
                // full codeword it sits two places before the end.
                let s1 = gf_times_alpha(gf_times_alpha(horner));
                let p0 = gf_mul(s0 ^ s1, INV_ALPHA_PLUS_ONE);
                let p1 = s0 ^ p0;
                area[2 * (self.parity_at + v) + plane] = p0;
                area[2 * (self.parity_at + self.vectors + v) + plane] = p1;
            }
        }
    }
}

/// Fill in a raw Mode 2 sector's EDC, and for Form 1 its P and Q parity.
/// The submode byte (bit 5) picks the form; everything up to the EDC
/// must already be in place.
fn encode_mode2_sector(sector: &mut [u8]) {
    debug_assert_eq!(sector.len(), RAW_SECTOR_SIZE);
    if sector[SUBMODE_AT] & SUBMODE_FORM2 != 0 {
        let crc = edc(&sector[SUBHEADER_AT..FORM2_EDC_AT]);
        sector[FORM2_EDC_AT..FORM2_EDC_AT + 4].copy_from_slice(&crc.to_le_bytes());
        return;
    }
    let crc = edc(&sector[SUBHEADER_AT..FORM1_EDC_AT]);
    sector[FORM1_EDC_AT..FORM1_EDC_AT + 4].copy_from_slice(&crc.to_le_bytes());
    let mut header = [0u8; 4];
    header.copy_from_slice(&sector[HEADER_AT..HEADER_AT + 4]);
    sector[HEADER_AT..HEADER_AT + 4].fill(0);
    let area = &mut sector[HEADER_AT..];
    P_CODE.encode(area);
    Q_CODE.encode(area);
    sector[HEADER_AT..HEADER_AT + 4].copy_from_slice(&header);
}

fn bin_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// A sensible PSX `SYSTEM.CNF` that boots `PSX.EXE`. Write this as the
/// first file, then append the EXE under the name `PSX.EXE`.
pub fn default_system_cnf() -> Vec<u8> {
    // PSX-SPX documents: line-endings are CR-LF, each assignment on
    // its own line. TCB / EVENT / STACK are the "fine on everything"
    // values used by the official SDK templates.
    let text = b"BOOT = cdrom:\\PSX.EXE;1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFFF0\r\n";
    text.to_vec()
}

// ---------------------------------------------------------------------
// Helpers -- byte-layer encoding of ISO 9660 primitives.
// ---------------------------------------------------------------------

fn write_le_u32(dst: &mut [u8], value: u32) {
    dst[0..4].copy_from_slice(&value.to_le_bytes());
}

fn write_be_u32(dst: &mut [u8], value: u32) {
    dst[0..4].copy_from_slice(&value.to_be_bytes());
}

fn write_both_endian_u32(dst: &mut [u8], value: u32) {
    dst[0..4].copy_from_slice(&value.to_le_bytes());
    dst[4..8].copy_from_slice(&value.to_be_bytes());
}

fn write_both_endian_u16(dst: &mut [u8], value: u16) {
    dst[0..2].copy_from_slice(&value.to_le_bytes());
    dst[2..4].copy_from_slice(&value.to_be_bytes());
}

/// Fill `dst` with `s` as ASCII, space-padding (or truncating) to
/// `dst.len()`.
fn fill_ascii(dst: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(dst.len());
    dst[..n].copy_from_slice(&bytes[..n]);
    for byte in &mut dst[n..] {
        *byte = b' ';
    }
}

fn pad_to(buf: &mut Vec<u8>, size: usize) {
    if buf.len() < size {
        buf.resize(size, 0);
    }
}

/// Normalise a user-supplied filename like "hello.exe" to the
/// ISO 9660 Level 1 form "HELLO.EXE;1" suitable for a directory
/// record's file-identifier field.
fn iso_file_identifier(name: &str) -> String {
    let mut out = name.to_ascii_uppercase();
    if !out.contains(';') {
        out.push_str(";1");
    }
    out
}

/// Encode a single directory record. `ident` is the raw identifier
/// bytes -- a single `\x00` for ".", single `\x01` for "..", or the
/// ASCII-uppercase filename otherwise.
fn dir_record(extent_lba: u32, size: u32, ident: &[u8], is_dir: bool) -> Vec<u8> {
    let ident_len = ident.len() as u8;
    let base_len: u8 = 33;
    let mut record_len = base_len + ident_len;
    if !record_len.is_multiple_of(2) {
        record_len += 1;
    }
    let mut r = vec![0u8; record_len as usize];

    r[0] = record_len;
    r[1] = 0; // extended attribute record length
    write_both_endian_u32(&mut r[2..10], extent_lba);
    write_both_endian_u32(&mut r[10..18], size);
    r[18..25].copy_from_slice(&FIXED_DIR_DATE);
    r[25] = if is_dir { 0x02 } else { 0x00 };
    // r[26] file unit size = 0
    // r[27] interleave gap = 0
    write_both_endian_u16(&mut r[28..32], 1); // volume seq number
    r[32] = ident_len;
    r[33..33 + ident.len()].copy_from_slice(ident);
    // Optional trailing pad byte already zero if record_len was odd.

    r
}

/// Append one path-table entry for a directory. `parent_num` is 1 for
/// root (self-reference). `big_endian` toggles between the L-path and
/// M-path encoding.
fn encode_path_table_entry(
    buf: &mut Vec<u8>,
    extent_lba: u32,
    parent_num: u16,
    ident: &[u8],
    big_endian: bool,
) {
    buf.push(ident.len() as u8);
    buf.push(0); // extended attr record length
    if big_endian {
        buf.extend_from_slice(&extent_lba.to_be_bytes());
        buf.extend_from_slice(&parent_num.to_be_bytes());
    } else {
        buf.extend_from_slice(&extent_lba.to_le_bytes());
        buf.extend_from_slice(&parent_num.to_le_bytes());
    }
    buf.extend_from_slice(ident);
    if !ident.len().is_multiple_of(2) {
        buf.push(0); // padding to even
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xa_file_keeps_subheaders_and_form_edc() {
        let mut raw = vec![0u8; 2 * XA_SECTOR_SIZE];
        // Sector 0: Form 1 video (submode 0x48), sector 1: Form 2 audio (0x64).
        raw[..8].copy_from_slice(&[1, 0, 0x48, 0, 1, 0, 0x48, 0]);
        raw[8..8 + 2048].fill(0xAB);
        let s1 = XA_SECTOR_SIZE;
        raw[s1..s1 + 8].copy_from_slice(&[1, 0, 0x64, 0x01, 1, 0, 0x64, 0x01]);
        raw[s1 + 8..s1 + 8 + 2324].fill(0xCD);
        let mut b = IsoBuilder::new();
        b.add_xa_file("movie.str", raw).unwrap();
        let cooked = b.build();
        // File lands at LBA 21, size 2 * 2048, cooked data = Form 1 payload.
        assert_eq!(cooked[21 * SECTOR_SIZE], 0xAB);
        let bin = b.build_bin();
        let v = &bin[21 * RAW_SECTOR_SIZE..22 * RAW_SECTOR_SIZE];
        let a = &bin[22 * RAW_SECTOR_SIZE..23 * RAW_SECTOR_SIZE];
        assert_eq!(&v[16..24], &[1, 0, 0x48, 0, 1, 0, 0x48, 0]);
        assert_eq!(&a[16..24], &[1, 0, 0x64, 0x01, 1, 0, 0x64, 0x01]);
        assert_eq!(a[24], 0xCD);
        assert_eq!(v[24], 0xAB);
        assert_eq!(&v[0x818..0x81C], &edc(&v[0x10..0x818]).to_le_bytes());
        assert_eq!(&a[0x92C..0x930], &edc(&a[0x10..0x92C]).to_le_bytes());
        // Headers still carry the absolute MSF of LBA 21 / 22 in mode 2.
        assert_eq!(&v[12..16], &[0x00, 0x02, 0x21, 0x02]);
        assert_eq!(&a[12..16], &[0x00, 0x02, 0x22, 0x02]);
        assert!(b.add_xa_file("bad", vec![0; 100]).is_none());
    }

    #[test]
    fn empty_builder_produces_minimal_image() {
        // Even with no files: sector map 0..=20 is fixed, file area
        // starts at 21 but is empty, so total = 21 sectors.
        let img = IsoBuilder::new().build();
        assert_eq!(img.len(), 21 * SECTOR_SIZE);
        // System area is zero-filled.
        assert!(img[..16 * SECTOR_SIZE].iter().all(|&b| b == 0));
        // PVD at sector 16 with CD001 signature.
        assert_eq!(img[16 * SECTOR_SIZE], 0x01);
        assert_eq!(&img[16 * SECTOR_SIZE + 1..16 * SECTOR_SIZE + 6], b"CD001");
        // VDST at sector 17.
        assert_eq!(img[17 * SECTOR_SIZE], 0xFF);
        assert_eq!(&img[17 * SECTOR_SIZE + 1..17 * SECTOR_SIZE + 6], b"CD001");
    }

    #[test]
    fn single_file_lands_at_expected_sector() {
        let mut b = IsoBuilder::new();
        b.add_file("HELLO.TXT", b"hello world".to_vec());
        let img = b.build();
        // File should live at sector 21 per our layout plan.
        let file_start = 21 * SECTOR_SIZE;
        assert_eq!(&img[file_start..file_start + 11], b"hello world");
        // Everything past the written bytes in that sector is zero.
        assert!(img[file_start + 11..file_start + SECTOR_SIZE]
            .iter()
            .all(|&b| b == 0));
    }

    #[test]
    fn volume_id_lands_in_pvd() {
        let img = IsoBuilder::new().volume_id("HELLO").build();
        let vol_start = 16 * SECTOR_SIZE + 40;
        assert_eq!(&img[vol_start..vol_start + 5], b"HELLO");
    }

    #[test]
    fn playstation_identifiers_land_in_pvd() {
        let img = IsoBuilder::new().build();
        let pvd = &img[16 * SECTOR_SIZE..17 * SECTOR_SIZE];
        assert_eq!(&pvd[8..19], b"PLAYSTATION");
        assert_eq!(&pvd[574..585], b"PLAYSTATION");
    }

    #[test]
    fn system_area_overrides_initial_sectors() {
        let mut area = vec![0u8; 16 * SECTOR_SIZE];
        area[4 * SECTOR_SIZE] = 0xAB;
        let img = IsoBuilder::new().system_area(area).unwrap().build();
        assert_eq!(img[4 * SECTOR_SIZE], 0xAB);
        assert_eq!(&img[16 * SECTOR_SIZE + 1..16 * SECTOR_SIZE + 6], b"CD001");
    }

    #[test]
    fn multi_file_adds_sectors_per_file() {
        let mut b = IsoBuilder::new();
        b.add_file("SMALL.TXT", vec![b'A'; 100]);
        // Second file = 2 sectors worth; needs two allocated sectors.
        b.add_file("BIG.BIN", vec![b'B'; SECTOR_SIZE + 500]);
        let img = b.build();
        // SMALL.TXT at sector 21, BIG.BIN at sector 22.
        assert_eq!(img[21 * SECTOR_SIZE], b'A');
        assert_eq!(img[22 * SECTOR_SIZE], b'B');
        assert_eq!(img[23 * SECTOR_SIZE], b'B');
        // Total sector count = 21 (fixed) + 1 (SMALL.TXT) + 2 (BIG.BIN).
        assert_eq!(img.len() / SECTOR_SIZE, 24);
    }

    #[test]
    fn padding_reserves_invisible_file_area_sectors() {
        let mut b = IsoBuilder::new();
        b.add_file("FIRST.BIN", vec![b'A'; 1]);
        b.add_padding_sectors(3);
        b.add_file("SECOND.BIN", vec![b'B'; 1]);
        let img = b.build();

        assert_eq!(img[21 * SECTOR_SIZE], b'A');
        assert_eq!(img[25 * SECTOR_SIZE], b'B');
        assert_eq!(img.len() / SECTOR_SIZE, 26);

        let root = &img[20 * SECTOR_SIZE..21 * SECTOR_SIZE];
        let root_text = core::str::from_utf8(root).unwrap_or("");
        assert!(root_text.contains("FIRST.BIN;1"));
        assert!(root_text.contains("SECOND.BIN;1"));
        assert!(!root_text.contains("PADDING"));
    }

    #[test]
    fn default_system_cnf_boots_psx_exe() {
        let cnf = default_system_cnf();
        let text = core::str::from_utf8(&cnf).unwrap();
        assert!(text.contains("BOOT = cdrom:\\PSX.EXE;1"));
        assert!(text.contains("STACK = 801FFFF0"));
    }

    #[test]
    fn file_identifier_gets_version_suffix() {
        assert_eq!(iso_file_identifier("HELLO.TXT"), "HELLO.TXT;1");
        assert_eq!(iso_file_identifier("already;5"), "ALREADY;5");
    }

    #[test]
    fn path_table_sizes_match_pvd_field() {
        let img = IsoBuilder::new().build();
        // PVD path-table size at bytes 132..140 of sector 16.
        let pvd = &img[16 * SECTOR_SIZE..17 * SECTOR_SIZE];
        let path_size_le = u32::from_le_bytes(pvd[132..136].try_into().unwrap());
        assert_eq!(path_size_le, 10);
    }

    #[test]
    fn file_round_trips_via_disc_reader() {
        // Build a raw .bin image, then parse it with the companion
        // `Disc` type to confirm a file is readable through the same
        // path the emulator uses. End-to-end sanity check that the
        // PVD + directory map + file-extent LBAs all agree.
        let payload = b"ROUNDTRIP";
        let mut b = IsoBuilder::new();
        b.add_file("TEST.BIN", payload.to_vec());
        let bin = b.build_bin();

        let disc = crate::Disc::from_bin(bin);
        // File should be at sector 21 per the layout plan.
        let sector = disc.read_sector_user(21).expect("read sector 21");
        assert_eq!(&sector[..payload.len()], payload);
    }

    #[test]
    fn raw_bin_has_sync_pattern_at_every_sector() {
        let img = IsoBuilder::new().build_bin();
        let total = img.len() / RAW_SECTOR_SIZE;
        assert!(total >= 21);
        for lba in 0..total {
            let base = lba * RAW_SECTOR_SIZE;
            assert_eq!(img[base], 0x00, "LBA {lba}: sync byte 0");
            for (i, &b) in img[base + 1..base + 11].iter().enumerate() {
                assert_eq!(b, 0xFF, "LBA {lba} sync[{}]", i + 1);
            }
            assert_eq!(img[base + 11], 0x00, "LBA {lba}: sync byte 11");
            assert_eq!(img[base + 15], 0x02, "LBA {lba}: mode byte");
        }
    }

    #[test]
    fn raw_bin_user_data_matches_cooked() {
        let mut b = IsoBuilder::new();
        b.add_file("X.DAT", b"PAYLOAD-BYTES".to_vec());
        let cooked = b.build();
        let raw = b.build_bin();
        for lba in 0..(cooked.len() / SECTOR_SIZE) {
            let cooked_start = lba * SECTOR_SIZE;
            let raw_user_start = lba * RAW_SECTOR_SIZE + 24;
            assert_eq!(
                &cooked[cooked_start..cooked_start + SECTOR_SIZE],
                &raw[raw_user_start..raw_user_start + SECTOR_SIZE],
                "user data differs at LBA {lba}"
            );
        }
    }

    #[test]
    fn raw_bin_has_mode2_form1_edc_ecc() {
        let mut b = IsoBuilder::new();
        b.add_file("X.DAT", b"PAYLOAD-BYTES".to_vec());
        let raw = b.build_bin();
        let sector = &raw[16 * RAW_SECTOR_SIZE..17 * RAW_SECTOR_SIZE];
        let expected_edc = edc(&sector[0x10..0x818]).to_le_bytes();
        assert_eq!(&sector[0x818..0x81C], &expected_edc);
        assert!(sector[0x81C..].iter().any(|&byte| byte != 0));
    }

    #[test]
    fn raw_bin_msf_header_is_bcd_encoded() {
        // LBA 0 → MSF 00:02:00 → BCD 0x00 0x02 0x00.
        let img = IsoBuilder::new().build_bin();
        assert_eq!(img[12], 0x00);
        assert_eq!(img[13], 0x02);
        assert_eq!(img[14], 0x00);
        // LBA 16 → 00:02:16 → BCD 0x00 0x02 0x16.
        let b16 = 16 * RAW_SECTOR_SIZE;
        assert_eq!(img[b16 + 12], 0x00);
        assert_eq!(img[b16 + 13], 0x02);
        assert_eq!(img[b16 + 14], 0x16);
    }

    /// FNV-1a 64 over a whole image, used to pin `build_bin` output.
    fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }

    /// Deterministic filler bytes (a 32-bit LCG), so payloads vary per byte.
    fn filler(seed: u32, len: usize) -> Vec<u8> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            })
            .collect()
    }

    /// Raw XA sectors: one per `(submode, seed)`, payload from `filler`.
    fn xa_sectors(kinds: &[(u8, u32)]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (i, &(submode, seed)) in kinds.iter().enumerate() {
            let sub = [1, i as u8, submode, 0x01];
            raw.extend_from_slice(&sub);
            raw.extend_from_slice(&sub);
            raw.extend_from_slice(&filler(seed, XA_SECTOR_SIZE - 8));
        }
        raw
    }

    /// Images built before the EDC/ECC encoder was rewritten from
    /// ECMA-130, hashed. The rewrite must reproduce them byte for byte.
    #[test]
    fn build_bin_matches_golden_images() {
        let empty = IsoBuilder::new().build_bin();

        let mut one = IsoBuilder::new().volume_id("GOLDEN");
        one.add_file("ONE.DAT", filler(1, 5000));
        let one = one.build_bin();

        let mut many = IsoBuilder::new();
        many.add_file("SYSTEM.CNF", default_system_cnf());
        many.add_file("ZERO.BIN", vec![0; 2048]);
        many.add_file("ONES.BIN", vec![0xFF; 4097]);
        many.add_padding_sectors(3);
        many.add_file("TINY.TXT", b"x".to_vec());
        many.add_file("RAND.BIN", filler(7, 3 * 2048 + 11));
        let many = many.build_bin();

        let mut xa = IsoBuilder::new();
        xa.add_file("LEAD.DAT", filler(3, 100));
        xa.add_xa_file(
            "MOVIE.STR",
            xa_sectors(&[
                (0x08, 11),
                (0x48, 12),
                (0x64, 13),
                (0x20, 14),
                (0x08, 15),
                (0x24, 16),
                (0xE4, 17),
                (0x48, 18),
            ]),
        )
        .unwrap();
        xa.add_file("TAIL.DAT", filler(5, 2048 * 2));
        let xa = xa.build_bin();

        let got = [
            (empty.len(), fnv1a64(&empty)),
            (one.len(), fnv1a64(&one)),
            (many.len(), fnv1a64(&many)),
            (xa.len(), fnv1a64(&xa)),
        ];
        let want: [(usize, u64); 4] = [
            (21 * RAW_SECTOR_SIZE, 0x78C8_015E_E9F9_01A3),
            (24 * RAW_SECTOR_SIZE, 0x5AFC_1554_D3AB_B66D),
            (34 * RAW_SECTOR_SIZE, 0x92F9_30D2_6F43_49B0),
            (32 * RAW_SECTOR_SIZE, 0x6643_03E0_863B_B740),
        ];
        assert_eq!(got, want);
    }

    /// Check every P and Q codeword of a built Form 1 sector the way a
    /// decoder would: both ECMA-130 parity-check sums must be zero, with
    /// the weights raised to their powers directly rather than by Horner.
    #[test]
    fn form1_codewords_have_zero_syndromes() {
        let mut b = IsoBuilder::new();
        b.add_file("RAND.BIN", filler(99, 2048));
        let bin = b.build_bin();
        let mut sector = bin[21 * RAW_SECTOR_SIZE..22 * RAW_SECTOR_SIZE].to_vec();
        sector[HEADER_AT..HEADER_AT + 4].fill(0);
        let area = &sector[HEADER_AT..];
        let alpha_pow = |e: usize| (0..e).fold(1u8, |a, _| gf_times_alpha(a));
        for code in [&P_CODE, &Q_CODE] {
            let n = code.len + 2;
            for plane in 0..2 {
                for v in 0..code.vectors {
                    let mut words: Vec<usize> = (0..code.len)
                        .map(|i| (v * code.vector_step + i * code.symbol_step) % code.wrap)
                        .collect();
                    words.push(code.parity_at + v);
                    words.push(code.parity_at + code.vectors + v);
                    let (mut s0, mut s1) = (0u8, 0u8);
                    for (k, &w) in words.iter().enumerate() {
                        let c = area[2 * w + plane];
                        s0 ^= c;
                        s1 ^= gf_mul(alpha_pow(n - 1 - k), c);
                    }
                    assert_eq!((s0, s1), (0, 0), "vector {v} plane {plane}");
                }
            }
        }
        // P and Q together write every byte from 0x81C to the end.
        assert_eq!(
            2 * (Q_CODE.parity_at + 2 * Q_CODE.vectors),
            RAW_SECTOR_SIZE - HEADER_AT
        );
        assert_eq!(2 * P_CODE.parity_at, 0x81C - HEADER_AT);
    }
}
