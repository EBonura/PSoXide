// SPDX-License-Identifier: GPL-2.0-or-later
//! The job against an in-memory card that logs every transaction with the
//! vblank it happened on, so the spacing the real card needs is checked
//! without a console.

extern crate std;

use super::*;
use crate::{RamCard, CARD_SIZE, DATA_BLOCKS};
use std::boxed::Box;
use std::vec::Vec;

const NAME: &str = "BASLUS-99999JOB0001";
const OTHER: &str = "BASLUS-99999JOB0002";

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Op {
    Read(u16),
    Write(u16),
}

/// A card image on the heap: the host test thread's stack is small and the
/// debug build copies a `RamCard` by value more than once.
struct Heap(Box<RamCard>);

impl Heap {
    fn from_image(image: &[u8]) -> Self {
        let mut card = Box::new(RamCard::new());
        for (i, chunk) in image.chunks(FRAME_SIZE).enumerate() {
            let frame: &[u8; FRAME_SIZE] = chunk.try_into().unwrap();
            card.write_frame(i as u16, frame).unwrap();
        }
        Heap(card)
    }

    fn image(&self) -> &[u8; CARD_SIZE] {
        self.0.image()
    }
}

impl Block for Heap {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        self.0.read_frame(frame, out)
    }
    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()> {
        self.0.write_frame(frame, data)
    }
}

/// A card that logs each transaction with the current vblank, and can stop
/// answering writes after a set number of them (a card pulled mid-save).
struct Logged {
    inner: Heap,
    now: u32,
    ops: [(u32, Op); 1024],
    count: usize,
    writes_left: usize,
}

impl Logged {
    fn on(image: &[u8]) -> Self {
        Logged {
            inner: Heap::from_image(image),
            now: 0,
            ops: [(0, Op::Read(0)); 1024],
            count: 0,
            writes_left: usize::MAX,
        }
    }

    fn log(&self) -> &[(u32, Op)] {
        &self.ops[..self.count]
    }

    fn writes(&self) -> usize {
        self.log()
            .iter()
            .filter(|(_, op)| matches!(op, Op::Write(_)))
            .count()
    }

    fn record(&mut self, op: Op) {
        self.ops[self.count] = (self.now, op);
        self.count += 1;
    }
}

impl Block for Logged {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        self.record(Op::Read(frame));
        self.inner.read_frame(frame, out)
    }

    fn write_frame(&mut self, _frame: u16, _data: &[u8; FRAME_SIZE]) -> Result<()> {
        panic!("a job must write unsettled and keep the commit wait itself");
    }

    fn write_frame_unsettled(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()> {
        if self.writes_left == 0 {
            return Err(Error::NoCard);
        }
        self.writes_left -= 1;
        self.record(Op::Write(frame));
        self.inner.write_frame(frame, data)
    }
}

fn formatted_with(saves: &[(&str, &[u8])]) -> Vec<u8> {
    let mut card = Card::new(Heap::from_image(&[0u8; CARD_SIZE]));
    card.format().unwrap();
    for (name, data) in saves {
        card.write(name, "T", data).unwrap();
    }
    card.into_inner().image().to_vec()
}

/// Run a whole operation, one vblank per step. Returns the vblank it
/// finished on.
fn run<const N: usize, F>(
    job: &mut CardJob<N>,
    dev: &mut Logged,
    start: u32,
    plan: F,
) -> Result<u32>
where
    F: FnOnce(&mut Card<Staged<'_, N>>) -> Result<()>,
{
    let mut plan = Some(plan);
    let mut now = start;
    job.begin();
    for _ in 0..50_000 {
        dev.now = now;
        match job.step(dev, now)? {
            Step::NeedsPlan => job.plan(plan.take().expect("planned twice"))?,
            Step::Done => return Ok(now),
            _ => {}
        }
        now = now.wrapping_add(1);
    }
    panic!("the job never finished");
}

fn read_back(dev: Logged, name: &str, buf: &mut [u8]) -> Result<usize> {
    Card::new(dev.inner).read(name, buf)
}

#[test]
fn a_job_leaves_the_card_byte_identical_to_the_blocking_call() {
    let old = [0x11u8; 9_000];
    let new = [0x22u8; 9_500];
    let base = formatted_with(&[(NAME, &old), (OTHER, b"another game's save")]);

    let mut blocking = Card::new(Heap::from_image(&base));
    blocking.write(NAME, "T", &new).unwrap();

    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(9_500) }>::new();
    run(&mut job, &mut dev, 100, |c| c.write(NAME, "T", &new)).unwrap();

    assert!(dev.inner.image() == blocking.into_inner().image());
    assert!(!job.is_busy());
}

