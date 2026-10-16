//! Cook a grid world into the compact render pack of the Tomb Raider-style
//! world renderer.
//!
//! The BSP cook stays the source of collision, PVS and entity placement. This
//! module only replaces what the renderer draws: one quad per authored floor,
//! ceiling and wall band, every corner snapped to the PXBSP lattice (editor
//! units / 16) and deduplicated, so a corner shared by neighbouring tiles is
//! one vertex the runtime transforms once.
//!
//! The faces come from the project's grid document ([`document`], what Play
//! uses) or, for the open-hall bench scripts, from a raw bonnie-32 level file
//! named by `OPEN_HALL_GRID_LEVEL` ([`raw_level`]). Both feed the same pack
//! writer below. [`collision`] derives the thin collision brushes the PXBSP
//! needs from the same document.
//!
//! Lighting is the editor's own brush bake (`brush_light`), run per quad
//! corner against the same point lights, ambient and occluding brushes the BSP
//! cook uses, so the two renderers show the same light.
//!
//! The pack is little-endian. See `psx_bsp::render::grid` for the reader; the
//! two layouts are kept in step by hand.

pub mod collision;
pub mod document;
mod raw_level;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::brush::{Brush, BrushContents, FaceUv, Plane};
use crate::brush_compile::CompiledSurface;
use crate::brush_light::bake_brush_vertex_lighting;
use crate::grid::GridWorld;
use crate::{ProjectDocument, ResourceData, ResourceId};

/// Editor units per PXBSP world unit.
const WORLD_DIVISOR: f64 = 16.0;
/// Cells per macro cell edge (log2).
const MACRO_SHIFT: u32 = 2;
/// Thickness of a floor or ceiling collision slab, editor units. The pack's
/// sky lid sits one slab above the highest face.
pub const SLAB_THICKNESS: f64 = 144.0;

const MAGIC: u32 = 0x3144_5247; // "GRD1"
const VERSION: u32 = 1;
const HEADER_WORDS: usize = 16;
const QUAD_BYTES: usize = 44;

pub const QUAD_TRIANGLE: u8 = 1;
/// The plane is axis aligned: `d` holds the signed plane coordinate and the
/// side test needs no multiplies. Bits 2 and 3 name the axis, bit 4 flags a
/// negative-facing plane.
pub const QUAD_AXIS: u8 = 2;
const QUAD_AXIS_SHIFT: u32 = 2;
const QUAD_AXIS_NEGATIVE: u8 = 0x10;

/// Material name for a texture nothing else can supply: bonnie draws a
/// checkerboard for an empty reference, and `_USER` textures are embedded
/// indexed images the importer does not decode.
pub const FALLBACK_TEXTURE: &str = "__fallback";

/// Material resource name for a bonnie texture reference. Shared by the
/// importer (which registers the material) and the grid cook (which finds
/// it again), so the two cannot drift.
pub fn texture_key(pack: &str, name: &str) -> String {
    if name.is_empty() || pack == "_USER" {
        FALLBACK_TEXTURE.to_string()
    } else {
        name.to_string()
    }
}

/// How a face names its material.
enum FaceMaterial {
    /// A material resource name (the raw bonnie source).
    Name(String),
    /// A material resource id, `None` for an untextured face.
    Id(Option<ResourceId>),
}

/// One authored surface piece before lattice snapping.
struct Face {
    /// Convex ring in editor units, wound so its cross product points to the
    /// visible side.
    ring: Vec<[f64; 3]>,
    material: FaceMaterial,
    /// Global cell column and row.
    cell: (usize, usize),
    /// Index of the room the face belongs to, for its ambient light.
    room: usize,
    /// Texel coordinates per ring corner when the source supplies them; the
    /// raw source leaves them to the paraxial projection.
    texels: Option<Vec<[f64; 2]>>,
}

