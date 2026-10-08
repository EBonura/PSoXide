//! The transport state machine against a scripted drive.

use crate::fake::{sector_word, FakeDrive};
use crate::*;
use psx_hw::cd::{CMD_PAUSE, CMD_READN, CMD_SEEKL, CMD_SETLOC, CMD_SETMODE, MODE_DOUBLE_SPEED};
use std::boxed::Box;
use std::vec;
use std::vec::Vec;

const CANARY: u32 = 0xDEAD_BEEF;
const BRACKET: [u8; 5] = [CMD_SETLOC, CMD_SEEKL, CMD_SETMODE, CMD_READN, CMD_PAUSE];

// ------------------------------------------------------------------ helpers

/// A destination the test owns, with a guard sector after the request's
/// extent so a write past the end shows up.
struct Buf {
    ptr: *mut u32,
    sectors: u32,
}

impl Buf {
    fn new(sectors: u32) -> Buf {
        let words = (sectors as usize + 1) * 512;
        let slice: *mut [u32] = Box::into_raw(vec![CANARY; words].into_boxed_slice());
        Buf {
            ptr: slice.cast::<u32>(),
            sectors,
        }
    }

    fn len(&self) -> usize {
        (self.sectors as usize + 1) * 512
    }

    fn word(&self, index: usize) -> u32 {
        assert!(index < self.len());
        // SAFETY: `index` is inside the allocation made in `new`, which lives
        // until `drop`; the transport only writes it through the pointer.
        unsafe { self.ptr.add(index).read_volatile() }
    }

    /// Words `0..sectors * 512` are sectors `first_lba..` of the disc.
    fn assert_sectors(&self, first_lba: u32, sectors: u32) {
        for sector in 0..sectors {
            for word in 0..512u32 {
                assert_eq!(
                    self.word((sector * 512 + word) as usize),
                    sector_word(first_lba + sector, word),
                    "sector {sector} word {word}"
                );
            }
        }
    }

    /// Everything from sector `from` on (guard included) is still the canary.
    fn assert_untouched_from(&self, from: u32) {
        for word in (from as usize * 512)..self.len() {
            assert_eq!(self.word(word), CANARY, "word {word} was written");
        }
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        let slice = core::ptr::slice_from_raw_parts_mut(self.ptr, self.len());
        // SAFETY: the pointer and length are the boxed slice `new` leaked
        // with `Box::into_raw`; nothing uses it after this.
        drop(unsafe { Box::from_raw(slice) });
    }
}

fn request(lba: u32, sectors: u32, buf: &Buf) -> Request {
    // SAFETY: `buf` owns `sectors * 2048` writable bytes plus a guard, leaked
    // for the whole test, and the tests only read it through volatile loads.
    unsafe { Request::new_raw(lba, sectors, buf.ptr) }
}

fn engine() -> Engine<FakeDrive> {
    engine_with(Config::DEFAULT)
}

fn engine_with(config: Config) -> Engine<FakeDrive> {
    let mut engine = Engine::new(FakeDrive::new(), config);
    engine.attach();
    engine
}

/// Deliver interrupts until the drive has nothing more to say.
fn run(engine: &mut Engine<FakeDrive>) -> u32 {
    let mut delivered = 0;
    while engine.hw_mut().raise() {
        engine.on_interrupt();
        delivered += 1;
        assert!(delivered < 100_000, "interrupt storm");
    }
    delivered
}

/// Deliver exactly `count` interrupts.
fn step(engine: &mut Engine<FakeDrive>, count: u32) {
    for n in 0..count {
        assert!(engine.hw_mut().raise(), "no interrupt for step {n}");
        engine.on_interrupt();
    }
}

/// Deliver interrupts until `ticket` has landed `received` sectors.
fn step_until_received(engine: &mut Engine<FakeDrive>, ticket: Ticket, received: u32) {
    for _ in 0..10_000 {
        if matches!(engine.state(ticket), RequestState::Active { received: r } if r >= received) {
            return;
        }
        assert!(engine.hw_mut().raise(), "the drive went quiet");
        engine.on_interrupt();
    }
    panic!("never reached {received} sectors");
}

/// Deliver interrupts until `ticket` has finished.
fn step_until_finished(engine: &mut Engine<FakeDrive>, ticket: Ticket) {
    for _ in 0..10_000 {
        if matches!(engine.state(ticket), RequestState::Finished(_)) {
            return;
        }
        assert!(engine.hw_mut().raise(), "the drive went quiet");
        engine.on_interrupt();
    }
    panic!("never finished");
}

fn finished(engine: &mut Engine<FakeDrive>, ticket: Ticket) -> Completion {
    match engine.state(ticket) {
        RequestState::Finished(done) => done,
        other => panic!("ticket {ticket:?} is {other:?}"),
    }
}

fn submit(engine: &mut Engine<FakeDrive>, request: Request) -> Ticket {
    engine.submit(request).expect("accepted")
}

// ------------------------------------------------------------------ reading

#[test]
fn one_request_lands_in_its_destination() {
    let mut engine = engine();
    let buf = Buf::new(3);
    let ticket = submit(&mut engine, request(100, 3, &buf));
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    assert_eq!(done.outcome, Outcome::Done);
    assert_eq!(done.received, 3);
    buf.assert_sectors(100, 3);
    buf.assert_untouched_from(3);
    assert_eq!(engine.hw().popped, [100, 101, 102]);
    assert_eq!(engine.phase(), Phase::Done);
    assert!(engine.is_idle());
    assert!(!engine.hw().output_enabled, "the controller is left quiet");
    assert_eq!(engine.stats().sectors, 3);
    assert_eq!(engine.stats().requests_done, 1);
}

