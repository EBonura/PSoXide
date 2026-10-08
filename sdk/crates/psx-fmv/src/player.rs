// SPDX-License-Identifier: GPL-2.0-or-later
//! A streaming STR player a game can call: [`play`] streams one movie from
//! the disc at double speed, decodes it through the MDEC and shows it, with
//! its interleaved XA audio going straight from the drive to the SPU.
//!
//! The pipeline is the `hello-fmv` console test's, without the test's
//! sector stamps and overlay:
//!
//! 1. A polled pump drains every sector the drive has ready into a ring of
//!    three frame slots. It runs between every unit of work, including
//!    every wait and every two macroblocks of the bitstream decode.
//! 2. [`crate::bitstream::decode_frame`] turns the next frame's bitstream into one
//!    of two run-length buffers while the MDEC decodes the previous frame
//!    from the other.
//! 3. Decoded 16-pixel columns come back over DMA1 into one of two column
//!    buffers and go to a free display buffer over DMA2 while DMA1 fills
//!    the other.
//! 4. The flip is queued to the VBlank handler. There are three display
//!    buffers, so the MDEC can start the next frame while the last one
//!    waits for its flip.
//!
//! Pacing follows the drive, not the VBlank: a frame is shown
//! [`Config::latency_vblanks`] after its last sector arrived. The audio
//! plays as its sectors arrive, so picture and sound keep a fixed offset
//! for the whole movie whatever the display's refresh rate, and a 25 fps
//! PAL movie plays at 25 fps on a 60 Hz display. A frame completed while an
//! older one still waits for the decoder replaces it (counted in
//! [`Outcome::skipped`]).
//!
//! Memory comes from the caller ([`memory_words`] says how much), so a
//! game lends a buffer that is idle while the movie plays, such as a level
//! buffer at boot. The player takes over the CD drive, the MDEC, DMA
//! channels 0 to 2, root counter 2, the display and the VRAM under
//! [`Config::buffers`] while it runs; the SPU must already be initialised.

use core::ptr::addr_of_mut;

use psx_gpu::prim::FillRect;
use psx_gpu::Gpu;
use psx_hw::gpu::gp1;
use psx_hw::mdec::{DECODE_15BPP, DECODE_24BPP};
use psx_io::periph::GpuDma;
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
use psx_rt::interrupts;
use psx_spu::CdVolume;
use psx_vram::VramRect;

use crate::stream::{Chunk, FrameAssembler, CHUNK_PAYLOAD_BYTES, MAX_CHUNKS};
use crate::{bitstream, mdec};

/// One movie on the disc, as the build measured it.
#[derive(Copy, Clone, Debug)]
pub struct Movie {
    /// First sector of the file.
    pub lba: u32,
    /// Frames in the movie; the stream ends after the last.
    pub frames: u32,
    /// Picture width, a multiple of 16.
    pub width: u16,
    /// Picture height, a multiple of 16.
    pub height: u16,
    /// Most sectors any frame spans.
    pub max_chunks: u16,
    /// Most run-length words any frame decodes to
    /// ([`crate::bitstream::decode_frame`]'s result).
    pub rle_words: usize,
    /// XA audio file and channel to play, or `None` for a silent movie.
    pub xa: Option<(u8, u8)>,
}

/// Display buffers at (0, 0), (0, 256) and (512, 0): room for 320 pixels
/// at 24 bits (480 halfwords) by up to 256 lines each.
pub const DEFAULT_BUFFERS: [(u16, u16); 3] = [(0, 0), (0, 256), (512, 0)];

/// Colour depth of the decoded picture.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Depth {
    /// 15-bit: the GPU can still draw over the movie (subtitles, a skip
    /// prompt), at the price of 5-bit banding on smooth gradients.
    Rgb15,
    /// 24-bit, as most commercial players show their movies: no banding,
    /// 1.5 times the MDEC output and VRAM upload, and the display runs in
    /// 24-bit mode, where the GPU cannot draw. The player switches the
    /// display mode for the movie and restores it on return.
    Rgb24,
}

