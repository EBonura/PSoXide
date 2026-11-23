//! A scene disc: the playtest layout plus `SCENES.TBL` and further program,
//! image and data files as named slots, one table row each.
//!
//! The table format is `psx-scene-abi` (CORE-ABI.md, the scene core's day-0
//! page); this module only places files and fills rows from them. mkisopsx
//! uses it for a single-game disc and the demo disc's mkdisc can use it for
//! each member image. **Provisional** with the page: P1 implements Program
//! rows only, and the other kinds' fields may still change.
//!
//! Layout, every LBA member-relative:
//!
//! ```text
//! 21  SYSTEM.CNF
//! 22  SCENES.TBL      one sector (31 rows)
//! 23  PSX.EXE         the boot program, the row the header names
//! ..  slot files      in table order while they fit the boot area,
//!                     contiguous with the boot exe (no seek from it)
//! ..  padding, optional CDTEST.BIN
//! 1024 WORLD.PAK, UI.PAK
//! ..  the slot files that did not fit, still in table order
//! ```
//!
//! The core image needs no slot: CORE-ABI.md 4.1 stores it inside every
//! program that hosts it, right after `.data`, so it is read with the boot
//! exe. A [`ScenePlan::core_image`] is the hash source the rows carry, and
//! each program row is checked to contain those exact bytes.

pub use psx_scene_abi as abi;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use psx_scene_abi::{
    block, encode_table, rows_capacity, word_fnv, Kind, Row, TableError, BOOT_EXE_LBA, CORE_WINDOW,
    LOAD_ADDR, NO_SCENE, PAYLOAD_END, SECTOR_BYTES, TABLE_FILE_NAME, TABLE_LBA,
};

use crate::{
    cd_stream_bench_payload, default_system_cnf, pad_to_payload, Exe, IsoBuilder,
    PlaytestLayoutError, CD_STREAM_BENCH_FILE_NAME, PLAYTEST_FIRST_FILE_LBA, UI_PACK_FILE_NAME,
    WORLD_PACK_DEFAULT_START_LBA, WORLD_PACK_FILE_NAME,
};

/// Name the boot exe must have: `SYSTEM.CNF` boots it.
pub const BOOT_EXE_NAME: &str = "PSX.EXE";

/// One row of the table and the file it describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneSlot {
    /// Root-directory name, also the row's name (upper-cased).
    pub name: String,
    /// What the row describes.
    pub kind: Kind,
    /// The file's bytes; `None` for the boot exe passed to
    /// [`add_scene_disc_files`] itself, which must be named [`BOOT_EXE_NAME`].
    pub file: Option<Vec<u8>>,
    /// Row flags (`psx_scene_abi::ROW_FLAG_PARTNER_CREDIT`).
    pub flags: u8,
    /// Return scene id, `NO_SCENE` for none.
    pub return_scene: u16,
    /// Read attempts; 0 means 3.
    pub read_attempts: u8,
    /// Minimum handoff block version the program needs.
    pub min_block_version: u8,
    /// Partner credit id, 0 none.
    pub partner_credit: u16,
    /// The core hash this row carries instead of the plan's core image's.
    pub core_hash: Option<u32>,
}

impl SceneSlot {
    /// A slot with the defaults: no flags, no return scene, three read
    /// attempts, the current block version, no partner credit.
    pub fn new(name: &str, kind: Kind, file: Option<Vec<u8>>) -> Self {
        Self {
            name: name.to_ascii_uppercase(),
            kind,
            file,
            flags: 0,
            return_scene: NO_SCENE,
            read_attempts: 0,
            min_block_version: block::VERSION as u8,
            partner_credit: 0,
            core_hash: None,
        }
    }
}

/// Every scene of a disc, in table order (the row index is the scene id).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScenePlan {
    /// The rows, boot exe included.
    pub slots: Vec<SceneSlot>,
    /// The prebuilt core image (`psx-core.bin`): its word FNV is the core
    /// hash program rows carry, and every program row must contain it.
    pub core_image: Option<Vec<u8>>,
}