#[test]
fn a_transfer_is_seek_first_at_double_speed() {
    let mut engine = engine();
    let buf = Buf::new(2);
    submit(&mut engine, request(100, 2, &buf));
    run(&mut engine);
    let drive = engine.hw();
    assert_eq!(drive.commands(), BRACKET);
    assert_eq!(drive.log[2].params, [MODE_DOUBLE_SPEED]);
    assert_eq!(drive.mode, MODE_DOUBLE_SPEED);
    assert_eq!(drive.setloc_targets(), [100]);
}

#[test]
fn setloc_names_the_sector_in_absolute_minutes_seconds_frames() {
    let mut engine = engine();
    let lbas = [0, 1, 74, 75, 4499, 4500, 333_000];
    for lba in lbas {
        let buf = Buf::new(1);
        submit(&mut engine, request(lba, 1, &buf));
        run(&mut engine);
        buf.assert_sectors(lba, 1);
    }
    assert_eq!(engine.hw().setloc_targets(), lbas);
}

#[test]
fn the_request_is_done_before_the_drive_has_stopped() {
    let mut engine = engine();
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(10, 2, &buf));
    step_until_received(&mut engine, ticket, 1);
    assert_eq!(engine.state(ticket), RequestState::Active { received: 1 });
    step(&mut engine, 1);
    // The last sector is in and Pause is on its way.
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    assert!(!engine.is_idle(), "Pause has not completed yet");
    run(&mut engine);
    assert!(engine.is_idle());
}

#[test]
fn sectors_in_flight_when_pause_lands_are_dropped() {
    let mut engine = engine();
    engine.hw_mut().sectors_in_flight_at_pause = 2;
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(100, 2, &buf));
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).received, 2);
    buf.assert_sectors(100, 2);
    buf.assert_untouched_from(2);
    assert_eq!(engine.hw().popped, [100, 101, 102, 103]);
    let stats = engine.stats();
    assert_eq!((stats.sectors, stats.discarded_sectors), (2, 2));
}

#[test]
fn a_partial_last_sector_stores_only_the_chunk() {
    let mut engine = engine();
    let buf = Buf::new(3);
    let bytes = 2 * 2048 + 101; // 26 words (rounded up) of the third sector
    let ticket = submit(&mut engine, request(7, 3, &buf).with_byte_len(bytes));
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).received, 3);
    buf.assert_sectors(7, 2);
    for word in 0..26 {
        assert_eq!(buf.word(1024 + word), sector_word(9, word as u32));
    }
    for word in (1024 + 26)..(4 * 512) {
        assert_eq!(buf.word(word), CANARY, "word {word}");
    }
}

#[test]
fn the_request_helpers() {
    let buf = Buf::new(4);
    let whole = request(50, 4, &buf);
    assert_eq!((whole.lba(), whole.sectors(), whole.end_lba()), (50, 4, 54));
    assert_eq!(whole.priority(), Priority::NORMAL);
    // Out-of-range byte lengths are ignored.
    for bytes in [0, 3 * 2048, 4 * 2048 + 1] {
        assert_eq!(whole.with_byte_len(bytes), whole);
    }
    assert_ne!(whole.with_byte_len(3 * 2048 + 1), whole);
    // SAFETY: both counts are below the request's sectors.
    let rest = unsafe { whole.remaining_after(3) }.expect("one sector left");
    assert_eq!((rest.lba(), rest.sectors()), (53, 1));
    assert_eq!(rest.destination, whole.destination.wrapping_add(3 * 512));
    // SAFETY: as above.
    assert_eq!(unsafe { whole.remaining_after(4) }, None);
    // SAFETY: as above.
    let partial = unsafe { whole.with_byte_len(3 * 2048 + 10).remaining_after(3) }.expect("rest");
    assert_eq!(partial.store_words(0), 3);
    assert_eq!(whole.store_words(1), 512);
}

// ---------------------------------------------------------------- chaining

#[test]
fn contiguous_requests_chain_without_a_seek() {
    let mut engine = engine();
    let (a, b) = (Buf::new(2), Buf::new(3));
    let first = submit(&mut engine, request(10, 2, &a));
    let second = submit(&mut engine, request(12, 3, &b));
    assert_eq!(engine.state(second), RequestState::Queued);
    run(&mut engine);
    assert_eq!(engine.hw().commands(), BRACKET, "one bracket serves both");
    assert_eq!(finished(&mut engine, first).outcome, Outcome::Done);
    assert_eq!(finished(&mut engine, second).outcome, Outcome::Done);
    a.assert_sectors(10, 2);
    b.assert_sectors(12, 3);
    a.assert_untouched_from(2);
    b.assert_untouched_from(3);
    assert_eq!(engine.stats().chained, 1);
    assert_eq!(engine.stats().discarded_sectors, 0);
}

#[test]
fn a_chain_can_be_longer_and_end_on_a_partial_sector() {
    let mut engine = engine();
    let (a, b, c) = (Buf::new(2), Buf::new(1), Buf::new(2));
    submit(&mut engine, request(10, 2, &a));
    submit(&mut engine, request(12, 1, &b));
    let last = submit(&mut engine, request(13, 2, &c).with_byte_len(2048 + 100));
    run(&mut engine);
    assert_eq!(engine.hw().commands(), BRACKET);
    assert_eq!(finished(&mut engine, last).received, 2);
    a.assert_sectors(10, 2);
    b.assert_sectors(12, 1);
    c.assert_sectors(13, 1);
    for word in 512 + 25..3 * 512 {
        assert_eq!(c.word(word), CANARY);
    }
    assert_eq!(engine.stats().chained, 2);
}

