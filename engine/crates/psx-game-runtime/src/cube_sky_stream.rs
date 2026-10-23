//! Swapping the cube sky while the game runs.
//!
//! A world cut into chunks bakes one cube sky per chunk: the world beyond that
//! chunk's ring of neighbours. Every chunk's sky is quantised to the same six
//! palettes and stored sparsely (`psx_level::sky_tiles`): the 8x8 tiles that
//! differ from the level's background, and nothing else. When the player steps
//! into another chunk, [`CubeSkyStream`] reads the new entry sixteen sectors
//! at a time through the asynchronous CD transport the rooms stream through
//! ([`RoomTransport`]) and walks the tile masks of the shown sky and the new
//! one: a tile the new sky stores is uploaded over the atlas, a tile only the
//! shown sky stored is wiped back to the background. The renderer puts a few
//! dozen of these a frame into its own GPU list as small image transfers
//! ([`write_tile_packets`]), so the GPU does them while it draws and the CPU
//! never waits for it. The distant picture refreshes from the top down over a
//! few frames, behind the fog, and no second copy of the sky takes VRAM.
//!
//! The stream keeps the exact tile-level state of VRAM (a mask: a clear bit
//! means the tile reads the background), so a swap cut short by another chunk
//! change needs no repair: the next one diffs against what is really there.
//!
//! Sky reads are queued at normal priority, so a room the player needs goes
//! first. A failed read starts the sky over on a later call; nothing waits.

use crate::grid_stream::{Poll, RoomTransport};
use psx_level::sky_tiles::{
    self as st, Background, MASK_WORDS, TILES_OFFSET, TILE_BYTES, TILE_COUNT, TILE_WORDS,
};

/// Sectors one read brings in.
const PIECE_SECTORS: u32 = 16;
const SECTOR_BYTES: usize = st::SECTOR_BYTES;

/// Words of the read window: one piece behind what is left of the previous
/// one (less than a tile; the first piece may hold the whole header).
pub const CUBE_SKY_WINDOW_WORDS: usize = (PIECE_SECTORS as usize * SECTOR_BYTES + TILES_OFFSET) / 4;

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

    /// The window's words, for a stream that shares its memory (the far
    /// cards read their pictures through it when no chunk sky does).
    pub fn words_mut(&mut self) -> &mut [u32] {
        &mut self.words
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
    /// First sector of the entry on disc.
    lba: u32,
    /// Sectors it occupies.
    sectors: u32,
    /// Sectors asked for so far.
    asked: u32,
    /// The read in flight and the sectors it brings.
    ticket: Option<(K, u32)>,
    /// Bytes at the start of the window left from the previous piece.
    carry: usize,
    /// The header and mask have landed and the mask is in `incoming`.
    header: bool,
    /// Tiles the entry stores.
    tiles: u16,
    /// Stored tiles handed to the renderer.
    taken: u16,
    /// The next tile index the walk looks at.
    pos: u16,
    /// Stored tiles landed and not taken yet, from window byte `ready_at` on.
    ready: u16,
    ready_at: usize,
    /// Bytes of the window the last read filled; what is past the ready
    /// tiles moves to the window's start once they are taken.
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
    /// Reads that failed or were cancelled, and entries that did not parse.
    pub failed: u32,
    /// Tiles uploaded.
    pub tiles: u32,
    /// Tiles wiped back to the background.
    pub wipes: u32,
    /// Skies put on screen.
    pub shown: u32,
    /// Submits the transport refused (its queue full).
    pub refused: u32,
}

/// `src` of a wipe.
const WIPE: u16 = u16::MAX;

/// The tile operations to put in one frame's list, planned before anything
/// is taken, so a frame that has no room for them leaves the stream as it
/// was ([`CubeSkyStream::plan`], [`write_tile_packets`],
/// [`CubeSkyStream::apply`]).
pub struct SkyPlan<const N: usize> {
    len: usize,
    /// Tile index of each operation.
    tile: [u16; N],
    /// Window word an upload's eight words start at, or [`WIPE`].
    src: [u16; N],
}

