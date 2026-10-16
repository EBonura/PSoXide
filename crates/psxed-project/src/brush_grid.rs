//! Open-hall experiment: cook a bonnie-32 sector-grid level into a compact
//! grid render pack for the Tomb Raider-style world renderer.
//!
//! The BSP cook stays the source of collision, PVS and entity placement. This
//! module only replaces what the renderer draws: it reads the same bonnie-32
//! level file the importer read, emits one quad per authored floor, ceiling
//! and sector-tall wall band, snaps every corner to the PXBSP lattice (editor
//! units / 16) and deduplicates corners, so a corner shared by neighbouring
//! tiles is one vertex the runtime transforms once.
//!
//! Lighting is the editor's own brush bake (`brush_light`), run per quad
//! corner against the same point lights, ambient and occluding brushes the BSP
//! cook uses, so the two renderers show the same light.
//!
//! The pack is little-endian. See `psx_bsp::render::grid` for the reader; the
//! two layouts are kept in step by hand.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::brush::{Brush, BrushContents, FaceUv, Plane};
use crate::brush_compile::CompiledSurface;
use crate::brush_light::bake_brush_vertex_lighting;
use crate::{ProjectDocument, ResourceData, ResourceId};

/// Editor units per bonnie sector after the importer's 9/16 scale.
const CELL_EDITOR: i32 = 576;
const SCALE_NUM: i32 = 9;
const SCALE_DEN: i32 = 16;
/// Editor units per PXBSP world unit.
const WORLD_DIVISOR: f64 = 16.0;
/// Editor units per texel (the importer's UV scale: 32 texels per sector).
const TEXEL_EDITOR: f64 = 18.0;
/// Cells per macro cell edge (log2).
const MACRO_SHIFT: u32 = 2;
/// Thickness the importer gives a ceiling slab, so the sky lid sits above it.
const SLAB_THICKNESS: f64 = 144.0;

const MAGIC: u32 = 0x3144_5247; // "GRD1"
const VERSION: u32 = 1;
const HEADER_WORDS: usize = 16;
const QUAD_BYTES: usize = 44;

pub const QUAD_TRIANGLE: u8 = 1;

#[derive(Deserialize, Clone, Copy)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Deserialize)]
struct TexRef {
    name: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Default)]
enum NormalMode {
    #[default]
    Front,
    Both,
    Back,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Default)]
enum SplitDirection {
    #[default]
    NwSe,
    NeSw,
}

#[derive(Deserialize)]
struct HFace {
    heights: [f32; 4],
    texture: TexRef,
    #[serde(default)]
    split_direction: SplitDirection,
    #[serde(default)]
    heights_2: Option<[f32; 4]>,
    #[serde(default)]
    texture_2: Option<TexRef>,
}

#[derive(Deserialize)]
struct VFace {
    heights: [f32; 4],
    texture: TexRef,
    #[serde(default)]
    normal_mode: NormalMode,
}

#[derive(Deserialize, Default)]
struct Sector {
    floor: Option<HFace>,
    ceiling: Option<HFace>,
    #[serde(default)]
    walls_north: Vec<VFace>,
    #[serde(default)]
    walls_east: Vec<VFace>,
    #[serde(default)]
    walls_south: Vec<VFace>,
    #[serde(default)]
    walls_west: Vec<VFace>,
    #[serde(default)]
    walls_nwse: Vec<VFace>,
    #[serde(default)]
    walls_nesw: Vec<VFace>,
}

#[derive(Deserialize)]
struct Room {
    position: V3,
    width: usize,
    depth: usize,
    sectors: Vec<Vec<Option<Sector>>>,
}

#[derive(Deserialize)]
struct Level {
    rooms: Vec<Room>,
}

/// One authored surface piece before lattice snapping.
struct Face {
    /// Convex ring in editor units, wound so its cross product points to the
    /// visible side.
    ring: Vec<[f64; 3]>,
    texture: String,
    /// Global cell column and row.
    cell: (usize, usize),
}

/// What the grid cook produced, for the cook report.
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
}

