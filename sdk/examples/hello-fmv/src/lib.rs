//! `hello-fmv` -- FMV console test: stream a 2x STR with XA audio, check
//! every sector, and show the counts over the video.
//!
//! The disc carries `MOVIE.STR`, built by `tools/fmv_test_movie.py` from
//! synthetic content only: a 320x240 15 fps test pattern with temporal
//! noise (so every frame fills the 2x sector budget) and interleaved XA
//! stereo beeps, one per second. Each video sector carries its ordinal,
//! the file's video-sector total, its index in the file and a checksum
//! (see the script), so the player can tell a skipped sector (LOST) from a
//! corrupt one (BAD) while the drive streams for 75 seconds.
//!
//! Pipeline per frame:
//!
//! 1. The drive streams at double speed with XA-ADPCM on and the file 1 /
//!    channel 0 filter set: audio sectors go straight to the SPU, video
//!    sectors arrive as 2048-byte data. A polled pump drains every ready
//!    sector (checksum, ordinal gap check) into a three-slot frame ring
//!    and runs between every unit of work, including every wait.
//! 2. `psx_fmv::bs::decode_frame` runs the bitstream decode on the CPU.
//! 3. The MDEC decodes at 15bpp (DMA0 in); each 16-pixel column comes back
//!    over DMA1 and goes to the back buffer.
//! 4. The overlay is drawn over the frame, then the display flips at a
//!    VBlank, paced to 15 fps.
//!
//! A frame completed while an older one is still queued replaces it (LATE:
//! the decoder is slower than the stream, which costs smoothness, not
//! data). When the stream ends a summary screen stays up. The same numbers
//! go to the TTY as one `FMV PASS ...` / `FMV FAIL ...` line.
//!
//! [`run`] is the whole test and returns once the summary is on screen, so
//! the same player runs from this example's own boot (`src/bin.rs`) and from
//! the hardware-test suite's menu. The caller owns the VBlank counter: it
//! must already be installed, because reinstalling it would reset a count
//! the caller may be pacing on.

#![no_std]

use core::ptr::addr_of_mut;
use psx_fmv::{bs, iso, mdec, str::FrameAssembler};
use psx_font::{
    fonts::{basic::BASIC, basic_8x16::BASIC_8X16_BITMAP},
    BitOrder, BitmapFont, FontAtlas,
};
use psx_gpu::{self as gpu, Resolution, VideoMode};
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::{interrupts, tty};
use psx_spu::{self as spu, CdVolume, Volume};
use psx_vram::{Clut, TexDepth, Tpage, VramRect};

const MOVIE: &str = "MOVIE.STR";
/// Double speed, XA-ADPCM to the SPU, file/channel filter.
const CD_MODE: u8 = 0x80 | 0x40 | 0x08;
const XA_FILE: u8 = 1;
const XA_CHANNEL: u8 = 0;

const WIDTH: u16 = 320;
const HEIGHT: u16 = 240;
const COLUMNS: u16 = WIDTH / 16;
const ROWS: u32 = HEIGHT as u32 / 16;
/// 15bpp column of 16 x HEIGHT pixels, two per word.
const COLUMN_WORDS: usize = 8 * HEIGHT as usize;
/// VBlanks per movie frame: 60 Hz / 15 fps.
const VBLANKS_PER_FRAME: u32 = 4;
/// Frame slot: 16 chunks of 2016 bytes (a 2x 15 fps frame is 8 to 10).
const SLOT_WORDS: usize = 16 * 2016 / 4;
const SLOTS: usize = 3;
/// MDEC run-length buffer: 64K halfwords.
const RLE_WORDS: usize = 32 * 1024;
/// End the stream if no sector arrives for this long (2 s).
pub const STALL_VBLANKS: u32 = 120;
/// Checksum seed; must match tools/fmv_test_movie.py.
const SEED: u32 = 0x9E37_79B9;
/// First word of every STR video sector: 0x0160, 0x8001.
const STR_MAGIC: u32 = 0x8001_0160;

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const SMALL_TPAGE: Tpage = Tpage::new(384, 0, TexDepth::Bit4);
const SMALL_CLUT: Clut = Clut::new(336, 256);
/// Left margin: keeps text inside a CRT's overscan-safe area.
const X0: i16 = 16;