impl<const N: usize> SkyPlan<N> {
    /// An empty plan.
    pub const fn new() -> Self {
        Self {
            len: 0,
            tile: [0; N],
            src: [0; N],
        }
    }

    /// Operations planned.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is planned.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for SkyPlan<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// `shown_lba` while the atlas holds a mix of skies.
const MIXED: u32 = u32::MAX;

/// Streams the wanted chunk sky over the shown one.
pub struct CubeSkyStream<K> {
    /// What the stream has done so far.
    pub counters: SkyCounters,
    /// LBA of the sky on screen, 0 for the one the level started with,
    /// `MIXED` part way through a swap.
    shown_lba: u32,
    /// `shown` was set from the level's own sky.
    started: bool,
    /// LBA of an entry that did not parse, left alone until another is wanted.
    bad_lba: u32,
    /// Tiles of VRAM that may differ from the background.
    shown: [u32; MASK_WORDS],
    /// Tiles the sky being read stores.
    incoming: [u32; MASK_WORDS],
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
                tiles: 0,
                wipes: 0,
                shown: 0,
                refused: 0,
            },
            shown_lba: 0,
            started: false,
            bad_lba: 0,
            shown: [0; MASK_WORDS],
            incoming: [0; MASK_WORDS],
            job: None,
        }
    }

    /// LBA of the sky on screen (0: the level's own).
    pub fn shown(&self) -> u32 {
        self.shown_lba
    }

    /// Drive the stream one call: the sky at `lba` (`sectors` long; 0 for
    /// none) is wanted over the cube sky that is resident (`resident`).
    /// `initial` is the tile mask of the level's own sky, which VRAM starts
    /// with. Call once per tick; the renderer takes the tile operations with
    /// [`Self::plan`].
    pub fn step<T>(
        &mut self,
        transport: &mut T,
        window: &mut CubeSkyWindow,
        resident: bool,
        lba: u32,
        sectors: u32,
        initial: &[u32],
    ) -> SkyStep
    where
        T: RoomTransport<Ticket = K>,
    {
        if !self.started {
            self.started = true;
            self.shown = [0; MASK_WORDS];
            for (shown, initial) in self.shown.iter_mut().zip(initial) {
                *shown = *initial;
            }
        }
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
        if !resident {
            return SkyStep::Idle;
        }
        if self.job.is_none() {
            if lba == 0 || sectors == 0 || lba == self.shown_lba || lba == self.bad_lba {
                return SkyStep::Idle;
            }
            self.job = Some(Job {
                lba,
                sectors,
                asked: 0,
                ticket: None,
                carry: 0,
                header: false,
                tiles: 0,
                taken: 0,
                pos: 0,
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
            // The renderer has tiles of this read still to take: the window
            // stays as it is.
            return SkyStep::Reading;
        }
        if job.filled > 0 {
            // Every ready tile was taken: what is left of a tile moves to the
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
                    if Self::land(job, &mut self.incoming, window, landed).is_err() {
                        // An entry that does not parse: leave it, rather than
                        // read it again every frame.
                        self.counters.failed += 1;
                        self.bad_lba = job.lba;
                        self.job = None;
                        return SkyStep::Idle;
                    }
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
        if job.header {
            Self::skip_clear(&self.incoming, &self.shown, &mut job.pos);
            if usize::from(job.pos) >= TILE_COUNT {
                // Every operation is in a frame's list: the sky is (or is
                // about to be) on screen.
                let lba = job.lba;
                self.job = None;
                self.shown_lba = lba;
                self.counters.shown += 1;
                return SkyStep::Shown(lba);
            }
        }
        if job.asked >= job.sectors {
            if job.header && job.taken == job.tiles {
                // Only wipes are left, and they need no more data.
                return SkyStep::Reading;
            }
            // Every sector came in and tiles are still missing: a bad entry.
            self.counters.failed += 1;
            self.bad_lba = job.lba;
            self.job = None;
            return SkyStep::Idle;
        }
        let count = PIECE_SECTORS.min(job.sectors - job.asked);
        transport.begin_transfer();
        // SAFETY: the window is a static the caller keeps for the stream's
        // life; `carry` is a multiple of four (the header, the tiles and the
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

    /// Note what the first `landed` bytes of the window hold: the entry's
    /// header and mask first, then the whole tiles as ready for the renderer.
    /// `Err` when the entry does not parse.
    fn land(
        job: &mut Job<K>,
        incoming: &mut [u32; MASK_WORDS],
        window: &CubeSkyWindow,
        landed: usize,
    ) -> Result<(), ()> {
        let mut at = 0;
        if !job.header {
            if landed < TILES_OFFSET {
                job.carry = landed;
                return Ok(());
            }
            let head = window.word(0).to_le_bytes();
            let tiles = st::header_tile_count(&head).ok_or(())?;
            let mut stored = 0usize;
            for (i, mask) in incoming.iter_mut().enumerate() {
                *mask = window.word(st::HEADER_BYTES + i * 4);
                stored += mask.count_ones() as usize;
            }
            if stored != tiles {
                return Err(());
            }
            job.header = true;
            job.tiles = tiles as u16;
            at = TILES_OFFSET;
        }
        let whole = (landed - at) / TILE_BYTES;
        job.ready = whole.min(usize::from(job.tiles - job.taken)) as u16;
        job.ready_at = at;
        job.filled = landed;
        job.carry = 0;
        Ok(())
    }

    /// Move `pos` over tiles neither sky stores.
    fn skip_clear(incoming: &[u32; MASK_WORDS], shown: &[u32; MASK_WORDS], pos: &mut u16) {
        let mut at = usize::from(*pos);
        while at < TILE_COUNT {
            let active = (incoming[at / 32] | shown[at / 32]) >> (at % 32);
            if active != 0 {
                at += active.trailing_zeros() as usize;
                break;
            }
            at = (at / 32 + 1) * 32;
        }
        *pos = at.min(TILE_COUNT) as u16;
    }

    /// Whether a sky is being streamed and has tile operations to hand out.
    pub fn active(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.header && !job.cancelling)
    }

    /// Plan the next operations, at most `max` (and `N`), without taking them: each tile
    /// the new sky stores whose data has landed is an upload, each tile only
    /// the shown atlas holds is a wipe, in tile order.
    pub fn plan<const N: usize>(&self, plan: &mut SkyPlan<N>, max: usize) {
        plan.len = 0;
        let Some(job) = self.job.as_ref().filter(|j| j.header && !j.cancelling) else {
            return;
        };
        let mut pos = usize::from(job.pos);
        let mut next_word = job.ready_at / 4;
        let mut uploads_left = usize::from(job.ready);
        let limit = N.min(max);
        while plan.len < limit && pos < TILE_COUNT {
            let (word, bit) = (pos / 32, pos % 32);
            let (incoming, shown) = (self.incoming[word], self.shown[word]);
            let active = (incoming | shown) >> bit;
            if active == 0 {
                pos = (word + 1) * 32;
                continue;
            }
            let tile = pos + active.trailing_zeros() as usize;
            let bit = tile % 32;
            let src = if incoming >> bit & 1 != 0 {
                if uploads_left == 0 {
                    // Its data has not landed: the walk waits here.
                    break;
                }
                uploads_left -= 1;
                let src = next_word;
                next_word += TILE_WORDS;
                src as u16
            } else {
                WIPE
            };
            plan.tile[plan.len] = tile as u16;
            plan.src[plan.len] = src;
            plan.len += 1;
            pos = tile + 1;
        }
    }

    /// Take the operations of `plan` (made from the stream's present state
    /// and put in a frame's list): VRAM now holds their tiles.
    pub fn apply<const N: usize>(&mut self, plan: &SkyPlan<N>) {
        let Some(job) = self.job.as_mut().filter(|j| j.header && !j.cancelling) else {
            return;
        };
        if plan.len == 0 {
            return;
        }
        let mut uploads = 0u16;
        for i in 0..plan.len {
            let tile = usize::from(plan.tile[i]);
            let bit = 1u32 << (tile % 32);
            if plan.src[i] == WIPE {
                self.shown[tile / 32] &= !bit;
            } else {
                self.shown[tile / 32] |= bit;
                uploads += 1;
            }
        }
        job.taken += uploads;
        job.ready -= uploads;
        job.ready_at += usize::from(uploads) * TILE_BYTES;
        job.pos = plan.tile[plan.len - 1] + 1;
        Self::skip_clear(&self.incoming, &self.shown, &mut job.pos);
        self.counters.tiles += u32::from(uploads);
        self.counters.wipes += plan.len as u32 - u32::from(uploads);
        // The atlas now holds a mix of skies until this one is done.
        self.shown_lba = MIXED;
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
/// Words of one operation's node, with its tag.
const OP_NODE_WORDS: usize = 1 + TRANSFER_HEADER + TILE_WORDS;
const _: () = assert!(OP_NODE_WORDS - 1 <= NODE_WORDS);

/// Words [`write_tile_packets`] needs for `ops` operations: each is one image
/// transfer in one node, plus one texture-cache flush.
pub const fn tile_packet_words(ops: usize) -> usize {
    ops * OP_NODE_WORDS + 2
}

/// Write the operations of `plan` into `out` as a tagged packet stream for OT
/// slot `slot`, ready for `OtFrame::add_committed_tagged_packet_stream`: each
/// an image transfer of one tile to its place in the atlas at `origin` (a
/// wipe carries the background's words), then a texture-cache flush, all in
/// reverse (the stream's packets are prepended, so the first written runs
/// last). Returns `(words, packets)` written; nothing when `out` is short or
/// the plan is empty. The caller applies the plan once the stream is
/// committed.
pub fn write_tile_packets<const N: usize>(
    plan: &SkyPlan<N>,
    window: &CubeSkyWindow,
    background: &Background<'_>,
    origin: (u16, u16),
    slot: u16,
    out: &mut [u32],
) -> (usize, usize) {
    let words = tile_packet_words(plan.len);
    if plan.len == 0 || out.len() < words {
        return (0, 0);
    }
    let tag = u32::from(slot) | ((OP_NODE_WORDS as u32 - 1) << 24);
    // The flush runs after every tile: it goes lowest.
    out[0] = u32::from(slot) | (1 << 24);
    out[1] = 0x0100_0000;
    let mut end = words;
    for i in 0..plan.len {
        let tile = usize::from(plan.tile[i]);
        let (face, tile_y, tile_x) = st::tile_position(tile);
        // Nodes in running order, each placed below the one before.
        let node: &mut [u32; OP_NODE_WORDS] = (&mut out[end - OP_NODE_WORDS..end])
            .try_into()
            .unwrap_or_else(|_| unreachable!());
        node[0] = tag;
        node[1] = psx_hw::gpu::gp0::COPY_CPU_TO_VRAM;
        // A face is 64 halfwords wide, a tile 2 wide and 8 high.
        node[2] = psx_hw::gpu::pack_xy(
            origin.0 + (face * 64 + tile_x * 2) as u16,
            origin.1 + (tile_y * 8) as u16,
        );
        node[3] = psx_hw::gpu::pack_xy(2, 8);
        let src = plan.src[i];
        if src == WIPE {
            for (r, word) in node[4..].iter_mut().enumerate() {
                *word = background.row_word(face, tile_y * st::TILE_TEXELS + r, tile_x);
            }
        } else {
            let from = usize::from(src);
            node[4..].copy_from_slice(&window.words[from..from + TILE_WORDS]);
        }
        end -= OP_NODE_WORDS;
    }
    (words, 1 + plan.len)
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// Rows of the test background: face `f` is index `f + 1` except a band
    /// of rows 40..60 that changes index at column 100.
    fn background_tables() -> (Vec<u16>, Vec<u8>) {
        let (mut rows, mut runs) = (Vec::new(), Vec::new());
        for face in 0..st::FACES {
            for row in 0..st::FACE_TEXELS {
                rows.push((runs.len() / 2) as u16);
                if (40..60).contains(&row) {
                    runs.extend([99, face as u8 + 1, 255, 9]);
                } else {
                    runs.extend([255, face as u8 + 1]);
                }
            }
        }
        rows.push((runs.len() / 2) as u16);
        (rows, runs)
    }

    const WIDTH: usize = st::FACES * st::FACE_TEXELS;

    /// A dense atlas: the background with `count` pseudo-random tiles of
    /// noise laid over it.
    fn atlas(bg: &Background<'_>, seed: u32, count: usize) -> Vec<u8> {
        let mut out = std::vec![0u8; WIDTH * st::FACE_TEXELS];
        for tile in 0..TILE_COUNT {
            let (face, ty, tx) = st::tile_position(tile);
            for r in 0..8 {
                let word = bg.row_word(face, ty * 8 + r, tx);
                for t in 0..8 {
                    out[(ty * 8 + r) * WIDTH + face * st::FACE_TEXELS + tx * 8 + t] =
                        (word >> (4 * t)) as u8 & 0xF;
                }
            }
        }
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        for _ in 0..count {
            let tile = next() as usize % TILE_COUNT;
            let (face, ty, tx) = st::tile_position(tile);
            for r in 0..8 {
                for t in 0..8 {
                    let at = (ty * 8 + r) * WIDTH + face * st::FACE_TEXELS + tx * 8 + t;
                    out[at] = (next() % 16) as u8;
                }
            }
        }
        out
    }

    /// The sparse entry of a dense atlas.
    fn encode(atlas: &[u8], bg: &Background<'_>) -> Vec<u8> {
        let mut mask = std::vec![0u8; st::MASK_BYTES];
        let mut tiles = Vec::new();
        let mut count = 0u16;
        for tile in 0..TILE_COUNT {
            let (face, ty, tx) = st::tile_position(tile);
            let words: [u32; 8] = core::array::from_fn(|r| {
                let at = (ty * 8 + r) * WIDTH + face * st::FACE_TEXELS + tx * 8;
                (0..8).fold(0u32, |w, t| w | u32::from(atlas[at + t]) << (4 * t))
            });
            if words != bg.tile_words(face, ty, tx) {
                mask[tile / 8] |= 1 << (tile % 8);
                count += 1;
                for w in words {
                    tiles.extend_from_slice(&w.to_le_bytes());
                }
            }
        }
        let mut entry = Vec::new();
        entry.extend_from_slice(&st::VERSION.to_le_bytes());
        entry.extend_from_slice(&count.to_le_bytes());
        entry.extend_from_slice(&mask);
        entry.extend_from_slice(&tiles);
        entry
    }

    fn mask_words(entry: &[u8]) -> Vec<u32> {
        entry[st::HEADER_BYTES..TILES_OFFSET]
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect()
    }

    /// A disc of whole-sector entries and a transport reading it, a read
    /// finishing on the poll after it was queued.
    struct Disc {
        bytes: Vec<u8>,
        reads: Vec<(u32, u32, usize)>,
        cancelled: Vec<usize>,
    }

    impl Disc {
        /// Lay entries out one after another from sector 10; returns the
        /// disc and each entry's `(lba, sectors)`.
        fn with(entries: &[Vec<u8>]) -> (Self, Vec<(u32, u32)>) {
            let mut bytes = std::vec![0u8; 10 * SECTOR_BYTES];
            let mut at = Vec::new();
            for e in entries {
                let sectors = e.len().div_ceil(SECTOR_BYTES);
                at.push(((bytes.len() / SECTOR_BYTES) as u32, sectors as u32));
                bytes.extend_from_slice(e);
                bytes.resize(bytes.len().div_ceil(SECTOR_BYTES) * SECTOR_BYTES, 0);
            }
            // Room to read a whole piece past the last entry.
            bytes.resize(bytes.len() + PIECE_SECTORS as usize * SECTOR_BYTES, 0);
            (
                Self {
                    bytes,
                    reads: Vec::new(),
                    cancelled: Vec::new(),
                },
                at,
            )
        }
    }

    impl RoomTransport for Disc {
        type Ticket = usize;

        unsafe fn submit(
            &mut self,
            lba: u32,
            sectors: u32,
            destination: *mut u32,
            _urgent: bool,
        ) -> Option<usize> {
            let from = lba as usize * SECTOR_BYTES;
            let len = sectors as usize * SECTOR_BYTES;
            // SAFETY: the stream promised `sectors * 2048` bytes at the word
            // aligned destination.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.bytes[from..from + len].as_ptr(),
                    destination.cast::<u8>(),
                    len,
                );
            }
            self.reads.push((lba, sectors, 0));
            Some(self.reads.len() - 1)
        }

        fn poll(&mut self, ticket: usize) -> Poll {
            if self.cancelled.contains(&ticket) {
                return Poll::Cancelled;
            }
            let read = &mut self.reads[ticket];
            read.2 += 1;
            if read.2 < 2 {
                Poll::Pending
            } else {
                Poll::Done
            }
        }

        fn cancel(&mut self, ticket: usize) {
            self.cancelled.push(ticket);
        }

        fn begin_transfer(&mut self) {}
    }

    /// The atlas VRAM holds, as palette indices, changed only by the GPU
    /// packets the renderer would write.
    struct Vram {
        indices: Vec<u8>,
    }

    impl Vram {
        fn holding(atlas: &[u8]) -> Self {
            Self {
                indices: atlas.to_vec(),
            }
        }

        /// Run a packet stream the way the GPU does: nodes from the last
        /// written to the first. Returns the largest node.
        fn run(&mut self, stream: &[u32], origin: (usize, usize)) -> usize {
            let mut nodes = Vec::new();
            let mut at = 0;
            while at < stream.len() {
                let payload = (stream[at] >> 24) as usize;
                nodes.push(&stream[at + 1..at + 1 + payload]);
                at += payload + 1;
            }
            assert_eq!(at, stream.len());
            let mut largest = 0;
            for node in nodes.iter().rev() {
                largest = largest.max(node.len());
                if node[0] == 0x0100_0000 {
                    continue;
                }
                assert_eq!(node[0], psx_hw::gpu::gp0::COPY_CPU_TO_VRAM);
                let (x, y) = (
                    (node[1] & 0xFFFF) as usize - origin.0,
                    (node[1] >> 16) as usize - origin.1,
                );
                let (w, h) = ((node[2] & 0xFFFF) as usize, (node[2] >> 16) as usize);
                assert_eq!((w, h), (2, 8));
                assert_eq!(node.len(), 3 + w * h / 2);
                for r in 0..h {
                    for t in 0..8 {
                        self.indices[(y + r) * WIDTH + x * 4 + t] =
                            (node[3 + r] >> (4 * t)) as u8 & 0xF;
                    }
                }
            }
            largest
        }
    }

    struct Rig {
        rows: Vec<u16>,
        runs: Vec<u8>,
        atlases: Vec<Vec<u8>>,
        entries: Vec<Vec<u8>>,
    }

    impl Rig {
        fn new(counts: &[usize]) -> Self {
            let (rows, runs) = background_tables();
            let bg = Background::new(&rows, &runs);
            let atlases: Vec<Vec<u8>> = counts
                .iter()
                .enumerate()
                .map(|(i, &c)| atlas(&bg, 17 + i as u32 * 31, c))
                .collect();
            let entries = atlases.iter().map(|a| encode(a, &bg)).collect();
            Self {
                rows,
                runs,
                atlases,
                entries,
            }
        }

        fn bg(&self) -> Background<'_> {
            Background::new(&self.rows, &self.runs)
        }
    }

    /// Everything a test drives.
    struct Bench {
        stream: CubeSkyStream<usize>,
        disc: Disc,
        window: CubeSkyWindow,
        vram: Vram,
        at: Vec<(u32, u32)>,
        initial: Vec<u32>,
    }

    impl Bench {
        fn new(rig: &Rig) -> Self {
            let (disc, at) = Disc::with(&rig.entries);
            Self {
                stream: CubeSkyStream::new(),
                disc,
                window: CubeSkyWindow::new(),
                vram: Vram::holding(&rig.atlases[0]),
                at,
                initial: mask_words(&rig.entries[0]),
            }
        }

        /// Plan, write and apply up to `per_frame` operations for a list at
        /// `origin`; returns the packet words.
        fn write_frame(
            &mut self,
            bg: &Background<'_>,
            per_frame: usize,
            origin: (u16, u16),
        ) -> Vec<u32> {
            let mut plan = SkyPlan::<96>::new();
            self.stream.plan(&mut plan, per_frame);
            let mut out = std::vec![0u32; tile_packet_words(plan.len())];
            let (words, _) = write_tile_packets(&plan, &self.window, bg, origin, 7, &mut out);
            self.stream.apply(&plan);
            out.truncate(words);
            out
        }

        /// One frame wanting sky `want`: step, then write and run up to
        /// `per_frame` operations.
        fn frame(&mut self, bg: &Background<'_>, want: usize, per_frame: usize) -> SkyStep {
            let (lba, sectors) = self.at[want];
            let step = self.stream.step(
                &mut self.disc,
                &mut self.window,
                true,
                lba,
                sectors,
                &self.initial,
            );
            let out = self.write_frame(bg, per_frame, (0, 0));
            if !out.is_empty() {
                assert!(self.vram.run(&out, (0, 0)) <= NODE_WORDS);
            }
            step
        }

        fn show(&mut self, bg: &Background<'_>, want: usize, per_frame: usize) -> usize {
            for frame in 1..=3000 {
                if self.frame(bg, want, per_frame) == SkyStep::Shown(self.at[want].0) {
                    return frame;
                }
            }
            panic!("sky {want} never showed");
        }
    }

    #[test]
    fn swapping_skies_leaves_vram_equal_to_each_dense_atlas() {
        let rig = Rig::new(&[120, 300, 40, 700]);
        let bg = rig.bg();
        let mut bench = Bench::new(&rig);
        // 1, 2, 3, then 0 (VRAM started on it, but the stream is asked to
        // stream it like any other) and 3 again.
        for want in [1, 2, 3, 0, 3] {
            bench.show(&bg, want, 96);
            assert_eq!(bench.vram.indices, rig.atlases[want], "after sky {want}");
            assert_eq!(bench.stream.shown(), bench.at[want].0);
        }
        assert_eq!(bench.stream.counters.shown, 5);
        assert_eq!(bench.stream.counters.failed, 0);
    }

    #[test]
    fn a_swap_cut_short_by_another_leaves_nothing_behind() {
        let rig = Rig::new(&[200, 500, 350]);
        let bg = rig.bg();
        let mut bench = Bench::new(&rig);
        // Sky 0 is shown whole, then 0 -> 1 is cut short after a few frames
        // of 20 operations.
        bench.show(&bg, 0, 96);
        for _ in 0..6 {
            bench.frame(&bg, 1, 20);
        }
        assert_ne!(bench.vram.indices, rig.atlases[0]);
        assert_ne!(bench.vram.indices, rig.atlases[1]);
        // The player turns back: the sky it left must come back whole even
        // though the stream last finished showing it.
        bench.show(&bg, 0, 33);
        assert_eq!(bench.vram.indices, rig.atlases[0]);
        // A swap cut short while a read is in flight, then a third sky.
        bench.frame(&bg, 1, 5);
        bench.show(&bg, 2, 33);
        assert_eq!(bench.vram.indices, rig.atlases[2]);
    }

    #[test]
    fn an_entry_longer_than_a_read_streams_across_pieces() {
        // 3,500 random tiles are some 2,500 distinct ones, over 80 KiB: three or
        // more reads of at most sixteen sectors, the
        // window refilled behind tiles that straddle each piece.
        let rig = Rig::new(&[10, 3500]);
        let bg = rig.bg();
        let mut bench = Bench::new(&rig);
        assert!(bench.at[1].1 > 2 * PIECE_SECTORS);
        // Seven operations a frame: tiles straddle sector and piece edges.
        bench.show(&bg, 1, 7);
        assert_eq!(bench.vram.indices, rig.atlases[1]);
        assert!(bench.stream.counters.submitted >= 3);
        assert!(bench.disc.reads.iter().all(|r| r.1 <= PIECE_SECTORS));
    }

    #[test]
    fn a_wipe_carries_the_background_words_of_its_tile() {
        let rig = Rig::new(&[60, 0]);
        let bg = rig.bg();
        let mut bench = Bench::new(&rig);
        let mut wipes = Vec::new();
        for _ in 0..40 {
            let (lba, sectors) = bench.at[1];
            bench.stream.step(
                &mut bench.disc,
                &mut bench.window,
                true,
                lba,
                sectors,
                &bench.initial,
            );
            let out = bench.write_frame(&bg, 96, (320, 256));
            let words = out.len();
            let mut at = 0;
            while at < words {
                let payload = (out[at] >> 24) as usize;
                let node = &out[at + 1..at + 1 + payload];
                if node[0] == psx_hw::gpu::gp0::COPY_CPU_TO_VRAM {
                    wipes.push(node.to_vec());
                }
                at += payload + 1;
            }
        }
        // Every wipe is the background of the tile at its place.
        for node in &wipes {
            let (x, y) = (
                (node[1] & 0xFFFF) as usize - 320,
                (node[1] >> 16) as usize - 256,
            );
            let words = bg.tile_words(x / 64, y / 8, x % 64 / 2);
            assert_eq!(node[3..], words);
        }
        // They are exactly the tiles sky 0 stored.
        assert_eq!(wipes.len(), st::header_tile_count(&rig.entries[0]).unwrap());
    }

    #[test]
    fn a_short_buffer_takes_no_operation() {
        let rig = Rig::new(&[60, 0]);
        let mut bench = Bench::new(&rig);
        for _ in 0..4 {
            let (lba, sectors) = bench.at[1];
            bench.stream.step(
                &mut bench.disc,
                &mut bench.window,
                true,
                lba,
                sectors,
                &bench.initial,
            );
        }
        let mut plan = SkyPlan::<96>::new();
        bench.stream.plan(&mut plan, 5);
        assert_eq!(plan.len(), 5);
        let mut out = std::vec![0u32; tile_packet_words(5) - 1];
        assert_eq!(
            write_tile_packets(&plan, &bench.window, &rig.bg(), (0, 0), 0, &mut out),
            (0, 0)
        );
        // Nothing was applied: the same operations are planned again.
        let mut again = SkyPlan::<96>::new();
        bench.stream.plan(&mut again, 5);
        assert_eq!(again.tile, plan.tile);
    }

    #[test]
    fn an_entry_that_does_not_parse_is_left_alone() {
        let rig = Rig::new(&[60]);
        let mut bad = rig.entries[0].clone();
        bad[0] = 9;
        let (mut disc, at) = Disc::with(&[bad]);
        let initial = mask_words(&rig.entries[0]);
        let mut stream = CubeSkyStream::<usize>::new();
        let mut window = CubeSkyWindow::new();
        for _ in 0..8 {
            stream.step(&mut disc, &mut window, true, at[0].0, at[0].1, &initial);
        }
        assert_eq!(stream.counters.failed, 1);
        assert!(!stream.active());
        // It is not read again every frame.
        let reads = disc.reads.len();
        stream.step(&mut disc, &mut window, true, at[0].0, at[0].1, &initial);
        assert_eq!(disc.reads.len(), reads);
    }

    #[test]
    fn nothing_streams_while_the_cube_sky_is_not_resident() {
        let rig = Rig::new(&[60, 5]);
        let (mut disc, at) = Disc::with(&rig.entries);
        let mut stream = CubeSkyStream::<usize>::new();
        let mut window = CubeSkyWindow::new();
        let initial = mask_words(&rig.entries[0]);
        assert_eq!(
            stream.step(&mut disc, &mut window, false, at[1].0, at[1].1, &initial),
            SkyStep::Idle
        );
        assert!(disc.reads.is_empty());
    }
}
