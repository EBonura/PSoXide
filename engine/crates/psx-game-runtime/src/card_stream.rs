//! Keeping the far cards' pictures in VRAM while the player walks.
//!
//! A far card ([`psx_grid::render::cards`]) shows one baked view of its room,
//! the one nearest the direction the eye is in. The sky atlas is cut into
//! [`SLOTS`] slots, each the size of one picture (one 2 KiB disc sector, a
//! `16 x 64` halfword rectangle with its palette in the last row). This
//! module decides which room holds which slot, which view each slot should
//! show, and reads the missing views off the disc one sector at a time
//! through the asynchronous transport the rooms stream through
//! ([`RoomTransport`]).
//!
//! * **Membership.** A room with a card holds a slot while it is within
//!   `enter` units of the eye (plan distance) and gives it up past `leave`
//!   (the gap is the hysteresis). With no slot free the farthest slot past
//!   the newcomer's distance is taken over.
//! * **Views.** Each slot wants the view [`view_toward`] picks for the eye,
//!   kept while [`view_keep`] still says it serves. A slot whose shown view
//!   is not the wanted one is read next, nearest first.
//! * **Reads.** Up to [`STAGES`] sectors are in flight or waiting for the
//!   GPU, each landing in its own 2 KiB part of a window the game keeps. A
//!   landed picture is uploaded by [`write_upload_packets`] as GPU list
//!   nodes (one row each, under the 16 word node limit), so the GPU does the
//!   copy as it draws and the CPU never waits for it. Until it is, the slot
//!   keeps showing what it had.
//!
//! A failed read frees its stage and the slot is asked for again later.
//! Nothing here waits.

use psx_grid::render::{
    view_keep, view_toward, CardTable, LiveSlot, NO_SLOT, NO_VIEW, PICTURE_ROWS, SLOTS, SLOT_BYTES,
    SLOT_HALFWORDS, SLOT_ROWS,
};

use crate::grid_stream::{Poll, RoomTransport};

/// Reads in flight or landed and waiting for the GPU, at most.
pub const STAGES: usize = 4;
/// Words of the window the stream reads into.
pub const CARD_WINDOW_WORDS: usize = STAGES * SLOT_BYTES / 4;
/// Rooms the stream can track. A level with more rooms has its farthest
/// ones left without cards.
pub const MAX_ROOMS: usize = 512;
/// Ticks between two membership passes.
const PASS_TICKS: u32 = 4;

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
    /// Slots taken over from a farther room.
    pub evictions: u32,
    /// Submits the transport refused (its queue full).
    pub refused: u32,
    /// Slots in use at the last pass.
    pub live: u32,
    /// Rooms in range at the last pass that found no slot.
    pub starved: u32,
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
}

impl Slot {
    const EMPTY: Self = Self {
        room: u16::MAX,
        shown: NO_VIEW,
        want: NO_VIEW,
        pending: false,
    };
}

#[derive(Copy, Clone)]
enum Stage<K> {
    Free,
    Reading {
        ticket: K,
        slot: u8,
        room: u16,
        view: u8,
    },
    Ready {
        slot: u8,
        room: u16,
        view: u8,
    },
}

/// What a call to [`CardStream::update`] knows about the level and the eye.
pub struct CardInput<'a> {
    /// The card of each room.
    pub table: &'a CardTable<'a>,
    /// Per room, the sector of its pictures relative to `base_lba` (0 for
    /// none); empty for a level without cards.
    pub sectors: &'a [u32],
    /// Disc sector `sectors` is relative to.
    pub base_lba: u32,
    /// The eye, world runtime units.
    pub eye: [i32; 3],
    /// A room within this plan distance of the eye holds a slot.
    pub enter: i32,
    /// A room beyond this one gives its slot up.
    pub leave: i32,
}