/// The 8x16 VGA font with every column doubled: 16x16 glyphs that the
/// 1:1 textured-rectangle path draws exactly, big enough to read on a
/// filmed TV. (Scaled quads interpolate UVs; a rect blit cannot bleed.)
static WIDE_BITMAP: [u8; 128 * 16 * 2] = widen(&BASIC_8X16_BITMAP);
static WIDE: BitmapFont = BitmapFont {
    glyph_w: 16,
    glyph_h: 16,
    first_char: 0,
    glyph_count: 128,
    bitmap: &WIDE_BITMAP,
    glyph_advances: None,
    advance_x: 16,
    line_height: 16,
    bit_order: BitOrder::Msb,
};

const fn widen(src: &[u8; 2048]) -> [u8; 4096] {
    let mut out = [0u8; 4096];
    let mut i = 0;
    while i < 2048 {
        let b = src[i];
        let mut w: u16 = 0;
        let mut bit = 0;
        while bit < 8 {
            if b & (0x80 >> bit) != 0 {
                w |= 0xC000 >> (2 * bit);
            }
            bit += 1;
        }
        out[2 * i] = (w >> 8) as u8;
        out[2 * i + 1] = w as u8;
        i += 1;
    }
    out
}
const WHITE: (u8, u8, u8) = (220, 220, 220);
const GREEN: (u8, u8, u8) = (60, 220, 90);
const RED: (u8, u8, u8) = (240, 60, 60);
const YELLOW: (u8, u8, u8) = (230, 210, 60);

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut SLOT: [[u32; SLOT_WORDS]; SLOTS] = [[0; SLOT_WORDS]; SLOTS];
static mut RLE: [u32; RLE_WORDS] = [0; RLE_WORDS];
static mut COLUMN: [u32; COLUMN_WORDS] = [0; COLUMN_WORDS];

