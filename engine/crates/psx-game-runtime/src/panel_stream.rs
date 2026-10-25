//! Keeping the far panels' tiles in VRAM while the player walks.
//!
//! A far panel ([`psx_grid::render::panels`]) is the level beyond the rooms drawn whole, seen
//! from a bake eye of the lattice along one of eight headings, cut into tiles of one atlas slot
//! each. This module decides which bake eye serves the camera, which tiles of its panels the
//! camera can see, and reads the missing ones off the disc through the asynchronous
//! transport the rooms stream through ([`RoomTransport`]).
//!
//! * **The eye.** The nearest bake eye in 3D ([`octagonal_distance`]), kept until another is
//!   nearer by a sixth of the lattice's spacing.
//! * **The block.** Everything an eye's panels need sits in one block of the disc
//!   ([`psx_grid::eye_block`]): an index (which tiles each heading stores, every panel's depth
//!   mesh and room cells, the length of each tile) and then the tiles as LZ blocks
//!   ([`psx_grid::tile_lz`]), cut into groups of [`GROUP_SECTORS`] sectors that no tile
//!   crosses. A read brings in whole groups.
//! * **The tiles.** For each heading whose panel can be in the view, the tile columns the
//!   field of view (and a margin to turn into) crosses and the rows the pitch looks at; those
//!   tiles that are stored take a slot, the ones nearest the middle of the view ranking first.
//!   Every panel of the eye is kept, whichever way the camera looks.
//! * **A change of eye.** The tiles of the eye left are kept, and drawn where the new eye's
//!   tile of the same place is stored and not in VRAM yet, until it lands: the picture is
//!   never blank while the new one comes. They are the first to give up their slots.
//! * **Reads.** The window the blocks land in is [`STAGES`] stages of one group. A read of an
//!   eye's first group brings in its index and the tiles after it; when both stages are free
//!   it takes the second group too, one seek for the average eye. A landed group stays in its
//!   stage a while ([`WARM_TICKS`]): the tiles the next pass wants that lie in it are taken
//!   from there without another read. A landed tile is decoded by
//!   [`write_tile_uploads`] straight into GPU list nodes, so the GPU does the copy as it draws
//!   and the CPU never waits.
//!
//! A failed read frees its stage and the tile is asked for again at the next pass. Nothing
//! here waits.

use psx_grid::eye_block::{BlockIndex, GROUP_BYTES, GROUP_SECTORS, NO_PANEL, SECTOR};
use psx_grid::flood::octagonal_distance;
use psx_grid::render::{
    PanelRef, CELLS, CELL_EMPTY, COLS, HEADINGS, HORIZON_ROW, MESH, NO_TILE_SLOT, PICTURE_ROWS,
    ROWS, SLOTS as ATLAS_SLOTS, TILES, TILE_TEXELS, VIEW_COS_Q12, VIEW_SIN_Q12,
};
use psx_grid::tile_lz::{decode_tile_slack, OUT_SLACK, TILE_BYTES};

use crate::grid_stream::{Poll, RoomTransport};

/// Slots of the stream: the sky atlas's, and a second atlas's when there is one.
const SLOTS: usize = ATLAS_SLOTS + psx_grid::render::SECOND_ATLAS_SLOTS;
/// Groups the window holds at once: reads in flight, or landed and waiting for the GPU.
pub const STAGES: usize = 2;
/// Words of the window the reads land in.
pub const WINDOW_WORDS: usize = STAGES * GROUP_BYTES / 4;
/// Entries a read keeps: the tiles it brought in that wait for the GPU, and an eye's index.
const ENTRIES: usize = 32;
/// Ticks a landed group stays in its stage with nothing to give: the pass that the index
/// forces runs in the tick it lands, so its tiles are attached by the end of that tick.
pub const WARM_TICKS: u32 = 1;
/// Bytes of a sector, as the index counts offsets.
const SECTOR_BYTES: u32 = SECTOR as u32;
/// Ticks between two passes.
const PASS_TICKS: u32 = 8;
/// Rank added to a tile of a panel the camera is not looking at.
const OFF_VIEW: i32 = 1 << 14;
/// Measurement: count the tiles the screen shows that have no picture, and how long tiles
/// wait (the counters are read from RAM; it costs code and time, so a shipping build has it off).
const METER: bool = false;
/// Panels whose header and tile slots are kept.
pub const CACHE: usize = 3 * HEADINGS;
/// Rank of a tile of an eye the camera has left, past every tile of the eye it is at.
const STALE: i32 = 1 << 20;
/// Rank of a tile of the eye the camera is about to cross to, past every tile of the eye in
/// use.
const PREFETCH: i32 = 1 << 18;
/// Rank of a tile of an eye kept ready that nothing else could stand in for, past every tile
/// of the eye in use and ahead of the other tiles of the eyes kept ready.
const EXPOSED: i32 = PREFETCH / 4;
/// Rank of a held tile nothing wants now.
const IDLE: i32 = 1 << 22;
/// A held tile is only evicted for a tile that ranks better by more than this.
const EVICT_MARGIN: i32 = 64;
/// Ticks a wanted sector may wait before it is read ahead of the better ranked ones.
const PATIENCE: u32 = 90;
/// Tenths of a spacing from the eye in use the camera is before the second eye is kept ready.
const SECOND_FROM: i32 = 3;
/// The eye kept ready is kept while it is within this fraction (1 in) of a spacing of being as near
/// the camera as the nearest.
const NEXT_STICKS: i32 = 8;
/// Smallest smoothed step of the eye a pass, runtime units, that counts as walking.
const MIN_WALK: i32 = 3;
const NO_SLOT_INDEX: u8 = 0xff;
const NO_ENTRY: u8 = 0xff;
/// Eyes whose lists of stored tiles are kept: the one in use, the one the camera is about to
/// cross to, and the ones just left.
const EYES: usize = 4;

/// What a [`PanelStream`] has done, for telemetry and diagnosis.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct PanelCounters {
    /// Reads queued.
    pub submitted: u32,
    /// Reads that landed.
    pub landed: u32,
    /// Reads that failed or were cancelled.
    pub failed: u32,
    /// Tiles uploaded to VRAM.
    pub uploads: u32,
    /// Slots taken over from a worse ranked tile.
    pub evictions: u32,
    /// Submits the transport refused (its queue full).
    pub refused: u32,
    /// Slots in use at the last pass.
    pub held: u32,
    /// Tiles wanted at the last pass.
    pub wanted: u32,
    /// Of those, the ones not in VRAM yet.
    pub missing: u32,
    /// Wanted tiles that found no slot.
    pub starved: u32,
    /// Index of the bake eye in use.
    pub eye: u32,
    /// First sector (from the pictures' base) of the last read queued.
    pub last_first: u32,
    /// Sectors of the last read queued.
    pub last_sectors: u32,
    /// Sectors read in all.
    pub sectors: u32,
    /// With `METER` on: over every pass, the tiles the screen shows that the eye in use stores.
    pub visible: u32,
    /// With `METER` on: of those, the ones with no picture to draw (not even a stale one
    /// standing in).
    pub visible_missing: u32,
    /// With `METER` on: the passes with any such tile.
    pub passes_missing: u32,
    /// With `METER` on: tiles shown after a wait.
    pub shown: u32,
    /// With `METER` on: ticks those tiles waited from the pass that wanted them to the read
    /// being queued.
    pub wait_queue: u32,
    /// With `METER` on: ticks from the read being queued to the upload being in a frame's list.
    pub wait_load: u32,
    /// With `METER` on: of the queued-to-frame wait, the part until the read landed.
    pub wait_read: u32,
    /// Passes in a row the camera has had a nearer eye it did not cross to. The camera crosses
    /// as soon as an eye is nearer, so this stays 0; a replay checks that it does.
    pub gate_run: u32,
    /// How far the camera is from the eye in use, in tenths of a spacing between eyes.
    pub far: u32,
    /// Passes in a row since the camera crossed to another eye in which a picture of the eye in
    /// use was still on its way (the eye left stands in for it): how long a crossing takes to
    /// settle.
    pub settling: u32,
    /// Tiles whose LZ block did not decode (a damaged disc); they are drawn blank.
    pub undecodable: u32,
    /// Which stages of the window hold a read (bit 0 the first, bit 1 the second), bit 2 when
    /// the one in the first landed.
    pub stage_mask: u32,
    /// Eye indexes asked for and not queued yet.
    pub indexes_waiting: u32,
    /// Landed reads of tiles for eyes the camera is not at yet that gave up their stage to a
    /// read the eye in use was waiting for.
    pub preempted: u32,
    /// Tiles of the panels in view of the eye in use that are not in VRAM (`u32::MAX` while its
    /// index is not in).
    pub view_missing: u32,
}

impl PanelCounters {
    /// All counters zero.
    pub const ZERO: Self = Self {
        submitted: 0,
        landed: 0,
        failed: 0,
        uploads: 0,
        evictions: 0,
        refused: 0,
        held: 0,
        wanted: 0,
        missing: 0,
        starved: 0,
        eye: 0,
        last_first: 0,
        last_sectors: 0,
        sectors: 0,
        visible: 0,
        visible_missing: 0,
        passes_missing: 0,
        shown: 0,
        wait_queue: 0,
        wait_load: 0,
        wait_read: 0,
        gate_run: 0,
        far: 0,
        settling: 0,
        undecodable: 0,
        stage_mask: 0,
        indexes_waiting: 0,
        preempted: 0,
        view_missing: 0,
    };
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum SlotState {
    Free,
    /// Holds a tile whose read is not queued yet.
    Wanted,
    /// The read is in flight or landed and waiting for the GPU.
    Pending,
    /// VRAM holds the tile.
    Shown,
}

#[derive(Copy, Clone)]
struct Slot {
    state: SlotState,
    eye: u16,
    heading: u8,
    tile: u8,
    /// Index into the cache of the tile's panel.
    cache: u8,
    /// A pass found the tile in the view of the eye in use, or of the eye the camera is
    /// about to be nearest to (`prefetch`).
    wanted: bool,
    /// Wanted only to be there when the camera crosses to the next eye.
    prefetch: bool,
    rank: i32,
    /// Tick the tile started waiting at (0 when it is not).
    since: u32,
}

impl Slot {
    const FREE: Self = Self {
        state: SlotState::Free,
        eye: u16::MAX,
        heading: 0,
        tile: 0,
        cache: NO_ENTRY,
        wanted: false,
        prefetch: false,
        rank: i32::MAX,
        since: 0,
    };
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Header {
    /// Not asked for.
    Absent,
    /// Asked for, the read not queued yet.
    Wanted,
    /// The read is in flight.
    Pending,
    /// The depth mesh is in.
    Ready,
}

/// What an eye's block says of its panels: the index at its start, kept (the depth meshes and
/// room cells of its panels too, for the cache to take), and so where each tile is.
#[derive(Copy, Clone)]
struct EyeList {
    eye: u16,
    state: Header,
    used: u32,
    since: u32,
    index: BlockIndex,
    /// The eye's tiles are in VRAM (or on their way) from an earlier time: only the index is
    /// read, one sector.
    only_index: bool,
    /// Per heading, the panel's depth mesh as the index holds it.
    depth: [[u8; MESH]; HEADINGS],
    /// Per heading, the panel's room cells.
    rooms: [[u8; CELLS]; HEADINGS],
}

impl EyeList {
    const EMPTY: Self = Self {
        eye: u16::MAX,
        state: Header::Absent,
        used: 0,
        since: 0,
        index: BlockIndex::EMPTY,
        only_index: false,
        depth: [[0; MESH]; HEADINGS],
        rooms: [[CELL_EMPTY; CELLS]; HEADINGS],
    };

    /// Fill from the block's first sector, `bytes` giving its byte at an offset.
    fn parse(&mut self, bytes: impl Fn(usize) -> u8) {
        self.index = BlockIndex::parse(&bytes);
        for heading in 0..HEADINGS {
            let Some(at) = self.index.panel_at(heading) else {
                continue;
            };
            for (k, d) in self.depth[heading].iter_mut().enumerate() {
                *d = bytes(at + k);
            }
            for (k, r) in self.rooms[heading].iter_mut().enumerate() {
                *r = bytes(at + MESH + k);
            }
        }
        self.state = Header::Ready;
    }

    /// Whether `tile` of `heading` is stored (the index is in).
    fn stored(&self, heading: usize, tile: usize) -> bool {
        self.state == Header::Ready && self.index.stored(heading, tile)
    }