/// Where and how to show it.
#[derive(Copy, Clone, Debug)]
pub struct Config {
    /// Output depth.
    pub depth: Depth,
    /// VRAM origins (halfword x, line y) of the three display buffers, each
    /// `screen_width` pixels by `screen_height` lines; the movie is centred
    /// in them over black. [`DEFAULT_BUFFERS`] suits a 320-wide display.
    pub buffers: [(u16, u16); 3],
    /// The display's size.
    pub screen_width: u16,
    /// Display height.
    pub screen_height: u16,
    /// VBlanks from a frame's last sector to its flip: the decoder's
    /// headroom, and the fixed lag of the picture behind the sound.
    pub latency_vblanks: u32,
    /// End the stream if no sector arrives for this many VBlanks.
    pub stall_vblanks: u32,
}

/// Why [`play`] returned.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Stop {
    /// The last frame was shown.
    #[default]
    End,
    /// The caller's poll asked to stop.
    Skipped,
    /// No sector for [`Config::stall_vblanks`].
    Stall,
    /// The drive reported an error.
    CdError,
    /// The MDEC failed [`WEDGE_ERRORS`] frames in a row.
    Wedged,
    /// Setup failed (drive, MDEC or memory); nothing played.
    Setup,
}

/// What happened, for logs and tests.
#[derive(Copy, Clone, Debug, Default)]
pub struct Outcome {
    /// Why it ended.
    pub stop: Stop,
    /// Frames shown.
    pub shown: u32,
    /// Frames replaced before the decoder reached them.
    pub skipped: u32,
    /// Frames shown later than their due VBlank, and the worst lateness.
    pub late: u32,
    /// Most VBlanks any frame was shown after it was due.
    pub worst_late_vblanks: u32,
    /// Frames abandoned with sectors missing, and bitstream or MDEC
    /// failures.
    pub dropped: u32,
    /// Frames the bitstream decoder or the MDEC failed.
    pub decode_errors: u32,
    /// Number of the last complete frame the stream delivered.
    pub last_frame: u32,
    /// VBlanks from the start of the stream to the return.
    pub vblanks: u32,
    /// CPU per decoded frame, in kilocycles: bitstream decode, MDEC and
    /// upload, and waiting.
    pub kcyc_vlc: u32,
    /// MDEC and upload time not overlapped with the bitstream decode.
    pub kcyc_mdec: u32,
    /// Waiting for sectors or the flip.
    pub kcyc_wait: u32,
}

/// Consecutive MDEC failures that end a movie as [`Stop::Wedged`].
pub const WEDGE_ERRORS: u32 = 8;
/// Frame slots: one filling, one ready, one being decoded.
const SLOTS: usize = 3;
/// Words of bitstream one sector carries.
const CHUNK_WORDS: usize = CHUNK_PAYLOAD_BYTES / 4;
/// Longest a frame may take from its decode command to its last column in
/// VRAM before it counts as a decode error.
const DECODE_TIMEOUT_VBLANKS: u32 = 30;
/// Macroblocks between pumps inside the bitstream decode.
const PUMP_MACROBLOCKS: u32 = 2;
/// Double speed, XA-ADPCM to the SPU, file/channel filter.
const MODE_XA: u8 = 0x80 | 0x40 | 0x08;
/// Double speed, data only.
const MODE_DATA: u8 = 0x80;
/// Non-video sectors in a row that end the stream: past the file's end,
/// when the last frame never completed.
const FOREIGN_SECTORS: u32 = 32;
/// GP1(08h), display mode, and its 24-bit colour bit.
const GP1_DISPLAY_MODE: u32 = 0x0800_0000;
const DISPLAY_24BIT: u32 = 1 << 4;

/// The current GP1(08h) parameter, rebuilt from GPUSTAT bits 16..22.
fn display_mode() -> u32 {
    let stat = psx_io::gpu::status().bits();
    ((stat >> 17) & 3)
        | ((stat >> 19) & 1) << 2
        | ((stat >> 20) & 1) << 3
        | ((stat >> 21) & 1) << 4
        | ((stat >> 22) & 1) << 5
        | ((stat >> 16) & 1) << 6
        | ((stat >> 14) & 1) << 7
}

