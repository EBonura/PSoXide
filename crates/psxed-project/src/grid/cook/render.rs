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
    PlaneRef, Quad, RCell, Vtx, CELL_OPEN_SKY, QUAD2_REPEAT, QUAD_AXIS, QUAD_AXIS_NEG,
    QUAD_AXIS_SHIFT, QUAD_DOUBLE, QUAD_PLANE, QUAD_TRI,
};

use super::units::RuntimeUnits;
use crate::face_uv::UvMode;
use crate::grid::faces::{cell_faces_with, GridFace, UvDefaults};
use crate::grid::lighting::BakeRig;
use crate::grid::mesh_faces::{mesh_faces_with, placed as placed_mesh};
use crate::grid::plane_faces::plane_faces_with;
use crate::grid::uv_split;
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
    /// Texture width and height in texels. Powers of two from 8 to 128 can
    /// repeat through the GPU texture window, see [`Self::tex_shape`].
    pub texels: [u16; 2],
}

impl BoundMaterial {
    /// `Material::tex_shape` of this material: 0 for an untextured stand-in
    /// or a size the texture window cannot repeat.
    pub fn tex_shape(&self) -> u16 {
        if self.textured {
            psx_grid::tex_shape(self.texels[0], self.texels[1])
        } else {
            0
        }
    }

    /// Whether a primitive of this material may carry the `REPEAT` flag.
    pub fn can_repeat(&self) -> bool {
        self.tex_shape() != 0
    }
}

/// Binds document materials to level-global material slots.
pub trait MaterialBinder {
    /// The binding of `material`, or why it cannot be bound.
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String>;

    /// How much of the light that lands on `material` it sends back, per
    /// channel, for the bounce of a world that gathers its light: the
    /// average colour of its texture times its tint. Binders with no texture
    /// bytes use the mid-grey default.
    fn albedo(&mut self, _material: Option<ResourceId>) -> [f32; 3] {
        crate::grid::lighting::gather::FALLBACK_ALBEDO
    }

    /// The grid tiling default of `material`, for faces that do not name a
    /// mode. Binders without a project have none.
    fn uv_default(&self, _material: ResourceId) -> Option<UvMode> {
        None
    }

    /// Whether `material` is see-through (translucent or alpha-cut), which
    /// decides whether the camera passes through a plane wearing it.
    fn see_through(&self, _material: Option<ResourceId>) -> bool {
        false
    }
}

/// A binder's tiling defaults, as the face builder wants them.
struct BinderDefaults<'a>(&'a dyn MaterialBinder);

impl UvDefaults for BinderDefaults<'_> {
    fn default_uv(&self, material: ResourceId) -> Option<UvMode> {
        self.0.uv_default(material)
    }
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
    /// Primitives added by cutting faces at texture tile boundaries.
    pub uv_split_quads: usize,
    pub lit_corners: usize,
    /// Primitives that repeat their texture through the texture window.
    pub repeat_quads: usize,
}

/// Render tables of one room.
#[derive(Default)]
pub struct Tables {
    pub vertices: Vec<Vtx>,
    pub quads: Vec<Quad>,
    pub cells: Vec<RCell>,
    pub material_use: Vec<u8>,
    /// One reference per free plane that produced quads.
    pub plane_refs: Vec<PlaneRef>,
    /// Continuous texel coordinates per quad corner before they were rebased
    /// into one repeat, parallel to `quads`. The LOD merge needs them.
    pub texels: Vec<[[f64; 2]; 4]>,
    pub stats: RenderStats,
}

/// Inputs shared by every room of one cook.
pub struct Context<'a> {
    pub world: &'a GridWorld,
    pub units: RuntimeUnits,
    /// Bakes the corner colours: the point lights, the facing shade and the
    /// shadow query, shared with the editor preview.
    pub rig: BakeRig<'a>,
    pub binder: &'a mut dyn MaterialBinder,
    pub materials: &'a mut MaterialSet,
    /// Whether primitives may repeat their texture through the GPU texture
    /// window (up to four tiles per axis). Off, every primitive holds one
    /// tile, which is how format version 2 levels were cooked.
    pub repeat: bool,
}

/// Packed UVs of a primitive and how it addresses its texture.
struct Rebased {
    uv: Vec<[u8; 2]>,
    /// The texels pass what the primitive can hold.
    overflow: bool,
    /// The UVs are raw and the primitive needs the texture window: some axis
    /// holds more than one tile.
    repeat: bool,
}