    /// The group of the block `tile` of `heading` lies in.
    fn group_of(&self, heading: usize, tile: usize) -> u16 {
        (self.index.offset[heading][tile] / GROUP_BYTES as u32) as u16
    }
}

/// A panel the stream keeps: its depth mesh and which slot holds which of its tiles.
#[derive(Copy, Clone)]
struct Cached {
    eye: u16,
    heading: u8,
    header: Header,
    /// Rank of the best tile of the panel the last pass wanted.
    rank: i32,
    used: u32,
    since: u32,
    depth: [u8; MESH],
    /// Per cell, the room whose geometry is all it shows (see [`PanelRef::rooms`]).
    rooms: [u8; CELLS],
    /// Per tile, the slot that holds it (any state), or [`NO_SLOT_INDEX`].
    held: [u8; TILES],
    /// Per tile, the slot to draw it from this frame, or [`NO_TILE_SLOT`].
    show: [u8; TILES],
}

impl Cached {
    const EMPTY: Self = Self {
        eye: u16::MAX,
        heading: 0,
        header: Header::Absent,
        rank: i32::MAX,
        used: 0,
        since: 0,
        depth: [0; MESH],
        rooms: [CELL_EMPTY; CELLS],
        held: [NO_SLOT_INDEX; TILES],
        show: [NO_TILE_SLOT; TILES],
    };
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    /// An eye's index.
    Eye,
    /// A tile.
    Tile,
}

/// What a read holds that something waits for.
#[derive(Copy, Clone)]
struct Entry {
    kind: Kind,
    eye: u16,
    heading: u8,
    tile: u8,
    slot: u8,
    cache: u8,
    /// Byte of the tile's LZ block in the read, from the start of its first stage.
    at: u16,
    /// Bytes of the block.
    len: u16,
}

/// One read: whole groups of one eye's block landing in consecutive stages.
#[derive(Copy, Clone)]
struct Run<K> {
    ticket: K,
    landed: bool,
    /// The group stays in its stage a while after the tiles it was read for are gone, for the
    /// tiles a pass will want from it (an eye's first read).
    warm: bool,
    eye: u16,
    /// The block's group in the first stage.
    group: u16,
    /// Groups read: 1, or 2 when the read fills both stages.
    groups: u8,
    /// Sectors of the groups the read brought in, from the first group's start: `lo..hi`. A
    /// read takes only the sectors the tiles wanted lie in (and what lies between them).
    lo: u8,
    hi: u8,
    /// Tick the read landed at.
    landed_at: u32,
    entries: [Option<Entry>; ENTRIES],
}

// The stages sit in a static array and a boxed read would be a heap block for every one.
#[allow(clippy::large_enum_variant)]
#[derive(Copy, Clone)]
enum Stage<K> {
    Free,
    Busy(Run<K>),
    /// Taken by the long read of the stage before.
    Spanned,
}

/// A landed tile ready for the GPU: where its LZ block sits in the window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TileUpload {
    /// Atlas slot to write.
    pub slot: u8,
    stage: u8,
    entry: u8,
    /// Byte of the block in the window.
    pub at: u32,
    /// Bytes of the block.
    pub len: u16,
}

/// Decode the tile `upload` names into `picture` (one atlas slot as words, the 63 rows and the
/// palette row). `false` when its LZ block does not decode; `picture` is then zero.
pub fn decode_upload(
    upload: &TileUpload,
    window: &[u32],
    picture: &mut crate::card_stream::Picture,
) -> bool {
    // SAFETY: a byte view of the window's words, which live as long as the borrow.
    let bytes =
        unsafe { core::slice::from_raw_parts(window.as_ptr().cast::<u8>(), window.len() * 4) };
    // SAFETY: the picture is `SECTOR_WORDS + 1` aligned words, which is `TILE_BYTES + OUT_SLACK`
    // bytes.
    let tile = unsafe { &mut *picture.as_mut_ptr().cast::<[u8; TILE_BYTES + OUT_SLACK]>() };
    let at = upload.at as usize;
    let decoded = bytes
        .get(at..at + usize::from(upload.len))
        .is_some_and(|block| decode_tile_slack(block, tile).is_ok());
    if !decoded {
        picture.fill(0);
    }
    decoded
}

/// Put the landed tiles in `uploads` into `out` as GPU list nodes (see
/// [`crate::card_stream::write_filled_packets`]), decoding each straight into the nodes' rows.
/// `window` is the memory the stream's reads landed in. Returns `(words, packets, undecodable)`.
pub fn write_tile_uploads(
    uploads: &[Option<TileUpload>],
    window: &[u32],
    origin: (u16, u16),
    origin2: (u16, u16),
    slot: u16,
    out: &mut [u32],
) -> (usize, usize, u32) {
    let mut slots = [0u8; 8];
    let mut picked = [None; 8];
    let mut count = 0;
    for upload in uploads.iter().flatten().take(slots.len()) {
        slots[count] = upload.slot;
        picked[count] = Some(*upload);
        count += 1;
    }
    let mut bad = 0;
    let (words, packets) = crate::card_stream::write_filled_packets(
        &slots[..count],
        origin,
        origin2,
        slot,
        out,
        |k, picture| {
            let ok = picked[k].is_some_and(|upload| decode_upload(&upload, window, picture));
            bad += u32::from(!ok);
        },
    );
    (words, packets, bad)
}

/// What a call to [`PanelStream::update`] knows about the level and the camera.
pub struct PanelInput<'a> {
    /// The bake eyes, runtime units.
    pub eyes: &'a [[i16; 3]],
    /// Per eye, the sector of its block (relative to `base_lba`), 0 for an eye with no panel.
    /// The block starts with a sector that lists which tiles each heading stores, then the
    /// eight panels, each its header sector and its stored tiles.
    pub blocks: &'a [u32],
    /// Per eye, the sectors its block takes.
    pub sectors: &'a [u16],
    /// Disc sector `blocks` is relative to.
    pub base_lba: u32,
    /// Slots the atlases hold: that of the sky's, or twice that with a second atlas.
    pub slots: usize,
    /// The camera, world runtime units.
    pub eye: [i32; 3],
    /// The camera's forward direction in plan, unit length as Q12.
    pub forward: [i32; 2],
    /// `tan` of the camera's pitch (up positive) as Q12.
    pub pitch_tan_q12: i32,
    /// The camera's projection plane, pixels.
    pub focal: i32,
    /// Half the screen, pixels, as `[width, height]`.
    pub half_screen: [i32; 2],
    /// The projection plane the panels were baked for.
    pub panel_focal: i32,
    /// Runtime units between lattice eyes.
    pub spacing: i32,
}

/// The tiles of one heading the camera can see.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Visible {
    /// First tile column of the view.
    pub first_col: usize,
    /// Last tile column of the view.
    pub last_col: usize,
    /// First tile row of the view.
    pub first_row: usize,
    /// Last tile row of the view.
    pub last_row: usize,
    /// `tan` of the camera's yaw off the heading, Q12.
    pub tan_yaw: i32,
    /// Texel row of the middle of the view.
    pub middle_row: i32,
}

/// The columns and rows of the panel of `heading` the camera looks at, with a margin of a few
/// degrees to turn into; `None` when the panel is wholly outside the view.
pub fn visible(heading: usize, input: &PanelInput<'_>) -> Option<Visible> {
    visible_in(heading, &input.view(), 18, PICTURE_ROWS / 3)
}

/// What the camera looks like to the tile choice: the part of [`PanelInput`] that is about the
/// view.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewSpec {
    /// The camera's forward direction in plan, unit length as Q12.
    pub forward: [i32; 2],
    /// `tan` of the camera's pitch (up positive) as Q12.
    pub pitch_tan_q12: i32,
    /// The camera's projection plane, pixels.
    pub focal: i32,
    /// Half the screen, pixels, as `[width, height]`.
    pub half_screen: [i32; 2],
    /// The projection plane the panels were baked for.
    pub panel_focal: i32,
}

impl PanelInput<'_> {
    /// The view part of the input.
    pub fn view(&self) -> ViewSpec {
        ViewSpec {
            forward: self.forward,
            pitch_tan_q12: self.pitch_tan_q12,
            focal: self.focal,
            half_screen: self.half_screen,
            panel_focal: self.panel_focal,
        }
    }
}

/// [`visible`] for a view, with the field of view widened by `widen / 16` (16 is none) and
/// `row_margin` texel rows kept above and below it.
pub fn visible_in(
    heading: usize,
    input: &ViewSpec,
    widen: i32,
    row_margin: i32,
) -> Option<Visible> {
    let h = heading % HEADINGS;
    let (c, s) = (i32::from(VIEW_COS_Q12[h]), i32::from(VIEW_SIN_Q12[h]));
    let (fx, fz) = (input.forward[0], input.forward[1]);
    let cos = (fx * c + fz * s) >> 12;
    let sin = (-fx * s + fz * c) >> 12;
    // More than 60 degrees off: the panel (27.5 degrees either side) cannot be in a view of
    // 28 degrees either side.
    if cos < 2048 {
        return None;
    }
    let tan_yaw = (sin << 12) / cos;
    // `tan` of half the field of view, a quarter more for the margin.
    let half = ((input.half_screen[0] << 12) / input.focal.max(1)) * widen / 16;
    let numerator_hi = (tan_yaw + half) << 12;
    let denominator_hi = 4096 - ((tan_yaw * half) >> 12);
    let high = if denominator_hi > 0 {
        numerator_hi / denominator_hi
    } else {
        i32::MAX / 4
    };
    let numerator_lo = (tan_yaw - half) << 12;
    let denominator_lo = 4096 + ((tan_yaw * half) >> 12);
    let low = if denominator_lo > 0 {
        numerator_lo / denominator_lo
    } else {
        i32::MIN / 4
    };
    // Past 23 degrees off its heading a panel's picture belongs to the next one's: the
    // columns wholly beyond are left to it.
    let (low, high) = (low.max(-PANEL_EDGE), high.min(PANEL_EDGE));
    if low >= high {
        return None;
    }
    let focal = input.panel_focal.max(1);
    let column_tan = |texel: i32| ((texel - COLS_CENTRE) << 12) / focal;
    let (mut first_col, mut last_col) = (usize::MAX, 0usize);
    for col in 0..COLS {
        let left = column_tan(col as i32 * TILE_TEXELS);
        let right = column_tan((col as i32 + 1) * TILE_TEXELS);
        if left < high && right > low {
            first_col = first_col.min(col);
            last_col = col;
        }
    }
    if first_col == usize::MAX {
        return None;
    }
    // The rows: the panel's texel row at the middle of the view, and half the screen's height
    // in panel texels, with a row of margin.
    let middle_row = HORIZON_ROW - ((focal * input.pitch_tan_q12) >> 12);
    let reach = input.half_screen[1] * focal / input.focal.max(1) + row_margin;
    let first_row = ((middle_row - reach).max(0) / PICTURE_ROWS) as usize;
    let last_row =
        (((middle_row + reach).min(ROWS as i32 * PICTURE_ROWS - 1)).max(0) / PICTURE_ROWS) as usize;
    if middle_row + reach < 0 || middle_row - reach >= ROWS as i32 * PICTURE_ROWS {
        return None;
    }
    Some(Visible {
        first_col,
        last_col,
        first_row: first_row.min(ROWS - 1),
        last_row: last_row.min(ROWS - 1),
        tan_yaw,
        middle_row,
    })
}

/// `tan` of the angle off its heading (23 degrees) a panel is used up to, Q12.
const PANEL_EDGE: i32 = 1760;
/// Texel column of the principal point.
const COLS_CENTRE: i32 = COLS as i32 * TILE_TEXELS / 2;

/// The sectors a tile's block lies in, `from..to`, counted from the start of group `group` of its
/// eye's block.
fn tile_sectors(index: &BlockIndex, heading: usize, tile: usize, group: u16) -> (u8, u8) {
    let offset = index.offset[heading][tile];
    let len = u32::from(index.len[heading][tile]);
    let base = u32::from(group) * GROUP_SECTORS as u32;
    let from = offset / SECTOR_BYTES - base;
    let to = (offset + len - 1) / SECTOR_BYTES + 1 - base;
    (from as u8, to as u8)
}

/// Whether the screen shows the tile at `col`, `row` of a panel `view` sees.
fn tile_in_view(view: &Visible, col: usize, row: usize) -> bool {
    (view.first_col..=view.last_col).contains(&col)
        && (view.first_row..=view.last_row).contains(&row)
}

/// What a pass works out about the camera once for every eye it asks tiles of.
struct View {
    /// Per heading, the tiles the camera sees of its panel.
    seen: [Option<Visible>; HEADINGS],
    /// Per tile column, `tan` of its middle's angle off the panel's heading, Q12.
    middle: [i32; COLS],
}