/// Display buffers: one shown, one decoded and waiting for its flip, one
/// the MDEC decodes into. With two, the MDEC would sit idle from a frame's
/// end until that frame's flip, which a 24-bit movie cannot afford.
const BUFFERS: usize = 3;

fn slot_words(m: &Movie) -> usize {
    m.max_chunks as usize * CHUNK_WORDS
}

/// Run-length buffer words: the measured maximum, rounded up to whole DMA
/// blocks.
fn rle_words(m: &Movie) -> usize {
    m.rle_words.div_ceil(mdec::DMA_BLOCK_WORDS) * mdec::DMA_BLOCK_WORDS
}

/// VRAM halfwords one decoded pixel takes.
fn halfwords_x2(depth: Depth) -> u16 {
    match depth {
        Depth::Rgb15 => 2,
        Depth::Rgb24 => 3,
    }
}

/// One column, 16 pixels wide: 16 * height pixels of 2 or 3 bytes.
fn column_words(m: &Movie, depth: Depth) -> usize {
    4 * halfwords_x2(depth) as usize * m.height as usize
}

/// Words of memory [`play`] needs for `movie` at `depth`.
pub fn memory_words(movie: &Movie, depth: Depth) -> usize {
    SLOTS * slot_words(movie) + 2 * rle_words(movie) + 2 * column_words(movie, depth)
}

/// Root counter 2 (system clock / 8) accounting of where the time goes.
const PHASE_VLC: usize = 0;
const PHASE_MDEC: usize = 1;
const PHASE_WAIT: usize = 2;

struct Clock {
    phase: usize,
    last: u16,
    acc: [u32; 3],
}

impl Clock {
    fn switch(&mut self, next: usize) {
        let now = psx_io::timers::counter(psx_io::timers::Timer::Timer2);
        self.acc[self.phase] += now.wrapping_sub(self.last) as u32;
        self.last = now;
        self.phase = next;
    }
}

/// Raw views of the caller's memory. The pieces are disjoint; the player
/// hands each to one user at a time (see `Stream` and `Player`).
#[derive(Copy, Clone)]
struct Buffers {
    slots: *mut u32,
    slot_words: usize,
    rle: *mut u32,
    rle_words: usize,
    columns: *mut u32,
    column_words: usize,
}

impl Buffers {
    fn slot(&self, i: usize) -> &'static mut [u8] {
        // SAFETY: slot `i` lies inside the caller's buffer (memory_words),
        // and the stream fills only the slot the decoder is not reading.
        unsafe {
            core::slice::from_raw_parts_mut(
                self.slots.add(i * self.slot_words) as *mut u8,
                self.slot_words * 4,
            )
        }
    }
    fn rle(&self, i: usize) -> &'static mut [u32] {
        // SAFETY: as above; the bitstream decoder writes one buffer while
        // DMA0 reads the other.
        unsafe { core::slice::from_raw_parts_mut(self.rle.add(i * self.rle_words), self.rle_words) }
    }
    fn rle16(&self, i: usize) -> &'static mut [u16] {
        let words = self.rle(i);
        // SAFETY: the same buffer as halfwords.
        unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u16, words.len() * 2) }
    }
    fn column(&self, i: usize) -> *mut u32 {
        // SAFETY: column `i` lies inside the caller's buffer.
        unsafe { self.columns.add(i * self.column_words) }
    }
}

static mut READER: SectorReader = SectorReader::new();
static mut SECTOR: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

fn reader() -> &'static mut SectorReader {
    // SAFETY: single-threaded; only the player uses its reader.
    unsafe { &mut *addr_of_mut!(READER) }
}

