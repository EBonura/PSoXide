//! Streamed PXBSP worlds: the region format and the slotted resident map.
//!
//! Design 2026-10-08, sections 3.3 to 3.6 and milestone M6. Everything here
//! sits behind the `streaming` feature, so a consumer that does not stream
//! (quake-psx, the HL and CS runtimes) compiles none of it.
//!
//! # Shape
//!
//! A streamed world is one ordinary PXBSP v6 container, the *top*, plus one
//! payload blob per region.
//!
//! * The top holds the resident tree: one node per cut plane, whose children
//!   lead to a region subtree or, while the region is absent, to a *stub
//!   leaf*. It also holds the global material table, the entities and the
//!   [`StreamingIndex`] lump (directory, link patches, visibility lists).
//! * A region payload ([`RegionBuild`] / [`RegionView`]) is a self contained
//!   subtree: planes, vertices, faces, marks, leaves, render nodes, clip
//!   nodes for the two body hulls and its PVS rows. Every index in it is
//!   region local.
//! * The resident map ([`PxbspResidentMap::load_streamed`]) is the container
//!   with each geometry lump widened to `top + slots * cap` records. Installing
//!   a region copies it into one slot while adding the slot bases to every
//!   index (relocation) and then patches one child halfword in each of three
//!   top trees. Uninstalling patches them back to the stub.
//!
//! # Index spaces
//!
//! Render children use the PXBSP convention `-(leaf + 1)`. Global leaf 0 is
//! the shared solid sentinel, leaves `1..=R` are the stubs (region `r` is leaf
//! `r + 1`), leaves up to `1 + rpad` are solid padding so that the first slot
//! leaf starts on a byte boundary of the dense PVS bitmap, and slot `s` owns
//! leaves `1 + rpad + s * lcap ..` for its local leaves `1..=n`.
//!
//! # Collision
//!
//! The wire code of a stub leaf is `CONTENTS_UNRESIDENT` (-7), outside the
//! valid leaf range, so a loader without this feature rejects the container.
//! The resident image canonicalises it to `CONTENTS_SOLID`, and an unresident
//! clip child is `CONTENTS_SOLID` as well: unresident space is a wall for hull
//! 0, 1 and 2 with no instruction added to the trace loops.
//!
//! # Visibility
//!
//! A region's PVS rows use the canonical rank layout: bit
//! `rank * lcap + (local_leaf - 1)`, rank 0 being the region itself and the
//! other ranks the sorted ids of the rest of its visible set `V(R)`. Rows are
//! at most 1024 bytes. At run time [`PxbspResidentMap::streamed_leaf_visibility_into`]
//! expands a row into the dense virtual-leaf bitmap the renderer already
//! walks (bit `leaf - 1`), so the face marking and node culling loops are
//! shared with the legacy path.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::Cell;

use super::*;

/// `PXSI`, the first word of the StreamingIndex lump.
pub const STREAMING_INDEX_MAGIC: u32 = u32::from_le_bytes(*b"PXSI");
pub const STREAMING_INDEX_VERSION: u16 = 1;
/// `PXRG`, the first word of a region payload.
pub const REGION_MAGIC: u32 = u32::from_le_bytes(*b"PXRG");
pub const REGION_VERSION: u16 = 1;
pub const STREAMING_INDEX_HEADER_BYTES: usize = 48;
pub const REGION_ENTRY_BYTES: usize = 40;
pub const REGION_HEADER_BYTES: usize = 64;
/// Bytes per CD sector; region payloads start on one.
pub const SECTOR_BYTES: u32 = 2048;
/// Dense PVS bitmap the renderer owns: virtual leaves must fit in it.
pub const MAX_VIRTUAL_LEAVES: usize = PXBSP_MAX_VISIBILITY_BYTES * 8;
/// Stub leaf wire contents, re-exported for the cooker.
pub use crate::collision::CONTENTS_UNRESIDENT;
const NO_SLOT: u16 = u16::MAX;

const PLANE_BYTES: usize = 12;
const VERTEX_BYTES: usize = 12;
const FACE_BYTES: usize = 10;
const MARK_BYTES: usize = 2;
const LEAF_BYTES: usize = 14;
const NODE_BYTES: usize = 16;
const CLIPNODE_BYTES: usize = 6;

use crate::pxbsp::PXBSP_MAX_VISIBILITY_BYTES;

/// 32-bit FNV-1a, the checksum of region payloads.
pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = Fnv32::new();
    hash.update(bytes);
    hash.finish()
}

/// Incremental 32-bit FNV-1a: feed a payload in slices across frames and
/// compare [`Fnv32::finish`] with [`RegionEntry::fnv`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fnv32(u32);

impl Fnv32 {
    pub const fn new() -> Self {
        Self(0x811c_9dc5)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        for &byte in bytes {
            hash ^= u32::from(byte);
            hash = hash.wrapping_mul(0x0100_0193);
        }
        self.0 = hash;
    }

    pub const fn finish(self) -> u32 {
        self.0
    }
}

impl Default for Fnv32 {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a streamed container, index, payload or install request was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamError {
    /// The StreamingIndex lump is malformed; the text names the rule.
    BadIndex(&'static str),
    /// A region payload is malformed.
    BadRegion(&'static str),
    /// A region payload index points outside its own tables.
    BadReference(&'static str),
    BadChecksum,
    ShortPayload,
    RegionOutOfRange,
    SlotOutOfRange,
    SlotBusy,
    AlreadyInstalled,
    NotInstalled,
    /// A region table is larger than the slot capacity the index declares.
    ExceedsSlot(&'static str),
    /// The map was not loaded with [`PxbspResidentMap::load_streamed`].
    NotStreamed,
}

/// Failure while loading a streamed container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamLoadError<E> {
    Map(PxbspMapLoadError<E>),
    Stream(StreamError),
    /// Zero slots, or the virtual leaf space would not fit the renderer's
    /// dense PVS bitmap.
    Slots,
}

/// Capacity of one resident slot, per lump. `leaves` is a power of two and a
/// multiple of eight so a region's leaves are a whole number of PVS bytes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SlotCaps {
    pub faces: u16,
    pub vertices: u16,
    pub planes: u16,
    pub marks: u16,
    pub nodes: u16,
    pub clip_nodes: u16,
    pub leaves: u16,
    pub vis_bytes: u32,
}

/// Records of each lump that live in the top container (always resident).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TopCounts {
    pub planes: u16,
    pub vertices: u16,
    pub faces: u16,
    pub marks: u16,
    pub leaves: u16,
    pub nodes: u16,
    pub clip_nodes: u16,
}

/// One region's directory entry: where it lives, how to patch it in, and
/// which regions its PVS rows span.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegionEntry {
    /// First sector of the payload inside the region pack.
    pub sector_start: u32,
    /// Payload length in bytes (header plus body, before sector padding).
    pub payload_bytes: u32,
    /// FNV-1a over the whole payload.
    pub fnv: u32,
    pub mins: [i16; 3],
    pub maxs: [i16; 3],
    /// Top node (render), top clip node (hull 1) and top clip node (hull 2)
    /// whose child stands for this region.
    pub parents: [u16; 3],
    /// Bit `i` is the child side (0 front, 1 back) of `parents[i]`.
    pub sides: u8,
    /// Offset into [`StreamingIndex::vis_lists`] and length of `V(R)`.
    pub vis_offset: u32,
    pub vis_count: u16,
}

impl RegionEntry {
    pub const fn side(&self, patch: usize) -> usize {
        ((self.sides >> patch) & 1) as usize
    }

    pub const fn sectors(&self) -> u32 {
        self.payload_bytes.div_ceil(SECTOR_BYTES)
    }
}

/// Decoded StreamingIndex lump.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamingIndex {
    pub caps: SlotCaps,
    pub top: TopCounts,
    pub regions: Vec<RegionEntry>,
    /// Concatenated `V(R)` lists, each starting with the region itself.
    pub vis_lists: Vec<u16>,
}

impl StreamingIndex {
    /// Stub and padding leaves before the first slot: `R` rounded up to eight.
    pub fn top_leaf_pad(&self) -> usize {
        self.regions.len().div_ceil(8) * 8
    }

    /// `V(R)` of `region`: the region first, then the rest ascending.
    pub fn vis_list(&self, region: usize) -> &[u16] {
        let entry = &self.regions[region];
        &self.vis_lists[entry.vis_offset as usize..][..entry.vis_count as usize]
    }

    /// Bytes of one decompressed PVS row of `region`.
    pub fn row_bytes(&self, region: usize) -> usize {
        self.regions[region].vis_count as usize * self.caps.leaves as usize / 8
    }

