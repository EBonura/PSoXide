// SPDX-License-Identifier: GPL-2.0-or-later
//! The standard PlayStation 1 memory-card filesystem, on top of any [`Block`].
//!
//! Layout (matching the BIOS / PCSX-Redux formatted image, so saves are visible
//! in the console's card manager):
//!
//! ```text
//! block 0  = directory
//!   frame 0        "MC" header + XOR checksum
//!   frames 1..=15  one directory entry per data block 1..=15
//!   frames 16..=35 broken-sector list (0xFF)
//! blocks 1..=15 = 8 KiB data blocks
//!   a saved file's first block:
//!     frame 0      "SC" title header + 16-colour icon CLUT
//!     frame 1      16x16 4bpp icon
//!     frames 2..63 payload
//!   continuation blocks: 64 payload frames
//! ```
//!
//! Directory entry (128 bytes):
//! `[0]` alloc state, `[4..8]` size LE (the file's *total block allocation* in
//! bytes -- `blocks * 8192`, not the payload length; the BIOS validates this
//! against the block count and hides entries where it disagrees),
//! `[8..10]` next-block link LE, `[10..30]` NUL-terminated name,
//! `[127]` XOR checksum of `[0..127)`.
//!
//! `SC` title header, first frame of a file's first block: `[2..4]` icon
//! display flag, u16 LE (`0x11` none, `0x12` one static frame, `0x13`
//! two-frame animated), `[4..68]` title (Shift-JIS), `[0x60..0x80]` 16-colour
//! CLUT.

use crate::{
    Block, Card, Entry, Error, Result, CONTAINER_LEN, CONTAINER_MAGIC, DATA_BLOCKS,
    FLAG_COMPRESSED, FRAMES_PER_BLOCK, FRAME_SIZE, MAX_NAME,
};

// Directory entry field offsets.
const E_STATE: usize = 0;
const E_SIZE: usize = 4;
const E_LINK: usize = 8;
const E_NAME: usize = 10;
const E_CHK: usize = 127;

// Allocation states.
const ST_FIRST: u8 = 0x51;
const ST_MIDDLE: u8 = 0x52;
const ST_LAST: u8 = 0x53;
const ST_FREE: u8 = 0xA0;
/// A block is free if its state's high nibble is `0xA` (formatted or deleted).
fn is_free(state: u8) -> bool {
    state & 0xF0 == 0xA0
}

// Title (`SC`) header offsets within a file's first frame.
// `icon_flag` is a u16 LE (0x11 none, 0x12 one static frame, 0x13 two-frame
// animated) immediately followed by the title -- there is no separate
// block-count field in the real format.
const T_ICON_FLAG: usize = 2;
const T_TITLE: usize = 4;
const T_TITLE_LEN: usize = 64;
const T_CLUT: usize = 0x60;
/// One static icon frame (this driver always writes exactly one).
const ICON_FLAG_STATIC: u16 = 0x12;

const LINK_NONE: u16 = 0xFFFF;
/// Payload frames in a file's first block (title + icon consume 2 frames).
const FIRST_BLOCK_FRAMES: usize = FRAMES_PER_BLOCK - 2;
/// Payload bytes in a file's first block.
const FIRST_BLOCK_CAP: usize = FIRST_BLOCK_FRAMES * FRAME_SIZE; // 7936
/// Payload bytes in a continuation block.
const BLOCK_CAP: usize = FRAMES_PER_BLOCK * FRAME_SIZE; // 8192
/// Bytes in one allocated block -- the unit the directory size field counts.
const BLOCK_BYTES: usize = FRAMES_PER_BLOCK * FRAME_SIZE; // 8192

/// XOR of a frame's first 127 bytes (the directory checksum).
fn checksum(frame: &[u8; FRAME_SIZE]) -> u8 {
    frame[..E_CHK].iter().fold(0u8, |a, &b| a ^ b)
}

/// Blocks needed to hold `total` payload bytes (including the container header).
fn blocks_for(total: usize) -> usize {
    if total <= FIRST_BLOCK_CAP {
        1
    } else {
        1 + (total - FIRST_BLOCK_CAP).div_ceil(BLOCK_CAP)
    }
}

