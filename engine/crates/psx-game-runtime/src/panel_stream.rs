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
//! * **The tiles.** For each heading whose panel can be in the view, the tile columns the
//!   field of view (and a margin to turn into) crosses and the rows the pitch looks at; those
//!   tiles that are stored take a slot, the ones nearest the middle of the view ranking first.
//!   A tile is one sector, found from the panel's first sector and which tiles precede it.
//! * **The panels' headers.** The sector before a panel's tiles holds its depth mesh; it is
//!   read like a tile and kept in a small cache of panels, which also holds which slot has
//!   which tile. A tile is drawn once its header is in.
//! * **A change of eye.** The tiles of the eye left are kept, and drawn where the new eye's
//!   tile of the same place is stored and not in VRAM yet, until it lands: the picture is
//!   never blank while the new one comes. They are the first to give up their slots.
//! * **Reads.** The window the pictures land in is cut into cells of one sector. Up to
//!   [`STAGES`] reads are in flight or landed and waiting for the GPU, each a run of up to
//!   [`RUN_CELLS`] consecutive sectors: the best ranked wanted sector and every other wanted
//!   sector within the same stretch (the cook lays the tiles of a panel one after another,
//!   column by column). A landed tile is uploaded by `card_stream::write_upload_packets` as GPU
//!   list nodes, so the GPU does the copy as it draws and the CPU never waits.
//!
//! A failed read frees its stage and the tile is asked for again at the next pass. Nothing
//! here waits.

use psx_grid::flood::octagonal_distance;
use psx_grid::render::{
    PanelRef, CELLS, CELL_EMPTY, CENTRAL_ROWS, COLS, HEADER_ROOMS_AT, HEADINGS, HORIZON_ROW, MESH,
    NO_TILE_SLOT, PICTURE_ROWS, ROWS, SLOTS as ATLAS_SLOTS, TILES, TILE_TEXELS, VIEW_COS_Q12,
    VIEW_SIN_Q12,
};

use crate::card_stream::{Upload, RUN_CELLS, STAGES};
use crate::grid_stream::{Poll, RoomTransport};

/// Sectors of a panel's header: its depth mesh.
/// Slots of the stream: the sky atlas's, and a second atlas's when there is one.
const SLOTS: usize = ATLAS_SLOTS + psx_grid::render::SECOND_ATLAS_SLOTS;
const HEADER_SECTORS: u32 = 1;
const OUTER_ROWS: u8 = !CENTRAL_ROWS & ((1 << ROWS) - 1);
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
/// Rank of a held tile nothing wants now.
const IDLE: i32 = 1 << 22;
/// A held tile is only evicted for a tile that ranks better by more than this.
const EVICT_MARGIN: i32 = 64;
/// Ticks a wanted sector may wait before it is read ahead of the better ranked ones.
const PATIENCE: u32 = 90;
/// Smallest smoothed step of the eye a pass, runtime units, that counts as walking.
const MIN_WALK: i32 = 3;
/// Sectors a read may read over, to start where the last one ended and so skip a seek.
const CHAIN_GAP: u32 = 6;
/// Wanted sectors a read over both stages' parts of the window must bring in beyond the best
/// stretch of one part's length for both to be taken.
const SPAN_GAIN: usize = 3;
/// Experiment: one read over both stages' parts of the window (it did not cut the reads).
const SPAN: bool = false;
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

/// What an eye's block says of its panels (its first sector): which tiles each heading
/// stores, and so where each panel and each tile of it is.
#[derive(Copy, Clone)]
struct EyeList {
    eye: u16,
    state: Header,
    used: u32,
    since: u32,
    cols: [[u8; COLS]; HEADINGS],
    /// Sector of each panel's header counted from the block's start, 0 for a heading with no
    /// panel.
    first: [u32; HEADINGS],
    /// Where the central rows' tiles of each panel start, and the other rows'.
    central: [u32; HEADINGS],
    outer: [u32; HEADINGS],
}

impl EyeList {
    const EMPTY: Self = Self {
        eye: u16::MAX,
        state: Header::Absent,
        used: 0,
        since: 0,
        cols: [[0; COLS]; HEADINGS],
        first: [0; HEADINGS],
        central: [0; HEADINGS],
        outer: [0; HEADINGS],
    };

