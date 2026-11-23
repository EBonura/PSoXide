//! A scene disc: the playtest layout plus `SCENES.TBL` (version 2) and the
//! disc's further images, Program exes and Overlay blobs, one image row each.
//!
//! The table format is `psx-scene-abi`'s; this module places the image files,
//! fills the image and scene rows from them, and checks the build rules the
//! core relies on but cannot check itself. mkisopsx uses it for a single-game
//! disc and the demo disc's mkdisc can use it for each member image.
//!
//! Layout, every LBA member-relative:
//!
//! ```text
//! 21  SYSTEM.CNF
//! 22  SCENES.TBL      one sector
//! 23  PSX.EXE         image 0, the boot program
//! ..  image files     in image order while they fit the boot area,
//!                     contiguous with the boot exe (no seek from it)
//! ..  padding, optional CDTEST.BIN
//! 1024 WORLD.PAK, UI.PAK
//! ..  the image files that did not fit, still in image order
//! ```
//!
//! Each image is one file, so a Program or a RESIDENT_SET Overlay loads in one
//! read. The core never reads the table from the disc at a switch: a program
//! reads it once and keeps a RAM copy, which may be a compact one
//! ([`SceneDisc::compact`], `psx_scene_abi::encode_compact`) holding only the
//! rows that program reaches.
//!
//! Build rules checked here:
//!
//! - image 0 is a Program, the boot exe, at LBA 23; each Program has exactly
//!   one START scene (its entry is the exe's PC); the boot scene is image 0's;
//! - an in-program scene's entry lies inside its Program's load range, an
//!   Overlay scene's inside the Overlay's;
//! - a Program loads in `LOAD_ADDR..PAYLOAD_END` and its load size stops before
//!   its stored core copy ([`payload_before_core`]);
//! - an Overlay loads inside its owner's reserved RAM (below `PAYLOAD_END`);
//!   co-resident images never overlap (an Overlay and its owner, two
//!   RESIDENT_SET Overlays of one owner); an Overlay's evicts mask is exactly
//!   the other images whose load ranges intersect its own;
//! - `I <= 32` and `S + I <= 62`.
//!
//! A scene that runs inside its Program's exe has no image row: its scene row
//! names the Program's image and its entry is a function in the exe.

pub use psx_scene_abi as abi;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::{format, vec};

use psx_scene_abi::{
    block, encode_compact, encode_table, word_fnv, Class, ImageRow, Kind, SceneRow, Span,
    TableError, BOOT_EXE_LBA, CORE_WINDOW, LOAD_ADDR, MAX_IMAGES, MAX_ROWS, NAME_BYTES,
    PAYLOAD_END, SCENE_FLAG_START, SECTOR_BYTES, STACK_INIT, TABLE_FILE_NAME, TABLE_LBA,
};

use crate::{
    cd_stream_bench_payload, pad_to_payload, Exe, IsoBuilder, PlaytestLayoutError,
    CD_STREAM_BENCH_FILE_NAME, PLAYTEST_FIRST_FILE_LBA, UI_PACK_FILE_NAME,
    WORLD_PACK_DEFAULT_START_LBA, WORLD_PACK_FILE_NAME,
};

/// `SYSTEM.CNF` for a scene disc: the default one with `STACK` at the core window's base.
///
/// psx-rt's `_start` never sets `$sp`; the BIOS does, from this `STACK` line, not from the exe
/// header. The default `801FFFF0` would start the boot program's stack inside the core window,
/// so a scene disc starts it at [`STACK_INIT`], where every hosting program's link puts it.
#[must_use]
pub fn scene_system_cnf() -> Vec<u8> {
    format!("BOOT = cdrom:\\PSX.EXE;1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = {STACK_INIT:08X}\r\n")
        .into_bytes()
}

/// Name the boot exe must have: `SYSTEM.CNF` boots it.
pub const BOOT_EXE_NAME: &str = "PSX.EXE";

