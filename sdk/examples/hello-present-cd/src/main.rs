//! `hello-present-cd` -- the one present path while the CD drive delivers
//! sectors, and across instruction-cache flushes.
//!
//! The disc carries `CDTEST.BIN` (`mkisopsx --cdtest-sectors`), found by name.
//! Four phases, each checked against the pattern every sector must hold:
//!
//! 1. Polled reads (`SectorReader`): [`READS`] reads of 1 to 8 sectors at
//!    scattered places with nothing presented, the control; then the same
//!    reads with a chain frame handed to the presenter between the sectors of
//!    each read (so the thread and the handler kick while the drive streams)
//!    and a frame drawn through the port after each read.
//! 2. Interrupt-driven streaming (`psx-cdstream`): [`STREAM_SECTORS`] sectors
//!    in requests of [`REQUEST_SECTORS`], the control; then the same stream
//!    while the foreground presents chain and drawn frames.
//! 3. Cache flushes: [`FLUSH_FRAMES`] chain frames each followed at once by an
//!    instruction-cache flush, while the frame waits in the slot or walks, and
//!    drawn frames each followed by one.
//!
//! A VBlank hook counts a tear whenever a chain frame had started before the
//! frame before it was on screen. The run fails on a tear, a part not kicked exactly once, a frame
//! not flipped exactly once, a flip sooner than the cap, a stall the presenter
//! had to recover, a wrong sector, or a CD error, retry, timeout or dropped
//! sector the same phase does not have with presenting off. The verdict goes
//! to the TTY (`PRESENT-CD PASS ...` or `PRESENT-CD FAIL ...`);
//! `tools/present_cd_gate.sh` reads it.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use psx_cdstream::{Config, Outcome, Priority, Request, RequestState, Ticket};
use psx_fmv::iso;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::{FillRect, TriGouraud};
use psx_gpu::Gpu;
use psx_io::cd::reader::SectorReader;
use psx_io::periph::Cd;
use psx_rt::present::{self, Part, Presenter};
use psx_rt::{dispatch, interrupts, tty};

const FILE_NAME: &str = "CDTEST.BIN";
const SECTOR_WORDS: usize = 512;
/// Polled reads in each run of phase 1.
const READS: u32 = 24;
/// Sectors streamed in each run of phase 2, and the size of one request.
const STREAM_SECTORS: u32 = 192;
const REQUEST_SECTORS: u32 = 16;
/// Chain frames in phase 3.
const FLUSH_FRAMES: u32 = 60;
/// VBlanks any one wait is given.
const PATIENCE_VBLANKS: u32 = 600;
/// Chain parts and frames the hook can follow.
const MAX_PARTS: usize = 2048;

const EMPTY: TriGouraud = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]);
static mut TABLES: [OrderingTable<8>; 2] = [OrderingTable::new(), OrderingTable::new()];
static mut PACKETS: [[TriGouraud; 4]; 2] = [[EMPTY; 4], [EMPTY; 4]];
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut STREAM: [[u32; REQUEST_SECTORS as usize * SECTOR_WORDS]; 2] =
    [[0; REQUEST_SECTORS as usize * SECTOR_WORDS]; 2];
static mut FILE_LBA: u32 = 0;
static mut FILE_SECTORS: u32 = 0;

// The hook's view: for chain part `k`, the frame it belongs to (frames are
// numbered in the order they are handed over, chain and drawn alike).
static mut FRAME_OF_PART: [u32; MAX_PARTS] = [0; MAX_PARTS];
static mut RECORDING: bool = false;
static mut KICK_BASE: u32 = 0;
static mut FLIP_BASE: u32 = 0;
static mut TEARS: u32 = 0;

fn flips_total() -> u32 {
    interrupts::flip_histogram().iter().sum()
}

