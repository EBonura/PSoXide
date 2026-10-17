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
//! * Polygons are wound clockwise seen from their visible side, the engine's
//!   front-face convention, so the right-handed cross product of a ring points
//!   away from the viewer. [`GridFace::normal`] is the visible-side normal.

use crate::face_uv::UvMode;
use crate::ResourceId;

use super::band_geom::{self, Span};
use super::bands;
use super::faces_bands::{neighbour_heights, stitched_rings, BandedWall};
use super::overrides::WallMode;
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::{GridSurface, GridTri, GridWall, WallFacing};
use super::units::{GridScale, SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::uv::{Frame, Mapper};
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
    /// A wall derived from a height difference (see [`super::derive`]),
    /// listed under the sector that sees it.
    Derived {
        /// The host sector's edge it lies on.
        edge: WallEdge,
        /// Which rule made it.
        kind: super::derive::DerivedKind,
    },
    /// A free plane of the room, or one tile of it.
    Plane {
        /// Index in the room's plane list.
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
    /// Corners in editor units, clockwise seen from the visible side.
    pub points: Vec<[i32; 3]>,
    /// The same corners in grid units with the room's vertical offset
    /// applied, `[x, y, z]`. The cook maps these to runtime units in one
    /// step so what is drawn and what is stood on round identically.
    pub grid: Vec<[i32; 3]>,
    /// Texel coordinates per corner, before any wrap into the texture.
    pub texels: Vec<[f64; 2]>,
    /// Unit normal of the visible side.
    pub normal: [f64; 3],
    /// Material resource.
    pub material: Option<ResourceId>,
    /// Whether the back side is drawn too.
    pub double_sided: bool,
    /// Drawn at the material tint with no light baked in.
    pub unlit: bool,
    /// The texels come from the plain legacy mapping (world aligned at 32
    /// texels per sector, no turn or flip). Informational: the cook cuts
    /// such a face like any other when it spans more tiles than a primitive
    /// holds.
    pub legacy_uv: bool,
    /// The rectangle (grid units, in the face's own texture axes) its
    /// texture is measured against: the sector square of a floor, the record
    /// of an authored wall, the whole run of a derived wall, the plane
    /// rectangle. See [`super::uv`].
    pub frame: Frame,
}

#[derive(Clone, Copy)]
pub(super) struct Corner {
    pub p: [i32; 3],
    pub g: [i32; 3],
    pub uv: [f64; 2],
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

/// Where a material's grid tiling default comes from. Faces take the
/// default only when their own mode is `Inherit`.
pub trait UvDefaults {
    /// The tiling default of `material`, if it has one.
    fn default_uv(&self, material: ResourceId) -> Option<UvMode>;
}

/// No material has a tiling default.
impl UvDefaults for () {
    fn default_uv(&self, _material: ResourceId) -> Option<UvMode> {
        None
    }
}

pub(super) struct Builder<'a> {
    pub scale: GridScale,
    pub room: usize,
    pub cell: [i32; 2],
    pub y_offset: i32,
    pub defaults: &'a dyn UvDefaults,
    /// Whether the faces pushed next use the plain legacy mapping.
    pub legacy_uv: bool,
    /// The frame of the faces pushed next.
    pub frame: Frame,
    /// The room the faces belong to, when they are walls that can be banded
    /// or cut into courses (`None` for planes).
    pub room_ref: Option<&'a GridRoom>,
    pub out: &'a mut Vec<GridFace>,
}

impl Builder<'_> {
    /// Whether a rectangular wall is cut into courses at absolute multiples
    /// of one sector in height: only rooms made before derived walls and
    /// bands (legacy walls, byte identical output). Everything else is one
    /// face, cut by the texture splitter and the band lines alone.
    pub(super) fn cut_courses(&self) -> bool {
        self.room_ref
            .is_none_or(|room| room.wall_mode == WallMode::Authored && room.band_sets.is_empty())
    }

    /// The tiling default of `material`.
    pub(super) fn default_uv(&self, material: Option<ResourceId>) -> Option<UvMode> {
        material.and_then(|id| self.defaults.default_uv(id))
    }
}

/// Editor-space point of a grid coordinate in a room lifted by `y_offset`.
pub(super) fn editor_point(scale: GridScale, y_offset: i32, gx: i32, gy: i32, gz: i32) -> [i32; 3] {
    [
        scale.to_editor(gx),
        scale.to_editor(y_offset + gy),
        scale.to_editor(gz),
    ]
}