/// One image of the plan, in image order (the index is its loaded-mask bit).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSpec {
    /// Root-directory file name (upper-cased); scenes and overlays name the image by it.
    pub name: String,
    /// Program or Overlay.
    pub kind: Kind,
    /// The file's bytes: a PS-X EXE for a Program, the raw blob for an Overlay. `None` for the
    /// boot exe passed to [`add_scene_disc_files`] itself (image 0 only).
    pub file: Option<Vec<u8>>,
    /// Program: the core hash its row carries instead of the plan's core image's (a program of
    /// another core pin); it then keeps its whole payload.
    pub core_hash: Option<u32>,
    /// Program: the RAM its Overlays may load into. Needed when it owns an Overlay.
    pub reserve: Option<Span>,
    /// Program: names of the scenes it can switch to, for its compact table copy (the scenes
    /// entering it and its Overlays' are always in). `None` reaches every scene.
    pub reaches: Option<Vec<String>>,
    /// Overlay: the owning Program's image name.
    pub owner: Option<String>,
    /// Overlay: its load address.
    pub load_addr: u32,
    /// Overlay: how it is kept.
    pub class: Option<Class>,
}

impl ImageSpec {
    /// A Program image; `file` `None` stands for the boot exe.
    pub fn program(name: &str, file: Option<Vec<u8>>) -> Self {
        Self {
            name: name.to_ascii_uppercase(),
            kind: Kind::Program,
            file,
            core_hash: None,
            reserve: None,
            reaches: None,
            owner: None,
            load_addr: 0,
            class: None,
        }
    }

    /// An Overlay image owned by the Program named `owner`.
    pub fn overlay(name: &str, file: Vec<u8>, owner: &str, load_addr: u32, class: Class) -> Self {
        Self {
            kind: Kind::Overlay,
            owner: Some(owner.to_ascii_uppercase()),
            load_addr,
            class: Some(class),
            ..Self::program(name, Some(file))
        }
    }
}

/// One scene of the plan, in scene order (the index is its id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneSpec {
    /// Row name, ASCII, up to 16 bytes; [`ImageSpec::reaches`] names scenes by it.
    pub name: String,
    /// The image it runs in, by name.
    pub image: String,
    /// It starts its Program: the entry is the exe's PC, so `entry` must be `None`.
    pub start: bool,
    /// The entry PC of an in-program or Overlay scene.
    pub entry: Option<u32>,
    /// Row flags other than START (`psx_scene_abi::SCENE_FLAG_PARTNER_CREDIT`).
    pub flags: u8,
    /// Partner credit id, 0 none.
    pub partner_credit: u16,
    /// Minimum handoff block version the scene needs.
    pub min_block_version: u8,
}

impl SceneSpec {
    /// The scene that starts the Program `image`.
    pub fn start(name: &str, image: &str) -> Self {
        Self {
            name: name.into(),
            image: image.to_ascii_uppercase(),
            start: true,
            entry: None,
            flags: 0,
            partner_credit: 0,
            min_block_version: block::VERSION as u8,
        }
    }

    /// A scene entered at `entry` inside `image` (a running Program, or an Overlay).
    pub fn at(name: &str, image: &str, entry: u32) -> Self {
        Self {
            start: false,
            entry: Some(entry),
            ..Self::start(name, image)
        }
    }
}

/// Every image and scene of a disc.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScenePlan {
    /// The images, boot exe first.
    pub images: Vec<ImageSpec>,
    /// The scenes.
    pub scenes: Vec<SceneSpec>,
    /// The prebuilt core image (`psx-core.bin`): its word FNV is the core hash Program rows
    /// carry, and every Program must contain it.
    pub core_image: Option<Vec<u8>>,
}

/// One Program's compact RAM copy of the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactCopy {
    /// The Program's image index.
    pub image: u8,
    /// The scene ids it holds.
    pub scenes: Vec<u16>,
    /// The images it holds, as a mask.
    pub images: u32,
    /// The copy's bytes (`psx_scene_abi::compact_bytes` long).
    pub bytes: Vec<u8>,
}

