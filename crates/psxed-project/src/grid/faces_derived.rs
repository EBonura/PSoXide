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
use super::room::GridRoom;
use super::sector::WallEdge;
use super::units::{SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::uv::{Frame, Mapper};

impl Builder<'_> {
    /// The faces of one derived wall hosted by the builder's cell.
    pub(super) fn derived_wall(&mut self, room: &GridRoom, wall: &DerivedWall) {
        if wall.hidden {
            return;
        }
        let [cx, cz] = self.cell;
        let (scale, y_offset) = (self.scale, self.y_offset);
        let (ea, eb) = wall.edge.endpoints();
        // A face seen from the back of a diagonal walks it the other way.
        let (ea, eb) = if wall.back { (eb, ea) } else { (ea, eb) };
        let at = |e: [i32; 2]| {
            [
                cx.saturating_mul(SECTOR_UNITS) + e[0] * SECTOR_UNITS,
                cz.saturating_mul(SECTOR_UNITS) + e[1] * SECTOR_UNITS,
            ]
        };
        let (a, b) = (at(ea), at(eb));
        let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
        // World position along the edge at `u`.
        let plain = |u: i32| {
            [
                a[0] + (b[0] - a[0]) / SECTOR_UNITS * u,
                a[1] + (b[1] - a[1]) / SECTOR_UNITS * u,
            ]
        };
        // Where a position along the edge is drawn: displaced by the room's
        // jitter. Texels stay on the plain position.
        let jitter = self.jitter;
        let point = |u: i32| {
            let at = plain(u);
            match &jitter {
                Some(j) => {
                    let o = j.along(a, b, i64::from(u), i64::from(SECTOR_UNITS));
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
                (u_grid(0) * texel + u_grid(SECTOR_UNITS) * texel) / 2.0,
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
        let front = wall.edge.front_direction();
        let sign = if wall.back { -1.0 } else { 1.0 };
        let toward = [sign * f64::from(front[0]), 0.0, sign * f64::from(front[1])];
        // The frame of a derived wall is its whole run, so a fit mapping
        // repeats along the run and not per sector.
        let run = derive::run_extent(room, wall);
        let frame = Frame {
            min: [run.u[0], t_of(run.y[1])],
            max: [run.u[1], t_of(run.y[0])],
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
                    double_sided: false,
                    neighbours: neighbour_heights(room, wall.host, wall.edge),
                    run: [run.u[0], run.u[1]],
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
            Some(h) => vec![
                [0, h[0]],
                [SECTOR_UNITS, h[1]],
                [SECTOR_UNITS, h[2]],
                [0, h[3]],
            ],
            None => wall.ring.clone(),
        };
        let neighbours = neighbour_heights(room, wall.host, wall.edge);
        for ring in stitched_rings(outline, [0, SECTOR_UNITS], &neighbours) {
            let corners: Vec<Corner> = ring.iter().map(|p| corner(p[0], p[1])).collect();
            self.push(kind, corners, toward, wall.material, false);
        }
    }
}
