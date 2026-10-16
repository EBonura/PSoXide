//! Serialisers for the level core and room chunks. Host side only: the guest
//! never builds a file, it casts one.
//!
//! The writer takes already-integer record tables, lays the collision block
//! out first and the render block second, aligns every table to four bytes
//! and fills in the header. The cook and the tests share it, so the bytes a
//! test reads back are the bytes Play ships.

use alloc::vec::Vec;

use crate::format::*;

/// Everything a room chunk holds, as plain tables.
#[derive(Clone, Debug, Default)]
pub struct RoomChunk {
    pub room_id: u16,
    pub flags: u16,
    pub width: u8,
    pub depth: u8,
    pub min_y: i16,
    pub max_y: i16,
    pub lod_far_cells: u8,
    pub lod_shift: u8,
    pub sectors: Vec<Sector>,
    pub exts: Vec<SectorExt>,
    pub walls: Vec<ColWall>,
    pub splitbs: Vec<SplitB>,
    pub col_planes: Vec<ColPlane>,
    pub liquids: Vec<Liquid>,
    pub movers: Vec<Mover>,
    pub mover_verts: Vec<u16>,
    pub material_use: Vec<u8>,
    pub vertices: Vec<Vtx>,
    pub quads: Vec<Quad>,
    pub cells: Vec<RCell>,
    pub lod_blocks: Vec<LodBlock>,
    pub lod_quads: Vec<Quad>,
    pub plane_refs: Vec<PlaneRef>,
}

fn align4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn put(
    out: &mut Vec<u8>,
    refs: &mut [SectionRef; SECTION_COUNT],
    id: usize,
    bytes: &[u8],
    count: usize,
) {
    align4(out);
    refs[id] = SectionRef {
        offset: out.len() as u32,
        count: count as u32,
    };
    out.extend_from_slice(bytes);
}

impl RoomChunk {
    /// Size of each section's bytes, in section order, for reports.
    pub fn section_bytes(&self) -> [usize; SECTION_COUNT] {
        let counts = [
            self.sectors.len(),
            self.exts.len(),
            self.walls.len(),
            self.splitbs.len(),
            self.col_planes.len(),
            self.liquids.len(),
            self.movers.len(),
            self.mover_verts.len(),
            self.material_use.len(),
            self.vertices.len(),
            self.quads.len(),
            self.cells.len(),
            self.lod_blocks.len(),
            self.lod_quads.len(),
            self.plane_refs.len(),
        ];
        let mut out = [0; SECTION_COUNT];
        for i in 0..SECTION_COUNT {
            out[i] = counts[i] * SECTION_RECORD_BYTES[i];
        }
        out
    }

    /// The chunk bytes, unpadded. The caller pads to a sector when it packs.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = alloc::vec![0u8; core::mem::size_of::<RoomHeader>()];
        let mut refs = [SectionRef::default(); SECTION_COUNT];
        put(
            &mut out,
            &mut refs,
            section::SECTORS,
            bytes_of(&self.sectors),
            self.sectors.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::SECTOR_EXTS,
            bytes_of(&self.exts),
            self.exts.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::WALLS,
            bytes_of(&self.walls),
            self.walls.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::SPLITBS,
            bytes_of(&self.splitbs),
            self.splitbs.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::COL_PLANES,
            bytes_of(&self.col_planes),
            self.col_planes.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::LIQUIDS,
            bytes_of(&self.liquids),
            self.liquids.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::MOVERS,
            bytes_of(&self.movers),
            self.movers.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::MOVER_VERTS,
            bytes_of(&self.mover_verts),
            self.mover_verts.len(),
        );
        align4(&mut out);
        let render_offset = out.len() as u32;
        put(
            &mut out,
            &mut refs,
            section::MATERIAL_USE,
            &self.material_use,
            self.material_use.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::VERTICES,
            bytes_of(&self.vertices),
            self.vertices.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::QUADS,
            bytes_of(&self.quads),
            self.quads.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::CELLS,
            bytes_of(&self.cells),
            self.cells.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::LOD_BLOCKS,
            bytes_of(&self.lod_blocks),
            self.lod_blocks.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::LOD_QUADS,
            bytes_of(&self.lod_quads),
            self.lod_quads.len(),
        );
        put(
            &mut out,
            &mut refs,
            section::PLANE_REFS,
            bytes_of(&self.plane_refs),
            self.plane_refs.len(),
        );
        align4(&mut out);
        let header = RoomHeader {
            magic: ROOM_MAGIC,
            version: VERSION,
            flags: self.flags,
            byte_len: out.len() as u32,
            render_offset,
            room_id: self.room_id,
            width: self.width,
            depth: self.depth,
            min_y: self.min_y,
            max_y: self.max_y,
            lod_far_cells: self.lod_far_cells,
            lod_shift: self.lod_shift,
            reserved: 0,
            sections: refs,
            reserved2: 0,
        };
        out[..core::mem::size_of::<RoomHeader>()]
            .copy_from_slice(bytes_of(core::slice::from_ref(&header)));
        out
    }
}

/// The resident level core as plain tables.
#[derive(Clone, Debug, Default)]
pub struct LevelCore {
    pub flags: u32,
    pub sector_units: u16,
    pub pool_pages: u16,
    pub resident_hops: u8,
    pub prefetch_hops: u8,
    pub rooms: Vec<RoomRecord>,
    pub portals: Vec<Portal>,
    pub materials: Vec<Material>,
}

impl LevelCore {
    /// The core bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header = LevelHeader {
            magic: LEVEL_MAGIC,
            version: VERSION,
            min_reader: MIN_READER,
            flags: self.flags,
            room_count: self.rooms.len() as u16,
            portal_count: self.portals.len() as u16,
            material_count: self.materials.len() as u16,
            sector_units: self.sector_units,
            pool_pages: self.pool_pages,
            resident_hops: self.resident_hops,
            prefetch_hops: self.prefetch_hops,
            reserved: 0,
            layout_hash: LAYOUT_HASH,
        };
        let mut out = Vec::new();
        out.extend_from_slice(bytes_of(core::slice::from_ref(&header)));
        out.extend_from_slice(bytes_of(&self.rooms));
        out.extend_from_slice(bytes_of(&self.portals));
        out.extend_from_slice(bytes_of(&self.materials));
        align4(&mut out);
        out
    }
}

/// A heap copy of `bytes` whose start is word aligned, for tests and tools
/// that read a file back with the zero-copy views.
pub fn aligned_copy(bytes: &[u8]) -> Vec<u32> {
    let mut words = alloc::vec![0u32; bytes.len().div_ceil(4)];
    // SAFETY: the destination holds at least `bytes.len()` bytes and the two
    // buffers cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            words.as_mut_ptr().cast::<u8>(),
            bytes.len(),
        );
    }
    words
}

/// View an aligned word buffer as the first `len` bytes.
pub fn words_as_bytes(words: &[u32], len: usize) -> &[u8] {
    // SAFETY: `len` never exceeds the buffer (callers pass the source length).
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), len.min(words.len() * 4)) }
}