/// Streams the tiles the panels show.
pub struct PanelStream<K> {
    /// What the stream has done so far.
    pub counters: PanelCounters,
    slots: [Slot; SLOTS],
    cache: [Cached; CACHE],
    lists: [EyeList; EYES],
    stages: [Stage<K>; STAGES],
    /// Slots in use: the first atlas's, or both atlases'.
    limit: usize,
    /// Every picture of the eye in use is in VRAM (as of the last pass).
    loaded: bool,
    ticks: u32,
    /// The bake eye in use (valid when `have_eye`).
    eye: u16,
    have_eye: bool,
    /// The view of the last update, for the draw.
    view: ViewSpec,
    /// The eye whose tiles are kept ready, if the camera is near enough to crossing to it.
    next: Option<u16>,
    /// The eye nearest where the camera walks to, kept ready as well.
    next2: Option<u16>,
    /// Where the eye was at the last pass (plan), and its smoothed step a pass.
    last_eye: [i32; 2],
    have_last: bool,
    velocity: [i32; 2],
    /// Sector the drive ends at once the reads queued so far are done (0 before the first).
    tail: u32,
    /// An eye's index landed: decide the tiles at once, not at the next pass.
    force_pass: bool,
    /// Something that decides what to read may have changed (a pass, a read that finished, a
    /// stage freed): [`Self::start_reads`] looks. Clear, with every stage free, the tick has
    /// nothing to do and costs next to nothing.
    reads_dirty: bool,
}

impl<K: Copy> PanelStream<K> {
    /// A stream holding nothing.
    pub const fn new() -> Self {
        Self {
            counters: PanelCounters::ZERO,
            slots: [Slot::FREE; SLOTS],
            cache: [Cached::EMPTY; CACHE],
            lists: [EyeList::EMPTY; EYES],
            stages: [Stage::Free; STAGES],
            limit: ATLAS_SLOTS,
            loaded: false,
            ticks: 0,
            eye: 0,
            have_eye: false,
            view: ViewSpec {
                forward: [0; 2],
                pitch_tan_q12: 0,
                focal: 0,
                half_screen: [0; 2],
                panel_focal: 0,
            },
            next: None,
            next2: None,
            last_eye: [0; 2],
            have_last: false,
            velocity: [0; 2],
            tail: 0,
            force_pass: false,
            reads_dirty: true,
        }
    }

    /// Drive the stream one tick: poll the reads in flight, every few ticks (and when an eye's
    /// index has just landed) re-decide the eye and the wanted tiles, and start the next reads.
    /// `window` is [`WINDOW_WORDS`] words of memory the reads land in, and must stay put while
    /// the stream lives.
    pub fn update<T>(&mut self, transport: &mut T, window: &mut [u32], input: &PanelInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        if window.len() < WINDOW_WORDS || input.blocks.is_empty() || input.eyes.is_empty() {
            return;
        }
        self.view = input.view();
        self.limit = input.slots.clamp(ATLAS_SLOTS, SLOTS);
        self.poll(transport, window);
        if self.ticks.is_multiple_of(PASS_TICKS) || self.force_pass {
            self.force_pass = false;
            self.pass(input);
        }
        self.ticks = self.ticks.wrapping_add(1);
        self.start_reads(transport, window, input);
        self.counters.stage_mask = self
            .stages
            .iter()
            .enumerate()
            .map(|(i, s)| u32::from(!matches!(s, Stage::Free)) << i)
            .sum::<u32>()
            | match &self.stages[0] {
                Stage::Busy(run) if run.landed => 4,
                _ => 0,
            };
        self.counters.indexes_waiting = self
            .lists
            .iter()
            .filter(|l| l.eye != u16::MAX && l.state == Header::Wanted)
            .count() as u32;
    }

    /// Retire the reads that finished, taking the indexes out of the window.
    fn poll<T>(&mut self, transport: &mut T, window: &[u32])
    where
        T: RoomTransport<Ticket = K>,
    {
        for index in 0..STAGES {
            let Stage::Busy(run) = &mut self.stages[index] else {
                continue;
            };
            if run.landed {
                continue;
            }
            match transport.poll(run.ticket) {
                Poll::Pending => {}
                Poll::Done => {
                    self.reads_dirty = true;
                    self.counters.landed += 1;
                    run.landed = true;
                    run.landed_at = self.ticks;
                    let base = index * GROUP_BYTES;
                    for entry in run.entries.iter_mut() {
                        let Some(e) = *entry else { continue };
                        match e.kind {
                            Kind::Eye => {
                                *entry = None;
                                let list = &mut self.lists[usize::from(e.cache)];
                                if list.eye == e.eye {
                                    let bytes = |k: usize| {
                                        window[(base + k) / 4].to_le_bytes()[(base + k) % 4]
                                    };
                                    list.parse(bytes);
                                    self.force_pass = true;
                                }
                            }
                            Kind::Tile => {
                                let slot = &self.slots[usize::from(e.slot)];
                                if METER && slot.state == SlotState::Pending && slot.since != 0 {
                                    self.counters.wait_read += self.ticks.wrapping_sub(slot.since);
                                }
                            }
                        }
                    }
                    if !run.warm && run.entries.iter().all(Option::is_none) {
                        let span = run.groups > 1;
                        self.free_stage(index, span);
                    }
                }
                Poll::Cancelled | Poll::Failed => {
                    self.reads_dirty = true;
                    self.counters.failed += 1;
                    for e in run.entries.iter().flatten() {
                        match e.kind {
                            Kind::Eye => {
                                let list = &mut self.lists[usize::from(e.cache)];
                                if list.eye == e.eye {
                                    list.state = Header::Wanted;
                                }
                            }
                            Kind::Tile => {
                                let slot = &mut self.slots[usize::from(e.slot)];
                                if slot.eye == e.eye
                                    && slot.heading == e.heading
                                    && slot.tile == e.tile
                                    && slot.state == SlotState::Pending
                                {
                                    slot.state = SlotState::Wanted;
                                }
                            }
                        }
                    }
                    let span = run.groups > 1;
                    self.free_stage(index, span);
                }
            }
        }
    }

    /// Free stage `index`, and the one a long read took with it.
    fn free_stage(&mut self, index: usize, span: bool) {
        self.stages[index] = Stage::Free;
        if span && index + 1 < STAGES {
            self.stages[index + 1] = Stage::Free;
        }
    }

    /// The bake eye nearest the camera, kept until another is nearer by a quarter of the
    /// spacing.
    fn choose_eye(&mut self, input: &PanelInput<'_>) {
        let distance = |eye: usize| {
            let e = input.eyes[eye];
            octagonal_distance(
                input.eye,
                [i32::from(e[0]), i32::from(e[1]), i32::from(e[2])],
            )
        };
        // The nearest eye (the first of equals), one distance an eye.
        let mut best = 0;
        let mut best_distance = i32::MAX;
        for eye in 0..input.eyes.len() {
            let d = distance(eye);
            if d < best_distance {
                (best, best_distance) = (eye, d);
            }
        }
        // The camera crosses to a nearer eye at once, whatever the disc has brought in: the
        // tiles of the eye it leaves stand in for those of the new one that have not landed
        // ([`Self::rebuild_show`]), so the camera never waits on the drive.
        let current = usize::from(self.eye);
        let lost = !self.have_eye || current >= input.eyes.len();
        let nearer = !lost && best_distance + input.spacing / 4 < distance(current);
        if lost || nearer {
            self.eye = best as u16;
            self.have_eye = true;
        }
        let here = distance(usize::from(self.eye));
        let waiting =
            !lost && usize::from(self.eye) != best && best_distance + input.spacing / 4 < here;
        self.counters.gate_run = if waiting {
            self.counters.gate_run + 1
        } else {
            0
        };
        self.counters.far = (here * 10 / input.spacing.max(1)) as u32;
        self.counters.eye = u32::from(self.eye);
        // The eye the camera is about to cross to: the one nearest the place it walks to (a bit
        // under a spacing ahead along the way it has been going, or where it is when it stands
        // still), other than the one in use, when that is within half a spacing of being as
        // near as the one in use is.
        let step = [
            input.eye[0] - self.last_eye[0],
            input.eye[2] - self.last_eye[1],
        ];
        if self.have_last {
            self.velocity = [
                (3 * self.velocity[0] + step[0]) >> 2,
                (3 * self.velocity[1] + step[1]) >> 2,
            ];
        }
        self.last_eye = [input.eye[0], input.eye[2]];
        self.have_last = true;
        let speed = octagonal_distance([self.velocity[0], 0, self.velocity[1]], [0; 3]);
        let ahead = if speed >= MIN_WALK {
            let reach = input.spacing * 3 / 5;
            [
                input.eye[0] + self.velocity[0] * reach / speed,
                input.eye[1],
                input.eye[2] + self.velocity[1] * reach / speed,
            ]
        } else {
            input.eye
        };
        let to_ahead = |eye: usize| {
            let e = input.eyes[eye];
            octagonal_distance(ahead, [i32::from(e[0]), i32::from(e[1]), i32::from(e[2])])
        };
        // The second eye is only started while the camera walks, once it is out from the eye
        // in use and the first is in VRAM: its list and headers are read ahead of every tile,
        // and the one nearest the walk changes from pass to pass, so starting it early would
        // spend the drive on the lists of eyes never crossed to.
        let walking = speed >= MIN_WALK && here * 10 >= SECOND_FROM * input.spacing;
        // One sweep of the other eyes finds both: the nearest to the camera, which is the one it
        // crosses to when it is a quarter spacing nearer, and the two nearest where it walks to
        // (the second of them for when the first is the nearest), so that a crossing finds the
        // pictures there whichever way the walk bends. Each is the first of equals.
        let current = usize::from(self.eye);
        let (mut near, mut near_distance) = (None, i32::MAX);
        let mut ahead_best: [Option<(usize, i32)>; 2] = [None; 2];
        for eye in (0..input.eyes.len()).filter(|&eye| eye != current) {
            let d = distance(eye);
            if d < near_distance {
                (near, near_distance) = (Some(eye), d);
            }
            if walking {
                let a = to_ahead(eye);
                if ahead_best[0].is_none_or(|(_, best)| a < best) {
                    ahead_best[1] = ahead_best[0];
                    ahead_best[0] = Some((eye, a));
                } else if ahead_best[1].is_none_or(|(_, second)| a < second) {
                    ahead_best[1] = Some((eye, a));
                }
            }
        }
        // The eye kept ready stays so while it is almost as near as the nearest: where the
        // camera is about as near to several eyes it would otherwise change from pass to pass,
        // and none would be read in before the camera crossed.
        let kept = self
            .next
            .map(usize::from)
            .filter(|&eye| eye != current && eye < input.eyes.len())
            .filter(|&eye| distance(eye) <= near_distance + input.spacing / NEXT_STICKS);
        let near = kept
            .or(near)
            .filter(|&eye| distance(eye) < here + input.spacing);
        self.next = near.map(|eye| eye as u16);
        let first_ready = walking && near.is_none_or(|eye| self.eye_ready(eye as u16, true, input));
        self.next2 = None;
        if first_ready {
            let pick = match ahead_best {
                [Some((eye, _)), second] if Some(eye) == near => second,
                [first, _] => first,
            };
            self.next2 = pick
                .map(|(eye, _)| eye)
                .filter(|&eye| distance(eye) < here + input.spacing)
                .map(|eye| eye as u16);
        }
    }

    /// Whether every picture of the eye in use is in VRAM, as of the last pass.
    pub fn eye_loaded(&self) -> bool {
        self.loaded
    }

    /// Whether every stored tile of every panel of `eye` is in VRAM (of the panels in view
    /// only, with `view_only`).
    fn eye_ready(&self, eye: u16, view_only: bool, input: &PanelInput<'_>) -> bool {
        let Some(list) = self
            .lists
            .iter()
            .find(|l| l.eye == eye && l.state == Header::Ready)
        else {
            return false;
        };
        for heading in 0..HEADINGS {
            if list.index.panel[heading] == NO_PANEL
                || (view_only && visible(heading, input).is_none())
            {
                continue;
            }
            let Some(cached) = self
                .cache
                .iter()
                .find(|c| c.eye == eye && c.heading == heading as u8)
            else {
                return false;
            };
            if cached.header != Header::Ready {
                return false;
            }
            let view = if view_only {
                visible(heading, input)
            } else {
                None
            };
            for tile in 0..TILES {
                if !list.stored(heading, tile)
                    || (view_only
                        && !view.is_some_and(|v| tile_in_view(&v, tile / ROWS, tile % ROWS)))
                {
                    continue;
                }
                let held = cached.held[tile];
                if held == NO_SLOT_INDEX || self.slots[usize::from(held)].state != SlotState::Shown
                {
                    return false;
                }
            }
        }
        true
    }