/// What [`add_scene_disc_files`] wrote: the rows as pressed and the table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneDisc {
    /// The rows, in scene id order.
    pub rows: Vec<Row>,
    /// The boot exe's scene id.
    pub boot_scene: u16,
    /// `SCENES.TBL`'s bytes, a whole sector.
    pub table: Vec<u8>,
}

/// Why a scene disc could not be laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneDiscError {
    /// The boot area did not fit (the boot exe starts at LBA 23 here).
    Layout(PlaytestLayoutError),
    /// No slot, or more than the one-sector table holds.
    RowCount(usize),
    /// No slot stands for the boot exe, or more than one does.
    BootSlots(usize),
    /// The boot slot is not named [`BOOT_EXE_NAME`].
    BootName(String),
    /// A name is empty, too long for a row, or not ASCII.
    BadName(String),
    /// Two files would have one name (fixed layout names included).
    DuplicateName(String),
    /// An executable kind's file is not a PS-X EXE.
    NotAnExe(String),
    /// A program's `t_size` is not whole sectors.
    TSize(String),
    /// A program would load outside `0x80010000..PAYLOAD_END` (the switch
    /// stack and the core window above it).
    Bounds {
        /// The row's name.
        name: String,
        /// Its load address.
        t_addr: u32,
        /// Its stored size.
        t_size: u32,
    },
    /// The core image is empty, not whole words, or larger than the window.
    CoreImage(usize),
    /// A program row does not contain the core image's bytes.
    CoreMissing(String),
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

fn is_exe_kind(kind: Kind) -> bool {
    !matches!(kind, Kind::Data | Kind::Module)
}

/// True when `payload` holds `image` at a word-aligned offset.
fn contains_words(payload: &[u8], image: &[u8]) -> bool {
    payload.len() >= image.len()
        && (0..=payload.len() - image.len())
            .step_by(4)
            .any(|at| payload[at..at + image.len()] == *image)
}

