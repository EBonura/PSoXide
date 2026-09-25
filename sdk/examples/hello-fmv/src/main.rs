//! `hello-fmv` -- stream and play a video-only `.STR` from the disc.
//!
//! The disc carries `MOVIE.STR` (a 2048-byte-sector STR from
//! `psxavenc -t strv -v v2 -s 320x240 -r 15 -x 2`, built from a synthetic
//! test pattern) next to the EXE; `mkisopsx --file` puts it there.
//!
//! Pipeline per frame:
//!
//! 1. The CD streams sectors at double speed (ReadN, 2048-byte mode). A
//!    polled pump drains every ready sector into a three-slot frame ring
//!    (one decoding, one ready, one filling) and is run between every
//!    unit of work, so the drive never runs ahead by more than a column.
//! 2. `psx_fmv::bs::decode_frame` turns the frame's bitstream into MDEC
//!    run-length data on the CPU.
//! 3. The MDEC decodes it (DMA0 in, 15bpp), and each 16-pixel column comes
//!    back over DMA1 and goes to the back buffer in VRAM.
//! 4. The display flips to the new frame at a VBlank, paced to 15 fps.
//!
//! A late frame (a newer one completed while it was still queued) is
//! skipped, never shown out of time. Stats go to the TTY as
//! `FMV frames=.. late=.. lost=.. errors=.. sectors=.. vblanks=..`.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_fmv::{bs, iso, mdec, str::FrameAssembler};
use psx_gpu::{self as gpu, Resolution, VideoMode};
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::{interrupts, tty};
use psx_vram::VramRect;

const MOVIE: &str = "MOVIE.STR";
const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
const COLUMNS: u16 = WIDTH / 16;
const ROWS: u32 = HEIGHT as u32 / 16;
/// 15bpp column of 16 x HEIGHT pixels, two per word.
const COLUMN_WORDS: usize = 8 * HEIGHT as usize;
/// VBlanks per movie frame: 60 Hz / 15 fps.
const VBLANKS_PER_FRAME: u32 = 4;
/// Frame slot: 16 chunks of 2016 bytes (a 2x 15 fps frame is 10).
const SLOT_WORDS: usize = 16 * 2016 / 4;
const SLOTS: usize = 3;
/// MDEC run-length buffer: 64K halfwords.
const RLE_WORDS: usize = 32 * 1024;
/// Give up if no frame completes for this many VBlanks (3 s).
const STALL_VBLANKS: u32 = 180;

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut SLOT: [[u32; SLOT_WORDS]; SLOTS] = [[0; SLOT_WORDS]; SLOTS];
static mut RLE: [u32; RLE_WORDS] = [0; RLE_WORDS];
static mut COLUMN: [u32; COLUMN_WORDS] = [0; COLUMN_WORDS];

/// Where the CPU time goes, sampled from root counter 2 (system clock / 8)
/// at every pump and phase change. Each sample interval stays well under
/// the counter's 65536 * 8-cycle wrap because the pump runs at least once
/// per decoded column.
const PHASE_VLC: usize = 0;
const PHASE_MDEC: usize = 1;
const PHASE_WAIT: usize = 2;
static mut PHASE: usize = PHASE_WAIT;
static mut LAST: u16 = 0;
static mut ACC: [u32; 3] = [0; 3];

fn clock(next: Option<usize>) {
    use psx_io::timers::{counter, Timer};
    // SAFETY: single-threaded profiling statics.
    unsafe {
        let now = counter(Timer::Timer2);
        let phase = *addr_of_mut!(PHASE);
        (*addr_of_mut!(ACC))[phase] += now.wrapping_sub(*addr_of_mut!(LAST)) as u32;
        *addr_of_mut!(LAST) = now;
        if let Some(n) = next {
            *addr_of_mut!(PHASE) = n;
        }
    }
}

/// Stream state the pump owns. Slot buffers live in `SLOT`; the pump only
/// ever writes the `filling` slot, and the decoder only reads `decoding`.
struct Stream {
    sectors_left: u32,
    sectors: u32,
    filling: usize,
    ready: Option<(usize, u32)>,
    decoding: Option<usize>,
    asm: FrameAssembler,
    late: u32,
    cd_errors: u32,
    last_frame_vblank: u32,
}

