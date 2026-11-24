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
//!   RESIDENT_SET Overlays of one owner; a Program counts up to its stored
//!   core, not to the sector edge its row loads: what lies past it is dead
//!   once it runs); an Overlay's evicts mask is exactly
//!   the other images whose ranges intersect its own, plus its own bit for
//!   a VOLATILE Overlay (one over memory its owner also uses: never kept, so
//!   read again at every load);
//! - a Program's carried Overlays (`carries`, its row's word 5) are its own,
//!   not VOLATILE, and its payload holds each byte for byte with its sector
//!   padding; its Overlays may lie over those ranges of it and no other;
//! - an image placed before a further file (`add_scene_disc_files_with`) goes
//!   right before it; an image with a tail is the Overlay whose file ends where
//!   the next file (`WORLD.PAK` or that one) starts, with nothing between them; its row's size and checksum cover the
//!   tail, so the reserve and overlap rules above count it too;
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
    PAYLOAD_END, REQUIRE_MARKER, SCENE_FLAG_START, SECTOR_BYTES, STACK_INIT, TABLE_FILE_NAME,
    TABLE_LBA,
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
#[allow(
    clippy::exhaustive_structs,
    reason = "plain data the disc builders fill in by literal"
)]
pub struct ImageSpec {
    /// Root-directory file name (upper-cased); scenes and overlays name the image by it.
    pub name: String,
    /// Program or Overlay.
    pub kind: Kind,
    /// The file's bytes: a PS-X EXE for a Program, the raw blob for an Overlay. `None` for the
    /// boot exe passed to [`add_scene_disc_files`] itself (image 0 only).
    pub file: Option<Vec<u8>>,
    /// Program: the core hash its row carries instead of the plan's core image's (a program of
    /// another core pin); it then keeps its whole payload. 0 says the program is not on the core
    /// at all (a boot program that chain-loads the game, say): no running core has hash 0, so no
    /// core switch can start it. A program without a stored core and without this is refused
    /// ([`SceneDiscError::CoreMissing`]): a program that forgot to link the core.
    pub core_hash: Option<u32>,
    /// Program: the RAM its Overlays may load into. Needed when it owns an Overlay.
    pub reserve: Option<Span>,
    /// Program: names of the scenes it can switch to, for its compact table copy (the scenes
    /// entering it and its Overlays' are always in). `None` reaches every scene.
    pub reaches: Option<Vec<String>>,
    /// Program: its own Overlays its exe already holds at their load addresses, byte for byte
    /// (a front end linked into the program). Their bits go into its row's word 5, so the core
    /// counts them as loaded whenever the program is entered; their range may then lie inside
    /// the program's payload. None may be VOLATILE ([`SceneDiscError::Carried`] otherwise).
    pub carries: Vec<String>,
    /// Overlay: the owning Program's image name.
    pub owner: Option<String>,
    /// Overlay: its load address.
    pub load_addr: u32,
    /// Overlay: how it is kept.
    pub class: Option<Class>,
    /// Where its file goes on the disc.
    pub placement: Placement,
    /// Overlay: the first sectors of the file it is placed before (`WORLD.PAK`, or a further file)
    /// that its row also loads, in the same read, right after
    /// its own sector-padded bytes (a title menu and the pack header its owner reads next). Its
    /// size and checksum cover them, so the core checks them with the overlay and its owner takes
    /// them from RAM (`psx_core::link::image_tail`) instead of reading them after a stop. Only the
    /// image that ends where that file starts, with nothing between (no `CDTEST.BIN` before
    /// `WORLD.PAK`); the owner's reserve holds them ([`SceneDiscError::Tail`] otherwise).
    pub tail: u32,
    /// Overlay: it runs over its owner's host buffer (psoxide-overlay.ld's `.bss.overlay_host`,
    /// which starts at `.data`'s end, over the owner's boot-only store and stored core). The
    /// owner's live span then ends at this overlay's load address instead of at its stored core.
    /// Only a hosted overlay may lie in the owner's stored bytes.
    pub hosted: bool,
}

