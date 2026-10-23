//! Streaming the room chunks of a grid level off `WORLD.PAK`.
//!
//! [`GridStreamer`] is the glue between three parts that each do one job:
//!
//! * `psx_grid::stream` says which rooms are wanted (the need set the camera
//!   and the activation radius reach, and the lead ring beyond it);
//! * `psx_residency` decides which of them to read next, where to put them in
//!   a page pool, and what to evict to make room;
//! * the CD transport (`psx_cdstream`, through [`RoomTransport`]) reads
//!   sectors straight into the pool pages, one sector per interrupt.
//!
//! The streamer owns no policy. Each frame the game hands it the wanted list;
//! it polls the reads in flight, steps the residency engine, reports what was
//! evicted (the caller unlinks those rooms before it touches room data again)
//! and installs at most one landed room per call (the caller parses it and
//! makes it resident). A room that has not arrived is not an error: the
//! collision world treats it as a solid box and the renderer skips it, so a
//! player who outruns the stream meets a wall, not a hole.
//!
//! Failure never stalls anything: a failed read or install frees its pages
//! and the room backs off before the next try (`psx_residency::Backoff`).

use core::mem::MaybeUninit;

use psx_grid::RoomRecord;
use psx_residency::{
    Category, CategoryConfig, Class, Config, Eviction, Extent, Host, Placement, PoolConfig,
    ReadOutcome, ReadRequest, RequestId, Residency, ResourceKey, State, Submit, Wanted, PAGE_BYTES,
};

/// The residency category rooms live in.
pub const ROOM_CATEGORY: Category = Category::WORLD_REGION;

/// Reads the streamer keeps in flight at most (the residency engine's own
/// `max_in_flight` is 2; the transport queue holds 4).
const FLIGHT: usize = 4;
/// Evictions one step reports.
const EVICTIONS: usize = 8;

/// How a request ended, as the transport reports it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Queued or reading.
    Pending,
    /// Every sector landed in the destination.
    Done,
    /// Stopped after a cancel; the sectors that landed are not used.
    Cancelled,
    /// The drive failed it for good, or the transport lost it.
    Failed,
}

/// What the streamer needs from the CD transport. The console implementation
/// calls `psx-cdstream` ([`DiscTransport`]); the tests stand in a script.
pub trait RoomTransport {
    /// Identity of one queued read.
    type Ticket: Copy;

    /// Queue `sectors` sectors from `lba` into `destination`. `urgent` reads
    /// go first. `None` when the queue is full.
    ///
    /// # Safety
    /// `destination` must be word aligned and valid for `sectors * 2048`
    /// bytes until the request has been reported finished by [`Self::poll`]
    /// (or cancelled and then reported finished).
    unsafe fn submit(
        &mut self,
        lba: u32,
        sectors: u32,
        destination: *mut u32,
        urgent: bool,
    ) -> Option<Self::Ticket>;

    /// How `ticket` stands.
    fn poll(&mut self, ticket: Self::Ticket) -> Poll;

    /// Stop `ticket` at the next sector.
    fn cancel(&mut self, ticket: Self::Ticket);

    /// The streamer is about to put a read on the drive: make sure the
    /// transport exists and no music holds the drive.
    fn begin_transfer(&mut self);
}

/// The console's transport: the global `psx-cdstream` engine, through the
/// same arbitration with music the blocking readers use.
#[derive(Copy, Clone, Debug, Default)]
pub struct DiscTransport;

#[cfg(target_arch = "mips")]
impl RoomTransport for DiscTransport {
    type Ticket = psx_cdstream::Ticket;

    unsafe fn submit(
        &mut self,
        lba: u32,
        sectors: u32,
        destination: *mut u32,
        urgent: bool,
    ) -> Option<Self::Ticket> {
        // SAFETY: forwarded from the caller's contract.
        let request = unsafe { psx_cdstream::Request::new_raw(lba, sectors, destination) }
            .with_resumes(3)
            .with_priority(if urgent {
                psx_cdstream::Priority::URGENT
            } else {
                psx_cdstream::Priority::NORMAL
            });
        psx_cdstream::submit(request).ok()
    }