    /// Fill from the list's sector.
    fn parse(&mut self, bytes: impl Fn(usize) -> u8) {
        // The block: this list, the headers of the panels there are, then the central rows'
        // tiles of each panel in turn, then the others'.
        let (mut present, mut central, mut outer) = (0u32, [0u32; HEADINGS], [0u32; HEADINGS]);
        let mut any = [false; HEADINGS];
        for h in 0..HEADINGS {
            let (mut c_count, mut o_count) = (0u32, 0u32);
            for c in 0..COLS {
                let b = bytes(h * COLS + c);
                self.cols[h][c] = b;
                any[h] |= b != 0;
                c_count += u32::from(b & CENTRAL_ROWS).count_ones();
                o_count += u32::from(b & OUTER_ROWS).count_ones();
            }
            central[h] = c_count;
            outer[h] = o_count;
            present += u32::from(any[h]);
        }
        let (mut header, mut c_next) = (1u32, 1 + present * HEADER_SECTORS);
        let mut o_next = c_next + central.iter().sum::<u32>();
        for h in 0..HEADINGS {
            if !any[h] {
                self.first[h] = 0;
                continue;
            }
            self.first[h] = header;
            header += HEADER_SECTORS;
            self.central[h] = c_next;
            c_next += central[h];
            self.outer[h] = o_next;
            o_next += outer[h];
        }
        self.state = Header::Ready;
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
    /// The panel's header sector, relative to the pictures' base.
    sector: u32,
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
        sector: 0,
        depth: [0; MESH],
        rooms: [CELL_EMPTY; CELLS],
        held: [NO_SLOT_INDEX; TILES],
        show: [NO_TILE_SLOT; TILES],
    };
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Kind {
    /// An eye's list of stored tiles.
    Eye,
    /// A panel's depth mesh.
    Header,
    Tile,
}

/// What a cell of a read holds.
#[derive(Copy, Clone)]
struct Entry {
    kind: Kind,
    eye: u16,
    heading: u8,
    tile: u8,
    slot: u8,
    cache: u8,
}

/// One read: consecutive sectors landing in consecutive cells of a stage.
#[derive(Copy, Clone)]
struct Run<K> {
    ticket: K,
    landed: bool,
    entries: [Option<Entry>; RUN_MAX],
    /// The read fills both stages' parts of the window: up to `RUN_MAX` sectors for one seek.
    span: bool,
}

/// Sectors of the longest read (`RUN_CELLS * STAGES` when `SPAN` is on).
const RUN_MAX: usize = RUN_CELLS;

#[derive(Copy, Clone)]
enum Stage<K> {
    Free,
    Busy(Run<K>),
    /// Taken by the long read of the stage before.
    Spanned,
}

/// What a call to [`PanelStream::update`] knows about the level and the camera.
pub struct PanelInput<'a> {
    /// The bake eyes, runtime units.
    pub eyes: &'a [[i16; 3]],
    /// Per eye, the sector of its block (relative to `base_lba`), 0 for an eye with no panel.
    /// The block starts with a sector that lists which tiles each heading stores, then the
    /// eight panels, each its header sector and its stored tiles.
    pub blocks: &'a [u32],
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

/// A panel's place on the disc: `[header sector, first central tile, first other tile, stored
/// columns]`, the columns being one row mask each (`COLS` bytes in the last two words).
type PanelEntry = [u32; 5];

/// The row masks of a panel's columns, four to a word.
fn pack_cols(cols: &[u8; COLS]) -> (u32, u32) {
    let mut words = [0u32; 2];
    for (i, &c) in cols.iter().enumerate() {
        words[i / 4] |= u32::from(c) << (8 * (i % 4));
    }
    (words[0], words[1])
}

/// The stored tiles of a panel, one row mask a column.
fn cols_of(entry: &PanelEntry) -> [u8; COLS] {
    core::array::from_fn(|i| (entry[3 + i / 4] >> (8 * (i % 4))) as u8)
}

/// The sector of `tile` of a panel: among the central rows' tiles or the others', column by
/// column.
fn tile_sector(entry: &PanelEntry, tile: usize) -> u32 {
    let cols = cols_of(entry);
    let (col, row) = (tile / ROWS, tile % ROWS);
    let (mask, base) = if CENTRAL_ROWS >> row & 1 != 0 {
        (CENTRAL_ROWS, entry[1])
    } else {
        (OUTER_ROWS, entry[2])
    };
    let before: u32 = cols[..col]
        .iter()
        .map(|b| (b & mask).count_ones())
        .sum::<u32>()
        + (cols[col] & mask & ((1u8 << row) - 1)).count_ones();
    base + before
}

fn stored(entry: &PanelEntry, tile: usize) -> bool {
    entry[0] != 0 && cols_of(entry)[tile / ROWS] >> (tile % ROWS) & 1 != 0
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
    /// Where the eye was at the last pass (plan), and its smoothed step a pass.
    last_eye: [i32; 2],
    have_last: bool,
    velocity: [i32; 2],
    /// Sector the drive ends at once the reads queued so far are done (0 before the first).
    tail: u32,
}

impl<K: Copy> PanelStream<K> {
    /// A stream holding nothing.
    pub const fn new() -> Self {
        Self {
            counters: PanelCounters {
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
            },
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
            last_eye: [0; 2],
            have_last: false,
            velocity: [0; 2],
            tail: 0,
        }
    }

