//! Host tests: the run, the readers and the job against a scripted drive behind
//! the real `psx-cdstream` engine.

extern crate std;

use std::collections::VecDeque;
use std::vec::Vec;

use psx_cdstream::{CdHw, Config, Engine, LeaseState, Owner};
use psx_hw::cd::{CMD_PAUSE, CMD_READN, CMD_SEEKL, CMD_SETLOC, CMD_SETMODE};
use psx_level::LevelWorldPackEntryRecord;

use super::{Poll, Request, RequestState, Run, Stage, SubmitError, Ticket, Transport};
use crate::cd_stream::{
    read_chunk_banded_with, read_chunk_blocking_with, read_chunks_contiguous_with, CdController,
    UiChunkPlan, WorldChunkDestination, WorldRoomSlotsReadJob, FNV_OFFSET, FNV_PRIME,
    ROOM_CHUNK_STATUS_OK, SECTOR_BYTES, SECTOR_WORDS, STATUS_CD_ERROR,
};

/// One word of the fake disc.
fn disc_word(lba: u32, word: u32) -> u32 {
    lba.wrapping_mul(0x9E37_79B1) ^ word.wrapping_mul(0x85EB_CA6B) ^ 0xA5A5_0000
}

fn disc_byte(lba: u32, index: usize) -> u8 {
    disc_word(lba, (index / 4) as u32).to_le_bytes()[index % 4]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Data(u32),
    Complete,
    Acknowledge,
    Error,
}

impl Event {
    fn code(self) -> u8 {
        match self {
            Event::Data(_) => 1,
            Event::Complete => 2,
            Event::Acknowledge => 3,
            Event::Error => 5,
        }
    }
}

/// A scripted CD controller that raises the interrupts a real drive would.
struct Drive {
    events: VecDeque<Event>,
    reading: bool,
    next_lba: u32,
    target: u32,
    output: bool,
    source: bool,
    vblank: u32,
    /// Setloc targets, in order.
    setlocs: Vec<u32>,
    /// A drive error replaces the data interrupt of this sector, once.
    error_at: Vec<u32>,
}

impl Drive {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            reading: false,
            next_lba: 0,
            target: 0,
            output: false,
            source: false,
            vblank: 0,
            setlocs: Vec::new(),
            error_at: Vec::new(),
        }
    }

    fn raise(&mut self) -> bool {
        if self.events.is_empty() && self.reading {
            let lba = self.next_lba;
            self.next_lba += 1;
            if let Some(at) = self.error_at.iter().position(|&bad| bad == lba) {
                self.error_at.remove(at);
                self.reading = false;
                self.events.push_back(Event::Error);
            } else {
                self.events.push_back(Event::Data(lba));
            }
        }
        !self.events.is_empty() && self.output && self.source
    }
}

fn decode_msf(params: &[u8]) -> u32 {
    let bcd = |v: u8| u32::from(v >> 4) * 10 + u32::from(v & 15);
    (bcd(params[0]) * 60 + bcd(params[1])) * 75 + bcd(params[2]) - 150
}

impl CdHw for Drive {
    fn issue(&mut self, command: u8, params: &[u8]) -> bool {
        self.events.clear();
        match command {
            CMD_SETLOC => {
                self.target = decode_msf(params);
                self.setlocs.push(self.target);
                self.events.push_back(Event::Acknowledge);
            }
            CMD_SEEKL => {
                self.events.push_back(Event::Acknowledge);
                self.events.push_back(Event::Complete);
                self.next_lba = self.target;
            }
            CMD_SETMODE => self.events.push_back(Event::Acknowledge),
            CMD_READN => {
                self.reading = true;
                self.events.push_back(Event::Acknowledge);
            }
            CMD_PAUSE => {
                self.reading = false;
                self.events.push_back(Event::Acknowledge);
                self.events.push_back(Event::Complete);
            }
            other => panic!("the transport sent command {other:#04x}"),
        }
        self.output = true;
        true
    }

    fn interrupt_code(&mut self) -> u8 {
        self.events.front().map_or(0, |event| event.code())
    }

    fn error_response(&mut self) -> (u8, u8) {
        (0x03, 0x04)
    }

    fn discard_response(&mut self) {}

    fn acknowledge(&mut self, bits: u8) {
        if self
            .events
            .front()
            .is_some_and(|head| head.code() & bits != 0)
        {
            self.events.pop_front();
        }
    }

