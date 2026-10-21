//! Swapping the cube sky while the game runs.
//!
//! A world cut into chunks bakes one cube sky per chunk: the world beyond that
//! chunk's ring of neighbours. Every chunk's sky is quantised to the same six
//! palettes, so a new sky differs from the shown one in its texels only. When
//! the player steps into another chunk, [`CubeSkyStream`] reads the new
//! atlas sixteen sectors at a time through the asynchronous CD transport the
//! rooms stream through ([`RoomTransport`]) and hands the renderer every whole
//! row as it lands ([`CubeSkyStream::take_rows`]); the renderer puts a few
//! rows a frame into its own GPU list as image transfers over the shown atlas
//! ([`write_row_packets`]), so the GPU uploads them while it draws and the
//! CPU never waits for it. The distant picture refreshes from the top down
//! over a second or so, behind the fog, and no second copy of the sky takes
//! VRAM.
//!
//! Sky reads are queued at normal priority, so a room the player needs goes
//! first. A failed read starts the sky over on a later call; nothing waits.

use crate::grid_stream::{Poll, RoomTransport};
use crate::vram::{VramRuntime, DIRECTIONAL_SKY_ATLAS_HEIGHT, DIRECTIONAL_SKY_ATLAS_WIDTH};
use psx_level::AssetId;