#[test]
fn it_reads_the_directory_then_only_writes() {
    let base = formatted_with(&[(NAME, b"old")]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(3) }>::new();
    run(&mut job, &mut dev, 0, |c| c.write(NAME, "T", b"new")).unwrap();

    let ops = dev.log();
    for (i, (_, op)) in ops[..DIR_FRAMES].iter().enumerate() {
        assert_eq!(*op, Op::Read(i as u16));
    }
    // The plan reads only the cached directory: no card read follows the
    // probe, and the replacement queues exactly what `queue_frames` allows.
    assert!(ops[DIR_FRAMES..]
        .iter()
        .all(|(_, op)| matches!(op, Op::Write(_))));
    assert_eq!(dev.writes(), queue_frames(3));
}

#[test]
fn a_step_makes_at_most_one_transaction_and_a_write_is_followed_by_the_commit_wait() {
    let base = formatted_with(&[(NAME, &[7u8; 3_000])]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(3_000) }>::new();
    let end = run(&mut job, &mut dev, 1_000, |c| {
        c.write(NAME, "T", &[9u8; 3_000])
    })
    .unwrap();

    let ops = dev.log();
    // One transaction per step, one step per vblank.
    for pair in ops.windows(2) {
        assert!(pair[1].0 > pair[0].0, "two transactions on one vblank");
    }
    // After any write, the next transaction is at least SETTLE_VBLANKS later.
    for pair in ops.windows(2) {
        if matches!(pair[0].1, Op::Write(_)) {
            assert!(
                pair[1].0 - pair[0].0 >= SETTLE_VBLANKS,
                "{:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }
    // The job reports done only once the last commit has had its time.
    let last = ops.last().unwrap().0;
    assert_eq!(end - last, SETTLE_VBLANKS);
}

#[test]
fn waiting_steps_do_not_touch_the_card() {
    let base = formatted_with(&[]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    job.begin();
    let mut now = 50;
    let mut planned = false;
    let mut waits = 0;
    while job.is_busy() {
        dev.now = now;
        let before = dev.count;
        match job.step(&mut dev, now).unwrap() {
            Step::NeedsPlan => {
                job.plan(|c| c.write(NAME, "T", b"x")).unwrap();
                planned = true;
            }
            Step::Waiting => {
                waits += 1;
                assert_eq!(dev.count, before);
            }
            _ => {}
        }
        // Stepping twice within the same vblank never double-books the card.
        let again = dev.count;
        if job.is_busy() && job.step(&mut dev, now).unwrap() == Step::Waiting {
            assert_eq!(dev.count, again);
        }
        now += 1;
    }
    assert!(planned);
    assert!(waits > 0);
}

#[test]
fn the_wait_survives_the_vblank_counter_wrapping() {
    let base = formatted_with(&[]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    let start = u32::MAX - 20;
    run(&mut job, &mut dev, start, |c| c.write(NAME, "T", b"x")).unwrap();
    for pair in dev.log().windows(2) {
        if matches!(pair[0].1, Op::Write(_)) {
            assert!(pair[1].0.wrapping_sub(pair[0].0) >= SETTLE_VBLANKS);
        }
    }
    let mut buf = [0u8; 4];
    assert_eq!(read_back(dev, NAME, &mut buf), Ok(1));
}

#[test]
fn a_configured_wait_is_kept() {
    let base = formatted_with(&[]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(1) }>::new().with_settle_vblanks(3);
    run(&mut job, &mut dev, 0, |c| c.write(NAME, "T", b"x")).unwrap();
    let mut gaps = 0;
    for pair in dev.log().windows(2) {
        if matches!(pair[0].1, Op::Write(_)) {
            assert_eq!(pair[1].0 - pair[0].0, 3);
            gaps += 1;
        }
    }
    assert!(gaps >= 2);
}

#[test]
fn the_next_operation_waits_out_the_last_commit_of_the_one_before() {
    let base = formatted_with(&[]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    let first = run(&mut job, &mut dev, 0, |c| c.write(NAME, "T", b"x")).unwrap();
    let writes = dev.count;
    // The caller's clock is read once per operation: begin again on the very
    // vblank the first one finished, then once more on a vblank that is still
    // inside a commit window (a job abandoned right after a write).
    run(&mut job, &mut dev, first, |c| c.delete(NAME)).unwrap();
    assert!(dev.count > writes);
    let sent = dev.count;
    job.begin();
    let last_write = dev
        .log()
        .iter()
        .rposition(|(_, op)| matches!(op, Op::Write(_)))
        .unwrap();
    let at = dev.log()[last_write].0;
    for now in at..at + SETTLE_VBLANKS {
        assert_eq!(job.step(&mut dev, now), Ok(Step::Waiting));
    }
    assert_eq!(dev.count, sent);
    assert_eq!(job.step(&mut dev, at + SETTLE_VBLANKS), Ok(Step::Probing));
}

#[test]
fn a_pulled_card_leaves_the_old_save_or_the_new_one_after_any_number_of_writes() {
    let old = [0x11u8; 9_000];
    let new = [0x22u8; 9_500];
    let base = formatted_with(&[(NAME, &old)]);

    let mut completed = false;
    for allowed in 0..400 {
        let mut dev = Logged::on(&base);
        dev.writes_left = allowed;
        let mut job = CardJob::<{ queue_frames(9_500) }>::new();
        let result = run(&mut job, &mut dev, 0, |c| c.write(NAME, "T", &new));
        assert!(!job.is_busy());

        let mut buf = [0u8; 10_000];
        let n = read_back(dev, NAME, &mut buf)
            .unwrap_or_else(|e| panic!("after {allowed} writes the save is unreadable: {e:?}"));
        let got = &buf[..n];
        assert!(
            got == old || got == new,
            "after {allowed} writes the save is neither version"
        );
        if result.is_ok() {
            assert_eq!(got, new);
            completed = true;
            break;
        } else {
            assert_eq!(result, Err(Error::NoCard));
        }
    }
    assert!(completed);
}

#[test]
fn a_blank_card_is_formatted_and_written_in_one_plan() {
    let mut dev = Logged::on(&[0u8; CARD_SIZE]);
    let mut job = CardJob::<{ FORMAT_FRAMES + queue_frames(500) }>::new();
    run(&mut job, &mut dev, 0, |c| {
        assert!(!c.is_formatted()?);
        c.format()?;
        // The write sees the directory the format just queued.
        c.write(NAME, "T", &[5u8; 500])
    })
    .unwrap();
    assert_eq!(dev.writes(), FORMAT_FRAMES + 2 + 5 + 1);

    let mut card = Card::new(dev.inner);
    card.validate_filesystem().unwrap();
    let mut buf = [0u8; 600];
    assert_eq!(card.read(NAME, &mut buf), Ok(500));
    assert!(buf[..500].iter().all(|&b| b == 5));
}

#[test]
fn a_plan_the_queue_cannot_hold_is_refused_before_the_card_is_written() {
    let base = formatted_with(&[]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<4>::new();
    let result = run(&mut job, &mut dev, 0, |c| c.write(NAME, "T", &[1u8; 2_000]));
    assert_eq!(result, Err(Error::NoSpace));
    assert!(!job.is_busy());
    assert_eq!(dev.writes(), 0);
}

#[test]
fn a_plan_error_ends_the_job_with_the_card_untouched() {
    let mut dev = Logged::on(&[0u8; CARD_SIZE]); // blank: no MC header
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    let result = run(&mut job, &mut dev, 0, |c| {
        if !c.is_formatted()? {
            return Err(Error::NotFormatted);
        }
        c.write(NAME, "T", b"x")
    });
    assert_eq!(result, Err(Error::NotFormatted));
    assert!(!job.is_busy());
    assert_eq!(dev.writes(), 0);
    assert_eq!(dev.count, DIR_FRAMES);
}

#[test]
fn a_card_error_ends_the_job() {
    let mut dev = Logged::on(&[0u8; CARD_SIZE]);
    struct NoCardAtAll;
    impl Block for NoCardAtAll {
        fn read_frame(&mut self, _: u16, _: &mut [u8; FRAME_SIZE]) -> Result<()> {
            Err(Error::NoCard)
        }
        fn write_frame(&mut self, _: u16, _: &[u8; FRAME_SIZE]) -> Result<()> {
            Err(Error::NoCard)
        }
    }
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    job.begin();
    assert_eq!(job.step(&mut NoCardAtAll, 0), Err(Error::NoCard));
    assert!(!job.is_busy());
    assert_eq!(job.step(&mut dev, 1), Ok(Step::Idle));
    assert_eq!(dev.count, 0);
}

#[test]
fn planning_out_of_order_changes_nothing() {
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    assert_eq!(job.plan(|_| Ok(())), Err(Error::Protocol));
    job.begin();
    assert_eq!(job.plan(|_| Ok(())), Err(Error::Protocol));
    assert!(job.is_busy());
}

#[test]
fn a_plan_with_nothing_to_write_just_finishes() {
    let base = formatted_with(&[(NAME, b"keep")]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<4>::new();
    run(&mut job, &mut dev, 0, |c| {
        let mut list = [crate::Entry {
            name: [0; crate::MAX_NAME_LEN + 1],
            name_len: 0,
            blocks: 0,
        }; DATA_BLOCKS];
        assert_eq!(c.list(&mut list)?, 1);
        Ok(())
    })
    .unwrap();
    assert_eq!(dev.writes(), 0);
}

#[test]
fn staged_reads_outside_the_directory_are_refused() {
    let base = formatted_with(&[(NAME, b"keep")]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<4>::new();
    let result = run(&mut job, &mut dev, 0, |c| {
        let mut buf = [0u8; 8];
        c.read(NAME, &mut buf).map(|_| ())
    });
    assert_eq!(result, Err(Error::OutOfRange));
}

#[test]
fn queue_frames_is_what_a_replacing_write_queues() {
    for len in [0usize, 1, 100, 7_919, 7_920, 7_921, 16_000, 30_000, 60_000] {
        let blocks = crate::fs::blocks_for(CONTAINER_LEN + len);
        if blocks > DATA_BLOCKS / 2 {
            continue; // cannot be held twice on one card
        }
        let data = std_vec(len);
        let base = formatted_with(&[(NAME, &data[..len])]);
        let mut dev = Logged::on(&base);
        let mut job = CardJob::<{ queue_frames(30_000) }>::new();
        run(&mut job, &mut dev, 0, |c| c.write(NAME, "T", &data[..len])).unwrap();
        assert_eq!(dev.writes(), queue_frames(len), "payload {len}");
    }
}

#[test]
fn progress_runs_from_nothing_to_complete() {
    let base = formatted_with(&[]);
    let mut dev = Logged::on(&base);
    let mut job = CardJob::<{ queue_frames(1) }>::new();
    assert_eq!(job.progress_q8(), 0);
    job.begin();
    let mut last = 0;
    let mut now = 0;
    while job.is_busy() {
        dev.now = now;
        if job.step(&mut dev, now).unwrap() == Step::NeedsPlan {
            job.plan(|c| c.write(NAME, "T", b"x")).unwrap();
        }
        if job.is_busy() {
            let p = job.progress_q8();
            assert!(p >= last && p <= 256, "{last} then {p}");
            last = p;
        }
        now += 1;
    }
    assert_eq!(last, 256, "the last busy step is the final commit wait");
    assert_eq!(job.progress_q8(), 0);
}

/// A payload buffer big enough for the largest case above.
fn std_vec(len: usize) -> [u8; 70_000] {
    let mut v = [0u8; 70_000];
    for (i, b) in v[..len].iter_mut().enumerate() {
        *b = (i * 31 + 3) as u8;
    }
    v
}

#[test]
fn the_idle_job_is_all_zero_bytes_so_a_static_one_lives_in_bss() {
    // No padding: the size is exactly the fields, so every byte is defined.
    const SIZE: usize = 4 * 4 + 4 + 7 * 4 + DIR_FRAMES * FRAME_SIZE + 7 * FRAME_SIZE;
    assert_eq!(core::mem::size_of::<CardJob<7>>(), SIZE);
    let job = CardJob::<7>::new();
    // SAFETY: the job is `repr(C)` with no padding, fully initialised.
    let bytes =
        unsafe { core::slice::from_raw_parts(core::ptr::from_ref(&job).cast::<u8>(), SIZE) };
    assert!(bytes.iter().all(|&b| b == 0));
}