/// Texel coordinates rebased so each axis' minimum lands in the first tile,
/// then clamped so the last tile an axis touches never reaches the first
/// texel of the next one. With `repeat` an axis may touch up to four tiles
/// (`REPEAT` primitive, raw UVs); otherwise one, as before.
fn rebase_texels(texels: &[[f64; 2]], repeat: bool) -> Rebased {
    let limit = uv_split::tiles_per_primitive(repeat);
    let mut uvs = texels.to_vec();
    let mut overflow = false;
    let mut tiles = [1i64; 2];
    for axis in 0..2 {
        let min = uvs.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
        let max = uvs.iter().map(|uv| uv[axis]).fold(f64::MIN, f64::max);
        let touched = uv_split::tiles_touched(min, max);
        if touched > limit {
            overflow = true;
        }
        tiles[axis] = touched.min(limit);
        let top = tiles[axis] as f64 * TILE_TEXELS - 1.0;
        let shift = (min / TILE_TEXELS).floor() * TILE_TEXELS;
        for uv in &mut uvs {
            uv[axis] = (uv[axis] - shift).clamp(0.0, top);
        }
    }
    Rebased {
        uv: uvs
            .iter()
            .map(|uv| [uv[0].round() as u8, uv[1].round() as u8])
            .collect(),
        overflow,
        repeat: tiles.iter().any(|&t| t > 1),
    }
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

/// Per-room working state of [`build`]: the tables, the vertex weld map and
/// which materials the room draws.
struct RoomBuild {
    tables: Tables,
    vertex_of: HashMap<[i16; 3], u16>,
    used: [bool; 256],
    /// Lowest and highest vertex height of the face run being built.
    range: (i32, i32),
}

impl RoomBuild {
    fn new() -> Self {
        Self {
            tables: Tables::default(),
            vertex_of: HashMap::new(),
            used: [false; 256],
            range: (i32::MAX, i32::MIN),
        }
    }

    /// Weld `key` into the vertex table.
    fn vertex(&mut self, key: [i16; 3], room_index: usize) -> Result<u16, String> {
        let next = self.tables.vertices.len();
        let id = *self.vertex_of.entry(key).or_insert_with(|| {
            self.tables.vertices.push(Vtx {
                x: key[0],
                y: key[1],
                z: key[2],
                pad: 0,
            });
            next as u16
        });
        if self.tables.vertices.len() > usize::from(u16::MAX) {
            return Err(format!("room {room_index} has more than 65535 vertices"));
        }
        Ok(id)
    }

    /// Cook one face, cut at the texture tile boundaries first when its
    /// texels run across more than one tile. `place` names the face for
    /// errors and `extra_flags` is OR-ed into the quad flags (the plane
    /// marker).
    #[allow(clippy::too_many_arguments)]
    fn add_face(
        &mut self,
        cx: &mut Context<'_>,
        face: &GridFace,
        room_index: usize,
        origin: [i32; 3],
        ambient: [u8; 3],
        place: &dyn Fn() -> String,
        extra_flags: u8,
    ) -> Result<(), String> {
        // A face inside one tile is the common case and needs no material
        // lookup to say so.
        if uv_split::fits(&face.texels, 1) {
            return self.add_piece(
                cx,
                face,
                face,
                room_index,
                origin,
                ambient,
                place,
                extra_flags,
            );
        }
        let slot = cx.materials.slot(cx.binder, face.material)?;
        let bound = cx.materials.slots[usize::from(slot)].1;
        if !bound.textured {
            return self.add_piece(
                cx,
                face,
                face,
                room_index,
                origin,
                ambient,
                place,
                extra_flags,
            );
        }
        let pieces = uv_split::split(face, cx.world.scale, cx.repeat && bound.can_repeat());
        self.tables.stats.uv_split_quads += pieces.len() - 1;
        // The face counts once, however many primitives it became.
        let faces_before = self.tables.stats.faces;
        for piece in &pieces {
            self.add_piece(
                cx,
                piece,
                face,
                room_index,
                origin,
                ambient,
                place,
                extra_flags,
            )?;
        }
        self.tables.stats.faces = faces_before + 1;
        Ok(())
    }

    /// Cook one face that fits one tile into the tables.
    #[allow(clippy::too_many_arguments)]
    fn add_piece(
        &mut self,
        cx: &mut Context<'_>,
        face: &GridFace,
        parent: &GridFace,
        room_index: usize,
        origin: [i32; 3],
        ambient: [u8; 3],
        place: &dyn Fn() -> String,
        extra_flags: u8,
    ) -> Result<(), String> {
        self.tables.stats.faces += 1;
        // Snap the corners.
        let mut local: Vec<[i32; 3]> = Vec::with_capacity(face.grid.len());
        for g in &face.grid {
            local.push([
                cx.units.rt(g[0]) - origin[0],
                cx.units.rt(g[1]) - origin[1],
                cx.units.rt(g[2]) - origin[2],
            ]);
        }
        // The pack wants the ring's cross product to point at the visible
        // side, the face ring reversed.
        let ring: Vec<usize> = (0..local.len()).rev().collect();
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
            self.tables.stats.collapsed += 1;
            return Ok(());
        }
        let mut snapped: Vec<[i32; 3]> = keep.iter().map(|&i| local[i]).collect();
        let Some(mut normal) = ring_cross(&snapped) else {
            self.tables.stats.collapsed += 1;
            return Ok(());
        };
        // Keep the visible side the unsnapped ring chose.
        if dot_f(face.normal, normal) < 0.0 {
            keep.reverse();
            snapped.reverse();
            normal = normal.map(|v| -v);
        }
        if keep.len() > 4 {
            return Err(format!(
                "room {room_index} {}: a face with more than four corners",
                place()
            ));
        }
        let slot = cx.materials.slot(cx.binder, face.material)?;
        let (_, bound) = cx.materials.slots[usize::from(slot)];
        self.used[usize::from(slot)] = true;
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
        let dot_n = i32::from(n8[0]) * p0[0] + i32::from(n8[1]) * p0[1] + i32::from(n8[2]) * p0[2];
        let mut flags = extra_flags;
        let mut d = i16::try_from(dot_n >> 7)
            .map_err(|_| format!("room {room_index} {}: plane constant out of range", place()))?;
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
        let rebased = if bound.textured {
            rebase_texels(&texels, cx.repeat && bound.can_repeat())
        } else {
            Rebased {
                uv: vec![[0, 0]; texels.len()],
                overflow: false,
                repeat: false,
            }
        };
        let packed_uv = &rebased.uv;
        if rebased.overflow {
            self.tables.stats.uv_overflow += 1;
        }
        if rebased.repeat {
            self.tables.stats.repeat_quads += 1;
        }
        let mut quad = Quad {
            d,
            n: n8,
            material: slot,
            flags,
            flags2: if rebased.repeat { QUAD2_REPEAT } else { 0 },
            ..Quad::default()
        };
        for (k, &i) in keep.iter().enumerate() {
            let p = local[i];
            let key = [
                i16::try_from(p[0]).map_err(|_| format!("room {room_index}: x out of range"))?,
                i16::try_from(p[1]).map_err(|_| format!("room {room_index}: y out of range"))?,
                i16::try_from(p[2]).map_err(|_| format!("room {room_index}: z out of range"))?,
            ];
            quad.v[k] = self.vertex(key, room_index)?;
            let colour = cx.rig.corner(face, i, Some(parent), bound.tint, ambient);
            quad.color[k] =
                u32::from(colour[0]) | (u32::from(colour[1]) << 8) | (u32::from(colour[2]) << 16);
            quad.uv[k] = u16::from(packed_uv[k][0]) | (u16::from(packed_uv[k][1]) << 8);
            self.range = (self.range.0.min(p[1]), self.range.1.max(p[1]));
        }
        self.tables.stats.lit_corners += keep.len();
        if keep.len() == 3 {
            quad.v[3] = quad.v[2];
            quad.color[3] = quad.color[2];
            quad.uv[3] = quad.uv[2];
            self.tables.stats.triangles += 1;
        } else {
            self.tables.stats.quads += 1;
        }
        let mut kept_texels = [[0.0; 2]; 4];
        if bound.textured {
            for (k, t) in texels.iter().enumerate() {
                kept_texels[k] = *t;
            }
            if keep.len() == 3 {
                kept_texels[3] = kept_texels[2];
            }
        }
        self.tables.texels.push(kept_texels);
        self.tables.quads.push(quad);
        Ok(())
    }
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
    let mut build = RoomBuild::new();
    let mut faces: Vec<GridFace> = Vec::new();
    for z in 0..room.depth {
        for x in 0..room.width {
            faces.clear();
            cell_faces_with(
                cx.world,
                &BinderDefaults(&*cx.binder),
                room_index,
                x,
                z,
                &mut faces,
            );
            let first = build.tables.quads.len();
            build.range = (i32::MAX, i32::MIN);
            for face in &faces {
                let place = || format!("sector {x},{z}");
                build.add_face(cx, face, room_index, origin, ambient, &place, 0)?;
            }
            let count = build.tables.quads.len() - first;
            if first > usize::from(u16::MAX) {
                return Err(format!("room {room_index} has more than 65535 quads"));
            }
            let count = u8::try_from(count).map_err(|_| {
                format!("room {room_index} sector {x},{z}: {count} quads in one cell, at most 255")
            })?;
            let (lo, hi) = build.range;
            let open = room.cell_is_open(x, z);
            build.tables.cells.push(RCell {
                first: first as u16,
                count,
                flags: if open { CELL_OPEN_SKY } else { 0 },
                min_y: if count == 0 { 0 } else { lo as i16 },
                max_y: if count == 0 { 0 } else { hi as i16 },
            });
        }
    }
    // Free planes follow the cell quads; each is a run of quads with a box
    // for the renderer's per-frame frustum test.
    for index in 0..room.planes.len() {
        faces.clear();
        plane_faces_with(
            cx.world,
            &BinderDefaults(&*cx.binder),
            room_index,
            index,
            &mut faces,
        );
        if faces.is_empty() {
            continue;
        }
        let first = build.tables.quads.len();
        build.range = (i32::MAX, i32::MIN);
        for face in &faces {
            let place = || format!("plane {index}");
            build.add_face(cx, face, room_index, origin, ambient, &place, QUAD_PLANE)?;
        }
        let count = build.tables.quads.len() - first;
        if count == 0 {
            continue;
        }
        if build.tables.quads.len() > usize::from(u16::MAX) {
            return Err(format!("room {room_index} has more than 65535 quads"));
        }
        let count = u8::try_from(count)
            .map_err(|_| format!("room {room_index} plane {index}: {count} quads, at most 255"))?;
        let reference = plane_ref(cx, room, index, origin, first as u16, count, build.range)?;
        build.tables.plane_refs.push(reference);
    }
    // Static meshes follow the planes: an instance is one more run of quads
    // with a box for the renderer's frustum test, drawn through the same
    // plane list, so the runtime needs nothing of its own for them.
    for index in 0..room.meshes.len() {
        faces.clear();
        let made = mesh_faces_with(
            cx.world,
            &BinderDefaults(&*cx.binder),
            room_index,
            index,
            &mut faces,
        );
        if made == 0 {
            continue;
        }
        let id = room.meshes[index].id;
        let first = build.tables.quads.len();
        build.range = (i32::MAX, i32::MIN);
        for face in &faces {
            let place = || format!("mesh {id}");
            build.add_face(cx, face, room_index, origin, ambient, &place, QUAD_PLANE)?;
        }
        let count = build.tables.quads.len() - first;
        if count == 0 {
            continue;
        }
        if build.tables.quads.len() > usize::from(u16::MAX) {
            return Err(format!("room {room_index} has more than 65535 quads"));
        }
        let count = u8::try_from(count)
            .map_err(|_| format!("room {room_index} mesh {id}: {count} quads, at most 255"))?;
        let span = placed_mesh(cx.world, room_index, index)
            .map_or(([0; 2], [0; 2]), |p| p.mesh.cell_span(&p.built));
        build
            .tables
            .plane_refs
            .push(run_ref(room, span, first as u16, count, build.range));
    }
    build.tables.material_use = (0..=255u8)
        .filter(|m| build.used[usize::from(*m)])
        .collect();
    Ok(build.tables)
}