#[test]
fn a_request_that_does_not_continue_gets_its_own_seek() {
    let mut engine = engine();
    let (a, b) = (Buf::new(2), Buf::new(2));
    submit(&mut engine, request(10, 2, &a));
    let second = submit(&mut engine, request(50, 2, &b));
    // Nothing but interrupts between the two brackets: the handler starts
    // the second request itself.
    run(&mut engine);
    let mut both = BRACKET.to_vec();
    both.extend(BRACKET);
    assert_eq!(engine.hw().commands(), both);
    assert_eq!(engine.hw().setloc_targets(), [10, 50]);
    assert_eq!(finished(&mut engine, second).outcome, Outcome::Done);
    b.assert_sectors(50, 2);
    assert_eq!(engine.stats().chained, 0);
}

#[test]
fn chaining_is_skipped_when_the_continuation_arrives_too_late() {
    let mut engine = engine();
    let (a, b) = (Buf::new(1), Buf::new(1));
    let first = submit(&mut engine, request(10, 1, &a));
    step_until_finished(&mut engine, first); // last sector landed, Pause issued
    let second = submit(&mut engine, request(11, 1, &b));
    assert_eq!(engine.state(second), RequestState::Queued);
    run(&mut engine);
    assert_eq!(engine.stats().chained, 0);
    assert_eq!(engine.hw().setloc_targets(), [10, 11]);
    b.assert_sectors(11, 1);
}

// ---------------------------------------------------------------- priority

/// A long request to keep the drive busy while the queue fills.
fn busy_engine() -> (Engine<FakeDrive>, Ticket, Buf) {
    let mut engine = engine();
    let buf = Buf::new(30);
    let ticket = submit(&mut engine, request(0, 30, &buf));
    step_until_received(&mut engine, ticket, 2);
    (engine, ticket, buf)
}

#[test]
fn more_urgent_requests_go_first() {
    let (mut engine, _busy, _buf) = busy_engine();
    let (low, normal, urgent) = (Buf::new(1), Buf::new(1), Buf::new(1));
    submit(
        &mut engine,
        request(100, 1, &low).with_priority(Priority::BACKGROUND),
    );
    submit(&mut engine, request(200, 1, &normal));
    submit(
        &mut engine,
        request(300, 1, &urgent).with_priority(Priority::URGENT),
    );
    run(&mut engine);
    assert_eq!(engine.hw().setloc_targets(), [0, 300, 200, 100]);
    urgent.assert_sectors(300, 1);
    normal.assert_sectors(200, 1);
    low.assert_sectors(100, 1);
}

#[test]
fn equal_priorities_are_served_in_submission_order() {
    let (mut engine, _busy, _buf) = busy_engine();
    let bufs: Vec<Buf> = (0..3).map(|_| Buf::new(1)).collect();
    for (i, buf) in bufs.iter().enumerate() {
        submit(&mut engine, request(500 - 100 * i as u32, 1, buf));
    }
    run(&mut engine);
    assert_eq!(engine.hw().setloc_targets(), [0, 500, 400, 300]);
}

#[test]
fn among_equals_the_one_that_continues_goes_first() {
    let mut engine = engine();
    let (a, far, near) = (Buf::new(2), Buf::new(1), Buf::new(1));
    submit(&mut engine, request(10, 2, &a));
    submit(&mut engine, request(100, 1, &far));
    submit(&mut engine, request(12, 1, &near));
    run(&mut engine);
    assert_eq!(engine.hw().setloc_targets(), [10, 100]);
    assert_eq!(engine.stats().chained, 1);
    near.assert_sectors(12, 1);
    far.assert_sectors(100, 1);
}

#[test]
fn urgency_beats_continuing() {
    let mut engine = engine();
    let (a, near, urgent) = (Buf::new(2), Buf::new(1), Buf::new(1));
    submit(&mut engine, request(10, 2, &a));
    submit(
        &mut engine,
        request(12, 1, &near).with_priority(Priority::BACKGROUND),
    );
    submit(
        &mut engine,
        request(100, 1, &urgent).with_priority(Priority::URGENT),
    );
    run(&mut engine);
    assert_eq!(engine.hw().setloc_targets(), [10, 100, 12]);
    assert_eq!(engine.stats().chained, 0);
}

#[test]
fn the_queue_is_bounded_and_frees_up() {
    let (mut engine, busy, _buf) = busy_engine();
    let bufs: Vec<Buf> = (0..QUEUE_DEPTH + 1).map(|_| Buf::new(1)).collect();
    for buf in &bufs[..QUEUE_DEPTH] {
        submit(&mut engine, request(1000, 1, buf));
    }
    assert_eq!(
        engine.submit(request(1000, 1, &bufs[QUEUE_DEPTH])),
        Err(SubmitError::QueueFull)
    );
    assert_eq!(engine.stats().rejected, 1);
    assert!(engine.cancel(busy));
    run(&mut engine);
    assert!(engine.submit(request(1000, 1, &bufs[QUEUE_DEPTH])).is_ok());
}

#[test]
fn bad_requests_are_refused() {
    let mut engine = engine();
    let buf = Buf::new(1);
    // SAFETY: nothing is read or written; submit refuses these before
    // starting anything.
    let null = unsafe { Request::new_raw(0, 1, core::ptr::null_mut()) };
    assert_eq!(engine.submit(null), Err(SubmitError::NullDestination));
    // SAFETY: as above.
    let odd = unsafe { Request::new_raw(0, 1, buf.ptr.cast::<u8>().wrapping_add(2).cast()) };
    assert_eq!(engine.submit(odd), Err(SubmitError::MisalignedDestination));
    assert_eq!(engine.submit(request(0, 0, &buf)), Err(SubmitError::Empty));
    assert!(engine.hw().log.is_empty());
}

// ------------------------------------------------------------------- abort