    /// Tiles of the panels in view of the eye in use that are not in VRAM.
    fn view_missing(&self, input: &PanelInput<'_>) -> u32 {
        let Some(list) = self
            .lists
            .iter()
            .find(|l| l.eye == self.eye && l.state == Header::Ready)
        else {
            return u32::MAX;
        };
        let mut missing = 0;
        for heading in 0..HEADINGS {
            let Some(view) =
                visible(heading, input).filter(|_| list.index.panel[heading] != NO_PANEL)
            else {
                continue;
            };
            let cached = self
                .cache
                .iter()
                .find(|c| c.eye == self.eye && c.heading == heading as u8);
            for tile in 0..TILES {
                if !list.stored(heading, tile) || !tile_in_view(&view, tile / ROWS, tile % ROWS) {
                    continue;
                }
                let held = cached.map_or(NO_SLOT_INDEX, |c| c.held[tile]);
                let shown = held != NO_SLOT_INDEX
                    && self.slots[usize::from(held)].state == SlotState::Shown;
                missing += u32::from(!shown);
            }
        }
        missing
    }

    /// Cache index of the panel `(eye, heading)`, making one (and dropping the least
    /// recently wanted panel no slot refers to) when it is not there.
    fn cache_index(&mut self, eye: u16, heading: u8) -> Option<u8> {
        if let Some(at) = self
            .cache
            .iter()
            .position(|c| c.eye == eye && c.heading == heading)
        {
            return Some(at as u8);
        }
        // The panel nothing has wanted for longest, an empty one first. The tiles it holds go
        // with it (they are idle or of an eye left; a panel this pass wants stays).
        let now = self.ticks.max(1);
        let mut victim: Option<(usize, u32)> = None;
        for (index, c) in self.cache.iter().enumerate() {
            if c.eye != u16::MAX && c.used == now {
                continue;
            }
            let age = if c.eye == u16::MAX { 0 } else { c.used.max(1) };
            if victim.is_none_or(|(_, a)| age < a) {
                victim = Some((index, age));
            }
        }
        let (index, _) = victim?;
        for slot in 0..SLOTS {
            let held = self.slots[slot];
            if held.state != SlotState::Free && usize::from(held.cache) == index {
                self.slots[slot] = Slot::FREE;
            }
        }
        self.cache[index] = Cached {
            eye,
            heading,
            ..Cached::EMPTY
        };
        Some(index as u8)
    }

    /// A slot for a tile of rank `rank`: a free one, or the one holding the worst ranked tile
    /// if that is worse than `rank` by more than the margin and no pass wanted it.
    fn slot_for(&mut self, rank: i32) -> Option<usize> {
        if let Some(slot) = self.slots[..self.limit]
            .iter()
            .position(|s| s.state == SlotState::Free)
        {
            return Some(slot);
        }
        let mut worst: Option<(usize, i32)> = None;
        for (slot, held) in self.slots[..self.limit].iter().enumerate() {
            if held.wanted && !held.prefetch {
                continue;
            }
            if held.rank > rank.saturating_add(EVICT_MARGIN)
                && worst.is_none_or(|(_, w)| held.rank > w)
            {
                worst = Some((slot, held.rank));
            }
        }
        let (slot, _) = worst?;
        self.release(slot);
        self.counters.evictions += 1;
        Some(slot)
    }

    fn release(&mut self, slot: usize) {
        let held = self.slots[slot];
        if held.cache != NO_ENTRY {
            let cached = &mut self.cache[usize::from(held.cache)];
            if cached.held[usize::from(held.tile)] == slot as u8 {
                cached.held[usize::from(held.tile)] = NO_SLOT_INDEX;
                cached.show[usize::from(held.tile)] = NO_TILE_SLOT;
            }
        }
        self.slots[slot] = Slot::FREE;
    }

    /// Decide the eye and the wanted tiles.
    fn pass(&mut self, input: &PanelInput<'_>) {
        self.choose_eye(input);
        let eye = self.eye;
        // A tile of an eye left that was never read is not worth reading now.
        for slot in 0..SLOTS {
            let held = self.slots[slot];
            if held.state == SlotState::Wanted
                && held.eye != eye
                && Some(held.eye) != self.next
                && Some(held.eye) != self.next2
            {
                self.release(slot);
            }
        }
        for slot in &mut self.slots {
            slot.wanted = false;
            slot.prefetch = false;
            // Tiles no pass wants rank as idle; the ones of an eye the camera has left as stale.
            slot.rank = if slot.eye == eye { IDLE } else { IDLE + STALE };
        }
        let mut wanted = 0u32;
        let mut starved = 0u32;
        // What the camera sees of each heading, and where each tile column stands across a
        // panel: the same for every eye, so worked out once a pass.
        let panel_focal = input.panel_focal.max(1);
        let mut sights = View {
            seen: [None; HEADINGS],
            middle: [0; COLS],
        };
        for (heading, seen) in sights.seen.iter_mut().enumerate() {
            *seen = visible_in(heading, &self.view, 18, PICTURE_ROWS / 3);
        }
        for (col, middle) in sights.middle.iter_mut().enumerate() {
            *middle =
                ((col as i32 * TILE_TEXELS + TILE_TEXELS / 2 - COLS_CENTRE) << 12) / panel_focal;
        }
        self.want_tiles(input, &sights, eye, 0, false, &mut wanted, &mut starved);
        if let Some(next) = self.next {
            self.want_tiles(
                input,
                &sights,
                next,
                PREFETCH,
                true,
                &mut wanted,
                &mut starved,
            );
        }
        if let Some(next) = self.next2 {
            self.want_tiles(
                input,
                &sights,
                next,
                2 * PREFETCH,
                true,
                &mut wanted,
                &mut starved,
            );
        }
        self.rebuild_show(eye);
        let (mut held, mut missing) = (0u32, 0u32);
        for slot in &self.slots {
            if slot.state != SlotState::Free {
                held += 1;
            }
            if slot.wanted && !slot.prefetch && slot.state != SlotState::Shown {
                missing += 1;
            }
        }
        self.counters.held = held;
        self.counters.wanted = wanted;
        self.counters.missing = missing;
        self.counters.starved = starved;
        self.reads_dirty = true;
        self.loaded = self.have_eye && self.eye_ready(self.eye, false, input);
        self.counters.view_missing = self.view_missing(input);
        // Counted in the passes of every `PASS_TICKS` ticks, not in the extra one an index asks for.
        if self.loaded {
            self.counters.settling = 0;
        } else if self.ticks.is_multiple_of(PASS_TICKS) {
            self.counters.settling += 1;
        }
        if METER {
            self.count_visible(eye);
        }
    }

    /// Tally the tiles the screen shows against the ones that have a picture.
    fn count_visible(&mut self, eye: u16) {
        let Some(list) = self
            .lists
            .iter()
            .find(|l| l.eye == eye && l.state == Header::Ready)
        else {
            return;
        };
        let (mut seen, mut absent) = (0u32, 0u32);
        for heading in 0..HEADINGS {
            if list.index.panel[heading] == NO_PANEL {
                continue;
            }
            let Some(v) = visible_in(heading, &self.view, 16, 0) else {
                continue;
            };
            for col in v.first_col..=v.last_col {
                for row in v.first_row..=v.last_row {
                    let tile = col * ROWS + row;
                    if list.stored(heading, tile) {
                        seen += 1;
                        // Blank only if no panel of the heading, of the eye in use or of one left,
                        // has a picture to show for it.
                        let covered = self
                            .cache
                            .iter()
                            .any(|o| o.heading == heading as u8 && o.show[tile] != NO_TILE_SLOT);
                        absent += u32::from(!covered);
                    }
                }
            }
        }
        self.counters.visible = self.counters.visible.wrapping_add(seen);
        self.counters.visible_missing = self.counters.visible_missing.wrapping_add(absent);
        self.counters.passes_missing += u32::from(absent > 0);
    }

    /// Index of the list of `eye`'s stored tiles, asking for it (and giving up the one used
    /// longest ago that nothing refers to) when it is not there.
    fn list_index(&mut self, eye: u16, input: &PanelInput<'_>) -> Option<usize> {
        if let Some(at) = self.lists.iter().position(|l| l.eye == eye) {
            return Some(at);
        }
        if input.blocks.get(usize::from(eye)).is_none_or(|&b| b == 0) {
            return None;
        }
        let at = (0..EYES)
            .filter(|&i| {
                let l = &self.lists[i];
                l.eye == u16::MAX || (l.eye != self.eye && Some(l.eye) != self.next)
            })
            .min_by_key(|&i| {
                let l = &self.lists[i];
                if l.eye == u16::MAX {
                    0
                } else {
                    l.used.max(1)
                }
            })?;
        // An eye whose tiles are still held (the camera was there lately) needs only its index
        // again.
        let only_index = self
            .cache
            .iter()
            .any(|c| c.eye == eye && c.header == Header::Ready);
        self.lists[at] = EyeList {
            eye,
            state: Header::Wanted,
            since: self.ticks.max(1),
            only_index,
            ..EyeList::EMPTY
        };
        Some(at)
    }

    /// Tiles of the panels of `heading` that a panel of an eye other than `eye` holds in VRAM:
    /// the ones that would not be blank if the camera crossed to `eye` before its own landed.
    fn standing_in(&self, eye: u16, heading: u8) -> u8 {
        let mut mask = 0u8;
        for c in &self.cache {
            if c.eye == u16::MAX
                || c.eye == eye
                || c.heading != heading
                || c.header != Header::Ready
            {
                continue;
            }
            for (tile, &held) in c.held.iter().enumerate() {
                if held != NO_SLOT_INDEX && self.slots[usize::from(held)].state == SlotState::Shown
                {
                    mask |= 1 << tile;
                }
            }
        }
        mask
    }

    /// Ask for the tiles of `eye` the camera can see, ranked from `base` up by how far each is
    /// from the middle of the view. A tile of an eye kept ready that the screen shows and no
    /// panel of another eye could stand in for is ranked ahead of the rest of its eye
    /// ([`EXPOSED`]): crossing to the eye before it lands would leave a hole.
    #[allow(clippy::too_many_arguments)]
    fn want_tiles(
        &mut self,
        input: &PanelInput<'_>,
        sights: &View,
        eye: u16,
        base: i32,
        prefetch: bool,
        wanted: &mut u32,
        starved: &mut u32,
    ) {
        // Which tiles the eye stores is on the disc: ask, and wait for it.
        let Some(list) = self.list_index(eye, input) else {
            return;
        };
        self.lists[list].used = self.ticks.max(1);
        if self.lists[list].state != Header::Ready {
            return;
        }
        for heading in 0..HEADINGS {
            if self.lists[list].index.panel[heading] == NO_PANEL {
                continue;
            }
            // Every panel of the eye in use is kept, whichever way the camera looks: it may turn at
            // any moment, and a turn must not wait for the disc. The view only orders the reads.
            // An eye the camera is only about to cross to brings the panels it will see on
            // arriving; the others follow once it has crossed (the eye left stands in).
            let seen = sights.seen[heading];
            if prefetch && seen.is_none() {
                continue;
            }
            let Some(ci) = self.cache_index(eye, heading as u8) else {
                continue;
            };
            self.cache[usize::from(ci)].used = self.ticks.max(1);
            self.cache[usize::from(ci)].rank = i32::MAX;
            if self.cache[usize::from(ci)].header == Header::Absent {
                // The index came with the depth mesh and the room cells.
                let cached = &mut self.cache[usize::from(ci)];
                cached.depth = self.lists[list].depth[heading];
                cached.rooms = self.lists[list].rooms[heading];
                cached.header = Header::Ready;
                cached.since = self.ticks.max(1);
            }
            let standing_in = match seen {
                Some(_) if prefetch => self.standing_in(eye, heading as u8),
                _ => u8::MAX,
            };
            for col in 0..COLS {
                for row in 0..ROWS {
                    let tile = col * ROWS + row;
                    if !self.lists[list].stored(heading, tile) {
                        continue;
                    }
                    // An eye about to be crossed to brings the tiles the screen will show.
                    if prefetch && !seen.is_some_and(|view| tile_in_view(&view, col, row)) {
                        continue;
                    }
                    *wanted += u32::from(!prefetch);
                    // The tiles the camera sees first, the others after them.
                    let rank = match seen {
                        Some(view) => {
                            let across = (sights.middle[col] - view.tan_yaw).abs();
                            let down = (row as i32 * PICTURE_ROWS + PICTURE_ROWS / 2
                                - view.middle_row)
                                .abs();
                            let ahead =
                                if tile_in_view(&view, col, row) && standing_in >> tile & 1 == 0 {
                                    EXPOSED
                                } else {
                                    base
                                };
                            ahead + across + 4 * down
                        }
                        None => base + OFF_VIEW,
                    };
                    let cached = &mut self.cache[usize::from(ci)];
                    cached.rank = cached.rank.min(rank);
                    let held = cached.held[tile];
                    if held != NO_SLOT_INDEX {
                        let s = &mut self.slots[usize::from(held)];
                        s.wanted = true;
                        s.prefetch = prefetch;
                        s.rank = rank;
                        continue;
                    }
                    let Some(slot) = self.slot_for(rank) else {
                        *starved += u32::from(!prefetch);
                        continue;
                    };
                    self.slots[slot] = Slot {
                        state: SlotState::Wanted,
                        eye,
                        heading: heading as u8,
                        tile: tile as u8,
                        cache: ci,
                        wanted: true,
                        prefetch,
                        rank,
                        since: 0,
                    };
                    self.cache[usize::from(ci)].held[tile] = slot as u8;
                }
            }
        }
    }