/// The render reference of plane `index`: its quad run and the room-local
/// cell rectangle and height range the frustum test uses.
fn plane_ref(
    cx: &Context<'_>,
    room: &crate::grid::room::GridRoom,
    index: usize,
    origin: [i32; 3],
    first: u16,
    count: u8,
    range: (i32, i32),
) -> Result<PlaneRef, String> {
    let _ = (cx, origin);
    Ok(run_ref(
        room,
        room.planes[index].cell_span(),
        first,
        count,
        range,
    ))
}

/// The render reference of a run of `count` quads from `first` whose box
/// covers the world sectors `(cells_lo, cells_hi)` (clamped to the room) and
/// the local heights `(lo, hi)`.
fn run_ref(
    room: &crate::grid::room::GridRoom,
    (cells_lo, cells_hi): ([i32; 2], [i32; 2]),
    first: u16,
    count: u8,
    (lo, hi): (i32, i32),
) -> PlaneRef {
    let local = |world: i32, axis: usize, limit: usize| -> u8 {
        let c = (world - room.origin[axis]).clamp(0, limit as i32 - 1);
        c as u8
    };
    PlaneRef {
        quad: first,
        count,
        flags: 0,
        x0: local(cells_lo[0], 0, room.width),
        z0: local(cells_lo[1], 1, room.depth),
        x1: local(cells_hi[0], 0, room.width),
        z1: local(cells_hi[1], 1, room.depth),
        min_y: lo as i16,
        max_y: hi as i16,
    }
}