    /// Drive the stream one tick: poll the reads in flight, every few ticks re-decide the eye
    /// and the wanted tiles, and start the next reads. `window` is
    /// [`crate::card_stream::CARD_WINDOW_WORDS`] words of memory the reads land in, and must
    /// stay put while the stream lives.
    pub fn update<T>(&mut self, transport: &mut T, window: &mut [u32], input: &PanelInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        if window.len() < crate::card_stream::CARD_WINDOW_WORDS
            || input.blocks.is_empty()
            || input.eyes.is_empty()
        {
            return;
        }
        self.view = input.view();
        self.limit = input.slots.clamp(ATLAS_SLOTS, SLOTS);
        self.poll(transport, window);
        if self.ticks.is_multiple_of(PASS_TICKS) {
            self.pass(input);
        }
        self.ticks = self.ticks.wrapping_add(1);
        self.start_reads(transport, window, input);
    }

    /// Retire the reads that finished, taking the headers out of the window.
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
                    self.counters.landed += 1;
                    run.landed = true;
                    for (cell, entry) in run.entries.iter_mut().enumerate() {
                        let Some(e) = *entry else { continue };
                        if e.kind == Kind::Eye {
                            *entry = None;
                            let list = &mut self.lists[usize::from(e.cache)];
                            if list.eye == e.eye {
                                let from =
                                    (index * RUN_CELLS + cell) * crate::card_stream::SECTOR_WORDS;
                                let bytes = |k: usize| window[from + k / 4].to_le_bytes()[k % 4];
                                list.parse(bytes);
                            }
                            continue;
                        }
                        if e.kind == Kind::Tile {
                            let slot = &self.slots[usize::from(e.slot)];
                            if METER && slot.state == SlotState::Pending && slot.since != 0 {
                                self.counters.wait_read += self.ticks.wrapping_sub(slot.since);
                            }
                        }
                        if e.kind != Kind::Header {
                            continue;
                        }
                        *entry = None;
                        let cached = &mut self.cache[usize::from(e.cache)];
                        if cached.eye != e.eye || cached.heading != e.heading {
                            continue;
                        }
                        let from = (index * RUN_CELLS + cell) * crate::card_stream::SECTOR_WORDS;
                        for (k, word) in window[from..from + MESH.div_ceil(4)].iter().enumerate() {
                            for (b, byte) in word.to_le_bytes().iter().enumerate() {
                                if let Some(d) = cached.depth.get_mut(4 * k + b) {
                                    *d = *byte;
                                }
                            }
                        }
                        for (k, room) in cached.rooms.iter_mut().enumerate() {
                            let at = from + (HEADER_ROOMS_AT + k) / 4;
                            *room = window[at].to_le_bytes()[(HEADER_ROOMS_AT + k) % 4];
                        }
                        cached.header = Header::Ready;
                    }
                    if run.entries.iter().all(Option::is_none) {
                        let span = run.span;
                        self.free_stage(index, span);
                    }
                }
                Poll::Cancelled | Poll::Failed => {
                    self.counters.failed += 1;
                    for e in run.entries.iter().flatten() {
                        match e.kind {
                            Kind::Eye => {
                                let list = &mut self.lists[usize::from(e.cache)];
                                if list.eye == e.eye {
                                    list.state = Header::Wanted;
                                }
                            }
                            Kind::Header => {
                                let cached = &mut self.cache[usize::from(e.cache)];
                                if cached.eye == e.eye && cached.heading == e.heading {
                                    cached.header = Header::Wanted;
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
                    let span = run.span;
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
        let best = (0..input.eyes.len())
            .min_by_key(|&eye| distance(eye))
            .unwrap_or(0);
        // The camera crosses to a nearer eye only once every picture of that eye is in VRAM, so
        // that nothing is ever blank: until then the eye in use keeps serving it (the pictures
        // drift a little further from the real view), unless it is out of reach altogether.
        let current = usize::from(self.eye);
        let lost = !self.have_eye || current >= input.eyes.len();
        let nearer = !lost && distance(best) + input.spacing / 4 < distance(current);
        if lost
            || (nearer
                && (self.eye_ready(best as u16, input) || distance(current) > 2 * input.spacing))
        {
            self.eye = best as u16;
            self.have_eye = true;
        }
        let waiting = nearer && usize::from(self.eye) != best;
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
        let here = distance(usize::from(self.eye));
        self.next = (0..input.eyes.len())
            .filter(|&eye| eye != usize::from(self.eye))
            .min_by_key(|&eye| to_ahead(eye))
            .filter(|&eye| distance(eye) < here + input.spacing)
            .map(|eye| eye as u16);
        if waiting {
            self.next = Some(best as u16);
        }
    }

    /// Whether every picture of the eye in use is in VRAM, as of the last pass.
    pub fn eye_loaded(&self) -> bool {
        self.loaded
    }

    /// Whether every stored tile of every panel of `eye` is in VRAM.
    fn eye_ready(&self, eye: u16, input: &PanelInput<'_>) -> bool {
        let Some(list) = self
            .lists
            .iter()
            .position(|l| l.eye == eye && l.state == Header::Ready)
        else {
            return false;
        };
        for heading in 0..HEADINGS {
            let Some(entry) = self.entry_of(list, heading, input) else {
                continue;
            };
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
            for tile in 0..TILES {
                if !stored(&entry, tile) {
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
            if held.state == SlotState::Wanted && held.eye != eye && Some(held.eye) != self.next {
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
        self.want_tiles(input, eye, 0, false, &mut wanted, &mut starved);
        if let Some(next) = self.next {
            self.want_tiles(input, next, PREFETCH, true, &mut wanted, &mut starved);
        }
        self.rebuild_show(input, eye);
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
        self.loaded = self.have_eye && self.eye_ready(self.eye, input);
        if METER {
            self.count_visible(input, eye);
        }
    }

    /// Tally the tiles the screen shows against the ones that have a picture.
    fn count_visible(&mut self, input: &PanelInput<'_>, eye: u16) {
        let Some(list) = self
            .lists
            .iter()
            .position(|l| l.eye == eye && l.state == Header::Ready)
        else {
            return;
        };
        let (mut seen, mut absent) = (0u32, 0u32);
        for heading in 0..HEADINGS {
            let Some(entry) = self.entry_of(list, heading, input) else {
                continue;
            };
            let Some(v) = visible_in(heading, &self.view, 16, 0) else {
                continue;
            };
            let Some(ci) = self
                .cache
                .iter()
                .position(|c| c.eye == eye && c.heading == heading as u8)
            else {
                // Stored tiles of a panel nothing has asked for yet.
                for col in v.first_col..=v.last_col {
                    for row in v.first_row..=v.last_row {
                        let tile = col * ROWS + row;
                        if stored(&entry, tile) {
                            seen += 1;
                            absent += 1;
                        }
                    }
                }
                continue;
            };
            for col in v.first_col..=v.last_col {
                for row in v.first_row..=v.last_row {
                    let tile = col * ROWS + row;
                    if stored(&entry, tile) {
                        seen += 1;
                        absent += u32::from(self.cache[ci].show[tile] == NO_TILE_SLOT);
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
        self.lists[at] = EyeList {
            eye,
            state: Header::Wanted,
            since: self.ticks.max(1),
            ..EyeList::EMPTY
        };
        Some(at)
    }

    /// The entry of the panel of `heading` of the eye whose list is `list`: `[first sector,
    /// stored columns]`, in the shape [`tile_sector`] and [`stored`] read.
    fn entry_of(&self, list: usize, heading: usize, input: &PanelInput<'_>) -> Option<PanelEntry> {
        let l = &self.lists[list];
        let block = input
            .blocks
            .get(usize::from(l.eye))
            .copied()
            .filter(|&b| b != 0)?;
        if l.state != Header::Ready || l.first[heading] == 0 {
            return None;
        }
        let c = l.cols[heading];
        Some([
            block + l.first[heading],
            block + l.central[heading],
            block + l.outer[heading],
            pack_cols(&c).0,
            pack_cols(&c).1,
        ])
    }

    /// Ask for the tiles of `eye` the camera can see, ranked from `base` up by how far each is
    /// from the middle of the view.
    fn want_tiles(
        &mut self,
        input: &PanelInput<'_>,
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
            let Some(entry) = self.entry_of(list, heading, input) else {
                continue;
            };
            // Every panel of the eye is kept, whichever way the camera looks: it may turn at any
            // moment, and a turn must not wait for the disc. The view only orders the reads.
            let seen = visible(heading, input);
            let Some(ci) = self.cache_index(eye, heading as u8) else {
                continue;
            };
            self.cache[usize::from(ci)].used = self.ticks.max(1);
            self.cache[usize::from(ci)].rank = i32::MAX;
            if self.cache[usize::from(ci)].header == Header::Absent {
                self.cache[usize::from(ci)].header = Header::Wanted;
                self.cache[usize::from(ci)].since = self.ticks.max(1);
                self.cache[usize::from(ci)].sector = entry[0];
            }
            for col in 0..COLS {
                for row in 0..ROWS {
                    let tile = col * ROWS + row;
                    if !stored(&entry, tile) {
                        continue;
                    }
                    *wanted += u32::from(!prefetch);
                    // The tiles the camera sees first, the others after them.
                    let rank = base
                        + match seen {
                            Some(view) => {
                                let middle = ((col as i32 * TILE_TEXELS + TILE_TEXELS / 2
                                    - COLS_CENTRE)
                                    << 12)
                                    / input.panel_focal.max(1);
                                let across = (middle - view.tan_yaw).abs();
                                let down = (row as i32 * PICTURE_ROWS + PICTURE_ROWS / 2
                                    - view.middle_row)
                                    .abs();
                                across + 4 * down
                            }
                            None => OFF_VIEW,
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
    fn rebuild_show(&mut self, input: &PanelInput<'_>, eye: u16) {
        for c in 0..CACHE {
            for tile in 0..TILES {
                let cached = &self.cache[c];
                let held = cached.held[tile];
                let mut show = NO_TILE_SLOT;
                if cached.eye != u16::MAX && cached.header == Header::Ready && held != NO_SLOT_INDEX
                {
                    let slot = &self.slots[usize::from(held)];
                    if slot.state == SlotState::Shown {
                        if cached.eye == eye {
                            if slot.wanted {
                                show = held;
                            }
                        } else {
                            // Stale: only while the tile of the eye in use is not there.
                            let entry =
                                self.lists.iter().position(|l| l.eye == eye).and_then(|l| {
                                    self.entry_of(l, usize::from(cached.heading), input)
                                });
                            let replaced = self
                                .cache
                                .iter()
                                .find(|o| o.eye == eye && o.heading == cached.heading)
                                .is_some_and(|o| {
                                    let s = o.held[tile];
                                    s != NO_SLOT_INDEX
                                        && self.slots[usize::from(s)].state == SlotState::Shown
                                        && o.header == Header::Ready
                                });
                            // While the new eye's list is not in, its tile is not known to be
                            // stored: the old one stands in.
                            let new_stored = entry.is_none_or(|e| stored(&e, tile));
                            if new_stored && !replaced {
                                show = held;
                            }
                        }
                    }
                }
                self.cache[c].show[tile] = show;
            }
        }
    }

    /// Queue reads for the headers and tiles that need one into the free stages: the best
    /// ranked sector, and every other wanted sector in the same stretch of the disc.
    fn start_reads<T>(&mut self, transport: &mut T, window: &mut [u32], input: &PanelInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        loop {
            let Some(stage) = self.stages.iter().position(|s| matches!(s, Stage::Free)) else {
                return;
            };
            // Wanted sectors: (rank, sector, kind, which, waited).
            let mut wanted = [(0i32, 0u32, Kind::Tile, 0u8, 0u32); SLOTS + CACHE + EYES];
            let mut count = 0;
            for (index, l) in self.lists.iter().enumerate() {
                if l.eye == u16::MAX || l.state != Header::Wanted {
                    continue;
                }
                let Some(&block) = input.blocks.get(usize::from(l.eye)).filter(|&&b| b != 0) else {
                    continue;
                };
                // Nothing of the eye can be asked for until its list is in.
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
                );
                count += 1;
            }
            for (index, c) in self.cache.iter().enumerate() {
                if c.eye == u16::MAX || c.header != Header::Wanted || c.sector == 0 {
                    continue;
                }
                // A header gates every tile of its panel: it goes before them.
                wanted[count] = (
                    c.rank.saturating_sub(1),
                    c.sector,
                    Kind::Header,
                    index as u8,
                    self.ticks.wrapping_sub(c.since),
                );
                count += 1;
            }
            for (index, s) in self.slots.iter().enumerate() {
                if s.state != SlotState::Wanted {
                    continue;
                }
                // The tile's sector, from its panel's place in its eye's block.
                let Some(sector) = self
                    .lists
                    .iter()
                    .position(|l| l.eye == s.eye)
                    .and_then(|l| self.entry_of(l, usize::from(s.heading), input))
                    .map(|entry| tile_sector(&entry, usize::from(s.tile)))
                else {
                    continue;
                };
                let since = if s.since == 0 { self.ticks } else { s.since };
                wanted[count] = (
                    s.rank,
                    sector,
                    Kind::Tile,
                    index as u8,
                    self.ticks.wrapping_sub(since),
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
                return;
            }
            // The best ranked sector, unless one has waited out its patience.
            let head = wanted
                .iter()
                .filter(|w| w.4 > PATIENCE)
                .max_by_key(|w| (w.4, core::cmp::Reverse(w.0)))
                .or_else(|| {
                    // The most urgent tier first (the lists and headers, the tiles of the eye in
                    // use, the ones kept ready), and within it the sector the drive reaches
                    // soonest: the one that continues the last read, then the next ones
                    // ahead, then those behind. The drive then sweeps the wanted sectors, and a
                    // read that starts where the last ended costs no seek.
                    let tail = self.tail;
                    wanted.iter().min_by_key(|w| {
                        let tier = if w.0 < 0 {
                            0
                        } else if w.0 < PREFETCH {
                            1
                        } else {
                            2
                        };
                        let reach = if tail == 0 || w.1 > tail {
                            w.1.saturating_sub(tail)
                        } else {
                            1 << 24 | (tail - w.1)
                        };
                        (tier, reach, w.0)
                    })
                })
                .map(|w| w.1)
                .unwrap_or(0);
            // A read that starts where the last one ended continues without a seek, which
            // costs far more than the few sectors it reads over to get there: chain onto the
            // last read when the head is within [`CHAIN_GAP`] sectors of its end.
            let chain = self.tail != 0
                && head > self.tail
                && head - self.tail - 1 <= CHAIN_GAP
                && head - self.tail < RUN_CELLS as u32;
            // Otherwise the stretch of RUN_CELLS sectors holding the head and the most other
            // wanted sectors, the earliest among equals.
            let mut start = if chain { self.tail + 1 } else { head };
            let mut most = 0;
            if !chain {
                for back in 0..RUN_CELLS as u32 {
                    let Some(lo) = head.checked_sub(back).filter(|&lo| lo > 0) else {
                        break;
                    };
                    let inside = wanted
                        .iter()
                        .filter(|w| w.1 >= lo && w.1 < lo + RUN_CELLS as u32)
                        .count();
                    if inside > most {
                        most = inside;
                        start = lo;
                    }
                }
            }
            // With both stages free, a read may take both parts of the window: when it brings
            // in several more wanted sectors than the stretch of one, the one seek is worth it.
            let both_free = self.stages.iter().all(|s| matches!(s, Stage::Free));
            let mut span = false;
            if SPAN && both_free && !chain {
                let (mut start_long, mut most_long) = (start, 0);
                for back in 0..RUN_MAX as u32 {
                    let Some(lo) = head.checked_sub(back).filter(|&lo| lo > 0) else {
                        break;
                    };
                    let inside = wanted
                        .iter()
                        .filter(|w| w.1 >= lo && w.1 < lo + RUN_MAX as u32)
                        .count();
                    if inside > most_long {
                        most_long = inside;
                        start_long = lo;
                    }
                }
                if most_long >= most + SPAN_GAIN {
                    span = true;
                    start = start_long;
                }
            }
            let stage = if span { 0 } else { stage };
            let stretch = start..start + if span { RUN_MAX } else { RUN_CELLS } as u32;
            let first = if chain {
                start
            } else {
                wanted
                    .iter()
                    .filter(|w| stretch.contains(&w.1))
                    .map(|w| w.1)
                    .min()
                    .unwrap_or(head)
            };
            let last = wanted
                .iter()
                .filter(|w| stretch.contains(&w.1))
                .map(|w| w.1)
                .max()
                .unwrap_or(head);
            let mut entries = [None; RUN_MAX];
            let mut riders = 0;
            for &(_, sector, kind, which, _) in wanted.iter().filter(|w| stretch.contains(&w.1)) {
                let cell = (sector - first) as usize;
                if entries[cell].is_some() {
                    continue;
                }
                entries[cell] = Some(match kind {
                    Kind::Eye => {
                        let l = &self.lists[usize::from(which)];
                        Entry {
                            kind,
                            eye: l.eye,
                            heading: 0,
                            tile: 0,
                            slot: NO_SLOT_INDEX,
                            cache: which,
                        }
                    }
                    Kind::Header => {
                        let c = &self.cache[usize::from(which)];
                        Entry {
                            kind,
                            eye: c.eye,
                            heading: c.heading,
                            tile: 0,
                            slot: NO_SLOT_INDEX,
                            cache: which,
                        }
                    }
                    Kind::Tile => {
                        let s = &self.slots[usize::from(which)];
                        Entry {
                            kind,
                            eye: s.eye,
                            heading: s.heading,
                            tile: s.tile,
                            slot: which,
                            cache: s.cache,
                        }
                    }
                });
                riders += 1;
            }
            transport.begin_transfer();
            // SAFETY: the window is memory the caller keeps for the stream's life, `stage` is
            // one of its STAGES parts of RUN_CELLS sectors each, the run is at most RUN_CELLS
            // sectors, and the part is not reused until the read is reported finished.
            let destination = unsafe {
                window
                    .as_mut_ptr()
                    .add(stage * RUN_CELLS * crate::card_stream::SECTOR_WORDS)
            };
            let lba = input.base_lba + first;
            match unsafe { transport.submit(lba, last - first + 1, destination, false) } {
                Some(ticket) => {
                    for entry in entries.iter().flatten() {
                        match entry.kind {
                            Kind::Eye => {
                                self.lists[usize::from(entry.cache)].state = Header::Pending;
                            }
                            Kind::Header => {
                                self.cache[usize::from(entry.cache)].header = Header::Pending;
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
                        entries,
                        span,
                    });
                    if span {
                        for part in stage + 1..STAGES {
                            self.stages[part] = Stage::Spanned;
                        }
                    }
                    self.counters.submitted += 1;
                    self.tail = last;
                    self.counters.last_first = first;
                    self.counters.last_sectors = last - first + 1;
                    self.counters.sectors += last - first + 1;
                    let _ = riders;
                }
                None => {
                    self.counters.refused += 1;
                    return;
                }
            }
        }
    }

    /// The landed tiles waiting for the GPU, up to `N`, for slots that still hold the tile
    /// they were read for.
    pub fn uploads<const N: usize>(&mut self) -> ([Option<Upload>; N], usize) {
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
            for (cell, entry) in run.entries.iter_mut().enumerate() {
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
                if count < N {
                    out[count] = Some(Upload::new(e.slot, (index * RUN_CELLS + cell) as u8));
                    count += 1;
                }
            }
            if run.entries.iter().all(Option::is_none) {
                freed[index] = run.span;
                *stage = Stage::Free;
            }
        }
        for (index, &spanned) in freed.iter().enumerate() {
            if spanned && index + 1 < STAGES {
                self.stages[index + 1] = Stage::Free;
            }
        }
        (out, count)
    }

    /// The uploads in `taken` are in a frame's list: VRAM now holds their tiles and their cells
    /// are free again.
    pub fn applied(&mut self, taken: &[Option<Upload>]) {
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
            // A cell past the first part of the window is the long read's when that stage is.
            let id = usize::from(upload.cell);
            let index = if matches!(self.stages[id / RUN_CELLS], Stage::Spanned) {
                0
            } else {
                id / RUN_CELLS
            };
            let cell = id - index * RUN_CELLS;
            if let Stage::Busy(run) = &mut self.stages[index] {
                run.entries[cell] = None;
                if run.entries.iter().all(Option::is_none) {
                    let span = run.span;
                    self.free_stage(index, span);
                }
            }
            self.counters.uploads += 1;
        }
        // The tiles that landed show from the next frame on.
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
                        self.cache[c].show[tile] = held;
                    }
                }
            }
        }
    }

    /// The panels to draw this frame, the ones of an eye the camera has left first (the eye in
    /// use draws over them), as many as `out` holds.
    pub fn drawables<'a>(&'a self, eyes: &[[i16; 3]], out: &mut [Option<PanelRef<'a>>]) -> usize {
        let mut count = 0;
        for stale in [true, false] {
            for c in &self.cache {
                if c.eye == u16::MAX
                    || c.header != Header::Ready
                    || (c.eye != self.eye) != stale
                    || c.show.iter().all(|&s| s == NO_TILE_SLOT)
                {
                    continue;
                }
                let Some(at) = eyes.get(usize::from(c.eye)) else {
                    continue;
                };
                if count < out.len() {
                    // The tiles the screen shows, with a few percent to spare: the others
                    // are not worth projecting.
                    let Some(v) =
                        visible_in(usize::from(c.heading), &self.view, 17, PICTURE_ROWS / 8)
                    else {
                        continue;
                    };
                    out[count] = Some(PanelRef {
                        eye: [i32::from(at[0]), i32::from(at[1]), i32::from(at[2])],
                        heading: c.heading,
                        depth: &c.depth,
                        slots: &c.show,
                        rooms: &c.rooms,
                        first_col: v.first_col as u8,
                        last_col: v.last_col as u8,
                        first_row: v.first_row as u8,
                        last_row: v.last_row as u8,
                    });
                    count += 1;
                }
            }
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
    /// Sectors of a panel with every tile stored.
    const FULL: u32 = 1 + (COLS * ROWS) as u32;

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

        /// Lay an eye's block at `sector` (disc sector, `base_lba` 4 included in the
        /// caller's arithmetic): the list of stored tiles, then the panels of `headings`,
        /// every tile stored, each header's depth byte `k` being `k`.
        fn put_block(&mut self, sector: u32, headings: &[usize]) {
            let at = (4 + sector as usize) * SECTOR;
            for b in &mut self.bytes[at..at + SECTOR] {
                *b = 0;
            }
            for &h in headings {
                for c in 0..COLS {
                    self.bytes[at + h * COLS + c] = (1 << ROWS) - 1;
                }
            }
            for (n, &h) in headings.iter().enumerate() {
                let _ = h;
                let header = at + (1 + n) * SECTOR;
                for k in 0..MESH {
                    self.bytes[header + k] = k as u8;
                }
            }
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
            let (ups, count) = stream.uploads::<2>();
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
        std::vec![0u32; crate::card_stream::CARD_WINDOW_WORDS]
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
    fn a_tile_sector_counts_the_stored_tiles_before_it() {
        // Column 0: rows 0 and 2; column 1: row 1; header 10, tiles from 20 (every row of a
        // panel is a central one), the others from 30.
        let entry = [10u32, 20, 30, 0b0000_0010_0000_0101, 0];
        assert!(stored(&entry, 0) && stored(&entry, 2) && stored(&entry, ROWS + 1));
        assert!(!stored(&entry, 1) && !stored(&entry, ROWS));
        assert_eq!(tile_sector(&entry, 0), 20);
        assert_eq!(tile_sector(&entry, 2), 21);
        assert_eq!(tile_sector(&entry, ROWS + 1), 22);
    }

    #[test]
    fn the_tiles_of_the_panel_in_view_arrive_with_their_depth_mesh() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 5 + 60);
        disc.put_block(5, &[0]);
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
        disc.put_block(5, &[0, 1]);
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
        let blocks = [5u32, 5 + 1 + FULL];
        let mut disc = Disc::new(4 + 5 + 120);
        disc.put_block(5, &[0]);
        disc.put_block(5 + 1 + FULL, &[0]);
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
        let blocks = [5u32, 5 + 1 + FULL];
        let mut disc = Disc::new(4 + 5 + 120);
        disc.put_block(5, &[0]);
        disc.put_block(5 + 1 + FULL, &[0]);
        let mut window = window();
        let mut stream = PanelStream::<usize>::new();
        // Nearer the first eye, but within half a spacing of the second.
        let between = input(&eyes, &blocks, [650, 0, 0], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &between, 400);
        assert_eq!(stream.counters.eye, 0);
        let lbas: Vec<u32> = disc.reads.iter().map(|r| r.0).collect();
        assert!(lbas.iter().any(|&l| l >= 4 + 5 + 1 + FULL), "{lbas:?}");
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
    fn no_panel_means_no_tile_reads() {
        let eyes = [[0i16; 3]];
        let blocks = [5u32];
        let mut disc = Disc::new(4 + 40);
        // The list says nothing is stored.
        disc.put_block(5, &[]);
        let mut stream = PanelStream::<usize>::new();
        let mut window = window();
        let input = input(&eyes, &blocks, [0; 3], [4096, 0]);
        run(&mut stream, &mut disc, &mut window, &input, 60);
        // Only the list itself was read.
        assert_eq!(disc.reads.len(), 1);
        assert_eq!(drawn(&stream, &eyes).len(), 0);
    }
}