/// Read one sector synchronously into the player's sector buffer.
fn read_one(lba: u32) -> Option<&'static [u8]> {
    let r = reader();
    // SAFETY: single-threaded use of the reader and its sector buffer.
    unsafe {
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

/// Find a file in the disc's root directory by name: `(lba, bytes)`.
/// Takes the drive over (data mode, double speed) and leaves it stopped.
pub fn find_root_file(name: &str) -> Option<(u32, u32)> {
    // SAFETY: the caller is not using the drive; prepare takes it over.
    if !unsafe { reader().prepare() } {
        return None;
    }
    let (root, _) = crate::iso::root_directory(read_one(crate::iso::PVD_LBA)?)?;
    crate::iso::find_in_directory(read_one(root)?, name)
}

/// The drive side: sectors into frame slots.
struct Stream {
    buf: Buffers,
    last_frame: u32,
    frames: u32,
    done: bool,
    cd_error: bool,
    started: bool,
    /// Sectors in a row that were not video.
    foreign: u32,
    filling: usize,
    /// A complete frame waiting for the decoder: (slot, bytes, VBlank its
    /// last sector arrived).
    ready: Option<(usize, u32, u32)>,
    decoding: Option<usize>,
    asm: FrameAssembler,
    skipped: u32,
    last_sector_vblank: u32,
}

impl Stream {
    fn pump(&mut self) {
        while !self.done {
            // SAFETY: single-threaded use of the player's reader and sector.
            let got = unsafe { reader().try_read_sector(&mut *addr_of_mut!(SECTOR)) };
            match got {
                Ok(true) => {}
                Ok(false) => return,
                Err(()) => {
                    self.cd_error = true;
                    self.done = true;
                    return;
                }
            }
            let now = interrupts::vblank_count();
            self.last_sector_vblank = now;
            // SAFETY: SECTOR as bytes.
            let sector = unsafe {
                core::slice::from_raw_parts(addr_of_mut!(SECTOR) as *const u8, SECTOR_WORDS * 4)
            };
            if Chunk::parse(sector).is_none() {
                // A movie may carry empty data sectors in its audio slots
                // (WipEout's last seconds are silent), but a long run of
                // them means the drive has read past the file's end.
                self.foreign += 1;
                if self.started && self.foreign > FOREIGN_SECTORS {
                    self.done = true;
                }
                continue;
            }
            self.foreign = 0;
            self.started = true;
            if let Some(frame) = self.asm.add(sector, self.buf.slot(self.filling)) {
                self.last_frame = frame.number;
                if frame.number >= self.frames {
                    self.done = true;
                }
                let done = self.filling;
                let next = match self.ready.replace((done, frame.size, now)) {
                    Some((stale, _, _)) => {
                        self.skipped += 1;
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

/// The MDEC's current frame, one column at a time: DMA1 into a column
/// buffer, then DMA2 into the back buffer while DMA1 fills the other.
struct Columns {
    /// Left edge and column width, in VRAM halfwords.
    x: u16,
    step: u16,
    y: u16,
    count: u16,
    next: u16,
    reading: Option<(u16, usize)>,
    filled: Option<(u16, usize)>,
    uploading: Option<usize>,
    started: u32,
    /// DMA1 refused a column: the frame cannot complete.
    broken: bool,
}

impl Columns {
    fn start(x: u16, step: u16, y: u16, count: u16) -> Self {
        Columns {
            x,
            step,
            y,
            count,
            next: 0,
            reading: None,
            filled: None,
            uploading: None,
            started: interrupts::vblank_count(),
            broken: false,
        }
    }

    /// Move the transfers along; `true` once every column is in VRAM.
    fn service(&mut self, dma: &mut GpuDma, buf: &Buffers, height: u16) -> bool {
        if let Some(done) = self.reading {
            if mdec::column_done() {
                self.reading = None;
                self.filled = Some(done);
            }
        }
        if self.uploading.is_some() && psx_vram::dma_copy_to_vram_done() {
            self.uploading = None;
        }
        if self.uploading.is_none() {
            if let Some((c, b)) = self.filled.take() {
                let rect = VramRect::new(self.x + c * self.step, self.y, self.step, height);
                // SAFETY: column buffer `b` holds the rectangle's words, and
                // DMA1 does not refill it until this upload is done. Nothing
                // else uses GP0 or channel 2 while a frame decodes: the
                // player only draws between frames, after `uploading` is
                // clear.
                if unsafe { psx_vram::dma_copy_to_vram_start(dma, rect, buf.column(b)) } {
                    self.uploading = Some(b);
                } else {
                    // SAFETY: the column buffer holds `column_words` words.
                    let words =
                        unsafe { core::slice::from_raw_parts(buf.column(b), buf.column_words) };
                    psx_vram::upload_words(rect, words);
                }
            }
        }
        if self.reading.is_none() && self.next < self.count {
            let held = |b: usize| self.filled.map(|f| f.1) == Some(b) || self.uploading == Some(b);
            if let Some(b) = (0..2).find(|&b| !held(b)) {
                // SAFETY: buffer `b` is free (see `held`) and holds
                // `column_words` words.
                if unsafe { mdec::read_column_start(buf.column(b), buf.column_words) } {
                    self.reading = Some((self.next, b));
                    self.next += 1;
                } else {
                    // A column the MDEC channel cannot express: abandon the
                    // frame as a decode error rather than spin on it.
                    self.next = self.count;
                    self.broken = true;
                }
            }
        }
        self.next == self.count
            && self.reading.is_none()
            && self.filled.is_none()
            && self.uploading.is_none()
    }
}

struct Player {
    /// The GPU's channel 2: the column uploads and the flip's GP0(1Fh) go
    /// through it.
    dma: GpuDma,
    st: Stream,
    buf: Buffers,
    movie: Movie,
    /// Movie's left edge and column width in VRAM halfwords.
    x: u16,
    step: u16,
    y_offset: u16,
    latency: u32,
    /// The three display buffers' VRAM origins.
    origins: [(u16, u16); BUFFERS],
    /// The buffer on screen.
    front: usize,
    /// The MDEC's frame: (RLE buffer, display buffer, due VBlank).
    decoding: Option<(usize, usize, u32)>,
    cols: Columns,
    /// Decoded frames waiting for their flip, oldest first: (display
    /// buffer, due VBlank).
    waiting: [Option<(usize, u32)>; 2],
    /// The buffer whose flip is queued, until the VBlank handler applies it.
    queued: Option<usize>,
    /// A bitstream-decoded frame waiting for the MDEC: (RLE buffer, words,
    /// due VBlank).
    pending: Option<(usize, usize, u32)>,
    shown: u32,
    late: u32,
    worst_late: u32,
    errors: u32,
    in_a_row: u32,
    wedged: bool,
    clock: Clock,
}

impl Player {
    fn idle_phase(&self) -> usize {
        if self.decoding.is_some() {
            PHASE_MDEC
        } else {
            PHASE_WAIT
        }
    }

    /// A display buffer that is neither shown, nor waiting for or queued to
    /// its flip, nor being decoded into.
    fn free_buffer(&self) -> Option<usize> {
        (0..BUFFERS).find(|&b| {
            b != self.front
                && self.queued != Some(b)
                && self.decoding.map(|d| d.1) != Some(b)
                && self.waiting.iter().all(|w| w.map(|w| w.0) != Some(b))
        })
    }

    /// Nothing decoded, decoding or waiting to be shown.
    fn drained(&self) -> bool {
        self.decoding.is_none()
            && self.pending.is_none()
            && self.waiting[0].is_none()
            && self.queued.is_none()
    }

    /// Drain the drive, move the MDEC's transfers along, and queue or
    /// retire flips.
    fn tick(&mut self) {
        // Sample the clock at least once per pump: root counter 2 wraps
        // after 65,536 * 8 cycles, less than one bitstream decode.
        self.clock.switch(self.clock.phase);
        self.st.pump();
        if let Some(b) = self.queued {
            if !interrupts::is_display_control_queued() {
                self.front = b;
                self.queued = None;
                self.shown += 1;
            }
        }
        if let Some((_, b, due)) = self.decoding {
            if self
                .cols
                .service(&mut self.dma, &self.buf, self.movie.height)
                && !self.cols.broken
            {
                self.decoding = None;
                if mdec::decode_finish() {
                    self.in_a_row = 0;
                    let slot = if self.waiting[0].is_none() { 0 } else { 1 };
                    self.waiting[slot] = Some((b, due));
                } else {
                    self.decode_failed();
                }
            } else if self.cols.broken
                || interrupts::vblank_count().wrapping_sub(self.cols.started)
                    > DECODE_TIMEOUT_VBLANKS
            {
                mdec::abort_decode();
                psx_io::dma::abort(psx_io::dma::Channel::Gpu);
                // GP1(01h): drop a VRAM copy the abort left waiting for
                // pixels.
                psx_io::gpu::write_display_control(gp1::RESET_CMD_BUFFER);
                self.decoding = None;
                self.decode_failed();
            }
        }
        if self.queued.is_none() {
            if let Some((b, due)) = self.waiting[0] {
                let now = interrupts::vblank_count();
                let edge = now.wrapping_add(1);
                // Due at the next edge, or behind: more decoded frames
                // already wait, so show this one now rather than skip one
                // later.
                let behind = self.waiting[1].is_some()
                    || (self.pending.is_some() && self.st.ready.is_some());
                // GP0 is free only between column uploads.
                let gp0_free = self.cols.uploading.is_none() && psx_vram::dma_copy_to_vram_done();
                if gp0_free && (behind || edge.wrapping_sub(due) as i32 >= 0) {
                    // The VBlank handler flips only once GPUSTAT bit 24 says
                    // the GPU got past this frame's uploads: GP0(1Fh) after
                    // them, acknowledged afresh now that no flip is queued.
                    let gpu = Gpu::from_dma_mut(&mut self.dma);
                    gpu.arm_draw_done();
                    gpu.signal_draw_done();
                    let (x, y) = self.origins[b];
                    interrupts::queue_display_control_at_vblank(gp1::display_start(
                        x as u32, y as u32,
                    ));
                    let late = edge.wrapping_sub(due) as i32;
                    if late > 0 {
                        self.late += 1;
                        self.worst_late = self.worst_late.max(late as u32);
                    }
                    self.queued = Some(b);
                    self.waiting = [self.waiting[1], None];
                }
            }
        }
    }

    fn decode_failed(&mut self) {
        self.errors += 1;
        self.in_a_row += 1;
        if self.in_a_row >= WEDGE_ERRORS {
            self.wedged = true;
        }
        let _ = setup_mdec();
    }
}

fn setup_mdec() -> bool {
    mdec::reset() && mdec::load_tables().is_some_and(|t| t.enable_writes != 0)
}

/// Every display buffer black. A 24-bit line is 1.5 times as wide in VRAM
/// halfwords.
fn clear_buffers(dma: &mut GpuDma, config: &Config) {
    let gpu = Gpu::from_dma_mut(dma);
    let line = config.screen_width * halfwords_x2(config.depth) / 2;
    for &(x, y) in &config.buffers {
        gpu.draw(&FillRect::new(
            (x, y),
            (line, config.screen_height),
            (0, 0, 0),
        ));
    }
    gpu.wait_idle();
}

/// Hand the display back in the caller's `mode`, over black when the movie
/// ran at 24 bits: that picture read as 15-bit pixels would be noise.
fn leave_display(dma: &mut GpuDma, config: &Config, mode: u32) {
    if config.depth == Depth::Rgb24 {
        clear_buffers(dma, config);
        psx_io::gpu::write_display_control(GP1_DISPLAY_MODE | mode);
    }
}

fn setup_failed() -> Outcome {
    Outcome {
        stop: Stop::Setup,
        ..Outcome::default()
    }
}

/// Play `movie` until it ends or `poll` returns `true` (checked once per
/// VBlank, e.g. for a skip button). `memory` must hold at least
/// [`memory_words`] words and is free again on return. At 15 bits the last
/// frame stays on screen (the display start may be any of the three
/// buffers); at 24 bits every buffer is cleared to black and the caller's
/// display mode is restored. The drive is stopped
/// and the CD audio volume left at 0.
pub fn play(
    movie: &Movie,
    config: &Config,
    memory: &mut [u32],
    poll: &mut dyn FnMut() -> bool,
) -> Outcome {
    if memory.len() < memory_words(movie, config.depth)
        || movie.width == 0
        || movie.height == 0
        || !movie.width.is_multiple_of(16)
        || !movie.height.is_multiple_of(16)
        || movie.width > config.screen_width
        || movie.height > config.screen_height
        || movie.max_chunks == 0
        || movie.max_chunks > MAX_CHUNKS
    {
        return setup_failed();
    }
    let base = memory.as_mut_ptr();
    let sw = slot_words(movie);
    let rw = rle_words(movie);
    let buf = Buffers {
        slots: base,
        slot_words: sw,
        // SAFETY: offsets inside `memory` (checked against memory_words).
        rle: unsafe { base.add(SLOTS * sw) },
        rle_words: rw,
        // SAFETY: as above.
        columns: unsafe { base.add(SLOTS * sw + 2 * rw) },
        column_words: column_words(movie, config.depth),
    };

    // SAFETY: the player takes over the GPU, as its doc says; nothing else
    // drives channel 2 or GP0 until it returns.
    let mut dma = unsafe { GpuDma::steal() };

    let r = reader();
    // SAFETY: the player owns the drive until it returns; prepare takes it
    // over from whatever used it before. Unmute: a muted drive plays no XA.
    let ready = unsafe {
        match movie.xa {
            Some((file, channel)) => {
                r.prepare_mode(MODE_XA) && r.unmute() && r.set_filter(file, channel)
            }
            None => r.prepare_mode(MODE_DATA),
        }
    };
    if !ready || !setup_mdec() {
        return setup_failed();
    }
    if movie.xa.is_some() {
        psx_io::cd::set_audio_mixer(0x80, 0, 0x80, 0);
        psx_spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
        psx_spu::enable_cd_audio(true);
    }

    // Every display buffer black, the first one shown, in 24-bit mode if the
    // movie is.
    let x2 = halfwords_x2(config.depth);
    let _ = interrupts::take_queued_display_control();
    clear_buffers(&mut dma, config);
    let (x0, y0) = config.buffers[0];
    psx_io::gpu::write_display_control(gp1::display_start(x0 as u32, y0 as u32));
    let mode = display_mode();
    if config.depth == Depth::Rgb24 {
        psx_io::gpu::write_display_control(GP1_DISPLAY_MODE | mode | DISPLAY_24BIT);
    }

    // SAFETY: the reader was prepared above.
    if !unsafe { r.start_read(movie.lba) } {
        leave_display(&mut dma, config, mode);
        return setup_failed();
    }

    psx_io::timers::set_mode(psx_io::timers::Timer::Timer2, 0x0200);
    let start = interrupts::vblank_count();
    let mut p = Player {
        dma,
        st: Stream {
            buf,
            last_frame: 0,
            frames: movie.frames,
            done: false,
            cd_error: false,
            started: false,
            foreign: 0,
            filling: 0,
            ready: None,
            decoding: None,
            asm: FrameAssembler::new(),
            skipped: 0,
            last_sector_vblank: start,
        },
        buf,
        movie: *movie,
        x: (config.screen_width - movie.width) / 2 * x2 / 2,
        step: 8 * x2,
        y_offset: (config.screen_height - movie.height) / 2,
        latency: config.latency_vblanks,
        origins: config.buffers,
        front: 0,
        decoding: None,
        cols: Columns::start(0, 0, 0, 0),
        waiting: [None, None],
        queued: None,
        pending: None,
        shown: 0,
        late: 0,
        worst_late: 0,
        errors: 0,
        in_a_row: 0,
        wedged: false,
        clock: Clock {
            phase: PHASE_WAIT,
            last: psx_io::timers::counter(psx_io::timers::Timer::Timer2),
            acc: [0; 3],
        },
    };
    let mut polled = start;
    let mut skipped = false;
    let rows = movie.height as u32 / 16;
    let macroblocks = movie.width as u32 / 16 * rows;
    loop {
        p.tick();
        if p.wedged {
            break;
        }
        let now = interrupts::vblank_count();
        if now != polled {
            polled = now;
            if poll() {
                skipped = true;
                break;
            }
        }
        // Decode the next frame's bitstream while the MDEC works on this one.
        if p.pending.is_none() {
            if let Some((slot, bytes, arrived)) = p.st.ready.take() {
                p.clock.switch(PHASE_VLC);
                p.st.decoding = Some(slot);
                let rle = match p.decoding {
                    Some((r, _, _)) => 1 - r,
                    None => 0,
                };
                let frame_bytes = &p.buf.slot(slot)[..(bytes as usize).min(p.buf.slot_words * 4)];
                let out = p.buf.rle16(rle);
                let decoded = bitstream::decode_frame(
                    frame_bytes,
                    out,
                    macroblocks,
                    PUMP_MACROBLOCKS,
                    &mut || p.tick(),
                );
                p.st.decoding = None;
                match decoded {
                    Ok(words) => p.pending = Some((rle, words, arrived.wrapping_add(p.latency))),
                    Err(_) => p.errors += 1,
                }
                let phase = p.idle_phase();
                p.clock.switch(phase);
                continue;
            }
        }
        if p.decoding.is_none() && p.pending.is_some() {
            if let Some(b) = p.free_buffer() {
                let Some((rle, words, due)) = p.pending.take() else {
                    continue;
                };
                p.clock.switch(PHASE_MDEC);
                let depth = match config.depth {
                    Depth::Rgb15 => DECODE_15BPP,
                    Depth::Rgb24 => DECODE_24BPP,
                };
                // SAFETY: the bitstream decoder writes only the other buffer
                // until this decode is finished or given up.
                if unsafe { mdec::decode_start(p.buf.rle(rle), words, depth) }.is_err() {
                    // Nothing the decoder wrote can be sent (no whole block):
                    // nothing started, the frame is lost.
                    p.errors += 1;
                    continue;
                }
                let (x, y) = p.origins[b];
                p.cols = Columns::start(x + p.x, p.step, y + p.y_offset, movie.width / 16);
                p.decoding = Some((rle, b, due));
                continue;
            }
        }
        let idle = now.wrapping_sub(p.st.last_sector_vblank);
        if p.drained() && p.st.ready.is_none() && (p.st.done || idle > config.stall_vblanks) {
            break;
        }
        let phase = p.idle_phase();
        p.clock.switch(phase);
    }
    // A flip still queued would land after we hand the display back.
    let _ = interrupts::take_queued_display_control();
    mdec::abort_decode();
    psx_io::dma::abort(psx_io::dma::Channel::Gpu);
    if p.cols.uploading.is_some() {
        // The abort cut a VRAM copy short: drop the transfer the GPU still
        // waits to receive.
        psx_io::gpu::write_display_control(gp1::RESET_CMD_BUFFER);
    }
    // SAFETY: stop the stream we started (also ends the XA audio).
    unsafe { reader().stop() };
    if movie.xa.is_some() {
        psx_spu::set_cd_volume(CdVolume(0), CdVolume(0));
    }
    if config.depth == Depth::Rgb24 {
        leave_display(&mut p.dma, config, mode);
    }
    let stop = if skipped {
        Stop::Skipped
    } else if p.wedged {
        Stop::Wedged
    } else if p.st.cd_error {
        Stop::CdError
    } else if p.st.done {
        Stop::End
    } else {
        Stop::Stall
    };
    let per = (p.shown + p.st.skipped).max(1);
    let kcyc = |phase: usize| p.clock.acc[phase] / per * 8 / 1000;
    Outcome {
        stop,
        shown: p.shown,
        skipped: p.st.skipped,
        late: p.late,
        worst_late_vblanks: p.worst_late,
        dropped: p.st.asm.dropped,
        decode_errors: p.errors,
        last_frame: p.st.last_frame,
        vblanks: interrupts::vblank_count().wrapping_sub(start),
        kcyc_vlc: kcyc(PHASE_VLC),
        kcyc_mdec: kcyc(PHASE_MDEC),
        kcyc_wait: kcyc(PHASE_WAIT),
    }
}