/// What [`add_scene_disc_files`] wrote: the rows as pressed and the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneDisc {
    /// Scene rows, in id order.
    pub scenes: Vec<SceneRow>,
    /// Image rows, in index order.
    pub images: Vec<ImageRow>,
    /// The boot scene id.
    pub boot_scene: u16,
    /// `SCENES.TBL`'s bytes, one sector.
    pub table: Vec<u8>,
    /// Each Program's compact copy, in image order.
    pub compact: Vec<CompactCopy>,
}

/// Why a scene disc could not be laid out. Names are image or scene names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneDiscError {
    /// The boot area did not fit (the boot exe starts at LBA 23 here).
    Layout(PlaytestLayoutError),
    /// Image count outside `1..=32`, or scenes and images past 62 rows.
    RowCount {
        /// Images.
        images: usize,
        /// Scenes.
        scenes: usize,
    },
    /// Image 0 is not the boot exe (a Program named [`BOOT_EXE_NAME`] with no file of its own),
    /// or another image has no file.
    BootImage(String),
    /// A name is empty, too long or not ASCII.
    BadName(String),
    /// Two images, or two scenes, would have one name (fixed layout names included).
    DuplicateName(String),
    /// A scene or overlay names an image that is not in the plan, or a reach names no scene.
    Unknown(String),
    /// A Program image's file is not a PS-X EXE.
    NotAnExe(String),
    /// A Program's payload is not whole sectors.
    TSize(String),
    /// A Program would load outside `0x80010000..PAYLOAD_END` (the switch stack and the core
    /// window above it), or a reserve reaches outside it.
    Bounds {
        /// The image.
        name: String,
        /// Its load address.
        start: u32,
        /// One past its last byte.
        end: u32,
    },
    /// The core image is empty, not whole words, or larger than the window.
    CoreImage(usize),
    /// A Program does not end with the core image's bytes (only sector padding may follow its
    /// stored copy).
    CoreMissing(String),
    /// An Overlay's owner is not a Program, it has no class, its owner has no reserve, or a
    /// Program carries Overlay fields.
    Overlay(String),
    /// An Overlay loads outside its owner's reserved RAM.
    OutsideReserve {
        /// The Overlay.
        name: String,
        /// Its load range.
        span: Span,
        /// The owner's reserve.
        reserve: Span,
    },
    /// Two co-resident images overlap.
    Overlap {
        /// One image.
        a: String,
        /// The other.
        b: String,
    },
    /// A Program has no START scene, or more than one.
    StartScenes {
        /// The Program.
        image: String,
        /// START scenes it has.
        count: usize,
    },
    /// A scene's entry is missing, given for a START scene, outside its image's load range, or
    /// START is set on an Overlay scene.
    Entry(String),
    /// The table writer refused.
    Table(TableError),
}

impl From<PlaytestLayoutError> for SceneDiscError {
    fn from(error: PlaytestLayoutError) -> Self {
        SceneDiscError::Layout(error)
    }
}

const RESERVED_NAMES: [&str; 5] = [
    "SYSTEM.CNF",
    TABLE_FILE_NAME,
    WORLD_PACK_FILE_NAME,
    UI_PACK_FILE_NAME,
    CD_STREAM_BENCH_FILE_NAME,
];

fn sectors_of(len: usize) -> u32 {
    len.div_ceil(SECTOR_BYTES).max(1) as u32
}

fn overlaps(a: Span, b: Span) -> bool {
    a.start < b.end && b.start < a.end
}

/// Where a program's payload stops for the core: the start of its stored copy
/// of `core` (psoxide-core.ld stores it right after `.data`), rounded up to a
/// whole sector. A switch reads only this much, never the unused copy; the
/// running core is already in the window. `None` when the copy is missing or
/// anything but sector padding follows it.
pub fn payload_before_core(payload: &[u8], core: &[u8]) -> Option<usize> {
    if core.is_empty() || payload.len() < core.len() {
        return None;
    }
    let at = (0..(payload.len() - core.len()) / 4 + 1)
        .rev()
        .map(|word| word * 4)
        .find(|&at| payload[at..at + core.len()] == *core)?;
    payload[at + core.len()..]
        .iter()
        .all(|&b| b == 0)
        .then(|| at.next_multiple_of(SECTOR_BYTES))
}