/// Once per VBlank, after psx-rt's flip and kick.
fn hook() {
    // SAFETY: plain statics; the thread writes `FRAME_OF_PART[k]` before part
    // `k` is submitted and reads `TEARS` once `RECORDING` is clear.
    unsafe {
        if !read_volatile(addr_of!(RECORDING)) {
            return;
        }
        let kicks = present::stats()
            .kick_count
            .wrapping_sub(read_volatile(addr_of!(KICK_BASE)));
        if kicks == 0 || kicks as usize > MAX_PARTS {
            return;
        }
        let flips = flips_total().wrapping_sub(read_volatile(addr_of!(FLIP_BASE)));
        let newest = read_volatile(addr_of!(FRAME_OF_PART[kicks as usize - 1]));
        // The newest kicked frame may start only once the one before it is on
        // screen (frame `newest - 1`, the `newest`-th flip), so it never draws
        // into the buffer on screen (frames alternate buffers).
        if newest > flips {
            write_volatile(addr_of_mut!(TEARS), read_volatile(addr_of!(TEARS)) + 1);
        }
    }
}

// ------------------------------------------------------------------ pattern

fn expected_byte(index: u32, sectors: u32) -> u8 {
    const MAGIC: &[u8; 8] = b"PSOXSTRM";
    if index < 8 {
        MAGIC[index as usize]
    } else if index < 12 {
        sectors.to_le_bytes()[(index - 8) as usize]
    } else {
        index
            .wrapping_mul(37)
            .wrapping_add(index >> 3)
            .wrapping_add(0x5d) as u8
    }
}

fn expected_word(word: u32, sectors: u32) -> u32 {
    let at = word * 4;
    u32::from(expected_byte(at, sectors))
        | u32::from(expected_byte(at + 1, sectors)) << 8
        | u32::from(expected_byte(at + 2, sectors)) << 16
        | u32::from(expected_byte(at + 3, sectors)) << 24
}

/// Do the `sectors` sectors at `buffer` hold file sectors `first..`?
fn verify(buffer: *const u32, first: u32, sectors: u32) -> bool {
    // SAFETY: written once in `main` before any phase runs.
    let total = unsafe { FILE_SECTORS };
    let first_word = first * SECTOR_WORDS as u32;
    (0..sectors * SECTOR_WORDS as u32).all(|word| {
        // SAFETY: inside the caller's `sectors` sectors, whose read has ended.
        unsafe {
            buffer.add(word as usize).read_volatile() == expected_word(first_word + word, total)
        }
    })
}

// ------------------------------------------------------------------- frames

/// What presenting does between reads and while waiting.
struct Show {
    presenter: Presenter,
    buffers: DoubleBuffer,
    /// Frames handed over, chain and drawn.
    frames: u32,
    /// Chain parts submitted.
    parts: u32,
}

impl Show {
    /// Hand over a chain frame if the slot is free, without waiting.
    fn chain_if_free(&mut self) {
        if present::is_slot_full() {
            return;
        }
        present::wait_arena_free();
        let side = (self.frames & 1) as usize;
        // SAFETY: `wait_arena_free` guarantees no walk reads this side, and
        // these are the only references made to it this frame.
        let (table, packets) = unsafe {
            (
                &mut (*addr_of_mut!(TABLES))[side],
                &mut (*addr_of_mut!(PACKETS))[side],
            )
        };
        let mut frame = table.frame();
        frame.end_with_draw_done();
        for (t, packet) in packets.iter_mut().enumerate() {
            let shade = ((t as u32 * 30 + self.frames * 5) & 0x7f) as u8 + 0x20;
            *packet = TriGouraud::new(
                [(0, 0), (319, 0), (0, 239)],
                [(shade, 0, 40), (0, shade, 40), (40, 0, shade)],
            );
            frame.add(1 + t, packet);
        }
        if (self.parts as usize) < MAX_PARTS {
            // SAFETY: the hook reads this entry only once kicks pass it.
            unsafe {
                write_volatile(
                    addr_of_mut!(FRAME_OF_PART[self.parts as usize]),
                    self.frames,
                )
            };
        }
        // SAFETY: the table and packets are static and not rebuilt before
        // `wait_arena_free` frees this side; the table ends on GP0(1Fh).
        unsafe {
            self.presenter
                .submit(&mut self.buffers, table.submit_head(), Part::Final)
        };
        self.parts += 1;
        self.frames += 1;
    }