fn write_u32(buf: &mut [u8], v: u32) {
    buf[0..4].copy_from_slice(&v.to_le_bytes());
}
fn read_u32(buf: &[u8]) -> u32 {
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

// --------------------------------------------------------------------------
// A file's block chain: physical block numbers (1..=15), in order.
// --------------------------------------------------------------------------

struct Chain {
    blocks: [u8; DATA_BLOCKS],
    len: usize,
}

// --------------------------------------------------------------------------
// Sequential reader over a file's payload region (skips title+icon frames).
// --------------------------------------------------------------------------

struct DataCursor<'a> {
    blocks: &'a [u8],
    li: usize,  // index into `blocks`
    fib: usize, // frame-in-block (starts at 2 for the first block)
    off: usize, // byte within the current frame
    frame: [u8; FRAME_SIZE],
    loaded: bool,
}

impl<'a> DataCursor<'a> {
    fn new(blocks: &'a [u8]) -> Self {
        DataCursor {
            blocks,
            li: 0,
            fib: 2, // skip title + icon frames of the first block
            off: 0,
            frame: [0; FRAME_SIZE],
            loaded: false,
        }
    }

    fn next_byte<B: Block>(&mut self, dev: &mut B) -> Result<u8> {
        if self.li >= self.blocks.len() {
            return Err(Error::Corrupt);
        }
        if !self.loaded {
            let phys = self.blocks[self.li] as u16 * FRAMES_PER_BLOCK as u16 + self.fib as u16;
            dev.read_frame(phys, &mut self.frame)?;
            self.loaded = true;
        }
        let b = self.frame[self.off];
        self.off += 1;
        if self.off == FRAME_SIZE {
            self.off = 0;
            self.loaded = false;
            self.fib += 1;
            if self.fib == FRAMES_PER_BLOCK {
                self.fib = 0;
                self.li += 1;
            }
        }
        Ok(b)
    }