/// The image row of a Program whose exe as pressed is `bytes`, its header sector at
/// member-relative `lba`, plus the exe's entry PC. `core` is the plan's core image: its word FNV
/// is the row's core hash (unless `core_hash` names one), and the load size ends before the
/// exe's stored copy of it. `member_base` and `cdda_delta` are zero; the demo disc's mkdisc sets
/// them for each member's Programs before `psx_scene_abi::encode_table`.
pub fn program_image_row(
    name: &str,
    bytes: &[u8],
    lba: u32,
    core_hash: Option<u32>,
    core: Option<&[u8]>,
) -> Result<(ImageRow, u32), SceneDiscError> {
    let exe = Exe::parse(bytes).map_err(|_| SceneDiscError::NotAnExe(name.into()))?;
    if !exe.payload.len().is_multiple_of(SECTOR_BYTES) {
        return Err(SceneDiscError::TSize(name.into()));
    }
    let (stored, core_hash) = match (core_hash, core) {
        (Some(hash), _) => (exe.payload.len(), hash),
        (None, Some(core)) => (
            payload_before_core(&exe.payload, core)
                .ok_or_else(|| SceneDiscError::CoreMissing(name.into()))?,
            word_fnv(core),
        ),
        (None, None) => (exe.payload.len(), 0),
    };
    let size = stored as u32;
    let end = exe.load_addr.checked_add(size);
    if exe.load_addr < LOAD_ADDR || end.is_none_or(|end| end > PAYLOAD_END) {
        return Err(SceneDiscError::Bounds {
            name: name.into(),
            start: exe.load_addr,
            end: end.unwrap_or(u32::MAX),
        });
    }
    let row = ImageRow {
        lba,
        size,
        load_addr: exe.load_addr,
        checksum: word_fnv(&exe.payload[..stored]),
        core_hash,
        evicts: 0,
        member_base: 0,
        kind: Kind::Program,
        class: None,
        owner: 0,
        cdda_delta: 0,
    };
    Ok((row, exe.initial_pc))
}

fn check_name(name: &str, seen: &mut Vec<String>) -> Result<(), SceneDiscError> {
    if name.is_empty() || name.len() > NAME_BYTES || !name.is_ascii() {
        return Err(SceneDiscError::BadName(name.into()));
    }
    if seen.iter().any(|s| s == name) {
        return Err(SceneDiscError::DuplicateName(name.into()));
    }
    seen.push(name.into());
    Ok(())
}