    /// The slots to draw each tile from: those of the eye in use that a pass wants, and those
    /// of an eye left where the new eye's tile is stored and not in VRAM yet.
    fn rebuild_show(&mut self, eye: u16) {
        let list = self
            .lists
            .iter()
            .find(|l| l.eye == eye && l.state == Header::Ready);
        for c in 0..CACHE {
            let cached = &self.cache[c];
            let mut shows = [NO_TILE_SLOT; TILES];
            if cached.eye != u16::MAX && cached.header == Header::Ready {
                if cached.eye == eye {
                    for (tile, show) in shows.iter_mut().enumerate() {
                        let held = cached.held[tile];
                        if held != NO_SLOT_INDEX {
                            let slot = &self.slots[usize::from(held)];
                            if slot.state == SlotState::Shown && slot.wanted {
                                *show = held;
                            }
                        }
                    }
                } else {
                    // Stale: only while the tile of the eye in use is not there. What the new
                    // eye stores of this heading, and the panel it holds for it, are the same
                    // for every tile. While the new eye's index is not in, its tile is not
                    // known to be stored: the old one stands in.
                    let heading = usize::from(cached.heading);
                    let list = list.filter(|l| l.index.panel[heading] != NO_PANEL);
                    let replacing = self
                        .cache
                        .iter()
                        .find(|o| o.eye == eye && o.heading == cached.heading)
                        .filter(|o| o.header == Header::Ready);
                    for (tile, show) in shows.iter_mut().enumerate() {
                        let held = cached.held[tile];
                        if held == NO_SLOT_INDEX
                            || self.slots[usize::from(held)].state != SlotState::Shown
                        {
                            continue;
                        }
                        let replaced = replacing.is_some_and(|o| {
                            let s = o.held[tile];
                            s != NO_SLOT_INDEX
                                && self.slots[usize::from(s)].state == SlotState::Shown
                        });
                        let new_stored = list.is_none_or(|l| l.stored(heading, tile));
                        if new_stored && !replaced {
                            *show = held;
                        }
                    }
                }
            }
            self.cache[c].show = shows;
        }
    }

    /// Free the landed groups nothing took tiles from within [`WARM_TICKS`].
    fn retire_warm(&mut self) {
        for index in 0..STAGES {
            let Stage::Busy(run) = &self.stages[index] else {
                continue;
            };
            if run.landed
                && run.entries.iter().all(Option::is_none)
                && self.ticks.wrapping_sub(run.landed_at) >= WARM_TICKS
            {
                let span = run.groups > 1;
                self.free_stage(index, span);
            }
        }
    }

    /// Give the wanted tiles that lie in a group already landed an entry in it: they need no
    /// read.
    fn attach_warm(&mut self, input: &PanelInput<'_>) {
        let _ = input;
        for stage in 0..STAGES {
            let Stage::Busy(run) = &mut self.stages[stage] else {
                continue;
            };
            if !run.landed {
                continue;
            }
            let Some(list) = self
                .lists
                .iter()
                .find(|l| l.eye == run.eye && l.state == Header::Ready)
            else {
                continue;
            };
            let (first, last) = (run.group, run.group + u16::from(run.groups) - 1);
            for (index, s) in self.slots.iter_mut().enumerate() {
                if s.state != SlotState::Wanted || s.eye != run.eye {
                    continue;
                }
                let (heading, tile) = (usize::from(s.heading), usize::from(s.tile));
                if !list.stored(heading, tile) {
                    continue;
                }
                let group = list.group_of(heading, tile);
                if group < first || group > last {
                    continue;
                }
                // The tile's sectors, from the first group's start, must have been read.
                let (from, to) = tile_sectors(&list.index, heading, tile, first);
                if from < run.lo || to > run.hi {
                    continue;
                }
                let Some(free) = run.entries.iter().position(Option::is_none) else {
                    break;
                };
                run.entries[free] = Some(Entry {
                    kind: Kind::Tile,
                    eye: s.eye,
                    heading: s.heading,
                    tile: s.tile,
                    slot: index as u8,
                    cache: s.cache,
                    at: (list.index.offset[heading][tile] - u32::from(first) * GROUP_BYTES as u32)
                        as u16,
                    len: list.index.len[heading][tile],
                });
                s.state = SlotState::Pending;
                if METER && s.since == 0 {
                    s.since = self.ticks.max(1);
                }
            }
        }
    }

    /// Whether something the eye in use needs is waiting for a read: its index, or a tile of
    /// its view that has no read yet.
    fn urgent_waiting(&self, input: &PanelInput<'_>) -> bool {
        self.lists.iter().any(|l| {
            l.eye == self.eye
                && l.state == Header::Wanted
                && input
                    .blocks
                    .get(usize::from(l.eye))
                    .is_some_and(|&b| b != 0)
        }) || self
            .slots
            .iter()
            .any(|s| s.state == SlotState::Wanted && !s.prefetch && s.eye == self.eye)
    }

    /// Free a stage for a read the eye in use is waiting for: one whose landed tiles are all for
    /// eyes the camera is not at yet gives up its stage (the tiles are asked for again later),
    /// and a read of such tiles (or of such an eye's index) still on its way is cancelled (its
    /// stage frees when the transport says so). Returns whether a stage was freed at once.
    fn make_room<T>(&mut self, transport: &mut T) -> bool
    where
        T: RoomTransport<Ticket = K>,
    {
        let others = |stream: &Self, run: &Run<K>| {
            run.entries.iter().flatten().all(|e| match e.kind {
                Kind::Tile => stream.slots[usize::from(e.slot)].eye != stream.eye,
                Kind::Eye => e.eye != stream.eye,
            })
        };
        for index in 0..STAGES {
            let Stage::Busy(run) = &self.stages[index] else {
                continue;
            };
            if !run.landed || !others(self, run) {
                continue;
            }
            let span = run.groups > 1;
            let entries = run.entries;
            for e in entries.iter().flatten() {
                let slot = &mut self.slots[usize::from(e.slot)];
                if e.kind == Kind::Tile
                    && slot.eye == e.eye
                    && slot.heading == e.heading
                    && slot.tile == e.tile
                    && slot.state == SlotState::Pending
                {
                    slot.state = SlotState::Wanted;
                }
            }
            self.free_stage(index, span);
            self.counters.preempted += 1;
            self.reads_dirty = true;
            return true;
        }
        let mut cancelled = false;
        for index in 0..STAGES {
            let Stage::Busy(run) = &self.stages[index] else {
                continue;
            };
            if run.landed || !others(self, run) {
                continue;
            }
            transport.cancel(run.ticket);
            self.counters.preempted += 1;
            cancelled = true;
        }
        if cancelled {
            self.reads_dirty = true;
        }
        false
    }

