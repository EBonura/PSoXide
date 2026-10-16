//! The drawable faces of a grid world.
//!
//! One function turns the document into flat polygons in editor units, with
//! texel UVs, so the editor preview, the light bake and (later) the cook all
//! read the same geometry instead of each reinterpreting sectors at draw
//! time. Output is deterministic and shares corner positions exactly: every
//! corner is `scale.to_editor` of an integer grid coordinate, so neighbouring
//! faces agree to the unit and the cook can weld them.
//!
//! Rules, all inherited from bonnie-32 so imported levels look the same:
//!
//! * A planar surface whose two triangles agree becomes one quad; anything
//!   else becomes two triangles.
//! * A rectangular wall is cut into bands at absolute multiples of one
//!   sector in height, so the world-aligned texture tiles and every band
//!   spans at most [`TEXELS_PER_SECTOR`] texels vertically.
//! * Default mapping is world aligned at [`TEXELS_PER_SECTOR`] texels per
//!   sector, so adjacent faces continue each other. A face's offset, rotation
//!   and scale apply about the face's own centre.
//! * Polygons are wound counter-clockwise seen from their visible side.

use crate::brush::FaceUv;
use crate::ResourceId;

use super::sector::{GridSector, WallEdge};
use super::surface::{GridSurface, GridWall, WallFacing};
use super::units::{GridScale, SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::world::GridWorld;

/// What a face was made from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridFaceKind {
    /// Floor; `triangle` is `None` for a merged quad, else `0` or `1`.
    Floor {
        /// Which triangle of a split surface, if split.
        triangle: Option<u8>,
    },
    /// Ceiling, same convention as `Floor`.
    Ceiling {
        /// Which triangle of a split surface, if split.
        triangle: Option<u8>,
    },
    /// A wall segment (or one band of it).
    Wall {
        /// Owning edge.
        edge: WallEdge,
        /// Index in the edge's stack.
        index: usize,
    },
}

/// One drawable polygon of 3 or 4 corners.
#[derive(Clone, Debug, PartialEq)]
pub struct GridFace {
    /// Room index.
    pub room: usize,
    /// World sector `[x, z]` that owns the face.
    pub cell: [i32; 2],
    /// Source of the face.
    pub kind: GridFaceKind,
    /// Corners in editor units, counter-clockwise seen from the visible side.
    pub points: Vec<[i32; 3]>,
    /// Texel coordinates per corner, before any wrap into the texture.
    pub texels: Vec<[f64; 2]>,
    /// Unit normal of the visible side.
    pub normal: [f64; 3],
    /// Material resource.
    pub material: Option<ResourceId>,
    /// Whether the back side is drawn too.
    pub double_sided: bool,
}

#[derive(Clone, Copy)]
struct Corner {
    p: [i32; 3],
    uv: [f64; 2],
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Newell normal of a ring, not normalised.
fn ring_normal(ring: &[Corner]) -> [f64; 3] {
    let mut n = [0.0; 3];
    for i in 0..ring.len() {
        let a = ring[i].p.map(f64::from);
        let b = ring[(i + 1) % ring.len()].p.map(f64::from);
        let c = cross(a, b);
        n = [n[0] + c[0], n[1] + c[1], n[2] + c[2]];
    }
    n
}

/// Apply a per-face UV transform about `centre`.
fn place(uv: &FaceUv, centre: [f64; 2], raw: [f64; 2]) -> [f64; 2] {
    if uv.is_identity() {
        return raw;
    }
    let local = [raw[0] - centre[0], raw[1] - centre[1]];
    let linear = uv.apply_linear(local);
    [
        linear[0] + centre[0] + f64::from(uv.offset_texels[0]),
        linear[1] + centre[1] + f64::from(uv.offset_texels[1]),
    ]
}

struct Builder<'a> {
    scale: GridScale,
    room: usize,
    cell: [i32; 2],
    y_offset: i32,
    out: &'a mut Vec<GridFace>,
}

/// Editor-space point of a grid coordinate in a room lifted by `y_offset`.
fn editor_point(scale: GridScale, y_offset: i32, gx: i32, gy: i32, gz: i32) -> [i32; 3] {
    [
        scale.to_editor(gx),
        scale.to_editor(y_offset + gy),
        scale.to_editor(gz),
    ]
}

