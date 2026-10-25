//! Keeping the far cards' pictures in VRAM while the player walks.
//!
//! A far card ([`psx_grid::render::cards`]) shows one baked view of its room,
//! the one nearest the direction the eye is in. The sky atlas is cut into
//! [`SLOTS`] slots, each the size of one picture (one 2 KiB disc sector, a
//! `16 x 64` halfword rectangle with its palette in the last row). This
//! module decides which room holds which slot, which view each slot should
//! show, and reads the missing views off the disc through the asynchronous
//! transport the rooms stream through ([`RoomTransport`]).
//!
//! * **Membership.** A room with a card holds a slot while it is within
//!   `enter` units of the eye (plan distance) and gives it up past `leave`
//!   (the gap is the hysteresis). `enter` reaches past the distance cards
//!   show from, so a room has its slot, and its picture on the way, before
//!   its card is drawn. With no slot free the room with the worst *rank*
//!   (see below) gives its slot up to a better one.
//! * **Rank.** A room inside the card distance (`far`) always outranks one
//!   beyond it. Among the rooms of a class the nearer one ranks better, and
//!   one the eye is walking towards ranks better by up to `lead` units, so
//!   slots and reads go to the rooms about to come into view first.
//! * **Views.** Each slot wants the view [`view_toward`] picks for the eye,
//!   kept while [`view_keep`] still says it serves. A slot whose shown view
//!   is not the wanted one is read next; one that shows nothing at all
//!   (the card would be blank, or is about to be) before one that only shows
//!   the wrong side.
//! * **Reads.** The window the pictures land in is cut into [`CELLS`] cells
//!   of one sector. Up to [`STAGES`] reads are in flight or waiting for the
//!   GPU, each a run of up to [`RUN_CELLS`] consecutive sectors: the best
//!   ranked picture and every other wanted picture that lies within the
//!   same stretch of the disc (the cook lays the pictures of one view
//!   side by side, nearby rooms next to each other, so rooms that are near
//!   each other in the world are near on the disc). A landed picture is
//!   uploaded by [`write_upload_packets`] as GPU list nodes (one row each,
//!   under the 16 word node limit), so the GPU does the copy as it draws and
//!   the CPU never waits for it. Until it is, the slot keeps showing what it
//!   had.
//!
//! * **Changes of view.** A wrong side that is drawn is not overwritten: while
//!   more than [`OVERLAY_RESERVE`] slots are free the new view is read into a
//!   spare slot of the same room and shown half blended over the old one for
//!   [`FADE_TICKS`] (the draw marks it [`LiveSlot::fade`]), then takes over and
//!   the old slot is freed, so a view change shows in two steps. With no slot
//!   to spare it is overwritten in place, as before.
//!
//! A failed read frees its stage and the slots are asked for again later.
//! Nothing here waits.

use psx_grid::render::{
    view_keep, view_toward, CardBand, CardTable, LiveSlot, NO_SLOT, NO_VIEW, OUTER_LAYERS,
    PICTURE_ROWS, SLOTS, SLOT_BYTES, SLOT_HALFWORDS, SLOT_ROWS,
};

use crate::grid_stream::{Poll, RoomTransport};

/// Cells of one sector the window the reads land in is cut into.
pub const CELLS: usize = 16;
/// Sectors one read brings in at most.
pub const RUN_CELLS: usize = 8;
/// Reads in flight or landed and waiting for the GPU, at most. Two keep the
/// drive busy and leave the queue room for the rooms' own reads.
pub const STAGES: usize = CELLS / RUN_CELLS;
/// Words of the window the stream reads into.
pub const CARD_WINDOW_WORDS: usize = CELLS * SLOT_BYTES / 4;
/// Rooms the stream can track. A level with more rooms has its farthest
/// ones left without cards.
pub const MAX_ROOMS: usize = 512;
/// Rooms the candidate list holds: the rooms near the eye. A level with more
/// in reach has the extra ones left without cards.
const MAX_CANDIDATES: usize = 384;
/// Ticks between two membership passes.
const PASS_TICKS: u32 = 8;
/// Units past `enter` the candidate room list reaches, and (halved) how far
/// the eye may walk before it is made again.
const CANDIDATE_MARGIN: i32 = 320;
/// Rooms that take a slot in one pass: a fresh level fills its atlas over a
/// few passes instead of one long one.
const NEW_PER_PASS: usize = 12;
/// Rank offset between the rooms inside the card distance and the ones
/// beyond it (and between the read tiers).
const CLASS_GAP: i32 = 1 << 24;
/// Shortest move between two passes, in runtime units, that updates the
/// heading.
const MIN_STEP: i32 = 16;
/// Ticks a slot may wait for its picture before it is read ahead of the
/// ones nearer on the disc.
const PATIENCE: u32 = 90;
/// Slots left free for newcomers: a wrong side is only replaced beside the old
/// picture (so the change shows in two steps) while more than this many are.
const OVERLAY_RESERVE: usize = 6;
/// Ticks the new view shows half blended over the old one before it takes
/// over.
const FADE_TICKS: u32 = 14;
/// Units past the distances a card is drawn between that a room still counts
/// as drawable: the eye moves less than this between two passes.
const DRAW_MARGIN: i32 = 100;

/// What a [`CardStream`] has done, for telemetry and diagnosis.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct CardCounters {
    /// Reads queued.
    pub submitted: u32,
    /// Reads that landed.
    pub landed: u32,
    /// Reads that failed or were cancelled.
    pub failed: u32,
    /// Pictures uploaded to VRAM.
    pub uploads: u32,
    /// Slots taken over from a worse ranked room.
    pub evictions: u32,
    /// Submits the transport refused (its queue full).
    pub refused: u32,
    /// Slots in use at the last pass.
    pub live: u32,
    /// Rooms in range at the last pass that found no slot.
    pub starved: u32,
    /// Pictures that rode along in a read queued for another: the reads
    /// merging saved.
    pub merged: u32,
    /// Slots at the last pass that wait for a picture (any tier).
    pub backlog: u32,
    /// Of those, slots inside the card distance that show nothing yet.
    pub blank: u32,
    /// First sector (from the pictures' base) of the last read queued.
    pub last_first: u32,
    /// Sectors of the last read queued.
    pub last_sectors: u32,
}

#[derive(Copy, Clone)]
struct Slot {
    /// Room held, or [`u16::MAX`].
    room: u16,
    /// View VRAM holds ([`NO_VIEW`] for none).
    shown: u8,
    /// View wanted.
    want: u8,
    /// A read for this slot is in flight or waiting for the GPU.
    pending: bool,
    /// Rank of the room at the last pass: lower is better, rooms inside the
    /// card distance below [`CLASS_GAP`].
    rank: i32,
    /// The room is too near for its card to be drawn (something else stands
    /// there): a wrong side matters only once the eye backs off.
    hidden: bool,
    /// Tick the slot started waiting for a picture at (0 when it is not).
    since: u32,
    /// The room is, as of the last pass, at a distance a card is drawn from
    /// (give or take [`DRAW_MARGIN`]).
    drawable: bool,
    /// Sector of the wanted picture, as of the last pass (0 for none).
    sector: u32,
    /// Layer of the card (0 for a room's, see [`CardInput::outer`]).
    layer: u8,
    /// The other slot of this room while a change of view shows in two
    /// steps ([`NO_SLOT`] otherwise).
    partner: u8,
    /// This slot holds the view the room is changing to; `partner` holds the
    /// one it is leaving.
    incoming: bool,
    /// Tick the incoming view began to show at.
    fade_start: u32,
}