    pub fn encoded_len(&self) -> usize {
        STREAMING_INDEX_HEADER_BYTES
            + self.regions.len() * REGION_ENTRY_BYTES
            + (self.vis_lists.len() * 2).div_ceil(4) * 4
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&STREAMING_INDEX_MAGIC.to_le_bytes());
        out.extend_from_slice(&STREAMING_INDEX_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(self.regions.len() as u16).to_le_bytes());
        out.extend_from_slice(&self.caps.leaves.to_le_bytes());
        out.extend_from_slice(&(self.top_leaf_pad() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        for value in [
            self.caps.faces,
            self.caps.vertices,
            self.caps.planes,
            self.caps.marks,
            self.caps.nodes,
            self.caps.clip_nodes,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&self.caps.vis_bytes.to_le_bytes());
        for value in [
            self.top.planes,
            self.top.vertices,
            self.top.faces,
            self.top.marks,
            self.top.leaves,
            self.top.nodes,
            self.top.clip_nodes,
            0,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        debug_assert_eq!(out.len(), STREAMING_INDEX_HEADER_BYTES);
        for entry in &self.regions {
            out.extend_from_slice(&entry.sector_start.to_le_bytes());
            out.extend_from_slice(&entry.payload_bytes.to_le_bytes());
            out.extend_from_slice(&entry.fnv.to_le_bytes());
            for value in entry.mins.into_iter().chain(entry.maxs) {
                out.extend_from_slice(&value.to_le_bytes());
            }
            for value in entry.parents {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.push(entry.sides);
            out.push(0);
            out.extend_from_slice(&entry.vis_offset.to_le_bytes());
            out.extend_from_slice(&entry.vis_count.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        for &id in &self.vis_lists {
            out.extend_from_slice(&id.to_le_bytes());
        }
        out.resize(self.encoded_len(), 0);
        out
    }

    /// Parse and fully validate a StreamingIndex lump.
    pub fn parse(bytes: &[u8]) -> Result<Self, StreamError> {
        let bad = StreamError::BadIndex;
        if bytes.len() < STREAMING_INDEX_HEADER_BYTES {
            return Err(bad("lump shorter than its header"));
        }
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        if u32_at(0) != STREAMING_INDEX_MAGIC {
            return Err(bad("bad magic"));
        }
        if u16_at(4) != STREAMING_INDEX_VERSION {
            return Err(bad("unsupported version"));
        }
        if u16_at(6) != 0 {
            return Err(bad("unknown flags"));
        }
        let region_count = u16_at(8) as usize;
        let leaves = u16_at(10);
        let pad = u16_at(12) as usize;
        if region_count < 2 {
            return Err(bad("a streamed world needs at least two regions"));
        }
        if leaves < 8 || !leaves.is_power_of_two() {
            return Err(bad("leaf cap must be a power of two of at least eight"));
        }
        if pad != region_count.div_ceil(8) * 8 {
            return Err(bad("stub padding does not match the region count"));
        }
        let caps = SlotCaps {
            faces: u16_at(16),
            vertices: u16_at(18),
            planes: u16_at(20),
            marks: u16_at(22),
            nodes: u16_at(24),
            clip_nodes: u16_at(26),
            leaves,
            vis_bytes: u32_at(28),
        };
        let top = TopCounts {
            planes: u16_at(32),
            vertices: u16_at(34),
            faces: u16_at(36),
            marks: u16_at(38),
            leaves: u16_at(40),
            nodes: u16_at(42),
            clip_nodes: u16_at(44),
        };
        if top.leaves as usize != 1 + pad {
            return Err(bad("top leaves are not the sentinel, stubs and padding"));
        }
        if top.nodes == 0 || top.clip_nodes == 0 {
            return Err(bad("the top trees are empty"));
        }
        let entries_end = STREAMING_INDEX_HEADER_BYTES + region_count * REGION_ENTRY_BYTES;
        if bytes.len() < entries_end {
            return Err(bad("directory is truncated"));
        }
        let mut regions = Vec::with_capacity(region_count);
        let mut vis_total = 0usize;
        for r in 0..region_count {
            let at = STREAMING_INDEX_HEADER_BYTES + r * REGION_ENTRY_BYTES;
            let mut mins = [0i16; 3];
            let mut maxs = [0i16; 3];
            for axis in 0..3 {
                mins[axis] = u16_at(at + 12 + axis * 2) as i16;
                maxs[axis] = u16_at(at + 18 + axis * 2) as i16;
                if mins[axis] > maxs[axis] {
                    return Err(bad("region bounds are inverted"));
                }
            }
            let entry = RegionEntry {
                sector_start: u32_at(at),
                payload_bytes: u32_at(at + 4),
                fnv: u32_at(at + 8),
                mins,
                maxs,
                parents: [u16_at(at + 24), u16_at(at + 26), u16_at(at + 28)],
                sides: bytes[at + 30],
                vis_offset: u32_at(at + 32),
                vis_count: u16_at(at + 36),
            };
            if bytes[at + 31] != 0 || u16_at(at + 38) != 0 || entry.sides & !7 != 0 {
                return Err(bad("reserved directory bits are set"));
            }
            if entry.parents[0] >= top.nodes
                || entry.parents[1] >= top.clip_nodes
                || entry.parents[2] >= top.clip_nodes
            {
                return Err(bad("link patch parent is outside the top trees"));
            }
            if entry.payload_bytes < REGION_HEADER_BYTES as u32 {
                return Err(bad("region payload shorter than its header"));
            }
            if entry.vis_count == 0 {
                return Err(bad("a region must see at least itself"));
            }
            if entry.vis_offset as usize != vis_total {
                return Err(bad("visibility lists are not packed in region order"));
            }
            if entry.vis_count as usize * leaves as usize / 8 > PXBSP_MAX_VISIBILITY_BYTES {
                return Err(bad("PVS row wider than 1024 bytes"));
            }
            vis_total += entry.vis_count as usize;
            regions.push(entry);
        }
        if bytes.len() != entries_end + (vis_total * 2).div_ceil(4) * 4 {
            return Err(bad("lump length does not match the directory"));
        }
        let mut vis_lists = Vec::with_capacity(vis_total);
        for i in 0..vis_total {
            vis_lists.push(u16_at(entries_end + i * 2));
        }
        for (r, entry) in regions.iter().enumerate() {
            let list = &vis_lists[entry.vis_offset as usize..][..entry.vis_count as usize];
            if list[0] as usize != r {
                return Err(bad("a visibility list must start with its own region"));
            }
            if list[1..].windows(2).any(|pair| pair[0] >= pair[1])
                || list[1..]
                    .iter()
                    .any(|&q| q as usize >= region_count || q as usize == r)
            {
                return Err(bad("visibility list is unsorted or out of range"));
            }
        }
        Ok(Self {
            caps,
            top,
            regions,
            vis_lists,
        })
    }
}

// ---- region payload -------------------------------------------------------

/// Section placement shared by the encoder and the parser.
#[derive(Clone, Copy, Debug)]
struct RegionLayout {
    /// (start, len) of planes, vertices, faces, marks, leaves, nodes,
    /// clipnodes and visibility, relative to the payload start.
    sections: [(usize, usize); 8],
    end: usize,
}

fn region_layout(counts: &[usize; 7], vis_bytes: usize) -> RegionLayout {
    let sizes = [
        counts[0] * PLANE_BYTES,
        counts[1] * VERTEX_BYTES,
        counts[2] * FACE_BYTES,
        counts[3] * MARK_BYTES,
        counts[4] * LEAF_BYTES,
        counts[5] * NODE_BYTES,
        counts[6] * CLIPNODE_BYTES,
        vis_bytes,
    ];
    let mut sections = [(0usize, 0usize); 8];
    let mut cursor = REGION_HEADER_BYTES;
    for (slot, len) in sections.iter_mut().zip(sizes) {
        let start = cursor.div_ceil(4) * 4;
        *slot = (start, len);
        cursor = start + len;
    }
    RegionLayout {
        sections,
        end: cursor,
    }
}

/// One region as the cooker produces it, in wire bytes with region-local
/// indices. See the module docs for the index conventions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RegionBuild {
    pub id: u16,
    /// `|V(R)|`, the rank count of the PVS rows.
    pub vis_count: u16,
    /// 12-byte [`CompactPlane`] records.
    pub planes: Vec<u8>,
    pub vertices: Vec<u8>,
    pub faces: Vec<u8>,
    pub marks: Vec<u8>,
    pub leaves: Vec<u8>,
    pub nodes: Vec<u8>,
    pub clip_nodes: Vec<u8>,
    pub vis: Vec<u8>,
    /// Render subtree root as a child value: a local node, or a leaf code.
    pub render_root: i16,
    /// Hull 1 and hull 2 subtree roots as clip child values.
    pub clip_roots: [i16; 2],
}

impl RegionBuild {
    /// Serialise header and body. The result is not sector padded.
    pub fn encode(&self) -> Vec<u8> {
        let counts = [
            self.planes.len() / PLANE_BYTES,
            self.vertices.len() / VERTEX_BYTES,
            self.faces.len() / FACE_BYTES,
            self.marks.len() / MARK_BYTES,
            self.leaves.len() / LEAF_BYTES,
            self.nodes.len() / NODE_BYTES,
            self.clip_nodes.len() / CLIPNODE_BYTES,
        ];
        let layout = region_layout(&counts, self.vis.len());
        let mut out = alloc::vec![0u8; layout.end];
        let bodies: [&[u8]; 8] = [
            &self.planes,
            &self.vertices,
            &self.faces,
            &self.marks,
            &self.leaves,
            &self.nodes,
            &self.clip_nodes,
            &self.vis,
        ];
        for ((start, len), body) in layout.sections.iter().zip(bodies) {
            debug_assert_eq!(*len, body.len());
            out[*start..*start + *len].copy_from_slice(body);
        }
        let mut put16 =
            |at: usize, value: u16| out[at..at + 2].copy_from_slice(&value.to_le_bytes());
        put16(4, REGION_VERSION);
        put16(8, self.id);
        put16(10, self.vis_count);
        for (i, count) in counts.iter().enumerate() {
            put16(12 + i * 2, *count as u16);
        }
        put16(32, self.render_root as u16);
        put16(34, self.clip_roots[0] as u16);
        put16(36, self.clip_roots[1] as u16);
        out[0..4].copy_from_slice(&REGION_MAGIC.to_le_bytes());
        out[28..32].copy_from_slice(&(self.vis.len() as u32).to_le_bytes());
        let body_len = (layout.end - REGION_HEADER_BYTES) as u32;
        out[40..44].copy_from_slice(&body_len.to_le_bytes());
        let fnv = fnv1a32(&out[REGION_HEADER_BYTES..]);
        out[44..48].copy_from_slice(&fnv.to_le_bytes());
        out
    }
}

/// Validated, zero-copy view of one region payload.
#[derive(Clone, Copy, Debug)]
pub struct RegionView<'a> {
    pub id: u16,
    pub vis_count: u16,
    pub counts: [usize; 7],
    pub planes: &'a [u8],
    pub vertices: &'a [u8],
    pub faces: &'a [u8],
    pub marks: &'a [u8],
    pub leaves: &'a [u8],
    pub nodes: &'a [u8],
    pub clip_nodes: &'a [u8],
    pub vis: &'a [u8],
    pub render_root: i16,
    pub clip_roots: [i16; 2],
    head: RegionHead,
}

impl<'a> RegionView<'a> {
    /// Parse the header, check the layout and the body checksum.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, StreamError> {
        let head = parse_head(bytes)?;
        let layout = &head.layout;
        if bytes.len() < layout.end {
            return Err(StreamError::ShortPayload);
        }
        if fnv1a32(&bytes[REGION_HEADER_BYTES..layout.end]) != head.body_fnv {
            return Err(StreamError::BadChecksum);
        }
        let section =
            |i: usize| &bytes[layout.sections[i].0..layout.sections[i].0 + layout.sections[i].1];
        Ok(Self {
            id: head.id,
            vis_count: head.vis_count,
            counts: head.counts,
            planes: section(0),
            vertices: section(1),
            faces: section(2),
            marks: section(3),
            leaves: section(4),
            nodes: section(5),
            clip_nodes: section(6),
            vis: section(7),
            render_root: head.render_root,
            clip_roots: head.clip_roots,
            head,
        })
    }
}

/// The decoded 64 byte region header: everything an install needs to place
/// and check the records that follow, before any of them has arrived.
#[derive(Clone, Copy, Debug)]
struct RegionHead {
    id: u16,
    vis_count: u16,
    counts: [usize; 7],
    vis_len: usize,
    layout: RegionLayout,
    render_root: i16,
    clip_roots: [i16; 2],
    body_fnv: u32,
}

/// Decode and check a region header (`bytes` holds at least the 64 bytes).
fn parse_head(bytes: &[u8]) -> Result<RegionHead, StreamError> {
    let bad = StreamError::BadRegion;
    if bytes.len() < REGION_HEADER_BYTES {
        return Err(StreamError::ShortPayload);
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if u32_at(0) != REGION_MAGIC {
        return Err(bad("bad magic"));
    }
    if u16_at(4) != REGION_VERSION || u16_at(6) != 0 {
        return Err(bad("unsupported version or flags"));
    }
    let mut counts = [0usize; 7];
    for (i, count) in counts.iter_mut().enumerate() {
        *count = u16_at(12 + i * 2) as usize;
    }
    if u16_at(26) != 0 || bytes[48..REGION_HEADER_BYTES].iter().any(|&b| b != 0) {
        return Err(bad("reserved header bytes are set"));
    }
    let vis_len = u32_at(28) as usize;
    let layout = region_layout(&counts, vis_len);
    if u32_at(40) as usize != layout.end - REGION_HEADER_BYTES {
        return Err(bad("body length does not match the counts"));
    }
    Ok(RegionHead {
        id: u16_at(8),
        vis_count: u16_at(10),
        counts,
        vis_len,
        layout,
        render_root: u16_at(32) as i16,
        clip_roots: [u16_at(34) as i16, u16_at(36) as i16],
        body_fnv: u32_at(44),
    })
}

/// Reads region payloads out of a region pack through the engine's `ReadAt`
/// seam (a CD stream, an ISO file or a slice).
pub struct RegionLoader<R: ReadAt> {
    reader: R,
    pack_base: u32,
}

impl<R: ReadAt> RegionLoader<R> {
    /// `pack_base` is the byte offset of the region pack inside `reader`.
    pub const fn new(reader: R, pack_base: u32) -> Self {
        Self { reader, pack_base }
    }

    pub fn into_inner(self) -> R {
        self.reader
    }

    /// Read `region`'s payload into `buf` and verify it against the
    /// directory checksum. Returns the payload length.
    pub fn load(
        &mut self,
        index: &StreamingIndex,
        region: u16,
        buf: &mut [u8],
    ) -> Result<usize, RegionReadError<R::Error>> {
        let entry = index
            .regions
            .get(region as usize)
            .ok_or(RegionReadError::Stream(StreamError::RegionOutOfRange))?;
        let len = entry.payload_bytes as usize;
        if buf.len() < len {
            return Err(RegionReadError::Stream(StreamError::ShortPayload));
        }
        let offset = entry
            .sector_start
            .checked_mul(SECTOR_BYTES)
            .and_then(|o| o.checked_add(self.pack_base))
            .ok_or(RegionReadError::Stream(StreamError::RegionOutOfRange))?;
        self.reader
            .read_exact_at(offset, &mut buf[..len])
            .map_err(RegionReadError::Read)?;
        if fnv1a32(&buf[..len]) != entry.fnv {
            return Err(RegionReadError::Stream(StreamError::BadChecksum));
        }
        Ok(len)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegionReadError<E> {
    Read(E),
    Stream(StreamError),
}

// ---- resident state -------------------------------------------------------

/// Run-time state of a slotted map: which region sits in which slot.
#[derive(Debug)]
pub struct StreamState {
    index: StreamingIndex,
    slots: u16,
    rpad: usize,
    slot_of_region: Vec<u16>,
    region_of_slot: Vec<u16>,
    /// Counts installed in each slot (zero when free).
    slot_counts: Vec<[usize; 7]>,
    /// PVS ranks dropped because the region they name was not resident.
    pvs_missing: Cell<u32>,
}

impl StreamState {
    pub fn index(&self) -> &StreamingIndex {
        &self.index
    }

    /// Bytes one slot adds to the resident image (every lump's slot capacity).
    pub fn slot_bytes(&self) -> usize {
        let caps = &self.index.caps;
        caps.planes as usize * PLANE_BYTES
            + caps.vertices as usize * VERTEX_BYTES
            + caps.faces as usize * FACE_BYTES
            + caps.marks as usize * MARK_BYTES
            + caps.leaves as usize * LEAF_BYTES
            + caps.nodes as usize * NODE_BYTES
            + caps.clip_nodes as usize * CLIPNODE_BYTES
            + caps.vis_bytes as usize
    }

    pub fn slot_count(&self) -> usize {
        self.slots as usize
    }

    pub fn slot_of(&self, region: u16) -> Option<u16> {
        match self.slot_of_region.get(region as usize) {
            Some(&slot) if slot != NO_SLOT => Some(slot),
            _ => None,
        }
    }

    pub fn region_in_slot(&self, slot: u16) -> Option<u16> {
        match self.region_of_slot.get(slot as usize) {
            Some(&region) if region != NO_SLOT => Some(region),
            _ => None,
        }
    }

    pub fn is_resident(&self, region: u16) -> bool {
        self.slot_of(region).is_some()
    }

    /// Lowest free slot.
    pub fn free_slot(&self) -> Option<u16> {
        self.region_of_slot
            .iter()
            .position(|&r| r == NO_SLOT)
            .map(|s| s as u16)
    }

    pub fn resident_count(&self) -> usize {
        self.region_of_slot
            .iter()
            .filter(|&&r| r != NO_SLOT)
            .count()
    }

    /// Bits of the dense virtual-leaf PVS bitmap: `rpad + slots * lcap`.
    pub fn virtual_leaf_bits(&self) -> usize {
        self.rpad + self.slots as usize * self.index.caps.leaves as usize
    }

    /// PVS ranks skipped because their region was absent (should stay 0).
    pub fn pvs_missing(&self) -> u32 {
        self.pvs_missing.get()
    }

    fn lcap(&self) -> usize {
        self.index.caps.leaves as usize
    }

    /// Region owning global leaf `leaf`, whether stub or slot leaf.
    pub fn leaf_region(&self, leaf: usize) -> Option<u16> {
        let regions = self.index.regions.len();
        if leaf == 0 {
            return None;
        }
        if leaf <= regions {
            return Some((leaf - 1) as u16);
        }
        if leaf <= self.rpad {
            return None;
        }
        let slot = (leaf - 1 - self.rpad) / self.lcap();
        self.region_in_slot(slot as u16)
    }
}

/// An index problem found by [`PxbspResidentMap::check_integrity`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntegrityError {
    pub what: &'static str,
    pub index: usize,
}

fn err(what: &'static str, index: usize) -> IntegrityError {
    IntegrityError { what, index }
}

fn rd16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn wr16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// Region-local leaf code -> region-local leaf number (0 = solid sentinel).
const fn leaf_number(child: i16) -> usize {
    (-1i32 - child as i32) as usize
}

/// The bases a slot adds to region-local indices.
#[derive(Clone, Copy, Debug)]
struct Bases {
    planes: usize,
    vertices: usize,
    faces: usize,
    marks: usize,
    nodes: usize,
    clip_nodes: usize,
    vis: usize,
    /// Global leaf number of the slot's first local leaf (`L = 1`).
    leaf: usize,
}

impl StreamState {
    fn bases(&self, slot: usize) -> Bases {
        let caps = &self.index.caps;
        let top = &self.index.top;
        Bases {
            planes: top.planes as usize + slot * caps.planes as usize,
            vertices: top.vertices as usize + slot * caps.vertices as usize,
            faces: top.faces as usize + slot * caps.faces as usize,
            marks: top.marks as usize + slot * caps.marks as usize,
            nodes: top.nodes as usize + slot * caps.nodes as usize,
            clip_nodes: top.clip_nodes as usize + slot * caps.clip_nodes as usize,
            vis: slot * caps.vis_bytes as usize,
            leaf: 1 + self.rpad + slot * caps.leaves as usize,
        }
    }
}

impl Bases {
    fn render_child(&self, child: i16) -> i16 {
        if child >= 0 {
            (child as usize + self.nodes) as i16
        } else {
            let number = leaf_number(child);
            if number == 0 {
                child
            } else {
                (-1i32 - (self.leaf + number - 1) as i32) as i16
            }
        }
    }

    fn clip_child(&self, child: i16) -> i16 {
        if child >= 0 {
            (child as usize + self.clip_nodes) as i16
        } else {
            child
        }
    }
}

/// Counts against the slot caps, the roots and the PVS row width: the checks
/// that need no record.
fn check_head(head: &RegionHead, caps: &SlotCaps) -> Result<(), StreamError> {
    let [planes, vertices, faces, marks, leaves, nodes, clip_nodes] = head.counts;
    let refuse = StreamError::BadReference;
    if planes > caps.planes as usize {
        return Err(StreamError::ExceedsSlot("planes"));
    }
    if vertices > caps.vertices as usize {
        return Err(StreamError::ExceedsSlot("vertices"));
    }
    if faces > caps.faces as usize {
        return Err(StreamError::ExceedsSlot("faces"));
    }
    if marks > caps.marks as usize {
        return Err(StreamError::ExceedsSlot("marks"));
    }
    if leaves > caps.leaves as usize {
        return Err(StreamError::ExceedsSlot("leaves"));
    }
    if nodes > caps.nodes as usize {
        return Err(StreamError::ExceedsSlot("nodes"));
    }
    if clip_nodes > caps.clip_nodes as usize {
        return Err(StreamError::ExceedsSlot("clipnodes"));
    }
    if head.vis_len > caps.vis_bytes as usize {
        return Err(StreamError::ExceedsSlot("visibility"));
    }
    let row_bytes = head.vis_count as usize * caps.leaves as usize / 8;
    let root_ok = if head.render_root >= 0 {
        (head.render_root as usize) < nodes
    } else {
        leaf_number(head.render_root) <= leaves
    };
    if !root_ok {
        return Err(refuse("render root"));
    }
    for root in head.clip_roots {
        if root >= 0 && root as usize >= clip_nodes {
            return Err(refuse("clip root"));
        }
    }
    if row_bytes > PXBSP_MAX_VISIBILITY_BYTES {
        return Err(refuse("row width"));
    }
    // A row needs `row_bytes` once decompressed; the run-length stream is
    // checked when it is decoded, but an offset must at least be in range.
    Ok(())
}

/// The tables of a region payload, in payload order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Table {
    Planes,
    Vertices,
    Faces,
    Marks,
    Leaves,
    Nodes,
    Clip,
    Vis,
}

impl Table {
    const ALL: [Self; 8] = [
        Self::Planes,
        Self::Vertices,
        Self::Faces,
        Self::Marks,
        Self::Leaves,
        Self::Nodes,
        Self::Clip,
        Self::Vis,
    ];

    /// Bytes per record; the bulk tables (planes, vertices, visibility) are
    /// copied unchanged, so they count in single bytes.
    const fn record_bytes(self) -> usize {
        match self {
            Self::Faces => FACE_BYTES,
            Self::Marks => MARK_BYTES,
            Self::Leaves => LEAF_BYTES,
            Self::Nodes => NODE_BYTES,
            Self::Clip => CLIPNODE_BYTES,
            Self::Planes | Self::Vertices | Self::Vis => 1,
        }
    }
}

/// What a record is checked against: the region's own counts.
#[derive(Clone, Copy)]
struct CheckCtx {
    planes: usize,
    vertices: usize,
    faces: usize,
    marks: usize,
    leaves: usize,
    nodes: usize,
    clip_nodes: usize,
    vis_len: usize,
    materials: usize,
}

impl CheckCtx {
    fn new(head: &RegionHead, materials: usize) -> Self {
        let [planes, vertices, faces, marks, leaves, nodes, clip_nodes] = head.counts;
        Self {
            planes,
            vertices,
            faces,
            marks,
            leaves,
            nodes,
            clip_nodes,
            vis_len: head.vis_len,
            materials,
        }
    }
}

/// Check one record against the region's own tables. Returns the material
/// bit (materials below 32) a face uses.
fn check_record(table: Table, record: &[u8], ctx: &CheckCtx) -> Result<u32, StreamError> {
    let refuse = StreamError::BadReference;
    match table {
        Table::Faces => {
            let plane = rd16(record, 0) as usize;
            let first = rd16(record, 2) as usize;
            let texture = rd16(record, 4) as usize;
            let count = record[7] as usize;
            if plane >= ctx.planes
                || count < 3
                || first + count > ctx.vertices
                || texture >= ctx.materials
            {
                return Err(refuse("face"));
            }
            if record[8] > 64 || record[9] > 64 {
                return Err(refuse("face light style"));
            }
            return Ok(if texture < 32 { 1 << texture } else { 0 });
        }
        Table::Marks => {
            if rd16(record, 0) as usize >= ctx.faces {
                return Err(refuse("mark surface"));
            }
        }
        Table::Leaves => {
            let contents = record[0] as i8;
            if !(-6..=-1).contains(&contents) || contents == -2 {
                return Err(refuse("leaf contents"));
            }
            let first = rd16(record, 8) as usize;
            let count = rd16(record, 2) as usize;
            let vis = i32::from_le_bytes(record[4..8].try_into().unwrap());
            if first + count > ctx.marks || vis < -1 || (vis >= 0 && vis as usize >= ctx.vis_len) {
                return Err(refuse("leaf"));
            }
        }
        Table::Nodes => {
            let plane = rd16(record, 0) as usize;
            let first = rd16(record, 12) as usize;
            let count = rd16(record, 14) as usize;
            if plane >= ctx.planes || first + count > ctx.faces {
                return Err(refuse("node"));
            }
            for side in 0..2 {
                let child = rd16(record, 2 + side * 2) as i16;
                let good = if child >= 0 {
                    (child as usize) < ctx.nodes
                } else {
                    leaf_number(child) <= ctx.leaves
                };
                if !good {
                    return Err(refuse("node child"));
                }
            }
        }
        Table::Clip => {
            let plane = rd16(record, 0) as i16;
            if plane < 0 || plane as usize >= ctx.planes {
                return Err(refuse("clipnode plane"));
            }
            for side in 0..2 {
                let child = rd16(record, 2 + side * 2) as i16;
                if child >= 0 && child as usize >= ctx.clip_nodes {
                    return Err(refuse("clipnode child"));
                }
            }
        }
        Table::Planes | Table::Vertices | Table::Vis => {}
    }
    Ok(0)
}

/// Write record `index` of `table` into the slot, adding the slot bases to
/// every index it holds.
fn put_record(
    storage: &mut [u8],
    ranges: &[LumpRange; PXBSP_LUMP_COUNT],
    bases: &Bases,
    table: Table,
    index: usize,
    record: &[u8],
) {
    let at = |kind: PxbspLumpKind, base: usize, size: usize| {
        ranges[kind as usize].offset as usize + (base + index) * size
    };
    match table {
        Table::Faces => {
            let out =
                &mut storage[at(PxbspLumpKind::Faces, bases.faces, FACE_BYTES)..][..FACE_BYTES];
            out.copy_from_slice(record);
            wr16(out, 0, rd16(record, 0) + bases.planes as u16);
            wr16(out, 2, rd16(record, 2) + bases.vertices as u16);
        }
        Table::Marks => {
            let to = at(PxbspLumpKind::MarkSurfaces, bases.marks, MARK_BYTES);
            wr16(storage, to, rd16(record, 0) + bases.faces as u16);
        }
        Table::Leaves => {
            let out =
                &mut storage[at(PxbspLumpKind::Leaves, bases.leaf, LEAF_BYTES)..][..LEAF_BYTES];
            out.copy_from_slice(record);
            wr16(out, 8, rd16(record, 8) + bases.marks as u16);
            let vis = i32::from_le_bytes(record[4..8].try_into().unwrap());
            if vis >= 0 {
                out[4..8].copy_from_slice(&(vis + bases.vis as i32).to_le_bytes());
            }
        }
        Table::Nodes => {
            let out =
                &mut storage[at(PxbspLumpKind::Nodes, bases.nodes, NODE_BYTES)..][..NODE_BYTES];
            out.copy_from_slice(record);
            wr16(out, 0, rd16(record, 0) + bases.planes as u16);
            for side in 0..2 {
                let child = bases.render_child(rd16(record, 2 + side * 2) as i16);
                wr16(out, 2 + side * 2, child as u16);
            }
            wr16(out, 12, rd16(record, 12) + bases.faces as u16);
        }
        Table::Clip => {
            let out = &mut storage
                [at(PxbspLumpKind::ClipNodes, bases.clip_nodes, CLIPNODE_BYTES)..]
                [..CLIPNODE_BYTES];
            wr16(out, 0, rd16(record, 0) + bases.planes as u16);
            for side in 0..2 {
                let child = bases.clip_child(rd16(record, 2 + side * 2) as i16);
                wr16(out, 2 + side * 2, child as u16);
            }
        }
        Table::Planes | Table::Vertices | Table::Vis => {}
    }
}

/// Copy `bytes` of a bulk table (planes, vertices, visibility), starting at
/// byte `offset` of the table, into the slot unchanged.
fn put_bytes(
    storage: &mut [u8],
    ranges: &[LumpRange; PXBSP_LUMP_COUNT],
    bases: &Bases,
    table: Table,
    offset: usize,
    bytes: &[u8],
) {
    let base = match table {
        Table::Planes => {
            ranges[PxbspLumpKind::Planes as usize].offset as usize + bases.planes * PLANE_BYTES
        }
        Table::Vertices => {
            ranges[PxbspLumpKind::Vertices as usize].offset as usize + bases.vertices * VERTEX_BYTES
        }
        Table::Vis => ranges[PxbspLumpKind::Visibility as usize].offset as usize + bases.vis,
        _ => return,
    };
    storage[base + offset..base + offset + bytes.len()].copy_from_slice(bytes);
}

/// Where a streamed install stands after an [`PxbspResidentMap::install_feed`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallProgress {
    /// More of the payload is expected.
    Working,
    /// The whole payload arrived, checked out and sits relocated in the slot;
    /// only the link is left.
    Staged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallPhase {
    Receiving,
    Staged,
    Done,
}

/// A region install in flight, fed the payload in order and in any chunking.
///
/// The install keeps no copy of the payload: each record is checked against
/// the region's header and written into the slot, relocated, as soon as its
/// bytes arrive. A partial record waits in a 16 byte carry. The slot is
/// invisible to every traversal until [`PxbspResidentMap::link_region`].
#[derive(Clone, Debug)]
pub struct RegionInstall {
    region: u16,
    slot: u16,
    entry: RegionEntry,
    bases: Bases,
    caps: SlotCaps,
    materials_total: usize,
    header: [u8; REGION_HEADER_BYTES],
    header_len: usize,
    head: Option<RegionHead>,
    /// Payload bytes consumed so far.
    pos: usize,
    carry: [u8; NODE_BYTES],
    carry_len: usize,
    fnv: Fnv32,
    materials: u32,
    phase: InstallPhase,
}

impl RegionInstall {
    pub const fn region(&self) -> u16 {
        self.region
    }

    pub const fn slot(&self) -> u16 {
        self.slot
    }

    /// Payload bytes fed so far.
    pub const fn fed_bytes(&self) -> usize {
        self.pos
    }

    /// Whether the whole payload is in and checked, so only
    /// [`PxbspResidentMap::link_region`] is left.
    pub const fn is_staged(&self) -> bool {
        matches!(self.phase, InstallPhase::Staged)
    }

    /// Bit `m` is set when a face of the region uses material `m`, for
    /// materials below 32. Final once the install is staged.
    pub const fn materials(&self) -> u32 {
        self.materials
    }
}

/// Feed `bytes` of table `table` (starting `offset` bytes into it) to the
/// install: check and relocate every record they complete.
fn consume_table(
    job: &mut RegionInstall,
    storage: &mut [u8],
    ranges: &[LumpRange; PXBSP_LUMP_COUNT],
    ctx: &CheckCtx,
    table: Table,
    offset: usize,
    bytes: &[u8],
) -> Result<(), StreamError> {
    let size = table.record_bytes();
    if size == 1 {
        put_bytes(storage, ranges, &job.bases, table, offset, bytes);
        return Ok(());
    }
    let mut bytes = bytes;
    let mut offset = offset;
    if job.carry_len > 0 {
        let take = (size - job.carry_len).min(bytes.len());
        job.carry[job.carry_len..job.carry_len + take].copy_from_slice(&bytes[..take]);
        job.carry_len += take;
        bytes = &bytes[take..];
        offset += take;
        if job.carry_len == size {
            let record = job.carry;
            job.materials |= check_record(table, &record[..size], ctx)?;
            put_record(
                storage,
                ranges,
                &job.bases,
                table,
                (offset - size) / size,
                &record[..size],
            );
            job.carry_len = 0;
        }
    }
    let whole = bytes.len() / size * size;
    let first = offset / size;
    for (k, record) in bytes[..whole].chunks_exact(size).enumerate() {
        job.materials |= check_record(table, record, ctx)?;
        put_record(storage, ranges, &job.bases, table, first + k, record);
    }
    let rest = &bytes[whole..];
    if !rest.is_empty() {
        job.carry[..rest.len()].copy_from_slice(rest);
        job.carry_len = rest.len();
    }
    Ok(())
}

impl PxbspResidentMap {
    /// Whether this map was loaded by [`Self::load_streamed`].
    pub fn is_streamed(&self) -> bool {
        self.stream.is_some()
    }

    /// Slot bookkeeping of a streamed map.
    pub fn streaming(&self) -> Option<&StreamState> {
        self.stream.as_deref()
    }

    /// Load a streamed container and widen it to `slots` resident slots.
    ///
    /// The container is read through `reader` exactly like [`Self::load`];
    /// its geometry lumps hold only the top. The returned map has every
    /// region absent: all stub children point at stub leaves, which read as
    /// solid, until [`Self::install_region`] fills a slot.
    pub fn load_streamed<R: ReadAt>(
        &mut self,
        map_id: u32,
        reader: &mut R,
        slots: u16,
    ) -> Result<(), StreamLoadError<R::Error>> {
        self.load_streamed_inner(map_id, reader, slots, false)
    }

    /// [`Self::load_streamed`] for a map made with `with_capacity(0)` (no
    /// preallocated capacity): the image is allocated once, at exactly the
    /// size the container and `slots` call for. The guest uses this so the
    /// heap holds the pool and nothing more.
    pub fn load_streamed_exact<R: ReadAt>(
        &mut self,
        map_id: u32,
        reader: &mut R,
        slots: u16,
    ) -> Result<(), StreamLoadError<R::Error>> {
        self.load_streamed_inner(map_id, reader, slots, true)
    }

    fn load_streamed_inner<R: ReadAt>(
        &mut self,
        map_id: u32,
        reader: &mut R,
        slots: u16,
        exact: bool,
    ) -> Result<(), StreamLoadError<R::Error>> {
        self.prepare_owned_load();
        self.stream = None;
        let index = PxbspIndex::read(reader)
            .map_err(|e| StreamLoadError::Map(PxbspMapLoadError::Index(e)))?;
        if index.version() != PxbspVersion::V6 {
            return Err(StreamLoadError::Map(
                PxbspMapLoadError::StaticLegacyVersion {
                    found: index.version().wire(),
                },
            ));
        }
        let si = index.lump(PxbspLumpKind::StreamingIndex);
        let mut si_bytes = alloc::vec![0u8; si.len as usize];
        reader
            .read_exact_at(si.offset, &mut si_bytes)
            .map_err(|e| StreamLoadError::Map(PxbspMapLoadError::Read(e)))?;
        let sindex = StreamingIndex::parse(&si_bytes).map_err(StreamLoadError::Stream)?;
        let rpad = sindex.top_leaf_pad();
        if slots == 0 || rpad + slots as usize * sindex.caps.leaves as usize > MAX_VIRTUAL_LEAVES {
            return Err(StreamLoadError::Slots);
        }
        let slots_n = slots as usize;
        let caps = sindex.caps;
        let top = sindex.top;
        // Every global index must stay inside its wire field.
        let reach = |top_n: u16, cap: u16| top_n as usize + slots_n * cap as usize;
        if reach(top.planes, caps.planes) > i16::MAX as usize
            || reach(top.nodes, caps.nodes) > i16::MAX as usize
            || reach(top.clip_nodes, caps.clip_nodes) > i16::MAX as usize
            || reach(top.leaves, caps.leaves) > i16::MAX as usize
            || reach(top.faces, caps.faces) > u16::MAX as usize
            || reach(top.vertices, caps.vertices) > u16::MAX as usize
            || reach(top.marks, caps.marks) > u16::MAX as usize
            || slots_n * caps.vis_bytes as usize > i32::MAX as usize
        {
            return Err(StreamLoadError::Stream(StreamError::BadIndex(
                "slot geometry overflows a wire index",
            )));
        }
        let kind_geometry = |kind: PxbspLumpKind| -> Option<(usize, usize, usize, usize)> {
            // (record bytes, top records, slot records, container check)
            Some(match kind {
                PxbspLumpKind::Vertices => (
                    VERTEX_BYTES,
                    top.vertices as usize,
                    caps.vertices as usize,
                    0,
                ),
                PxbspLumpKind::Planes => {
                    (PLANE_BYTES, top.planes as usize, caps.planes as usize, 0)
                }
                PxbspLumpKind::Faces => (FACE_BYTES, top.faces as usize, caps.faces as usize, 0),
                PxbspLumpKind::MarkSurfaces => {
                    (MARK_BYTES, top.marks as usize, caps.marks as usize, 0)
                }
                PxbspLumpKind::Leaves => (LEAF_BYTES, top.leaves as usize, caps.leaves as usize, 0),
                PxbspLumpKind::Nodes => (NODE_BYTES, top.nodes as usize, caps.nodes as usize, 0),
                PxbspLumpKind::ClipNodes => (
                    CLIPNODE_BYTES,
                    top.clip_nodes as usize,
                    caps.clip_nodes as usize,
                    0,
                ),
                _ => return None,
            })
        };
        let mut top_len = [0usize; PXBSP_LUMP_COUNT];
        let mut full_len = [0usize; PXBSP_LUMP_COUNT];
        for kind in RESIDENT_LUMPS {
            let source = index.lump(kind).len as usize;
            let (top_bytes, full_bytes) = if let Some((rec, top_n, slot_n, _)) = kind_geometry(kind)
            {
                if source != top_n * rec {
                    return Err(StreamLoadError::Stream(StreamError::BadIndex(
                        "container lump does not match the top counts",
                    )));
                }
                (source, (top_n + slots_n * slot_n) * rec)
            } else if kind == PxbspLumpKind::Visibility {
                (source, source + slots_n * caps.vis_bytes as usize)
            } else {
                (source, source)
            };
            top_len[kind as usize] = top_bytes;
            full_len[kind as usize] = full_bytes;
        }
        let total = RESIDENT_LUMPS.iter().try_fold(0usize, |total, &kind| {
            total
                .checked_add(3)
                .map(|v| v & !3)
                .and_then(|a| a.checked_add(full_len[kind as usize]))
        });
        let Some(total) = total else {
            return Err(StreamLoadError::Map(PxbspMapLoadError::TooLarge {
                required: usize::MAX,
                capacity: self.storage_capacity(),
            }));
        };
        if exact {
            // Word storage: the vertex lump is read in place, so the image
            // must start four-byte aligned, which a byte vector on the bump
            // heap does not promise.
            self.storage = PxbspResidentStorage::Words {
                words: alloc::vec![0u32; total.div_ceil(4)],
                len: total,
            };
        } else {
            if total > self.storage_capacity() {
                return Err(StreamLoadError::Map(PxbspMapLoadError::TooLarge {
                    required: total,
                    capacity: self.storage_capacity(),
                }));
            }
            self.owned_bytes_mut().resize(total, 0);
        }
        let mut destination = 0usize;
        for kind in RESIDENT_LUMPS {
            destination = align_up_4(destination);
            let source = index.lump(kind);
            let end = destination + source.len as usize;
            if let Err(error) =
                reader.read_exact_at(source.offset, &mut self.owned_slice_mut()[destination..end])
            {
                self.clear_loaded_state();
                return Err(StreamLoadError::Map(PxbspMapLoadError::Read(error)));
            }
            self.ranges[kind as usize] = LumpRange {
                offset: destination as u32,
                len: top_len[kind as usize] as u32,
            };
            destination += full_len[kind as usize];
        }
        for kind in PxbspLumpKind::ALL {
            self.source_ranges[kind as usize] = index.lump(kind);
        }
        self.source_file_len = index.file_len();

        // Stub leaves: wire -7 becomes solid in the resident image.
        let leaf_range = self.ranges[PxbspLumpKind::Leaves as usize];
        let regions = sindex.regions.len();
        {
            let leaves =
                &mut self.owned_slice_mut()[leaf_range.offset as usize..leaf_range.end() as usize];
            let mut bad = None;
            for leaf in 0..leaf_range.len as usize / LEAF_BYTES {
                let record = &mut leaves[leaf * LEAF_BYTES..][..LEAF_BYTES];
                let wanted_stub = leaf >= 1 && leaf <= regions;
                if wanted_stub {
                    if record[0] as i8 != CONTENTS_UNRESIDENT as i8
                        || rd16(record, 10) as usize != leaf - 1
                    {
                        bad = Some("stub leaf is not contents -7 carrying its region");
                    }
                    record[0] = crate::collision::CONTENTS_SOLID as i8 as u8;
                } else if (leaf == 0 || leaf <= rpad)
                    && record[0] as i8 != crate::collision::CONTENTS_SOLID as i8
                {
                    bad = Some("sentinel or padding leaf is not solid");
                }
            }
            if let Some(what) = bad {
                self.clear_loaded_state();
                return Err(StreamLoadError::Stream(StreamError::BadIndex(what)));
            }
        }
        // The world model's PVS width is the whole virtual leaf space.
        let models = self.ranges[PxbspLumpKind::Models as usize];
        if models.len as usize >= 32 {
            let bits = (rpad + slots_n * caps.leaves as usize) as i16;
            let at = models.offset as usize + 26;
            wr16(self.owned_slice_mut(), at, bits as u16);
        }
        if let Err(error) = self.validate_references() {
            self.clear_loaded_state();
            return Err(StreamLoadError::Map(error));
        }
        for kind in RESIDENT_LUMPS {
            self.ranges[kind as usize].len = full_len[kind as usize] as u32;
        }
        self.map_id = Some(map_id);
        self.generation = self.generation.wrapping_add(1);
        self.stream = Some(Box::new(StreamState {
            index: sindex,
            slots,
            rpad,
            slot_of_region: alloc::vec![NO_SLOT; regions],
            region_of_slot: alloc::vec![NO_SLOT; slots_n],
            slot_counts: alloc::vec![[0; 7]; slots_n],
            pvs_missing: Cell::new(0),
        }));
        Ok(())
    }

    /// Install `payload` (a region's whole blob) into `slot`: verify its
    /// checksum, validate every reference, copy the tables with the slot
    /// bases added, then link the subtree into the three top trees. Nothing
    /// is allocated, and the map is unchanged when an error is returned.
    ///
    /// A caller that cannot hold a whole payload, or must spread the work
    /// over frames, uses [`Self::begin_install`] and [`Self::install_feed`].
    pub fn install_region(
        &mut self,
        region: u16,
        slot: u16,
        payload: &[u8],
    ) -> Result<(), StreamError> {
        // Check everything first so a refusal writes nothing.
        let view = RegionView::parse(payload)?;
        let (caps, materials) = {
            let state = self.stream.as_deref().ok_or(StreamError::NotStreamed)?;
            (state.index.caps, self.materials().len())
        };
        check_head(&view.head, &caps)?;
        let ctx = CheckCtx::new(&view.head, materials);
        for table in Table::ALL {
            let (bytes, size) = match table {
                Table::Faces => (view.faces, FACE_BYTES),
                Table::Marks => (view.marks, MARK_BYTES),
                Table::Leaves => (view.leaves, LEAF_BYTES),
                Table::Nodes => (view.nodes, NODE_BYTES),
                Table::Clip => (view.clip_nodes, CLIPNODE_BYTES),
                _ => continue,
            };
            for record in bytes.chunks_exact(size) {
                check_record(table, record, &ctx)?;
            }
        }
        let mut job = self.begin_install(region, slot)?;
        if self.install_feed(&mut job, payload)? != InstallProgress::Staged {
            return Err(StreamError::ShortPayload);
        }
        self.link_region(&mut job)
    }

    /// Start installing `region` into `slot` from a stream of payload bytes.
    ///
    /// Feed the payload, in order and in chunks of any size, to
    /// [`Self::install_feed`]; each record is checked against the header and
    /// written, relocated, into the slot as its bytes arrive, so no landing
    /// buffer is needed. Only one install may be in flight per slot, and the
    /// slot must stay unused by anything else until [`Self::link_region`]
    /// returns or the job is dropped.
    pub fn begin_install(&self, region: u16, slot: u16) -> Result<RegionInstall, StreamError> {
        let state = self.stream.as_deref().ok_or(StreamError::NotStreamed)?;
        let entry = *state
            .index
            .regions
            .get(region as usize)
            .ok_or(StreamError::RegionOutOfRange)?;
        if slot >= state.slots {
            return Err(StreamError::SlotOutOfRange);
        }
        if state.slot_of_region[region as usize] != NO_SLOT {
            return Err(StreamError::AlreadyInstalled);
        }
        if state.region_of_slot[slot as usize] != NO_SLOT {
            return Err(StreamError::SlotBusy);
        }
        Ok(RegionInstall {
            region,
            slot,
            entry,
            bases: state.bases(slot as usize),
            caps: state.index.caps,
            materials_total: self.materials().len(),
            header: [0; REGION_HEADER_BYTES],
            header_len: 0,
            head: None,
            pos: 0,
            carry: [0; NODE_BYTES],
            carry_len: 0,
            fnv: Fnv32::new(),
            materials: 0,
            phase: InstallPhase::Receiving,
        })
    }

    /// Feed the next payload bytes to an install in flight.
    ///
    /// Bytes past the payload's end (the sector padding of the last read) are
    /// ignored, so a caller can hand over whole sectors. Returns
    /// [`InstallProgress::Staged`] once the payload is complete, its checksum
    /// matches the directory ([`RegionEntry::fnv`]) and every table sits
    /// relocated in the slot; [`Self::link_region`] then makes it visible.
    ///
    /// A refused payload leaves the slot with partial, unreachable records
    /// and the map consistent; drop the job and start again.
    pub fn install_feed(
        &mut self,
        job: &mut RegionInstall,
        bytes: &[u8],
    ) -> Result<InstallProgress, StreamError> {
        if self.stream.is_none() {
            return Err(StreamError::NotStreamed);
        }
        if job.phase != InstallPhase::Receiving {
            return Ok(if job.phase == InstallPhase::Staged {
                InstallProgress::Staged
            } else {
                InstallProgress::Working
            });
        }
        let total = job.entry.payload_bytes as usize;
        let mut bytes = &bytes[..bytes.len().min(total - job.pos)];
        job.fnv.update(bytes);
        let ranges = self.ranges;
        let storage = self.owned_slice_mut();
        while !bytes.is_empty() {
            let Some(head) = job.head else {
                let take = (REGION_HEADER_BYTES - job.header_len).min(bytes.len());
                job.header[job.header_len..job.header_len + take].copy_from_slice(&bytes[..take]);
                job.header_len += take;
                job.pos += take;
                bytes = &bytes[take..];
                if job.header_len == REGION_HEADER_BYTES {
                    let head = parse_head(&job.header)?;
                    if head.id != job.region || head.vis_count != job.entry.vis_count {
                        return Err(StreamError::BadRegion(
                            "payload does not belong to this region",
                        ));
                    }
                    if head.layout.end != total {
                        return Err(StreamError::BadRegion(
                            "payload length does not match the directory",
                        ));
                    }
                    check_head(&head, &job.caps)?;
                    job.head = Some(head);
                }
                continue;
            };
            let ctx = CheckCtx::new(&head, job.materials_total);
            // The section holding `pos`, or the gap before the next one.
            let mut step = bytes.len();
            for (table, &(start, len)) in Table::ALL.iter().zip(&head.layout.sections) {
                if job.pos < start {
                    step = (start - job.pos).min(bytes.len());
                    break;
                }
                if job.pos < start + len {
                    step = (start + len - job.pos).min(bytes.len());
                    let offset = job.pos - start;
                    consume_table(job, storage, &ranges, &ctx, *table, offset, &bytes[..step])?;
                    break;
                }
            }
            job.pos += step;
            bytes = &bytes[step..];
        }
        if job.pos == total {
            if job.fnv.finish() != job.entry.fnv {
                return Err(StreamError::BadChecksum);
            }
            job.phase = InstallPhase::Staged;
            return Ok(InstallProgress::Staged);
        }
        Ok(InstallProgress::Working)
    }

    /// Link a staged region into the three top trees (one halfword each) and
    /// record its slot. The only step that makes the region visible.
    pub fn link_region(&mut self, job: &mut RegionInstall) -> Result<(), StreamError> {
        if job.phase != InstallPhase::Staged {
            return Err(StreamError::BadRegion("install is not staged"));
        }
        let head = job.head.expect("a staged install has its header");
        {
            let state = self.stream.as_deref().ok_or(StreamError::NotStreamed)?;
            if state.slot_of_region[job.region as usize] != NO_SLOT {
                return Err(StreamError::AlreadyInstalled);
            }
            if state.region_of_slot[job.slot as usize] != NO_SLOT {
                return Err(StreamError::SlotBusy);
            }
        }
        let ranges = self.ranges;
        let storage = self.owned_slice_mut();
        let node_at = ranges[PxbspLumpKind::Nodes as usize].offset as usize;
        let clip_at = ranges[PxbspLumpKind::ClipNodes as usize].offset as usize;
        let entry = job.entry;
        let render = job.bases.render_child(head.render_root);
        wr16(
            storage,
            node_at + entry.parents[0] as usize * NODE_BYTES + 2 + entry.side(0) * 2,
            render as u16,
        );
        for hull in 0..2 {
            let child = job.bases.clip_child(head.clip_roots[hull]);
            wr16(
                storage,
                clip_at
                    + entry.parents[1 + hull] as usize * CLIPNODE_BYTES
                    + 2
                    + entry.side(1 + hull) * 2,
                child as u16,
            );
        }
        let state = self.stream.as_deref_mut().expect("checked above");
        state.slot_of_region[job.region as usize] = job.slot;
        state.region_of_slot[job.slot as usize] = job.region;
        state.slot_counts[job.slot as usize] = head.counts;
        self.generation = self.generation.wrapping_add(1);
        job.phase = InstallPhase::Done;
        Ok(())
    }

    /// Unlink `region` (children back to the stub / solid) and free its slot.
    /// Returns the slot. The slot's stale records are unreachable from then on.
    pub fn uninstall_region(&mut self, region: u16) -> Result<u16, StreamError> {
        let (entry, slot) = {
            let state = self.stream.as_deref().ok_or(StreamError::NotStreamed)?;
            let entry = *state
                .index
                .regions
                .get(region as usize)
                .ok_or(StreamError::RegionOutOfRange)?;
            (
                entry,
                state.slot_of(region).ok_or(StreamError::NotInstalled)?,
            )
        };
        let ranges = self.ranges;
        let storage = self.owned_slice_mut();
        let node_at = ranges[PxbspLumpKind::Nodes as usize].offset as usize;
        let clip_at = ranges[PxbspLumpKind::ClipNodes as usize].offset as usize;
        let stub = (-1i32 - (region as i32 + 1)) as i16;
        wr16(
            storage,
            node_at + entry.parents[0] as usize * NODE_BYTES + 2 + entry.side(0) * 2,
            stub as u16,
        );
        for hull in 0..2 {
            wr16(
                storage,
                clip_at
                    + entry.parents[1 + hull] as usize * CLIPNODE_BYTES
                    + 2
                    + entry.side(1 + hull) * 2,
                crate::collision::CONTENTS_SOLID as u16,
            );
        }
        let state = self.stream.as_deref_mut().expect("checked above");
        state.slot_of_region[region as usize] = NO_SLOT;
        state.region_of_slot[slot as usize] = NO_SLOT;
        state.slot_counts[slot as usize] = [0; 7];
        self.generation = self.generation.wrapping_add(1);
        Ok(slot)
    }

    /// Region whose cell holds `point`, by walking the resident tree. `None`
    /// on a malformed tree or a point that falls into the solid sentinel.
    pub fn region_at_point(&self, point: Vec3I32) -> Option<u16> {
        let state = self.stream.as_deref()?;
        let top_nodes = state.index.top.nodes as usize;
        let mut node_index = self.world_head_node()?;
        loop {
            if node_index < 0 {
                return state.leaf_region(leaf_number(node_index));
            }
            if node_index as usize >= top_nodes {
                // First node of an installed subtree: its slot names the region.
                let slot = (node_index as usize - top_nodes) / state.index.caps.nodes as usize;
                return state.region_in_slot(slot as u16);
            }
            let node = self.compact_nodes().get(node_index as usize)?;
            let plane = self.planes().get(node.plane as usize)?;
            let dot = match plane.kind {
                0 => point.x,
                1 => point.y,
                2 => point.z,
                _ => mul_q12_i32(point.x, plane.normal.x as i32)
                    .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
                    .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
            };
            node_index = node.children[(dot.saturating_sub(plane.distance) < 0) as usize];
        }
    }

    /// The region at `point` when it is *not* resident: what the streamer
    /// raises to a demand request when a trace ends against a stub wall.
    pub fn unresident_region_at(&self, point: Vec3I32) -> Option<u16> {
        let state = self.stream.as_deref()?;
        let top_nodes = state.index.top.nodes as usize;
        let mut node_index = self.world_head_node()?;
        loop {
            if node_index < 0 {
                let leaf = leaf_number(node_index);
                let regions = state.index.regions.len();
                return (leaf >= 1 && leaf <= regions).then(|| (leaf - 1) as u16);
            }
            if node_index as usize >= top_nodes {
                return None;
            }
            let node = self.compact_nodes().get(node_index as usize)?;
            let plane = self.planes().get(node.plane as usize)?;
            let dot = match plane.kind {
                0 => point.x,
                1 => point.y,
                2 => point.z,
                _ => mul_q12_i32(point.x, plane.normal.x as i32)
                    .saturating_add(mul_q12_i32(point.y, plane.normal.y as i32))
                    .saturating_add(mul_q12_i32(point.z, plane.normal.z as i32)),
            };
            node_index = node.children[(dot.saturating_sub(plane.distance) < 0) as usize];
        }
    }

    /// Expand the PVS row of `leaf_index` into the dense virtual-leaf bitmap
    /// (bit `leaf - 1`) in `output`, using `scratch` (at least 1024 bytes) for
    /// the compressed-row decode. Returns the addressable bit count, or
    /// `None` for a solid, stub or row-less leaf and malformed data.
    ///
    /// Ranks whose region is absent are dropped and counted in
    /// [`StreamState::pvs_missing`].
    #[inline(never)]
    pub fn streamed_leaf_visibility_into(
        &self,
        leaf_index: usize,
        scratch: &mut [u8],
        output: &mut [u8],
    ) -> Option<usize> {
        let state = self.stream.as_deref()?;
        let lcap = state.lcap();
        let bytes_per_rank = lcap / 8;
        let region = state.leaf_region(leaf_index)?;
        if leaf_index <= state.rpad {
            return None;
        }
        let leaf = self.leaves().get(leaf_index)?;
        let offset = usize::try_from(leaf.visibility_offset).ok()?;
        let list = state.index.vis_list(region as usize);
        let row_bytes = list.len() * bytes_per_rank;
        let bits = state.virtual_leaf_bits();
        let out_bytes = bits / 8;
        if row_bytes > scratch.len() || out_bytes > output.len() {
            return None;
        }
        scratch[..row_bytes].fill(0);
        if !crate::pxbsp::decompress_visibility(
            self.visibility(),
            offset,
            &mut scratch[..row_bytes],
        ) {
            return None;
        }
        output[..out_bytes].fill(0);
        for (rank, &q) in list.iter().enumerate() {
            let Some(slot) = state.slot_of(q) else {
                state
                    .pvs_missing
                    .set(state.pvs_missing.get().wrapping_add(1));
                continue;
            };
            let from = rank * bytes_per_rank;
            let to = (state.rpad + slot as usize * lcap) / 8;
            output[to..to + bytes_per_rank].copy_from_slice(&scratch[from..from + bytes_per_rank]);
        }
        Some(bits)
    }

    /// `leaf_visibility_into` for a streamed map: the dense virtual-leaf row.
    #[inline(never)]
    pub(crate) fn streamed_leaf_visibility_dense(
        &self,
        leaf_index: usize,
        output: &mut [u8],
    ) -> Option<usize> {
        let mut scratch = [0u8; PXBSP_MAX_VISIBILITY_BYTES];
        self.streamed_leaf_visibility_into(leaf_index, &mut scratch, output)
    }

    /// Walk every reference of the resident image and check that the map has
    /// no dangling pointer: reachable nodes, leaves, clip nodes, marks and
    /// faces all sit in the top or in an *installed* slot, the stub of an
    /// absent region is reachable only through its own link, and the three
    /// link halfwords agree with the slot table. Host and test use.
    pub fn check_integrity(&self) -> Result<(), IntegrityError> {
        let state = self.stream.as_deref().ok_or(err("not streamed", 0))?;
        let top = state.index.top;
        let caps = state.index.caps;
        let regions = state.index.regions.len();
        let nodes = self.nodes();
        let clips = self.clip_nodes();
        let leaves = self.leaves();
        let marks = self.mark_surfaces();
        let faces = self.faces();
        let planes = self.planes();
        // Which slot owns a global index, if it is in a slot at all.
        let slot_of = |index: usize, top_n: usize, cap: usize| -> Option<usize> {
            if index < top_n || cap == 0 {
                None
            } else {
                Some((index - top_n) / cap)
            }
        };
        let live = |slot: usize| {
            state
                .region_of_slot
                .get(slot)
                .is_some_and(|&r| r != NO_SLOT)
        };

        // Link halfwords against the slot table.
        for (r, entry) in state.index.regions.iter().enumerate() {
            let installed = state.slot_of_region[r] != NO_SLOT;
            let render = nodes
                .get(entry.parents[0] as usize)
                .ok_or(err("render parent", r))?;
            let child = render.children[entry.side(0)];
            let stub = (-1i32 - (r as i32 + 1)) as i16;
            if installed == (child == stub) {
                return Err(err("render link disagrees with the slot table", r));
            }
            if installed {
                let slot = state.slot_of_region[r] as usize;
                let bases = state.bases(slot);
                let inside = if child >= 0 {
                    child as usize >= bases.nodes
                        && (child as usize) < bases.nodes + caps.nodes as usize
                } else {
                    let leaf = leaf_number(child);
                    leaf >= bases.leaf && leaf < bases.leaf + caps.leaves as usize || leaf == 0
                };
                if !inside {
                    return Err(err("render link leaves its slot", r));
                }
            }
            for hull in 0..2 {
                let clip = clips
                    .get(entry.parents[1 + hull] as usize)
                    .ok_or(err("clip parent", r))?;
                let child = clip.children[entry.side(1 + hull)];
                if !installed && child != crate::collision::CONTENTS_SOLID {
                    return Err(err("absent region is not solid in a clip tree", r));
                }
                if installed && child >= 0 {
                    let bases = state.bases(state.slot_of_region[r] as usize);
                    if (child as usize) < bases.clip_nodes
                        || child as usize >= bases.clip_nodes + caps.clip_nodes as usize
                    {
                        return Err(err("clip link leaves its slot", r));
                    }
                }
            }
        }

        // Reachability from the heads.
        let world = self.brush_models().get(0).ok_or(err("no world model", 0))?;
        let mut stack: Vec<i16> = alloc::vec![world.head_nodes[0]];
        let mut seen = alloc::vec![false; nodes.len()];
        let mut stub_seen = alloc::vec![false; regions];
        while let Some(child) = stack.pop() {
            if child < 0 {
                let leaf = leaf_number(child);
                if leaf == 0 {
                    continue;
                }
                if leaf <= regions {
                    if state.slot_of_region[leaf - 1] != NO_SLOT {
                        return Err(err("stub reachable while its region is resident", leaf - 1));
                    }
                    stub_seen[leaf - 1] = true;
                    continue;
                }
                if leaf <= state.rpad {
                    return Err(err("padding leaf reachable", leaf));
                }
                let slot = (leaf - 1 - state.rpad) / caps.leaves as usize;
                if !live(slot) {
                    return Err(err("leaf in a free slot is reachable", leaf));
                }
                let local = (leaf - 1 - state.rpad) % caps.leaves as usize;
                if local >= state.slot_counts[slot][4] {
                    return Err(err("leaf beyond the installed count", leaf));
                }
                let record = leaves.get(leaf).ok_or(err("leaf out of range", leaf))?;
                let bases = state.bases(slot);
                let first = record.first_mark_surface as usize;
                for mark_index in first..first + record.mark_surface_count as usize {
                    if mark_index < bases.marks
                        || mark_index >= bases.marks + state.slot_counts[slot][3]
                    {
                        return Err(err("mark outside its slot", leaf));
                    }
                    let face = marks
                        .get(mark_index)
                        .ok_or(err("mark out of range", mark_index))?
                        as usize;
                    if face < bases.faces || face >= bases.faces + state.slot_counts[slot][2] {
                        return Err(err("face outside its slot", face));
                    }
                    let record = faces.get(face).ok_or(err("face out of range", face))?;
                    if (record.plane as usize) < bases.planes
                        || record.plane as usize >= bases.planes + state.slot_counts[slot][0]
                        || (record.first_vertex as usize) < bases.vertices
                        || record.first_vertex as usize + record.vertex_count as usize
                            > bases.vertices + state.slot_counts[slot][1]
                    {
                        return Err(err("face references outside its slot", face));
                    }
                }
                if record.visibility_offset >= 0 {
                    let off = record.visibility_offset as usize;
                    if off < bases.vis || off >= bases.vis + caps.vis_bytes as usize {
                        return Err(err("visibility offset outside its slot", leaf));
                    }
                }
                continue;
            }
            let index = child as usize;
            if index >= nodes.len() {
                return Err(err("node out of range", index));
            }
            if seen[index] {
                continue;
            }
            seen[index] = true;
            if let Some(slot) = slot_of(index, top.nodes as usize, caps.nodes as usize) {
                if !live(slot) {
                    return Err(err("node in a free slot is reachable", index));
                }
                if index - (top.nodes as usize + slot * caps.nodes as usize)
                    >= state.slot_counts[slot][5]
                {
                    return Err(err("node beyond the installed count", index));
                }
            }
            let node = nodes.get(index).ok_or(err("node out of range", index))?;
            if node.plane as usize >= planes.len() {
                return Err(err("node plane out of range", index));
            }
            stack.push(node.children[0]);
            stack.push(node.children[1]);
        }
        for (r, seen) in stub_seen.iter().enumerate().take(regions) {
            if state.slot_of_region[r] == NO_SLOT && !seen {
                return Err(err("absent region has no reachable stub", r));
            }
        }

        // Clip trees: contents may be anything negative, nodes must be live.
        for hull in 0..2 {
            let head = world.head_nodes[2 + hull];
            let mut stack = alloc::vec![head];
            let mut seen = alloc::vec![false; clips.len()];
            while let Some(child) = stack.pop() {
                if child < 0 {
                    continue;
                }
                let index = child as usize;
                if index >= clips.len() {
                    return Err(err("clipnode out of range", index));
                }
                if seen[index] {
                    continue;
                }
                seen[index] = true;
                if let Some(slot) =
                    slot_of(index, top.clip_nodes as usize, caps.clip_nodes as usize)
                {
                    if !live(slot) {
                        return Err(err("clipnode in a free slot is reachable", index));
                    }
                    let bases = state.bases(slot);
                    let node = clips.get(index).unwrap();
                    if (node.plane as usize) < bases.planes
                        || node.plane as usize >= bases.planes + state.slot_counts[slot][0]
                    {
                        return Err(err("clipnode plane outside its slot", index));
                    }
                } else if clips.get(index).unwrap().plane as usize >= planes.len() {
                    return Err(err("clipnode plane out of range", index));
                }
                let node = clips.get(index).unwrap();
                stack.push(node.children[0]);
                stack.push(node.children[1]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::collision::{CONTENTS_EMPTY, CONTENTS_SOLID};
    use crate::pxbsp::PxbspEntity;
    use crate::pxbsp_resident::tests::{valid_lumps, write_file};
    use crate::SliceReader;

    const LCAP: u16 = 8;
    /// Cell `r` spans x in `[100 r, 100 (r + 1))` world units.
    const CELL: i32 = 100;

    fn push16(out: &mut Vec<u8>, value: i16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn plane_x(units: i32) -> Vec<u8> {
        let mut out = Vec::new();
        push16(&mut out, 4096);
        push16(&mut out, 0);
        push16(&mut out, 0);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(units << 12).to_le_bytes());
        out
    }

    fn leaf(contents: i8, vis: i32, first_mark: u16, marks: u16, tag: u16) -> Vec<u8> {
        let mut out = vec![contents as u8, 0];
        out.extend_from_slice(&marks.to_le_bytes());
        out.extend_from_slice(&vis.to_le_bytes());
        out.extend_from_slice(&first_mark.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn node(plane: u16, front: i16, back: i16, first_face: u16, faces: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&plane.to_le_bytes());
        push16(&mut out, front);
        push16(&mut out, back);
        // Generous bounds: -128 .. 127 grid cells.
        out.extend_from_slice(&[0x80, 0x80, 0x80, 0x7f, 0x7f, 0x7f]);
        out.extend_from_slice(&first_face.to_le_bytes());
        out.extend_from_slice(&faces.to_le_bytes());
        out
    }

    fn clipnode(plane: i16, front: i16, back: i16) -> Vec<u8> {
        let mut out = Vec::new();
        push16(&mut out, plane);
        push16(&mut out, front);
        push16(&mut out, back);
        out
    }

    fn compress(row: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < row.len() {
            if row[i] != 0 {
                out.push(row[i]);
                i += 1;
            } else {
                let start = i;
                while i < row.len() && row[i] == 0 && i - start < 255 {
                    i += 1;
                }
                out.extend_from_slice(&[0, (i - start) as u8]);
            }
        }
        out
    }

    /// One region: a render node splitting its cell, two leaves, two faces,
    /// two clip nodes (one per hull), and PVS rows seeing all of `list`.
    fn region(r: u16, regions: u16) -> (RegionBuild, Vec<u16>) {
        let mut list = vec![r];
        if r > 0 {
            list.push(r - 1);
        }
        if r + 1 < regions {
            list.push(r + 1);
        }
        let x0 = CELL * i32::from(r);
        let mut build = RegionBuild {
            id: r,
            vis_count: list.len() as u16,
            ..RegionBuild::default()
        };
        build.planes.extend(plane_x(x0 + CELL / 2));
        // Vertices: two triangles; the position encodes the region.
        for tri in 0..2i16 {
            for corner in 0..3i16 {
                push16(&mut build.vertices, (x0 as i16) + tri * 40 + corner);
                push16(&mut build.vertices, corner);
                push16(&mut build.vertices, 1);
                build.vertices.extend_from_slice(&[0, 0, 128, 128, 128, 0]);
            }
        }
        for face in 0..2u16 {
            let mut record = Vec::new();
            record.extend_from_slice(&0u16.to_le_bytes());
            record.extend_from_slice(&(face * 3).to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes());
            record.extend_from_slice(&[0, 3, 0, 0]);
            build.faces.extend(record);
        }
        for face in 0..2u16 {
            build.marks.extend_from_slice(&face.to_le_bytes());
        }
        // Row: every leaf of every listed region.
        let row_bytes = list.len();
        let row = vec![0b11u8; row_bytes];
        let compressed = compress(&row);
        build.vis = compressed;
        build.leaves.extend(leaf(CONTENTS_EMPTY as i8, 0, 0, 1, 0));
        build.leaves.extend(leaf(CONTENTS_EMPTY as i8, 0, 1, 1, 0));
        // Render node: front (x >= mid) -> local leaf 2, back -> local leaf 1.
        build.nodes.extend(node(0, -3, -2, 0, 0));
        // Hull 1 node 0 and hull 2 node 1: front empty, back solid.
        build
            .clip_nodes
            .extend(clipnode(0, CONTENTS_EMPTY, CONTENTS_SOLID));
        build
            .clip_nodes
            .extend(clipnode(0, CONTENTS_EMPTY, CONTENTS_SOLID));
        build.render_root = 0;
        build.clip_roots = [0, 1];
        (build, list)
    }

    struct World {
        container: Vec<u8>,
        index: StreamingIndex,
        payloads: Vec<Vec<u8>>,
        pack: Vec<u8>,
    }

    fn world(regions: u16) -> World {
        let mut lumps = valid_lumps();
        let r = regions as usize;
        let rpad = r.div_ceil(8) * 8;
        // Top planes: cut x = CELL * (i + 1).
        let mut planes = Vec::new();
        for i in 0..r - 1 {
            planes.extend(plane_x(CELL * (i as i32 + 1)));
        }
        lumps[PxbspLumpKind::Planes as usize] = planes;
        lumps[PxbspLumpKind::Vertices as usize].clear();
        lumps[PxbspLumpKind::Faces as usize].clear();
        lumps[PxbspLumpKind::MarkSurfaces as usize].clear();
        lumps[PxbspLumpKind::Visibility as usize].clear();
        // Leaves: sentinel, one stub per region, solid padding.
        let mut leaves = leaf(CONTENTS_SOLID as i8, -1, 0, 0, 0);
        for i in 0..r {
            leaves.extend(leaf(CONTENTS_UNRESIDENT as i8, -1, 0, 0, i as u16));
        }
        for _ in r..rpad {
            leaves.extend(leaf(CONTENTS_SOLID as i8, -1, 0, 0, 0));
        }
        lumps[PxbspLumpKind::Leaves as usize] = leaves;
        // Chain: node i splits at x = CELL (i + 1); back child is region i.
        let stub = |i: usize| -(i as i16 + 2);
        let mut nodes = Vec::new();
        let mut parents = vec![[0u16; 3]; r];
        let mut sides = vec![0u8; r];
        for i in 0..r - 1 {
            let front = if i + 2 < r {
                (i + 1) as i16
            } else {
                stub(r - 1)
            };
            nodes.extend(node(i as u16, front, stub(i), 0, 0));
            parents[i][0] = i as u16;
            sides[i] |= 1;
        }
        parents[r - 1][0] = (r - 2) as u16;
        lumps[PxbspLumpKind::Nodes as usize] = nodes;
        // Clip: sentinel, then two chains.
        let mut clip = clipnode(0, CONTENTS_EMPTY, CONTENTS_EMPTY);
        for hull in 0..2usize {
            let base = 1 + hull * (r - 1);
            for i in 0..r - 1 {
                let front = if i + 2 < r {
                    (base + i + 1) as i16
                } else {
                    CONTENTS_SOLID
                };
                clip.extend(clipnode(i as i16, front, CONTENTS_SOLID));
                parents[i][1 + hull] = (base + i) as u16;
                sides[i] |= 2 << hull;
            }
            parents[r - 1][1 + hull] = (base + r - 2) as u16;
        }
        lumps[PxbspLumpKind::ClipNodes as usize] = clip;
        // Model heads: render 0, sentinel clip 0, hull 1 and 2 roots.
        let mut model = Vec::new();
        for _ in 0..9 {
            push16(&mut model, 0);
        }
        for head in [0, 0, 1, 1 + (r - 1) as i16] {
            push16(&mut model, head);
        }
        push16(&mut model, 0);
        model.extend_from_slice(&[0, 0, 0, 0]);
        lumps[PxbspLumpKind::Models as usize] = model;
        // Entities: none (leaf 0 is fine for the table checks).
        let mut entities = vec![0u8; 8];
        entities[2..4].copy_from_slice(&(PxbspEntity::SIZE as u16).to_le_bytes());
        entities[4..8].copy_from_slice(&8u32.to_le_bytes());
        lumps[PxbspLumpKind::Entities as usize] = entities;

        let mut payloads = Vec::new();
        let mut index = StreamingIndex {
            caps: SlotCaps {
                faces: 4,
                vertices: 8,
                planes: 2,
                marks: 4,
                nodes: 2,
                clip_nodes: 4,
                leaves: LCAP,
                vis_bytes: 16,
            },
            top: TopCounts {
                planes: (r - 1) as u16,
                vertices: 0,
                faces: 0,
                marks: 0,
                leaves: (1 + rpad) as u16,
                nodes: (r - 1) as u16,
                clip_nodes: (1 + 2 * (r - 1)) as u16,
            },
            regions: Vec::new(),
            vis_lists: Vec::new(),
        };
        let mut pack = Vec::new();
        for id in 0..regions {
            let (build, list) = region(id, regions);
            let blob = build.encode();
            let sector = (pack.len() / SECTOR_BYTES as usize) as u32;
            pack.extend_from_slice(&blob);
            pack.resize(
                pack.len().div_ceil(SECTOR_BYTES as usize) * SECTOR_BYTES as usize,
                0,
            );
            let x0 = (CELL * i32::from(id)) as i16;
            index.regions.push(RegionEntry {
                sector_start: sector,
                payload_bytes: blob.len() as u32,
                fnv: fnv1a32(&blob),
                mins: [x0, 0, 0],
                maxs: [x0 + CELL as i16, 8, 8],
                parents: parents[id as usize],
                sides: sides[id as usize],
                vis_offset: index.vis_lists.len() as u32,
                vis_count: list.len() as u16,
            });
            index.vis_lists.extend(list);
            payloads.push(blob);
        }
        lumps[PxbspLumpKind::StreamingIndex as usize] = index.encode();
        World {
            container: write_file(&lumps),
            index,
            payloads,
            pack,
        }
    }

    fn load(world: &World, slots: u16) -> PxbspResidentMap {
        let mut map = PxbspResidentMap::new();
        map.load_streamed(1, &mut SliceReader::new(&world.container), slots)
            .expect("streamed load");
        map
    }

    fn at(x: i32) -> Vec3I32 {
        Vec3I32 {
            x: x << 12,
            y: 1 << 12,
            z: 1 << 12,
        }
    }

    fn hull_contents(map: &PxbspResidentMap, hull: usize, x: i32) -> i16 {
        map.model_collision_hull(0, hull)
            .unwrap()
            .point_contents(at(x))
            .unwrap()
    }

    #[test]
    fn index_round_trips_and_rejects_corruption() {
        let w = world(5);
        let bytes = w.index.encode();
        assert_eq!(StreamingIndex::parse(&bytes), Ok(w.index.clone()));
        let mut bad = bytes.clone();
        bad[0] ^= 1;
        assert!(StreamingIndex::parse(&bad).is_err());
        let mut bad = bytes.clone();
        bad[10] = 12; // leaf cap not a power of two
        assert!(StreamingIndex::parse(&bad).is_err());
        let mut bad = bytes.clone();
        // First region's visibility list no longer starts with itself.
        let lists = STREAMING_INDEX_HEADER_BYTES + 5 * REGION_ENTRY_BYTES;
        bad[lists] = 3;
        assert!(StreamingIndex::parse(&bad).is_err());
        assert!(StreamingIndex::parse(&bytes[..bytes.len() - 4]).is_err());
    }

    #[test]
    fn a_loader_without_streaming_rejects_the_wire_stub_code() {
        assert!(!super::super::valid_leaf_contents(CONTENTS_UNRESIDENT));
        let w = world(3);
        let mut map = PxbspResidentMap::new();
        assert_eq!(
            map.load(1, &mut SliceReader::new(&w.container)),
            Err(PxbspMapLoadError::StreamedWorldUnsupported)
        );
    }

    #[test]
    fn a_fresh_map_is_all_stub_and_reads_solid() {
        let w = world(4);
        let map = load(&w, 2);
        assert!(map.is_streamed());
        map.check_integrity().expect("integrity");
        for r in 0..4 {
            for hull in 1..3 {
                assert_eq!(
                    hull_contents(&map, hull, 100 * r + 10),
                    CONTENTS_SOLID,
                    "hull {hull}"
                );
            }
            assert_eq!(map.unresident_region_at(at(100 * r + 10)), Some(r as u16));
            let leaf = map.point_leaf_index(at(100 * r + 10)).unwrap();
            assert_eq!(map.leaves().get(leaf).unwrap().contents, CONTENTS_SOLID);
        }
    }

    #[test]
    fn installing_relocates_into_the_slot_and_links_the_tree() {
        let w = world(4);
        let mut map = load(&w, 2);
        // Region 2 into slot 1.
        map.install_region(2, 1, &w.payloads[2]).unwrap();
        map.check_integrity().expect("integrity");
        let state = map.streaming().unwrap();
        assert_eq!(state.slot_of(2), Some(1));
        assert_eq!(map.region_at_point(at(210)), Some(2));
        assert_eq!(map.unresident_region_at(at(210)), None);
        assert_eq!(map.unresident_region_at(at(10)), Some(0));
        // Hull 1: x < 250 is solid inside the installed cell's back half.
        assert_eq!(hull_contents(&map, 1, 210), CONTENTS_SOLID);
        assert_eq!(hull_contents(&map, 1, 270), CONTENTS_EMPTY);
        assert_eq!(hull_contents(&map, 2, 270), CONTENTS_EMPTY);
        // Neighbours stay walls.
        assert_eq!(hull_contents(&map, 1, 310), CONTENTS_SOLID);
        let leaf = map.point_leaf_index(at(270)).unwrap();
        let top_leaves = map.streaming().unwrap().index().top.leaves as usize;
        assert_eq!(leaf, top_leaves + 8 + 1, "slot 1, local leaf 2");
        // Mark and face indices were relocated by slot 1's bases.
        let record = map.leaves().get(leaf).unwrap();
        assert_eq!(record.first_mark_surface, 4 + 1);
        let face = map.mark_surfaces().get(5).unwrap();
        assert_eq!(face, 4 + 1);
        assert_eq!(map.faces().get(face as usize).unwrap().first_vertex, 8 + 3);
        // Uninstall restores the wall.
        assert_eq!(map.uninstall_region(2), Ok(1));
        map.check_integrity().expect("integrity");
        assert_eq!(hull_contents(&map, 1, 270), CONTENTS_SOLID);
        assert_eq!(map.uninstall_region(2), Err(StreamError::NotInstalled));
    }

    #[test]
    fn streamed_installs_build_the_same_image_as_the_one_shot_install() {
        let w = world(4);
        let mut whole = load(&w, 3);
        for (region, slot) in [(2u16, 1u16), (0, 2), (3, 0)] {
            whole
                .install_region(region, slot, &w.payloads[region as usize])
                .unwrap();
        }
        // Chunk sizes that split every record and the header.
        for chunk in [1usize, 5, 7, 13, 64, 1000, 4096] {
            let mut streamed = load(&w, 3);
            let mut feeds = 0usize;
            for (region, slot) in [(2u16, 1u16), (0, 2), (3, 0)] {
                let payload = &w.payloads[region as usize];
                let mut job = streamed.begin_install(region, slot).unwrap();
                let mut progress = InstallProgress::Working;
                for piece in payload.chunks(chunk) {
                    assert_eq!(progress, InstallProgress::Working);
                    progress = streamed.install_feed(&mut job, piece).unwrap();
                    feeds += 1;
                    // Staging writes only slot bytes; nothing is linked yet.
                    if progress == InstallProgress::Working {
                        assert_eq!(streamed.streaming().unwrap().slot_of(region), None);
                    }
                }
                assert_eq!(progress, InstallProgress::Staged, "chunk {chunk}");
                assert!(job.is_staged());
                assert_eq!(job.fed_bytes(), payload.len());
                assert_eq!(job.materials(), 1, "both faces use material 0");
                assert_eq!(
                    streamed.unresident_region_at(at(i32::from(region) * 100 + 10)),
                    Some(region),
                    "still a wall before the link"
                );
                streamed.link_region(&mut job).unwrap();
                assert_eq!(
                    streamed.link_region(&mut job),
                    Err(StreamError::BadRegion("install is not staged"))
                );
            }
            streamed.check_integrity().expect("integrity");
            assert_eq!(
                streamed.owned_slice_mut(),
                whole.owned_slice_mut(),
                "chunk {chunk}"
            );
            if chunk == 1 {
                assert!(feeds > 100, "a one byte chunk really slices ({feeds})");
            }
        }
    }

    #[test]
    fn sector_padding_after_the_payload_is_ignored() {
        let w = world(3);
        let mut map = load(&w, 2);
        let mut job = map.begin_install(1, 0).unwrap();
        let sectors = w.payloads[1].len().div_ceil(SECTOR_BYTES as usize);
        let at = w.index.regions[1].sector_start as usize * SECTOR_BYTES as usize;
        let padded = &w.pack[at..at + sectors * SECTOR_BYTES as usize];
        assert_eq!(
            map.install_feed(&mut job, padded).unwrap(),
            InstallProgress::Staged
        );
        assert_eq!(job.fed_bytes(), w.payloads[1].len());
        map.link_region(&mut job).unwrap();
        map.check_integrity().unwrap();
    }

    #[test]
    fn a_refused_streamed_install_never_links() {
        let w = world(3);
        let mut map = load(&w, 2);
        // A dangling mark is refused as the record arrives.
        let mut build = region(1, 3).0;
        build.marks[0] = 9;
        let payload = build.encode();
        let mut job = map.begin_install(1, 0).unwrap();
        let mut result = Ok(InstallProgress::Working);
        for piece in payload.chunks(3) {
            result = map.install_feed(&mut job, piece);
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result, Err(StreamError::BadReference("mark surface")));
        assert_eq!(
            map.link_region(&mut job),
            Err(StreamError::BadRegion("install is not staged"))
        );
        map.check_integrity().unwrap();
        assert_eq!(map.streaming().unwrap().slot_of(1), None);
        // A flipped byte passes every record check and fails the checksum at
        // the end, still unlinked.
        let mut bad = w.payloads[1].clone();
        let flip = bad.len() - 5;
        bad[flip] ^= 0x01;
        let mut job = map.begin_install(1, 0).unwrap();
        assert_eq!(
            map.install_feed(&mut job, &bad),
            Err(StreamError::BadChecksum)
        );
        map.check_integrity().unwrap();
        assert_eq!(map.streaming().unwrap().slot_of(1), None);
        // The slot is still free for the good payload.
        map.install_region(1, 0, &w.payloads[1]).unwrap();
        map.check_integrity().unwrap();
    }

    #[test]
    fn exact_loading_allocates_the_image_and_nothing_more() {
        let w = world(4);
        let reference = load(&w, 3);
        let mut exact = PxbspResidentMap::with_capacity(0);
        exact
            .load_streamed_exact(1, &mut SliceReader::new(&w.container), 3)
            .expect("exact load");
        let bytes = exact.owned_slice_mut().len();
        assert_eq!(bytes, reference.storage_bytes().len());
        assert_eq!(exact.storage_capacity(), bytes.div_ceil(4) * 4);
        assert_eq!(exact.storage_bytes().as_ptr() as usize & 3, 0);
        exact.install_region(1, 2, &w.payloads[1]).unwrap();
        exact.check_integrity().unwrap();
    }

    #[test]
    fn incremental_fnv_matches_the_one_shot_hash() {
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i * 31 + 7) as u8).collect();
        let mut hash = Fnv32::new();
        for chunk in bytes.chunks(333) {
            hash.update(chunk);
        }
        assert_eq!(hash.finish(), fnv1a32(&bytes));
    }

    #[test]
    fn dense_visibility_follows_the_slots_not_the_regions() {
        let w = world(4);
        let mut map = load(&w, 3);
        // Region 1 in slot 2, its neighbours 0 and 2 in slots 0 and 1.
        map.install_region(1, 2, &w.payloads[1]).unwrap();
        map.install_region(0, 0, &w.payloads[0]).unwrap();
        map.install_region(2, 1, &w.payloads[2]).unwrap();
        let leaf = map.point_leaf_index(at(110)).unwrap();
        let mut scratch = [0u8; 1024];
        let mut dense = [0u8; 1024];
        let bits = map
            .streamed_leaf_visibility_into(leaf, &mut scratch, &mut dense)
            .unwrap();
        let rpad = 8;
        assert_eq!(bits, rpad + 3 * 8);
        // Slot s contributes bits rpad + 8 s .. +2 (two leaves per region).
        for slot in 0..3 {
            let byte = dense[(rpad + slot * 8) / 8];
            assert_eq!(byte, 0b11, "slot {slot}");
        }
        assert_eq!(map.streaming().unwrap().pvs_missing(), 0);
        // Evict region 2: its slot's bits vanish and the miss is counted.
        map.uninstall_region(2).unwrap();
        let leaf = map.point_leaf_index(at(110)).unwrap();
        map.streamed_leaf_visibility_into(leaf, &mut scratch, &mut dense)
            .unwrap();
        assert_eq!(dense[(rpad + 8) / 8], 0);
        assert_eq!(map.streaming().unwrap().pvs_missing(), 1);
        // The legacy accessor routes to the same expansion.
        let mut out = [0u8; 1024];
        assert_eq!(map.leaf_visibility_into(leaf, &mut out), Some(bits));
    }

    #[test]
    fn refused_installs_leave_the_map_untouched() {
        let w = world(3);
        let mut map = load(&w, 2);
        let before = map.owned_slice_mut().to_vec();
        // Wrong region for the payload.
        assert!(map.install_region(1, 0, &w.payloads[0]).is_err());
        // Flipped body byte: checksum.
        let mut bad = w.payloads[1].clone();
        let at = bad.len() - 5;
        bad[at] ^= 0xff;
        assert_eq!(
            map.install_region(1, 0, &bad),
            Err(StreamError::BadChecksum)
        );
        // Dangling mark with a valid checksum.
        let mut build = region(1, 3).0;
        build.marks[0] = 9;
        assert_eq!(
            map.install_region(1, 0, &build.encode()),
            Err(StreamError::BadReference("mark surface"))
        );
        assert_eq!(
            map.owned_slice_mut(),
            &before[..],
            "refused installs write nothing"
        );
        // Out of range slot, double install, busy slot.
        assert_eq!(
            map.install_region(1, 2, &w.payloads[1]),
            Err(StreamError::SlotOutOfRange)
        );
        map.install_region(1, 0, &w.payloads[1]).unwrap();
        assert_eq!(
            map.install_region(1, 1, &w.payloads[1]),
            Err(StreamError::AlreadyInstalled)
        );
        assert_eq!(
            map.install_region(2, 0, &w.payloads[2]),
            Err(StreamError::SlotBusy)
        );
        map.uninstall_region(1).unwrap();
        map.check_integrity().unwrap();
    }

    #[test]
    fn region_loader_reads_sector_aligned_payloads_and_checks_them() {
        let w = world(4);
        let mut loader = RegionLoader::new(SliceReader::new(&w.pack), 0);
        let mut buf = vec![0u8; 4096];
        for r in 0..4u16 {
            let n = loader.load(&w.index, r, &mut buf).unwrap();
            assert_eq!(&buf[..n], &w.payloads[r as usize][..]);
        }
        assert_eq!(
            loader.load(&w.index, 9, &mut buf),
            Err(RegionReadError::Stream(StreamError::RegionOutOfRange))
        );
        let mut small = [0u8; 16];
        assert!(loader.load(&w.index, 0, &mut small).is_err());
        let mut corrupt = w.pack.clone();
        corrupt[SECTOR_BYTES as usize + 70] ^= 1;
        let mut loader = RegionLoader::new(SliceReader::new(&corrupt), 0);
        assert_eq!(
            loader.load(&w.index, 1, &mut buf),
            Err(RegionReadError::Stream(StreamError::BadChecksum))
        );
    }

    /// Random install and uninstall orders: after every operation the map has
    /// no dangling reference, the slot table matches the links, and exactly
    /// the absent regions read solid.
    #[test]
    fn random_install_orders_never_dangle() {
        let regions = 9u16;
        let w = world(regions);
        let slots = 4u16;
        let mut seed = 0x1234_5678u32;
        let mut next = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            seed >> 8
        };
        for round in 0..40 {
            let mut map = load(&w, slots);
            for _ in 0..120 {
                let region = (next() % u32::from(regions)) as u16;
                let resident = map.streaming().unwrap().is_resident(region);
                if resident && next() % 2 == 0 {
                    map.uninstall_region(region).unwrap();
                } else if !resident {
                    match map.streaming().unwrap().free_slot() {
                        Some(slot) => map
                            .install_region(region, slot, &w.payloads[region as usize])
                            .unwrap(),
                        None => assert_eq!(
                            map.install_region(region, 0, &w.payloads[region as usize]),
                            Err(StreamError::SlotBusy)
                        ),
                    }
                }
                map.check_integrity()
                    .unwrap_or_else(|e| panic!("round {round}: {e:?}"));
                for r in 0..i32::from(regions) {
                    let resident = map.streaming().unwrap().is_resident(r as u16);
                    for hull in 1..3 {
                        let front = hull_contents(&map, hull, 100 * r + 70);
                        let back = hull_contents(&map, hull, 100 * r + 20);
                        assert_eq!(back, CONTENTS_SOLID);
                        assert_eq!(front == CONTENTS_EMPTY, resident, "region {r} hull {hull}");
                    }
                    assert_eq!(
                        map.unresident_region_at(at(100 * r + 70)).is_some(),
                        !resident
                    );
                }
            }
        }
    }
}