/// Where the CPU time goes, sampled from root counter 2 (system clock / 8)
/// at every pump and phase change. Each interval stays under the counter's
/// 65536 * 8-cycle wrap because the pump runs at least once per column.
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
    file_lba: u32,
    done: bool,
    /// Video sectors that passed the checks.
    good: u32,
    /// Video sectors in the file (from the sector headers).
    total: u32,
    /// Next expected video ordinal.
    expected: u32,
    /// Ordinals skipped (never delivered).
    lost: u32,
    /// Sectors with a bad magic or checksum, or repeated / out of order.
    bad: u32,
    /// Absolute LBA of the last good sector.
    lba: u32,
    /// `lba` when the first LOST, BAD or drive error was seen.
    first_err_lba: Option<u32>,
    filling: usize,
    ready: Option<(usize, u32)>,
    decoding: Option<usize>,
    asm: FrameAssembler,
    late: u32,
    cd_errors: u32,
    last_sector_vblank: u32,
    /// Stop after this many video sectors (0: the whole file).
    cut: u32,
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
        while !self.done {
            // SAFETY: single-threaded; READER/SECTOR are only used here.
            let got =
                unsafe { (*addr_of_mut!(READER)).try_read_sector(&mut *addr_of_mut!(SECTOR)) };
            match got {
                Ok(true) => {}
                Ok(false) => return,
                Err(()) => {
                    self.cd_errors += 1;
                    self.first_err_lba.get_or_insert(self.lba);
                    self.done = true;
                    return;
                }
            }
            self.last_sector_vblank = interrupts::vblank_count();
            // SAFETY: SECTOR is a plain word buffer.
            let words = unsafe { &*addr_of_mut!(SECTOR) };
            if !self.check(words) {
                continue;
            }
            // SAFETY: the same buffer viewed as bytes.
            let sector = unsafe {
                core::slice::from_raw_parts(words.as_ptr() as *const u8, SECTOR_WORDS * 4)
            };
            if let Some(frame) = self.asm.add(sector, slot_bytes(self.filling)) {
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

    /// Validate one video sector's test fields. `false` drops it.
    fn check(&mut self, w: &[u32; SECTOR_WORDS]) -> bool {
        let ordinal = w[5] & 0xFFFF;
        let mut h = SEED ^ ordinal;
        for &word in &w[8..] {
            h = h.rotate_left(5).wrapping_add(word);
        }
        if w[0] != STR_MAGIC || h != w[7] || ordinal < self.expected {
            self.bad += 1;
            self.first_err_lba.get_or_insert(self.lba);
            return false;
        }
        if ordinal > self.expected {
            self.lost += ordinal - self.expected;
            self.first_err_lba.get_or_insert(self.lba);
        }
        self.expected = ordinal + 1;
        self.total = w[5] >> 16;
        if self.cut != 0 && self.cut < self.total {
            self.total = self.cut;
        }
        self.lba = self.file_lba + w[6];
        self.good += 1;
        if self.expected >= self.total {
            self.done = true;
        }
        true
    }
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

/// Small fixed text buffer for overlay lines.
struct Text {
    buf: [u8; 40],
    len: usize,
}

impl Text {
    fn new() -> Self {
        Text {
            buf: [b' '; 40],
            len: 0,
        }
    }
    fn s(mut self, s: &str) -> Self {
        for &b in s.as_bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }
    /// Decimal, zero-padded to `width` digits.
    fn n(mut self, v: u32, width: usize) -> Self {
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut x = v;
        loop {
            digits[count] = b'0' + (x % 10) as u8;
            count += 1;
            x /= 10;
            if x == 0 {
                break;
            }
        }
        while count < width {
            digits[count] = b'0';
            count += 1;
        }
        for i in (0..count).rev() {
            if self.len < self.buf.len() {
                self.buf[self.len] = digits[i];
                self.len += 1;
            }
        }
        self
    }
    /// `mm:ss` from VBlanks (NTSC, 60 per second).
    fn time(self, vblanks: u32) -> Self {
        let secs = vblanks / 60;
        self.n(secs / 60, 2).s(":").n(secs % 60, 2)
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("?")
    }
}

fn print_num(label: &str, v: u32) {
    tty::print(" ");
    tty::print(label);
    tty::print("=");
    tty::print(Text::new().n(v, 1).as_str());
}

/// Point GPU drawing at the 320x240 buffer starting at VRAM row `y`.
fn target(y: u16) {
    gpu::set_draw_area(0, y, WIDTH - 1, y + HEIGHT - 1);
    gpu::set_draw_offset(0, y as i16);
}

/// Live counters, drawn over the bottom of the video frame.
fn draw_overlay(
    font: &FontAtlas,
    y: u16,
    st: &Stream,
    shown: u32,
    vblanks: u32,
    label: &str,
) {
    gpu::fill_rect(0, y + 172, WIDTH, 68, 0, 0, 0);
    target(y);
    let l1 = Text::new().s("FR ").n(shown, 4).s("  LATE ").n(st.late, 4);
    let l2 = Text::new().s("LOST ").n(st.lost, 4).s(" BAD ").n(st.bad, 4);
    let l3 = Text::new().s("LBA ").n(st.lba, 6).s("  ").time(vblanks);
    let health = if st.lost == 0 && st.bad == 0 && st.cd_errors == 0 {
        GREEN
    } else {
        RED
    };
    font.draw_text(X0, 176, l1.as_str(), WHITE);
    font.draw_text(X0, 194, l2.as_str(), health);
    font.draw_text(X0, 212, l3.as_str(), WHITE);
    if !label.is_empty() {
        // A band over the top of the picture: the counters fill the bottom.
        gpu::fill_rect(0, y, WIDTH, 20, 0, 0, 0);
        font.draw_text(X0, 2, label, YELLOW);
    }
}

struct Summary {
    pass: bool,
    shown: u32,
    dropped: u32,
    errors: u32,
    vblanks: u32,
    kcyc: [u32; 3],
}

/// Everything the summary screen shows, for a caller that records it.
#[derive(Copy, Clone, Debug, Default)]
pub struct Outcome {
    /// The PASS criteria: every video sector arrived intact and in order,
    /// every frame decoded, and the drive reported no error.
    pub pass: bool,
    /// Set when the test could not start (drive, file or MDEC setup); the
    /// counters below are then all zero.
    pub setup_error: Option<&'static str>,
    /// Video sectors that passed their checks, and the file's total.
    pub good: u32,
    pub total: u32,
    /// Video sectors never delivered, and ones delivered corrupt, repeated
    /// or out of order.
    pub lost: u32,
    pub bad: u32,
    /// Frames lost to bad sectors.
    pub dropped: u32,
    /// Drive error IRQs, and MDEC or bitstream failures.
    pub cd_errors: u32,
    pub decode_errors: u32,
    /// Frames shown, and frames skipped because the decoder was still busy
    /// (informational: CPU speed, not the disc).
    pub shown: u32,
    pub late: u32,
    /// Stream duration in VBlanks.
    pub vblanks: u32,
    /// Absolute LBA of the last good sector, and of the first problem.
    pub lba: u32,
    pub first_err_lba: Option<u32>,
    /// CPU cost per frame in kilocycles: bitstream decode, MDEC plus
    /// upload, and waiting.
    pub kcyc_vlc: u32,
    pub kcyc_mdec: u32,
    pub kcyc_wait: u32,
    /// Why the stream ended.
    pub stop: Stop,
    /// Video sectors the run was cut at (`Options::max_sectors`), or the
    /// file's total for a whole-movie run.
    pub target: u32,
}

/// Why a run ended.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Stop {
    /// Every video sector of the run arrived (the file's, or the cut's).
    #[default]
    End = 0,
    /// No sector for [`STALL_VBLANKS`]: the drive stopped delivering.
    Stall = 1,
    /// The MDEC failed [`WEDGE_ERRORS`] frames in a row.
    Wedged = 2,
    /// The drive reported an error.
    CdError = 3,
    /// Setup failed; nothing streamed.
    Setup = 4,
}

/// How a run is set up and how long it lasts.
#[derive(Copy, Clone)]
pub struct Options {
    /// MDEC setup (reset, tables, DMA requests), run at the start and again
    /// after a decode error; `false` when DMA0 decodes cannot run.
    pub setup: fn() -> bool,
    /// Stop after this many video sectors; 0 plays the whole file. The pass
    /// criteria then apply to the cut: every sector up to it intact.
    pub max_sectors: u32,
    /// Shown on the overlay and the summary, so a filmed run says which
    /// setup it used. Up to 12 characters; "" for none.
    pub label: &'static str,
}

impl Options {
    /// The whole movie behind the SDK's own setup.
    pub const DEFAULT: Options = Options {
        setup: mdec_setup,
        max_sectors: 0,
        label: "",
    };
}

/// Consecutive MDEC failures that end a run as [`Stop::Wedged`].
pub const WEDGE_ERRORS: u32 = 8;

/// Final screen: stays up after the stream ends.
fn draw_summary(font: &FontAtlas, small: &FontAtlas, st: &Stream, s: &Summary, label: &str) {
    gpu::fill_rect(0, 0, WIDTH, HEIGHT, 0, 0, 0);
    target(0);
    let verdict = if s.pass { "PASS" } else { "FAIL" };
    let tint = if s.pass { GREEN } else { RED };
    font.draw_text(32, 12, "FMV CONSOLE TEST", WHITE);
    small.draw_text(X0, 2, label, YELLOW);
    // Verdict banner: a solid block reads from across the room.
    gpu::fill_rect(16, 32, 288, 32, tint.0, tint.1, tint.2);
    let ink = if s.pass { (0, 0, 0) } else { (255, 255, 255) };
    font.draw_text(64, 40, Text::new().s("RESULT: ").s(verdict).as_str(), ink);
    let lines = [
        (
            Text::new().s("SECT ").n(st.good, 1).s("/").n(st.total, 1),
            if st.good == st.total && st.total > 0 {
                GREEN
            } else {
                RED
            },
        ),
        (
            Text::new()
                .s("LOST ")
                .n(st.lost, 1)
                .s("  BAD ")
                .n(st.bad, 1),
            if st.lost == 0 && st.bad == 0 {
                GREEN
            } else {
                RED
            },
        ),
        (
            Text::new()
                .s("FRAMES ")
                .n(s.shown + st.late, 1)
                .s(" DROP ")
                .n(s.dropped, 1),
            if s.dropped == 0 { GREEN } else { RED },
        ),
        (
            Text::new()
                .s("SHW ")
                .n(s.shown, 4)
                .s(" LATE ")
                .n(st.late, 4),
            YELLOW,
        ),
        (
            Text::new()
                .s("CDERR ")
                .n(st.cd_errors, 1)
                .s(" DECERR ")
                .n(s.errors, 1),
            if st.cd_errors == 0 && s.errors == 0 {
                GREEN
            } else {
                RED
            },
        ),
        (
            Text::new().s("T ").time(s.vblanks).s(" LBA ").n(st.lba, 6),
            WHITE,
        ),
        match st.first_err_lba {
            None => (Text::new().s("ERR NEAR NONE"), GREEN),
            Some(l) => (Text::new().s("ERR NEAR ").n(l, 6), RED),
        },
    ];
    let mut y = 74;
    for (text, tint) in &lines {
        font.draw_text(X0, y, text.as_str(), *tint);
        y += 18;
    }
    let small_line = Text::new()
        .s("KCYC VLC ")
        .n(s.kcyc[PHASE_VLC], 1)
        .s(" MDEC ")
        .n(s.kcyc[PHASE_MDEC], 1)
        .s(" WAIT ")
        .n(s.kcyc[PHASE_WAIT], 1);
    small.draw_text(X0, 204, small_line.as_str(), WHITE);
    small.draw_text(X0, 214, "LATE: DECODER SLOW, NOT A FAIL", WHITE);
    gpu::draw_sync();
    psx_io::gpu::write_gp1(0x0500_0000);
}

/// The test could not start: a red screen, and the reason on the TTY.
fn fail(what: &'static str) -> Outcome {
    tty::print("FMV FAIL ");
    tty::println(what);
    gpu::fill_rect(0, 0, WIDTH, HEIGHT, 160, 0, 0);
    psx_io::gpu::write_gp1(0x0500_0000);
    Outcome {
        setup_error: Some(what),
        stop: Stop::Setup,
        ..Outcome::default()
    }
}

/// Wait for the next VBlank edge while keeping the drive drained.
fn wait_vblank_pumping(st: &mut Stream) {
    let start = interrupts::vblank_count();
    while interrupts::vblank_count() == start {
        st.pump();
    }
}

/// The SDK's MDEC setup: reset, then the tables. `false` unless DMA0 can
/// feed the decoder afterwards, which every frame needs.
pub fn mdec_setup() -> bool {
    mdec::reset() && mdec::load_tables().is_some_and(|t| t.enable_writes != 0)
}

/// Run the whole test: stream, check, and leave the summary (or, when the
/// test could not start, a red screen) on the displayed buffer at VRAM row 0.
/// Takes over the GPU, SPU, CD drive, MDEC and root counter 2; a caller that
/// carries on afterwards restores what it needs.
pub fn run() -> Outcome {
    run_with(Options::DEFAULT)
}

/// [`run`] with the caller's MDEC setup and length. The hardware-test suite
/// plays a short cut behind each of its diagnostic sequences that worked.
pub fn run_with(options: Options) -> Outcome {
    let setup = options.setup;
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    gpu::fill_rect(0, 0, WIDTH, 512, 0, 0, 0);
    let font = FontAtlas::upload(&WIDE, FONT_TPAGE, FONT_CLUT);
    let small = FontAtlas::upload(&BASIC, SMALL_TPAGE, SMALL_CLUT);

    spu::init();
    spu::set_main_volume(Volume::MAX, Volume::MAX);
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(true);

    // SAFETY: nothing else drives the CD while the test runs; prepare also
    // takes the drive over from whatever used it before.
    if !unsafe { (*addr_of_mut!(READER)).prepare() } {
        return fail("cd prepare");
    }
    let Some((lba, _size)) = find_movie() else {
        return fail("MOVIE.STR not found");
    };
    // SAFETY: the reader is prepared and idle. Demute first: a muted drive
    // plays no XA, and the program that ran before may have left it muted
    // (the hardware-test CD battery does).
    let xa_ok = unsafe {
        let r = &mut *addr_of_mut!(READER);
        r.demute() && r.prepare_mode(CD_MODE) && r.set_filter(XA_FILE, XA_CHANNEL)
    };
    if !xa_ok {
        return fail("cd xa mode");
    }
    psx_io::cdrom::set_audio_mixer(0x80, 0, 0x80, 0);
    if !setup() {
        return fail("mdec tables");
    }

    let mut st = Stream {
        file_lba: lba,
        done: false,
        good: 0,
        total: 0,
        expected: 0,
        lost: 0,
        bad: 0,
        lba,
        first_err_lba: None,
        filling: 0,
        ready: None,
        decoding: None,
        asm: FrameAssembler::new(),
        late: 0,
        cd_errors: 0,
        last_sector_vblank: interrupts::vblank_count(),
        cut: options.max_sectors,
    };
    // SAFETY: the reader was prepared above.
    if !unsafe { (*addr_of_mut!(READER)).start_read(lba) } {
        return fail("cd start");
    }

    let mut shown = 0u32;
    let mut errors = 0u32;
    let mut in_a_row = 0u32;
    let mut wedged = false;
    let mut back_y: u16 = 256;
    psx_io::timers::set_mode(psx_io::timers::Timer::Timer2, 0x0200);
    clock(Some(PHASE_WAIT));
    // SAFETY: single-threaded profiling statics; a second run from a menu
    // starts its profile from zero.
    unsafe { *addr_of_mut!(ACC) = [0; 3] };
    let start = interrupts::vblank_count();
    let mut next_flip = start;
    // SAFETY: RLE is only touched by this loop.
    let rle = unsafe { &mut *addr_of_mut!(RLE) };
    let rle16 =
        unsafe { core::slice::from_raw_parts_mut(rle.as_mut_ptr() as *mut u16, RLE_WORDS * 2) };

    loop {
        st.pump();
        let Some((slot, bytes)) = st.ready.take() else {
            let idle = interrupts::vblank_count().wrapping_sub(st.last_sector_vblank);
            if st.done || idle > STALL_VBLANKS {
                break;
            }
            continue;
        };
        clock(Some(PHASE_VLC));
        st.decoding = Some(slot);
        let frame = &slot_bytes(slot)[..(bytes as usize).min(SLOT_WORDS * 4)];
        let decoded =
            bs::decode_frame(frame, rle16, COLUMNS as u32 * ROWS, ROWS, &mut || st.pump());
        st.decoding = None; // the bitstream is fully consumed
        let words = match decoded {
            Ok(w) => w,
            Err(_) => {
                errors += 1;
                continue;
            }
        };
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
            in_a_row += 1;
            if in_a_row >= WEDGE_ERRORS {
                wedged = true;
                break;
            }
            let _ = setup();
            continue;
        }
        in_a_row = 0;
        let elapsed = interrupts::vblank_count().wrapping_sub(start);
        draw_overlay(&font, back_y, &st, shown + 1, elapsed, options.label);
        gpu::draw_sync();
        clock(Some(PHASE_WAIT));
        // Pace to 15 fps, then flip at the VBlank; the drive keeps draining.
        while (interrupts::vblank_count().wrapping_sub(next_flip) as i32) < 0 {
            st.pump();
        }
        wait_vblank_pumping(&mut st);
        psx_io::gpu::write_gp1(0x0500_0000 | ((back_y as u32) << 10));
        next_flip = interrupts::vblank_count().wrapping_add(VBLANKS_PER_FRAME - 1);
        back_y = if back_y == 0 { 256 } else { 0 };
        shown += 1;
    }
    // SAFETY: stop the stream we started (also ends the XA audio).
    unsafe { (*addr_of_mut!(READER)).stop() };
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    let stop = if wedged {
        Stop::Wedged
    } else if st.cd_errors != 0 {
        Stop::CdError
    } else if st.done {
        Stop::End
    } else {
        Stop::Stall
    };
    if st.expected < st.total {
        st.lost += st.total - st.expected;
        st.first_err_lba.get_or_insert(st.lba);
    }

    let acc = unsafe { *addr_of_mut!(ACC) };
    let per = (shown + st.late).max(1);
    let summary = Summary {
        pass: st.total > 0
            && st.good == st.total
            && st.lost == 0
            && st.bad == 0
            && st.cd_errors == 0
            && errors == 0
            && st.asm.dropped == 0,
        shown,
        dropped: st.asm.dropped,
        errors,
        vblanks,
        kcyc: [
            acc[PHASE_VLC] / per * 8 / 1000,
            acc[PHASE_MDEC] / per * 8 / 1000,
            acc[PHASE_WAIT] / per * 8 / 1000,
        ],
    };
    tty::print(if summary.pass { "FMV PASS" } else { "FMV FAIL" });
    print_num("sectors", st.good);
    print_num("total", st.total);
    print_num("lost", st.lost);
    print_num("bad", st.bad);
    print_num("frames", shown);
    print_num("late", st.late);
    print_num("dropped", st.asm.dropped);
    print_num("errors", errors);
    print_num("cd_errors", st.cd_errors);
    print_num("vblanks", vblanks);
    print_num("kcyc_vlc", summary.kcyc[PHASE_VLC]);
    print_num("kcyc_mdec_upload", summary.kcyc[PHASE_MDEC]);
    print_num("kcyc_wait", summary.kcyc[PHASE_WAIT]);
    print_num("stop", stop as u32);
    print_num("target", st.total);
    tty::println("");
    draw_summary(&font, &small, &st, &summary, options.label);
    Outcome {
        pass: summary.pass,
        setup_error: None,
        good: st.good,
        total: st.total,
        lost: st.lost,
        bad: st.bad,
        dropped: summary.dropped,
        cd_errors: st.cd_errors,
        decode_errors: summary.errors,
        shown: summary.shown,
        late: st.late,
        vblanks: summary.vblanks,
        lba: st.lba,
        first_err_lba: st.first_err_lba,
        kcyc_vlc: summary.kcyc[PHASE_VLC],
        kcyc_mdec: summary.kcyc[PHASE_MDEC],
        kcyc_wait: summary.kcyc[PHASE_WAIT],
        stop,
        target: st.total,
    }
}