    /// Queue reads for the indexes and tiles that need one into the free stages: the best
    /// ranked of them, and everything else wanted in the same group (and the next, when both
    /// stages are free).
    fn start_reads<T>(&mut self, transport: &mut T, window: &mut [u32], input: &PanelInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        if !self.reads_dirty {
            return;
        }
        self.attach_warm(input);
        self.retire_warm();
        // Every few ticks while all stages are taken: a read the eye in use waits for may
        // take one from tiles that are not for it.
        if self.ticks.is_multiple_of(4)
            && !self.stages.iter().any(|s| matches!(s, Stage::Free))
            && self.urgent_waiting(input)
        {
            self.make_room(transport);
        }
        loop {
            let Some(stage) = self.stages.iter().position(|s| matches!(s, Stage::Free)) else {
                return;
            };
            // What is wanted: (rank, sector, kind, which, waited, eye, group).
            let mut wanted = [(0i32, 0u32, Kind::Tile, 0u8, 0u32, 0u16, 0u16); SLOTS + EYES];
            let mut count = 0;
            for (index, l) in self.lists.iter().enumerate() {
                if l.eye == u16::MAX || l.state != Header::Wanted {
                    continue;
                }
                let Some(&block) = input.blocks.get(usize::from(l.eye)).filter(|&&b| b != 0) else {
                    continue;
                };
                // Nothing of the eye can be asked for until its index is in.
                let rank = if Some(l.eye) == Some(self.eye) {
                    -2
                } else {
                    PREFETCH - 2
                };
                wanted[count] = (
                    rank,
                    block,
                    Kind::Eye,
                    index as u8,
                    self.ticks.wrapping_sub(l.since),
                    l.eye,
                    0,
                );
                count += 1;
            }
            for (index, s) in self.slots.iter().enumerate() {
                if s.state != SlotState::Wanted {
                    continue;
                }
                // The tile's group, from its place in its eye's block.
                let Some((list, block)) = self
                    .lists
                    .iter()
                    .find(|l| l.eye == s.eye && l.state == Header::Ready)
                    .zip(input.blocks.get(usize::from(s.eye)).filter(|&&b| b != 0))
                else {
                    continue;
                };
                let group = list.group_of(usize::from(s.heading), usize::from(s.tile));
                let since = if s.since == 0 { self.ticks } else { s.since };
                wanted[count] = (
                    s.rank,
                    block + u32::from(group) * GROUP_SECTORS as u32,
                    Kind::Tile,
                    index as u8,
                    self.ticks.wrapping_sub(since),
                    s.eye,
                    group,
                );
                count += 1;
            }
            for s in &mut self.slots {
                if s.state == SlotState::Wanted && s.since == 0 {
                    s.since = self.ticks.max(1);
                }
            }
            let wanted = &wanted[..count];
            if wanted.is_empty() {
                self.reads_dirty = !self.stages.iter().all(|s| matches!(s, Stage::Free));
                return;
            }
            // The best ranked item, unless one has waited out its patience.
            let head = wanted
                .iter()
                .filter(|w| w.4 > PATIENCE && w.0 < 2 * PREFETCH)
                .max_by_key(|w| (w.4, core::cmp::Reverse(w.0)))
                .or_else(|| {
                    // The most urgent tier first (the indexes, the tiles of the eye in use,
                    // those of the eye nearest the camera, those of the one it walks to), and
                    // within it the group the drive reaches soonest: the one that continues the
                    // last read, then the next ones ahead, then those behind, the lowest first
                    // (a seek back lands at the start of the stretch, and the reads after it each
                    // start where the last ended). The drive then sweeps the wanted groups
                    // upward, and a read that starts where the last ended costs no seek.
                    let tail = self.tail;
                    wanted.iter().min_by_key(|w| {
                        let tier = if w.0 < 0 {
                            0
                        } else if w.0 < EXPOSED {
                            1
                        } else if w.0 < PREFETCH / 2 {
                            2
                        } else if w.0 < 2 * PREFETCH {
                            3
                        } else {
                            4
                        };
                        let reach = if tail == 0 || w.1 > tail {
                            w.1.saturating_sub(tail)
                        } else {
                            1 << 24 | w.1
                        };
                        (tier, reach, w.0)
                    })
                })
                .copied()
                .unwrap_or(wanted[0]);
            let (_, _, head_kind, head_which, _, eye, group) = head;
            let Some(&block) = input.blocks.get(usize::from(eye)) else {
                return;
            };
            // How many sectors the eye's block has.
            let known = input
                .sectors
                .get(usize::from(eye))
                .map(|&sectors| u32::from(sectors))
                .filter(|&sectors| sectors > 0);
            // With both stages free, a read takes two groups: an eye's first read brings in its
            // index and, for the average eye, all its tiles for one seek; a tile's brings in the
            // next group too when something wanted lies in it.
            let both_free = self.stages.iter().all(|s| matches!(s, Stage::Free));
            let next_wanted = wanted
                .iter()
                .any(|w| w.5 == eye && w.6 == group + 1 && w.2 == Kind::Tile);
            let more_blocks =
                known.is_none_or(|k| k > (u32::from(group) + 1) * GROUP_SECTORS as u32);
            // The eye in use reads its index and everything after it in one go (a crossing
            // waits for it); an eye kept ready reads its index alone, and then the sectors its
            // tiles lie in.
            let index_only = head_kind == Kind::Eye
                && (self.lists[usize::from(head_which)].only_index || eye != self.eye);
            let groups: u8 = if both_free
                && more_blocks
                && !index_only
                && (head_kind == Kind::Eye || next_wanted)
            {
                2
            } else {
                1
            };
            let first = block + u32::from(group) * GROUP_SECTORS as u32;
            let mut whole = u32::from(groups) * GROUP_SECTORS as u32;
            if let Some(k) = known {
                whole = whole.min(k.saturating_sub(u32::from(group) * GROUP_SECTORS as u32));
            }
            let whole = whole.max(1) as u8;
            // The sectors to read, from the first group's start: all of them for an eye's first
            // read, the index's for an eye kept ready, else from the first sector a wanted tile
            // lies in to the last.
            let (mut lo, mut hi) = (whole, 0u8);
            let mut entries = [None; ENTRIES];
            let mut at = 0;
            let mut warm = false;
            for &(_, _, kind, which, _, w_eye, w_group) in wanted.iter() {
                if w_eye != eye || w_group < group || w_group >= group + u16::from(groups) {
                    continue;
                }
                if at >= ENTRIES {
                    break;
                }
                entries[at] = Some(match kind {
                    Kind::Eye => {
                        warm = true;
                        (lo, hi) = (0, hi.max(if index_only { 1 } else { whole }));
                        Entry {
                            kind,
                            eye,
                            heading: 0,
                            tile: 0,
                            slot: NO_SLOT_INDEX,
                            cache: which,
                            at: 0,
                            len: 0,
                        }
                    }
                    Kind::Tile => {
                        let s = &self.slots[usize::from(which)];
                        let list = self
                            .lists
                            .iter()
                            .find(|l| l.eye == eye && l.state == Header::Ready)
                            .expect("a tile is wanted from a list that is in");
                        let (heading, tile) = (usize::from(s.heading), usize::from(s.tile));
                        let (from, to) = tile_sectors(&list.index, heading, tile, group);
                        (lo, hi) = (lo.min(from), hi.max(to.min(whole)));
                        Entry {
                            kind,
                            eye,
                            heading: s.heading,
                            tile: s.tile,
                            slot: which,
                            cache: s.cache,
                            at: (list.index.offset[heading][tile]
                                - u32::from(group) * GROUP_BYTES as u32)
                                as u16,
                            len: list.index.len[heading][tile],
                        }
                    }
                });
                at += 1;
            }
            if hi <= lo {
                // Nothing the groups hold is wanted (the wanted items went to other places).
                (lo, hi) = (0, 1);
            }
            let sectors = u32::from(hi - lo);
            let first = first + u32::from(lo);
            transport.begin_transfer();
            // SAFETY: the window is memory the caller keeps for the stream's life; the stage is
            // one of its STAGES groups (the first, when two are read), the read is at most
            // `groups` groups, and the memory is not reused until the read is reported finished.
            let destination = unsafe {
                window
                    .as_mut_ptr()
                    .add((stage * GROUP_BYTES + usize::from(lo) * SECTOR) / 4)
            };
            let lba = input.base_lba + first;
            // A read for the eye in use (its index, the tiles of its view and those nothing
            // could stand in for) goes ahead of reads that are waiting.
            let urgent = head.0 < EXPOSED;
            match unsafe { transport.submit(lba, sectors, destination, urgent) } {
                Some(ticket) => {
                    for entry in entries.iter().flatten() {
                        match entry.kind {
                            Kind::Eye => {
                                self.lists[usize::from(entry.cache)].state = Header::Pending;
                            }
                            Kind::Tile => {
                                let slot = &mut self.slots[usize::from(entry.slot)];
                                slot.state = SlotState::Pending;
                                if METER {
                                    self.counters.wait_queue += self.ticks.wrapping_sub(slot.since);
                                }
                                slot.since = self.ticks.max(1);
                            }
                        }
                    }
                    self.stages[stage] = Stage::Busy(Run {
                        ticket,
                        landed: false,
                        warm,
                        eye,
                        group,
                        groups,
                        lo,
                        hi,
                        landed_at: 0,
                        entries,
                    });
                    if groups > 1 {
                        for part in stage + 1..STAGES {
                            self.stages[part] = Stage::Spanned;
                        }
                    }
                    self.counters.submitted += 1;
                    self.tail = first + sectors - 1;
                    self.counters.last_first = first;
                    self.counters.last_sectors = sectors;
                    self.counters.sectors += sectors;
                }
                None => {
                    self.counters.refused += 1;
                    return;
                }
            }
        }
    }

    /// The landed tiles waiting for the GPU, up to `limit` (and `N`), for slots that still hold
    /// the tile they were read for.
    pub fn uploads<const N: usize>(&mut self, limit: usize) -> ([Option<TileUpload>; N], usize) {
        let limit = limit.min(N);
        let mut out = [None; N];
        let mut count = 0;
        let mut freed = [false; STAGES];
        for (index, stage) in self.stages.iter_mut().enumerate() {
            let Stage::Busy(run) = stage else {
                continue;
            };
            if !run.landed {
                continue;
            }
            for (n, entry) in run.entries.iter_mut().enumerate() {
                let Some(e) = *entry else { continue };
                if e.kind != Kind::Tile {
                    *entry = None;
                    continue;
                }
                let slot = &self.slots[usize::from(e.slot)];
                if slot.eye != e.eye
                    || slot.heading != e.heading
                    || slot.tile != e.tile
                    || slot.state != SlotState::Pending
                {
                    // The slot was given to another tile meanwhile.
                    *entry = None;
                    continue;
                }
                if count < limit {
                    out[count] = Some(TileUpload {
                        slot: e.slot,
                        stage: index as u8,
                        entry: n as u8,
                        at: (index * GROUP_BYTES) as u32 + u32::from(e.at),
                        len: e.len,
                    });
                    count += 1;
                }
            }
            if !run.warm && run.entries.iter().all(Option::is_none) {
                freed[index] = run.groups > 1;
                *stage = Stage::Free;
                self.reads_dirty = true;
            }
        }
        for (index, &spanned) in freed.iter().enumerate() {
            if spanned && index + 1 < STAGES {
                self.stages[index + 1] = Stage::Free;
            }
        }
        (out, count)
    }

    /// The uploads in `taken` are in a frame's list: VRAM now holds their tiles and their
    /// entries are free again.
    pub fn applied(&mut self, taken: &[Option<TileUpload>]) {
        self.reads_dirty |= !taken.is_empty();
        for upload in taken.iter().flatten() {
            let slot = &mut self.slots[usize::from(upload.slot)];
            if slot.state == SlotState::Pending {
                slot.state = SlotState::Shown;
                if METER {
                    self.counters.shown += 1;
                    self.counters.wait_load += self.ticks.wrapping_sub(slot.since);
                }
                slot.since = 0;
            }
            let index = usize::from(upload.stage);
            if let Stage::Busy(run) = &mut self.stages[index] {
                run.entries[usize::from(upload.entry)] = None;
                if !run.warm && run.entries.iter().all(Option::is_none) {
                    let span = run.groups > 1;
                    self.free_stage(index, span);
                }
            }
            self.counters.uploads += 1;
        }
        // The tiles that landed show from the next frame on, and the tile of an eye left that
        // stood in for each stops at once (drawn under it, it would only cost fill until the
        // next pass).
        for c in 0..CACHE {
            for tile in 0..TILES {
                let cached = &self.cache[c];
                let held = cached.held[tile];
                if held != NO_SLOT_INDEX
                    && cached.show[tile] == NO_TILE_SLOT
                    && cached.header == Header::Ready
                    && cached.eye == self.eye
                {
                    let slot = &self.slots[usize::from(held)];
                    if slot.state == SlotState::Shown && slot.wanted {
                        let heading = cached.heading;
                        self.cache[c].show[tile] = held;
                        for left in &mut self.cache {
                            if left.eye != self.eye && left.heading == heading {
                                left.show[tile] = NO_TILE_SLOT;
                            }
                        }
                    }
                }
            }
        }
    }

    /// The panel `c` as the draw reads it, when it has a picture to show and the screen sees it.
    fn drawable<'a>(&'a self, c: &'a Cached, eyes: &[[i16; 3]]) -> Option<PanelRef<'a>> {
        if c.eye == u16::MAX
            || c.header != Header::Ready
            || c.show.iter().all(|&s| s == NO_TILE_SLOT)
        {
            return None;
        }
        let at = eyes.get(usize::from(c.eye))?;
        // The tiles the screen shows, with a quarter of the field of view to spare (the view is
        // the last frame's, and a quick turn moves the screen's edge well past a few percent:
        // at 6 % a column of the next panel came a few frames late, a hole the colour of the
        // sky): the others are not worth projecting.
        let v = visible_in(usize::from(c.heading), &self.view, 20, PICTURE_ROWS / 8)?;
        Some(PanelRef {
            eye: [i32::from(at[0]), i32::from(at[1]), i32::from(at[2])],
            heading: c.heading,
            depth: &c.depth,
            slots: &c.show,
            rooms: &c.rooms,
            first_col: v.first_col as u8,
            last_col: v.last_col as u8,
            first_row: v.first_row as u8,
            last_row: v.last_row as u8,
        })
    }

    /// The panels to draw this frame, the ones of the eyes the camera has left first (the eye in
    /// use draws over them), as many as `out` holds. The eye in use keeps its places: the
    /// panels of the eyes left give way when `out` is short.
    pub fn drawables<'a>(&'a self, eyes: &[[i16; 3]], out: &mut [Option<PanelRef<'a>>]) -> usize {
        let mut current = [None; HEADINGS];
        let mut kept = 0;
        for c in self.cache.iter().filter(|c| c.eye == self.eye) {
            if let Some(panel) = self.drawable(c, eyes) {
                if kept < current.len() {
                    current[kept] = Some(panel);
                    kept += 1;
                }
            }
        }
        let room = out.len().saturating_sub(kept);
        let mut count = 0;
        for c in self.cache.iter().filter(|c| c.eye != self.eye) {
            if count < room {
                if let Some(panel) = self.drawable(c, eyes) {
                    out[count] = Some(panel);
                    count += 1;
                }
            }
        }
        for panel in current.iter().take(kept.min(out.len() - count)) {
            out[count] = *panel;
            count += 1;
        }
        count
    }

    /// Stop every read (on leaving gameplay) and forget every slot.
    pub fn release_all<T>(&mut self, transport: &mut T)
    where
        T: RoomTransport<Ticket = K>,
    {
        for stage in &self.stages {
            if let Stage::Busy(run) = stage {
                if !run.landed {
                    transport.cancel(run.ticket);
                }
            }
        }
        *self = Self::new();
    }
}