/// A landed picture ready for the GPU.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// Atlas slot to write.
    pub slot: u8,
    /// Which stage of the window holds the picture.
    pub stage: u8,
    room: u16,
    view: u8,
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
    by_room: [u8; MAX_ROOMS],
    stages: [Stage<K>; STAGES],
    ticks: u32,
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
            },
            slots: [Slot::EMPTY; SLOTS],
            live: [LiveSlot::EMPTY; SLOTS],
            by_room: [NO_SLOT; MAX_ROOMS],
            stages: [Stage::Free; STAGES],
            ticks: 0,
        }
    }

    /// The slots as the card draw reads them: the room each holds and the
    /// view its VRAM shows.
    pub fn live(&self) -> &[LiveSlot; SLOTS] {
        &self.live
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
            if let Stage::Reading {
                ticket,
                slot,
                room,
                view,
            } = *stage
            {
                match transport.poll(ticket) {
                    Poll::Pending => {}
                    Poll::Done => {
                        self.counters.landed += 1;
                        *stage = Stage::Ready { slot, room, view };
                    }
                    Poll::Cancelled | Poll::Failed => {
                        self.counters.failed += 1;
                        let held = &mut self.slots[usize::from(slot)];
                        if held.room == room {
                            held.pending = false;
                        }
                        *stage = Stage::Free;
                    }
                }
            }
        }
    }

    /// Membership and wanted views.
    fn pass(&mut self, input: &CardInput<'_>) {
        let rooms = input.table.len().min(MAX_ROOMS);
        let [ex, _, ez] = input.eye;
        // Slots whose rooms went out of range, and slots' views.
        for slot in 0..SLOTS {
            let held = self.slots[slot];
            if held.room == u16::MAX {
                continue;
            }
            let Some(card) = input.table.card(usize::from(held.room)) else {
                self.release(slot);
                continue;
            };
            let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
            if plan_distance(dx, dz) > input.leave {
                self.release(slot);
                continue;
            }
            let want = if held.want != NO_VIEW && view_keep(held.want, dx, dz) {
                held.want
            } else {
                view_toward(dx, dz)
            };
            self.slots[slot].want = want;
        }
        // Rooms that came into range take a free slot, or a farther one.
        let mut starved = 0u32;
        for room in 0..rooms {
            if self.by_room[room] != NO_SLOT {
                continue;
            }
            let Some(card) = input.table.card(room) else {
                continue;
            };
            let (dx, dz) = (ex - card.centre[0], ez - card.centre[1]);
            let distance = plan_distance(dx, dz);
            if distance > input.enter {
                continue;
            }
            let Some(slot) = self.free_slot_for(distance, input) else {
                starved += 1;
                continue;
            };
            self.slots[slot] = Slot {
                room: room as u16,
                shown: NO_VIEW,
                want: view_toward(dx, dz),
                pending: false,
            };
            self.by_room[room] = slot as u8;
        }
        let mut live = 0;
        for (slot, held) in self.slots.iter().enumerate() {
            self.live[slot] = LiveSlot {
                room: held.room,
                view: held.shown,
            };
            live += u32::from(held.room != u16::MAX);
        }
        self.counters.live = live;
        self.counters.starved = starved;
    }

    /// Give a slot's room up.
    fn release(&mut self, slot: usize) {
        let room = self.slots[slot].room;
        if let Some(by) = self.by_room.get_mut(usize::from(room)) {
            *by = NO_SLOT;
        }
        self.slots[slot] = Slot::EMPTY;
    }

    /// An empty slot, or the one held by the room farthest past `distance`.
    fn free_slot_for(&mut self, distance: i32, input: &CardInput<'_>) -> Option<usize> {
        if let Some(slot) = self.slots.iter().position(|s| s.room == u16::MAX) {
            return Some(slot);
        }
        let [ex, _, ez] = input.eye;
        let mut worst: Option<(usize, i32)> = None;
        for (slot, held) in self.slots.iter().enumerate() {
            let Some(card) = input.table.card(usize::from(held.room)) else {
                continue;
            };
            let d = plan_distance(ex - card.centre[0], ez - card.centre[1]);
            if d > distance && worst.is_none_or(|(_, w)| d > w) {
                worst = Some((slot, d));
            }
        }
        let (slot, _) = worst?;
        self.release(slot);
        self.counters.evictions += 1;
        Some(slot)
    }

    /// Queue reads for the slots that need another view, nearest first, into
    /// the free stages.
    fn start_reads<T>(&mut self, transport: &mut T, window: &mut [u32], input: &CardInput<'_>)
    where
        T: RoomTransport<Ticket = K>,
    {
        let [ex, _, ez] = input.eye;
        loop {
            let Some(stage) = self.stages.iter().position(|s| matches!(s, Stage::Free)) else {
                return;
            };
            let mut best: Option<(usize, i32)> = None;
            for (slot, held) in self.slots.iter().enumerate() {
                if held.room == u16::MAX
                    || held.pending
                    || held.want == NO_VIEW
                    || held.shown == held.want
                {
                    continue;
                }
                let Some(card) = input.table.card(usize::from(held.room)) else {
                    continue;
                };
                let d = plan_distance(ex - card.centre[0], ez - card.centre[1]);
                if best.is_none_or(|(_, b)| d < b) {
                    best = Some((slot, d));
                }
            }
            let Some((slot, _)) = best else {
                return;
            };
            let held = self.slots[slot];
            let sector = input
                .sectors
                .get(usize::from(held.room))
                .copied()
                .unwrap_or(0);
            if sector == 0 {
                // A room without pictures: never asked again.
                self.slots[slot].want = NO_VIEW;
                continue;
            }
            let lba = input.base_lba + sector + u32::from(held.want);
            transport.begin_transfer();
            // SAFETY: the window is memory the caller keeps for the stream's
            // life, `stage` is one of its STAGES parts of one sector each,
            // and the part is not reused until the read is reported finished.
            let destination = unsafe { window.as_mut_ptr().add(stage * SLOT_BYTES / 4) };
            match unsafe { transport.submit(lba, 1, destination, false) } {
                Some(ticket) => {
                    self.stages[stage] = Stage::Reading {
                        ticket,
                        slot: slot as u8,
                        room: held.room,
                        view: held.want,
                    };
                    self.slots[slot].pending = true;
                    self.counters.submitted += 1;
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
            if count == N {
                break;
            }
            if let Stage::Ready { slot, room, view } = *stage {
                let held = &mut self.slots[usize::from(slot)];
                if held.room != room {
                    // The slot was given to another room meanwhile.
                    *stage = Stage::Free;
                    continue;
                }
                out[count] = Some(Upload {
                    slot,
                    stage: index as u8,
                    room,
                    view,
                });
                count += 1;
            }
        }
        (out, count)
    }

    /// The uploads in `taken` are in a frame's list: VRAM now holds their
    /// pictures and their stages are free again.
    pub fn applied(&mut self, taken: &[Option<Upload>]) {
        for upload in taken.iter().flatten() {
            let held = &mut self.slots[usize::from(upload.slot)];
            if held.room == upload.room {
                held.shown = upload.view;
                held.pending = false;
                self.live[usize::from(upload.slot)].view = upload.view;
            }
            self.stages[usize::from(upload.stage)] = Stage::Free;
            self.counters.uploads += 1;
        }
    }

    /// Stop every read (on leaving gameplay) and forget every slot.
    pub fn release_all<T>(&mut self, transport: &mut T)
    where
        T: RoomTransport<Ticket = K>,
    {
        for stage in &self.stages {
            if let Stage::Reading { ticket, .. } = stage {
                transport.cancel(*ticket);
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
        let (x, y) = psx_grid::render::slot_origin(origin, usize::from(upload.slot));
        let from = usize::from(upload.stage) * SLOT_BYTES / 4;
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
            base_lba: 10,
            eye,
            enter: 450,
            leave: 600,
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
        let held: Vec<u16> = stream
            .live()
            .iter()
            .filter(|s| s.room != u16::MAX)
            .map(|s| s.room)
            .collect();
        assert_eq!(held.len(), 5, "{held:?}");
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
        let held: Vec<u16> = stream
            .live()
            .iter()
            .filter(|s| s.room != u16::MAX)
            .map(|s| s.room)
            .collect();
        assert!(!held.contains(&0) && !held.contains(&1));
        assert!(held.contains(&8) && held.contains(&10));
        // A room ahead of the eye (x = 1000 > 800) is seen from -x: view 4.
        let at = stream.live().iter().position(|s| s.room == 10).unwrap();
        assert_eq!(stream.live()[at].view, 4);
        // A room behind the eye (x = 500 < 800) is seen from +x: view 0.
        let at = stream.live().iter().position(|s| s.room == 5).unwrap();
        assert_eq!(stream.live()[at].view, 0);
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
        input.leave = 200_000;
        run(&mut stream, &mut disc, &mut window, &input, 8);
        let held = |stream: &CardStream<usize>| -> Vec<u16> {
            let mut v: Vec<u16> = stream
                .live()
                .iter()
                .filter(|s| s.room != u16::MAX)
                .map(|s| s.room)
                .collect();
            v.sort_unstable();
            v
        };
        // The first SLOTS rooms (the nearest to the eye at x = 0) hold them;
        // nothing nearer is turned away and nothing farther takes over.
        assert_eq!(held(&stream), (0..SLOTS as u16).collect::<Vec<_>>());
        assert!(stream.counters.starved >= 10);
        // Move the eye to the far end: nearer rooms take over from the
        // farthest ones.
        input.eye = [(count as i32) * 10, 0, 0];
        run(&mut stream, &mut disc, &mut window, &input, 16);
        let now = held(&stream);
        assert!(now.contains(&(count as u16 - 1)));
        assert!(!now.contains(&0));
        assert!(stream.counters.evictions > 0);
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
                stage: 1,
                room: 3,
                view: 2,
            }),
            None,
        ];
        let mut out = std::vec![0u32; upload_packet_words(1)];
        let (words, packets) = write_upload_packets(&uploads, &window, (320, 0), 2047, &mut out);
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