/// The playtest layout of [`crate::add_playtest_files`] with `SCENES.TBL` at
/// LBA 22, the boot exe at 23 and the plan's other images placed as the module
/// comment shows. `WORLD.PAK` stays at LBA 1024, so cooked pack LBAs do not
/// move; the boot exe has one sector less room than without a table.
///
/// Rows are filled from the files as pressed and checked against the module's
/// build rules; the table is written into its sector last.
pub fn add_scene_disc_files(
    builder: &mut IsoBuilder,
    mut exe_bytes: Vec<u8>,
    world_pack: Option<Vec<u8>>,
    ui_pack: Option<Vec<u8>>,
    cdtest_sectors: Option<usize>,
    plan: &ScenePlan,
) -> Result<SceneDisc, SceneDiscError> {
    let (images, scenes) = (&plan.images, &plan.scenes);
    let rows_ok = (1..=MAX_IMAGES).contains(&images.len())
        && !scenes.is_empty()
        && images.len() + scenes.len() <= MAX_ROWS;
    if !rows_ok {
        return Err(SceneDiscError::RowCount {
            images: images.len(),
            scenes: scenes.len(),
        });
    }
    let boot = &images[0];
    if boot.kind != Kind::Program || boot.file.is_some() || boot.name != BOOT_EXE_NAME {
        return Err(SceneDiscError::BootImage(boot.name.clone()));
    }
    if let Some(other) = images[1..].iter().find(|image| image.file.is_none()) {
        return Err(SceneDiscError::BootImage(other.name.clone()));
    }
    let mut seen: Vec<String> = RESERVED_NAMES.iter().map(|&n| n.into()).collect();
    for image in images {
        check_name(&image.name, &mut seen)?;
    }
    let mut seen = Vec::new();
    for scene in scenes {
        check_name(&scene.name, &mut seen)?;
    }
    if let Some(core) = &plan.core_image {
        if core.is_empty()
            || !core.len().is_multiple_of(4)
            || core.len() > CORE_WINDOW.len() as usize
        {
            return Err(SceneDiscError::CoreImage(core.len()));
        }
    }
    let index_of = |name: &str| -> Result<u8, SceneDiscError> {
        images
            .iter()
            .position(|image| image.name == name)
            .map(|i| i as u8)
            .ok_or_else(|| SceneDiscError::Unknown(name.into()))
    };

    // Every file as it will be pressed: exes padded to their payload, blobs to whole sectors.
    let _ = pad_to_payload(&mut exe_bytes);
    let mut files: Vec<Vec<u8>> = Vec::with_capacity(images.len());
    for image in images {
        let mut bytes = image.file.clone().unwrap_or_else(|| exe_bytes.clone());
        match image.kind {
            Kind::Program => {
                if pad_to_payload(&mut bytes).is_err() {
                    return Err(SceneDiscError::NotAnExe(image.name.clone()));
                }
            }
            Kind::Overlay => {
                let len = bytes.len().next_multiple_of(SECTOR_BYTES).max(SECTOR_BYTES);
                bytes.resize(len, 0);
            }
        }
        files.push(bytes);
    }

    // The boot area, as add_playtest_files checks it, one sector shorter.
    let exe_sectors = sectors_of(files[0].len());
    let cdtest_u32 = cdtest_sectors.unwrap_or(0) as u32;
    let cdtest_start = WORLD_PACK_DEFAULT_START_LBA.saturating_sub(cdtest_u32);
    let max_exe_sectors = cdtest_start.saturating_sub(BOOT_EXE_LBA);
    if exe_sectors > max_exe_sectors {
        return Err(PlaytestLayoutError::BootExeTooLarge {
            exe_sectors,
            max_sectors: max_exe_sectors,
        }
        .into());
    }
    let next_after_exe = BOOT_EXE_LBA + exe_sectors;
    if cdtest_u32 > WORLD_PACK_DEFAULT_START_LBA.saturating_sub(next_after_exe) {
        return Err(PlaytestLayoutError::CdtestTooLarge {
            cdtest_sectors: cdtest_u32,
            max_sectors: WORLD_PACK_DEFAULT_START_LBA.saturating_sub(next_after_exe),
        }
        .into());
    }

    debug_assert_eq!(PLAYTEST_FIRST_FILE_LBA + 1, TABLE_LBA);
    builder.add_file("SYSTEM.CNF", scene_system_cnf());
    builder.add_file(TABLE_FILE_NAME, vec![0u8; SECTOR_BYTES]);
    builder.add_file(BOOT_EXE_NAME, files[0].clone());
    // Images in table order: contiguous with the boot exe while they fit, the
    // rest after the packs, so a boot that loads them in order reads on.
    let mut room = cdtest_start - next_after_exe;
    let mut later = Vec::new();
    for index in 1..images.len() {
        let sectors = sectors_of(files[index].len());
        if later.is_empty() && sectors <= room {
            builder.add_file(&images[index].name, files[index].clone());
            room -= sectors;
        } else {
            later.push(index);
        }
    }
    builder.add_padding_sectors(room);
    if let Some(sectors) = cdtest_sectors {
        builder.add_file(CD_STREAM_BENCH_FILE_NAME, cd_stream_bench_payload(sectors));
    }
    if let Some(world_pack) = world_pack {
        builder.add_file(WORLD_PACK_FILE_NAME, world_pack);
    }
    if let Some(ui_pack) = ui_pack {
        builder.add_file(UI_PACK_FILE_NAME, ui_pack);
    }
    for index in later {
        builder.add_file(&images[index].name, files[index].clone());
    }
    let extents = builder.file_extents();
    let lba_of = |name: &str| {
        extents
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|&(_, lba, _)| lba)
            .expect("every image was added")
    };

    // Image rows. Programs first, so an Overlay can be checked against its owner.
    let core = plan.core_image.as_deref();
    let mut rows: Vec<Option<ImageRow>> = vec![None; images.len()];
    let mut pcs = vec![0u32; images.len()];
    for (index, image) in images.iter().enumerate() {
        if image.kind != Kind::Program {
            continue;
        }
        if image.owner.is_some() || image.class.is_some() || image.load_addr != 0 {
            return Err(SceneDiscError::Overlay(image.name.clone()));
        }
        if let Some(reserve) = image.reserve {
            if reserve.start < LOAD_ADDR || reserve.end > PAYLOAD_END || reserve.is_empty() {
                return Err(SceneDiscError::Bounds {
                    name: image.name.clone(),
                    start: reserve.start,
                    end: reserve.end,
                });
            }
        }
        let lba = lba_of(&image.name);
        let (row, pc) = program_image_row(&image.name, &files[index], lba, image.core_hash, core)?;
        rows[index] = Some(row);
        pcs[index] = pc;
    }
    for (index, image) in images.iter().enumerate() {
        if image.kind != Kind::Overlay {
            continue;
        }
        let bad = || SceneDiscError::Overlay(image.name.clone());
        let owner_name = image.owner.as_deref().ok_or_else(bad)?;
        let owner = index_of(owner_name)?;
        let owner_spec = &images[owner as usize];
        let class = image.class.ok_or_else(bad)?;
        if owner_spec.kind != Kind::Program || image.core_hash.is_some() {
            return Err(bad());
        }
        let reserve = owner_spec.reserve.ok_or_else(bad)?;
        let size = files[index].len() as u32;
        let span = Span {
            start: image.load_addr,
            end: image.load_addr.saturating_add(size),
        };
        if !reserve.holds(span.start, span.end) {
            return Err(SceneDiscError::OutsideReserve {
                name: image.name.clone(),
                span,
                reserve,
            });
        }
        rows[index] = Some(ImageRow {
            lba: lba_of(&image.name),
            size,
            load_addr: image.load_addr,
            checksum: word_fnv(&files[index]),
            core_hash: 0,
            evicts: 0,
            member_base: 0,
            kind: Kind::Overlay,
            class: Some(class),
            owner,
            cdda_delta: 0,
        });
    }
    let mut rows: Vec<ImageRow> = rows.into_iter().map(|r| r.expect("every image")).collect();
    debug_assert_eq!(rows[0].lba, BOOT_EXE_LBA);

    // Co-resident images never overlap: an Overlay and its owner, two RESIDENT_SETs of one owner.
    for (i, a) in rows.iter().enumerate() {
        for (j, b) in rows.iter().enumerate().skip(i + 1) {
            let co_resident = match (a.kind, b.kind) {
                (Kind::Program, Kind::Overlay) => b.owner as usize == i,
                (Kind::Overlay, Kind::Program) => a.owner as usize == j,
                (Kind::Overlay, Kind::Overlay) => {
                    a.owner == b.owner
                        && a.class == Some(Class::ResidentSet)
                        && b.class == Some(Class::ResidentSet)
                }
                (Kind::Program, Kind::Program) => false,
            };
            if co_resident && overlaps(a.span(), b.span()) {
                return Err(SceneDiscError::Overlap {
                    a: images[i].name.clone(),
                    b: images[j].name.clone(),
                });
            }
        }
    }
    // An Overlay's evicts mask: exactly the other images whose load ranges intersect its own.
    let spans: Vec<Span> = rows.iter().map(ImageRow::span).collect();
    for (i, row) in rows.iter_mut().enumerate() {
        if row.kind == Kind::Overlay {
            row.evicts = (0..spans.len())
                .filter(|&j| j != i && overlaps(spans[i], spans[j]))
                .fold(0, |mask, j| mask | 1 << j);
        }
    }

    // Scene rows.
    let mut scene_rows = Vec::with_capacity(scenes.len());
    let mut starts = vec![0usize; images.len()];
    for (id, scene) in scenes.iter().enumerate() {
        let image = index_of(&scene.image)?;
        let row = &rows[image as usize];
        let bad = || SceneDiscError::Entry(scene.name.clone());
        let entry = match (row.kind, scene.start, scene.entry) {
            (Kind::Program, true, None) => {
                starts[image as usize] += 1;
                pcs[image as usize]
            }
            (_, false, Some(entry)) if row.span().holds(entry, entry.saturating_add(1)) => entry,
            _ => return Err(bad()),
        };
        let start_flag = if scene.start { SCENE_FLAG_START } else { 0 };
        scene_rows.push(SceneRow {
            id: id as u16,
            image,
            flags: (scene.flags & !SCENE_FLAG_START) | start_flag,
            entry,
            partner_credit: scene.partner_credit,
            min_block_version: scene.min_block_version,
            name: SceneRow::name_bytes(&scene.name),
        });
    }
    for (index, image) in images.iter().enumerate() {
        if image.kind == Kind::Program && starts[index] != 1 {
            return Err(SceneDiscError::StartScenes {
                image: image.name.clone(),
                count: starts[index],
            });
        }
    }
    let boot_scene = scene_rows
        .iter()
        .find(|scene| scene.image == 0 && scene.starts_program())
        .expect("image 0 has its START scene")
        .id;

    let mut table = vec![0u8; SECTOR_BYTES];
    encode_table(&scene_rows, &rows, boot_scene, &mut table).map_err(SceneDiscError::Table)?;

    // Each Program's compact copy: the scenes entering it or its Overlays, the scenes it
    // reaches, and the images those need.
    let mut compact = Vec::new();
    for (index, image) in images.iter().enumerate() {
        if image.kind != Kind::Program {
            continue;
        }
        let ours = |i: u8| {
            let row = &rows[i as usize];
            i as usize == index || (row.kind == Kind::Overlay && row.owner as usize == index)
        };
        let mut ids: Vec<u16> = Vec::new();
        for scene in &scene_rows {
            let reached = match &image.reaches {
                None => true,
                Some(names) => names.iter().any(|n| n.as_str() == scene.name_str()),
            };
            if reached || ours(scene.image) {
                ids.push(scene.id);
            }
        }
        if let Some(names) = &image.reaches {
            if let Some(name) = names
                .iter()
                .find(|n| !scenes.iter().any(|s| s.name == n.as_str()))
            {
                return Err(SceneDiscError::Unknown(name.clone()));
            }
        }
        let mut mask = 0u32;
        for i in 0..rows.len() as u8 {
            if ours(i) {
                mask |= 1 << i;
            }
        }
        for &id in &ids {
            mask |= 1 << scene_rows[id as usize].image;
        }
        let mut bytes = vec![0u8; SECTOR_BYTES];
        let len = encode_compact(&table, &ids, mask, &mut bytes).map_err(SceneDiscError::Table)?;
        bytes.truncate(len);
        compact.push(CompactCopy {
            image: index as u8,
            scenes: ids,
            images: mask,
            bytes,
        });
    }

    let replaced = builder.set_file_content(TABLE_FILE_NAME, table.clone());
    debug_assert!(replaced);
    Ok(SceneDisc {
        scenes: scene_rows,
        images: rows,
        boot_scene,
        table,
        compact,
    })
}

#[cfg(test)]
mod tests;
