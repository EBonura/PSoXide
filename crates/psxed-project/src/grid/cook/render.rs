//! The render block of a room: shared lattice vertices, one quad per
//! authored face and the dense cell table.
//!
//! The faces come from `grid::faces`, the same polygons the editor draws, so
//! the cook never re-derives geometry. Every corner is mapped from grid units
//! by [`RuntimeUnits::rt`] and made room-local, then deduplicated by position
//! in first-use order while walking the cells `z` major, which keeps a visible
//! row of cells on a compact vertex range.

use std::collections::HashMap;

use psx_grid::{
    Quad, RCell, Vtx, CELL_OPEN_SKY, QUAD_AXIS, QUAD_AXIS_NEG, QUAD_AXIS_SHIFT, QUAD_DOUBLE,
    QUAD_TRI,
};

use super::units::RuntimeUnits;
use crate::grid::faces::{cell_faces, GridFace};
use crate::grid::light::{lit_corner, GridLight};
use crate::grid::world::GridWorld;
use crate::ResourceId;

/// Texels one material repeats over in the packet UVs (the renderer tiles a
/// 64 by 64 texture).
pub const TILE_TEXELS: f64 = 64.0;

/// What the cook needs to know about a bound material.
#[derive(Clone, Copy, Debug)]
pub struct BoundMaterial {
    /// Manifest texture asset id.
    pub asset: u16,
    /// `psx_grid::MATERIAL_*` flags.
    pub flags: u16,
    /// 0 opaque, 1 average, 2 add, 3 subtract, 4 add-quarter.
    pub blend: u8,
    /// UV-motion table index, 0 none.
    pub uv_motion: u8,
    /// Modulation colour, `[128; 3]` neutral.
    pub tint: [u8; 3],
    /// False for the flat-white stand-in of an untextured face; its UVs are
    /// pinned to the first texel.
    pub textured: bool,
}

/// Binds document materials to level-global material slots.
pub trait MaterialBinder {
    /// The binding of `material`, or why it cannot be bound.
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String>;
}

/// The level's material table, grown in first-use order.
#[derive(Default)]
pub struct MaterialSet {
    pub slots: Vec<(Option<ResourceId>, BoundMaterial)>,
}

impl MaterialSet {
    /// Slot of `material`, binding it on first use.
    pub fn slot(
        &mut self,
        binder: &mut dyn MaterialBinder,
        material: Option<ResourceId>,
    ) -> Result<u8, String> {
        if let Some(index) = self.slots.iter().position(|(id, _)| *id == material) {
            return Ok(index as u8);
        }
        if self.slots.len() >= 255 {
            return Err("more than 255 distinct materials in one level".to_string());
        }
        let bound = binder.bind(material)?;
        self.slots.push((material, bound));
        Ok((self.slots.len() - 1) as u8)
    }
}

/// Counters for the report.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    pub faces: usize,
    pub quads: usize,
    pub triangles: usize,
    pub collapsed: usize,
    pub uv_overflow: usize,
    pub lit_corners: usize,
}

/// Render tables of one room.
#[derive(Default)]
pub struct Tables {
    pub vertices: Vec<Vtx>,
    pub quads: Vec<Quad>,
    pub cells: Vec<RCell>,
    pub material_use: Vec<u8>,
    pub stats: RenderStats,
}

/// Inputs shared by every room of one cook.
pub struct Context<'a> {
    pub world: &'a GridWorld,
    pub units: RuntimeUnits,
    pub lights: &'a [GridLight],
    pub binder: &'a mut dyn MaterialBinder,
    pub materials: &'a mut MaterialSet,
    /// Whether the segment between two world points (runtime units) is
    /// blocked by solid world.
    pub occluded: &'a dyn Fn([f64; 3], [f64; 3]) -> bool,
}

/// Texel coordinates rebased so each axis' minimum lands in one repeat, then
/// clamped so a tile never reaches the far edge of its texture. Returns the
/// packed bytes and whether the span passed one repeat.
fn rebase_texels(texels: &[[f64; 2]]) -> (Vec<[u8; 2]>, bool) {
    let mut uvs = texels.to_vec();
    let mut over = false;
    for axis in 0..2 {
        let min = uvs.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
        let shift = (min / TILE_TEXELS).floor() * TILE_TEXELS;
        for uv in &mut uvs {
            uv[axis] -= shift;
            if uv[axis] > TILE_TEXELS - 1.0 {
                if uv[axis] > TILE_TEXELS + 0.5 {
                    over = true;
                }
                uv[axis] = TILE_TEXELS - 1.0;
            }
            if uv[axis] < 0.0 {
                uv[axis] = 0.0;
            }
        }
    }
    (
        uvs.iter()
            .map(|uv| [uv[0].round() as u8, uv[1].round() as u8])
            .collect(),
        over,
    )
}