    fn poll(&mut self, ticket: Self::Ticket) -> Poll {
        use psx_cdstream::{Outcome, RequestState};
        match psx_cdstream::state(ticket) {
            RequestState::Queued | RequestState::Active { .. } => Poll::Pending,
            RequestState::Finished(done) => match done.outcome {
                Outcome::Done => Poll::Done,
                Outcome::Cancelled => Poll::Cancelled,
                Outcome::Failed(_) => Poll::Failed,
            },
            RequestState::Unknown => Poll::Failed,
        }
    }

    fn cancel(&mut self, ticket: Self::Ticket) {
        let _ = psx_cdstream::cancel(ticket);
    }

    fn begin_transfer(&mut self) {
        psx_engine::cd_drive::ensure_transport();
        // Music holding the lease would leave the read queued forever. It is
        // paused where it stands and resumes once the reads are done.
        psx_engine::cd_drive::yield_music_for_data();
    }
}

/// The host build has no drive: nothing can be queued.
#[cfg(not(target_arch = "mips"))]
impl RoomTransport for DiscTransport {
    type Ticket = u32;

    unsafe fn submit(&mut self, _: u32, _: u32, _: *mut u32, _: bool) -> Option<u32> {
        None
    }

    fn poll(&mut self, _: u32) -> Poll {
        Poll::Failed
    }

    fn cancel(&mut self, _: u32) {}

    fn begin_transfer(&mut self) {}
}

/// Counters the streamer keeps itself (the residency engine has its own,
/// [`GridStreamer::totals`]).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamCounters {
    /// Rooms made resident (installed).
    pub installed: u32,
    /// Installs the caller refused (a chunk that did not parse).
    pub failed_installs: u32,
    /// Reads the transport had no room to queue.
    pub refused_submits: u32,
    /// Reads reported lost by the transport (failed or unknown).
    pub failed_reads: u32,
    /// Steps in which a need room was not resident.
    pub short_windows: u32,
}

#[derive(Copy, Clone)]
struct Flight<K: Copy> {
    /// Residency request id, 0 when the slot is free.
    id: u32,
    ticket: Option<K>,
}

impl<K: Copy> Flight<K> {
    const EMPTY: Self = Self {
        id: 0,
        ticket: None,
    };
}

/// What one [`GridStreamer::step`] did.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StepSummary {
    /// Reads put on the transport this step.
    pub issued: u32,
    /// Rooms evicted this step.
    pub evicted: u32,
    /// Wanted rooms that were not resident when the step ended.
    pub missing: u32,
    /// Reads in flight when the step ended.
    pub in_flight: u32,
}

/// The room pool, the residency state and the reads in flight.
///
/// `ENTRIES` bounds the rooms tracked at once (resident, reading, landed or
/// backing off) and so also the wanted list a step considers; the cook sizes
/// it. The page memory is not part of the struct: it is a separate static of
/// the game's, so this one stays small whatever the pool costs.
pub struct GridStreamer<const ENTRIES: usize, T: RoomTransport> {
    pool: *mut u8,
    pages: u32,
    residency: MaybeUninit<Residency<ENTRIES, ENTRIES, 1>>,
    flight: [Flight<T::Ticket>; FLIGHT],
    /// Absolute LBA of the sector room records count `pack_sector` from.
    base_lba: u32,
    started: bool,
    counters: StreamCounters,
}

impl<const ENTRIES: usize, T: RoomTransport> GridStreamer<ENTRIES, T> {
    /// A streamer that has not started. `const`, so it can sit in a static.
    pub const fn new() -> Self {
        Self {
            pool: core::ptr::null_mut(),
            pages: 0,
            residency: MaybeUninit::uninit(),
            flight: [Flight::EMPTY; FLIGHT],
            base_lba: 0,
            started: false,
            counters: StreamCounters {
                installed: 0,
                failed_installs: 0,
                refused_submits: 0,
                failed_reads: 0,
                short_windows: 0,
            },
        }
    }