/// Where an image's file goes on a scene disc.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_enums,
    reason = "the disc builders match every variant to report it"
)]
pub enum Placement {
    /// In image order: contiguous with the boot exe while it fits the boot area, else after the
    /// packs.
    #[default]
    InOrder,
    /// At the end of the boot area, right before `WORLD.PAK` (LBA 1024): for an image whose
    /// owner reads the pack's header right after it, so the two reads cost one seek and a
    /// short hop, not two long seeks (Half-Life's title menu overlay, read before the menu art).
    /// Images placed so keep their image order among themselves; the boot area must hold them.
    BeforeWorld,
    /// Right before the further root file of this name (`add_scene_disc_files_with`, mkisopsx's
    /// `--file`): for an image whose owner reads that file next (360X's front end before its
    /// game data). Images placed before one file keep their image order; the last one ends where
    /// the file starts.
    Before(String),
}

impl ImageSpec {
    /// A Program image; `file` `None` stands for the boot exe.
    #[must_use]
    pub fn program(name: &str, file: Option<Vec<u8>>) -> Self {
        Self {
            name: name.to_ascii_uppercase(),
            kind: Kind::Program,
            file,
            core_hash: None,
            reserve: None,
            reaches: None,
            carries: Vec::new(),
            owner: None,
            load_addr: 0,
            class: None,
            placement: Placement::InOrder,
            tail: 0,
            hosted: false,
        }
    }

    /// An Overlay image owned by the Program named `owner`.
    #[must_use]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "plain data the disc builders fill in by literal"
)]
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
    /// Images the scene needs in RAM on entry, by name: Overlays of the scene's own Program. The
    /// core loads each one whose loaded bit is clear before the scene runs (an in-program scene),
    /// or when the Program asks on its start (`psx_core::link::require`).
    pub needs: Vec<String>,
}

impl SceneSpec {
    /// The scene that starts the Program `image`.
    #[must_use]
    pub fn start(name: &str, image: &str) -> Self {
        Self {
            name: name.into(),
            image: image.to_ascii_uppercase(),
            start: true,
            entry: None,
            flags: 0,
            partner_credit: 0,
            min_block_version: block::VERSION as u8,
            needs: Vec::new(),
        }
    }

    /// A scene entered at `entry` inside `image` (a running Program, or an Overlay).
    #[must_use]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "plain data the disc builders fill in by literal"
)]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "plain data the disc builders fill in by literal"
)]
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
#[allow(
    clippy::exhaustive_structs,
    reason = "plain data the disc builders fill in by literal"
)]
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
#[allow(
    clippy::exhaustive_enums,
    reason = "the disc builders match every variant to report it"
)]
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
    /// An Overlay's owner is not a Program, it has no class, or a Program carries Overlay fields.
    Overlay(String),
    /// An Overlay's owner has no reserve: add `reserve=<lo>-<hi>` to the owner's `program` line
    /// (the RAM its overlays load into; for an overlay linked by psoxide-overlay.ld, a range
    /// holding OVERLAY_VMA and the overlay's size).
    NoReserve {
        /// The Overlay.
        overlay: String,
        /// Its owner, which needs `reserve=`.
        owner: String,
    },
    /// An image placed before `WORLD.PAK` ([`Placement::BeforeWorld`]) does not fit what the
    /// boot area has left after the boot exe.
    BeforeWorld(String),
    /// An image has a tail ([`ImageSpec::tail`]) but is not the Overlay whose file ends where the
    /// next file (`WORLD.PAK` or a [`Placement::Before`] file) starts, that file is shorter than
    /// the tail, or `CDTEST.BIN` lies between the image and `WORLD.PAK`.
    Tail(String),
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
    /// A Program's `carries` names an image that is not one of its own Overlays, a VOLATILE one,
    /// or one whose bytes its payload does not hold at the overlay's load address.
    Carried {
        /// The Program.
        program: String,
        /// The Overlay it says it carries.
        overlay: String,
    },
    /// A scene's `needs` names an image that is not an Overlay of the scene's own Program, two
    /// of its needed images overlap, or one overlaps the scene's own Overlay (the core loads the
    /// scene's image first, then each needed one, so none may evict another).
    Needs {
        /// The scene.
        scene: String,
        /// The needed image at fault.
        image: String,
    },
    /// A start scene needs images but its Program never asks for them: its EXE does not link
    /// `psx_core::link::require` (no `psx_scene_abi::REQUIRE_MARKER` in its bytes).
    NoRequire {
        /// The start scene.
        scene: String,
        /// Its Program.
        program: String,
    },
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
#[must_use]
pub fn payload_before_core(payload: &[u8], core: &[u8]) -> Option<usize> {
    core_store_offset(payload, core).map(|at| at.next_multiple_of(SECTOR_BYTES))
}

