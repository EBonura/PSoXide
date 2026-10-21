//! Serialisers for the level core and room chunks. Host side only: the guest
//! never builds a file, it casts one.
//!
//! The writer takes already-integer record tables, lays the collision block
//! out first and the render block second, aligns every table to four bytes
//! and fills in the header. The cook and the tests share it, so the bytes a
//! test reads back are the bytes Play ships.

use alloc::vec::Vec;

use crate::format::*;
use crate::lattice::LatticeTables;

/// The leaf tree's tables (see [`TreeExt`]): present when the room has a leaf
/// that is not a whole sector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeTables {
    /// The room's world origin modulo four sectors, `x | z << 2`.
    pub origin_mod: u8,
    /// One entry per sector, `cz * width + cx`.
    pub index: Vec<u16>,
    /// 16 entries per block.
    pub blocks: Vec<u16>,
}

/// The extra spans of a room (see [`SpanExt`]): present when some column
/// holds more than one span.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanTables {
    /// One entry per `SECTORS` record: an index into `runs`, or [`NO_SPAN`].
    pub index: Vec<u16>,
    /// One run per record that has extra spans.
    pub runs: Vec<SpanRun>,
    /// The extra spans, bottom to top within each run.
    pub spans: Vec<Sector>,
}

/// The light probes of a room (see [`ProbeExt`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeTables {
    /// One entry per sector, `cz * width + cx`: a plain probe, or a pointer
    /// into `multi` ([`PROBE_MULTI`]).
    pub cells: Vec<Probe>,
    /// The probes of the columns of several floors, bottom to top within
    /// each column.
    pub multi: Vec<Probe>,
}

impl ProbeTables {
    /// Tables for a `width` by `depth` room with no probe anywhere.
    pub fn empty(width: usize, depth: usize) -> Self {
        Self {
            cells: alloc::vec![
                Probe {
                    y: 0,
                    rgb: PROBE_MULTI,
                };
                width * depth
            ],
            multi: Vec::new(),
        }
    }

    /// Put the floors of sector `cell` (`(y, packed colour)` pairs, bottom to
    /// top). One floor is stored in the cell itself, several in `multi`, none
    /// leaves the cell empty.
    pub fn set_cell(&mut self, cell: usize, floors: &[(i16, u16)]) {
        self.cells[cell] = match floors {
            [] => Probe {
                y: 0,
                rgb: PROBE_MULTI,
            },
            [(y, rgb)] => Probe {
                y: *y,
                rgb: *rgb & !PROBE_MULTI,
            },
            many => {
                let first = self.multi.len() as i16;
                self.multi.extend(many.iter().map(|&(y, rgb)| Probe {
                    y,
                    rgb: rgb & !PROBE_MULTI,
                }));
                Probe {
                    y: first,
                    rgb: PROBE_MULTI | many.len() as u16,
                }
            }
        };
    }

    /// Bytes the tables take in a chunk, header included.
    pub fn bytes(&self) -> usize {
        core::mem::size_of::<ProbeExt>()
            + core::mem::size_of::<Probe>() * (self.cells.len() + self.multi.len())
    }
}

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
    /// One record per sector, or per leaf when `tree` is set.
    pub sectors: Vec<Sector>,
    /// The leaf tree, `None` for a dense room.
    pub tree: Option<TreeTables>,
    /// Extra spans of the columns that have them; `None` when every column
    /// is a single span.
    pub spans: Option<SpanTables>,
    /// The room's light probes; `None` for a room that has none.
    pub probes: Option<ProbeTables>,
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