    /// Draw a frame through the port and hand it over.
    fn drawn(&mut self, gpu: &mut Gpu) {
        let (_, y) = self.buffers.draw_origin();
        gpu.draw(&FillRect::new((0, y), (320, 240), (40, 8, 8)));
        gpu.draw(&FillRect::new(
            ((self.frames * 8 % 300) as u16, y + 100),
            (16, 16),
            (230, 230, 230),
        ));
        self.presenter.present_drawn(&mut self.buffers);
        self.frames += 1;
    }
}

// ------------------------------------------------------------------- phase 1

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Reads {
    sectors: u32,
    errors: u32,
    retries: u32,
}

/// One read of `count` sectors from file sector `first`; with `show`, a chain
/// frame goes to the presenter between its sectors. `true` when every sector
/// arrived with the pattern.
fn read_once(
    reader: &mut SectorReader,
    first: u32,
    count: u32,
    show: Option<&mut Show>,
) -> (bool, u32) {
    // SAFETY: written once in `main`.
    let lba = unsafe { FILE_LBA } + first;
    let mut good = 0;
    let mut all = reader.prepare() && reader.start_read(lba);
    let mut show = show;
    if all {
        for sector in 0..count {
            // SAFETY: single thread, one borrow at a time.
            let buffer = unsafe { &mut *addr_of_mut!(SECTOR) };
            if !reader.read_sector(buffer) || !verify(buffer.as_ptr(), first + sector, 1) {
                all = false;
                break;
            }
            good += 1;
            if let Some(show) = show.as_deref_mut() {
                show.chain_if_free();
            }
        }
    }
    reader.stop();
    (all, good)
}

fn run_reads(reader: &mut SectorReader, mut show: Option<(&mut Show, &mut Gpu)>) -> Reads {
    // SAFETY: written once in `main`.
    let file_sectors = unsafe { FILE_SECTORS };
    let mut out = Reads::default();
    for read in 0..READS {
        let count = 1 + read % 8;
        let first = (read * 37) % (file_sectors - 8);
        let (ok, good) = read_once(reader, first, count, show.as_mut().map(|(s, _)| &mut **s));
        out.sectors += good;
        if !ok {
            out.retries += 1;
            let _ = reader.stop_proved();
            let (ok, good) = read_once(reader, first, count, None);
            out.sectors += good;
            out.errors += u32::from(!ok);
        }
        if let Some((show, gpu)) = show.as_mut() {
            show.drawn(gpu);
        }
    }
    let _ = reader.stop_proved();
    out
}

// ------------------------------------------------------------------- phase 2

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Stream {
    good: u32,
    timeouts: u32,
    failed: u32,
    dropped: u32,
}

fn stream_slot(slot: usize) -> *mut u32 {
    // SAFETY: `slot` < 2; the address is formed without a reference.
    unsafe {
        addr_of_mut!(STREAM)
            .cast::<[u32; REQUEST_SECTORS as usize * SECTOR_WORDS]>()
            .add(slot)
            .cast()
    }
}

fn submit_stream(first: u32, slot: usize) -> Option<Ticket> {
    // SAFETY: each slot is static, and nothing reads or writes it until the
    // request that fills it has finished.
    let request = unsafe { Request::new_raw(FILE_LBA + first, REQUEST_SECTORS, stream_slot(slot)) };
    psx_cdstream::submit(request.with_priority(Priority::NORMAL)).ok()
}