#[test]
fn cancel_stops_at_the_next_sector() {
    let mut engine = engine();
    let buf = Buf::new(10);
    let ticket = submit(&mut engine, request(100, 10, &buf));
    step_until_received(&mut engine, ticket, 3);
    assert!(engine.cancel(ticket));
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    assert_eq!(done.outcome, Outcome::Cancelled);
    assert_eq!(done.received, 3);
    buf.assert_sectors(100, 3);
    buf.assert_untouched_from(3);
    assert_eq!(engine.hw().commands(), BRACKET);
    assert_eq!(engine.stats().discarded_sectors, 1);
    assert_eq!(engine.stats().requests_cancelled, 1);
    assert!(engine.is_idle());
}

#[test]
fn cancel_works_in_every_phase_before_the_first_sector() {
    for delivered in 0..=5 {
        let mut engine = engine();
        let buf = Buf::new(4);
        let ticket = submit(&mut engine, request(100, 4, &buf));
        step(&mut engine, delivered);
        assert!(engine.cancel(ticket), "{delivered} interrupts in");
        run(&mut engine);
        let done = finished(&mut engine, ticket);
        assert_eq!(
            done.outcome,
            Outcome::Cancelled,
            "{delivered} interrupts in"
        );
        assert_eq!(done.received, 0);
        buf.assert_untouched_from(0);
        assert_eq!(*engine.hw().commands().last().unwrap(), CMD_PAUSE);
        assert_eq!(engine.phase(), Phase::Done);
    }
}

#[test]
fn cancelling_a_finished_request_does_nothing() {
    let mut engine = engine();
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(5, 1, &buf));
    run(&mut engine);
    assert!(!engine.cancel(ticket));
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
}

#[test]
fn cancelling_a_queued_request_is_immediate_and_it_never_reads() {
    let (mut engine, _busy, _buf) = busy_engine();
    let waiting = Buf::new(1);
    let ticket = submit(&mut engine, request(900, 1, &waiting));
    assert!(engine.cancel(ticket));
    let done = finished(&mut engine, ticket);
    assert_eq!((done.outcome, done.received), (Outcome::Cancelled, 0));
    engine.cancel_all();
    run(&mut engine);
    assert_eq!(engine.hw().setloc_targets(), [0]);
    waiting.assert_untouched_from(0);
}

#[test]
fn an_aborted_request_can_be_resumed_from_what_landed() {
    let mut engine = engine();
    let buf = Buf::new(8);
    let original = request(200, 8, &buf);
    let ticket = submit(&mut engine, original);
    step_until_received(&mut engine, ticket, 3);
    engine.cancel(ticket);
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    assert_eq!(done.outcome, Outcome::Cancelled);
    // SAFETY: `received` is what the transport reported for this request.
    let rest = unsafe { original.remaining_after(done.received) }.expect("sectors left");
    let resumed = submit(&mut engine, rest);
    run(&mut engine);
    assert_eq!(finished(&mut engine, resumed).outcome, Outcome::Done);
    buf.assert_sectors(200, 8);
    assert_eq!(engine.hw().setloc_targets(), [200, 200 + done.received]);
}

#[test]
fn cancel_all_empties_everything() {
    let (mut engine, busy, _buf) = busy_engine();
    let bufs: Vec<Buf> = (0..3).map(|_| Buf::new(1)).collect();
    let tickets: Vec<Ticket> = bufs
        .iter()
        .map(|b| submit(&mut engine, request(700, 1, b)))
        .collect();
    engine.cancel_all();
    run(&mut engine);
    for ticket in tickets.into_iter().chain([busy]) {
        assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Cancelled);
    }
    assert_eq!(engine.queued_count(), 0);
    assert!(engine.is_idle());
}

// ------------------------------------------------------------------ errors

#[test]
fn a_drive_error_ends_the_request_and_the_next_start_recovers() {
    let mut engine = engine();
    engine.hw_mut().error_at.push(105);
    let buf = Buf::new(10);
    let ticket = submit(&mut engine, request(100, 10, &buf));
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    let Outcome::Failed(failure) = done.outcome else {
        panic!("{done:?}");
    };
    assert_eq!(failure.kind(), FailureKind::Drive { status: 3, code: 4 });
    assert_eq!(done.received, 5);
    buf.assert_sectors(100, 5);
    buf.assert_untouched_from(5);
    assert_eq!(engine.phase(), Phase::Failed);
    assert_eq!(engine.stats().error, failure.code());
    assert_eq!(engine.stats().requests_failed, 1);

    // The drive may still be busy: the next transfer pauses it first.
    let other = Buf::new(2);
    let next = submit(&mut engine, request(300, 2, &other));
    run(&mut engine);
    assert_eq!(finished(&mut engine, next).outcome, Outcome::Done);
    other.assert_sectors(300, 2);
    let log = engine.hw().commands();
    let tail = &log[BRACKET.len() - 1..];
    assert_eq!(
        tail,
        [
            CMD_PAUSE,
            CMD_SETLOC,
            CMD_SEEKL,
            CMD_SETMODE,
            CMD_READN,
            CMD_PAUSE
        ]
    );
    assert_eq!(engine.stats().error, 0, "a start clears the code");
}

#[test]
fn a_request_with_resumes_continues_after_an_error() {
    let mut engine = engine();
    engine.hw_mut().error_at.push(105);
    let buf = Buf::new(10);
    let ticket = submit(&mut engine, request(100, 10, &buf).with_resumes(1));
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    assert_eq!((done.outcome, done.received), (Outcome::Done, 10));
    buf.assert_sectors(100, 10);
    assert_eq!(engine.hw().setloc_targets(), [100, 105]);
    assert_eq!(engine.stats().resumed, 1);
    assert_eq!(engine.stats().requests_failed, 0);
    let log = engine.hw().commands();
    assert_eq!(
        &log[BRACKET.len() - 1..BRACKET.len() + 1],
        [CMD_PAUSE, CMD_SETLOC],
        "the recovery pause comes first, then the seek back"
    );
}