fn slot_bytes(i: usize) -> &'static mut [u8] {
    // SAFETY: slots are disjoint statics; callers keep the pump's filling
    // slot and the decoder's slot distinct (see `Stream`).
    unsafe {
        let p = addr_of_mut!(SLOT[i]) as *mut u8;
        core::slice::from_raw_parts_mut(p, SLOT_WORDS * 4)
    }
}

impl Stream {
    /// Drain every sector the drive has ready.
    fn pump(&mut self) {
        clock(None);
        while self.sectors_left > 0 {
            // SAFETY: single-threaded; READER/SECTOR are only used here.
            let got =
                unsafe { (*addr_of_mut!(READER)).try_read_sector(&mut *addr_of_mut!(SECTOR)) };
            match got {
                Ok(true) => {}
                Ok(false) => return,
                Err(()) => {
                    self.cd_errors += 1;
                    self.sectors_left = 0;
                    return;
                }
            }
            self.sectors += 1;
            self.sectors_left -= 1;
            // SAFETY: SECTOR is a plain word buffer, viewed as bytes.
            let sector = unsafe {
                core::slice::from_raw_parts(addr_of_mut!(SECTOR) as *const u8, SECTOR_WORDS * 4)
            };
            if let Some(frame) = self.asm.add(sector, slot_bytes(self.filling)) {
                self.last_frame_vblank = interrupts::vblank_count();
                let done = self.filling;
                let next = match self.ready.replace((done, frame.size)) {
                    // The queued frame was never shown: skip it, reuse its slot.
                    Some((stale, _)) => {
                        self.late += 1;
                        stale
                    }
                    None => (0..SLOTS)
                        .find(|&s| s != done && Some(s) != self.decoding)
                        .unwrap_or(done),
                };
                self.filling = next;
            }
        }
    }
}

fn start_stream(lba: u32) -> bool {
    // SAFETY: the reader was prepared in main.
    unsafe { (*addr_of_mut!(READER)).start_read(lba) }
}

/// Read one sector synchronously (directory lookups before streaming).
fn read_one(lba: u32) -> Option<&'static [u8]> {
    // SAFETY: single-threaded use of the reader and its sector buffer.
    unsafe {
        let r = &mut *addr_of_mut!(READER);
        if !r.start_read(lba) {
            return None;
        }
        let ok = r.read_sector(&mut *addr_of_mut!(SECTOR));
        r.stop();
        if !ok {
            return None;
        }
        Some(core::slice::from_raw_parts(
            addr_of_mut!(SECTOR) as *const u8,
            SECTOR_WORDS * 4,
        ))
    }
}

fn find_movie() -> Option<(u32, u32)> {
    let (root, _) = iso::root_directory(read_one(iso::PVD_LBA)?)?;
    iso::find_in_directory(read_one(root)?, MOVIE)
}

fn print_num(label: &str, v: u32) {
    let mut buf = [0u8; 10];
    let mut n = v;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    tty::print(" ");
    tty::print(label);
    tty::print("=");
    tty::print(core::str::from_utf8(&buf[i..]).unwrap_or("?"));
}