/// A one-frame control decode: the movie's first frame through the MDEC
/// twice, once fed and drained by CPU writes and reads, once over DMA0 and
/// DMA1 as the player does. Equal sums mean both paths decode the same
/// pixels. The sums add the output words, so they do not depend on the
/// block order, which DMA1 rearranges and CPU reads do not.
#[derive(Copy, Clone, Debug, Default)]
pub struct FrameControl {
    /// The frame was read and its bitstream decoded.
    pub read: bool,
    /// Run-length words the MDEC was given, and output words expected.
    pub rle_words: u32,
    pub expected: u32,
    /// CPU path: finished, output words drained, their sum.
    pub cpu_ok: bool,
    pub cpu_words: u32,
    pub cpu_sum: u32,
    /// DMA path (behind `dma_setup`): finished, words, sum.
    pub dma_ok: bool,
    pub dma_words: u32,
    pub dma_sum: u32,
}

/// Spins without progress before the CPU decode gives up.
const CPU_STALL_SPINS: u32 = 100_000;

/// Run [`FrameControl`]. Takes the CD drive and the MDEC; `dma_setup` is the
/// MDEC setup for the DMA half.
pub fn frame_control(dma_setup: fn() -> bool) -> FrameControl {
    let mut out = FrameControl::default();
    // SAFETY: single-threaded use of the reader, its sector buffer and the
    // frame buffers, as in `run`.
    let reader = unsafe { &mut *addr_of_mut!(READER) };
    if !unsafe { reader.prepare() } {
        return out;
    }
    let Some((lba, _)) = find_movie() else {
        return out;
    };
    // Plain data reads: the XA audio sectors come back too and are skipped.
    if !unsafe { reader.start_read(lba) } {
        return out;
    }
    let mut asm = FrameAssembler::new();
    let mut frame = None;
    for _ in 0..48 {
        // SAFETY: as above.
        if !unsafe { reader.read_sector(&mut *addr_of_mut!(SECTOR)) } {
            break;
        }
        let words = unsafe { &*addr_of_mut!(SECTOR) };
        if words[0] != STR_MAGIC {
            continue;
        }
        let sector =
            unsafe { core::slice::from_raw_parts(words.as_ptr() as *const u8, SECTOR_WORDS * 4) };
        if let Some(f) = asm.add(sector, slot_bytes(0)) {
            frame = Some(f);
            break;
        }
    }
    unsafe { reader.stop() };
    let Some(frame) = frame else {
        return out;
    };
    let rle = unsafe { &mut *addr_of_mut!(RLE) };
    let rle16 =
        unsafe { core::slice::from_raw_parts_mut(rle.as_mut_ptr() as *mut u16, RLE_WORDS * 2) };
    let bytes = &slot_bytes(0)[..(frame.size as usize).min(SLOT_WORDS * 4)];
    let Ok(words) = bs::decode_frame(bytes, rle16, COLUMNS as u32 * ROWS, ROWS, &mut || {}) else {
        return out;
    };
    out.read = true;
    out.rle_words = words as u32;
    out.expected = COLUMNS as u32 * ROWS * 128;

    // CPU: feed while the input FIFO has room, drain whatever comes out.
    if mdec::reset() && mdec::load_tables_cpu() {
        mdec::write_command(mdec::DECODE_15BPP | (words as u32 & 0xFFFF));
        let mut fed = 0usize;
        let mut idle = 0u32;
        while (fed < words || out.cpu_words < out.expected) && idle < CPU_STALL_SPINS {
            let status = mdec::status();
            let mut progress = false;
            if fed < words && status & mdec::STATUS_IN_FULL == 0 {
                mdec::write_command(rle[fed]);
                fed += 1;
                progress = true;
            }
            if status & mdec::STATUS_OUT_EMPTY == 0 {
                out.cpu_sum = out.cpu_sum.wrapping_add(mdec::read_data());
                out.cpu_words += 1;
                progress = true;
            }
            idle = if progress { 0 } else { idle + 1 };
        }
        out.cpu_ok = fed == words && out.cpu_words == out.expected;
    }

    // DMA, the player's way.
    if dma_setup() {
        // SAFETY: RLE stays untouched until decode_finish.
        unsafe { mdec::decode_start(rle, words, mdec::DECODE_15BPP) };
        let column = unsafe { &mut *addr_of_mut!(COLUMN) };
        let mut ok = true;
        for _ in 0..COLUMNS {
            if !mdec::read_column(column) {
                ok = false;
                break;
            }
            for &w in column.iter() {
                out.dma_sum = out.dma_sum.wrapping_add(w);
            }
            out.dma_words += COLUMN_WORDS as u32;
        }
        out.dma_ok = mdec::decode_finish() && ok;
    }
    tty::print("FMV CONTROL");
    print_num("read", out.read as u32);
    print_num("rle_words", out.rle_words);
    print_num("cpu_ok", out.cpu_ok as u32);
    print_num("cpu_words", out.cpu_words);
    print_num("cpu_sum", out.cpu_sum);
    print_num("dma_ok", out.dma_ok as u32);
    print_num("dma_words", out.dma_words);
    print_num("dma_sum", out.dma_sum);
    tty::println("");
    // Leave the MDEC as the player expects to find it.
    let _ = mdec::reset();
    out
}