/// Sectors one read brings in.
const PIECE_SECTORS: u32 = 16;
const SECTOR_BYTES: usize = 2048;
/// Asset header and texture header ahead of the pixels.
const HEADER_BYTES: usize = 28;
const ROW_BYTES: usize = DIRECTIONAL_SKY_ATLAS_WIDTH as usize / 2;
/// Words in one atlas row.
pub const ROW_WORDS: usize = ROW_BYTES / 4;

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

    fn word(&self, byte: usize) -> u32 {
        self.words[byte / 4]
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
    /// Rows handed to the renderer.
    rows: u16,
    /// Landed rows the renderer has not taken yet, from window byte
    /// `ready_at` on.
    ready: u16,
    ready_at: usize,
    /// Bytes of the window the last read filled; what is past the ready
    /// rows moves to the window's start once they are taken.
    filled: usize,
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
    /// tick; the renderer takes the landed rows with [`Self::take_rows`].
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
        if vram.cube_sky_origin(key).is_none() {
            return SkyStep::Idle;
        }
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
                ready: 0,
                ready_at: 0,
                filled: 0,
                cancelling: false,
            });
        }
        let Some(job) = self.job.as_mut() else {
            return SkyStep::Idle;
        };
        if job.ready > 0 {
            // The renderer has rows of this read still to take: the window
            // stays as it is.
            return SkyStep::Reading;
        }
        if job.filled > 0 {
            // Every ready row was taken: what is left of a row moves to the
            // window's start for the next read to follow.
            let (from, to) = (job.ready_at, job.filled);
            window.bytes_mut().copy_within(from..to, 0);
            job.carry = to - from;
            job.filled = 0;
        }
        if let Some((ticket, count)) = job.ticket {
            match transport.poll(ticket) {
                Poll::Pending => return SkyStep::Reading,
                Poll::Done if !job.cancelling => {
                    self.counters.landed += 1;
                    job.ticket = None;
                    let landed = job.carry + count as usize * SECTOR_BYTES;
                    Self::land(job, landed);
                    if job.ready > 0 {
                        return SkyStep::Reading;
                    }
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
            // Every row is in a frame's list: the sky is (or is about to be)
            // on screen.
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

    /// Note the whole rows the first `landed` bytes of the window hold as
    /// ready for the renderer.
    fn land(job: &mut Job<K>, landed: usize) {
        let mut at = 0;
        if !job.header {
            if landed < HEADER_BYTES {
                job.carry = landed;
                return;
            }
            job.header = true;
            at = HEADER_BYTES;
        }
        let rows = ((landed - at) / ROW_BYTES).min(usize::from(
            DIRECTIONAL_SKY_ATLAS_HEIGHT - job.rows - job.ready,
        ));
        job.ready = rows as u16;
        job.ready_at = at;
        job.filled = landed;
        job.carry = 0;
    }

    /// Rows landed and not yet taken.
    pub fn rows_ready(&self) -> u16 {
        self.job
            .map_or(0, |job| if job.cancelling { 0 } else { job.ready })
    }

    /// Hand the renderer up to `max` landed rows in order: `emit(row, words)`
    /// per row, with the atlas row index and its `ROW_WORDS` words. Returns
    /// how many it handed out.
    pub fn take_rows(
        &mut self,
        window: &CubeSkyWindow,
        max: u16,
        mut emit: impl FnMut(u16, &mut dyn Iterator<Item = u32>),
    ) -> u16 {
        let Some(job) = self.job.as_mut().filter(|job| !job.cancelling) else {
            return 0;
        };
        let rows = max.min(job.ready);
        for _ in 0..rows {
            let at = job.ready_at;
            let mut words = (0..ROW_WORDS).map(|i| window.word(at + i * 4));
            emit(job.rows, &mut words);
            job.rows += 1;
            job.ready -= 1;
            job.ready_at += ROW_BYTES;
        }
        self.counters.rows += u32::from(rows);
        rows
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

/// Words of linked-list payload one node may carry: a console loses words
/// from longer nodes while it draws (see `psx_gpu::chain::MAX_NODE_WORDS`).
const NODE_WORDS: usize = 16;
/// GP0(A0h) words ahead of an image transfer's data.
const TRANSFER_HEADER: usize = 3;

/// Words [`write_row_packets`] needs for `rows` rows: each row is one image
/// transfer split into nodes of at most [`NODE_WORDS`] words, plus one
/// texture-cache flush.
pub const fn row_packet_words(rows: usize) -> usize {
    let payload = TRANSFER_HEADER + ROW_WORDS;
    let nodes = payload.div_ceil(NODE_WORDS);
    rows * (payload + nodes) + 2
}

/// Write `rows` rows from `stream` into `out` as a tagged packet stream for
/// OT slot `slot`, ready for `OtFrame::add_committed_tagged_packet_stream`:
/// each row an image transfer to its place in the atlas at `origin`, split
/// into nodes the GPU takes safely, then a texture-cache flush, all in
/// reverse (the stream's packets are prepended, so the first written runs
/// last). Returns `(words, packets)` written; nothing when `out` is short or
/// no row is ready.
pub fn write_row_packets<K: Copy>(
    stream: &mut CubeSkyStream<K>,
    window: &CubeSkyWindow,
    origin: (u16, u16),
    rows: u16,
    slot: u16,
    out: &mut [u32],
) -> (usize, usize) {
    let rows = rows.min(stream.rows_ready());
    let words = row_packet_words(usize::from(rows));
    if rows == 0 || out.len() < words {
        return (0, 0);
    }
    let tag = |payload: usize| ((payload as u32) << 24) | u32::from(slot);
    // The flush runs after every row: it goes lowest.
    out[0] = tag(1);
    out[1] = 0x0100_0000;
    let mut packets = 1;
    let mut end = words;
    stream.take_rows(window, rows, |row, data| {
        let mut stream = [
            psx_hw::gpu::gp0::COPY_CPU_TO_VRAM,
            psx_hw::gpu::pack_xy(origin.0, origin.1 + row),
            psx_hw::gpu::pack_xy(DIRECTIONAL_SKY_ATLAS_WIDTH / 4, 1),
        ]
        .into_iter()
        .chain(data);
        let mut left = TRANSFER_HEADER + ROW_WORDS;
        // Nodes in running order, each placed below the one before.
        while left > 0 {
            let payload = left.min(NODE_WORDS);
            let at = end - payload - 1;
            out[at] = tag(payload);
            for word in &mut out[at + 1..at + 1 + payload] {
                *word = stream.next().unwrap_or(0);
            }
            end = at;
            left -= payload;
            packets += 1;
        }
    });
    (words, packets)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// A stream with `ready` landed rows whose words count up from `seed`.
    fn ready_stream(ready: u16, window: &mut CubeSkyWindow) -> CubeSkyStream<u32> {
        for (i, word) in window.words.iter_mut().enumerate() {
            *word = 0x1000_0000 + i as u32;
        }
        let mut stream = CubeSkyStream::new();
        stream.job = Some(Job {
            lba: 100,
            sectors: 97,
            asked: 16,
            ticket: None,
            carry: 0,
            header: true,
            rows: 10,
            ready,
            ready_at: 28,
            filled: 16 * SECTOR_BYTES,
            cancelling: false,
        });
        stream
    }

    #[test]
    fn row_packets_run_as_image_transfers_then_a_cache_flush() {
        let mut window = CubeSkyWindow::new();
        let mut stream = ready_stream(5, &mut window);
        let mut out = std::vec![0u32; row_packet_words(3)];
        let (words, packets) =
            write_row_packets(&mut stream, &window, (320, 256), 3, 1023, &mut out);
        assert_eq!(words, out.len());
        // Undo the prepending: the packets run from the last written to the
        // first, and every node fits what a console takes.
        let mut nodes = Vec::new();
        let mut at = 0;
        while at < words {
            let payload = (out[at] >> 24) as usize;
            assert!(payload <= NODE_WORDS && payload > 0);
            assert_eq!(out[at] & 0xFFFF, 1023);
            nodes.push(&out[at + 1..at + 1 + payload]);
            at += payload + 1;
        }
        assert_eq!(at, words);
        assert_eq!(nodes.len(), packets);
        let gp0: Vec<u32> = nodes.iter().rev().flat_map(|n| n.iter().copied()).collect();
        let mut expected = Vec::new();
        for row in 0..3u16 {
            expected.push(psx_hw::gpu::gp0::COPY_CPU_TO_VRAM);
            expected.push(psx_hw::gpu::pack_xy(320, 256 + 10 + row));
            expected.push(psx_hw::gpu::pack_xy(384, 1));
            let first = (28 + usize::from(row) * ROW_BYTES) / 4;
            expected.extend((first..first + ROW_WORDS).map(|i| 0x1000_0000 + i as u32));
        }
        expected.push(0x0100_0000);
        assert_eq!(gp0, expected);
        // Three of the five rows were taken; the job moves on from there.
        assert_eq!(stream.rows_ready(), 2);
        assert_eq!(stream.counters.rows, 3);
    }

    #[test]
    fn a_short_buffer_takes_no_row() {
        let mut window = CubeSkyWindow::new();
        let mut stream = ready_stream(2, &mut window);
        let mut out = std::vec![0u32; row_packet_words(2) - 1];
        assert_eq!(
            write_row_packets(&mut stream, &window, (0, 0), 2, 0, &mut out),
            (0, 0)
        );
        assert_eq!(stream.rows_ready(), 2);
    }
}