fn sub(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot_f(a: [f64; 3], b: [i32; 3]) -> f64 {
    a[0] * f64::from(b[0]) + a[1] * f64::from(b[1]) + a[2] * f64::from(b[2])
}

/// Plane normal of a snapped ring, or `None` when it collapsed to a line.
fn ring_cross(ring: &[[i32; 3]]) -> Option<[i32; 3]> {
    (1..ring.len() - 1).find_map(|i| {
        let n = cross(sub(ring[i], ring[0]), sub(ring[i + 1], ring[0]));
        (n != [0; 3]).then_some(n)
    })
}

/// Build the render tables of room `room_index`. `origin` is the room's
/// runtime origin `[x, y, z]`.
pub fn build(
    cx: &mut Context<'_>,
    room_index: usize,
    origin: [i32; 3],
    ambient: [u8; 3],
) -> Result<Tables, String> {
    let room = &cx.world.rooms[room_index];
    let mut tables = Tables::default();
    let mut vertex_of: HashMap<[i16; 3], u16> = HashMap::new();
    let mut faces: Vec<GridFace> = Vec::new();
    let mut used = [false; 256];
    for z in 0..room.depth {
        for x in 0..room.width {
            faces.clear();
            cell_faces(cx.world, room_index, x, z, &mut faces);
            let first = tables.quads.len();
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for face in &faces {
                tables.stats.faces += 1;
                // Snap the corners.
                let mut local: Vec<[i32; 3]> = Vec::with_capacity(face.grid.len());
                for g in &face.grid {
                    local.push([
                        cx.units.rt(g[0]) - origin[0],
                        cx.units.rt(g[1]) - origin[1],
                        cx.units.rt(g[2]) - origin[2],
                    ]);
                }
                // The pack wants the ring's cross product to point at the
                // visible side, the face ring reversed.
                let mut ring: Vec<usize> = (0..local.len()).rev().collect();
                // Distinct snapped corners; a face that collapses is dropped.
                let mut keep: Vec<usize> = Vec::new();
                for &i in &ring {
                    let p = local[i];
                    let last = keep.last().map(|&k| local[k]);
                    let first_p = keep.first().map(|&k| local[k]);
                    if last != Some(p) && first_p != Some(p) {
                        keep.push(i);
                    }
                }
                if keep.len() < 3 {
                    tables.stats.collapsed += 1;
                    continue;
                }
                let mut snapped: Vec<[i32; 3]> = keep.iter().map(|&i| local[i]).collect();
                let Some(mut normal) = ring_cross(&snapped) else {
                    tables.stats.collapsed += 1;
                    continue;
                };
                // Keep the visible side the unsnapped ring chose.
                if dot_f(face.normal, normal) < 0.0 {
                    keep.reverse();
                    snapped.reverse();
                    normal = normal.map(|v| -v);
                }
                ring.clear();
                if keep.len() > 4 {
                    return Err(format!(
                        "room {room_index} sector {x},{z}: a face with more than four corners"
                    ));
                }
                let slot = cx.materials.slot(cx.binder, face.material)?;
                let (_, bound) = cx.materials.slots[usize::from(slot)];
                used[usize::from(slot)] = true;
                // Plane.
                let length = (f64::from(normal[0]).powi(2)
                    + f64::from(normal[1]).powi(2)
                    + f64::from(normal[2]).powi(2))
                .sqrt();
                let n8 = normal.map(|v| {
                    ((f64::from(v) / length) * 127.0)
                        .round()
                        .clamp(-127.0, 127.0) as i8
                });
                let p0 = snapped[0];
                let dot_n =
                    i32::from(n8[0]) * p0[0] + i32::from(n8[1]) * p0[1] + i32::from(n8[2]) * p0[2];
                let mut flags = 0u8;
                let mut d = i16::try_from(dot_n >> 7).map_err(|_| {
                    format!("room {room_index} sector {x},{z}: plane constant out of range")
                })?;
                let zeros = normal.map(|v| v == 0);
                if zeros.iter().filter(|z| **z).count() == 2 {
                    let axis = normal.iter().position(|v| *v != 0).unwrap_or(0);
                    let negative = normal[axis] < 0;
                    flags |= QUAD_AXIS | ((axis as u8) << QUAD_AXIS_SHIFT);
                    if negative {
                        flags |= QUAD_AXIS_NEG;
                    }
                    let coordinate = i16::try_from(p0[axis])
                        .map_err(|_| format!("room {room_index}: coordinate out of range"))?;
                    d = if negative { -coordinate } else { coordinate };
                }
                if keep.len() == 3 {
                    flags |= QUAD_TRI;
                }
                if face.double_sided || bound.flags & 3 == 2 {
                    flags |= QUAD_DOUBLE;
                }
                // UVs per kept corner.
                let texels: Vec<[f64; 2]> = keep.iter().map(|&i| face.texels[i]).collect();
                let (packed_uv, over) = if bound.textured {
                    rebase_texels(&texels)
                } else {
                    (vec![[0, 0]; texels.len()], false)
                };
                if over {
                    tables.stats.uv_overflow += 1;
                }
                let mut quad = Quad {
                    d,
                    n: n8,
                    material: slot,
                    flags,
                    ..Quad::default()
                };
                for (k, &i) in keep.iter().enumerate() {
                    let p = local[i];
                    let key = [
                        i16::try_from(p[0])
                            .map_err(|_| format!("room {room_index}: x out of range"))?,
                        i16::try_from(p[1])
                            .map_err(|_| format!("room {room_index}: y out of range"))?,
                        i16::try_from(p[2])
                            .map_err(|_| format!("room {room_index}: z out of range"))?,
                    ];
                    let next = tables.vertices.len();
                    let id = *vertex_of.entry(key).or_insert_with(|| {
                        tables.vertices.push(Vtx {
                            x: key[0],
                            y: key[1],
                            z: key[2],
                            pad: 0,
                        });
                        next as u16
                    });
                    if tables.vertices.len() > usize::from(u16::MAX) {
                        return Err(format!("room {room_index} has more than 65535 vertices"));
                    }
                    quad.v[k] = id;
                    let world_point = [
                        f64::from(p[0] + origin[0]),
                        f64::from(p[1] + origin[1]),
                        f64::from(p[2] + origin[2]),
                    ];
                    let colour = lit_corner(
                        world_point,
                        face.normal,
                        bound.tint,
                        ambient,
                        cx.lights,
                        cx.occluded,
                    );
                    quad.color[k] = u32::from(colour[0])
                        | (u32::from(colour[1]) << 8)
                        | (u32::from(colour[2]) << 16);
                    quad.uv[k] = u16::from(packed_uv[k][0]) | (u16::from(packed_uv[k][1]) << 8);
                    lo = lo.min(p[1]);
                    hi = hi.max(p[1]);
                }
                tables.stats.lit_corners += keep.len();
                if keep.len() == 3 {
                    quad.v[3] = quad.v[2];
                    quad.color[3] = quad.color[2];
                    quad.uv[3] = quad.uv[2];
                    tables.stats.triangles += 1;
                } else {
                    tables.stats.quads += 1;
                }
                tables.quads.push(quad);
            }
            let count = tables.quads.len() - first;
            if first > usize::from(u16::MAX) {
                return Err(format!("room {room_index} has more than 65535 quads"));
            }
            let count = u8::try_from(count).map_err(|_| {
                format!("room {room_index} sector {x},{z}: {count} quads in one cell, at most 255")
            })?;
            let doc = room.sector(x, z);
            let open = doc.is_some_and(|s| s.floor.is_some() && s.ceiling.is_none());
            tables.cells.push(RCell {
                first: first as u16,
                count,
                flags: if open { CELL_OPEN_SKY } else { 0 },
                min_y: if count == 0 { 0 } else { lo as i16 },
                max_y: if count == 0 { 0 } else { hi as i16 },
            });
        }
    }
    tables.material_use = (0..=255u8).filter(|m| used[usize::from(*m)]).collect();
    Ok(tables)
}