    fn read_exact<B: Block>(&mut self, dev: &mut B, out: &mut [u8]) -> Result<()> {
        for b in out.iter_mut() {
            *b = self.next_byte(dev)?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Sequential writer over a file's payload region.
// --------------------------------------------------------------------------

struct DataWriter<'a> {
    blocks: &'a [u8],
    li: usize,
    fib: usize,
    off: usize,
    frame: [u8; FRAME_SIZE],
}

impl<'a> DataWriter<'a> {
    fn new(blocks: &'a [u8]) -> Self {
        DataWriter {
            blocks,
            li: 0,
            fib: 2,
            off: 0,
            frame: [0; FRAME_SIZE],
        }
    }

    fn push<B: Block>(&mut self, dev: &mut B, byte: u8) -> Result<()> {
        self.frame[self.off] = byte;
        self.off += 1;
        if self.off == FRAME_SIZE {
            self.flush_frame(dev)?;
        }
        Ok(())
    }

    fn flush_frame<B: Block>(&mut self, dev: &mut B) -> Result<()> {
        if self.li >= self.blocks.len() {
            return Err(Error::NoSpace);
        }
        let phys = self.blocks[self.li] as u16 * FRAMES_PER_BLOCK as u16 + self.fib as u16;
        dev.write_frame(phys, &self.frame)?;
        self.off = 0;
        self.frame = [0; FRAME_SIZE];
        self.fib += 1;
        if self.fib == FRAMES_PER_BLOCK {
            self.fib = 0;
            self.li += 1;
        }
        Ok(())
    }

    /// Flush a final partial frame (zero-padded).
    fn finish<B: Block>(&mut self, dev: &mut B) -> Result<()> {
        if self.off > 0 {
            self.flush_frame(dev)?;
        }
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Filesystem operations on Card.
// --------------------------------------------------------------------------

impl<B: Block> Card<B> {
    fn read_dir(&mut self, index: usize) -> Result<[u8; FRAME_SIZE]> {
        let mut f = [0u8; FRAME_SIZE];
        self.dev.read_frame(1 + index as u16, &mut f)?;
        Ok(f)
    }

    fn write_dir(&mut self, index: usize, frame: &mut [u8; FRAME_SIZE]) -> Result<()> {
        frame[E_CHK] = checksum(frame);
        self.dev.write_frame(1 + index as u16, frame)
    }

    /// True if the card carries the `MC` directory header.
    pub fn is_formatted(&mut self) -> Result<bool> {
        let mut f = [0u8; FRAME_SIZE];
        self.dev.read_frame(0, &mut f)?;
        Ok(f[0] == b'M' && f[1] == b'C')
    }

    /// Lay down a fresh, empty directory (byte-identical to a BIOS format's
    /// system area). Data blocks are left as-is; the directory marks them free.
    pub fn format(&mut self) -> Result<()> {
        // Frame 0: "MC" header.
        let mut hdr = [0u8; FRAME_SIZE];
        hdr[0] = b'M';
        hdr[1] = b'C';
        hdr[E_CHK] = checksum(&hdr);
        self.dev.write_frame(0, &hdr)?;

        // Frames 1..=15: free directory entries.
        for i in 0..DATA_BLOCKS {
            let mut e = [0u8; FRAME_SIZE];
            e[E_STATE] = ST_FREE;
            e[E_LINK] = 0xFF;
            e[E_LINK + 1] = 0xFF;
            self.write_dir(i, &mut e)?;
        }

        // Frames 16..=35: broken-sector list (all "no broken sector").
        for frame in 16..36u16 {
            let mut f = [0u8; FRAME_SIZE];
            f[0] = 0xFF;
            f[1] = 0xFF;
            f[2] = 0xFF;
            f[3] = 0xFF;
            f[8] = 0xFF;
            f[9] = 0xFF;
            self.dev.write_frame(frame, &f)?;
        }
        // Remaining system frames 36..=63: cleared.
        let zero = [0u8; FRAME_SIZE];
        for frame in 36..FRAMES_PER_BLOCK as u16 {
            self.dev.write_frame(frame, &zero)?;
        }
        Ok(())
    }

    /// Number of free 8 KiB blocks.
    pub fn free_blocks(&mut self) -> Result<usize> {
        let mut n = 0;
        for i in 0..DATA_BLOCKS {
            if is_free(self.read_dir(i)?[E_STATE]) {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Find the first-block directory index of `name`, if present.
    fn find(&mut self, name: &[u8]) -> Result<Option<usize>> {
        for i in 0..DATA_BLOCKS {
            let e = self.read_dir(i)?;
            if e[E_STATE] == ST_FIRST && entry_name_eq(&e, name) {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// Follow the link chain from a first-block entry.
    fn chain(&mut self, first: usize) -> Result<Chain> {
        let mut blocks = [0u8; DATA_BLOCKS];
        let mut len = 0;
        let mut idx = first;
        loop {
            if len >= DATA_BLOCKS {
                return Err(Error::Corrupt); // loop / overrun
            }
            let e = self.read_dir(idx)?;
            blocks[len] = (idx + 1) as u8; // physical block = dir index + 1
            len += 1;
            let link = u16::from_le_bytes([e[E_LINK], e[E_LINK + 1]]);
            match e[E_STATE] {
                ST_LAST => break,
                ST_FIRST | ST_MIDDLE => {
                    if link == LINK_NONE {
                        break; // single-block file (first with no link)
                    }
                    if link as usize >= DATA_BLOCKS {
                        return Err(Error::Corrupt);
                    }
                    idx = link as usize;
                }
                _ => return Err(Error::Corrupt),
            }
        }
        Ok(Chain { blocks, len })
    }

    /// List all files. Returns the number written to `out` (capped at its len).
    pub fn list(&mut self, out: &mut [Entry]) -> Result<usize> {
        let mut n = 0;
        for i in 0..DATA_BLOCKS {
            let e = self.read_dir(i)?;
            if e[E_STATE] != ST_FIRST {
                continue;
            }
            if n >= out.len() {
                break;
            }
            let mut name = [0u8; MAX_NAME + 1];
            let name_len = copy_entry_name(&e, &mut name);
            let blocks = self.chain(i)?.len as u8;
            out[n] = Entry {
                name,
                name_len,
                blocks,
            };
            n += 1;
        }
        Ok(n)
    }

    /// Delete `name`, freeing its blocks. `Ok(())` even if it did not exist.
    pub fn delete(&mut self, name: &str) -> Result<()> {
        let name = name.as_bytes();
        let Some(first) = self.find(name)? else {
            return Ok(());
        };
        let chain = self.chain(first)?;
        for k in 0..chain.len {
            let index = chain.blocks[k] as usize - 1;
            let mut e = [0u8; FRAME_SIZE];
            e[E_STATE] = ST_FREE;
            e[E_LINK] = 0xFF;
            e[E_LINK + 1] = 0xFF;
            self.write_dir(index, &mut e)?;
        }
        Ok(())
    }

    /// Write a save (uncompressed), overwriting any existing file of the same
    /// name. `name` is the BIOS file name (product code + label, <= 20 ASCII);
    /// `title` is the human-readable label shown by the card manager (<= 32
    /// ASCII). Uses the generic placeholder icon; see [`Card::write_with_icon`]
    /// for a game-specific one.
    pub fn write(&mut self, name: &str, title: &str, data: &[u8]) -> Result<()> {
        self.write_inner(name, title, data, false, data.len() as u32, &Icon::default())
    }

    /// Like [`Card::write`], with a custom [`Icon`] shown in the card manager
    /// instead of the generic placeholder.
    pub fn write_with_icon(
        &mut self,
        name: &str,
        title: &str,
        data: &[u8],
        icon: &Icon,
    ) -> Result<()> {
        self.write_inner(name, title, data, false, data.len() as u32, icon)
    }

    /// Write a save, compressing the payload with LZSS when it helps. `scratch`
    /// receives the compressor output and must be at least `data.len()` bytes;
    /// if compression does not shrink the data, it is stored as-is.
    #[cfg(feature = "compress")]
    pub fn write_compressed(
        &mut self,
        name: &str,
        title: &str,
        data: &[u8],
        scratch: &mut [u8],
    ) -> Result<()> {
        self.write_compressed_with_icon(name, title, data, scratch, &Icon::default())
    }

    /// Like [`Card::write_compressed`], with a custom [`Icon`] shown in the
    /// card manager instead of the generic placeholder.
    #[cfg(feature = "compress")]
    pub fn write_compressed_with_icon(
        &mut self,
        name: &str,
        title: &str,
        data: &[u8],
        scratch: &mut [u8],
        icon: &Icon,
    ) -> Result<()> {
        match crate::compress::compress(data, scratch) {
            Some(clen) if clen < data.len() => {
                self.write_inner(name, title, &scratch[..clen], true, data.len() as u32, icon)
            }
            _ => self.write_inner(name, title, data, false, data.len() as u32, icon),
        }
    }

    fn write_inner(
        &mut self,
        name: &str,
        title: &str,
        stored: &[u8],
        compressed: bool,
        raw_len: u32,
        icon: &Icon,
    ) -> Result<()> {
        let name = name.as_bytes();
        validate_name(name)?;
        validate_title(title)?;

        let total = CONTAINER_LEN + stored.len();
        let need = blocks_for(total);
        if need > DATA_BLOCKS {
            return Err(Error::NoSpace);
        }

        // Overwrite: free any existing same-name file first.
        if let Some(first) = self.find(name)? {
            let chain = self.chain(first)?;
            for k in 0..chain.len {
                let mut e = [0u8; FRAME_SIZE];
                e[E_STATE] = ST_FREE;
                e[E_LINK] = 0xFF;
                e[E_LINK + 1] = 0xFF;
                self.write_dir(chain.blocks[k] as usize - 1, &mut e)?;
            }
        }

        // Allocate `need` free directory indices.
        let mut alloc = [0u8; DATA_BLOCKS];
        let mut got = 0;
        for i in 0..DATA_BLOCKS {
            if got == need {
                break;
            }
            if is_free(self.read_dir(i)?[E_STATE]) {
                alloc[got] = i as u8;
                got += 1;
            }
        }
        if got < need {
            return Err(Error::NoSpace);
        }

        // Physical block numbers (dir index + 1) for the data cursor.
        let mut phys = [0u8; DATA_BLOCKS];
        for k in 0..need {
            phys[k] = alloc[k] + 1;
        }

        // Title + icon frames in the first block.
        self.write_title(phys[0], title, icon)?;

        // Container header + payload across the chain.
        let mut hdr = [0u8; CONTAINER_LEN];
        hdr[0..4].copy_from_slice(&CONTAINER_MAGIC);
        hdr[4] = if compressed { FLAG_COMPRESSED } else { 0 };
        write_u32(&mut hdr[8..], raw_len);
        write_u32(&mut hdr[12..], stored.len() as u32);

        let mut w = DataWriter::new(&phys[..need]);
        for &b in hdr.iter() {
            w.push(&mut self.dev, b)?;
        }
        for &b in stored.iter() {
            w.push(&mut self.dev, b)?;
        }
        w.finish(&mut self.dev)?;

        // Directory entries with the link chain, name + size in the first.
        for k in 0..need {
            let mut e = [0u8; FRAME_SIZE];
            e[E_STATE] = if k == 0 {
                ST_FIRST // a single-block file is FIRST with a terminal link
            } else if k == need - 1 {
                ST_LAST
            } else {
                ST_MIDDLE
            };
            // Link points at the NEXT block's directory index.
            let link = if k + 1 < need {
                alloc[k + 1] as u16
            } else {
                LINK_NONE
            };
            e[E_LINK] = link as u8;
            e[E_LINK + 1] = (link >> 8) as u8;
            if k == 0 {
                // Real PS1 format: size is the block-aligned allocation, not
                // the raw payload length (`total`) -- the BIOS validates this
                // against the block count and hides entries where it disagrees.
                write_u32(&mut e[E_SIZE..], (need * BLOCK_BYTES) as u32);
                let n = name.len().min(MAX_NAME);
                e[E_NAME..E_NAME + n].copy_from_slice(&name[..n]);
            }
            self.write_dir(alloc[k] as usize, &mut e)?;
        }
        Ok(())
    }

    /// Write the `SC` title header + icon into a file's first block.
    fn write_title(&mut self, phys_block: u8, title: &str, icon: &Icon) -> Result<()> {
        let mut hdr = [0u8; FRAME_SIZE];
        hdr[0] = b'S';
        hdr[1] = b'C';
        hdr[T_ICON_FLAG..T_ICON_FLAG + 2].copy_from_slice(&ICON_FLAG_STATIC.to_le_bytes());
        // Title as Shift-JIS: ASCII is a single-byte subset, copied verbatim.
        let tb = title.as_bytes();
        let n = tb.len().min(T_TITLE_LEN);
        hdr[T_TITLE..T_TITLE + n].copy_from_slice(&tb[..n]);
        // Icon palette.
        for (i, c) in icon.clut.iter().enumerate() {
            hdr[T_CLUT + i * 2] = *c as u8;
            hdr[T_CLUT + i * 2 + 1] = (*c >> 8) as u8;
        }
        let base = phys_block as u16 * FRAMES_PER_BLOCK as u16;
        self.dev.write_frame(base, &hdr)?;

        // Icon pixels in the next frame.
        self.dev.write_frame(base + 1, &icon.pixels)?;
        Ok(())
    }

    /// Read a save into `buf`. Returns the payload length. Transparently
    /// decompresses if the save was written with [`Card::write_compressed`]
    /// (requires the `compress` feature).
    pub fn read(&mut self, name: &str, buf: &mut [u8]) -> Result<usize> {
        let name = name.as_bytes();
        let first = self.find(name)?.ok_or(Error::NotFound)?;
        let chain = self.chain(first)?;
        let blocks = &chain.blocks[..chain.len];

        let mut cur = DataCursor::new(blocks);
        let mut hdr = [0u8; CONTAINER_LEN];
        cur.read_exact(&mut self.dev, &mut hdr)?;
        if hdr[0..4] != CONTAINER_MAGIC {
            return Err(Error::BadContainer);
        }
        let flags = hdr[4];
        let raw_len = read_u32(&hdr[8..]) as usize;
        let stored_len = read_u32(&hdr[12..]) as usize;
        if buf.len() < raw_len {
            return Err(Error::BufferTooSmall);
        }

        if flags & FLAG_COMPRESSED == 0 {
            // Writer invariant: uncompressed saves store the payload verbatim,
            // so the two lengths must agree. A mismatch means a corrupt header;
            // reject it instead of trusting stored_len (which was never checked
            // against buf and could slice out of bounds).
            if stored_len != raw_len {
                return Err(Error::BadContainer);
            }
            cur.read_exact(&mut self.dev, &mut buf[..stored_len])?;
            return Ok(stored_len);
        }

        // Compressed: stream the compressed bytes straight into the LZSS decoder,
        // which uses `buf` itself as the history window (no scratch needed).
        #[cfg(feature = "compress")]
        {
            let mut remaining = stored_len;
            let produced = crate::compress::decompress_from(
                || {
                    if remaining == 0 {
                        return None;
                    }
                    remaining -= 1;
                    cur.next_byte(&mut self.dev).ok()
                },
                &mut buf[..raw_len],
            )
            .ok_or(Error::Compression)?;
            if produced != raw_len {
                return Err(Error::Compression);
            }
            Ok(raw_len)
        }
        #[cfg(not(feature = "compress"))]
        {
            let _ = stored_len;
            Err(Error::Compression)
        }
    }
}

// --------------------------------------------------------------------------
// Name / title helpers.
// --------------------------------------------------------------------------

fn validate_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(Error::BadName);
    }
    if name.iter().any(|&b| !(0x20..=0x7E).contains(&b)) {
        return Err(Error::BadName);
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<()> {
    if title.is_empty() || title.len() > 32 {
        return Err(Error::BadName);
    }
    Ok(())
}

fn entry_name_eq(entry: &[u8; FRAME_SIZE], name: &[u8]) -> bool {
    let mut buf = [0u8; MAX_NAME + 1];
    let len = copy_entry_name(entry, &mut buf) as usize;
    &buf[..len] == name
}

fn copy_entry_name(entry: &[u8; FRAME_SIZE], out: &mut [u8; MAX_NAME + 1]) -> u8 {
    let mut len = 0;
    while len < MAX_NAME {
        let b = entry[E_NAME + len];
        if b == 0 {
            break;
        }
        out[len] = b;
        len += 1;
    }
    len as u8
}

// --------------------------------------------------------------------------
// Save icon (16x16 4bpp + a 16-colour BGR555 palette).
// --------------------------------------------------------------------------

/// The pixel data + palette shown for a save in the card manager. Build a
/// custom one with [`Icon::new`], or use [`Icon::default`] for a generic
/// placeholder.
#[derive(Copy, Clone)]
pub struct Icon {
    /// 16-entry BGR555 palette, index 0 unused (transparent).
    pub clut: [u16; 16],
    /// 16x16 pixels, 4bpp packed two-per-byte (low nibble = left pixel).
    pub pixels: [u8; FRAME_SIZE],
}

impl Icon {
    /// Build an icon from a palette and a 16x16 grid of palette indices
    /// (0..16, row-major).
    pub fn new(clut: [u16; 16], indices: &[[u8; 16]; 16]) -> Self {
        let mut pixels = [0u8; FRAME_SIZE];
        for (y, row) in indices.iter().enumerate() {
            for (x, &idx) in row.iter().enumerate() {
                let byte = (y * 16 + x) / 2;
                if x & 1 == 0 {
                    pixels[byte] = (pixels[byte] & 0xF0) | (idx & 0x0F);
                } else {
                    pixels[byte] = (pixels[byte] & 0x0F) | ((idx & 0x0F) << 4);
                }
            }
        }
        Icon { clut, pixels }
    }
}

impl Default for Icon {
    /// A generic bordered blue square with a yellow accent corner.
    fn default() -> Self {
        let mut pixels = [0u8; FRAME_SIZE];
        default_icon(&mut pixels);
        Icon {
            clut: default_clut(),
            pixels,
        }
    }
}

/// 16-entry BGR555 palette: transparent, frame, fill, accent.
fn default_clut() -> [u16; 16] {
    let mut c = [0u16; 16];
    c[0] = 0x0000; // background
    c[1] = 0x7FFF; // white frame
    c[2] = 0x7C00; // blue fill (BGR555: blue in high bits)
    c[3] = 0x03FF; // yellow accent
    c
}

/// Draw a 16x16 4bpp icon: a bordered blue square with an accent corner.
fn default_icon(out: &mut [u8; FRAME_SIZE]) {
    for y in 0..16usize {
        for x in 0..16usize {
            let idx: u8 = if x == 0 || x == 15 || y == 0 || y == 15 {
                1 // frame
            } else if (2..=5).contains(&x) && (2..=3).contains(&y) {
                3 // accent label strip
            } else {
                2 // fill
            };
            let byte = (y * 16 + x) / 2;
            if x & 1 == 0 {
                out[byte] = (out[byte] & 0xF0) | idx; // low nibble = left pixel
            } else {
                out[byte] = (out[byte] & 0x0F) | (idx << 4); // high nibble = right
            }
        }
    }
}