    fn silence_output(&mut self) {
        self.output = false;
    }

    fn drop_data_request(&mut self) {}

    unsafe fn pop_sector(&mut self, destination: *mut u32, store_words: usize) -> bool {
        let Some(Event::Data(lba)) = self.events.front().copied() else {
            panic!("pop_sector with {:?} pending", self.events.front());
        };
        for word in 0..store_words {
            // SAFETY: the caller guarantees `store_words` writable words.
            unsafe {
                destination
                    .add(word)
                    .write_volatile(disc_word(lba, word as u32))
            };
        }
        true
    }

    fn set_source_enabled(&mut self, enabled: bool) {
        self.source = enabled;
    }

    fn vblank_count(&mut self) -> u32 {
        self.vblank
    }
}

/// The real transport engine over the scripted drive. Every call into it lets
/// the drive raise up to `irqs_per_call` interrupts, as if that much time had
/// passed in the foreground.
struct Rig {
    engine: Engine<Drive>,
    irqs_per_call: u32,
    /// Foreground service calls seen, the rig's stand-in for elapsed time.
    services: u32,
    /// Service calls the drive stays silent for before it raises anything.
    silent_services: u32,
    /// Service calls per VBlank on the rig's display clock; 0 stops the clock.
    services_per_vblank: u32,
}

impl Rig {
    fn new(irqs_per_call: u32) -> Self {
        let mut engine = Engine::new(Drive::new(), Config::DEFAULT);
        engine.attach();
        Self {
            engine,
            irqs_per_call,
            services: 0,
            silent_services: 0,
            services_per_vblank: 0,
        }
    }

    fn pump(&mut self) {
        if self.services < self.silent_services {
            return;
        }
        for _ in 0..self.irqs_per_call {
            if !self.engine.hw_mut().raise() {
                break;
            }
            self.engine.on_interrupt();
        }
    }

    fn setlocs(&self) -> Vec<u32> {
        self.engine.hw().setlocs.clone()
    }
}

impl Transport for Rig {
    fn submit(&mut self, request: Request) -> Result<Ticket, SubmitError> {
        self.engine.submit(request)
    }

    fn state(&mut self, ticket: Ticket) -> RequestState {
        self.pump();
        self.engine.state(ticket)
    }

    fn cancel_all(&mut self) {
        self.engine.cancel_all();
    }

    fn is_idle(&mut self) -> bool {
        self.pump();
        self.engine.is_idle()
    }

    fn service(&mut self) {
        self.services = self.services.saturating_add(1);
        self.engine.service();
    }

    fn begin_transfer(&mut self) {}

    fn vblank_count(&mut self) -> u32 {
        self.services
            .checked_div(self.services_per_vblank)
            .unwrap_or(0)
    }
}

/// Whether the sector the run last handed out holds disc sector `lba`.
type TestStage = Stage<4, 2>;
type TestRun = Run<4, 2>;

fn sector_matches<const W: usize, const S: usize>(
    run: &Run<W, S>,
    stage: &Stage<W, S>,
    lba: u32,
) -> bool {
    let words = run.sector_ptr(stage).cast::<u32>();
    (0..SECTOR_WORDS)
        // SAFETY: the run handed out a whole sector of aligned words.
        .all(|word| unsafe { words.add(word).read_volatile() } == disc_word(lba, word as u32))
}

fn drain<const W: usize, const S: usize>(
    rig: &mut Rig,
    run: &mut Run<W, S>,
    stage: &mut Stage<W, S>,
    first_lba: u32,
    count: u32,
) {
    for index in 0..count {
        let mut spins = 0;
        loop {
            match run.next_sector(rig, stage) {
                Poll::Sector => break,
                Poll::Pending => {
                    spins += 1;
                    assert!(spins < 10_000, "sector {index} never arrived");
                }
                other => panic!("sector {index}: {other:?}"),
            }
        }
        assert!(
            sector_matches(run, stage, first_lba + index),
            "sector {index} (lba {}) holds the wrong words",
            first_lba + index
        );
    }
    assert_eq!(run.next_sector(rig, stage), Poll::Done);
}

fn a_run_delivers_every_sector_in_order<const W: usize, const S: usize>() {
    let mut rig = Rig::new(1);
    let mut stage = Stage::<W, S>::zeroed();
    let mut run = Run::<W, S>::ZERO;
    // 11 sectors is five full windows and a one-sector tail.
    run.begin(&mut rig, &mut stage, 1000, 11);
    drain(&mut rig, &mut run, &mut stage, 1000, 11);
    assert_eq!(
        rig.setlocs(),
        [1000],
        "one seek for the whole run ({W}x{S})"
    );
}