/// Where a program's stored copy of `core` starts in its payload, unrounded: the end of the bytes
/// it may still use once it runs (`.text`, `.data`, a boot-only store). What follows up to the
/// sector edge is loaded by a switch but dead once the program runs (its `.bss`, or an overlay
/// host buffer, psoxide-overlay.ld). `None` as for [`payload_before_core`].
fn core_store_offset(payload: &[u8], core: &[u8]) -> Option<usize> {
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
        .then_some(at)
}

/// The image row of a Program whose exe as pressed is `bytes`, its header sector at
/// member-relative `lba`, plus the exe's entry PC. `core` is the plan's core image: its word FNV
/// is the row's core hash (unless `core_hash` names one), and the load size ends before the
/// exe's stored copy of it. `member_base` and `cdda_delta` are zero; the demo disc's mkdisc sets
/// them for each member's Programs before `psx_scene_abi::encode_table`.
///
/// # Errors
///
/// A [`SceneDiscError`] naming the image, scene or placement the disc cannot hold.
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
        // Reserved for a Program (only an Overlay names its owner); the core does not read it.
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
///
/// # Errors
///
/// A [`SceneDiscError`] naming the image, scene or placement the disc cannot hold.
pub fn add_scene_disc_files(
    builder: &mut IsoBuilder,
    exe_bytes: Vec<u8>,
    world_pack: Option<Vec<u8>>,
    ui_pack: Option<Vec<u8>>,
    cdtest_sectors: Option<usize>,
    plan: &ScenePlan,
) -> Result<SceneDisc, SceneDiscError> {
    add_scene_disc_files_with(
        builder,
        exe_bytes,
        world_pack,
        ui_pack,
        cdtest_sectors,
        plan,
        &[],
    )
}