#[test]
fn resumes_run_out() {
    let mut engine = engine();
    engine.hw_mut().error_at.extend([105, 107]);
    let buf = Buf::new(10);
    let ticket = submit(&mut engine, request(100, 10, &buf).with_resumes(1));
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    assert!(matches!(done.outcome, Outcome::Failed(_)));
    assert_eq!(done.received, 7);
    buf.assert_sectors(100, 7);
    buf.assert_untouched_from(7);
    assert_eq!(engine.stats().resumed, 1);
}

#[test]
fn a_resume_goes_ahead_of_other_requests_of_its_priority() {
    let mut engine = engine();
    engine.hw_mut().error_at.push(105);
    let (a, b) = (Buf::new(10), Buf::new(1));
    let first = submit(&mut engine, request(100, 10, &a).with_resumes(1));
    submit(&mut engine, request(900, 1, &b));
    run(&mut engine);
    assert_eq!(finished(&mut engine, first).outcome, Outcome::Done);
    assert_eq!(engine.hw().setloc_targets(), [100, 105, 900]);
}

#[test]
fn resuming_works_even_with_a_full_queue() {
    let mut engine = engine();
    engine.hw_mut().error_at.push(103);
    let a = Buf::new(6);
    let first = submit(&mut engine, request(100, 6, &a).with_resumes(1));
    let others: Vec<Buf> = (0..QUEUE_DEPTH).map(|_| Buf::new(1)).collect();
    for b in &others {
        submit(&mut engine, request(800, 1, b));
    }
    run(&mut engine);
    assert_eq!(finished(&mut engine, first).outcome, Outcome::Done);
    a.assert_sectors(100, 6);
}

#[test]
fn a_lost_pause_acknowledge_is_caught_by_the_watchdog() {
    let mut engine = engine();
    engine.hw_mut().swallow_pause = true;
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(10, 1, &buf));
    run(&mut engine);
    // The sector landed; the drive never answers the Pause.
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    assert_eq!(engine.phase(), Phase::Stopping);
    engine.hw_mut().vblank = 600;
    engine.service();
    assert_eq!(engine.phase(), Phase::Stopping, "not yet past the limit");
    engine.hw_mut().vblank = 601;
    engine.service();
    assert_eq!(engine.phase(), Phase::Failed);
    assert_eq!(Failure(engine.stats().error).kind(), FailureKind::Watchdog);
    // The next transfer begins with a recovery pause.
    engine.hw_mut().swallow_pause = false;
    let next = Buf::new(1);
    let ticket = submit(&mut engine, request(20, 1, &next));
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    let log = engine.hw().commands();
    assert_eq!(
        &log[BRACKET.len()..BRACKET.len() + 2],
        [CMD_PAUSE, CMD_SETLOC]
    );
}

#[test]
fn the_watchdog_ends_a_request_the_drive_never_answers() {
    let mut engine = engine();
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(10, 1, &buf));
    engine.hw_mut().vblank = 700;
    let state = engine.state(ticket);
    let RequestState::Finished(done) = state else {
        panic!("{state:?}");
    };
    let Outcome::Failed(failure) = done.outcome else {
        panic!("{done:?}");
    };
    assert_eq!(failure.kind(), FailureKind::Watchdog);
    assert_eq!(failure.code() & 0xff, Phase::SettingTarget as u32);
}

#[test]
fn the_watchdog_waits_for_a_stalled_transfer_not_a_flowing_one() {
    let mut engine = engine();
    let buf = Buf::new(20);
    let ticket = submit(&mut engine, request(0, 20, &buf));
    while engine.hw_mut().raise() {
        // 500 VBlanks between interrupts is under the 600 limit, and each
        // interrupt restarts the count.
        engine.hw_mut().vblank += 500;
        engine.on_interrupt();
        engine.service();
    }
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
}

#[test]
fn a_custom_timeout_applies() {
    let config = Config {
        timeout_vblanks: 10,
        ..Config::DEFAULT
    };
    let mut engine = engine_with(config);
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(0, 1, &buf));
    engine.hw_mut().vblank = 11;
    assert!(matches!(
        engine.state(ticket),
        RequestState::Finished(Completion {
            outcome: Outcome::Failed(_),
            ..
        })
    ));
}

#[test]
fn an_interrupt_the_phase_has_no_use_for_fails_the_transfer() {
    let mut engine = engine();
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(0, 1, &buf));
    step(&mut engine, 1); // Setloc acknowledged, SeekL sent
    engine.hw_mut().inject_complete();
    engine.on_interrupt();
    let done = finished(&mut engine, ticket);
    let Outcome::Failed(failure) = done.outcome else {
        panic!("{done:?}");
    };
    assert_eq!(failure.kind(), FailureKind::UnexpectedInterrupt { flag: 2 });
    assert_eq!(engine.phase(), Phase::Failed);
}

#[test]
fn a_refused_command_fails_the_request() {
    let mut engine = engine();
    engine.hw_mut().refuse_command = Some(CMD_SETLOC);
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(0, 1, &buf));
    let done = finished(&mut engine, ticket);
    let Outcome::Failed(failure) = done.outcome else {
        panic!("{done:?}");
    };
    assert_eq!(
        failure.kind(),
        FailureKind::CommandRefused {
            command: CMD_SETLOC
        }
    );
    assert!(!engine.hw().output_enabled, "the controller is silenced");
    // And the engine recovers once the controller does.
    engine.hw_mut().refuse_command = None;
    let ticket = submit(&mut engine, request(0, 1, &buf));
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
}