fn run_stream(mut show: Option<(&mut Show, &mut Gpu)>) -> Stream {
    let before = psx_cdstream::stats();
    let mut out = Stream::default();
    let mut tickets = [submit_stream(0, 0), submit_stream(REQUEST_SECTORS, 1)];
    let mut next = 2 * REQUEST_SECTORS;
    let mut first = 0u32;
    let mut slot = 0;
    let mut turn = 0u32;
    while first < STREAM_SECTORS {
        let start = interrupts::vblank_count();
        let done = loop {
            if let Some(ticket) = tickets[slot] {
                if let RequestState::Finished(done) = psx_cdstream::state(ticket) {
                    break Some(done);
                }
            } else {
                break None;
            }
            if interrupts::vblank_count().wrapping_sub(start) > PATIENCE_VBLANKS {
                break None;
            }
            if let Some((show, gpu)) = show.as_mut() {
                turn += 1;
                if turn % 4 == 0 {
                    show.drawn(gpu);
                } else {
                    show.chain_if_free();
                }
            }
        };
        match done {
            Some(done)
                if done.outcome == Outcome::Done
                    && done.received == REQUEST_SECTORS
                    && verify(stream_slot(slot), first, REQUEST_SECTORS) =>
            {
                out.good += REQUEST_SECTORS;
            }
            Some(_) => out.failed += 1,
            None => out.timeouts += 1,
        }
        first += REQUEST_SECTORS;
        tickets[slot] = if next < STREAM_SECTORS {
            let ticket = submit_stream(next, slot);
            next += REQUEST_SECTORS;
            ticket
        } else {
            None
        };
        slot ^= 1;
    }
    let after = psx_cdstream::stats();
    out.dropped = after
        .discarded_sectors
        .wrapping_sub(before.discarded_sectors);
    out.failed += after.requests_failed.wrapping_sub(before.requests_failed);
    out
}

// ------------------------------------------------------------------- report

fn print(label: &str, value: u32) {
    const DIGITS: &[u8; 10] = b"0123456789";
    let mut text = [b'0'; 10];
    let mut at = 10;
    let mut v = value;
    loop {
        at -= 1;
        text[at] = DIGITS[(v % 10) as usize];
        v /= 10;
        if v == 0 {
            break;
        }
    }
    tty::print(" ");
    tty::print(label);
    tty::print("=");
    tty::print(core::str::from_utf8(&text[at..]).unwrap_or("?"));
}