impl Slot {
    const EMPTY: Self = Self {
        room: u16::MAX,
        shown: NO_VIEW,
        want: NO_VIEW,
        pending: false,
        rank: i32::MAX,
        hidden: false,
        since: 0,
        drawable: false,
        sector: 0,
        layer: 0,
        partner: NO_SLOT,
        incoming: false,
        fade_start: 0,
    };
}

/// A picture a read is bringing in.
#[derive(Copy, Clone)]
struct Entry {
    slot: u8,
    room: u16,
    view: u8,
}

/// One read: consecutive sectors landing in consecutive cells of a stage.
#[derive(Copy, Clone)]
struct Run<K> {
    ticket: K,
    landed: bool,
    /// Per cell of the run, the picture it holds that a slot still waits
    /// for, until the picture is in a frame's list.
    entries: [Option<Entry>; RUN_CELLS],
}

#[derive(Copy, Clone)]
enum Stage<K> {
    Free,
    Busy(Run<K>),
}

/// What a call to [`CardStream::update`] knows about the level and the eye.
pub struct CardInput<'a> {
    /// The card of each room.
    pub table: &'a CardTable<'a>,
    /// Per room, the sector of its pictures (view 0) relative to `base_lba`
    /// (0 for none); empty for a level without cards.
    pub sectors: &'a [u32],
    /// Sectors between a room's view `k` and view `k + 1`.
    pub stride: u32,
    /// Disc sector `sectors` is relative to.
    pub base_lba: u32,
    /// The eye, world runtime units.
    pub eye: [i32; 3],
    /// A room within this plan distance of the eye holds a slot.
    pub enter: i32,
    /// A room beyond this one gives its slot up.
    pub leave: i32,
    /// Cards show out to this plan distance: rooms inside it outrank the
    /// ones beyond (which only hold a slot to be early).
    pub far: i32,
    /// Cards are not drawn closer than this plan distance.
    pub near: i32,
    /// A room's card this near is held (it keeps its slot and a picture, so it can stand in
    /// for the room at once) but its view is not kept up to date: a wrong side matters only
    /// once the eye backs off. The rooms this near are drawn whole.
    pub hold_near: i32,
    /// The layers of cards beyond the rooms' (card layer `k + 1` is entry `k`,
    /// see [`psx_grid::render::outer_bands`]). The values above are layer 0's.
    /// A layer's cards take a slot a fade before they show and give it up
    /// half a fade past that.
    pub outer: [CardBand; OUTER_LAYERS],
    /// How much better a room the eye is walking straight towards ranks
    /// than one beside it, and one behind ranks worse, in runtime units.
    pub lead: i32,
    /// Where each tile of rooms stands; the eye is in the tile whose centre is
    /// nearest. Empty for a level without copies.
    pub centres: &'a [[i16; 2]],
    /// Per pair of tiles (`X * tiles + Y`): the sector (low 24 bits) and
    /// views (top 8) of the copy of tile `Y`'s pictures that lies near tile
    /// `X`, or 0.
    pub copies: &'a [u32],
    /// Per room, its tile and its place among the tile's rooms.
    pub room_tile: &'a [u8],
    /// Per room, its place among the rooms of its tile.
    pub room_place: &'a [u8],
}

/// A landed picture ready for the GPU.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// Atlas slot to write.
    pub slot: u8,
    /// Which cell of the window holds the picture.
    pub cell: u8,
    room: u16,
    view: u8,
}

impl Upload {
    /// An upload of the picture in `cell` of the window to `slot`, for the streams that
    /// keep track of what their slots hold themselves (the panels').
    pub const fn new(slot: u8, cell: u8) -> Self {
        Self {
            slot,
            cell,
            room: 0,
            view: 0,
        }
    }
}

/// Words of one sector.
pub const SECTOR_WORDS: usize = SLOT_BYTES / 4;

/// The distances one layer of cards works to.
#[derive(Copy, Clone)]
struct Band {
    near: i32,
    far: i32,
    enter: i32,
    leave: i32,
}

/// Rank offset between the cards of one layer and the next: a layer's cards
/// rank behind every card of the nearer layers within the same class.
const LAYER_RANK: i32 = 1 << 20;

impl CardInput<'_> {
    /// The distances layer `layer` works to.
    fn band(&self, layer: u8) -> Band {
        let Some(row) = usize::from(layer)
            .checked_sub(1)
            .and_then(|k| self.outer.get(k))
            .filter(|row| row.far > 0)
        else {
            return Band {
                near: self.near,
                far: self.far,
                enter: self.enter,
                leave: self.leave,
            };
        };
        // A fade past the far edge: the outer layers change slowly and
        // their cards are few, but every slot they hold is one a nearer
        // layer cannot use.
        let enter = row.far + row.fade;
        Band {
            near: row.near,
            // The stream measures plan distance to within 7.5 percent over.
            far: row.far + row.far / 12,
            enter,
            leave: enter + row.fade / 2,
        }
    }

    /// The farthest `enter` of any layer: how far from the eye a card can
    /// want a slot.
    fn widest_enter(&self) -> i32 {
        self.outer
            .iter()
            .map(|row| row.far + row.fade)
            .fold(self.enter, i32::max)
    }
}

/// Plan distance of two offsets within 7 percent: the larger plus three
/// eighths of the smaller.
fn plan_distance(dx: i32, dz: i32) -> i32 {
    let (a, b) = (dx.abs(), dz.abs());
    a.max(b) + ((a.min(b) * 3) >> 3)
}

/// Streams the pictures the cards show.
pub struct CardStream<K> {
    /// What the stream has done so far.
    pub counters: CardCounters,
    slots: [Slot; SLOTS],
    live: [LiveSlot; SLOTS],
    /// The slots to draw, ascending: those with a picture whose room is
    /// drawable.
    draw: [u8; SLOTS],
    draw_count: usize,
    by_room: [u8; MAX_ROOMS],
    stages: [Stage<K>; STAGES],
    ticks: u32,
    /// Direction the eye has been walking in, plan, unit length as Q12
    /// (zero before it has moved).
    heading: [i32; 2],
    last_eye: [i32; 2],
    seen_eye: bool,
    /// The rooms within `enter + CANDIDATE_MARGIN` of `candidates_eye`, so a
    /// pass looks at the few rooms near the eye and not at every room.
    candidates: [u16; MAX_CANDIDATES],
    candidate_count: usize,
    candidates_eye: Option<[i32; 2]>,
    /// The tile the eye is in (`usize::MAX` before it is known).
    region: usize,
    /// Sector the drive ends at once the reads queued so far are done (0
    /// before the first), relative to the pictures' base.
    tail: u32,
}

impl<K: Copy> CardStream<K> {
    /// A stream holding nothing.
    pub const fn new() -> Self {
        Self {
            counters: CardCounters {
                submitted: 0,
                landed: 0,
                failed: 0,
                uploads: 0,
                evictions: 0,
                refused: 0,
                live: 0,
                starved: 0,
                merged: 0,
                backlog: 0,
                blank: 0,
                last_first: 0,
                last_sectors: 0,
            },
            slots: [Slot::EMPTY; SLOTS],
            live: [LiveSlot::EMPTY; SLOTS],
            draw: [0; SLOTS],
            draw_count: 0,
            by_room: [NO_SLOT; MAX_ROOMS],
            stages: [Stage::Free; STAGES],
            ticks: 0,
            heading: [0; 2],
            last_eye: [0; 2],
            seen_eye: false,
            candidates: [0; MAX_CANDIDATES],
            candidate_count: 0,
            candidates_eye: None,
            region: usize::MAX,
            tail: 0,
        }
    }