#[test]
fn a_data_fifo_that_never_fills_fails_the_transfer() {
    let mut engine = engine();
    engine.hw_mut().data_never_ready = true;
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(0, 2, &buf));
    run(&mut engine);
    let done = finished(&mut engine, ticket);
    let Outcome::Failed(failure) = done.outcome else {
        panic!("{done:?}");
    };
    assert_eq!(failure.kind(), FailureKind::DataNotReady);
    buf.assert_untouched_from(0);
    assert!(!engine.hw().output_enabled);
}

#[test]
fn an_interrupt_with_no_flag_is_ignored() {
    let mut engine = engine();
    engine.on_interrupt();
    assert_eq!(engine.phase(), Phase::Idle);
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(0, 2, &buf));
    step(&mut engine, 5); // ReadN acknowledged, no sector yet
    assert_eq!(engine.phase(), Phase::Reading);
    engine.on_interrupt();
    assert_eq!(engine.phase(), Phase::Reading);
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    buf.assert_sectors(0, 2);
}

// ------------------------------------------------------------------- lease

#[test]
fn an_idle_drive_is_leased_at_once() {
    let mut engine = engine();
    assert_eq!(engine.request_audio_lease(), LeaseState::Granted);
    assert_eq!(engine.owner(), Owner::Audio);
    assert!(
        !engine.hw().source_open,
        "the audio code owns the controller"
    );
}

#[test]
fn the_handler_leaves_the_controller_alone_while_audio_holds_it() {
    let mut engine = engine();
    engine.request_audio_lease();
    let before = engine.hw().discards;
    engine.on_interrupt();
    assert_eq!(engine.hw().discards, before);
    assert_eq!(engine.stats().irq_count, 1);
}

#[test]
fn requests_queue_while_audio_holds_the_drive_and_run_after() {
    let mut engine = engine();
    engine.request_audio_lease();
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(40, 2, &buf));
    assert_eq!(engine.state(ticket), RequestState::Queued);
    assert!(engine.hw().log.is_empty());
    assert!(!engine.hw_mut().raise());
    assert!(engine.release_audio_lease());
    assert_eq!(engine.owner(), Owner::Data);
    assert!(engine.hw().source_open);
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    buf.assert_sectors(40, 2);
}

#[test]
fn a_lease_aborts_the_read_in_flight() {
    let mut engine = engine();
    let (a, b) = (Buf::new(10), Buf::new(1));
    let reading = submit(&mut engine, request(100, 10, &a));
    let waiting = submit(&mut engine, request(900, 1, &b));
    step_until_received(&mut engine, reading, 4);
    assert_eq!(engine.request_audio_lease(), LeaseState::Pending);
    assert_eq!(engine.lease_state(), LeaseState::Pending);
    assert!(engine.hw().source_open, "the handler still has work to do");
    run(&mut engine);
    assert_eq!(engine.lease_state(), LeaseState::Granted);
    assert_eq!(engine.owner(), Owner::Audio);
    let done = finished(&mut engine, reading);
    assert_eq!((done.outcome, done.received), (Outcome::Cancelled, 4));
    a.assert_sectors(100, 4);
    a.assert_untouched_from(4);
    // The queued request did not start, and the source is closed.
    assert_eq!(engine.state(waiting), RequestState::Queued);
    assert_eq!(engine.hw().setloc_targets(), [100]);
    assert!(!engine.hw().source_open);
    assert_eq!(*engine.hw().commands().last().unwrap(), CMD_PAUSE);
}

#[test]
fn a_lease_does_not_start_or_chain_the_request_behind_the_one_it_aborts() {
    let mut engine = engine();
    let (a, b) = (Buf::new(2), Buf::new(2));
    let first = submit(&mut engine, request(10, 2, &a));
    let second = submit(&mut engine, request(12, 2, &b));
    step_until_received(&mut engine, first, 1);
    engine.request_audio_lease();
    run(&mut engine);
    assert_eq!(finished(&mut engine, first).outcome, Outcome::Cancelled);
    assert_eq!(engine.state(second), RequestState::Queued);
    assert_eq!(engine.lease_state(), LeaseState::Granted);
    assert_eq!(engine.stats().chained, 0);
    b.assert_untouched_from(0);
}

#[test]
fn a_lease_waits_for_the_pause_that_follows_a_finished_request() {
    let mut engine = engine();
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(10, 1, &buf));
    step_until_finished(&mut engine, ticket); // last sector lands, Pause issued
    assert_eq!(engine.request_audio_lease(), LeaseState::Pending);
    run(&mut engine);
    assert_eq!(engine.lease_state(), LeaseState::Granted);
    assert_eq!(engine.stats().requests_cancelled, 0);
}

#[test]
fn a_lease_can_be_withdrawn_while_pending() {
    let mut engine = engine();
    let buf = Buf::new(10);
    let ticket = submit(&mut engine, request(0, 10, &buf));
    step_until_received(&mut engine, ticket, 2);
    assert_eq!(engine.request_audio_lease(), LeaseState::Pending);
    assert!(engine.release_audio_lease());
    assert_eq!(engine.lease_state(), LeaseState::None);
    run(&mut engine);
    assert_eq!(engine.owner(), Owner::Data);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Cancelled);
    assert!(!engine.release_audio_lease(), "nothing left to release");
}

#[test]
fn the_first_read_after_audio_pauses_first() {
    let mut engine = engine();
    engine.request_audio_lease();
    engine.release_audio_lease();
    let buf = Buf::new(1);
    let ticket = submit(&mut engine, request(5, 1, &buf));
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    let mut expected = vec![CMD_PAUSE];
    expected.extend(BRACKET);
    assert_eq!(engine.hw().commands(), expected);
    // Only the first one.
    let again = submit(&mut engine, request(6, 1, &buf));
    run(&mut engine);
    assert_eq!(finished(&mut engine, again).outcome, Outcome::Done);
    assert_eq!(engine.hw().commands().len(), expected.len() + BRACKET.len());
}