/// Faces and placement from one source.
struct FaceSet {
    faces: Vec<Face>,
    /// Editor-space corner of the lowest cell.
    origin_editor: (i32, i32),
    /// Cells along X and Z.
    grid: (usize, usize),
    /// Editor units per cell edge.
    cell_editor: i32,
    /// Ambient light per room, indexed by [`Face::room`]; empty falls back
    /// to the input's single ambient.
    room_ambient: Vec<[u8; 3]>,
}

/// Where the cook reads its faces from.
pub enum GridSource<'a> {
    /// The grid document, what Play cooks.
    Document(&'a GridWorld),
    /// A raw decompressed bonnie-32 level file (bench scripts).
    RawLevel(&'a Path),
}

#[derive(Clone, Debug, Default)]
pub struct GridPackStats {
    pub faces: usize,
    pub quads: usize,
    pub triangles: usize,
    pub vertices: usize,
    pub cells: usize,
    pub materials: usize,
    pub dropped_unmapped: usize,
    pub uv_overflow: usize,
    pub bytes: usize,
    pub lights: usize,
    pub lit_corners: usize,
    pub lit_corners_unshadowed: usize,
}

impl GridPackStats {
    pub fn summary(&self) -> String {
        format!(
            "grid pack: {} faces -> {} quads + {} triangles, {} shared vertices, {} cells, {} materials, {} bytes ({} faces unmapped, {} uv spans over 63 texels), {} lights, {} corners lit ({} without shadows)",
            self.faces,
            self.quads,
            self.triangles,
            self.vertices,
            self.cells,
            self.materials,
            self.bytes,
            self.dropped_unmapped,
            self.uv_overflow,
            self.lights,
            self.lit_corners,
            self.lit_corners_unshadowed
        )
    }
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Ring with consecutive duplicate points removed.
fn face_normal(ring: &[[f64; 3]]) -> [f64; 3] {
    let mut normal = [0.0; 3];
    for i in 1..ring.len() - 1 {
        normal = cross(sub(ring[i], ring[0]), sub(ring[i + 1], ring[0]));
        if dot(normal, normal) > 0.0 {
            break;
        }
    }
    let length = dot(normal, normal).sqrt();
    normal.map(|v| v / length)
}

/// Texel coordinates rebased so each axis' minimum lands in one 64 texel
/// repeat, then clamped to 0..=63 so a tile never reaches the far edge of
/// its 64 texel texture. Counts a face whose span is past one repeat.
fn rebase_texels(mut uvs: Vec<[f64; 2]>, overflow: &mut usize) -> Vec<[u8; 2]> {
    let mut spans_over = false;
    for axis in 0..2 {
        let min = uvs.iter().map(|uv| uv[axis]).fold(f64::MAX, f64::min);
        let shift = (min / 64.0).floor() * 64.0;
        for uv in &mut uvs {
            uv[axis] -= shift;
            if uv[axis] > 63.0 {
                if uv[axis] > 64.5 {
                    spans_over = true;
                }
                uv[axis] = 63.0;
            }
            if uv[axis] < 0.0 {
                uv[axis] = 0.0;
            }
        }
    }
    if spans_over {
        *overflow += 1;
    }
    uvs.iter()
        .map(|uv| [uv[0].round() as u8, uv[1].round() as u8])
        .collect()
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// Inputs the cook borrows from the playtest package build.
pub struct GridCookInput<'a> {
    pub project: &'a ProjectDocument,
    pub source: GridSource<'a>,
    /// Ambient light for faces whose room has none of its own.
    pub ambient: [u8; 3],
    pub lights: &'a [crate::brush_light::BrushPointLight],
    pub tints: &'a [crate::brush_light::BrushMaterialTint],
    /// `(texture key, asset id)` of every texture the BSP cook registered.
    pub texture_assets: &'a [(String, u16)],
}

struct QuadRecord {
    v: [u16; 4],
    color: [u32; 4],
    uv: [u16; 4],
    d: i32,
    normal: [i8; 3],
    material: u8,
    flags: u8,
}

/// Cook the grid pack. The bytes are what `GridPack::parse` reads.
pub fn cook_grid_pack(input: &GridCookInput<'_>) -> Result<(Vec<u8>, GridPackStats), String> {
    let FaceSet {
        faces,
        origin_editor,
        grid,
        cell_editor,
        room_ambient,
    } = match &input.source {
        GridSource::Document(world) => document::face_set(world)?,
        GridSource::RawLevel(path) => raw_level::face_set(path)?,
    };
    let scene = input.project.active_scene();
    let occluders: Vec<Brush> = scene
        .brushes
        .iter()
        .filter(|brush| brush.contents.is_solid())
        .cloned()
        .collect();

    // Material resource name and id -> (material resource, asset id).
    let mut material_of: BTreeMap<String, (ResourceId, u16)> = BTreeMap::new();
    let mut material_by_id: HashMap<ResourceId, u16> = HashMap::new();
    for resource in &input.project.resources {
        let ResourceData::Material(material) = &resource.data else {
            continue;
        };
        if material.sky_aperture {
            continue;
        }
        // A material with no image is the cook's flat white texture.
        let path = material.psxt_path.as_deref().unwrap_or("@brush-flat-white");
        let asset = input
            .texture_assets
            .iter()
            .find(|(key, _)| key.split('#').next() == Some(path))
            .map(|(_, asset)| *asset);
        if let Some(asset) = asset {
            material_of.insert(resource.name.clone(), (resource.id, asset));
            material_by_id.insert(resource.id, asset);
        }
    }

    let mut stats = GridPackStats {
        faces: faces.len(),
        ..GridPackStats::default()
    };

    // Keep only faces whose texture the BSP cook registered, so the runtime
    // can bind it.
    let mut kept: Vec<(&Face, ResourceId, u16)> = Vec::new();
    for face in &faces {
        let resolved = match &face.material {
            FaceMaterial::Name(name) => material_of.get(name).copied(),
            FaceMaterial::Id(id) => id.and_then(|id| Some((id, *material_by_id.get(&id)?))),
        };
        match resolved {
            Some((id, asset)) => kept.push((face, id, asset)),
            None => stats.dropped_unmapped += 1,
        }
    }

    // Bake every corner with the editor's brush light bake.
    let surfaces: Vec<CompiledSurface> = kept
        .iter()
        .map(|&(face, material, _)| {
            // The playtest project is already at engine scale: lights, occluders
            // and ambient live in world units, so bake the ring there too.
            let ring_world_f64: Vec<[f64; 3]> = face
                .ring
                .iter()
                .map(|p| p.map(|v| v / WORLD_DIVISOR))
                .collect();
            let points: Vec<[i32; 3]> = ring_world_f64
                .iter()
                .map(|p| p.map(|v| v.round() as i32))
                .collect();
            // First non-collinear triple gives the plane; the ring is wound
            // so its cross product points at the visible side.
            let plane = (1..points.len() - 1)
                .find_map(|i| Plane::from_points([points[0], points[i], points[i + 1]]))
                .expect("oriented ring has a plane");
            CompiledSurface {
                plane,
                vertices: ring_world_f64,
                material: Some(material),
                uv: FaceUv::default(),
                edge_repair: None,
                contents: BrushContents::Solid,
                source_brush: 0,
                source_face: 0,
            }
        })
        .collect();
    // Bake per ambient value: each room has its own, the bake takes one.
    let ambient_of = |face: &Face| {
        room_ambient
            .get(face.room)
            .copied()
            .unwrap_or(input.ambient)
    };
    let mut by_ambient: BTreeMap<[u8; 3], Vec<usize>> = BTreeMap::new();
    for (index, &(face, _, _)) in kept.iter().enumerate() {
        by_ambient.entry(ambient_of(face)).or_default().push(index);
    }
    let mut baked: Vec<Vec<u32>> = vec![Vec::new(); kept.len()];
    stats.lights = input.lights.len();
    for (ambient, members) in &by_ambient {
        let group: Vec<CompiledSurface> = members.iter().map(|&i| surfaces[i].clone()).collect();
        let lit =
            bake_brush_vertex_lighting(&group, &occluders, *ambient, input.lights, input.tints)
                .map_err(|error| format!("grid light bake: {error:?}"))?;
        let open = bake_brush_vertex_lighting(&group, &[], *ambient, input.lights, input.tints)
            .map_err(|error| format!("grid light bake: {error:?}"))?;
        let flat =
            u32::from(ambient[0]) | (u32::from(ambient[1]) << 8) | (u32::from(ambient[2]) << 16);
        let count = |colors: &[Vec<u32>]| {
            colors
                .iter()
                .flatten()
                .filter(|c| (**c & 0xff_ffff) != flat)
                .count()
        };
        stats.lit_corners += count(&lit);
        stats.lit_corners_unshadowed += count(&open);
        for (&index, colors) in members.iter().zip(lit) {
            baked[index] = colors;
        }
    }

    // Dense material numbering in first-seen order.
    let mut material_slots: Vec<u16> = Vec::new();
    let mut slot_of: HashMap<u16, u8> = HashMap::new();

    let world = |p: &[f64; 3]| -> [i16; 3] {
        p.map(|v| (v / WORLD_DIVISOR).round().clamp(-32768.0, 32767.0) as i16)
    };
    let origin_world = [
        (f64::from(origin_editor.0) / WORLD_DIVISOR).round() as i32,
        (f64::from(origin_editor.1) / WORLD_DIVISOR).round() as i32,
    ];
    let cell_world = (f64::from(cell_editor) / WORLD_DIVISOR).round() as i32;
    let (grid_w, grid_d) = grid;
    let cell_count = grid_w * grid_d;

    // Quads grouped by cell, in cell order.
    let mut by_cell: Vec<Vec<usize>> = vec![Vec::new(); cell_count];
    for (index, (face, _, _)) in kept.iter().enumerate() {
        by_cell[face.cell.1 * grid_w + face.cell.0].push(index);
    }

    let mut vertex_of: HashMap<[i16; 3], u16> = HashMap::new();
    let mut vertices: Vec<[i16; 3]> = Vec::new();
    let mut quads: Vec<QuadRecord> = Vec::new();
    let mut cells: Vec<(u16, u16, i16, i16)> = Vec::with_capacity(cell_count);
    let mut max_y_editor = f64::MIN;

    for cell_faces in &by_cell {
        let first = quads.len();
        let mut min_y = i16::MAX;
        let mut max_y = i16::MIN;
        for &index in cell_faces {
            let (face, _, asset) = kept[index];
            let ring_world: Vec<[i16; 3]> = face.ring.iter().map(world).collect();
            // Distinct snapped corners; a face that collapses is dropped.
            let mut distinct: Vec<[i16; 3]> = Vec::new();
            let mut keep: Vec<usize> = Vec::new();
            for (i, p) in ring_world.iter().enumerate() {
                if distinct.last() != Some(p) && distinct.first() != Some(p) {
                    distinct.push(*p);
                    keep.push(i);
                }
            }
            if distinct.len() < 3 {
                continue;
            }
            // Plane in world units from the snapped corners.
            let wf = |p: [i16; 3]| p.map(f64::from);
            let mut normal = [0.0; 3];
            for i in 1..distinct.len() - 1 {
                normal = cross(
                    sub(wf(distinct[i]), wf(distinct[0])),
                    sub(wf(distinct[i + 1]), wf(distinct[0])),
                );
                if dot(normal, normal) > 0.0 {
                    break;
                }
            }
            if dot(normal, normal) == 0.0 {
                continue;
            }
            // Keep the visible side the unsnapped ring chose.
            let want = face_normal(&face.ring);
            if dot(normal, want) < 0.0 {
                distinct.reverse();
                keep.reverse();
                normal = normal.map(|v| -v);
            }
            let length = dot(normal, normal).sqrt();
            let n8 = normal.map(|v| ((v / length) * 127.0).round().clamp(-127.0, 127.0) as i8);
            let p0 = distinct[0];
            let d = i32::from(n8[0]) * i32::from(p0[0])
                + i32::from(n8[1]) * i32::from(p0[1])
                + i32::from(n8[2]) * i32::from(p0[2]);
            // Exactly axis-aligned planes (floors, ceilings, straight walls) get a
            // multiply-free side test: the camera coordinate against the plane
            // coordinate.
            let axis_flags = {
                let zero = normal.map(|v| v == 0.0);
                match zero {
                    [true, false, true] | [false, true, true] | [true, true, false] => {
                        let axis = normal.iter().position(|v| *v != 0.0).unwrap();
                        let negative = normal[axis] < 0.0;
                        QUAD_AXIS
                            | ((axis as u8) << QUAD_AXIS_SHIFT)
                            | if negative { QUAD_AXIS_NEGATIVE } else { 0 }
                    }
                    _ => 0,
                }
            };
            let d = if axis_flags != 0 {
                let axis = usize::from((axis_flags >> QUAD_AXIS_SHIFT) & 3);
                let coordinate = i32::from(p0[axis]);
                if axis_flags & QUAD_AXIS_NEGATIVE != 0 {
                    -coordinate
                } else {
                    coordinate
                }
            } else {
                d
            };

            let uvs_all = match &face.texels {
                Some(texels) => rebase_texels(texels.clone(), &mut stats.uv_overflow),
                None => raw_level::face_uv(&face.ring, &mut stats.uv_overflow),
            };
            let slot = {
                *slot_of.entry(asset).or_insert_with(|| {
                    material_slots.push(asset);
                    (material_slots.len() - 1) as u8
                })
            };
            let colors = &baked[index];
            let count = distinct.len().min(4);
            if distinct.len() > 4 {
                return Err("grid face with more than four corners".to_string());
            }
            let mut record = QuadRecord {
                v: [0; 4],
                color: [0; 4],
                uv: [0; 4],
                d,
                normal: n8,
                material: slot,
                flags: axis_flags | if count == 3 { QUAD_TRIANGLE } else { 0 },
            };
            for k in 0..count {
                let p = distinct[k];
                let id = *vertex_of.entry(p).or_insert_with(|| {
                    vertices.push(p);
                    (vertices.len() - 1) as u16
                });
                let source = keep[k];
                record.v[k] = id;
                record.color[k] = colors[source] & 0x00ff_ffff;
                let uv = uvs_all[source];
                record.uv[k] = u16::from(uv[0]) | (u16::from(uv[1]) << 8);
                min_y = min_y.min(p[1]);
                max_y = max_y.max(p[1]);
            }
            if count == 3 {
                record.v[3] = record.v[2];
                record.color[3] = record.color[2];
                record.uv[3] = record.uv[2];
            }
            for p in &face.ring {
                max_y_editor = max_y_editor.max(p[1]);
            }
            if count == 3 {
                stats.triangles += 1;
            } else {
                stats.quads += 1;
            }
            quads.push(record);
        }
        let count = quads.len() - first;
        if first > usize::from(u16::MAX) || count > usize::from(u16::MAX) {
            return Err("grid pack has more than 65535 quads".to_string());
        }
        cells.push((first as u16, count as u16, min_y, max_y));
    }
    if vertices.len() > usize::from(u16::MAX) {
        return Err("grid pack has more than 65535 vertices".to_string());
    }
    if material_slots.len() > 255 {
        return Err("grid pack has more than 255 materials".to_string());
    }

    // Macro cells: cells grouped 4 x 4.
    let macro_w = grid_w.div_ceil(1 << MACRO_SHIFT);
    let macro_d = grid_d.div_ceil(1 << MACRO_SHIFT);
    let mut macros: Vec<(i16, i16, u16)> = Vec::with_capacity(macro_w * macro_d);
    // A column is open to the sky when its cell has a floor but no ceiling.
    let has_ceiling = |cx: usize, cz: usize| {
        by_cell[cz * grid_w + cx]
            .iter()
            .any(|&index| face_normal(&kept[index].0.ring)[1] < -0.5)
    };
    let has_floor = |cx: usize, cz: usize| {
        by_cell[cz * grid_w + cx]
            .iter()
            .any(|&index| face_normal(&kept[index].0.ring)[1] > 0.5)
    };
    for mz in 0..macro_d {
        for mx in 0..macro_w {
            let mut min_y = i16::MAX;
            let mut max_y = i16::MIN;
            let mut open = false;
            for dz in 0..(1usize << MACRO_SHIFT) {
                for dx in 0..(1usize << MACRO_SHIFT) {
                    let (cx, cz) = ((mx << MACRO_SHIFT) + dx, (mz << MACRO_SHIFT) + dz);
                    if cx >= grid_w || cz >= grid_d {
                        continue;
                    }
                    let (_, count, lo, hi) = cells[cz * grid_w + cx];
                    if count > 0 {
                        min_y = min_y.min(lo);
                        max_y = max_y.max(hi);
                        open |= has_floor(cx, cz) && !has_ceiling(cx, cz);
                    }
                }
            }
            macros.push((min_y, max_y, u16::from(open)));
        }
    }

    stats.vertices = vertices.len();
    stats.cells = cell_count;
    stats.materials = material_slots.len();

    let lid_y = ((max_y_editor + SLAB_THICKNESS) / WORLD_DIVISOR).round() as i32;
    let mut out = Vec::new();
    let header = [
        MAGIC,
        VERSION,
        (grid_w as u32) | ((grid_d as u32) << 16),
        origin_world[0] as u32,
        origin_world[1] as u32,
        (cell_world as u32 & 0xffff) | ((lid_y as u32 & 0xffff) << 16),
        vertices.len() as u32,
        quads.len() as u32,
        cell_count as u32,
        macros.len() as u32,
        material_slots.len() as u32,
        (macro_w as u32) | ((macro_d as u32) << 16),
        MACRO_SHIFT,
        0,
        0,
        0,
    ];
    debug_assert_eq!(header.len(), HEADER_WORDS);
    for word in header {
        push_u32(&mut out, word);
    }
    for asset in &material_slots {
        push_u16(&mut out, *asset);
    }
    pad4(&mut out);
    for p in &vertices {
        push_i16(&mut out, p[0]);
        push_i16(&mut out, p[1]);
        push_i16(&mut out, p[2]);
        push_i16(&mut out, 0);
    }
    for (first, count, lo, hi) in &cells {
        push_u16(&mut out, *first);
        push_u16(&mut out, *count);
        push_i16(&mut out, *lo);
        push_i16(&mut out, *hi);
    }
    for (lo, hi, flags) in &macros {
        push_i16(&mut out, *lo);
        push_i16(&mut out, *hi);
        push_u16(&mut out, *flags);
        push_u16(&mut out, 0);
    }
    for quad in &quads {
        let before = out.len();
        for v in quad.v {
            push_u16(&mut out, v);
        }
        for color in quad.color {
            push_u32(&mut out, color);
        }
        for uv in quad.uv {
            push_u16(&mut out, uv);
        }
        push_u32(&mut out, quad.d as u32);
        for n in quad.normal {
            out.push(n as u8);
        }
        out.push(quad.material);
        out.push(quad.flags);
        out.extend_from_slice(&[0, 0, 0]);
        debug_assert_eq!(out.len() - before, QUAD_BYTES);
    }
    stats.bytes = out.len();
    Ok((out, stats))
}