fn fail(what: &str) -> ! {
    tty::print("FMV FAIL ");
    tty::println(what);
    gpu::fill_rect(0, 0, WIDTH, HEIGHT, 160, 0, 0);
    loop {
        interrupts::wait_vblank();
    }
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    gpu::fill_rect(0, 0, WIDTH, 512, 0, 0, 0);

    // SAFETY: first and only prepare; nothing else drives the CD.
    if !unsafe { (*addr_of_mut!(READER)).prepare() } {
        fail("cd prepare");
    }
    let Some((lba, size)) = find_movie() else {
        fail("MOVIE.STR not found");
    };
    mdec::reset();
    if !mdec::load_tables() {
        fail("mdec tables");
    }

    let mut st = Stream {
        sectors_left: size / 2048,
        sectors: 0,
        filling: 0,
        ready: None,
        decoding: None,
        asm: FrameAssembler::new(),
        late: 0,
        cd_errors: 0,
        last_frame_vblank: interrupts::vblank_count(),
    };
    if !start_stream(lba) {
        fail("cd start");
    }

    let mut shown = 0u32;
    let mut errors = 0u32;
    let mut back_y: u16 = 256;
    psx_io::timers::set_mode(psx_io::timers::Timer::Timer2, 0x0200);
    clock(Some(PHASE_WAIT));
    let start = interrupts::vblank_count();
    let mut next_flip = start;
    // SAFETY: RLE is only touched by this loop.
    let rle = unsafe { &mut *addr_of_mut!(RLE) };
    let rle16 =
        unsafe { core::slice::from_raw_parts_mut(rle.as_mut_ptr() as *mut u16, RLE_WORDS * 2) };

    loop {
        st.pump();
        let Some((slot, bytes)) = st.ready.take() else {
            let idle = interrupts::vblank_count().wrapping_sub(st.last_frame_vblank);
            if st.sectors_left == 0 || idle > STALL_VBLANKS {
                break;
            }
            continue;
        };
        clock(Some(PHASE_VLC));
        st.decoding = Some(slot);
        let frame = &slot_bytes(slot)[..(bytes as usize).min(SLOT_WORDS * 4)];
        let decoded =
            bs::decode_frame(frame, rle16, COLUMNS as u32 * ROWS, ROWS, &mut || st.pump());
        let words = match decoded {
            Ok(w) => w,
            Err(_) => {
                errors += 1;
                st.decoding = None;
                continue;
            }
        };
        st.decoding = None; // the bitstream is fully consumed
        clock(Some(PHASE_MDEC));
        // SAFETY: RLE stays untouched until decode_finish below.
        unsafe { mdec::decode_start(rle, words, mdec::DECODE_15BPP) };
        // SAFETY: COLUMN is only used here.
        let column = unsafe { &mut *addr_of_mut!(COLUMN) };
        let mut ok = true;
        for c in 0..COLUMNS {
            if !mdec::read_column(column) {
                ok = false;
                break;
            }
            psx_vram::upload_words(VramRect::new(c * 16, back_y, 16, HEIGHT), column);
            st.pump();
        }
        if !mdec::decode_finish() || !ok {
            errors += 1;
            mdec::reset();
            let _ = mdec::load_tables();
            continue;
        }
        clock(Some(PHASE_WAIT));
        // Pace to 15 fps, then flip at the VBlank.
        while (interrupts::vblank_count().wrapping_sub(next_flip) as i32) < 0 {
            st.pump();
        }
        interrupts::wait_vblank();
        psx_io::gpu::write_gp1(0x0500_0000 | ((back_y as u32) << 10));
        next_flip = interrupts::vblank_count().wrapping_add(VBLANKS_PER_FRAME - 1);
        back_y = if back_y == 0 { 256 } else { 0 };
        shown += 1;
    }
    // SAFETY: stop the stream we started.
    unsafe { (*addr_of_mut!(READER)).stop() };
    let vblanks = interrupts::vblank_count().wrapping_sub(start);

    let pass = shown > 0 && errors == 0 && st.cd_errors == 0 && st.asm.dropped == 0;
    tty::print(if pass { "FMV PASS" } else { "FMV FAIL" });
    print_num("frames", shown);
    print_num("late", st.late);
    print_num("lost", st.asm.dropped);
    print_num("errors", errors);
    print_num("cd_errors", st.cd_errors);
    print_num("sectors", st.sectors);
    print_num("vblanks", vblanks);
    // Average kilocycles per decoded frame in each phase.
    let acc = unsafe { *addr_of_mut!(ACC) };
    let per = (shown + st.late).max(1);
    print_num("kcyc_vlc", acc[PHASE_VLC] / per * 8 / 1000);
    print_num("kcyc_mdec_upload", acc[PHASE_MDEC] / per * 8 / 1000);
    print_num("kcyc_wait", acc[PHASE_WAIT] / per * 8 / 1000);
    tty::println("");
    loop {
        interrupts::wait_vblank();
    }
}