#[test]
fn the_recovery_pause_after_audio_can_be_turned_off() {
    let config = Config {
        pause_after_audio: false,
        ..Config::DEFAULT
    };
    let mut engine = engine_with(config);
    engine.request_audio_lease();
    engine.release_audio_lease();
    let buf = Buf::new(1);
    submit(&mut engine, request(5, 1, &buf));
    run(&mut engine);
    assert_eq!(engine.hw().commands(), BRACKET);
}

#[test]
fn the_audio_lease_is_a_no_op_before_attach() {
    let mut engine = Engine::new(FakeDrive::new(), Config::DEFAULT);
    assert_eq!(engine.request_audio_lease(), LeaseState::Granted);
    assert_eq!(engine.owner(), Owner::Boot);
    assert!(!engine.release_audio_lease());
}

// ------------------------------------------------------------------- owner

#[test]
fn nothing_runs_before_attach() {
    let mut engine = Engine::new(FakeDrive::new(), Config::DEFAULT);
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(0, 2, &buf));
    assert_eq!(engine.state(ticket), RequestState::Queued);
    assert!(engine.hw().log.is_empty());
    assert!(engine.hw().source_calls.is_empty());
    engine.attach();
    assert!(engine.hw().source_open);
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    buf.assert_sectors(0, 2);
}

#[test]
fn detach_needs_an_idle_drive_and_an_empty_queue() {
    let mut engine = engine();
    let buf = Buf::new(4);
    let ticket = submit(&mut engine, request(0, 4, &buf));
    assert!(!engine.detach(), "busy");
    engine.cancel_all();
    assert!(!engine.detach(), "still stopping");
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Cancelled);
    assert!(engine.detach());
    assert_eq!(engine.owner(), Owner::Boot);
    assert!(!engine.hw().source_open);
    // Attached again, it reads as before.
    engine.attach();
    let again = submit(&mut engine, request(0, 1, &buf));
    run(&mut engine);
    assert_eq!(finished(&mut engine, again).outcome, Outcome::Done);
}

#[test]
fn only_while_busy_opens_the_source_for_the_transfer_alone() {
    let config = Config {
        irq_mask_policy: IrqMaskPolicy::OnlyWhileBusy,
        ..Config::DEFAULT
    };
    let mut engine = engine_with(config);
    assert!(!engine.hw().source_open, "idle after attach");
    let buf = Buf::new(2);
    let ticket = submit(&mut engine, request(0, 2, &buf));
    assert!(engine.hw().source_open);
    run(&mut engine);
    assert_eq!(finished(&mut engine, ticket).outcome, Outcome::Done);
    assert!(
        !engine.hw().source_open,
        "closed again once the drive stopped"
    );
    // And a failure closes it too.
    engine.hw_mut().refuse_command = Some(CMD_SETLOC);
    let failed = submit(&mut engine, request(0, 2, &buf));
    assert!(matches!(
        finished(&mut engine, failed).outcome,
        Outcome::Failed(_)
    ));
    assert!(!engine.hw().source_open);
}

#[test]
fn always_on_keeps_the_source_open_between_transfers() {
    let mut engine = engine();
    assert!(engine.hw().source_open);
    let buf = Buf::new(1);
    submit(&mut engine, request(0, 1, &buf));
    run(&mut engine);
    assert!(engine.hw().source_open);
    // One open at attach, no flapping afterwards.
    assert_eq!(engine.hw().source_calls, [true]);
}

// ------------------------------------------------------------------ report

#[test]
fn tickets_move_from_queued_to_active_to_finished() {
    let mut engine = engine();
    let (a, b) = (Buf::new(3), Buf::new(1));
    let first = submit(&mut engine, request(10, 3, &a));
    let second = submit(&mut engine, request(50, 1, &b));
    assert_ne!(first, second);
    assert_eq!(engine.state(first), RequestState::Active { received: 0 });
    assert_eq!(engine.state(second), RequestState::Queued);
    step_until_received(&mut engine, first, 2);
    assert_eq!(engine.state(first), RequestState::Active { received: 2 });
    run(&mut engine);
    assert!(matches!(engine.state(first), RequestState::Finished(_)));
    assert!(matches!(engine.state(second), RequestState::Finished(_)));
    assert_eq!(engine.state(Ticket(9999)), RequestState::Unknown);
}

#[test]
fn old_results_are_forgotten_after_the_ring_fills() {
    let mut engine = engine();
    let buf = Buf::new(1);
    let mut tickets = Vec::new();
    for _ in 0..RECENT_RESULTS + 2 {
        tickets.push(submit(&mut engine, request(0, 1, &buf)));
        run(&mut engine);
    }
    assert_eq!(engine.state(tickets[0]), RequestState::Unknown);
    assert!(matches!(
        engine.state(*tickets.last().unwrap()),
        RequestState::Finished(_)
    ));
}

#[test]
fn counters_add_up() {
    let mut engine = engine();
    engine.hw_mut().clock_step = 3;
    engine.hw_mut().sectors_in_flight_at_pause = 1;
    let (a, b) = (Buf::new(4), Buf::new(2));
    submit(&mut engine, request(0, 4, &a));
    submit(&mut engine, request(4, 2, &b));
    let delivered = run(&mut engine);
    let stats = engine.stats();
    assert_eq!(stats.irq_count, delivered);
    assert_eq!(stats.sectors, 6);
    assert_eq!(stats.discarded_sectors, 1);
    assert_eq!(stats.requests_done, 2);
    assert_eq!(stats.chained, 1);
    assert_eq!(stats.max_irq_ticks, 3);
    assert_eq!(stats.phase, Phase::Done as u32);
    assert_eq!(
        engine.hw().popped.len() as u32,
        stats.sectors + stats.discarded_sectors
    );
}