    /// Start streaming into `pool` (page memory, a whole number of 2 KiB
    /// pages that stays where it is for the rest of the run). Room records
    /// are read at `base_lba + pack_sector`. `keep_radius` is the walk
    /// distance, in the units of the wanted list, beyond which an unwanted
    /// room is a free victim.
    pub fn start(&mut self, pool: &'static mut [u32], base_lba: u32, keep_radius: u32) {
        let pages = (pool.len() * 4 / PAGE_BYTES) as u32;
        let mut config = Config::<1>::new();
        config.pools[0] = PoolConfig {
            capacity_pages: pages,
            placement: Placement::BestFit,
        };
        config.categories[ROOM_CATEGORY.number() as usize] = CategoryConfig {
            pool: 0,
            max_pages: pages,
            max_entries: ENTRIES.min(usize::from(u16::MAX) - 1) as u16,
            // Parsed views borrow the chunk bytes, so a room never moves.
            movable: false,
            installs: true,
            keep_radius,
        };
        self.residency.write(Residency::new(config));
        self.pool = pool.as_mut_ptr().cast::<u8>();
        self.pages = pages;
        self.base_lba = base_lba;
        self.flight = [Flight::EMPTY; FLIGHT];
        self.counters = StreamCounters::default();
        self.started = true;
    }

    /// Whether [`Self::start`] has run.
    pub const fn started(&self) -> bool {
        self.started
    }

    /// Pages in the pool.
    pub const fn pages(&self) -> u32 {
        self.pages
    }

    /// The streamer's counters.
    pub const fn counters(&self) -> StreamCounters {
        self.counters
    }

    fn res(&self) -> &Residency<ENTRIES, ENTRIES, 1> {
        debug_assert!(self.started);
        // SAFETY: `start` writes it before `started` is set, and nothing
        // reads it earlier.
        unsafe { self.residency.assume_init_ref() }
    }

    /// The residency engine's counters since [`Self::start`].
    pub fn totals(&self) -> psx_residency::Totals {
        self.res().totals()
    }

    /// Pages in use by rooms (resident, reading, landed).
    pub fn pages_in_use(&self) -> u32 {
        self.res().category_pages(ROOM_CATEGORY)
    }

    /// Reads in flight.
    pub fn in_flight(&self) -> usize {
        self.flight.iter().filter(|f| f.id != 0).count()
    }

    /// Whether room `room` is resident (parsed and linked).
    pub fn is_resident(&self, room: u16) -> bool {
        self.started && self.res().is_resident(room_key(room))
    }

    /// Whether room `room` is anywhere between wanted and resident: reading,
    /// landed or installing.
    pub fn is_loading(&self, room: u16) -> bool {
        self.started
            && matches!(
                self.res().state(room_key(room)),
                State::Reading | State::Landed | State::Installing
            )
    }