/// [`add_scene_disc_files`] with further root files (`name`, bytes) after everything else, in
/// order (mkisopsx's `--file`): an image placed [`Placement::Before`] one of them goes right
/// before it.
///
/// # Errors
///
/// A [`SceneDiscError`] naming the image, scene or placement the disc cannot hold.
///
/// # Panics
///
/// Never: the plan's image count is checked before its boot image is read.
pub fn add_scene_disc_files_with(
    builder: &mut IsoBuilder,
    mut exe_bytes: Vec<u8>,
    world_pack: Option<Vec<u8>>,
    ui_pack: Option<Vec<u8>>,
    cdtest_sectors: Option<usize>,
    plan: &ScenePlan,
    extra_files: &[(String, Vec<u8>)],
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
    for (name, _) in extra_files {
        check_name(name, &mut seen)?;
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
    // Images placed right before the packs take the end of the boot area first.
    let before_world: Vec<usize> = (1..images.len())
        .filter(|&index| images[index].placement == Placement::BeforeWorld)
        .collect();
    for &index in &before_world {
        let sectors = sectors_of(files[index].len());
        if sectors > room {
            return Err(SceneDiscError::BeforeWorld(images[index].name.clone()));
        }
        room -= sectors;
    }
    // Images placed right before a further file, in image order: each file's list.
    let before_file = |name: &str| -> Vec<usize> {
        (1..images.len())
            .filter(|&index| matches!(&images[index].placement, Placement::Before(n) if n == name))
            .collect()
    };
    for image in images {
        if let Placement::Before(name) = &image.placement {
            if !extra_files.iter().any(|(n, _)| n == name) {
                return Err(SceneDiscError::Unknown(name.clone()));
            }
        }
    }
    // A tail is the first sectors of the file an image is placed right before, read on from
    // that image: only the image that ends where the file starts may have one, with nothing
    // between them (no CDTEST.BIN before WORLD.PAK).
    let mut tails: Vec<Vec<u8>> = vec![Vec::new(); images.len()];
    for (index, image) in images.iter().enumerate() {
        if image.tail == 0 {
            continue;
        }
        let bytes = image.tail as usize * SECTOR_BYTES;
        let next: Option<&[u8]> = match &image.placement {
            Placement::BeforeWorld if before_world.last() == Some(&index) => {
                world_pack.as_deref().filter(|_| cdtest_sectors.is_none())
            }
            Placement::Before(name) if before_file(name).last() == Some(&index) => extra_files
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, file)| file.as_slice()),
            _ => None,
        };
        match next {
            Some(file) if image.kind == Kind::Overlay && file.len() >= bytes => {
                tails[index] = file[..bytes].to_vec();
            }
            _ => return Err(SceneDiscError::Tail(image.name.clone())),
        }
    }
    let mut later = Vec::new();
    for index in 1..images.len() {
        if images[index].placement != Placement::InOrder {
            continue;
        }
        let sectors = sectors_of(files[index].len());
        if later.is_empty() && sectors <= room {
            builder.add_file(&images[index].name, files[index].clone());
            room -= sectors;
        } else {
            later.push(index);
        }
    }
    builder.add_padding_sectors(room);
    for &index in &before_world {
        builder.add_file(&images[index].name, files[index].clone());
    }
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
    for (name, bytes) in extra_files {
        for index in before_file(name) {
            builder.add_file(&images[index].name, files[index].clone());
        }
        builder.add_file(name, bytes.clone());
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
    let mut live_ends = vec![0u32; images.len()];
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
        // What the program still uses once it runs ends at its stored core, not at the sector
        // edge: its own overlays may load over the rest (psoxide-overlay.ld's host buffer).
        live_ends[index] = match (image.core_hash, core) {
            (None, Some(core)) => Exe::parse(&files[index])
                .ok()
                .and_then(|exe| core_store_offset(&exe.payload, core))
                .map_or(row.span().end, |at| row.load_addr + at as u32),
            _ => row.span().end,
        };
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
        let reserve = owner_spec
            .reserve
            .ok_or_else(|| SceneDiscError::NoReserve {
                overlay: image.name.clone(),
                owner: owner_spec.name.clone(),
            })?;
        // The tail, if any, is loaded and checked with the overlay's own bytes.
        let mut loaded = files[index].clone();
        loaded.extend_from_slice(&tails[index]);
        let size = loaded.len() as u32;
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
            checksum: word_fnv(&loaded),
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
    // The RAM each image holds while its owner runs: a Program's ends at its stored core.
    let spans: Vec<Span> = rows
        .iter()
        .zip(&live_ends)
        .map(|(row, &end)| match row.kind {
            Kind::Program => Span {
                start: row.load_addr,
                end,
            },
            Kind::Overlay => row.span(),
        })
        .collect();
    // A program whose overlays run over a host buffer (psoxide-overlay.ld; the manifest says
    // `host`) has the host at `__data_end`, over its boot-only store and its stored core, both
    // consumed before `_start` (the core's boot copies its image to the window, the mover copies
    // `.boot` to its run range). What it still uses once it runs then ends where its first hosted
    // overlay starts; the link refuses a host below `__data_end`, which the exe alone does not
    // show. Every other overlay is still checked against the whole span up to the stored core.
    let mut spans = spans;
    for (row, image) in rows.iter().zip(images) {
        if row.kind != Kind::Overlay || !image.hosted {
            continue;
        }
        let owner = row.owner as usize;
        let start = row.load_addr;
        if let Some(span) = spans.get_mut(owner) {
            if rows[owner].kind == Kind::Program && span.start < start && start < span.end {
                span.end = start;
            }
        }
    }

    // Carried overlays: a Program's row word 5 lists its own Overlays its exe already holds at
    // their load addresses, byte for byte, sector padding included: a reload from the disc writes
    // the padding too, so the payload may hold nothing else there.
    let mut carried_spans: Vec<Vec<Span>> = vec![Vec::new(); images.len()];
    let mut word5 = vec![0u32; images.len()];
    for (index, image) in images.iter().enumerate() {
        for name in &image.carries {
            let bad = || SceneDiscError::Carried {
                program: image.name.clone(),
                overlay: name.clone(),
            };
            let o = index_of(name)? as usize;
            let (program, overlay) = (&rows[index], &rows[o]);
            if image.kind != Kind::Program
                || overlay.kind != Kind::Overlay
                || overlay.owner as usize != index
                || overlay.class == Some(Class::Volatile)
                || images[o].tail != 0
            {
                return Err(bad());
            }
            let own = &files[o];
            let exe = Exe::parse(&files[index]).map_err(|_| bad())?;
            let at = overlay.load_addr.wrapping_sub(program.load_addr) as usize;
            let holds = overlay.load_addr >= program.load_addr
                && at + own.len() <= program.size as usize
                && exe.payload[at..at + own.len()] == *own;
            if !holds {
                return Err(bad());
            }
            word5[index] |= 1 << o;
            carried_spans[index].push(Span {
                start: overlay.load_addr,
                end: overlay.load_addr + own.len() as u32,
            });
        }
    }
    for (row, word) in rows.iter_mut().zip(word5) {
        row.evicts |= word;
    }
    // Where an Overlay may lie over its owner: only over the overlays the owner carries.
    let over_carried = |program: usize, overlay: Span| {
        let span = spans[program];
        let mut from = overlay.start.max(span.start);
        let to = overlay.end.min(span.end);
        while from < to {
            match carried_spans[program]
                .iter()
                .find(|c| c.start <= from && from < c.end)
            {
                Some(c) => from = c.end,
                None => return false,
            }
        }
        true
    };

    // Co-resident images never overlap: an Overlay and its owner, two RESIDENT_SETs of one owner.
    for (i, a) in rows.iter().enumerate() {
        for (j, b) in rows.iter().enumerate().skip(i + 1) {
            let co_resident = match (a.kind, b.kind) {
                (Kind::Program, Kind::Overlay) => {
                    b.owner as usize == i && !over_carried(i, spans[j])
                }
                (Kind::Overlay, Kind::Program) => {
                    a.owner as usize == j && !over_carried(j, spans[i])
                }
                (Kind::Overlay, Kind::Overlay) => {
                    a.owner == b.owner
                        && a.class == Some(Class::ResidentSet)
                        && b.class == Some(Class::ResidentSet)
                }
                (Kind::Program, Kind::Program) => false,
            };
            if co_resident && overlaps(spans[i], spans[j]) {
                return Err(SceneDiscError::Overlap {
                    a: images[i].name.clone(),
                    b: images[j].name.clone(),
                });
            }
        }
    }
    // An Overlay's evicts mask: exactly the other images whose ranges intersect its own, and
    // a VOLATILE one's own bit (the core applies the mask after setting the bit, so it is never
    // kept and every load reads the overlay again).
    for (i, row) in rows.iter_mut().enumerate() {
        if row.kind == Kind::Overlay {
            let own = if row.class == Some(Class::Volatile) {
                1 << i
            } else {
                0
            };
            // Never its owner: it may lie over the owner only where the owner carries overlays.
            let owner = row.owner as usize;
            row.evicts = (0..spans.len())
                .filter(|&j| j != i && j != owner && overlaps(spans[i], spans[j]))
                .fold(own, |mask, j| mask | 1 << j);
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
        // Required images: Overlays of the scene's own Program, loaded after the scene's own image,
        // so none may lie over that image or over another needed one.
        let program = if row.kind == Kind::Program {
            image
        } else {
            row.owner
        };
        let mut needs = 0u32;
        let mut needed: Vec<Span> = Vec::new();
        for name in &scene.needs {
            let bad = || SceneDiscError::Needs {
                scene: scene.name.clone(),
                image: name.clone(),
            };
            let n = index_of(name)?;
            let need = &rows[n as usize];
            let span = spans[n as usize];
            let clash = needed.iter().any(|&s| overlaps(s, span))
                || (row.kind == Kind::Overlay
                    && n != image
                    && overlaps(spans[image as usize], span));
            if need.kind != Kind::Overlay || need.owner != program || clash {
                return Err(bad());
            }
            needed.push(span);
            needs |= 1 << n;
        }
        // A program asks for its start scene's images itself (`psx_core::link::require`, which keeps
        // REQUIRE_MARKER in its .rodata): one that never links the call runs without them.
        if scene.start
            && needs != 0
            && !files[image as usize]
                .windows(REQUIRE_MARKER.len())
                .any(|w| w == REQUIRE_MARKER)
        {
            return Err(SceneDiscError::NoRequire {
                scene: scene.name.clone(),
                program: images[image as usize].name.clone(),
            });
        }
        let start_flag = if scene.start { SCENE_FLAG_START } else { 0 };
        scene_rows.push(SceneRow {
            id: id as u16,
            image,
            flags: (scene.flags & !SCENE_FLAG_START) | start_flag,
            entry,
            partner_credit: scene.partner_credit,
            min_block_version: scene.min_block_version,
            needs,
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