    /// The slots as the card draw reads them: the room each holds and the
    /// view its VRAM shows.
    pub fn live(&self) -> &[LiveSlot; SLOTS] {
        &self.live
    }

    /// The slots the card draw looks at: the ones that show a picture and
    /// whose room is at a distance cards are drawn from. Refreshed every
    /// pass and whenever a picture lands, so the draw does not walk every
    /// slot to find the few in the band.
    pub fn draw_list(&self) -> &[u8] {
        &self.draw[..self.draw_count]
    }

    fn rebuild_draw_list(&mut self) {
        let mut count = 0;
        for (slot, held) in self.slots.iter().enumerate() {
            if held.room != u16::MAX && held.drawable && held.shown != NO_VIEW {
                self.draw[count] = slot as u8;
                count += 1;
            }
        }
        self.draw_count = count;
    }

    /// Drive the stream one tick: poll the reads in flight, every few ticks
    /// re-decide which rooms hold slots and which view each wants, and
    /// start the next reads. `window` is [`CARD_WINDOW_WORDS`] words of
    /// memory the reads land in, and must stay put while the stream lives.
    pub fn update<T>(&mut self, transport: &mut T, window: &mut [u32], input: &CardInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        if window.len() < CARD_WINDOW_WORDS || input.sectors.is_empty() {
            return;
        }
        self.poll(transport);
        if self.ticks.is_multiple_of(PASS_TICKS) {
            self.pass(input);
        }
        self.ticks = self.ticks.wrapping_add(1);
        self.start_reads(transport, window, input);
    }

