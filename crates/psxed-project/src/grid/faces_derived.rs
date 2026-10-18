//! The drawable faces of derived walls.
//!
//! Same conventions as an authored wall (see `faces.rs`): texel `u` follows
//! the host edge's walking direction in world units, `v` is minus the world
//! height, both at the default 32 texels per sector, and a rectangle is cut
//! at absolute multiples of one sector in height. A derived face is seen from
//! its host sector, so its texture reads the same way an authored front-facing
//! wall of that sector would.

use super::band_geom::Span;
use super::bands;
use super::derive::{self, DerivedWall};
use super::faces::{editor_point, Builder, Corner, GridFaceKind};
use super::faces_bands::{neighbour_heights, stitched_rings, BandedWall};
use super::overrides::ClosureFacing;
use super::room::GridRoom;
use super::sector::WallEdge;
use super::units::{SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::uv::{Frame, Mapper};

/// Where a derived wall lies and what its texture is measured against.
pub(crate) struct DerivedGeom {
    /// Grid position of the face's start and end, in its walking direction.
    pub a: [i32; 2],
    pub b: [i32; 2],
    /// Length of the segment along `u`, grid units.
    pub len: i32,
    /// Extent of the run the texture is measured against: `u` in the
    /// signed coordinate texels use, `y` room relative.
    pub run_u: [f64; 2],
    pub run_y: [i32; 2],
    /// Cut heights of the banded walls across the start and end.
    pub neighbours: [Vec<i32>; 2],
}

impl Builder<'_> {
    /// The faces of one derived wall hosted by the builder's cell.
    pub(super) fn derived_wall(&mut self, room: &GridRoom, wall: &DerivedWall) {
        if wall.hidden {
            return;
        }
        let (ea, eb) = wall.edge.endpoints();
        // A face seen from the back of a diagonal walks it the other way.
        let (ea, eb) = if wall.back { (eb, ea) } else { (ea, eb) };
        let run = derive::run_extent(room, wall);
        let geom = DerivedGeom {
            a: self.square.at(ea),
            b: self.square.at(eb),
            len: SECTOR_UNITS,
            run_u: run.u,
            run_y: run.y,
            neighbours: neighbour_heights(room, wall.host, wall.edge),
        };
        self.derived_wall_in(room, wall, geom);
    }

    /// The faces of one derived wall lying as `geom` says.
    pub(super) fn derived_wall_in(
        &mut self,
        room: &GridRoom,
        wall: &DerivedWall,
        geom: DerivedGeom,
    ) {
        // The faces carry the span that sees them, so a pick can name one of
        // several stacked on an edge.
        let saved = std::mem::replace(&mut self.span, wall.span);
        self.derived_wall_faces(room, wall, geom);
        self.span = saved;
    }

    fn derived_wall_faces(&mut self, room: &GridRoom, wall: &DerivedWall, geom: DerivedGeom) {
        if wall.hidden {
            return;
        }
        let (scale, y_offset) = (self.scale, self.y_offset);
        let DerivedGeom { a, b, len, .. } = geom;
        let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
        // World position along the edge at `u`.
        let plain = |u: i32| {
            [
                a[0] + (b[0] - a[0]) / len * u,
                a[1] + (b[1] - a[1]) / len * u,
            ]
        };
        // Where a position along the edge is drawn: displaced by the room's
        // jitter. Texels stay on the plain position.
        let jitter = self.jitter;
        let point = |u: i32| {
            let at = plain(u);
            match &jitter {
                Some(j) => {
                    let o = j.along(a, b, i64::from(u), i64::from(len));
                    [at[0] + o[0], at[1] + o[1]]
                }
                None => at,
            }
        };
        // Position along the host edge's walking direction, in grid units.
        let u_grid = |u: i32| -> f64 {
            let [px, pz] = plain(u);
            match wall.edge {
                WallEdge::North => f64::from(px),
                WallEdge::East => f64::from(pz),
                WallEdge::South => -f64::from(px),
                WallEdge::West => -f64::from(pz),
                // `u` is the projected position, the diagonal is root two
                // longer; a diagonal face measures its own length from 0.
                WallEdge::Nwse | WallEdge::Nesw => f64::from(u) * core::f64::consts::SQRT_2,
            }
        };
        let t_of = |gy: i32| -f64::from(y_offset + gy);
        let centre = match wall.heights {
            Some(h) => [
                (u_grid(0) * texel + u_grid(len) * texel) / 2.0,
                (t_of(h[0]) * texel + t_of(h[1]) * texel + t_of(h[2]) * texel + t_of(h[3]) * texel)
                    / 4.0,
            ],
            None => {
                let n = wall.ring.len() as f64;
                [
                    wall.ring.iter().map(|p| u_grid(p[0]) * texel).sum::<f64>() / n,
                    wall.ring.iter().map(|p| t_of(p[1]) * texel).sum::<f64>() / n,
                ]
            }
        };
        // A closure wall is also drawn from outside when the room says so.
        let double_sided = wall.kind == derive::DerivedKind::Closure
            && room.defaults.closure_facing == ClosureFacing::Both;
        let front = wall.edge.front_direction();
        let sign = if wall.back { -1.0 } else { 1.0 };
        let toward = [sign * f64::from(front[0]), 0.0, sign * f64::from(front[1])];
        // The frame of a derived wall is its whole run, so a fit mapping
        // repeats along the run and not per sector.
        let frame = Frame {
            min: [geom.run_u[0], t_of(geom.run_y[1])],
            max: [geom.run_u[1], t_of(geom.run_y[0])],
        };
        let uv = wall.uv.unwrap_or_default();
        let mapper = Mapper::new(&uv, self.default_uv(wall.material), frame, centre);
        self.legacy_uv = mapper.is_legacy();
        self.frame = frame;
        let corner = |u: i32, gy: i32| {
            let [px, pz] = point(u);
            Corner {
                p: editor_point(scale, y_offset, px, gy, pz),
                g: [px, y_offset + gy, pz],
                uv: mapper.texel([u_grid(u), t_of(gy)]),
            }
        };
        let kind = GridFaceKind::Derived {
            edge: wall.edge,
            kind: wall.kind,
        };
        if let Some((_, set)) = bands::resolve(room, wall.band_set) {
            if let Some(span) = Span::from_ring(&wall.ring) {
                let u_at = |u: i32| u_grid(u);
                let point_at = |u: i32| point(u);
                self.banded(&BandedWall {
                    kind,
                    span,
                    reference: span,
                    set,
                    material: wall.material,
                    toward,
                    double_sided,
                    neighbours: geom.neighbours,
                    run: geom.run_u,
                    u_grid: &u_at,
                    point: &point_at,
                });
                return;
            }
        }
        // One face, cut only by the texture splitter (a tall wall is not
        // cut into courses), with the cut heights of a banded neighbour put
        // into its vertical edges.
        let outline: Vec<[i32; 2]> = match wall.heights {
            Some(h) => vec![[0, h[0]], [len, h[1]], [len, h[2]], [0, h[3]]],
            None => wall.ring.clone(),
        };
        for ring in stitched_rings(outline, [0, len], &geom.neighbours) {
            let corners: Vec<Corner> = ring.iter().map(|p| corner(p[0], p[1])).collect();
            self.push(kind, corners, toward, wall.material, double_sided);
        }
    }
}