#[test]
fn a_run_delivers_every_sector_in_order_in_every_ring_shape() {
    a_run_delivers_every_sector_in_order::<4, 2>();
    a_run_delivers_every_sector_in_order::<2, 1>();
    a_run_delivers_every_sector_in_order::<3, 2>();
}

#[test]
fn a_foreground_that_falls_behind_costs_a_seek_not_a_sector() {
    let mut rig = Rig::new(40);
    let mut stage = TestStage::zeroed();
    let mut run = TestRun::ZERO;
    run.begin(&mut rig, &mut stage, 500, 40);
    // Let the ring fill and the drive stop before consuming anything.
    for _ in 0..10 {
        rig.service();
    }
    drain(&mut rig, &mut run, &mut stage, 500, 40);
    assert!(rig.setlocs().len() > 1, "the dry ring re-seeked");
}

#[test]
fn a_new_run_stops_the_previous_one() {
    let mut rig = Rig::new(2);
    let mut stage = TestStage::zeroed();
    let mut run = TestRun::ZERO;
    run.begin(&mut rig, &mut stage, 100, 30);
    for _ in 0..50 {
        let _ = run.next_sector(&mut rig, &mut stage);
    }
    run.begin(&mut rig, &mut stage, 700, 6);
    drain(&mut rig, &mut run, &mut stage, 700, 6);
}

fn an_audio_lease_aborts_the_read_and_the_run_resumes_where_it_stopped<
    const W: usize,
    const S: usize,