impl Builder<'_> {
    /// Finish a ring: drop repeated corners, wind it to face `toward` and
    /// push it. Rings that collapse to a line or point are dropped.
    fn push(
        &mut self,
        kind: GridFaceKind,
        mut ring: Vec<Corner>,
        toward: [f64; 3],
        material: Option<ResourceId>,
        double_sided: bool,
    ) {
        let mut deduped: Vec<Corner> = Vec::with_capacity(ring.len());
        for corner in ring.drain(..) {
            if deduped.last().map(|c| c.p) != Some(corner.p) {
                deduped.push(corner);
            }
        }
        while deduped.len() > 1 && deduped.first().map(|c| c.p) == deduped.last().map(|c| c.p) {
            deduped.pop();
        }
        if deduped.len() < 3 {
            return;
        }
        let normal = ring_normal(&deduped);
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        if length == 0.0 {
            return;
        }
        let mut normal = normal.map(|v| v / length);
        let facing = normal[0] * toward[0] + normal[1] * toward[1] + normal[2] * toward[2];
        if facing < 0.0 {
            deduped.reverse();
            normal = normal.map(|v| -v);
        }
        self.out.push(GridFace {
            room: self.room,
            cell: self.cell,
            kind,
            points: deduped.iter().map(|c| c.p).collect(),
            texels: deduped.iter().map(|c| c.uv).collect(),
            normal,
            material,
            double_sided,
        });
    }

    /// Floor or ceiling faces for one surface.
    fn surface(&mut self, surface: &GridSurface, ceiling: bool) {
        let [cx, cz] = self.cell;
        let (scale, y_offset) = (self.scale, self.y_offset);
        let x0 = cx.saturating_mul(SECTOR_UNITS);
        let z0 = cz.saturating_mul(SECTOR_UNITS);
        let xz = [
            [x0, z0],
            [x0 + SECTOR_UNITS, z0],
            [x0 + SECTOR_UNITS, z0 + SECTOR_UNITS],
            [x0, z0 + SECTOR_UNITS],
        ];
        let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
        let centre = [
            (f64::from(x0) + f64::from(SECTOR_UNITS) / 2.0) * texel,
            (f64::from(z0) + f64::from(SECTOR_UNITS) / 2.0) * texel,
        ];
        let toward = if ceiling {
            [0.0, -1.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let corner = |h: &[i32; 4], i: usize, uv: &FaceUv| Corner {
            p: editor_point(scale, y_offset, xz[i][0], h[i], xz[i][1]),
            uv: place(
                uv,
                centre,
                [f64::from(xz[i][0]) * texel, f64::from(xz[i][1]) * texel],
            ),
        };
        let kind = |triangle| {
            if ceiling {
                GridFaceKind::Ceiling { triangle }
            } else {
                GridFaceKind::Floor { triangle }
            }
        };
        let merged =
            surface.heights_b.is_none() && surface.tri(1) == surface.tri(0) && surface.is_planar();
        if merged {
            let tri = surface.tri(0);
            let ring: Vec<Corner> = (0..4)
                .map(|i| corner(&surface.heights, i, &tri.uv))
                .collect();
            self.push(kind(None), ring, toward, tri.material, false);
            return;
        }
        for index in 0..2 {
            let tri = surface.tri(index);
            let heights = surface.heights_for(index);
            let ring: Vec<Corner> = surface
                .split
                .triangle(index)
                .into_iter()
                .map(|i| corner(&heights, i, &tri.uv))
                .collect();
            self.push(kind(Some(index as u8)), ring, toward, tri.material, false);
        }
    }

    /// Wall faces for one segment.
    fn wall(&mut self, edge: WallEdge, index: usize, wall: &GridWall) {
        let [cx, cz] = self.cell;
        let (scale, y_offset) = (self.scale, self.y_offset);
        let (ea, eb) = edge.endpoints();
        let at = |e: [i32; 2]| {
            [
                cx.saturating_mul(SECTOR_UNITS) + e[0] * SECTOR_UNITS,
                cz.saturating_mul(SECTOR_UNITS) + e[1] * SECTOR_UNITS,
            ]
        };
        let (a, b) = (at(ea), at(eb));
        let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
        // Texel u along the walking direction a -> b.
        let diagonal_length = f64::from(SECTOR_UNITS) * core::f64::consts::SQRT_2;
        let u_at = |end: usize| -> f64 {
            let [px, pz] = if end == 0 { a } else { b };
            match edge {
                WallEdge::North => f64::from(px) * texel,
                WallEdge::East => f64::from(pz) * texel,
                WallEdge::South => -f64::from(px) * texel,
                WallEdge::West => -f64::from(pz) * texel,
                WallEdge::Nwse | WallEdge::Nesw => {
                    if end == 0 {
                        0.0
                    } else {
                        diagonal_length * texel
                    }
                }
            }
        };
        let v_of = |gy: i32| -f64::from(y_offset + gy) * texel;
        let h = wall.heights;
        let centre = [
            (u_at(0) + u_at(1)) / 2.0,
            (v_of(h[0]) + v_of(h[1]) + v_of(h[2]) + v_of(h[3])) / 4.0,
        ];
        let front = edge.front_direction();
        let length = f64::from(front[0] * front[0] + front[1] * front[1]).sqrt();
        let mut toward = [
            f64::from(front[0]) / length,
            0.0,
            f64::from(front[1]) / length,
        ];
        if wall.facing == WallFacing::Back {
            toward = toward.map(|v| -v);
        }
        let double_sided = wall.facing == WallFacing::Both;
        let corner = |end: usize, gy: i32| {
            let [px, pz] = if end == 0 { a } else { b };
            Corner {
                p: editor_point(scale, y_offset, px, gy, pz),
                uv: place(&wall.uv, centre, [u_at(end), v_of(gy)]),
            }
        };
        let kind = GridFaceKind::Wall { edge, index };
        let rectangle = h[0] == h[1] && h[3] == h[2] && h[3] > h[0];
        if !rectangle {
            let ring = vec![
                corner(0, h[0]),
                corner(1, h[1]),
                corner(1, h[2]),
                corner(0, h[3]),
            ];
            self.push(kind, ring, toward, wall.material, double_sided);
            return;
        }
        // Bands on absolute multiples of one sector in world height.
        let (bottom, top) = (y_offset + h[0], y_offset + h[3]);
        let mut start = bottom;
        while start < top {
            let next = ((start.div_euclid(SECTOR_UNITS) + 1) * SECTOR_UNITS).min(top);
            let (lo, hi) = (start - y_offset, next - y_offset);
            let ring = vec![corner(0, lo), corner(1, lo), corner(1, hi), corner(0, hi)];
            self.push(kind, ring, toward, wall.material, double_sided);
            start = next;
        }
    }

    fn sector(&mut self, sector: &GridSector) {
        if let Some(floor) = &sector.floor {
            self.surface(floor, false);
        }
        if let Some(ceiling) = &sector.ceiling {
            self.surface(ceiling, true);
        }
        for edge in WallEdge::ALL {
            for (index, wall) in sector.walls(edge).iter().enumerate() {
                self.wall(edge, index, wall);
            }
        }
    }
}

/// Append the faces of local sector `(x, z)` in room `room` to `out`.
/// Missing rooms and sectors append nothing.
pub fn cell_faces(world: &GridWorld, room: usize, x: usize, z: usize, out: &mut Vec<GridFace>) {
    let Some(r) = world.rooms.get(room) else {
        return;
    };
    let Some(sector) = r.sector(x, z) else {
        return;
    };
    Builder {
        scale: world.scale,
        room,
        cell: r.world_cell(x, z),
        y_offset: r.y_offset,
        out,
    }
    .sector(sector);
}

/// Every face of the world, room by room, `z` major within a room.
pub fn all_faces(world: &GridWorld) -> Vec<GridFace> {
    let mut out = Vec::new();
    for (room_index, room) in world.rooms.iter().enumerate() {
        for (x, z, _) in room.iter() {
            cell_faces(world, room_index, x, z, &mut out);
        }
    }
    out
}

/// Number of faces [`all_faces`] would return, without keeping them.
pub fn face_count(world: &GridWorld) -> usize {
    let mut scratch = Vec::new();
    let mut count = 0;
    for (room_index, room) in world.rooms.iter().enumerate() {
        for (x, z, _) in room.iter() {
            scratch.clear();
            cell_faces(world, room_index, x, z, &mut scratch);
            count += scratch.len();
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::{GridSplit, GridTri};

    fn one_cell(configure: impl FnOnce(&mut GridSector)) -> GridWorld {
        let mut room = GridRoom::new("r", 1, 1);
        configure(room.sector_or_insert(0, 0).unwrap());
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
        }
    }

    fn facing(face: &GridFace, direction: [f64; 3]) -> bool {
        face.normal
            .iter()
            .zip(direction)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            > 0.99
    }

    #[test]
    fn flat_floor_is_one_upward_quad() {
        let world = one_cell(|s| s.floor = Some(GridSurface::flat(0, None)));
        let faces = all_faces(&world);
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].points.len(), 4);
        assert!(facing(&faces[0], [0.0, 1.0, 0.0]));
        assert_eq!(faces[0].kind, GridFaceKind::Floor { triangle: None });
    }

    #[test]
    fn ceiling_faces_down() {
        let world = one_cell(|s| s.ceiling = Some(GridSurface::flat(1024, None)));
        let faces = all_faces(&world);
        assert_eq!(faces.len(), 1);
        assert!(facing(&faces[0], [0.0, -1.0, 0.0]));
        assert!(faces[0].points.iter().all(|p| p[1] == 1024));
    }

    #[test]
    fn non_planar_or_split_level_surfaces_become_two_triangles() {
        let bent = one_cell(|s| {
            let mut floor = GridSurface::flat(0, None);
            floor.heights = [0, 0, 256, 0];
            s.floor = Some(floor);
        });
        let faces = all_faces(&bent);
        assert_eq!(faces.len(), 2);
        assert!(faces.iter().all(|f| f.points.len() == 3));
        assert!(faces.iter().all(|f| f.normal[1] > 0.0));

        let two_materials = one_cell(|s| {
            let mut floor = GridSurface::flat(0, None);
            let mut project = crate::ProjectDocument::new("t");
            let id = project.add_resource(
                "m",
                crate::ResourceData::Material(crate::MaterialResource::opaque(None)),
            );
            floor.tri_b = Some(GridTri::with_material(Some(id)));
            s.floor = Some(floor);
        });
        assert_eq!(all_faces(&two_materials).len(), 2);
    }

    #[test]
    fn split_direction_chooses_the_diagonal() {
        let tri_corners = |split| {
            let world = one_cell(|s| {
                let mut floor = GridSurface::flat(0, None);
                floor.heights = [0, 0, 256, 0];
                floor.split = split;
                s.floor = Some(floor);
            });
            all_faces(&world)
                .iter()
                .map(|f| {
                    let mut p = f.points.clone();
                    p.sort();
                    p
                })
                .collect::<Vec<_>>()
        };
        // NW=(0,0) NE=(1024,0) SE=(1024,1024) SW=(0,1024); NW-SE splits into
        // NW,NE,SE and NW,SE,SW; NE-SW into NW,NE,SW and NE,SE,SW.
        let nwse = tri_corners(GridSplit::NwSe);
        assert!(nwse
            .iter()
            .all(|t| t.iter().any(|p| p[0] == 0 && p[2] == 0)));
        let nesw = tri_corners(GridSplit::NeSw);
        assert!(nesw
            .iter()
            .all(|t| t.iter().any(|p| p[0] == 1024 && p[2] == 0)));
        assert_ne!(nwse, nesw);
    }

    #[test]
    fn walls_face_into_the_sector_and_flip_with_facing() {
        for edge in [
            WallEdge::North,
            WallEdge::East,
            WallEdge::South,
            WallEdge::West,
        ] {
            let make = |facing| {
                one_cell(|s| {
                    let mut wall = GridWall::span(0, 512, None);
                    wall.facing = facing;
                    s.walls_mut(edge).push(wall);
                })
            };
            let front = all_faces(&make(WallFacing::Front));
            assert_eq!(front.len(), 1, "{edge:?}");
            let into = edge.front_direction();
            assert!(
                facing(&front[0], [f64::from(into[0]), 0.0, f64::from(into[1])]),
                "{edge:?} front"
            );
            let back = all_faces(&make(WallFacing::Back));
            assert!(
                facing(&back[0], [-f64::from(into[0]), 0.0, -f64::from(into[1])]),
                "{edge:?} back"
            );
            let both = all_faces(&make(WallFacing::Both));
            assert!(both[0].double_sided && !front[0].double_sided);
        }
    }

    #[test]
    fn diagonal_walls_have_diagonal_normals() {
        let world = one_cell(|s| {
            s.nwse.push(GridWall::span(0, 512, None));
            s.nesw.push(GridWall::span(0, 512, None));
        });
        let faces = all_faces(&world);
        let h = core::f64::consts::FRAC_1_SQRT_2;
        assert!(facing(&faces[0], [h, 0.0, -h]));
        assert!(facing(&faces[1], [-h, 0.0, -h]));
    }

    #[test]
    fn tall_rectangular_walls_are_banded_at_sector_multiples() {
        let world = one_cell(|s| s.north.push(GridWall::span(256, 2560, None)));
        let faces = all_faces(&world);
        // 256..1024, 1024..2048, 2048..2560
        assert_eq!(faces.len(), 3);
        let spans: Vec<(i32, i32)> = faces
            .iter()
            .map(|f| {
                (
                    f.points.iter().map(|p| p[1]).min().unwrap(),
                    f.points.iter().map(|p| p[1]).max().unwrap(),
                )
            })
            .collect();
        assert_eq!(spans, vec![(256, 1024), (1024, 2048), (2048, 2560)]);
        // Each full band spans exactly 32 texels vertically.
        let v: Vec<f64> = faces[1].texels.iter().map(|t| t[1]).collect();
        let span =
            v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min);
        assert!((span - 32.0).abs() < 1e-9);
    }

    #[test]
    fn room_offset_shifts_wall_bands_with_the_world() {
        // A wall from 0 to 1024 in a room lifted by 512 crosses the 1024
        // world line, so it is cut in two.
        let mut room = GridRoom::new("r", 1, 1);
        room.y_offset = 512;
        room.sector_or_insert(0, 0)
            .unwrap()
            .north
            .push(GridWall::span(0, 1024, None));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
        };
        assert_eq!(all_faces(&world).len(), 2);
    }

    #[test]
    fn sloped_walls_stay_whole_and_triangles_have_three_corners() {
        let world = one_cell(|s| {
            let mut slanted = GridWall::span(0, 512, None);
            slanted.heights = [0, 512, 1536, 1024];
            s.east.push(slanted);
            let mut triangle = GridWall::span(0, 512, None);
            triangle.heights = [0, 0, 512, 0];
            s.west.push(triangle);
        });
        let faces = all_faces(&world);
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0].points.len(), 4);
        assert_eq!(faces[1].points.len(), 3);
    }

    #[test]
    fn neighbouring_faces_share_exact_corner_positions() {
        let mut room = GridRoom::new("r", 2, 1);
        for x in 0..2 {
            room.sector_or_insert(x, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        }
        let world = GridWorld {
            scale: GridScale::DEFAULT,
            rooms: vec![room],
        };
        let faces = all_faces(&world);
        let east_of_first: Vec<[i32; 3]> = faces[0]
            .points
            .iter()
            .copied()
            .filter(|p| p[0] == 1344)
            .collect();
        let west_of_second: Vec<[i32; 3]> = faces[1]
            .points
            .iter()
            .copied()
            .filter(|p| p[0] == 1344)
            .collect();
        assert_eq!(east_of_first.len(), 2);
        let mut a = east_of_first;
        let mut b = west_of_second;
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn default_mapping_continues_across_cells() {
        let mut room = GridRoom::new("r", 2, 1);
        for x in 0..2 {
            room.sector_or_insert(x, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        }
        let world = GridWorld {
            scale: GridScale::DEFAULT,
            rooms: vec![room],
        };
        let faces = all_faces(&world);
        let u_at = |f: &GridFace, x: i32| -> Vec<f64> {
            f.points
                .iter()
                .zip(&f.texels)
                .filter(|(p, _)| p[0] == x)
                .map(|(_, t)| t[0])
                .collect()
        };
        assert!(u_at(&faces[0], 1344)
            .iter()
            .all(|u| (*u - 32.0).abs() < 1e-9));
        assert!(u_at(&faces[1], 1344)
            .iter()
            .all(|u| (*u - 32.0).abs() < 1e-9));
    }

    #[test]
    fn uv_offset_and_rotation_apply_about_the_face_centre() {
        let plain = one_cell(|s| s.floor = Some(GridSurface::flat(0, None)));
        let turned = one_cell(|s| {
            let mut floor = GridSurface::flat(0, None);
            floor.tri_a.uv.rotation_deg = 90;
            s.floor = Some(floor);
        });
        let shifted = one_cell(|s| {
            let mut floor = GridSurface::flat(0, None);
            floor.tri_a.uv.offset_texels = [3, -2];
            s.floor = Some(floor);
        });
        let (p, t, o) = (
            &all_faces(&plain)[0],
            &all_faces(&turned)[0],
            &all_faces(&shifted)[0],
        );
        let mean = |f: &GridFace| {
            let n = f.texels.len() as f64;
            [
                f.texels.iter().map(|t| t[0]).sum::<f64>() / n,
                f.texels.iter().map(|t| t[1]).sum::<f64>() / n,
            ]
        };
        // Rotation about the centre leaves the centre where it was.
        assert!((mean(p)[0] - mean(t)[0]).abs() < 1e-9);
        assert!((mean(p)[1] - mean(t)[1]).abs() < 1e-9);
        // ... but moves the corners.
        assert_ne!(p.texels[0], t.texels[0]);
        // Offset moves everything rigidly.
        assert!((mean(o)[0] - mean(p)[0] - 3.0).abs() < 1e-9);
        assert!((mean(o)[1] - mean(p)[1] + 2.0).abs() < 1e-9);
    }

    #[test]
    fn collapsed_geometry_produces_nothing() {
        let world = one_cell(|s| s.north.push(GridWall::span(512, 512, None)));
        assert!(all_faces(&world).is_empty());
        assert_eq!(face_count(&world), 0);
    }
}
