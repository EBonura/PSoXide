//! The raw bonnie-32 level source of the grid cook.
//!
//! Kept for the open-hall bench scripts: `OPEN_HALL_GRID_LEVEL` names a
//! decompressed bonnie-32 level and the cook rebuilds its faces from the file
//! instead of from the project's grid document. Play never needs it.

use serde::Deserialize;

use crate::brush::Plane;

use super::{
    cross, dot, face_normal, sub, texture_key, Face, FaceMaterial, FaceSet, SLAB_THICKNESS,
};

/// Editor units per bonnie sector after the importer's 9/16 scale.
pub(super) const CELL_EDITOR: i32 = 576;
const SCALE_NUM: i32 = 9;
const SCALE_DEN: i32 = 16;
/// Editor units per texel (the importer's UV scale: 32 texels per sector).
const TEXEL_EDITOR: f64 = 18.0;

#[derive(Deserialize, Clone, Copy)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Deserialize)]
struct TexRef {
    #[serde(default)]
    pack: String,
    name: String,
}

impl TexRef {
    fn key(&self) -> String {
        texture_key(&self.pack, &self.name)
    }
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

fn scale(value: f32) -> i32 {
    let scaled = value.round() as i64 * i64::from(SCALE_NUM);
    assert!(
        scaled % i64::from(SCALE_DEN) == 0,
        "bonnie coordinate {value} is off the 16-unit grid"
    );
    (scaled / i64::from(SCALE_DEN)) as i32
}

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
            material: FaceMaterial::Name(texture.to_string()),
            cell,
            room: 0,
            texels: None,
        });
    }
}

/// Clip a convex ring to the slab `lo <= y <= hi`, rounding cut points to whole
/// editor units, and return the result as pieces of at most four corners
/// (a longer ring is fanned into quads and triangles).
fn clip_band(ring: &[[i32; 3]], lo: i32, hi: i32) -> Vec<Vec<[i32; 3]>> {
    let clip = |input: Vec<[i32; 3]>, keep: &dyn Fn(i32) -> bool, plane: i32| -> Vec<[i32; 3]> {
        let mut output = Vec::new();
        for i in 0..input.len() {
            let (p, q) = (input[i], input[(i + 1) % input.len()]);
            let (p_in, q_in) = (keep(p[1]), keep(q[1]));
            if p_in {
                output.push(p);
            }
            if p_in != q_in {
                let t = f64::from(plane - p[1]) / f64::from(q[1] - p[1]);
                let at = |axis: usize| {
                    (f64::from(p[axis]) + t * f64::from(q[axis] - p[axis])).round() as i32
                };
                output.push([at(0), plane, at(2)]);
            }
        }
        output
    };
    let ring = clip(ring.to_vec(), &|y| y >= lo, lo);
    let ring = if ring.is_empty() {
        ring
    } else {
        clip(ring, &|y| y <= hi, hi)
    };
    let ring = dedupe(&ring);
    let mut pieces = Vec::new();
    let mut first = 1;
    while first + 1 < ring.len() {
        let last = (first + 2).min(ring.len() - 1);
        let mut piece = vec![ring[0]];
        piece.extend_from_slice(&ring[first..=last]);
        pieces.push(piece);
        first = last;
    }
    pieces
}

/// `push_face`, but a ring taller than the 64 texel texture, or straddling a
/// 64 texel repeat in y, would clamp its v coordinate and smear: cut it on
/// the same absolute sector bands the rectangular walls use.
fn push_banded(
    faces: &mut Vec<Face>,
    ring: &[[i32; 3]],
    toward: [f64; 3],
    texture: &str,
    cell: (usize, usize),
) {
    let low = ring.iter().map(|p| p[1]).min().expect("ring has points");
    let high = ring.iter().map(|p| p[1]).max().expect("ring has points");
    let period = (64.0 * TEXEL_EDITOR) as i32;
    let crosses = low.div_euclid(period) != (high - 1).div_euclid(period);
    if !(crosses || f64::from(high - low) > 63.0 * TEXEL_EDITOR) {
        push_face(faces, ring, toward, texture, cell);
        return;
    }
    let mut start = low.div_euclid(CELL_EDITOR) * CELL_EDITOR;
    while start < high {
        let next = start + CELL_EDITOR;
        for piece in clip_band(ring, start, next) {
            push_face(faces, &piece, toward, texture, cell);
        }
        start = next;
    }
}

/// Read the level file at `path` and collect its faces.
pub(super) fn face_set(path: &std::path::Path) -> Result<FaceSet, String> {
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let level: Level = ron::from_str(&source).map_err(|error| format!("parse level: {error}"))?;
    let (faces, origin_editor, grid) = collect_faces(&level);
    Ok(FaceSet {
        faces,
        origin_editor,
        grid,
        cell_editor: CELL_EDITOR,
        room_ambient: Vec::new(),
    })
}