    /// The bytes of a resident room, valid until it is evicted: its pages
    /// from the start of the run.
    ///
    /// The slice is `'static` only because the pool is; the caller drops every
    /// view of a room when [`Self::step`] reports it evicted.
    fn bytes_of(&self, key: ResourceKey) -> Option<&'static [u8]> {
        let res = self.res();
        let handle = res.handle(key)?;
        let run = res.try_run(handle)?;
        if run.end_page() > self.pages {
            return None;
        }
        // SAFETY: the run lies inside the pool, which is valid for the
        // program's life; the residency engine holds the pages until the key
        // is evicted, and the caller unlinks the slice then.
        Some(unsafe {
            core::slice::from_raw_parts(
                self.pool.add(run.first_page as usize * PAGE_BYTES),
                run.capacity_bytes(),
            )
        })
    }

    /// Poll the reads in flight, step the residency engine with `wanted`,
    /// and report every eviction to `on_evict` (the room index). `wanted` must
    /// list the most urgent rooms first and no more than `ENTRIES`.
    ///
    /// `rooms` are the level's room records (the sectors to read), and
    /// `distance` answers the walk distance of a resident room the wanted list
    /// did not name.
    pub fn step(
        &mut self,
        transport: &mut T,
        rooms: &[RoomRecord],
        wanted: &[Wanted],
        distance: &dyn Fn(u16) -> u32,
        on_evict: &mut dyn FnMut(u16),
    ) -> StepSummary {
        debug_assert!(self.started);
        // 1. Reads that finished.
        for slot in 0..FLIGHT {
            let Flight {
                id,
                ticket: Some(ticket),
            } = self.flight[slot]
            else {
                continue;
            };
            let outcome = match transport.poll(ticket) {
                Poll::Pending => continue,
                Poll::Done => ReadOutcome::Done,
                Poll::Cancelled => ReadOutcome::Cancelled,
                Poll::Failed => {
                    self.counters.failed_reads += 1;
                    ReadOutcome::Failed
                }
            };
            // SAFETY: `start` ran.
            unsafe { self.residency.assume_init_mut() }.complete_read(RequestId(id), outcome);
            self.flight[slot] = Flight::EMPTY;
        }
        // 2. Step the engine; its host talks to the transport and the pool.
        let mut evictions = [Eviction::NONE; EVICTIONS];
        let mut host = RoomHost {
            transport,
            rooms,
            base_lba: self.base_lba,
            pool: self.pool,
            pages: self.pages,
            flight: &mut self.flight,
            distance,
            refused: 0,
        };
        // SAFETY: `start` ran.
        let report =
            unsafe { self.residency.assume_init_mut() }.step(wanted, &mut host, &mut evictions);
        self.counters.refused_submits += host.refused;
        for eviction in &evictions[..report.eviction_count] {
            on_evict(eviction.key.index() as u16);
        }
        let missing = wanted
            .iter()
            .filter(|w| !self.res().is_resident(w.key))
            .count() as u32;
        if wanted
            .iter()
            .any(|w| w.class == Class::Demand && !self.res().is_resident(w.key))
        {
            self.counters.short_windows += 1;
        }
        StepSummary {
            issued: report.stats.issued,
            evicted: report.eviction_count as u32,
            missing,
            in_flight: self.in_flight() as u32,
        }
    }

    /// Whether a step or an install has something to act on that the last
    /// step left: a read in flight that has finished, or a landed room waiting
    /// to be installed. Cheap (one poll per read in flight and one pass over
    /// the table), so a caller whose wanted list has not changed can use it to
    /// skip the step until it says so.
    pub fn has_work(&self, transport: &mut T) -> bool {
        debug_assert!(self.started);
        let finished = self.flight.iter().any(|flight| {
            flight
                .ticket
                .is_some_and(|ticket| transport.poll(ticket) != Poll::Pending)
        });
        finished || self.res().next_install(None).is_some()
    }

    /// Install at most one landed room: hand its bytes to `install` (parse the
    /// chunk and make it resident), and mark it resident when that succeeds.
    /// Returns the room installed. A room that fails to install backs off and
    /// is read again later.
    pub fn install_next(
        &mut self,
        install: &mut dyn FnMut(u16, &'static [u8]) -> bool,
    ) -> Option<u16> {
        debug_assert!(self.started);
        let key = self.res().next_install(None)?;
        // SAFETY: `start` ran.
        let res = unsafe { self.residency.assume_init_mut() };
        res.begin_install(key)?;
        let room = key.index() as u16;
        let bytes = self.bytes_of(key);
        // SAFETY: as above.
        let res = unsafe { self.residency.assume_init_mut() };
        match bytes {
            Some(bytes) if install(room, bytes) => {
                res.finish_install(key);
                self.counters.installed += 1;
                Some(room)
            }
            _ => {
                res.fail_install(key);
                self.counters.failed_installs += 1;
                None
            }
        }
    }
}

impl<const ENTRIES: usize, T: RoomTransport> Default for GridStreamer<ENTRIES, T> {
    fn default() -> Self {
        Self::new()
    }
}

/// The key of room `room`.
pub fn room_key(room: u16) -> ResourceKey {
    ResourceKey::new(ROOM_CATEGORY, u32::from(room)).expect("a room index fits a key")
}

/// A wanted entry for `room`.
pub fn wanted_room(room: u16, class: Class, distance: u32) -> Wanted {
    Wanted::new(room_key(room), class, distance)
}

/// The residency engine's view of the world for one step.
struct RoomHost<'a, T: RoomTransport> {
    transport: &'a mut T,
    rooms: &'a [RoomRecord],
    base_lba: u32,
    pool: *mut u8,
    pages: u32,
    flight: &'a mut [Flight<T::Ticket>; FLIGHT],
    distance: &'a dyn Fn(u16) -> u32,
    refused: u32,
}