impl<K: Copy> Default for PanelStream<K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    const SECTOR: usize = psx_grid::render::SLOT_BYTES;
    /// Sectors between the blocks of the eyes a test puts on the disc.
    const SPAN: u32 = 40;

    /// The tile `k` of a panel, as the cook would have laid it out: 63 rows of 64 texels and a
    /// palette row, in a few runs that compress.
    fn picture(k: usize) -> Vec<u8> {
        (0..SECTOR)
            .map(|i| if i % 64 < 40 { 0 } else { (i / 64 + k) as u8 })
            .collect()
    }

    /// A tile nothing compresses.
    fn noise(k: usize) -> Vec<u8> {
        let mut x = k as u32 * 7919 + 1;
        (0..SECTOR)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect()
    }

    /// A disc of sectors, a read finishing on the poll after it was queued.
    struct Disc {
        bytes: Vec<u8>,
        reads: Vec<(u32, u32)>,
        polls: Vec<u32>,
    }

    impl Disc {
        /// `sectors` sectors, sector `n` filled with `n as u8`.
        fn new(sectors: usize) -> Self {
            let mut bytes = Vec::new();
            for n in 0..sectors {
                bytes.extend(core::iter::repeat_n(n as u8, SECTOR));
            }
            Self {
                bytes,
                reads: Vec::new(),
                polls: Vec::new(),
            }
        }

        /// Lay an eye's block at `sector` (disc sector, `base_lba` 4 included in the caller's
        /// arithmetic): the panels of `headings`, every tile stored, each depth byte `k` being
        /// `k`. With `noisy` the tiles do not compress and the block takes several groups.
        /// Returns the sectors the block takes.
        fn put_block(&mut self, sector: u32, headings: &[usize], noisy: bool) -> u32 {
            let panels: Vec<(usize, [u8; COLS])> = headings
                .iter()
                .map(|&h| (h, [(1 << ROWS) - 1; COLS]))
                .collect();
            self.put_panels(sector, &panels, noisy)
        }

        /// [`Self::put_block`] with the stored rows of each column given.
        fn put_panels(&mut self, sector: u32, panels: &[(usize, [u8; COLS])], noisy: bool) -> u32 {
            let depth: [u8; MESH] = core::array::from_fn(|k| k as u8);
            let rooms = [CELL_EMPTY; CELLS];
            let encoded: Vec<Vec<u8>> = (0..TILES)
                .map(|k| {
                    let tile = if noisy { noise(k) } else { picture(k) };
                    psx_grid::tile_lz::encode_tile(&tile)
                })
                .collect();
            let mut parts: [Option<psx_grid::eye_block::PanelPart<'_>>; HEADINGS] =
                Default::default();
            for &(h, cols) in panels {
                parts[h] = Some(psx_grid::eye_block::PanelPart {
                    cols,
                    depth: &depth,
                    rooms: &rooms,
                    tiles: core::array::from_fn(|k| {
                        (cols[k / ROWS] >> (k % ROWS) & 1 != 0).then(|| encoded[k].as_slice())
                    }),
                });
            }
            let block = if panels.is_empty() {
                // An eye that stores nothing still has its index sector.
                std::vec![0u8; SECTOR]
            } else {
                psx_grid::eye_block::build_block(&parts)
            };
            let at = (4 + sector as usize) * SECTOR;
            self.bytes[at..at + block.len()].copy_from_slice(&block);
            (block.len() / SECTOR) as u32
        }
    }

    impl RoomTransport for Disc {
        type Ticket = usize;

        unsafe fn submit(
            &mut self,
            lba: u32,
            sectors: u32,
            destination: *mut u32,
            _urgent: bool,
        ) -> Option<usize> {
            let from = lba as usize * SECTOR;
            let len = sectors as usize * SECTOR;
            // SAFETY: the stream promised room for `sectors` sectors.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.bytes[from..from + len].as_ptr(),
                    destination.cast::<u8>(),
                    len,
                );
            }
            self.reads.push((lba, sectors));
            self.polls.push(0);
            Some(self.reads.len() - 1)
        }

        fn poll(&mut self, ticket: usize) -> Poll {
            self.polls[ticket] += 1;
            if self.polls[ticket] < 2 {
                Poll::Pending
            } else {
                Poll::Done
            }
        }

        fn cancel(&mut self, _ticket: usize) {}

        fn begin_transfer(&mut self) {}
    }

    const FOCAL: i32 = 305;

    fn input<'a>(
        eyes: &'a [[i16; 3]],
        blocks: &'a [u32],
        eye: [i32; 3],
        forward: [i32; 2],
    ) -> PanelInput<'a> {
        PanelInput {
            eyes,
            blocks,
            sectors: &[],
            base_lba: 4,
            slots: SLOTS,
            eye,
            forward,
            pitch_tan_q12: 0,
            focal: FOCAL,
            half_screen: [160, 120],
            panel_focal: FOCAL * (COLS as i32 * TILE_TEXELS) / 320,
            spacing: 800,
        }
    }

    /// Run `ticks` updates and apply whatever landed, as the game does.
    fn run(
        stream: &mut PanelStream<usize>,
        disc: &mut Disc,
        window: &mut [u32],
        input: &PanelInput<'_>,
        ticks: usize,
    ) {
        for _ in 0..ticks {
            stream.update(disc, window, input);
            let (ups, count) = stream.uploads::<2>(2);
            stream.applied(&ups[..count]);
        }
    }

    fn drawn(stream: &PanelStream<usize>, eyes: &[[i16; 3]]) -> Vec<(u16, u8, usize)> {
        let mut out = [None; 9];
        let count = stream.drawables(eyes, &mut out);
        out[..count]
            .iter()
            .flatten()
            .map(|p| {
                let eye = eyes
                    .iter()
                    .position(|e| [i32::from(e[0]), i32::from(e[1]), i32::from(e[2])] == p.eye)
                    .unwrap() as u16;
                (
                    eye,
                    p.heading,
                    p.slots.iter().filter(|&&s| s != NO_TILE_SLOT).count(),
                )
            })
            .collect()
    }

    fn window() -> Vec<u32> {
        std::vec![0u32; WINDOW_WORDS]
    }

    #[test]
    fn the_columns_in_view_follow_the_camera() {
        let eyes = [[0i16; 3]];
        let blocks = [8u32];
        // Looking along heading 0: all its columns, none of the panels 90 degrees off.
        let along = input(&eyes, &blocks, [0; 3], [4096, 0]);
        let v = visible(0, &along).expect("in view");
        assert_eq!((v.first_col, v.last_col), (0, COLS - 1));
        assert!(visible(2, &along).is_none() && visible(6, &along).is_none());
        // Looking along heading 2, 45 degrees to the right of heading 1: the view is on that
        // panel's right edge.
        let turned = input(&eyes, &blocks, [0; 3], [0, 4096]);
        let v = visible(1, &turned).expect("heading 1 in view");
        assert_eq!(v.last_col, COLS - 1);
        assert!(v.first_col >= 1, "{v:?}");
        // The two panels either side of a view 22 degrees off both hold some of it.
        let between = input(&eyes, &blocks, [0; 3], [3784, 1568]);
        assert!(visible(0, &between).is_some() && visible(1, &between).is_some());
    }

    #[test]
    fn the_headings_in_view_for_a_camera_looking_towards_minus_z() {
        let eyes = [[0i16; 3]];
        let blocks = [8u32];
        // Looking along heading 6 (towards -z), a little towards -x.
        let view = input(&eyes, &blocks, [0; 3], [-556, -4059]);
        let seen: Vec<usize> = (0..HEADINGS)
            .filter(|&h| visible(h, &view).is_some())
            .collect();
        assert_eq!(seen, [5, 6, 7]);
        let v = visible(6, &view).unwrap();
        assert_eq!((v.first_col, v.last_col), (0, COLS - 1), "{v:?}");
    }

    #[test]
    fn the_rows_follow_the_pitch() {
        let eyes = [[0i16; 3]];
        let blocks = [8u32];
        let level = visible(0, &input(&eyes, &blocks, [0; 3], [4096, 0])).unwrap();
        let mut down = input(&eyes, &blocks, [0; 3], [4096, 0]);
        down.pitch_tan_q12 = -1500;
        let down = visible(0, &down).unwrap();
        assert!(down.last_row >= level.last_row);
        assert!(down.middle_row > level.middle_row);
        let mut up = input(&eyes, &blocks, [0; 3], [4096, 0]);
        up.pitch_tan_q12 = 1500;
        let up = visible(0, &up).unwrap();
        assert!(up.first_row <= level.first_row);
        assert!(up.middle_row < level.middle_row);
    }

    #[test]
    fn an_index_says_which_tiles_are_stored_and_in_which_group() {
        let mut disc = Disc::new(4 + 20);
        let sectors = disc.put_block(0, &[0, 3], true);
        assert!(
            sectors > GROUP_SECTORS as u32,
            "noise takes several groups: {sectors}"
        );
        let first = disc.bytes[4 * SECTOR..(4 + 1) * SECTOR].to_vec();
        let mut list = EyeList::EMPTY;
        list.parse(|k| first[k]);
        assert!(list.stored(0, 0) && list.stored(3, TILES - 1));
        assert!(!list.stored(1, 0));
        // Noise stores whole tiles, six to a group at most: the later ones sit in later groups.
        assert_eq!(list.group_of(0, 0), 0);
        assert!(list.group_of(3, TILES - 1) >= 1);
        assert_eq!(list.depth[3][5], 5);
    }

    #[test]
    fn the_tiles_of_the_panel_in_view_arrive_with_their_depth_mesh() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 5 + 60);
        disc.put_block(5, &[0], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let input = input(&eyes, &blocks, [0; 3], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &input, 200);
        let list = drawn(&stream, &eyes);
        assert_eq!(list.len(), 1, "{list:?}");
        let (eye, heading, tiles) = list[0];
        assert_eq!((eye, heading), (0, 0));
        // 5 columns, and the rows the view needs.
        let v = visible(0, &input).unwrap();
        assert_eq!(tiles, COLS * (v.last_row - v.first_row + 1), "{v:?}");
        let mut out = [None; 2];
        stream.drawables(&eyes, &mut out);
        let panel = out[0].unwrap();
        assert!(panel.depth.iter().enumerate().all(|(k, &d)| d == k as u8));
        // No sector was read twice.
        let mut sectors: Vec<u32> = disc
            .reads
            .iter()
            .flat_map(|&(lba, n)| lba..lba + n)
            .collect();
        let total = sectors.len();
        sectors.sort_unstable();
        sectors.dedup();
        assert_eq!(sectors.len(), total);
        assert_eq!(stream.counters.uploads as usize, tiles);
    }

    #[test]
    fn turning_to_the_next_heading_needs_nothing_read_the_panels_are_all_there() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 5 + 100);
        disc.put_block(5, &[0, 1], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let facing = input(&eyes, &blocks, [0; 3], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &facing, 200);
        let before = stream.counters.uploads;
        // Looking 40 degrees towards heading 1: its left columns are in view.
        let turned = input(&eyes, &blocks, [0; 3], [3138, 2632]);
        run(&mut stream, &mut disc, &mut window, &turned, 200);
        let list = drawn(&stream, &eyes);
        assert!(list.iter().any(|&(_, h, _)| h == 1), "{list:?}");
        assert!(list.iter().any(|&(_, h, _)| h == 0), "{list:?}");
        // Every panel of the eye was there before the turn: nothing is read for it.
        assert_eq!(stream.counters.uploads, before);
        assert_eq!(stream.counters.starved, 0);
    }

    #[test]
    fn a_change_of_eye_draws_the_old_tiles_until_the_new_ones_land() {
        let eyes = [[0i16; 3], [2000, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], false);
        disc.put_block(5 + SPAN, &[0], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        // Near the first eye: the second is far enough not to be kept ready.
        let at_first = input(&eyes, &blocks, [-1200, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &at_first, 200);
        assert_eq!(
            drawn(&stream, &eyes)
                .iter()
                .map(|d| d.0)
                .collect::<Vec<_>>(),
            [0]
        );
        // The camera jumps to the other eye: the first eye's tiles are drawn until the new
        // eye's land.
        let moved = input(&eyes, &blocks, [3200, 0, 0], [4096, 0]);
        for _ in 0..PASS_TICKS + 1 {
            stream.update(&mut disc, &mut window, &moved);
        }
        let list = drawn(&stream, &eyes);
        assert!(list.iter().any(|d| d.0 == 0), "{list:?}");
        run(&mut stream, &mut disc, &mut window, &moved, 300);
        let list = drawn(&stream, &eyes);
        assert_eq!(
            list.iter().map(|d| d.0).collect::<Vec<_>>(),
            [1],
            "{list:?}"
        );
    }

    #[test]
    fn the_next_eye_is_kept_ready_before_the_camera_crosses_to_it() {
        let eyes = [[0i16; 3], [1600, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], false);
        disc.put_block(5 + SPAN, &[0], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        // Nearer the first eye, but within half a spacing of the second.
        let between = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &between, 400);
        assert_eq!(stream.counters.eye, 0);
        let lbas: Vec<u32> = disc.reads.iter().map(|r| r.0).collect();
        assert!(lbas.iter().any(|&l| l >= 4 + 5 + SPAN), "{lbas:?}");
        // Crossing: the new eye's tiles are in already, so it draws at once.
        let crossed = input(&eyes, &blocks, [1000, 0, 0], [4096, 0]);
        for _ in 0..PASS_TICKS + 1 {
            stream.update(&mut disc, &mut window, &crossed);
        }
        assert_eq!(stream.counters.eye, 1);
        // The middle of the view was ready; the rest follows within a moment.
        assert!(stream.counters.missing < stream.counters.wanted);
        let list = drawn(&stream, &eyes);
        assert!(list.iter().any(|d| d.0 == 1), "{list:?}");
        run(&mut stream, &mut disc, &mut window, &crossed, 200);
        assert_eq!(stream.counters.missing, 0);
        let list = drawn(&stream, &eyes);
        assert_eq!(
            list.iter().map(|d| d.0).collect::<Vec<_>>(),
            [1],
            "{list:?}"
        );
    }

    #[test]
    fn the_camera_crosses_to_a_nearer_eye_at_once_and_never_waits_for_the_drive() {
        let eyes = [[0i16; 3], [800, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], false);
        disc.put_block(5 + SPAN, &[0], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        // Next to the first eye, with the second a spacing away (not kept ready).
        let at_first = input(&eyes, &blocks, [-100, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &at_first, 200);
        assert_eq!(stream.counters.eye, 0);
        assert!(stream.eye_loaded());
        // Past the middle by more than the hysteresis, with nothing of the second eye in
        // VRAM: the very next pass uses it.
        let crossed = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        for _ in 0..PASS_TICKS + 1 {
            stream.update(&mut disc, &mut window, &crossed);
        }
        assert_eq!(stream.counters.eye, 1);
        assert!(!stream.eye_loaded());
        // The first eye's tiles stand in, so nothing the screen shows is blank meanwhile.
        let list = drawn(&stream, &eyes);
        assert!(list.iter().any(|d| d.0 == 0 && d.2 > 0), "{list:?}");
        // And the second eye's own tiles replace them as they land.
        run(&mut stream, &mut disc, &mut window, &crossed, 300);
        assert!(stream.eye_loaded());
        let list = drawn(&stream, &eyes);
        assert_eq!(
            list.iter().map(|d| d.0).collect::<Vec<_>>(),
            [1],
            "{list:?}"
        );
    }

    #[test]
    fn the_eye_in_use_keeps_its_places_when_the_list_is_short() {
        let eyes = [[0i16; 3], [800, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], false);
        disc.put_block(5 + SPAN, &[0], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let at_first = input(&eyes, &blocks, [-100, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &at_first, 200);
        let crossed = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        // Until a moment when the new eye has some tiles in and the old one still stands in
        // for the rest.
        let mut both = false;
        for _ in 0..300 {
            run(&mut stream, &mut disc, &mut window, &crossed, 1);
            let eyes_drawn: Vec<u16> = drawn(&stream, &eyes).iter().map(|d| d.0).collect();
            if eyes_drawn.contains(&0) && eyes_drawn.contains(&1) {
                both = true;
                break;
            }
        }
        assert!(both, "the crossing never drew both eyes");
        let mut out = [None; 1];
        assert_eq!(stream.drawables(&eyes, &mut out), 1);
        let panel = out[0].expect("a panel");
        assert_eq!(
            panel.eye,
            [800, 0, 0],
            "the eye in use is drawn, not the one left"
        );
    }

    #[test]
    fn a_tile_is_never_drawn_from_two_eyes_while_a_crossing_settles() {
        let eyes = [[0i16; 3], [800, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], false);
        disc.put_block(5 + SPAN, &[0], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let at_first = input(&eyes, &blocks, [-100, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &at_first, 200);
        let crossed = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        let mut stand_ins = 0;
        for _ in 0..300 {
            run(&mut stream, &mut disc, &mut window, &crossed, 1);
            let mut out = [None; 9];
            let count = stream.drawables(&eyes, &mut out);
            let panels: Vec<_> = out[..count].iter().flatten().collect();
            for (a, first) in panels.iter().enumerate() {
                for second in &panels[a + 1..] {
                    if first.heading != second.heading || first.eye == second.eye {
                        continue;
                    }
                    for tile in 0..TILES {
                        assert!(
                            first.slots[tile] == NO_TILE_SLOT || second.slots[tile] == NO_TILE_SLOT,
                            "tile {tile} drawn from both eyes"
                        );
                    }
                }
            }
            stand_ins += panels.iter().filter(|p| p.eye == [0, 0, 0]).count();
        }
        assert!(stand_ins > 0, "the eye left never stood in");
    }

    #[test]
    fn a_tile_no_other_eye_could_stand_in_for_is_read_ahead_of_the_rest_of_its_eye() {
        let eyes = [[0i16; 3], [1600, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], false);
        disc.put_block(5 + SPAN, &[0], false);
        // The first eye stores nothing of the second column of the panel; the second does.
        disc.put_panels(5, &[(0, [(1 << ROWS) - 1, 0])], false);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let between = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &between, 400);
        assert_eq!(stream.counters.eye, 0);
        let ranks = |column: usize| -> Vec<i32> {
            stream
                .slots
                .iter()
                .filter(|s| s.state != SlotState::Free && s.eye == 1)
                .filter(|s| usize::from(s.tile) / ROWS == column)
                .map(|s| s.rank)
                .collect()
        };
        let (hole, covered) = (ranks(1), ranks(0));
        assert!(
            !hole.is_empty() && !covered.is_empty(),
            "{hole:?} {covered:?}"
        );
        assert!(hole.iter().any(|&r| r < PREFETCH / 2), "{hole:?}");
        assert!(covered.iter().all(|&r| r >= PREFETCH), "{covered:?}");
    }

    #[test]
    fn the_eye_kept_ready_stays_while_another_is_barely_nearer() {
        let eyes = [[0i16; 3], [700, 0, 0], [0, 0, 730]];
        let blocks = [5u32, 5 + SPAN, 5 + 2 * SPAN];
        let mut disc = Disc::new(4 + 5 + 3 * SPAN as usize + 10);
        for &block in &blocks {
            disc.put_block(block, &[0], false);
        }
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let at = |z: i32| input(&eyes, &blocks, [0, 0, z], [4096, 0]);
        // Next to the first eye, the one along x the nearest of the others.
        for _ in 0..PASS_TICKS + 1 {
            stream.update(&mut disc, &mut window, &at(0));
        }
        assert_eq!((stream.eye, stream.next), (0, Some(1)));
        // The one along z is nearer now, by less than an eighth of a spacing: no change.
        for _ in 0..PASS_TICKS + 1 {
            stream.update(&mut disc, &mut window, &at(40));
        }
        assert_eq!(stream.next, Some(1));
        // Clearly nearer: the eye kept ready follows.
        for _ in 0..PASS_TICKS + 1 {
            stream.update(&mut disc, &mut window, &at(200));
        }
        assert_eq!(stream.next, Some(2));
    }

    #[test]
    fn no_panel_means_no_tile_reads() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 40);
        // The list says nothing is stored.
        disc.put_block(5, &[], false);
        let mut stream = PanelStream::<usize>::new();
        let mut window = window();
        let input = input(&eyes, &blocks, [0; 3], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &input, 60);
        // Only the list itself was read.
        assert_eq!(disc.reads.len(), 1);
        assert_eq!(drawn(&stream, &eyes).len(), 0);
    }

    #[test]
    fn a_big_eye_loads_by_groups_and_every_tile_decodes_to_what_was_baked() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 5 + 80);
        let sectors = disc.put_block(5, &[0, 1], true);
        assert!(sectors > 2 * GROUP_SECTORS as u32 / 2, "{sectors}");
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let input = input(&eyes, &blocks, [0; 3], [4096, 0]);
        let mut checked = [[false; TILES]; 2];
        for _ in 0..600 {
            stream.update(&mut disc, &mut window, &input);
            let (ups, count) = stream.uploads::<3>(3);
            // SAFETY: a byte view of the window for the test.
            let bytes = unsafe {
                core::slice::from_raw_parts(window.as_ptr().cast::<u8>(), window.len() * 4)
            };
            for up in ups[..count].iter().flatten() {
                let slot = stream.slots[usize::from(up.slot)];
                let mut tile = [0u8; TILE_BYTES];
                psx_grid::tile_lz::decode_tile(
                    &bytes[up.at as usize..up.at as usize + usize::from(up.len)],
                    &mut tile,
                )
                .expect("decodes");
                let want = noise(usize::from(slot.tile));
                assert_eq!(
                    &tile[..],
                    &want[..],
                    "tile {} of heading {}",
                    slot.tile,
                    slot.heading
                );
                checked[usize::from(slot.heading)][usize::from(slot.tile)] = true;
            }
            stream.applied(&ups[..count]);
        }
        assert!(checked[0].iter().all(|&c| c) && checked[1].iter().all(|&c| c));
        assert!(stream.eye_loaded());
        // The index and the first groups came in one read of two groups; the rest in few.
        assert!(disc.reads.len() <= 3, "{:?}", disc.reads);
        assert_eq!(stream.counters.failed, 0);
    }

    #[test]
    fn landed_tiles_become_gpu_nodes_with_their_rows() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 5 + 80);
        disc.put_block(5, &[0], true);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let input = input(&eyes, &blocks, [0; 3], [4096, 0]);
        for _ in 0..200 {
            stream.update(&mut disc, &mut window, &input);
            let (ups, count) = stream.uploads::<2>(2);
            if count == 0 {
                continue;
            }
            let mut out = std::vec![0u32; crate::card_stream::upload_packet_words(count)];
            let (words, packets, bad) =
                write_tile_uploads(&ups[..count], &window, (0, 0), (0, 0), 7, &mut out);
            assert_eq!((words, packets, bad), (out.len(), 1 + count * 64, 0));
            // The last node written is the first picture's first row: its data is the first
            // eight words of the decoded tile.
            let up = ups[0].unwrap();
            let slot = stream.slots[usize::from(up.slot)];
            let want = noise(usize::from(slot.tile));
            let node = &out[out.len() - 12..];
            let row: Vec<u8> = node[4..].iter().flat_map(|w| w.to_le_bytes()).collect();
            assert_eq!(&row[..], &want[..32]);
            return;
        }
        panic!("nothing landed");
    }

    #[test]
    fn a_read_the_eye_in_use_waits_for_does_not_wait_for_prefetched_tiles_to_reach_the_gpu() {
        let eyes = [[0i16; 3], [1600, 0, 0], [8000, 0, 0]];
        let blocks = [5u32, 5 + SPAN, 5 + 2 * SPAN];
        let mut disc = Disc::new(4 + 5 + 3 * SPAN as usize + 40);
        disc.put_block(5, &[0], true);
        disc.put_block(5 + SPAN, &[0], true);
        disc.put_block(5 + 2 * SPAN, &[0], true);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        // The first eye loads and shows.
        let at_first = input(&eyes, &blocks, [-1200, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &at_first, 300);
        assert!(stream.eye_loaded());
        // Within reach of the second: its tiles are read ahead, and the GPU takes none of them.
        let between = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        for _ in 0..200 {
            stream.update(&mut disc, &mut window, &between);
        }
        assert!(
            !stream.stages.iter().any(|s| matches!(s, Stage::Free)),
            "both stages hold prefetched tiles"
        );
        // The camera jumps to the third eye: its index is queued at once, not after the GPU has
        // taken the second eye's tiles.
        let before = disc.reads.len();
        let far = input(&eyes, &blocks, [8000, 0, 0], [4096, 0]);
        for _ in 0..PASS_TICKS + 2 {
            stream.update(&mut disc, &mut window, &far);
        }
        assert!(stream.counters.preempted >= 1);
        let lba = |sector: u32| 4 + sector;
        assert!(
            disc.reads[before..]
                .iter()
                .any(|r| r.0 == lba(5 + 2 * SPAN)),
            "{:?}",
            &disc.reads[before..]
        );
    }

    #[test]
    fn an_eye_kept_ready_reads_its_index_alone_then_only_the_sectors_its_tiles_lie_in() {
        let eyes = [[0i16; 3], [1600, 0, 0]];
        let blocks = [5u32, 5 + SPAN];
        let mut disc = Disc::new(4 + 5 + 2 * SPAN as usize + 40);
        disc.put_block(5, &[0], true);
        // Two panels, so the block is several groups long and the camera sees one of them.
        let sectors = disc.put_block(5 + SPAN, &[0, 4], true);
        assert!(sectors > 2 * GROUP_SECTORS as u32, "{sectors}");
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        let between = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &between, 400);
        let second = 4 + blocks[1];
        let reads: Vec<(u32, u32)> = disc
            .reads
            .iter()
            .copied()
            .filter(|&(lba, _)| lba >= second && lba < second + sectors)
            .collect();
        // The first read of the eye is its index, one sector.
        assert_eq!(reads.first(), Some(&(second, 1)), "{reads:?}");
        // The heading the camera does not look at is never read: the eye's tiles that were
        // read are fewer sectors than the block.
        let read: u32 = reads.iter().map(|r| r.1).sum();
        assert!(read < sectors, "{read} of {sectors}: {reads:?}");
        assert!(
            stream
                .slots
                .iter()
                .any(|s| s.eye == 1 && s.state == SlotState::Shown),
            "the tiles in view were read"
        );
    }
}
