//! Swapping the cube sky while the game runs.
//!
//! A world cut into chunks bakes one cube sky per chunk: the world beyond that
//! chunk's ring of neighbours. Every chunk's sky is quantised to the same six
//! palettes, so a new sky differs from the shown one in its texels only. When
//! the player steps into another chunk, [`CubeSkyStream`] reads the new
//! atlas a few sectors at a time through the asynchronous CD transport the
//! rooms stream through ([`RoomTransport`]) and uploads every whole row as it
//! lands straight over the shown atlas ([`upload_cube_sky_rows`]). The game
//! never stops: the distant picture refreshes from the top down over the few
//! frames the read takes, behind the fog, and no second copy of the sky
//! takes VRAM.
//!
//! Sky reads are queued at normal priority, so a room the player needs goes
//! first. A failed read starts the sky over on a later call; nothing waits.

use crate::grid_stream::{Poll, RoomTransport};
use crate::vram::{
    upload_cube_sky_rows, VramRuntime, DIRECTIONAL_SKY_ATLAS_HEIGHT, DIRECTIONAL_SKY_ATLAS_WIDTH,
};
use psx_level::AssetId;

/// Sectors one read brings in. Each read that lands is one upload, and an
/// upload waits for the GPU to finish the display list in flight, so few big
/// reads cost far less than many small ones (4-sector reads cost about one
/// presented frame each on the hexagonal Citadel).
const PIECE_SECTORS: u32 = 16;
const SECTOR_BYTES: usize = 2048;
/// Asset header and texture header ahead of the pixels.
const HEADER_BYTES: usize = 28;
const ROW_BYTES: usize = DIRECTIONAL_SKY_ATLAS_WIDTH as usize / 2;

/// Words of the read window: one piece behind what is left of the previous
/// one (less than a row).
pub const CUBE_SKY_WINDOW_WORDS: usize = (PIECE_SECTORS as usize * SECTOR_BYTES + ROW_BYTES) / 4;

/// The RAM a sky read lands in. The game keeps one in a static, so it stays
/// in `.bss`.
#[repr(C, align(4))]
pub struct CubeSkyWindow {
    words: [u32; CUBE_SKY_WINDOW_WORDS],
}

impl CubeSkyWindow {
    /// A zeroed window.
    pub const fn new() -> Self {
        Self {
            words: [0; CUBE_SKY_WINDOW_WORDS],
        }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        let len = self.words.len() * 4;
        // SAFETY: in-bounds byte view of plain words.
        unsafe { core::slice::from_raw_parts_mut(self.words.as_mut_ptr().cast::<u8>(), len) }
    }
}

impl Default for CubeSkyWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// One sky being read.
#[derive(Copy, Clone)]
struct Job<K> {
    /// First sector of the atlas on disc.
    lba: u32,
    /// Sectors it occupies.
    sectors: u32,
    /// Sectors asked for so far.
    asked: u32,
    /// The read in flight and the sectors it brings.
    ticket: Option<(K, u32)>,
    /// Bytes at the start of the window left from the previous piece.
    carry: usize,
    header: bool,
    rows: u16,
    /// A cancel was sent; the job ends when the read reports finished.
    cancelling: bool,
}

/// What one [`CubeSkyStream::step`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SkyStep {
    /// Nothing to do: the wanted sky is shown, none is wanted, or the sky to
    /// write over is not resident.
    Idle,
    /// A sky is on its way.
    Reading,
    /// The sky at this LBA is now on screen.
    Shown(u32),
}

/// What a [`CubeSkyStream`] has done, for telemetry and diagnosis.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct SkyCounters {
    /// Reads queued.
    pub submitted: u32,
    /// Reads that landed.
    pub landed: u32,
    /// Reads that failed or were cancelled.
    pub failed: u32,
    /// Atlas rows uploaded.
    pub rows: u32,
    /// Skies put on screen.
    pub shown: u32,
    /// Submits the transport refused (its queue full).
    pub refused: u32,
}

/// Streams the wanted chunk sky over the shown one.
pub struct CubeSkyStream<K> {
    /// What the stream has done so far.
    pub counters: SkyCounters,
    /// LBA of the sky on screen, 0 for the one the level started with.
    shown: u32,
    job: Option<Job<K>>,
}

impl<K: Copy> CubeSkyStream<K> {
    /// A stream showing the level's own sky.
    pub const fn new() -> Self {
        Self {
            counters: SkyCounters {
                submitted: 0,
                landed: 0,
                failed: 0,
                rows: 0,
                shown: 0,
                refused: 0,
            },
            shown: 0,
            job: None,
        }
    }

    /// LBA of the sky on screen (0: the level's own).
    pub fn shown(&self) -> u32 {
        self.shown
    }