/// The playtest layout of [`crate::add_playtest_files`] with `SCENES.TBL` at
/// LBA 22, the boot exe at 23 and the plan's slot files placed as the module
/// comment shows. `WORLD.PAK` stays at LBA 1024, so cooked pack LBAs do not
/// move; the boot exe has one sector less room than without a table.
///
/// Rows are filled from the files as pressed (LBA, sectors and the PS-X EXE
/// header fields; the payload checksum is the word FNV over `t_size` bytes)
/// and the table is written into its sector last.
pub fn add_scene_disc_files(
    builder: &mut IsoBuilder,
    mut exe_bytes: Vec<u8>,
    world_pack: Option<Vec<u8>>,
    ui_pack: Option<Vec<u8>>,
    cdtest_sectors: Option<usize>,
    plan: &ScenePlan,
) -> Result<SceneDisc, SceneDiscError> {
    let slots = &plan.slots;
    if slots.is_empty() || slots.len() > rows_capacity(1) {
        return Err(SceneDiscError::RowCount(slots.len()));
    }
    let boots: Vec<usize> = (0..slots.len())
        .filter(|&i| slots[i].file.is_none())
        .collect();
    let &[boot] = boots.as_slice() else {
        return Err(SceneDiscError::BootSlots(boots.len()));
    };
    if slots[boot].name != BOOT_EXE_NAME {
        return Err(SceneDiscError::BootName(slots[boot].name.clone()));
    }
    let mut seen: Vec<&str> = RESERVED_NAMES.to_vec();
    for slot in slots {
        let name = slot.name.as_str();
        if name.is_empty() || name.len() > psx_scene_abi::NAME_BYTES || !name.is_ascii() {
            return Err(SceneDiscError::BadName(slot.name.clone()));
        }
        if seen.contains(&name) {
            return Err(SceneDiscError::DuplicateName(slot.name.clone()));
        }
        seen.push(name);
    }
    let core_hash = match &plan.core_image {
        Some(image) => {
            if image.is_empty()
                || !image.len().is_multiple_of(4)
                || image.len() > CORE_WINDOW.len() as usize
            {
                return Err(SceneDiscError::CoreImage(image.len()));
            }
            Some(word_fnv(image))
        }
        None => None,
    };

    // Every file as it will be pressed (executables padded to their t_size).
    let _ = pad_to_payload(&mut exe_bytes);
    let mut files: Vec<Vec<u8>> = Vec::with_capacity(slots.len());
    for slot in slots {
        let mut bytes = match &slot.file {
            Some(bytes) => bytes.clone(),
            None => exe_bytes.clone(),
        };
        if is_exe_kind(slot.kind) && pad_to_payload(&mut bytes).is_err() {
            return Err(SceneDiscError::NotAnExe(slot.name.clone()));
        }
        files.push(bytes);
    }

    // The boot area, as add_playtest_files checks it, one sector shorter.
    let exe_sectors = sectors_of(files[boot].len());
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
    builder.add_file("SYSTEM.CNF", default_system_cnf());
    builder.add_file(TABLE_FILE_NAME, vec![0u8; SECTOR_BYTES]);
    builder.add_file(BOOT_EXE_NAME, files[boot].clone());
    // Slots in table order: contiguous with the boot exe while they fit, the
    // rest after the packs, so a boot that runs the scenes in order reads on.
    let mut room = cdtest_start - next_after_exe;
    let mut later = Vec::new();
    for (index, slot) in slots.iter().enumerate() {
        if index == boot {
            continue;
        }
        let sectors = sectors_of(files[index].len());
        if later.is_empty() && sectors <= room {
            builder.add_file(&slot.name, files[index].clone());
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
        builder.add_file(&slots[index].name, files[index].clone());
    }

    let extents = builder.file_extents();
    let mut rows = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().enumerate() {
        let (_, lba, sectors) = extents
            .iter()
            .find(|(name, _, _)| *name == slot.name)
            .cloned()
            .expect("every slot was added");
        let mut row = Row {
            id: index as u16,
            kind: slot.kind,
            flags: slot.flags,
            lba,
            sectors,
            t_addr: 0,
            t_size: 0,
            pc: 0,
            checksum: 0,
            core_hash: 0,
            sp: 0,
            return_scene: slot.return_scene,
            read_attempts: slot.read_attempts,
            min_block_version: slot.min_block_version,
            partner_credit: slot.partner_credit,
            name: Row::name_bytes(&slot.name),
        };
        let bytes = &files[index];
        if is_exe_kind(slot.kind) {
            let exe = Exe::parse(bytes).map_err(|_| SceneDiscError::NotAnExe(slot.name.clone()))?;
            let t_size = exe.payload.len() as u32;
            if !t_size.is_multiple_of(SECTOR_BYTES as u32) {
                return Err(SceneDiscError::TSize(slot.name.clone()));
            }
            let in_bounds = exe.load_addr >= LOAD_ADDR
                && exe
                    .load_addr
                    .checked_add(t_size)
                    .is_some_and(|end| end <= PAYLOAD_END);
            if !in_bounds {
                return Err(SceneDiscError::Bounds {
                    name: slot.name.clone(),
                    t_addr: exe.load_addr,
                    t_size,
                });
            }
            if slot.core_hash.is_none() {
                if let Some(image) = &plan.core_image {
                    if !contains_words(&exe.payload, image) {
                        return Err(SceneDiscError::CoreMissing(slot.name.clone()));
                    }
                }
            }
            row.t_addr = exe.load_addr;
            row.t_size = t_size;
            row.pc = exe.initial_pc;
            row.sp = exe.initial_sp().unwrap_or(0);
            row.checksum = word_fnv(&exe.payload);
            row.core_hash = slot.core_hash.or(core_hash).unwrap_or(0);
        } else {
            // Data: the whole sectors as pressed, zero-filled past the file.
            let mut padded = bytes.clone();
            padded.resize(sectors as usize * SECTOR_BYTES, 0);
            row.t_size = padded.len() as u32;
            row.checksum = word_fnv(&padded);
            row.core_hash = slot.core_hash.unwrap_or(0);
        }
        rows.push(row);
    }

    let mut table = vec![0u8; SECTOR_BYTES];
    encode_table(&rows, boot as u16, &mut table).map_err(SceneDiscError::Table)?;
    let replaced = builder.set_file_content(TABLE_FILE_NAME, table.clone());
    debug_assert!(replaced);
    Ok(SceneDisc {
        rows,
        boot_scene: boot as u16,
        table,
    })
}

#[cfg(test)]
mod tests;