#[test]
fn the_trace_records_commands_and_interrupts() {
    let mut engine = engine();
    let buf = Buf::new(1);
    submit(&mut engine, request(0, 1, &buf));
    run(&mut engine);
    let commands: Vec<u8> = engine
        .hw()
        .trace
        .iter()
        .filter(|word| **word >> 24 == 6)
        .map(|word| (word >> 8) as u8)
        .collect();
    assert_eq!(commands, BRACKET);
    assert!(engine.hw().trace.iter().any(|word| *word >> 24 == 1));
}

#[test]
fn a_ticket_names_one_request() {
    let mut engine = engine();
    let buf = Buf::new(1);
    let a = submit(&mut engine, request(0, 1, &buf));
    let b = submit(&mut engine, request(0, 1, &buf));
    assert_eq!((a.id(), b.id()), (1, 2));
}

// ------------------------------------------------------------ model checks

/// xorshift32.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

struct Submitted {
    ticket: Ticket,
    buf: Buf,
    lba: u32,
    sectors: u32,
    byte_len: u32,
    done: Option<Completion>,
}

/// Random submissions, cancels, leases, faults and time against the
/// invariants every request must keep: what landed is the disc's data,
/// nothing is written past what was reported, every request ends, and the
/// engine ends idle with an empty queue.
fn model_run(seed: u32) {
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761) | 1);
    let mut engine = engine();
    let mut all: Vec<Submitted> = Vec::new();
    let mut leased = false;

    let poll = |engine: &mut Engine<FakeDrive>, all: &mut Vec<Submitted>| {
        for entry in all.iter_mut().filter(|e| e.done.is_none()) {
            if let RequestState::Finished(done) = engine.state(entry.ticket) {
                entry.done = Some(done);
            }
        }
    };

    for _ in 0..60 {
        match rng.below(10) {
            0..=2 => {
                let sectors = 1 + rng.below(6);
                let lba = [0, 6, 12, 40, 41, 100][rng.below(6) as usize] + rng.below(3) * 6;
                let buf = Buf::new(sectors);
                let mut req = request(lba, sectors, &buf)
                    .with_priority(Priority([0, 1, 1, 200][rng.below(4) as usize]))
                    .with_resumes(rng.below(3) as u8);
                let mut byte_len = 0;
                if rng.below(4) == 0 {
                    byte_len = (sectors - 1) * 2048 + 1 + rng.below(2048);
                    req = req.with_byte_len(byte_len);
                }
                if let Ok(ticket) = engine.submit(req) {
                    all.push(Submitted {
                        ticket,
                        buf,
                        lba,
                        sectors,
                        byte_len,
                        done: None,
                    });
                }
            }
            3..=5 => {
                for _ in 0..1 + rng.below(8) {
                    if !engine.hw_mut().raise() {
                        break;
                    }
                    engine.on_interrupt();
                }
            }
            6 => {
                if let Some(entry) = all.get(rng.below(all.len().max(1) as u32) as usize) {
                    engine.cancel(entry.ticket);
                }
            }
            7 => {
                if leased {
                    engine.release_audio_lease();
                    leased = false;
                } else {
                    engine.request_audio_lease();
                    leased = true;
                }
            }
            8 => {
                engine.hw_mut().error_at.push(rng.below(160));
                engine.hw_mut().sectors_in_flight_at_pause = rng.below(3);
            }
            _ => {
                engine.hw_mut().vblank += rng.below(40);
                engine.service();
            }
        }
        poll(&mut engine, &mut all);
    }

    // Wind down: no more faults, release the drive, let everything finish.
    engine.hw_mut().error_at.clear();
    if leased {
        engine.release_audio_lease();
    }
    for _ in 0..10_000 {
        if engine.hw_mut().raise() {
            engine.on_interrupt();
        } else if !engine.is_idle() {
            engine.hw_mut().vblank += 700;
            engine.service();
        } else {
            break;
        }
    }
    poll(&mut engine, &mut all);

    assert!(engine.is_idle(), "seed {seed}: phase {:?}", engine.phase());
    assert_eq!(engine.queued_count(), 0, "seed {seed}: queue not drained");
    for entry in &all {
        let done = entry
            .done
            .unwrap_or_else(|| panic!("seed {seed}: {:?} never finished", entry.ticket));
        assert!(done.received <= entry.sectors, "seed {seed}");
        if done.outcome == Outcome::Done {
            assert_eq!(done.received, entry.sectors, "seed {seed}");
        }
        // What landed is the disc's data, whole sectors and then the tail.
        let landed_words = if entry.byte_len != 0 && done.received == entry.sectors {
            (entry.sectors as usize - 1) * 512
                + (entry.byte_len as usize - (entry.sectors as usize - 1) * 2048).div_ceil(4)
        } else {
            done.received as usize * 512
        };
        for word in 0..landed_words {
            let want = sector_word(entry.lba + (word / 512) as u32, (word % 512) as u32);
            assert_eq!(entry.buf.word(word), want, "seed {seed} word {word}");
        }
        // And nothing beyond it was touched.
        for word in landed_words..(entry.sectors as usize + 1) * 512 {
            assert_eq!(entry.buf.word(word), CANARY, "seed {seed} word {word}");
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "exhaustive sweep; runs natively")]
fn random_workloads_keep_the_invariants() {
    for seed in 1..=400 {
        model_run(seed);
    }
}

#[test]
fn a_few_workloads_under_the_interpreter() {
    for seed in 1..=3 {
        model_run(seed);
    }
}
