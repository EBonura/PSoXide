//! An owned copy of a cooked room chunk, with geometry helpers shared by the
//! probes and the codec.

use psx_grid::{
    ColPlane, ColWall, Liquid, LodBlock, Mover, PlaneRef, Quad, RCell, RoomView, Sector, SectorExt,
    SplitB, Vtx,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Room {
    pub width: usize,
    pub depth: usize,
    pub s: i32,
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
    pub flags: u16,
    pub min_y: i16,
    pub max_y: i16,
    pub lod_far_cells: u8,
    pub lod_shift: u8,
}

impl Room {
    pub fn from_view(view: &RoomView<'_>, s: i32) -> Self {
        let h = view.col.header;
        Self {
            width: usize::from(h.width),
            depth: usize::from(h.depth),
            s,
            sectors: view.col.sectors.to_vec(),
            exts: view.col.exts.to_vec(),
            walls: view.col.walls.to_vec(),
            splitbs: view.col.splitbs.to_vec(),
            col_planes: view.col.col_planes.to_vec(),
            liquids: view.col.liquids.to_vec(),
            movers: view.col.movers.to_vec(),
            mover_verts: view.col.mover_verts.to_vec(),
            material_use: view.render.material_use.to_vec(),
            vertices: view.render.vertices.to_vec(),
            quads: level_ids(view.render.material_use, view.render.quads),
            cells: view.render.cells.to_vec(),
            lod_blocks: view.render.lod_blocks.to_vec(),
            lod_quads: level_ids(view.render.material_use, view.render.lod_quads),
            plane_refs: view.render.plane_refs.to_vec(),
            flags: h.flags,
            min_y: h.min_y,
            max_y: h.max_y,
            lod_far_cells: h.lod_far_cells,
            lod_shift: h.lod_shift,
        }
    }

    /// Position of vertex `i`.
    pub fn pos(&self, i: u16) -> [i32; 3] {
        let v = self.vertices[usize::from(i)];
        [i32::from(v.x), i32::from(v.y), i32::from(v.z)]
    }

    /// Positions of the corners of `q` (three for a triangle).
    pub fn ring(&self, q: &Quad) -> Vec<[i32; 3]> {
        let n = if q.flags & psx_grid::QUAD_TRI != 0 {
            3
        } else {
            4
        };
        (0..n).map(|k| self.pos(q.v[k])).collect()
    }

    /// Cell index range of `cell`.
    pub fn cell_quads(&self, cell: usize) -> std::ops::Range<usize> {
        let c = self.cells[cell];
        usize::from(c.first)..usize::from(c.first) + usize::from(c.count)
    }
}

/// Quads with their room-local material index replaced by the level material
/// id, the form the study codecs were written against.
fn level_ids(material_use: &[u8], quads: &[psx_grid::Quad]) -> Vec<psx_grid::Quad> {
    quads
        .iter()
        .map(|q| psx_grid::Quad {
            material: material_use[usize::from(q.material)],
            ..*q
        })
        .collect()
}