impl GridPackStats {
    pub fn summary(&self) -> String {
        format!(
            "grid pack: {} faces -> {} quads + {} triangles, {} shared vertices, {} cells, {} materials, {} bytes ({} faces unmapped, {} uv spans over 63 texels)",
            self.faces,
            self.quads,
            self.triangles,
            self.vertices,
            self.cells,
            self.materials,
            self.bytes,
            self.dropped_unmapped,
            self.uv_overflow
        )
    }
}

fn scale(value: f32) -> i32 {
    let scaled = value.round() as i64 * i64::from(SCALE_NUM);
    assert!(
        scaled % i64::from(SCALE_DEN) == 0,
        "bonnie coordinate {value} is off the 16-unit grid"
    );
    (scaled / i64::from(SCALE_DEN)) as i32
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
fn dedupe(points: &[[i32; 3]]) -> Vec<[i32; 3]> {
    let mut ring: Vec<[i32; 3]> = Vec::with_capacity(points.len());
    for point in points {
        if ring.last() != Some(point) && ring.first() != Some(point) {
            ring.push(*point);
        }
    }
    ring
}

/// Make `ring` face `toward` (cross product of its first non-collinear edge
/// pair has a positive dot with it). `None` when the ring is degenerate.
fn orient(mut ring: Vec<[i32; 3]>, toward: [f64; 3]) -> Option<Vec<[f64; 3]>> {
    if ring.len() < 3 {
        return None;
    }
    let p = |i: usize| ring[i].map(f64::from);
    let mut normal = [0.0; 3];
    for i in 1..ring.len() - 1 {
        normal = cross(sub(p(i), p(0)), sub(p(i + 1), p(0)));
        if dot(normal, normal) > 0.0 {
            break;
        }
    }
    if dot(normal, normal) == 0.0 {
        return None;
    }
    if dot(normal, toward) < 0.0 {
        ring.reverse();
    }
    Some(ring.iter().map(|p| p.map(f64::from)).collect())
}

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

fn push_face(
    faces: &mut Vec<Face>,
    points: &[[i32; 3]],
    toward: [f64; 3],
    texture: &str,
    cell: (usize, usize),
) {
    let ring = dedupe(points);
    if let Some(ring) = orient(ring, toward) {
        faces.push(Face {
            ring,
            texture: texture.to_string(),
            cell,
        });
    }
}

fn collect_faces(level: &Level) -> (Vec<Face>, (i32, i32), (usize, usize)) {
    // Global cell origin: the lowest room corner, so rooms placed apart share
    // one lattice.
    let mut min_x = i32::MAX;
    let mut min_z = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_z = i32::MIN;
    for room in &level.rooms {
        let ox = scale(room.position.x);
        let oz = scale(room.position.z);
        min_x = min_x.min(ox);
        min_z = min_z.min(oz);
        max_x = max_x.max(ox + room.width as i32 * CELL_EDITOR);
        max_z = max_z.max(oz + room.depth as i32 * CELL_EDITOR);
    }
    let grid = (
        ((max_x - min_x) / CELL_EDITOR) as usize,
        ((max_z - min_z) / CELL_EDITOR) as usize,
    );
    let mut faces = Vec::new();
    let s = std::f64::consts::FRAC_1_SQRT_2;
    for room in &level.rooms {
        let ox = scale(room.position.x);
        let oy = scale(room.position.y);
        let oz = scale(room.position.z);
        for gx in 0..room.width {
            for gz in 0..room.depth {
                let Some(Some(sector)) = room.sectors.get(gx).and_then(|c| c.get(gz)) else {
                    continue;
                };
                let x0 = ox + gx as i32 * CELL_EDITOR;
                let z0 = oz + gz as i32 * CELL_EDITOR;
                let (x1, z1) = (x0 + CELL_EDITOR, z0 + CELL_EDITOR);
                let cell = (
                    ((x0 - min_x) / CELL_EDITOR) as usize,
                    ((z0 - min_z) / CELL_EDITOR) as usize,
                );
                for (face, up) in [(&sector.floor, true), (&sector.ceiling, false)] {
                    let Some(face) = face else { continue };
                    let corners = |h: [f32; 4]| -> [[i32; 3]; 4] {
                        [
                            [x0, oy + scale(h[0]), z0],
                            [x1, oy + scale(h[1]), z0],
                            [x1, oy + scale(h[2]), z1],
                            [x0, oy + scale(h[3]), z1],
                        ]
                    };
                    let toward = if up { [0.0, 1.0, 0.0] } else { [0.0, -1.0, 0.0] };
                    let c1 = corners(face.heights);
                    let c2 = corners(face.heights_2.unwrap_or(face.heights));
                    let name1 = &face.texture.name;
                    let name2 = face
                        .texture_2
                        .as_ref()
                        .map(|t| &t.name)
                        .unwrap_or(name1);
                    let planar = face.heights_2.is_none()
                        && name1 == name2
                        && Plane::from_points([c1[0], c1[1], c1[2]])
                            .is_some_and(|plane| plane.side(c1[3]) == 0);
                    if planar {
                        push_face(&mut faces, &c1, toward, name1, cell);
                    } else {
                        let (t1, t2) = match face.split_direction {
                            SplitDirection::NwSe => ([0, 1, 2], [0, 2, 3]),
                            SplitDirection::NeSw => ([0, 1, 3], [1, 2, 3]),
                        };
                        push_face(&mut faces, &t1.map(|i| c1[i]), toward, name1, cell);
                        push_face(&mut faces, &t2.map(|i| c2[i]), toward, name2, cell);
                    }
                }
                let edges: [(&Vec<VFace>, [i32; 2], [i32; 2], [f64; 3]); 6] = [
                    (&sector.walls_north, [x0, z0], [x1, z0], [0.0, 0.0, 1.0]),
                    (&sector.walls_east, [x1, z0], [x1, z1], [-1.0, 0.0, 0.0]),
                    (&sector.walls_south, [x1, z1], [x0, z1], [0.0, 0.0, -1.0]),
                    (&sector.walls_west, [x0, z1], [x0, z0], [1.0, 0.0, 0.0]),
                    (&sector.walls_nwse, [x0, z0], [x1, z1], [s, 0.0, -s]),
                    (&sector.walls_nesw, [x1, z0], [x0, z1], [-s, 0.0, -s]),
                ];
                for (walls, a, b, front) in edges {
                    for wall in walls {
                        let h = wall.heights;
                        let visible = match wall.normal_mode {
                            NormalMode::Back => front.map(|v| -v),
                            NormalMode::Front | NormalMode::Both => front,
                        };
                        let name = &wall.texture.name;
                        let (y0, y1, y2, y3) = (
                            oy + scale(h[0]),
                            oy + scale(h[1]),
                            oy + scale(h[2]),
                            oy + scale(h[3]),
                        );
                        let rectangle = y0 == y1 && y2 == y3 && y3 > y0;
                        if rectangle {
                            // One quad per sector-tall band, cut on absolute
                            // multiples of a sector so the texture tiles like
                            // bonnie's.
                            let mut start = y0;
                            while start < y3 {
                                let next = ((start.div_euclid(CELL_EDITOR) + 1) * CELL_EDITOR).min(y3);
                                push_face(
                                    &mut faces,
                                    &[
                                        [a[0], start, a[1]],
                                        [b[0], start, b[1]],
                                        [b[0], next, b[1]],
                                        [a[0], next, a[1]],
                                    ],
                                    visible,
                                    name,
                                    cell,
                                );
                                start = next;
                            }
                        } else {
                            push_face(
                                &mut faces,
                                &[
                                    [a[0], y0, a[1]],
                                    [b[0], y1, b[1]],
                                    [b[0], y2, b[1]],
                                    [a[0], y3, a[1]],
                                ],
                                visible,
                                name,
                                cell,
                            );
                        }
                    }
                }
            }
        }
    }
    (faces, (min_x, min_z), grid)
}

/// bonnie-style texel coordinates of a ring, using the BSP cook's paraxial
/// projection (floor and ceiling on XZ, X-facing walls on ZY, Z-facing walls
/// on XY) at 18 editor units per texel, rebased so the minimum lands in one
/// 64-texel repeat. Values are clamped to 63 so a tile never reaches the far
/// edge of its 64 texel texture.
fn face_uv(ring: &[[f64; 3]], overflow: &mut usize) -> Vec<[u8; 2]> {
    let normal = face_normal(ring);
    let abs = normal.map(f64::abs);
    let project = |p: [f64; 3]| -> [f64; 2] {
        if abs[1] >= abs[0] && abs[1] >= abs[2] {
            [p[0], p[2]]
        } else if abs[0] >= abs[2] {
            [p[2], -p[1]]
        } else {
            [p[0], -p[1]]
        }
    };
    let mut uvs: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| {
            let raw = project(*p);
            [raw[0] / TEXEL_EDITOR, raw[1] / TEXEL_EDITOR]
        })
        .collect();
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
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// Inputs the cook borrows from the playtest package build.
pub struct GridCookInput<'a> {
    pub project: &'a ProjectDocument,
    pub level_path: &'a Path,
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
    let source = std::fs::read_to_string(input.level_path)
        .map_err(|error| format!("{}: {error}", input.level_path.display()))?;
    let level: Level = ron::from_str(&source).map_err(|error| format!("parse level: {error}"))?;
    let (faces, origin_editor, grid) = collect_faces(&level);
    let scene = input.project.active_scene();
    let occluders: Vec<Brush> = scene
        .brushes
        .iter()
        .filter(|brush| brush.contents.is_solid())
        .cloned()
        .collect();

    // Texture name -> (material resource, asset id).
    let mut material_of: BTreeMap<String, (ResourceId, u16)> = BTreeMap::new();
    for resource in &input.project.resources {
        let ResourceData::Material(material) = &resource.data else {
            continue;
        };
        if material.sky_aperture {
            continue;
        }
        let Some(path) = material.psxt_path.as_deref() else {
            continue;
        };
        let asset = input
            .texture_assets
            .iter()
            .find(|(key, _)| key.split('#').next() == Some(path))
            .map(|(_, asset)| *asset);
        if let Some(asset) = asset {
            material_of.insert(resource.name.clone(), (resource.id, asset));
        }
    }

    let mut stats = GridPackStats {
        faces: faces.len(),
        ..GridPackStats::default()
    };

    // Keep only faces whose texture the BSP cook registered, so the runtime
    // can bind it.
    let mut kept: Vec<&Face> = Vec::new();
    for face in &faces {
        if material_of.contains_key(&face.texture) {
            kept.push(face);
        } else {
            stats.dropped_unmapped += 1;
        }
    }

    // Bake every corner with the editor's brush light bake.
    let surfaces: Vec<CompiledSurface> = kept
        .iter()
        .map(|face| {
            let points: Vec<[i32; 3]> = face
                .ring
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
                vertices: face.ring.clone(),
                material: Some(material_of[&face.texture].0),
                uv: FaceUv::default(),
                edge_repair: None,
                contents: BrushContents::Solid,
                source_brush: 0,
                source_face: 0,
            }
        })
        .collect();
    let baked = bake_brush_vertex_lighting(
        &surfaces,
        &occluders,
        input.ambient,
        input.lights,
        input.tints,
    )
    .map_err(|error| format!("grid light bake: {error:?}"))?;

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
    let cell_world = (f64::from(CELL_EDITOR) / WORLD_DIVISOR).round() as i32;
    let (grid_w, grid_d) = grid;
    let cell_count = grid_w * grid_d;

    // Quads grouped by cell, in cell order.
    let mut by_cell: Vec<Vec<usize>> = vec![Vec::new(); cell_count];
    for (index, face) in kept.iter().enumerate() {
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
            let face = kept[index];
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
                normal = cross(sub(wf(distinct[i]), wf(distinct[0])), sub(wf(distinct[i + 1]), wf(distinct[0])));
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

            let uvs_all = face_uv(&face.ring, &mut stats.uv_overflow);
            let slot = {
                let asset = material_of[&face.texture].1;
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
                flags: if count == 3 { QUAD_TRIANGLE } else { 0 },
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
            .any(|&index| face_normal(&kept[index].ring)[1] < -0.5)
    };
    let has_floor = |cx: usize, cz: usize| {
        by_cell[cz * grid_w + cx]
            .iter()
            .any(|&index| face_normal(&kept[index].ring)[1] > 0.5)
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