fn lookup(reader: &mut SectorReader) -> Option<(u32, u32)> {
    // SAFETY: single thread, one borrow at a time.
    let sector = unsafe { &mut *addr_of_mut!(SECTOR) };
    let mut read = |lba: u32| -> Option<&[u8]> {
        if !reader.start_read(lba) {
            return None;
        }
        let ok = reader.read_sector(sector);
        reader.stop();
        // SAFETY: a `[u32; N]` viewed as its own bytes.
        ok.then(|| unsafe { core::slice::from_raw_parts(sector.as_ptr().cast::<u8>(), 2048) })
    };
    let (root, _) = iso::root_directory(read(iso::PVD_LBA)?)?;
    iso::find_in_directory(read(root)?, FILE_NAME)
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let buffers = DoubleBuffer::new(Resolution::R320X240);
    buffers.apply_draw_target(&mut gpu);
    gpu.wait_idle();
    let hooked = dispatch::on_vblank(hook).is_ok();
    let mut show = Show {
        presenter: Presenter::new(Some((8, 8, 24))),
        buffers,
        frames: 0,
        parts: 0,
    };

    let mut reader = SectorReader::with_cd(peripherals.cd);
    let found = reader.prepare().then(|| lookup(&mut reader)).flatten();
    let Some((lba, bytes)) = found else {
        tty::println("PRESENT-CD FAIL CDTEST.BIN not on the disc");
        return;
    };
    // SAFETY: written once, before any phase runs.
    unsafe {
        FILE_LBA = lba;
        FILE_SECTORS = bytes / 2048;
    }

    let before = present::stats();
    // SAFETY: the hook reads these only once `RECORDING` is set.
    unsafe {
        write_volatile(addr_of_mut!(KICK_BASE), before.kick_count);
        write_volatile(addr_of_mut!(FLIP_BASE), flips_total());
        write_volatile(addr_of_mut!(RECORDING), true);
    }
    let fast_before: u32 = interrupts::flip_histogram()[..2].iter().sum();

    // 1. Polled reads, without and with presenting.
    let reads_quiet = run_reads(&mut reader, None);
    let reads_shown = run_reads(&mut reader, Some((&mut show, &mut gpu)));
    present::wait_idle();

    // 2. The interrupt-driven stream, without and with presenting.
    let cd: Cd = reader.release();
    interrupts::wait_vblank();
    let config = Config {
        timeout_vblanks: 240,
        ..Config::DEFAULT
    };
    let installed = psx_cdstream::install(cd, config).is_ok();
    let stream_quiet = if installed {
        run_stream(None)
    } else {
        Stream::default()
    };
    let stream_shown = if installed {
        run_stream(Some((&mut show, &mut gpu)))
    } else {
        Stream::default()
    };
    present::wait_idle();

    // 3. Cache flushes between a hand-over and its kick, and while it walks.
    for _ in 0..FLUSH_FRAMES {
        present::wait_slot_empty();
        show.chain_if_free();
        psx_rt::cache::flush_instruction_cache();
        show.drawn(&mut gpu);
        psx_rt::cache::flush_instruction_cache();
    }
    present::wait_idle();
    interrupts::wait_vblank();
    interrupts::wait_vblank();

    // SAFETY: the hook stops writing once this is clear.
    let tears = unsafe {
        write_volatile(addr_of_mut!(RECORDING), false);
        read_volatile(addr_of!(TEARS))
    };
    let after = present::stats();
    let kicks = after.kick_count.wrapping_sub(before.kick_count);
    let recoveries = after.recovery_count.wrapping_sub(before.recovery_count);
    let flips = flips_total().wrapping_sub(unsafe { read_volatile(addr_of!(FLIP_BASE)) });
    let fast = interrupts::flip_histogram()[..2]
        .iter()
        .sum::<u32>()
        .wrapping_sub(fast_before);

    let checks = [
        (hooked, "no VBlank hook slot"),
        (installed, "psx-cdstream would not install"),
        (tears == 0, "a frame drew into the buffer on screen"),
        (kicks == show.parts, "parts not each kicked once"),
        (flips == show.frames, "frames not each flipped once"),
        (fast == 0, "a flip came sooner than the cap"),
        (recoveries == 0, "the presenter recovered a stall"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
        (
            reads_quiet.errors == 0 && reads_quiet.sectors > 0,
            "polled reads fail without presenting",
        ),
        (
            reads_shown.sectors == reads_quiet.sectors
                && reads_shown.errors <= reads_quiet.errors
                && reads_shown.retries <= reads_quiet.retries,
            "polled reads lost while presenting",
        ),
        (
            stream_quiet.good == STREAM_SECTORS,
            "the stream fails without presenting",
        ),
        (
            stream_shown.good == stream_quiet.good
                && stream_shown.timeouts <= stream_quiet.timeouts
                && stream_shown.failed <= stream_quiet.failed
                && stream_shown.dropped <= stream_quiet.dropped,
            "the stream lost sectors while presenting",
        ),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("PRESENT-CD check failed: ");
        tty::println(what);
    }
    tty::print(if failures == 0 {
        "PRESENT-CD PASS"
    } else {
        "PRESENT-CD FAIL"
    });
    for (label, value) in [
        ("failures", failures),
        ("frames", show.frames),
        ("parts", show.parts),
        ("kicks", kicks),
        (
            "threadkicks",
            after
                .thread_kick_count
                .wrapping_sub(before.thread_kick_count),
        ),
        ("flips", flips),
        ("tears", tears),
        ("reads_quiet", reads_quiet.sectors),
        ("reads_shown", reads_shown.sectors),
        ("retries_quiet", reads_quiet.retries),
        ("retries_shown", reads_shown.retries),
        ("stream_quiet", stream_quiet.good),
        ("stream_shown", stream_shown.good),
        ("dropped_shown", stream_shown.dropped),
        ("timeouts_shown", stream_shown.timeouts),
    ] {
        print(label, value);
    }
    tty::println("");
    loop {
        show.drawn(&mut gpu);
    }
}