impl Builder<'_> {
    /// Finish a ring: drop repeated corners, wind it to face `toward` and
    /// push it. Rings that collapse to a line or point are dropped.
    pub(super) fn push(
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
        let normal = normal.map(|v| v / length);
        let facing = normal[0] * toward[0] + normal[1] * toward[1] + normal[2] * toward[2];
        // The engine's front face is the clockwise one on screen, which is
        // the ring whose right-handed normal points away from the viewer.
        // `normal` below is the visible-side normal, the opposite of it.
        let normal = if facing > 0.0 {
            deduped.reverse();
            normal
        } else {
            normal.map(|v| -v)
        };
        self.out.push(GridFace {
            room: self.room,
            cell: self.cell,
            kind,
            points: deduped.iter().map(|c| c.p).collect(),
            grid: deduped.iter().map(|c| c.g).collect(),
            texels: deduped.iter().map(|c| c.uv).collect(),
            normal,
            material,
            double_sided,
            unlit: false,
            legacy_uv: self.legacy_uv,
            frame: self.frame,
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
        // The frame of a floor or ceiling is its sector square, shared by
        // both triangles so repeats continue across the diagonal.
        let frame = Frame {
            min: [f64::from(x0), f64::from(z0)],
            max: [
                f64::from(x0) + f64::from(SECTOR_UNITS),
                f64::from(z0) + f64::from(SECTOR_UNITS),
            ],
        };
        let toward = if ceiling {
            [0.0, -1.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let defaults = self.defaults;
        let mapper = |tri: &GridTri| {
            let default = tri.material.and_then(|id| defaults.default_uv(id));
            Mapper::new(&tri.uv, default, frame, centre)
        };
        let corner = |h: &[i32; 4], i: usize, mapper: &Mapper| Corner {
            p: editor_point(scale, y_offset, xz[i][0], h[i], xz[i][1]),
            g: [xz[i][0], y_offset + h[i], xz[i][1]],
            uv: mapper.texel([f64::from(xz[i][0]), f64::from(xz[i][1])]),
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
            let m = mapper(tri);
            let ring: Vec<Corner> = (0..4).map(|i| corner(&surface.heights, i, &m)).collect();
            self.legacy_uv = m.is_legacy();
            self.frame = frame;
            self.push(kind(None), ring, toward, tri.material, false);
            return;
        }
        for index in 0..2 {
            let tri = surface.tri(index);
            let heights = surface.heights_for(index);
            let m = mapper(tri);
            let ring: Vec<Corner> = surface
                .split
                .triangle(index)
                .into_iter()
                .map(|i| corner(&heights, i, &m))
                .collect();
            self.legacy_uv = m.is_legacy();
            self.frame = frame;
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
        // Position `u` along the walking direction a -> b, in grid units.
        let diagonal_length = f64::from(SECTOR_UNITS) * core::f64::consts::SQRT_2;
        let u_grid = |end: usize| -> f64 {
            let [px, pz] = if end == 0 { a } else { b };
            match edge {
                WallEdge::North => f64::from(px),
                WallEdge::East => f64::from(pz),
                WallEdge::South => -f64::from(px),
                WallEdge::West => -f64::from(pz),
                WallEdge::Nwse | WallEdge::Nesw => {
                    if end == 0 {
                        0.0
                    } else {
                        diagonal_length
                    }
                }
            }
        };
        let t_of = |gy: i32| -f64::from(y_offset + gy);
        let h = wall.heights;
        let centre = [
            (u_grid(0) * texel + u_grid(1) * texel) / 2.0,
            (t_of(h[0]) * texel + t_of(h[1]) * texel + t_of(h[2]) * texel + t_of(h[3]) * texel)
                / 4.0,
        ];
        // The frame of an authored wall is the record itself.
        let frame = Frame::around(
            [(0, h[0]), (1, h[1]), (1, h[2]), (0, h[3])].map(|(end, gy)| [u_grid(end), t_of(gy)]),
        );
        let mapper = Mapper::new(&wall.uv, self.default_uv(wall.material), frame, centre);
        self.legacy_uv = mapper.is_legacy();
        self.frame = frame;
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
                g: [px, y_offset + gy, pz],
                uv: mapper.texel([u_grid(end), t_of(gy)]),
            }
        };
        let kind = GridFaceKind::Wall { edge, index };
        if let Some((room, set)) = self
            .room_ref
            .and_then(|room| bands::resolve(room, wall.band_set).map(|(_, set)| (room, set)))
            .filter(|_| !wall.is_degenerate())
        {
            let length = if edge.is_diagonal() {
                (diagonal_length.round()) as i32
            } else {
                SECTOR_UNITS
            };
            let span = Span::from_heights(h, length);
            // Anchors measure against the whole stack on this edge, so one
            // trim runs at the base of every segment stacked there.
            let reference = room
                .local(self.cell)
                .and_then(|[x, z]| room.sector(x, z))
                .and_then(|sector| band_geom::stack_reference(sector.walls(edge), length))
                .unwrap_or(span);
            let (u0, u1) = (u_grid(0), u_grid(1));
            let u_at = |u: i32| u0 + (u1 - u0) * f64::from(u) / f64::from(length);
            let point = |u: i32| -> [i32; 2] {
                let k = f64::from(u) / f64::from(length);
                [
                    a[0] + (f64::from(b[0] - a[0]) * k).round() as i32,
                    a[1] + (f64::from(b[1] - a[1]) * k).round() as i32,
                ]
            };
            self.banded(&BandedWall {
                kind,
                span,
                reference,
                set,
                material: wall.material,
                toward,
                double_sided,
                neighbours: neighbour_heights(room, self.cell, edge),
                run: [u0.min(u1), u0.max(u1)],
                u_grid: &u_at,
                point: &point,
            });
            return;
        }
        let rectangle = h[0] == h[1] && h[3] == h[2] && h[3] > h[0];
        let length = if edge.is_diagonal() {
            diagonal_length.round() as i32
        } else {
            SECTOR_UNITS
        };
        // The cut heights of a banded neighbour go into the vertical edges
        // of a plain wall too (nothing is inserted in a room without bands).
        let neighbours = self
            .room_ref
            .map(|room| neighbour_heights(room, self.cell, edge))
            .unwrap_or_default();
        let plain = |this: &mut Self, outline: Vec<[i32; 2]>| {
            for ring in stitched_rings(outline, [0, length], &neighbours) {
                let corners: Vec<Corner> = ring
                    .iter()
                    .map(|p| corner(usize::from(p[0] != 0), p[1]))
                    .collect();
                this.push(kind, corners, toward, wall.material, double_sided);
            }
        };
        if !rectangle || !self.cut_courses() {
            plain(
                self,
                vec![[0, h[0]], [length, h[1]], [length, h[2]], [0, h[3]]],
            );
            return;
        }
        // Courses on absolute multiples of one sector in world height.
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
/// Missing rooms and sectors append nothing. Materials have no tiling
/// default; use [`cell_faces_with`] to supply them.
pub fn cell_faces(world: &GridWorld, room: usize, x: usize, z: usize, out: &mut Vec<GridFace>) {
    cell_faces_with(world, &(), room, x, z, out);
}

/// [`cell_faces`] with the materials' tiling defaults.
pub fn cell_faces_with(
    world: &GridWorld,
    defaults: &dyn UvDefaults,
    room: usize,
    x: usize,
    z: usize,
    out: &mut Vec<GridFace>,
) {
    let Some(r) = world.rooms.get(room) else {
        return;
    };
    let Some(sector) = r.sector(x, z) else {
        return;
    };
    let cell = r.world_cell(x, z);
    let mut builder = Builder {
        scale: world.scale,
        room,
        cell,
        y_offset: r.y_offset,
        defaults,
        legacy_uv: true,
        frame: Frame::around([[0.0, 0.0]]),
        room_ref: Some(r),
        out,
    };
    builder.sector(sector);
    if r.wall_mode == WallMode::Derived {
        for wall in super::derive::cell_walls(r, cell) {
            builder.derived_wall(r, &wall);
        }
    }
}

/// Every face of the world, room by room, `z` major within a room.
pub fn all_faces(world: &GridWorld) -> Vec<GridFace> {
    all_faces_with(world, &())
}

/// [`all_faces`] with the materials' tiling defaults.
pub fn all_faces_with(world: &GridWorld, defaults: &dyn UvDefaults) -> Vec<GridFace> {
    let mut out = Vec::new();
    for (room_index, room) in world.rooms.iter().enumerate() {
        for (x, z, _) in room.iter() {
            cell_faces_with(world, defaults, room_index, x, z, &mut out);
        }
        for index in 0..room.planes.len() {
            super::plane_faces::plane_faces_with(world, defaults, room_index, index, &mut out);
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
        for index in 0..room.planes.len() {
            scratch.clear();
            super::plane_faces::plane_faces(world, room_index, index, &mut scratch);
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
            ..GridWorld::default()
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
    fn rings_wind_clockwise_seen_from_the_visible_side() {
        // Right-handed cross product of the first two edges points away from
        // the visible side: down for a floor, up for a ceiling.
        let cross_y = |face: &GridFace| {
            let p = |i: usize| face.points[i].map(f64::from);
            let (a, b, c) = (p(0), p(1), p(2));
            (b[2] - a[2]) * (c[0] - a[0]) - (b[0] - a[0]) * (c[2] - a[2])
        };
        let floor = one_cell(|s| s.floor = Some(GridSurface::flat(0, None)));
        let ceiling = one_cell(|s| s.ceiling = Some(GridSurface::flat(1024, None)));
        // y component of (b - a) x (c - a) is dz1*dx2 - dx1*dz2.
        assert!(cross_y(&all_faces(&floor)[0]) < 0.0);
        assert!(cross_y(&all_faces(&ceiling)[0]) > 0.0);
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
            ..GridWorld::default()
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
            ..GridWorld::default()
        };
        let faces = all_faces(&world);
        let east_of_first: Vec<[i32; 3]> = faces[0]
            .points
            .iter()
            .copied()
            .filter(|p| p[0] == 1280)
            .collect();
        let west_of_second: Vec<[i32; 3]> = faces[1]
            .points
            .iter()
            .copied()
            .filter(|p| p[0] == 1280)
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
            ..GridWorld::default()
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
        assert!(u_at(&faces[0], 1280)
            .iter()
            .all(|u| (*u - 32.0).abs() < 1e-9));
        assert!(u_at(&faces[1], 1280)
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