impl<T: RoomTransport> Host for RoomHost<'_, T> {
    fn extent(&self, key: ResourceKey) -> Option<Extent> {
        let record = self.rooms.get(key.index() as usize)?;
        let sectors = u32::from(record.pack_sectors);
        (sectors > 0).then_some(Extent {
            lba: self.base_lba + record.pack_sector,
            sectors,
            pages: sectors,
            bytes: sectors * PAGE_BYTES as u32,
        })
    }

    fn walk_distance(&self, key: ResourceKey) -> u32 {
        (self.distance)(key.index() as u16)
    }

    fn can_accept(&self) -> bool {
        self.flight.iter().any(|f| f.id == 0)
    }

    fn submit(&mut self, request: &ReadRequest) -> Submit {
        let Some(slot) = self.flight.iter().position(|f| f.id == 0) else {
            return Submit::Full;
        };
        if request.run.end_page() > self.pages {
            return Submit::Full;
        }
        self.transport.begin_transfer();
        // SAFETY: the run is inside the pool (checked above), page aligned in
        // a word-aligned static, and the residency engine keeps the pages
        // reserved until the read is reported finished or cancelled.
        let ticket = unsafe {
            self.transport.submit(
                request.lba,
                request.sectors,
                self.pool
                    .add(request.run.first_page as usize * PAGE_BYTES)
                    .cast::<u32>(),
                request.class <= Class::CombatFill,
            )
        };
        match ticket {
            Some(ticket) => {
                self.flight[slot] = Flight {
                    id: request.id.0,
                    ticket: Some(ticket),
                };
                Submit::Accepted
            }
            None => {
                self.refused += 1;
                Submit::Full
            }
        }
    }

    fn cancel(&mut self, request: RequestId) {
        if let Some(flight) = self.flight.iter().find(|f| f.id == request.0) {
            if let Some(ticket) = flight.ticket {
                self.transport.cancel(ticket);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;
    use std::vec;
    use std::vec::Vec;

    const BASE: u32 = 1000;

    /// A drive that holds requests until the test finishes them, and writes
    /// the room's number into the first word of every sector it "reads".
    #[derive(Default)]
    struct Script {
        reqs: Vec<Req>,
        full_at: usize,
    }

    struct Req {
        lba: u32,
        sectors: u32,
        dest: *mut u32,
        urgent: bool,
        state: Poll,
        cancelled: bool,
    }

    impl RoomTransport for Script {
        type Ticket = usize;

        unsafe fn submit(
            &mut self,
            lba: u32,
            sectors: u32,
            dest: *mut u32,
            urgent: bool,
        ) -> Option<usize> {
            if self.full_at != 0
                && self
                    .reqs
                    .iter()
                    .filter(|r| r.state == Poll::Pending)
                    .count()
                    >= self.full_at
            {
                return None;
            }
            self.reqs.push(Req {
                lba,
                sectors,
                dest,
                urgent,
                state: Poll::Pending,
                cancelled: false,
            });
            Some(self.reqs.len() - 1)
        }

        fn poll(&mut self, ticket: usize) -> Poll {
            self.reqs[ticket].state
        }

        fn cancel(&mut self, ticket: usize) {
            self.reqs[ticket].cancelled = true;
        }

        fn begin_transfer(&mut self) {}
    }

    impl Script {
        /// Finish request `n`: payload written, or cancelled when asked to.
        fn finish(&mut self, n: usize, room: u32) {
            let req = &mut self.reqs[n];
            if req.cancelled {
                req.state = Poll::Cancelled;
                return;
            }
            for sector in 0..req.sectors as usize {
                // SAFETY: the streamer promised `sectors * 2048` bytes.
                unsafe { *req.dest.add(sector * 512) = room };
            }
            req.state = Poll::Done;
        }

        fn fail(&mut self, n: usize) {
            self.reqs[n].state = Poll::Failed;
        }

        fn pending(&self) -> Vec<usize> {
            (0..self.reqs.len())
                .filter(|&n| self.reqs[n].state == Poll::Pending)
                .collect()
        }
    }

    /// `count` rooms of `pages` pages each, back to back from sector 0.
    fn rooms(count: usize, pages: u16) -> Vec<RoomRecord> {
        (0..count)
            .map(|i| RoomRecord {
                pack_sector: i as u32 * u32::from(pages),
                pack_sectors: pages,
                ..RoomRecord::default()
            })
            .collect()
    }

    fn pool(pages: usize) -> &'static mut [u32] {
        Box::leak(vec![0u32; pages * 512].into_boxed_slice())
    }

    fn streamer<const E: usize>(pages: usize) -> GridStreamer<E, Script> {
        let mut streamer = GridStreamer::<E, Script>::new();
        streamer.start(pool(pages), BASE, 1_000);
        streamer
    }

    fn room_of_lba(lba: u32, pages: u32) -> u32 {
        (lba - BASE) / pages
    }

    /// One frame: step, finish every pending read, step again and install.
    fn settle<const E: usize>(
        s: &mut GridStreamer<E, Script>,
        t: &mut Script,
        rooms: &[RoomRecord],
        wanted: &[Wanted],
        pages: u32,
        resident: &mut Vec<u16>,
    ) {
        for _ in 0..24 {
            let evicted = &mut Vec::new();
            s.step(t, rooms, wanted, &|_| u32::MAX, &mut |r| evicted.push(r));
            resident.retain(|r| !evicted.contains(r));
            for n in t.pending() {
                let room = room_of_lba(t.reqs[n].lba, pages);
                t.finish(n, room);
            }
            s.step(t, rooms, wanted, &|_| u32::MAX, &mut |r| {
                resident.retain(|x| *x != r)
            });
            while let Some(room) = s.install_next(&mut |room, bytes| {
                assert_eq!(
                    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
                    u32::from(room),
                    "the pages hold the room that was read"
                );
                true
            }) {
                resident.push(room);
            }
        }
    }

    fn want(room: u16, class: Class, distance: u32) -> Wanted {
        wanted_room(room, class, distance)
    }

    #[test]
    fn a_streamer_has_work_only_when_a_read_finished_or_a_room_landed() {
        let rooms = rooms(3, 3);
        let mut t = Script::default();
        let mut s = streamer::<3>(9);
        let wanted = [want(0, Class::Demand, 0), want(1, Class::Demand, 100)];
        assert!(!s.has_work(&mut t), "nothing in flight yet");
        s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        assert_eq!(t.pending().len(), 2);
        assert!(!s.has_work(&mut t), "both reads are still on the drive");
        t.finish(0, 0);
        assert!(
            s.has_work(&mut t),
            "a read finished and has not been collected"
        );
        s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        assert!(s.has_work(&mut t), "the landed room waits to be installed");
        assert_eq!(s.install_next(&mut |_, _| true), Some(0));
        assert!(!s.has_work(&mut t), "the other read is still pending");
        t.fail(1);
        assert!(s.has_work(&mut t), "a failed read also needs a step");
    }

    #[test]
    fn wanted_rooms_are_read_nearest_first_and_become_resident() {
        let rooms = rooms(4, 3);
        let mut t = Script::default();
        let mut s = streamer::<4>(12);
        let wanted = [
            want(2, Class::Lead, 300),
            want(0, Class::Demand, 0),
            want(1, Class::Demand, 100),
        ];
        let summary = s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        // Two reads at a time; the demand rooms go first, nearest first.
        assert_eq!(summary.issued, 2);
        assert_eq!(t.reqs[0].lba, BASE);
        assert_eq!(t.reqs[1].lba, BASE + 3);
        assert!(t.reqs[0].urgent && t.reqs[1].urgent);
        assert_eq!(summary.missing, 3);
        let mut resident = Vec::new();
        settle(&mut s, &mut t, &rooms, &wanted, 3, &mut resident);
        resident.sort_unstable();
        assert_eq!(resident, vec![0, 1, 2]);
        assert!(s.is_resident(0) && s.is_resident(2) && !s.is_resident(3));
        assert_eq!(s.pages_in_use(), 9);
        assert_eq!(s.counters().installed, 3);
    }

    #[test]
    fn a_pool_smaller_than_the_world_evicts_what_is_left_behind() {
        // Six rooms of 3 pages in a 9 page pool: three at a time.
        let rooms = rooms(6, 3);
        let mut t = Script::default();
        let mut s = streamer::<6>(9);
        let mut resident = Vec::new();
        let first = [
            want(0, Class::Demand, 0),
            want(1, Class::Demand, 100),
            want(2, Class::Lead, 200),
        ];
        settle(&mut s, &mut t, &rooms, &first, 3, &mut resident);
        resident.sort_unstable();
        assert_eq!(resident, vec![0, 1, 2]);
        // The player walks on: 3 and 4 are needed, 2 is the lead, 0 and 1 are
        // behind. Making room evicts the rooms farthest behind.
        let second = [
            want(3, Class::Demand, 0),
            want(4, Class::Demand, 100),
            want(2, Class::Lead, 200),
        ];
        settle(&mut s, &mut t, &rooms, &second, 3, &mut resident);
        resident.sort_unstable();
        assert_eq!(resident, vec![2, 3, 4]);
        assert!(s.totals().evictions >= 2);
        assert!(s.pages_in_use() <= 9);
        // Once the need rooms are in, a window is no longer short.
        let before = s.counters().short_windows;
        let summary = s.step(&mut t, &rooms, &second, &|_| u32::MAX, &mut |_| {});
        assert_eq!(summary.missing, 0);
        assert_eq!(s.counters().short_windows, before);
    }

    #[test]
    fn a_failed_read_backs_off_and_is_read_again() {
        let rooms = rooms(2, 2);
        let mut t = Script::default();
        let mut s = streamer::<2>(4);
        let wanted = [want(0, Class::Demand, 0)];
        s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        t.fail(0);
        s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        assert_eq!(s.counters().failed_reads, 1);
        assert!(!s.is_loading(0), "the failed read freed its pages");
        // Inside the backoff nothing is issued; after it the room is read again.
        let reads = t.reqs.len();
        let mut resident = Vec::new();
        settle(&mut s, &mut t, &rooms, &wanted, 2, &mut resident);
        assert!(t.reqs.len() > reads, "the room was asked for again");
        assert_eq!(resident, vec![0]);
    }

    #[test]
    fn a_chunk_that_does_not_install_is_dropped_and_read_again() {
        let rooms = rooms(1, 2);
        let mut t = Script::default();
        let mut s = streamer::<1>(2);
        let wanted = [want(0, Class::Demand, 0)];
        s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        t.finish(0, 0);
        s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        assert_eq!(s.install_next(&mut |_, _| false), None);
        assert_eq!(s.counters().failed_installs, 1);
        assert!(!s.is_resident(0));
        let mut resident = Vec::new();
        settle(&mut s, &mut t, &rooms, &wanted, 2, &mut resident);
        assert_eq!(resident, vec![0]);
    }

    #[test]
    fn a_full_transport_leaves_the_room_for_the_next_step() {
        let rooms = rooms(3, 1);
        let mut t = Script {
            full_at: 1,
            ..Script::default()
        };
        let mut s = streamer::<3>(3);
        let wanted = [
            want(0, Class::Demand, 0),
            want(1, Class::Demand, 10),
            want(2, Class::Demand, 20),
        ];
        let summary = s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        assert_eq!(summary.issued, 1);
        assert_eq!(s.in_flight(), 1);
        let mut resident = Vec::new();
        settle(&mut s, &mut t, &rooms, &wanted, 1, &mut resident);
        resident.sort_unstable();
        assert_eq!(resident, vec![0, 1, 2]);
    }

    #[test]
    fn a_room_with_no_sectors_is_unknown_and_never_read() {
        let mut rooms = rooms(2, 2);
        rooms[1].pack_sectors = 0;
        let mut t = Script::default();
        let mut s = streamer::<2>(4);
        let wanted = [want(1, Class::Demand, 0)];
        let summary = s.step(&mut t, &rooms, &wanted, &|_| u32::MAX, &mut |_| {});
        assert_eq!(summary.issued, 0);
        assert_eq!(summary.missing, 1);
    }
}