fn put_ref(out: &mut Vec<u8>, slot: &mut SectionRef, bytes: &[u8], count: usize) {
    align4(out);
    *slot = SectionRef {
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

    /// Bytes of the leaf tree part (extension record, index and sub-blocks),
    /// zero for a dense room.
    pub fn tree_bytes(&self) -> usize {
        let tree = self.tree.as_ref().map_or(0, |t| {
            core::mem::size_of::<TreeExt>() + 2 * t.index.len() + 2 * t.blocks.len()
        });
        tree + self.span_bytes()
    }

    /// Bytes of the extra-span tables, zero without spans. A dense room with
    /// spans also carries an empty [`TreeExt`], counted here.
    pub fn span_bytes(&self) -> usize {
        self.spans.as_ref().map_or(0, |s| {
            let tree_ext = if self.tree.is_none() {
                core::mem::size_of::<TreeExt>()
            } else {
                0
            };
            tree_ext
                + core::mem::size_of::<SpanExt>()
                + 2 * s.index.len()
                + core::mem::size_of::<SpanRun>() * s.runs.len()
                + core::mem::size_of::<Sector>() * s.spans.len()
        })
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
        let mut tree_ext = 0u32;
        let mut tree_flag = 0u16;
        let mut span_flag = 0u16;
        let mut origin_mod = 0u8;
        if self.tree.is_some() || self.spans.is_some() {
            align4(&mut out);
            tree_ext = out.len() as u32;
            // The extension record first, filled in once the tables have
            // their places. A dense room with spans carries empty tree tables.
            out.extend_from_slice(&[0u8; core::mem::size_of::<TreeExt>()]);
            // The span record sits right after it (`tree_ext` plus the size
            // of `TreeExt`), reserved now and filled in last.
            let span_at = self.spans.as_ref().map(|_| {
                out.extend_from_slice(&[0u8; core::mem::size_of::<SpanExt>()]);
                out.len() - core::mem::size_of::<SpanExt>()
            });
            let mut refs2 = [SectionRef::default(); 2];
            if let Some(tree) = &self.tree {
                tree_flag = CHUNK_TREE;
                origin_mod = tree.origin_mod;
                put_ref(
                    &mut out,
                    &mut refs2[0],
                    bytes_of(&tree.index),
                    tree.index.len(),
                );
                put_ref(
                    &mut out,
                    &mut refs2[1],
                    bytes_of(&tree.blocks),
                    tree.blocks.len() / SUBBLOCK_ENTRIES,
                );
            }
            let ext = TreeExt {
                layout: TREE_LAYOUT_HASH,
                sector_index: refs2[0],
                subblocks: refs2[1],
            };
            out[tree_ext as usize..tree_ext as usize + core::mem::size_of::<TreeExt>()]
                .copy_from_slice(bytes_of(core::slice::from_ref(&ext)));
            if let (Some(spans), Some(at)) = (&self.spans, span_at) {
                span_flag = ROOM_SPANS;
                let mut refs3 = [SectionRef::default(); 3];
                put_ref(
                    &mut out,
                    &mut refs3[0],
                    bytes_of(&spans.index),
                    spans.index.len(),
                );
                put_ref(
                    &mut out,
                    &mut refs3[1],
                    bytes_of(&spans.runs),
                    spans.runs.len(),
                );
                put_ref(
                    &mut out,
                    &mut refs3[2],
                    bytes_of(&spans.spans),
                    spans.spans.len(),
                );
                let block = SpanExt {
                    layout: SPAN_LAYOUT_HASH,
                    index: refs3[0],
                    runs: refs3[1],
                    spans: refs3[2],
                };
                out[at..at + core::mem::size_of::<SpanExt>()]
                    .copy_from_slice(bytes_of(core::slice::from_ref(&block)));
            }
        }
        align4(&mut out);
        let render_offset = out.len() as u32;
        let mut probe_flag = 0u16;
        if let Some(probes) = &self.probes {
            // The probe record opens the render block, its tables right
            // behind it; the other render sections find their own places
            // through the section table, so they simply follow.
            probe_flag = ROOM_PROBES;
            let at = out.len();
            out.extend_from_slice(&[0u8; core::mem::size_of::<ProbeExt>()]);
            let mut refs4 = [SectionRef::default(); 2];
            put_ref(
                &mut out,
                &mut refs4[0],
                bytes_of(&probes.cells),
                probes.cells.len(),
            );
            put_ref(
                &mut out,
                &mut refs4[1],
                bytes_of(&probes.multi),
                probes.multi.len(),
            );
            let block = ProbeExt {
                layout: PROBE_LAYOUT_HASH,
                cells: refs4[0],
                multi: refs4[1],
            };
            out[at..at + core::mem::size_of::<ProbeExt>()]
                .copy_from_slice(bytes_of(core::slice::from_ref(&block)));
        }
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
            flags: self.flags | tree_flag | span_flag | probe_flag,
            byte_len: out.len() as u32,
            render_offset,
            room_id: self.room_id,
            width: self.width,
            depth: self.depth,
            min_y: self.min_y,
            max_y: self.max_y,
            lod_far_cells: self.lod_far_cells,
            lod_shift: self.lod_shift,
            origin_mod,
            reserved: 0,
            sections: refs,
            tree_ext,
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
    /// Pages (2 KiB) of the largest room chunk.
    pub max_room_pages: u8,
    /// Draw distance, sectors; 0 is unbounded.
    pub draw_far: u8,
    /// Entity activation radius, sectors.
    pub active_radius: u8,
    /// Prefetch radius beyond the draw distance, sectors.
    pub lead_radius: u8,
    pub maxima: RenderMaxima,
    pub rooms: Vec<RoomRecord>,
    pub portals: Vec<Portal>,
    pub materials: Vec<Material>,
    /// Per-room sector occupancy, one bit per sector of the room's rectangle
    /// (row major). Empty: every room owns its rectangle and no table is
    /// written, so levels without overlapping rooms cook as before.
    pub occupancy: Vec<Vec<u8>>,
}

impl LevelCore {
    /// Set the renderer scratch maxima and the largest chunk from the room
    /// chunks, as the cook ships them.
    pub fn measure_chunks(
        &mut self,
        chunks: &[RoomChunk],
        chunk_pages: impl Iterator<Item = usize>,
    ) {
        let mut maxima = RenderMaxima::default();
        for chunk in chunks {
            maxima.include(
                chunk.vertices.len(),
                chunk.cells.len(),
                chunk.quads.len(),
                chunk.lod_blocks.len(),
                chunk.lod_quads.len(),
            );
        }
        self.maxima = maxima;
        let pages = chunk_pages.max().unwrap_or(0);
        self.max_room_pages = u8::try_from(pages).unwrap_or(u8::MAX);
    }

    /// The core bytes; panics on a level the format cannot hold (see
    /// [`Self::try_to_bytes`]).
    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_bytes().expect("a level the core can hold")
    }

    /// The core bytes. The world lattice is built from the room records
    /// here, so it can never disagree with them.
    pub fn try_to_bytes(&self) -> Result<Vec<u8>, alloc::string::String> {
        if self.rooms.len() > MAX_ROOMS {
            return Err(alloc::format!(
                "{} rooms, at most {MAX_ROOMS}",
                self.rooms.len()
            ));
        }
        if self.portals.len() > usize::from(u16::MAX) {
            return Err(alloc::format!(
                "{} portals, at most 65535",
                self.portals.len()
            ));
        }
        let lattice = LatticeTables::build(&self.rooms, u32::from(self.sector_units))?;
        let header = LevelHeader {
            magic: LEVEL_MAGIC,
            version: VERSION,
            min_reader: MIN_READER,
            flags: self.flags | if self.occupancy.is_empty() { 0 } else { LEVEL_HAS_OCCUPANCY },
            room_count: self.rooms.len() as u16,
            portal_count: self.portals.len() as u16,
            sector_units: u8::try_from(self.sector_units).expect("sector size above 255"),
            material_count: u8::try_from(self.materials.len()).expect("more than 255 materials"),
            max_room_pages: self.max_room_pages,
            draw_far: self.draw_far,
            active_radius: self.active_radius,
            lead_radius: self.lead_radius,
            lattice_shift: lattice.shift,
            reserved: 0,
            lattice_origin: lattice.origin,
            lattice_cells: lattice.cells,
            lattice_refs: lattice.rooms.len() as u16,
            max_vertices: self.maxima.vertices,
            max_cells: self.maxima.cells,
            max_quads: self.maxima.quads,
            max_lod_blocks: self.maxima.lod_blocks,
            max_lod_quads: self.maxima.lod_quads,
            layout_hash: LAYOUT_HASH,
        };
        let mut out = Vec::new();
        out.extend_from_slice(bytes_of(core::slice::from_ref(&header)));
        out.extend_from_slice(bytes_of(&self.rooms));
        out.extend_from_slice(bytes_of(&self.portals));
        out.extend_from_slice(bytes_of(&self.materials));
        out.extend_from_slice(bytes_of(&lattice.start));
        out.extend_from_slice(bytes_of(&lattice.rooms));
        if !self.occupancy.is_empty() {
            if self.occupancy.len() != self.rooms.len() {
                return Err(alloc::format!("occupancy for {} rooms, the level has {}", self.occupancy.len(), self.rooms.len()));
            }
            let mut start = Vec::with_capacity(self.rooms.len() + 1);
            let mut at = 0usize;
            for bits in &self.occupancy {
                start.push(u16::try_from(at).map_err(|_| alloc::string::String::from("occupancy table above 64 KiB"))?);
                at += bits.len();
            }
            start.push(u16::try_from(at).map_err(|_| alloc::string::String::from("occupancy table above 64 KiB"))?);
            out.extend_from_slice(bytes_of(&start));
            for bits in &self.occupancy {
                out.extend_from_slice(bits);
            }
        }
        align4(&mut out);
        Ok(out)
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