/// A floor edge for risers: `(dx, dz, this floor's corners on the edge,
/// the neighbour's corners on it, the edge's end points)`.
type RiserSide = (i32, i32, [usize; 2], [usize; 2], [[i32; 2]; 2]);
/// The walls on one sector edge with the edge's ends and its front normal.
type WallRun<'a> = (&'a Vec<VFace>, [i32; 2], [i32; 2], [f64; 3]);

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
                    let toward = if up {
                        [0.0, 1.0, 0.0]
                    } else {
                        [0.0, -1.0, 0.0]
                    };
                    let c1 = corners(face.heights);
                    let c2 = corners(face.heights_2.unwrap_or(face.heights));
                    let name1 = &face.texture.key();
                    let key2 = face.texture_2.as_ref().map(TexRef::key);
                    let name2 = key2.as_ref().unwrap_or(name1);
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
                // Risers and skirts: where this floor stands above a neighbouring floor,
                // or borders no floor at all, and no authored wall covers the edge, the
                // importer's floor slab leaves its side exposed in the BSP world. Draw
                // the same face here so both renderers show the same step and the same
                // platform edge.
                if let Some(floor) = &sector.floor {
                    let height = |h: f32| oy + scale(h);
                    // (dx, dz, this floor corners on the edge, neighbour corners on it, edge points)
                    let sides: [RiserSide; 4] = [
                        (0, -1, [0, 1], [3, 2], [[x0, z0], [x1, z0]]),
                        (1, 0, [1, 2], [0, 3], [[x1, z0], [x1, z1]]),
                        (0, 1, [3, 2], [0, 1], [[x0, z1], [x1, z1]]),
                        (-1, 0, [0, 3], [1, 2], [[x0, z0], [x0, z1]]),
                    ];
                    for (side, (dx, dz, mine, theirs, points)) in sides.into_iter().enumerate() {
                        let (nx, nz) = (gx as i32 + dx, gz as i32 + dz);
                        // The neighbouring sector, when the lattice has one there.
                        let neighbour = if nx < 0 || nz < 0 {
                            None
                        } else {
                            room.sectors
                                .get(nx as usize)
                                .and_then(|c| c.get(nz as usize))
                                .and_then(|s| s.as_ref())
                        };
                        let mine_walls = match side {
                            0 => &sector.walls_north,
                            1 => &sector.walls_east,
                            2 => &sector.walls_south,
                            _ => &sector.walls_west,
                        };
                        let their_walls = neighbour.map(|n| match side {
                            0 => &n.walls_south,
                            1 => &n.walls_west,
                            2 => &n.walls_north,
                            _ => &n.walls_east,
                        });
                        if !mine_walls.is_empty() || their_walls.is_some_and(|w| !w.is_empty()) {
                            continue;
                        }
                        let a = mine.map(|k| height(floor.heights[k]));
                        // No floor next door: the slab side drops its full depth (a skirt).
                        let b = match neighbour.and_then(|n| n.floor.as_ref()) {
                            Some(other) => theirs.map(|k| height(other.heights[k])),
                            None => a.map(|h| h - SLAB_THICKNESS as i32),
                        };
                        if a[0] < b[0] || a[1] < b[1] || (a[0] == b[0] && a[1] == b[1]) {
                            continue;
                        }
                        let bottom = |a: i32, b: i32| b.max(a - SLAB_THICKNESS as i32);
                        let ring = [
                            [points[0][0], a[0], points[0][1]],
                            [points[1][0], a[1], points[1][1]],
                            [points[1][0], bottom(a[1], b[1]), points[1][1]],
                            [points[0][0], bottom(a[0], b[0]), points[0][1]],
                        ];
                        push_banded(
                            &mut faces,
                            &ring,
                            [f64::from(dx), 0.0, f64::from(dz)],
                            &floor.texture.key(),
                            cell,
                        );
                    }
                }
                let edges: [WallRun<'_>; 6] = [
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
                        let name = &wall.texture.key();
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
                                let next =
                                    ((start.div_euclid(CELL_EDITOR) + 1) * CELL_EDITOR).min(y3);
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
                            let ring = [
                                [a[0], y0, a[1]],
                                [b[0], y1, b[1]],
                                [b[0], y2, b[1]],
                                [a[0], y3, a[1]],
                            ];
                            // A wall with a sloped base or top is not banded above.
                            push_banded(&mut faces, &ring, visible, name, cell);
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
pub(super) fn face_uv(ring: &[[f64; 3]], overflow: &mut usize) -> Vec<[u8; 2]> {
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
    let uvs: Vec<[f64; 2]> = ring
        .iter()
        .map(|p| {
            let raw = project(*p);
            [raw[0] / TEXEL_EDITOR, raw[1] / TEXEL_EDITOR]
        })
        .collect();
    super::rebase_texels(uvs, overflow)
}