>() {
    let mut rig = Rig::new(1);
    let mut stage = Stage::<W, S>::zeroed();
    let mut run = Run::<W, S>::ZERO;
    run.begin(&mut rig, &mut stage, 2000, 24);
    let mut taken = 0u32;
    // Consume a few sectors, then take the lease in the middle of the read.
    while taken < 5 {
        if run.next_sector(&mut rig, &mut stage) == Poll::Sector {
            assert!(sector_matches(&run, &stage, 2000 + taken));
            taken += 1;
        }
    }
    let state = rig.engine.request_audio_lease();
    assert_ne!(state, LeaseState::None);
    // The read stops at the next sector and the lease is granted.
    let mut spins = 0;
    while rig.engine.lease_state() != LeaseState::Granted {
        rig.pump();
        if run.next_sector(&mut rig, &mut stage) == Poll::Sector {
            assert!(sector_matches(&run, &stage, 2000 + taken));
            taken += 1;
        }
        spins += 1;
        assert!(spins < 1_000, "lease never granted");
    }
    assert_eq!(rig.engine.owner(), Owner::Audio);
    let seeks_before = rig.setlocs().len();
    // While audio holds the drive nothing more lands.
    let mut landed_under_lease = 0;
    for _ in 0..20 {
        rig.pump();
        if run.next_sector(&mut rig, &mut stage) == Poll::Sector {
            // Sectors that landed before the abort may still be handed out,
            // but they must be the next ones in order.
            assert!(sector_matches(&run, &stage, 2000 + taken));
            taken += 1;
            landed_under_lease += 1;
        }
    }
    assert!(landed_under_lease < 24 - 5, "the read was not aborted");
    assert_eq!(rig.setlocs().len(), seeks_before, "no seek under the lease");

    assert!(rig.engine.release_audio_lease());
    // The rest arrives, in order, with no sector repeated or missing.
    let mut spins = 0;
    while taken < 24 {
        match run.next_sector(&mut rig, &mut stage) {
            Poll::Sector => {
                assert!(
                    sector_matches(&run, &stage, 2000 + taken),
                    "sector {taken} out of order"
                );
                taken += 1;
            }
            Poll::Pending => {
                spins += 1;
                assert!(spins < 10_000, "run did not resume");
                rig.service();
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(run.next_sector(&mut rig, &mut stage), Poll::Done);
    // The resumed read re-seeked to the sector after the last one that landed,
    // not to the start of the run.
    let resume_seek = rig.setlocs()[seeks_before];
    assert!(
        (2000 + 5..2000 + 24).contains(&resume_seek),
        "resume seek {resume_seek} is outside the unread part of the run"
    );
    assert!(rig.engine.stats().requests_cancelled >= 1);
}

#[test]
fn an_audio_lease_aborts_the_read_and_the_run_resumes_where_it_stopped_in_every_ring_shape() {
    an_audio_lease_aborts_the_read_and_the_run_resumes_where_it_stopped::<4, 2>();
    an_audio_lease_aborts_the_read_and_the_run_resumes_where_it_stopped::<2, 1>();
    an_audio_lease_aborts_the_read_and_the_run_resumes_where_it_stopped::<3, 2>();
}

fn a_drive_error_resumes_and_the_data_stays_exact<const W: usize, const S: usize>() {
    let mut rig = Rig::new(2);
    rig.engine.hw_mut().error_at.push(3000 + 7);
    let mut stage = Stage::<W, S>::zeroed();
    let mut run = Run::<W, S>::ZERO;
    run.begin(&mut rig, &mut stage, 3000, 16);
    drain(&mut rig, &mut run, &mut stage, 3000, 16);
    assert!(rig.engine.stats().resumed >= 1);
}

#[test]
fn a_drive_error_resumes_and_the_data_stays_exact_in_every_ring_shape() {
    a_drive_error_resumes_and_the_data_stays_exact::<4, 2>();
    a_drive_error_resumes_and_the_data_stays_exact::<2, 1>();
    a_drive_error_resumes_and_the_data_stays_exact::<3, 2>();
}

#[test]
fn an_unrecoverable_error_fails_the_run() {
    let mut rig = Rig::new(2);
    // More errors than a request may resume from.
    for _ in 0..10 {
        rig.engine.hw_mut().error_at.push(4000);
    }
    let mut stage = TestStage::zeroed();
    let mut run = TestRun::ZERO;
    run.begin(&mut rig, &mut stage, 4000, 8);
    let mut failure = None;
    for _ in 0..10_000 {
        match run.next_sector(&mut rig, &mut stage) {
            Poll::Failed(status) => {
                failure = Some(status);
                break;
            }
            Poll::Done => break,
            _ => rig.service(),
        }
    }
    assert_eq!(failure, Some(STATUS_CD_ERROR));
}

fn toc_entry(
    chunk: u16,
    sector_offset: u32,
    byte_size: u32,
    checksum: u32,
) -> LevelWorldPackEntryRecord {
    LevelWorldPackEntryRecord {
        room: psx_level::RoomIndex::new(chunk),
        sector_offset,
        sector_count: byte_size.div_ceil(2048),
        byte_size,
        checksum,
    }
}

fn fnv(bytes: impl Iterator<Item = u8>) -> u32 {
    bytes.fold(FNV_OFFSET, |sum, byte| {
        (sum ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// FNV of `byte_size` bytes of the disc starting at the sector `lba`.
fn disc_checksum(lba: u32, byte_size: usize) -> u32 {
    fnv((0..byte_size).map(|i| disc_byte(lba + (i / SECTOR_BYTES) as u32, i % SECTOR_BYTES)))
}

#[test]
fn read_chunk_blocking_verifies_and_lands_the_unpadded_bytes() {
    // A chunk that ends mid-sector.
    let size = 5 * SECTOR_BYTES + 700;
    let pack = 9000;
    let entry = toc_entry(3, 4, size as u32, disc_checksum(pack + 4, size));
    let mut rig = Rig::new(3);
    let mut cd = CdController::zeroed();
    let mut dst = std::vec![0u32; size.div_ceil(4)];
    let result = read_chunk_blocking_with(&mut rig, &mut cd, pack, &[entry], 3, &mut dst);
    assert_eq!(result.status, ROOM_CHUNK_STATUS_OK);
    assert_eq!(result.bytes, size);
    assert_eq!(result.sectors, 6);
    let bytes: &[u8] = unsafe { core::slice::from_raw_parts(dst.as_ptr().cast(), size) };
    for (i, byte) in bytes.iter().enumerate() {
        assert_eq!(
            *byte,
            disc_byte(pack + 4 + (i / SECTOR_BYTES) as u32, i % SECTOR_BYTES)
        );
    }
}

#[test]
fn a_blocking_read_waits_out_a_console_music_handoff() {
    // After audio the first sector takes about a second on a console. The
    // rig's drive stays silent for that long on its display clock, which is
    // more foreground spins than the old poll-count bound allowed, and the
    // read still lands: the bound is time, not spins.
    let spins_per_vblank = 30_000;
    let handoff = psx_engine::cd_drive::FIRST_SECTOR_AFTER_AUDIO_MS * 60 / 1000;
    let size = 2 * SECTOR_BYTES;
    let pack = 300;
    let entry = toc_entry(1, 0, size as u32, disc_checksum(pack, size));
    let mut rig = Rig::new(3);
    rig.services_per_vblank = spins_per_vblank;
    rig.silent_services = handoff * spins_per_vblank;
    assert!(rig.silent_services > 1_000_000);
    let mut cd = CdController::zeroed();
    let mut dst = std::vec![0u32; size / 4];
    let result = read_chunk_blocking_with(&mut rig, &mut cd, pack, &[entry], 1, &mut dst);
    assert_eq!(result.status, ROOM_CHUNK_STATUS_OK);
    assert!(rig.vblank_count() >= handoff);
}

#[test]
fn a_blocking_wait_on_a_dead_drive_gives_up_on_the_display_clock() {
    let mut rig = Rig::new(3);
    rig.services_per_vblank = 1_000;
    rig.silent_services = u32::MAX;
    let mut cd = CdController::zeroed();
    cd.begin_run(&mut rig, 100, 1);
    assert_eq!(
        cd.wait_sector(&mut rig),
        Err(crate::cd_stream::STATUS_DATA_TIMEOUT)
    );
    let waited = rig.vblank_count();
    let deadline = crate::cd_stream::BLOCKING_READ_DEADLINE_VBLANKS;
    assert!(
        waited > deadline && waited < deadline + 10,
        "gave up after {waited} vblanks"
    );
}

#[test]
fn read_chunk_blocking_reports_a_checksum_mismatch() {
    let size = 2 * SECTOR_BYTES;
    let entry = toc_entry(1, 0, size as u32, 0xDEAD_BEEF);
    let mut rig = Rig::new(3);
    let mut cd = CdController::zeroed();
    let mut dst = std::vec![0u32; size / 4];
    let result = read_chunk_blocking_with(&mut rig, &mut cd, 100, &[entry], 1, &mut dst);
    assert_eq!(result.status, crate::cd_stream::STATUS_CHECKSUM_MISMATCH);
}

#[test]
fn read_chunk_banded_feeds_the_consumer_every_byte_in_order() {
    let size = 7 * SECTOR_BYTES + 100;
    let pack = 12_000;
    let entry = toc_entry(9, 2, size as u32, disc_checksum(pack + 2, size));
    let mut rig = Rig::new(3);
    let mut cd = CdController::zeroed();
    let mut window = std::vec![0u32; 4 * SECTOR_BYTES / 4];
    let mut seen = 0usize;
    let result = read_chunk_banded_with(
        &mut rig,
        &mut cd,
        pack,
        &[entry],
        9,
        &mut window,
        |offset, bytes| {
            assert_eq!(offset, seen);
            for (i, byte) in bytes.iter().enumerate() {
                let at = offset + i;
                assert_eq!(
                    *byte,
                    disc_byte(pack + 2 + (at / SECTOR_BYTES) as u32, at % SECTOR_BYTES)
                );
            }
            // Take a whole 3000-byte row at a time, like a texture consumer.
            let took = bytes.len() / 3000 * 3000;
            seen += took;
            took
        },
    );
    assert_eq!(result.status, ROOM_CHUNK_STATUS_OK);
    assert_eq!(result.bytes, size);
}

#[test]
fn contiguous_plans_share_one_seek() {
    let pack = 15_000;
    let sizes = [3 * SECTOR_BYTES, 2 * SECTOR_BYTES + 5, SECTOR_BYTES];
    let offsets = [0u32, 4, 9];
    let mut plans = [UiChunkPlan::EMPTY; 3];
    for i in 0..3 {
        plans[i] = UiChunkPlan {
            sector_offset: offsets[i],
            sector_count: (sizes[i] as u32).div_ceil(2048),
            byte_size: sizes[i],
            checksum: disc_checksum(pack + offsets[i], sizes[i]),
            cache_word_start: i * 4096,
        };
    }
    let mut rig = Rig::new(1);
    let mut cd = CdController::zeroed();
    let mut cache = std::vec![0u32; 3 * 4096];
    let mut statuses = [99u32; 3];
    read_chunks_contiguous_with(&mut rig, &mut cd, pack, &plans, &mut cache, &mut statuses);
    assert_eq!(statuses, [ROOM_CHUNK_STATUS_OK; 3]);
    assert_eq!(rig.setlocs(), [pack], "one seek for all three chunks");
}

/// A destination that records what lands in each slot.
struct Slots {
    rows: [Vec<u8>; 3],
}

impl WorldChunkDestination for Slots {
    fn slot_capacity_bytes(&self, slot: usize) -> usize {
        self.rows[slot].len()
    }

    fn write_chunk_bytes(&mut self, slot: usize, offset: usize, bytes: &[u8]) -> bool {
        match self.rows[slot].get_mut(offset..offset + bytes.len()) {
            Some(row) => {
                row.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }
}

#[test]
fn the_job_loads_chunks_in_disc_order_and_verifies_each() {
    let pack = 20_000;
    // Chunk 1 and 2 are adjacent (one group); chunk 5 sits far away.
    let sizes = [2 * SECTOR_BYTES + 10, 3 * SECTOR_BYTES, SECTOR_BYTES + 99];
    let offsets = [10u32, 13, 400];
    let ids = [1u16, 2, 5];
    let toc: Vec<_> = (0..3)
        .map(|i| {
            toc_entry(
                ids[i],
                offsets[i],
                sizes[i] as u32,
                disc_checksum(pack + offsets[i], sizes[i]),
            )
        })
        .collect();
    let mut slots = Slots {
        rows: [
            std::vec![0; sizes[0]],
            std::vec![0; sizes[1]],
            std::vec![0; sizes[2]],
        ],
    };
    let capacities = [sizes[0], sizes[1], sizes[2]];
    let mut job = WorldRoomSlotsReadJob::<3>::new();
    job.start(pack, &toc, &ids, &[0, 1, 2], &capacities);
    let mut rig = Rig::new(1);
    let mut cd = CdController::zeroed();
    job.set_wait_for_sectors(true);
    let mut pumps = 0;
    while !job.is_done() {
        job.poll_into_with(&mut rig, &mut cd, &mut slots, 4);
        pumps += 1;
        assert!(pumps < 1_000, "job never finished");
    }
    assert_eq!(job.statuses(), &[ROOM_CHUNK_STATUS_OK; 3]);
    assert_eq!(job.completed_entries(), [true; 3]);
    assert_eq!(
        rig.setlocs(),
        [pack + 10, pack + 400],
        "one seek per disc group"
    );
    for (i, row) in slots.rows.iter().enumerate() {
        for (at, byte) in row.iter().enumerate() {
            assert_eq!(
                *byte,
                disc_byte(
                    pack + offsets[i] + (at / SECTOR_BYTES) as u32,
                    at % SECTOR_BYTES
                )
            );
        }
    }
}

#[test]
fn music_never_leaves_a_pending_lease_behind_a_busy_transport() {
    // Music asks for the drive while a read is stopping. If the request were
    // left pending the transport would grant it a moment later, and the next
    // read, queued in the belief that the drive is free, would wait forever on
    // a lease nobody holds.
    let mut rig = Rig::new(1);
    let mut stage = TestStage::zeroed();
    let mut run = TestRun::ZERO;
    run.begin(&mut rig, &mut stage, 6000, 3);
    drain(&mut rig, &mut run, &mut stage, 6000, 3);
    // The last sector has landed but the drive is still pausing.
    assert!(!rig.engine.is_idle());
    assert!(!rig.engine.try_audio_lease());
    assert_eq!(rig.engine.lease_state(), LeaseState::None);

    // The next read starts as soon as the drive stops.
    run.begin(&mut rig, &mut stage, 7000, 3);
    drain(&mut rig, &mut run, &mut stage, 7000, 3);
    assert_eq!(rig.engine.owner(), Owner::Data);
}

#[test]
fn music_takes_an_idle_drive_and_gives_it_back_to_data() {
    let mut rig = Rig::new(1);
    let mut stage = TestStage::zeroed();
    let mut run = TestRun::ZERO;
    assert!(rig.engine.try_audio_lease());
    assert_eq!(rig.engine.owner(), Owner::Audio);
    // A read queued under the lease waits for it.
    run.begin(&mut rig, &mut stage, 8000, 4);
    for _ in 0..50 {
        rig.service();
        assert_eq!(run.next_sector(&mut rig, &mut stage), Poll::Pending);
    }
    assert!(rig.engine.release_audio_lease());
    drain(&mut rig, &mut run, &mut stage, 8000, 4);
}