    /// Retire the reads that finished.
    fn poll<T>(&mut self, transport: &mut T)
    where
        T: RoomTransport<Ticket = K>,
    {
        for stage in &mut self.stages {
            let Stage::Busy(run) = stage else {
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
                }
                Poll::Cancelled | Poll::Failed => {
                    self.counters.failed += 1;
                    for entry in run.entries.iter().flatten() {
                        let held = &mut self.slots[usize::from(entry.slot)];
                        if held.room == entry.room {
                            held.pending = false;
                        }
                    }
                    *stage = Stage::Free;
                }
            }
        }
    }

    /// The tile the eye is in: the one whose centre is nearest, kept until
    /// another is nearer by more than [`DRAW_MARGIN`].
    fn update_region(&mut self, input: &CardInput<'_>) {
        let [ex, _, ez] = input.eye;
        let distance = |tile: usize| {
            let c = input.centres[tile];
            plan_distance(ex - i32::from(c[0]), ez - i32::from(c[1]))
        };
        let Some(best) = (0..input.centres.len()).min_by_key(|&tile| distance(tile)) else {
            self.region = usize::MAX;
            return;
        };
        if self.region >= input.centres.len()
            || distance(best) + DRAW_MARGIN < distance(self.region)
        {
            self.region = best;
        }
    }

    /// The sector of `room`'s picture of `view`: from the copy near the eye's
    /// tile when it holds that view, else from the full set.
    fn picture_sector(&self, room: usize, view: u8, input: &CardInput<'_>) -> u32 {
        let first = input.sectors.get(room).copied().unwrap_or(0);
        if first == 0 {
            return 0;
        }
        let tiles = input.centres.len();
        if self.region < tiles {
            if let (Some(&tile), Some(&place)) =
                (input.room_tile.get(room), input.room_place.get(room))
            {
                let copy = input
                    .copies
                    .get(self.region * tiles + usize::from(tile))
                    .copied()
                    .unwrap_or(0);
                let mask = (copy >> 24) as u8;
                if mask & (1 << view) != 0 {
                    let before = (mask & ((1u8 << view) - 1)).count_ones();
                    return (copy & 0x00ff_ffff) + before * input.stride + u32::from(place);
                }
            }
        }
        first + u32::from(view) * input.stride
    }

    /// Follow the direction the eye walks in.
    fn update_heading(&mut self, eye: [i32; 3]) {
        let [ex, _, ez] = eye;
        if self.seen_eye {
            let (dx, dz) = (ex - self.last_eye[0], ez - self.last_eye[1]);
            let length = plan_distance(dx, dz);
            if length >= MIN_STEP {
                let unit = [(dx << 12) / length, (dz << 12) / length];
                self.heading = if self.heading == [0, 0] {
                    unit
                } else {
                    [
                        (3 * self.heading[0] + unit[0]) >> 2,
                        (3 * self.heading[1] + unit[1]) >> 2,
                    ]
                };
            }
        }
        self.last_eye = [ex, ez];
        self.seen_eye = true;
    }

    /// Rank of a room the eye sees at offset `(dx, dz)` from it (eye minus
    /// card): plan distance, less the lead for a room straight ahead.
    fn rank(&self, dx: i32, dz: i32, distance: i32, band: &Band, layer: u8, lead: i32) -> i32 {
        let class = if distance <= band.far { 0 } else { CLASS_GAP };
        let toward = -dx * self.heading[0] - dz * self.heading[1];
        let ahead = toward / distance.max(1);
        // The distance a far layer's card is ranked by is its distance into
        // the layer's band, so nothing in an outer layer outranks the near one.
        let into = (distance - band.near).max(0);
        class + i32::from(layer) * LAYER_RANK + (into - ((ahead * lead) >> 12)).max(0)
    }

    /// Membership and wanted views.
    fn pass(&mut self, input: &CardInput<'_>) {
        let [ex, _, ez] = input.eye;
        self.update_heading(input.eye);
        self.update_region(input);
        // Slots whose rooms went out of range, and slots' views and ranks.
        for slot in 0..SLOTS {
            let held = self.slots[slot];
            if held.room == u16::MAX || held.incoming {
                continue;
            }
            let Some(card) = input.table.card(usize::from(held.room)) else {
                self.release(slot);
                continue;
            };
            let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
            let distance = plan_distance(dx, dz);
            let band = input.band(card.layer);
            if distance > band.leave {
                self.release(slot);
                continue;
            }
            let want = if held.want != NO_VIEW && view_keep(held.want, dx, dz) {
                held.want
            } else {
                view_toward(dx, dz)
            };
            self.slots[slot].want = want;
            self.slots[slot].rank = self.rank(dx, dz, distance, &band, card.layer, input.lead);
            self.slots[slot].hidden = distance
                < if card.layer == 0 {
                    input.hold_near.max(band.near)
                } else {
                    band.near
                };
            self.slots[slot].drawable =
                distance >= band.near - DRAW_MARGIN && distance <= band.far + DRAW_MARGIN;
        }
        self.step_overlays();
        self.refresh_candidates(input);
        // The best ranked rooms in range that hold nothing, in order.
        let mut best = [(i32::MAX, u16::MAX); NEW_PER_PASS];
        let mut candidates = 0u32;
        for index in 0..self.candidate_count {
            let room = usize::from(self.candidates[index]);
            if self.by_room[room] != NO_SLOT {
                continue;
            }
            let Some(card) = input.table.card(room) else {
                continue;
            };
            let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
            let distance = plan_distance(dx, dz);
            let band = input.band(card.layer);
            if distance > band.enter {
                continue;
            }
            candidates += 1;
            let rank = self.rank(dx, dz, distance, &band, card.layer, input.lead);
            if rank >= best[NEW_PER_PASS - 1].0 {
                continue;
            }
            let mut at = NEW_PER_PASS - 1;
            while at > 0 && best[at - 1].0 > rank {
                best[at] = best[at - 1];
                at -= 1;
            }
            best[at] = (rank, room as u16);
        }
        let mut starved = 0u32;
        let mut placed = 0u32;
        for &(rank, room) in best.iter().filter(|b| b.1 != u16::MAX) {
            let Some(card) = input.table.card(usize::from(room)) else {
                continue;
            };
            let Some(slot) = self.free_slot_for(rank, input) else {
                starved = candidates - placed;
                break;
            };
            let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
            self.slots[slot] = Slot {
                room,
                shown: NO_VIEW,
                want: view_toward(dx, dz),
                pending: false,
                rank,
                hidden: false,
                since: 0,
                drawable: false,
                sector: 0,
                layer: card.layer,
                partner: NO_SLOT,
                incoming: false,
                fade_start: 0,
            };
            self.by_room[usize::from(room)] = slot as u8;
            placed += 1;
        }
        for slot in 0..SLOTS {
            let held = self.slots[slot];
            if held.room != u16::MAX {
                self.slots[slot].sector = if held.want == NO_VIEW {
                    0
                } else {
                    self.picture_sector(usize::from(held.room), held.want, input)
                };
            }
        }
        let (mut live, mut backlog, mut blank) = (0, 0, 0);
        for (slot, held) in self.slots.iter().enumerate() {
            self.live[slot] = LiveSlot {
                room: held.room,
                view: held.shown,
                fade: held.incoming && held.shown != NO_VIEW,
            };
            live += u32::from(held.room != u16::MAX);
            let waiting = held.room != u16::MAX && held.want != NO_VIEW && held.shown != held.want;
            backlog += u32::from(waiting);
            blank += u32::from(waiting && held.shown == NO_VIEW && held.rank < CLASS_GAP);
        }
        self.rebuild_draw_list();
        self.counters.live = live;
        self.counters.backlog = backlog;
        self.counters.blank = blank;
        self.counters.starved = starved;
    }

    /// Make the list of rooms a pass looks at again when the eye has walked
    /// far enough from where it was made.
    fn refresh_candidates(&mut self, input: &CardInput<'_>) {
        let [ex, _, ez] = input.eye;
        let moved = self.candidates_eye.is_none_or(|at| {
            (ex - at[0]).abs() > CANDIDATE_MARGIN / 2 || (ez - at[1]).abs() > CANDIDATE_MARGIN / 2
        });
        if !moved {
            return;
        }
        let reach = input.widest_enter() + CANDIDATE_MARGIN;
        let mut count = 0;
        for room in 0..input.table.len().min(MAX_ROOMS) {
            let Some(card) = input.table.card(room) else {
                continue;
            };
            if (ex - card.centre[0]).abs() <= reach && (ez - card.centre[1]).abs() <= reach {
                self.candidates[count] = room as u16;
                count += 1;
                if count == MAX_CANDIDATES {
                    break;
                }
            }
        }
        self.candidate_count = count;
        self.candidates_eye = Some([ex, ez]);
    }

    /// Give a slot's room up. The outgoing slot of a change of view takes
    /// the incoming one with it; an incoming slot leaves the outgoing one
    /// holding the room.
    fn release(&mut self, slot: usize) {
        let held = self.slots[slot];
        if held.partner != NO_SLOT {
            let other = usize::from(held.partner);
            if held.incoming {
                self.slots[other].partner = NO_SLOT;
            } else {
                self.slots[other] = Slot::EMPTY;
            }
        }
        if !held.incoming {
            if let Some(by) = self.by_room.get_mut(usize::from(held.room)) {
                *by = NO_SLOT;
            }
        }
        self.slots[slot] = Slot::EMPTY;
    }

    /// Keep the changes of view that show in two steps going: the incoming
    /// slot follows its room's rank, wants what the room wants, and takes
    /// over once it has shown half blended for [`FADE_TICKS`].
    fn step_overlays(&mut self) {
        for slot in 0..SLOTS {
            let held = self.slots[slot];
            if held.room == u16::MAX || !held.incoming {
                continue;
            }
            let outgoing = usize::from(held.partner);
            let base = self.slots[outgoing];
            self.slots[slot].rank = base.rank;
            self.slots[slot].hidden = base.hidden;
            self.slots[slot].drawable = base.drawable;
            if held.shown == NO_VIEW {
                // Not landed yet: follow the room's wanted view; if the room
                // wants what it shows again, forget the change.
                if !held.pending {
                    if base.want == base.shown {
                        self.release(slot);
                    } else {
                        self.slots[slot].want = base.want;
                    }
                }
            } else if self.ticks.wrapping_sub(held.fade_start) >= FADE_TICKS {
                // The new view takes over; the old one's slot is free.
                self.slots[outgoing] = Slot::EMPTY;
                self.slots[slot].partner = NO_SLOT;
                self.slots[slot].incoming = false;
                self.by_room[usize::from(held.room)] = slot as u8;
            }
        }
    }

    /// An empty slot, or the one held by the worst ranked room, if that is
    /// worse than `rank` by more than half the gap between entering and
    /// leaving (so two rooms of equal rank do not trade a slot back and
    /// forth).
    fn free_slot_for(&mut self, rank: i32, input: &CardInput<'_>) -> Option<usize> {
        if let Some(slot) = self.slots.iter().position(|s| s.room == u16::MAX) {
            return Some(slot);
        }
        let margin = (input.leave - input.enter) / 2;
        let mut worst: Option<(usize, i32)> = None;
        for (slot, held) in self.slots.iter().enumerate() {
            if held.rank > rank.saturating_add(margin) && worst.is_none_or(|(_, w)| held.rank > w) {
                worst = Some((slot, held.rank));
            }
        }
        let (slot, _) = worst?;
        self.release(slot);
        self.counters.evictions += 1;
        Some(slot)
    }

    /// Queue reads for the slots that need another view into the free
    /// stages: the best ranked picture, and every other wanted picture on
    /// the same stretch of the disc.
    fn start_reads<T>(&mut self, transport: &mut T, window: &mut [u32], input: &CardInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        loop {
            let Some(stage) = self.stages.iter().position(|s| matches!(s, Stage::Free)) else {
                return;
            };
            // Wanted pictures: (read key, sector, slot).
            let mut wanted = [(0i32, 0u32, 0u8, 0u32); SLOTS];
            let mut count = 0;
            for slot in 0..SLOTS {
                let held = self.slots[slot];
                if held.room == u16::MAX
                    || held.want == NO_VIEW
                    || (held.partner != NO_SLOT && !held.incoming)
                {
                    continue;
                }
                if held.shown == held.want {
                    self.slots[slot].since = 0;
                    continue;
                }
                if held.since == 0 {
                    self.slots[slot].since = self.ticks.max(1);
                }
                if held.pending {
                    continue;
                }
                let sector = held.sector;
                if sector == 0 {
                    // A room without pictures: never asked again.
                    self.slots[slot].want = NO_VIEW;
                    continue;
                }
                // Blank cards first (the ones about to be drawn before the
                // ones further out), then wrong sides, those of cards
                // that are drawn before those hidden by nearer geometry.
                let tier = if held.shown != NO_VIEW || held.incoming {
                    2 + i32::from(held.hidden)
                } else if held.rank >= CLASS_GAP {
                    1
                } else {
                    0
                };
                let key = tier * CLASS_GAP + held.rank % CLASS_GAP;
                let waited = self.ticks.wrapping_sub(self.slots[slot].since);
                wanted[count] = (key, sector, slot as u8, waited);
                count += 1;
            }
            let wanted = &wanted[..count];
            // The most urgent tier first. Within it the picture nearest on the
            // disc to where the reads queued before it end (the drive then
            // sweeps the wanted pictures instead of hopping between them),
            // unless one has waited out its patience: the one that waited
            // longest, best ranked among equals.
            let Some(tier) = wanted.iter().map(|w| w.0 / CLASS_GAP).min() else {
                return;
            };
            let urgent = wanted.iter().filter(|w| w.0 / CLASS_GAP == tier);
            let tail = self.tail;
            let Some(&(_, head, _, _)) = urgent
                .clone()
                .filter(|w| w.3 > PATIENCE)
                .max_by_key(|w| (w.3, core::cmp::Reverse(w.0)))
                .or_else(|| {
                    urgent.min_by_key(|w| (if tail == 0 { 0 } else { w.1.abs_diff(tail) }, w.0))
                })
            else {
                return;
            };
            // The stretch of RUN_CELLS sectors holding the head and the most
            // other wanted pictures, the earliest among equals.
            let mut start = head;
            let mut most = 0;
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
            let stretch = start..start + RUN_CELLS as u32;
            let first = wanted
                .iter()
                .filter(|w| stretch.contains(&w.1))
                .map(|w| w.1)
                .min()
                .unwrap_or(head);
            let last = wanted
                .iter()
                .filter(|w| stretch.contains(&w.1))
                .map(|w| w.1)
                .max()
                .unwrap_or(head);
            let mut entries = [None; RUN_CELLS];
            let mut riders = 0;
            let mut spare = self.slots.iter().filter(|s| s.room == u16::MAX).count();
            for &(_, sector, slot, _) in wanted.iter().filter(|w| stretch.contains(&w.1)) {
                let mut slot = usize::from(slot);
                let held = self.slots[slot];
                // A wrong side that is drawn is replaced beside the old
                // picture while there is room to spare, so it shows in two
                // steps.
                if held.shown != NO_VIEW
                    && !held.incoming
                    && held.drawable
                    && held.layer == 0
                    && spare > OVERLAY_RESERVE
                {
                    if let Some(free) = self.slots.iter().position(|s| s.room == u16::MAX) {
                        self.slots[free] = Slot {
                            shown: NO_VIEW,
                            pending: true,
                            since: 0,
                            partner: slot as u8,
                            incoming: true,
                            fade_start: 0,
                            ..held
                        };
                        self.slots[slot].partner = free as u8;
                        spare -= 1;
                        slot = free;
                    }
                }
                let held = self.slots[slot];
                entries[(sector - first) as usize] = Some(Entry {
                    slot: slot as u8,
                    room: held.room,
                    view: held.want,
                });
                riders += 1;
            }
            transport.begin_transfer();
            // SAFETY: the window is memory the caller keeps for the stream's
            // life, `stage` is one of its STAGES parts of RUN_CELLS sectors
            // each, the run is at most RUN_CELLS sectors, and the part is not
            // reused until the read is reported finished.
            let destination =
                unsafe { window.as_mut_ptr().add(stage * RUN_CELLS * SLOT_BYTES / 4) };
            let lba = input.base_lba + first;
            match unsafe { transport.submit(lba, last - first + 1, destination, false) } {
                Some(ticket) => {
                    for entry in entries.iter().flatten() {
                        self.slots[usize::from(entry.slot)].pending = true;
                    }
                    self.stages[stage] = Stage::Busy(Run {
                        ticket,
                        landed: false,
                        entries,
                    });
                    self.counters.submitted += 1;
                    self.counters.merged += riders - 1;
                    self.tail = last;
                    self.counters.last_first = first;
                    self.counters.last_sectors = last - first + 1;
                }
                None => {
                    self.counters.refused += 1;
                    return;
                }
            }
        }
    }

    /// The landed pictures waiting for the GPU, up to `N`, for slots that
    /// still hold the room they were read for.
    pub fn uploads<const N: usize>(&mut self) -> ([Option<Upload>; N], usize) {
        let mut out = [None; N];
        let mut count = 0;
        for (index, stage) in self.stages.iter_mut().enumerate() {
            let Stage::Busy(run) = stage else {
                continue;
            };
            if !run.landed {
                continue;
            }
            for (cell, entry) in run.entries.iter_mut().enumerate() {
                let Some(picture) = *entry else {
                    continue;
                };
                if self.slots[usize::from(picture.slot)].room != picture.room {
                    // The slot was given to another room meanwhile.
                    *entry = None;
                    continue;
                }
                if count < N {
                    out[count] = Some(Upload {
                        slot: picture.slot,
                        cell: (index * RUN_CELLS + cell) as u8,
                        room: picture.room,
                        view: picture.view,
                    });
                    count += 1;
                }
            }
            if run.entries.iter().all(Option::is_none) {
                *stage = Stage::Free;
            }
        }
        (out, count)
    }

    /// The uploads in `taken` are in a frame's list: VRAM now holds their
    /// pictures and their cells are free again.
    pub fn applied(&mut self, taken: &[Option<Upload>]) {
        for upload in taken.iter().flatten() {
            let held = &mut self.slots[usize::from(upload.slot)];
            if held.room == upload.room {
                held.shown = upload.view;
                held.pending = false;
                held.since = 0;
                if held.incoming {
                    held.fade_start = self.ticks;
                }
                let live = &mut self.live[usize::from(upload.slot)];
                live.view = upload.view;
                live.fade = held.incoming;
            }
            let (index, cell) = (
                usize::from(upload.cell) / RUN_CELLS,
                usize::from(upload.cell) % RUN_CELLS,
            );
            if let Stage::Busy(run) = &mut self.stages[index] {
                run.entries[cell] = None;
                if run.entries.iter().all(Option::is_none) {
                    self.stages[index] = Stage::Free;
                }
            }
            self.counters.uploads += 1;
        }
        self.rebuild_draw_list();
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

impl<K: Copy> Default for CardStream<K> {
    fn default() -> Self {
        Self::new()
    }
}

/// Words of linked-list payload one node may carry: a console loses words
/// from longer nodes while it draws (see `psx_gpu::chain::MAX_NODE_WORDS`).
const NODE_WORDS: usize = 16;
/// GP0(A0h) words ahead of an image transfer's data.
const TRANSFER_HEADER: usize = 3;
/// Words of one row's data: a slot is 16 halfwords wide.
const ROW_WORDS: usize = SLOT_HALFWORDS as usize / 2;
/// Words of one row's node, with its tag.
const ROW_NODE_WORDS: usize = 1 + TRANSFER_HEADER + ROW_WORDS;
const _: () = assert!(ROW_NODE_WORDS - 1 <= NODE_WORDS);
/// Rows of a slot, the palette's included.
const ROWS: usize = SLOT_ROWS as usize;
const _: () = assert!(ROWS * ROW_WORDS * 4 == SLOT_BYTES);
const _: () = assert!(PICTURE_ROWS as usize + 1 == ROWS);

/// Words [`write_upload_packets`] needs for `pictures` pictures: each row
/// of each is one image transfer in one node, plus one texture-cache flush.
pub const fn upload_packet_words(pictures: usize) -> usize {
    pictures * ROWS * ROW_NODE_WORDS + 2
}

/// Write the uploads into `out` as a tagged packet stream for OT slot
/// `slot`, ready for `OtFrame::add_committed_tagged_packet_stream`: one image
/// transfer per row of each picture to its place in the atlas at `origin`,
/// then a texture-cache flush, all in reverse (the stream's packets are
/// prepended, so the first written runs last). `window` is the memory the
/// stream's reads landed in. Returns `(words, packets)` written; nothing when
/// `out` is short or there are no uploads.
pub fn write_upload_packets(
    uploads: &[Option<Upload>],
    window: &[u32],
    origin: (u16, u16),
    origin2: (u16, u16),
    slot: u16,
    out: &mut [u32],
) -> (usize, usize) {
    let pictures = uploads.iter().flatten().count();
    let words = upload_packet_words(pictures);
    if pictures == 0 || out.len() < words {
        return (0, 0);
    }
    let tag = u32::from(slot) | ((ROW_NODE_WORDS as u32 - 1) << 24);
    // The flush runs after every picture: it goes lowest.
    out[0] = u32::from(slot) | (1 << 24);
    out[1] = 0x0100_0000;
    let mut end = words;
    for upload in uploads.iter().flatten() {
        // Slots past the first atlas's are the second's (the far panels', when it was got).
        let (x, y) = if usize::from(upload.slot) >= SLOTS {
            psx_grid::render::slot_origin(
                origin2,
                psx_grid::render::second_atlas_slot(usize::from(upload.slot) - SLOTS),
            )
        } else {
            psx_grid::render::slot_origin(origin, usize::from(upload.slot))
        };
        let from = usize::from(upload.cell) * SLOT_BYTES / 4;
        for row in 0..ROWS {
            let node: &mut [u32; ROW_NODE_WORDS] = (&mut out[end - ROW_NODE_WORDS..end])
                .try_into()
                .unwrap_or_else(|_| unreachable!());
            node[0] = tag;
            node[1] = psx_hw::gpu::gp0::COPY_CPU_TO_VRAM;
            node[2] = psx_hw::gpu::pack_xy(x, y + row as u16);
            node[3] = psx_hw::gpu::pack_xy(SLOT_HALFWORDS, 1);
            let at = from + row * ROW_WORDS;
            node[4..].copy_from_slice(&window[at..at + ROW_WORDS]);
            end -= ROW_NODE_WORDS;
        }
    }
    (words, 1 + pictures * ROWS)
}

/// [`write_upload_packets`] for pictures that are not in memory as they are: `fill(k, words)`
/// writes the picture for `slots[k]` into the sector-sized buffer it is given, which is then
/// copied into the packets. The far panels decode their tiles this way. Returns `(words,
/// packets)` written; nothing when `out` is short or there are no slots.
pub fn write_filled_packets(
    slots: &[u8],
    origin: (u16, u16),
    origin2: (u16, u16),
    slot: u16,
    out: &mut [u32],
    mut fill: impl FnMut(usize, &mut [u32; SECTOR_WORDS]),
) -> (usize, usize) {
    let pictures = slots.len();
    let words = upload_packet_words(pictures);
    if pictures == 0 || out.len() < words {
        return (0, 0);
    }
    let tag = u32::from(slot) | ((ROW_NODE_WORDS as u32 - 1) << 24);
    out[0] = u32::from(slot) | (1 << 24);
    out[1] = 0x0100_0000;
    let mut end = words;
    let mut picture = [0u32; SECTOR_WORDS];
    for (k, &atlas_slot) in slots.iter().enumerate() {
        fill(k, &mut picture);
        let (x, y) = if usize::from(atlas_slot) >= SLOTS {
            psx_grid::render::slot_origin(
                origin2,
                psx_grid::render::second_atlas_slot(usize::from(atlas_slot) - SLOTS),
            )
        } else {
            psx_grid::render::slot_origin(origin, usize::from(atlas_slot))
        };
        for row in 0..ROWS {
            let node: &mut [u32; ROW_NODE_WORDS] = (&mut out[end - ROW_NODE_WORDS..end])
                .try_into()
                .unwrap_or_else(|_| unreachable!());
            node[0] = tag;
            node[1] = psx_hw::gpu::gp0::COPY_CPU_TO_VRAM;
            node[2] = psx_hw::gpu::pack_xy(x, y + row as u16);
            node[3] = psx_hw::gpu::pack_xy(SLOT_HALFWORDS, 1);
            let at = row * ROW_WORDS;
            node[4..].copy_from_slice(&picture[at..at + ROW_WORDS]);
            end -= ROW_NODE_WORDS;
        }
    }
    (words, 1 + pictures * ROWS)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    const SECTOR: usize = SLOT_BYTES;

    /// A disc of sectors, a read finishing on the poll after it was queued.
    struct Disc {
        bytes: Vec<u8>,
        reads: Vec<(u32, u32)>,
        polls: Vec<u32>,
        fail: Vec<usize>,
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
                fail: Vec::new(),
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
            if self.fail.contains(&ticket) {
                return Poll::Failed;
            }
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

    /// Cards at `x = 100 * room`, all on the x axis.
    fn rows(count: usize) -> Vec<[i16; 6]> {
        (0..count)
            .map(|room| [100 * room as i16, 0, 0, 200, 40, 0])
            .collect()
    }

    fn input<'a>(table: &'a CardTable<'a>, sectors: &'a [u32], eye: [i32; 3]) -> CardInput<'a> {
        CardInput {
            table,
            sectors,
            stride: 1,
            base_lba: 10,
            eye,
            enter: 450,
            leave: 600,
            far: 300,
            near: 0,
            hold_near: 0,
            lead: 0,
            outer: [CardBand::default(); OUTER_LAYERS],
            centres: &[],
            copies: &[],
            room_tile: &[],
            room_place: &[],
        }
    }

    /// Run `ticks` updates and apply whatever landed, as the game does.
    fn run(
        stream: &mut CardStream<usize>,
        disc: &mut Disc,
        window: &mut [u32],
        input: &CardInput<'_>,
        ticks: usize,
    ) {
        for _ in 0..ticks {
            stream.update(disc, window, input);
            let (ups, count) = stream.uploads::<2>();
            stream.applied(&ups[..count]);
        }
    }

    fn held(stream: &CardStream<usize>) -> Vec<u16> {
        let mut v: Vec<u16> = stream
            .live()
            .iter()
            .filter(|s| s.room != u16::MAX)
            .map(|s| s.room)
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn rooms_in_range_take_slots_and_their_wanted_view_arrives() {
        let rows = rows(12);
        let table = CardTable::new(&rows);
        // Room r's pictures at sector 8 * r + 1 (1..), eight views each.
        let sectors: Vec<u32> = (0..12).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * 12 + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let input = input(&table, &sectors, [-10, 0, 0]);
        run(&mut stream, &mut disc, &mut window, &input, 60);
        // Rooms 0..=4 are within 450 units (x = 0..400).
        assert_eq!(held(&stream), [0, 1, 2, 3, 4]);
        // The eye is at x = 0 and the cards stand on +x: the eye is in
        // direction 180 degrees from them, view 4.
        for live in stream.live().iter().filter(|s| s.room != u16::MAX) {
            assert_eq!(live.view, 4, "room {}", live.room);
        }
        // Each read asked for sector (room's first + 4) past the base.
        for &(lba, count) in &disc.reads {
            assert_eq!(count, 1);
            assert_eq!((lba - 10) % 8, 5, "lba {lba}");
        }
    }

    #[test]
    fn walking_past_a_room_turns_its_view_and_far_rooms_give_up_their_slots() {
        let rows = rows(12);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..12).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * 12 + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        run(
            &mut stream,
            &mut disc,
            &mut window,
            &input(&table, &sectors, [0, 0, 0]),
            60,
        );
        // Walk to x = 800: rooms 0 and 1 are now more than 600 behind.
        run(
            &mut stream,
            &mut disc,
            &mut window,
            &input(&table, &sectors, [800, 0, 0]),
            80,
        );
        let now = held(&stream);
        assert!(!now.contains(&0) && !now.contains(&1));
        assert!(now.contains(&8) && now.contains(&10));
        // A room ahead of the eye (x = 1000 > 800) is seen from -x: view 4.
        let at = stream.live().iter().position(|s| s.room == 10).unwrap();
        assert_eq!(stream.live()[at].view, 4);
        // A room behind the eye (x = 500 < 800) is seen from +x: view 0.
        let at = stream.live().iter().position(|s| s.room == 5).unwrap();
        assert_eq!(stream.live()[at].view, 0);
    }

    #[test]
    fn a_far_layer_takes_a_slot_in_its_band_and_never_outranks_the_rooms() {
        // Rooms 0..=3 on the x axis 100 apart; two layer cards, layer 1 at
        // x = 700 (shows from 600 to 1200 away) and layer 2 at x = 4000
        // (from 1100 on, no limit).
        let mut rows = rows(4);
        rows.push([700, 0, 0, 200, 40, 1]);
        rows.push([4000, 0, 0, 200, 40, 2]);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..6).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * 6 + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let outer = [
            CardBand {
                near: 600,
                far: 1200,
                fade: 100,
            },
            CardBand {
                near: 1100,
                far: 1 << 20,
                fade: 200,
            },
        ];
        let mut input = input(&table, &sectors, [-10, 0, 0]);
        input.outer = outer;
        run(&mut stream, &mut disc, &mut window, &input, 80);
        // The layer cards hold slots and show the view facing the eye.
        assert_eq!(held(&stream), [0, 1, 2, 3, 4, 5]);
        for live in stream.live().iter().filter(|s| s.room != u16::MAX) {
            assert_eq!(live.view, 4, "room {}", live.room);
        }
        // Both are drawn: layer 1 is 710 away, inside 600..=1200 (and its
        // slack), layer 2 is 4010 away.
        let drawn: Vec<u16> = stream
            .draw_list()
            .iter()
            .map(|&slot| stream.live()[usize::from(slot)].room)
            .collect();
        assert!(drawn.contains(&4) && drawn.contains(&5), "{drawn:?}");
        // Out of layer 1's band (past its enter) the card gives its slot up.
        let input = CardInput {
            eye: [-1300, 0, 0],
            ..input
        };
        run(&mut stream, &mut disc, &mut window, &input, 80);
        assert!(!held(&stream).contains(&4));
        assert!(held(&stream).contains(&5));
    }

    #[test]
    fn a_failed_read_is_asked_for_again() {
        let rows = rows(3);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..3).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * 3 + 40);
        disc.fail.push(0);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        run(
            &mut stream,
            &mut disc,
            &mut window,
            &input(&table, &sectors, [-10, 0, 0]),
            40,
        );
        assert_eq!(stream.counters.failed, 1);
        assert!(stream
            .live()
            .iter()
            .filter(|s| s.room != u16::MAX)
            .all(|s| s.view == 4));
    }

    #[test]
    fn the_farthest_room_makes_way_when_the_atlas_is_full() {
        let count = SLOTS + 10;
        let rows: Vec<[i16; 6]> = (0..count)
            .map(|room| [(room as i16) * 10, 0, 0, 200, 40, 0])
            .collect();
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..count as u32).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * count + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [0, 0, 0]);
        input.enter = 100_000;
        input.leave = 100_100;
        input.far = 100_000;
        run(&mut stream, &mut disc, &mut window, &input, 120);
        // The first SLOTS rooms (the nearest to the eye at x = 0) hold them;
        // nothing nearer is turned away and nothing farther takes over.
        assert_eq!(held(&stream), (0..SLOTS as u16).collect::<Vec<_>>());
        assert!(stream.counters.starved >= 10);
        // Move the eye to the far end: nearer rooms take over from the
        // farthest ones.
        input.eye = [(count as i32) * 10, 0, 0];
        run(&mut stream, &mut disc, &mut window, &input, 120);
        let now = held(&stream);
        assert!(now.contains(&(count as u16 - 1)));
        assert!(!now.contains(&0));
        assert!(stream.counters.evictions > 0);
    }

    /// Ticks the eye is held at `eye`, and for each tick the live entries of
    /// `room` (view, fade).
    fn watch(
        stream: &mut CardStream<usize>,
        disc: &mut Disc,
        window: &mut [u32],
        input: &CardInput<'_>,
        room: u16,
        ticks: usize,
    ) -> Vec<Vec<(u8, bool)>> {
        let mut seen = Vec::new();
        for _ in 0..ticks {
            stream.update(disc, window, input);
            let (ups, count) = stream.uploads::<2>();
            stream.applied(&ups[..count]);
            seen.push(
                stream
                    .live()
                    .iter()
                    .filter(|s| s.room == room)
                    .map(|s| (s.view, s.fade))
                    .collect(),
            );
        }
        seen
    }

    #[test]
    fn a_wrong_side_fades_in_beside_the_old_one_while_slots_are_free() {
        let rows = rows(12);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..12).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * 12 + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [-10, 0, 0]);
        input.far = 2000;
        run(&mut stream, &mut disc, &mut window, &input, 60);
        // Room 4 shows view 4. The eye walks past it: it is now seen from
        // the other side.
        input.eye = [800, 0, 0];
        let seen = watch(&mut stream, &mut disc, &mut window, &input, 4, 160);
        let pair = seen
            .iter()
            .find(|entries| entries.len() == 2)
            .expect("two slots for a while");
        // The old picture stays whole, the new one is the one fading in.
        assert!(
            pair.contains(&(4, false)) && pair.contains(&(0, true)),
            "{pair:?}"
        );
        // At the end one slot holds the new view, no longer fading.
        assert_eq!(seen.last().unwrap(), &[(0, false)]);
        // It never showed nothing on the way.
        assert!(seen.iter().all(|entries| !entries.is_empty()));
    }

    #[test]
    fn a_wrong_side_is_replaced_in_place_when_the_atlas_is_nearly_full() {
        // Slots all taken by rooms at the eye: nothing to spare.
        let count = SLOTS;
        let rows: Vec<[i16; 6]> = (0..count)
            .map(|room| [(room as i16) * 10 + 20, 0, 0, 200, 40, 0])
            .collect();
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..count as u32).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * count + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [0, 0, 0]);
        input.enter = 100_000;
        input.leave = 100_100;
        input.far = 100_000;
        run(&mut stream, &mut disc, &mut window, &input, 400);
        assert_eq!(held(&stream).len(), SLOTS);
        input.eye = [2000, 0, 0];
        let seen = watch(&mut stream, &mut disc, &mut window, &input, 3, 400);
        assert!(seen.iter().all(|entries| entries.len() == 1), "{seen:?}");
        assert_eq!(seen.last().unwrap(), &[(0, false)]);
    }

    #[test]
    fn a_picture_is_read_from_the_copy_near_the_eye_when_it_holds_the_view() {
        // Twelve rooms on the x axis in two tiles of six; the eye in tile 0.
        let rows = rows(12);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..12).map(|r| 8 * r + 1).collect();
        let centres = [[250, 0], [850, 0]];
        let room_tile: Vec<u8> = (0..12).map(|r| (r / 6) as u8).collect();
        let room_place: Vec<u8> = (0..12).map(|r| (r % 6) as u8).collect();
        // Tile 1's pictures of view 4 sit at sector 150 near tile 0 (mask
        // 0b1_0000, one view); tile 0 holds none near itself.
        let copies = [0, 150 | (0b1_0000 << 24), 0, 0];
        let mut disc = Disc::new(8 * 12 + 300);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [-10, 0, 0]);
        input.enter = 1200;
        input.leave = 1300;
        input.far = 1200;
        input.stride = 8;
        input.centres = &centres;
        input.copies = &copies;
        input.room_tile = &room_tile;
        input.room_place = &room_place;
        run(&mut stream, &mut disc, &mut window, &input, 80);
        // Room 7 (tile 1, place 1) wants view 4: its picture is at
        // 150 + 1; the copy holds nothing of view 4 for the rooms of tile 0.
        assert!(
            disc.reads
                .iter()
                .any(|&(lba, n)| lba <= 10 + 151 && 10 + 151 < lba + n),
            "{:?}",
            disc.reads
        );
        // Room 2 (tile 0) comes from the full set: sector 8 * 2 + 1 + 4 * 8.
        assert!(disc
            .reads
            .iter()
            .any(|&(lba, n)| lba <= 10 + 49 && 10 + 49 < lba + n));
        assert_eq!(stream.live().iter().filter(|s| s.view == 4).count(), 12);
    }

    #[test]
    fn pictures_that_lie_together_on_the_disc_come_in_one_read() {
        // Twelve rooms side by side, the cook's layout: the eight views of
        // all of them in blocks, room r's view k at sector 1 + 12 k + r.
        let rows = rows(12);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..12).map(|r| 1 + r).collect();
        let mut disc = Disc::new(1 + 12 * 8 + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [-10, 0, 0]);
        input.stride = 12;
        input.enter = 1200;
        input.leave = 1300;
        input.far = 1200;
        run(&mut stream, &mut disc, &mut window, &input, 80);
        assert_eq!(held(&stream).len(), 12);
        for live in stream.live().iter().filter(|s| s.room != u16::MAX) {
            assert_eq!(live.view, 4, "room {}", live.room);
        }
        // Twelve pictures in two reads of the eight sectors each can hold.
        assert!(disc.reads.len() <= 3, "{:?}", disc.reads);
        assert!(disc.reads.iter().any(|&(_, n)| n > 1));
        assert!(stream.counters.merged >= 8);
        // Each picture is the sector it was asked for: sector 1 + 12 * 4 + r.
        assert_eq!(stream.counters.uploads, 12);
    }

    #[test]
    fn a_blank_card_is_read_before_one_showing_the_wrong_side() {
        let rows = rows(4);
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..4).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * 4 + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [-10, 0, 0]);
        input.far = 1000;
        run(&mut stream, &mut disc, &mut window, &input, 40);
        assert!(stream
            .live()
            .iter()
            .filter(|s| s.room != u16::MAX)
            .all(|s| s.view == 4));
        // The eye walks round behind the cards; room 3 is the farthest.
        // Give room 0 (nearest) a stale view and room 3 a blank one.
        let slot0 = stream.by_room[0] as usize;
        let slot3 = stream.by_room[3] as usize;
        stream.slots[slot3].shown = NO_VIEW;
        stream.live[slot3].view = NO_VIEW;
        input.eye = [600, 0, 0];
        disc.reads.clear();
        stream.update(&mut disc, &mut window, &input);
        assert_eq!(stream.slots[slot0].want, 0);
        let &(lba, _) = disc.reads.first().expect("a read");
        // The first read holds room 3's picture of the view it wants (view 0
        // from x = 600: room 3 stands at x = 300): sector 8 * 3 + 1 + 0.
        assert!(
            lba <= 10 + 25 && 10 + 25 < lba + 8 || lba == 10 + 25,
            "lba {lba}"
        );
    }

    #[test]
    fn rooms_inside_the_card_distance_hold_slots_before_rooms_beyond_it() {
        // Rooms on both sides of the eye at x = 0: ten at positive x inside
        // `far`, the rest at negative x beyond it, more than the atlas holds.
        let count = SLOTS + 20;
        let rows: Vec<[i16; 6]> = (0..count)
            .map(|room| {
                let x = if room < 40 {
                    10 * room as i16
                } else {
                    -2000 - 10 * room as i16
                };
                [x, 0, 0, 200, 40, 0]
            })
            .collect();
        let table = CardTable::new(&rows);
        let sectors: Vec<u32> = (0..count as u32).map(|r| 8 * r + 1).collect();
        let mut disc = Disc::new(8 * count + 40);
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [0, 0, 0]);
        input.enter = 100_000;
        input.leave = 200_000;
        input.far = 500;
        run(&mut stream, &mut disc, &mut window, &input, 200);
        let now = held(&stream);
        assert!((0..40).all(|r| now.contains(&r)), "{now:?}");
    }

    #[test]
    fn the_room_the_eye_walks_towards_ranks_first() {
        let rows: Vec<[i16; 6]> = std::vec![[400, 0, 0, 200, 40, 0], [-400, 0, 0, 200, 40, 0]];
        let table = CardTable::new(&rows);
        let sectors = [1u32, 9];
        let mut stream = CardStream::<usize>::new();
        let mut input = input(&table, &sectors, [0, 0, 0]);
        input.far = 1000;
        input.lead = 200;
        // Walk towards +x.
        stream.update_heading([0, 0, 0]);
        stream.update_heading([100, 0, 0]);
        let ahead = stream.rank(-100 - 400 + 100, 0, 400, &input.band(0), 0, input.lead);
        let behind = stream.rank(-100 + 400 + 100, 0, 400, &input.band(0), 0, input.lead);
        assert!(ahead < behind, "{ahead} {behind}");
    }

    #[test]
    fn upload_nodes_copy_the_landed_sector_row_by_row() {
        let mut window = std::vec![0u32; CARD_WINDOW_WORDS];
        for (i, word) in window.iter_mut().enumerate() {
            *word = i as u32 + 1;
        }
        let uploads = [
            Some(Upload {
                slot: 26,
                cell: 1,
                room: 3,
                view: 2,
            }),
            None,
        ];
        let mut out = std::vec![0u32; upload_packet_words(1)];
        let (words, packets) =
            write_upload_packets(&uploads, &window, (320, 0), (0, 0), 2047, &mut out);
        assert_eq!(words, upload_packet_words(1));
        assert_eq!(packets, 1 + ROWS);
        // Walk the nodes as the GPU does, from the last written to the first.
        let mut nodes = Vec::new();
        let mut at = 0;
        while at < words {
            let payload = (out[at] >> 24) as usize;
            assert!(payload <= NODE_WORDS);
            assert_eq!(out[at] & 0xffff, 2047);
            nodes.push(&out[at + 1..at + 1 + payload]);
            at += payload + 1;
        }
        assert_eq!(at, words);
        let mut rows_seen = 0;
        for node in nodes.iter().rev() {
            if node[0] == 0x0100_0000 {
                continue;
            }
            assert_eq!(node[0], psx_hw::gpu::gp0::COPY_CPU_TO_VRAM);
            // Slot 26 is column 2, row 1: x = 320 + 32, y = 64 + row.
            assert_eq!(node[1] & 0xffff, 352);
            assert_eq!((node[1] >> 16) as usize, 64 + rows_seen);
            assert_eq!(node[2], 16 | (1 << 16));
            let first = SLOT_BYTES / 4 + rows_seen * ROW_WORDS;
            assert_eq!(node[3], window[first]);
            assert_eq!(node[10], window[first + ROW_WORDS - 1]);
            rows_seen += 1;
        }
        assert_eq!(rows_seen, ROWS);
    }
}