    /// Drive the stream one call: the sky at `lba` (`sectors` long; 0 for
    /// none) is wanted over the cube sky resident under `key`. Call once per
    /// tick.
    pub fn step<T, const R: usize, const V: usize, const P: usize, const C: usize>(
        &mut self,
        transport: &mut T,
        vram: &VramRuntime<R, V, P, C>,
        window: &mut CubeSkyWindow,
        key: AssetId,
        lba: u32,
        sectors: u32,
    ) -> SkyStep
    where
        T: RoomTransport<Ticket = K>,
    {
        // A different sky is wanted: stop the read in flight first.
        if let Some(job) = self.job.as_mut() {
            if job.lba != lba && !job.cancelling {
                match job.ticket {
                    Some((ticket, _)) => {
                        transport.cancel(ticket);
                        job.cancelling = true;
                    }
                    None => self.job = None,
                }
            }
        }
        let Some(origin) = vram.cube_sky_origin(key) else {
            return SkyStep::Idle;
        };
        if self.job.is_none() {
            if lba == 0 || sectors == 0 || lba == self.shown {
                return SkyStep::Idle;
            }
            self.job = Some(Job {
                lba,
                sectors,
                asked: 0,
                ticket: None,
                carry: 0,
                header: false,
                rows: 0,
                cancelling: false,
            });
        }
        let Some(job) = self.job.as_mut() else {
            return SkyStep::Idle;
        };
        if let Some((ticket, count)) = job.ticket {
            match transport.poll(ticket) {
                Poll::Pending => return SkyStep::Reading,
                Poll::Done if !job.cancelling => {
                    self.counters.landed += 1;
                    job.ticket = None;
                    let landed = job.carry + count as usize * SECTOR_BYTES;
                    let before = job.rows;
                    job.carry = Self::consume(job, origin, window, landed);
                    self.counters.rows += u32::from(job.rows - before);
                }
                _ => {
                    // Cancelled or failed: the next call starts over.
                    self.counters.failed += 1;
                    self.job = None;
                    return SkyStep::Reading;
                }
            }
        }
        if job.rows == DIRECTIONAL_SKY_ATLAS_HEIGHT {
            let lba = job.lba;
            self.job = None;
            self.shown = lba;
            self.counters.shown += 1;
            return SkyStep::Shown(lba);
        }
        if job.asked >= job.sectors {
            // Every sector came in and the atlas is still short: a bad chunk.
            // Leave it, rather than read it again every frame.
            self.job = None;
            self.shown = lba;
            return SkyStep::Idle;
        }
        let count = PIECE_SECTORS.min(job.sectors - job.asked);
        transport.begin_transfer();
        // SAFETY: the window is a static the caller keeps for the stream's
        // life; `carry` is a multiple of four (the header, the rows and the
        // sectors all are) and `carry + count * 2048` fits the window.
        let destination = unsafe { window.words.as_mut_ptr().add(job.carry / 4) };
        match unsafe { transport.submit(job.lba + job.asked, count, destination, false) } {
            Some(ticket) => {
                job.ticket = Some((ticket, count));
                job.asked += count;
                self.counters.submitted += 1;
            }
            None => self.counters.refused += 1,
        }
        SkyStep::Reading
    }

    /// Upload the whole rows the first `landed` bytes of the window hold and
    /// move what is left of a row to the window's start. Returns the bytes
    /// left there.
    fn consume(
        job: &mut Job<K>,
        origin: (u16, u16),
        window: &mut CubeSkyWindow,
        landed: usize,
    ) -> usize {
        let bytes = window.bytes_mut();
        let mut taken = 0;
        if !job.header {
            if landed < HEADER_BYTES {
                return landed;
            }
            job.header = true;
            taken = HEADER_BYTES;
        }
        let rows = ((landed - taken) / ROW_BYTES)
            .min(usize::from(DIRECTIONAL_SKY_ATLAS_HEIGHT - job.rows));
        if rows > 0 {
            upload_cube_sky_rows(origin, job.rows, &bytes[taken..taken + rows * ROW_BYTES]);
            job.rows += rows as u16;
            taken += rows * ROW_BYTES;
        }
        if job.rows == DIRECTIONAL_SKY_ATLAS_HEIGHT {
            // The palettes after the rows are the shared ones already shown.
            return 0;
        }
        bytes.copy_within(taken..landed, 0);
        landed - taken
    }

    /// Stop any read (on leaving gameplay). The stream starts over from the
    /// level's own sky.
    pub fn release<T>(&mut self, transport: &mut T)
    where
        T: RoomTransport<Ticket = K>,
    {
        if let Some(Job {
            ticket: Some((ticket, _)),
            ..
        }) = self.job
        {
            transport.cancel(ticket);
        }
        *self = Self::new();
    }
}

impl<K: Copy> Default for CubeSkyStream<K> {
    fn default() -> Self {
        Self::new()
    }
}
