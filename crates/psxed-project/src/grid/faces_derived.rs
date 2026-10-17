//! The drawable faces of derived walls.
//!
//! Same conventions as an authored wall (see `faces.rs`): texel `u` follows
//! the host edge's walking direction in world units, `v` is minus the world
//! height, both at the default 32 texels per sector, and a rectangle is cut
//! at absolute multiples of one sector in height. A derived face is seen from
//! its host sector, so its texture reads the same way an authored front-facing
//! wall of that sector would.

use crate::face_uv::FaceUv;

use super::derive::DerivedWall;
use super::faces::{editor_point, place, Builder, Corner, GridFaceKind};
use super::sector::WallEdge;
use super::units::{SECTOR_UNITS, TEXELS_PER_SECTOR};

impl Builder<'_> {
    /// The faces of one derived wall hosted by the builder's cell.
    pub(super) fn derived_wall(&mut self, wall: &DerivedWall) {
        if wall.hidden {
            return;
        }
        let [cx, cz] = self.cell;
        let (scale, y_offset) = (self.scale, self.y_offset);
        let (ea, eb) = wall.edge.endpoints();
        let at = |e: [i32; 2]| {
            [
                cx.saturating_mul(SECTOR_UNITS) + e[0] * SECTOR_UNITS,
                cz.saturating_mul(SECTOR_UNITS) + e[1] * SECTOR_UNITS,
            ]
        };
        let (a, b) = (at(ea), at(eb));
        let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
        // World position along the edge at `u`.
        let point = |u: i32| {
            [
                a[0] + (b[0] - a[0]) / SECTOR_UNITS * u,
                a[1] + (b[1] - a[1]) / SECTOR_UNITS * u,
            ]
        };
        let u_of = |u: i32| -> f64 {
            let [px, pz] = point(u);
            match wall.edge {
                WallEdge::North => f64::from(px) * texel,
                WallEdge::East => f64::from(pz) * texel,
                WallEdge::South => -f64::from(px) * texel,
                _ => -f64::from(pz) * texel,
            }
        };
        let v_of = |gy: i32| -f64::from(y_offset + gy) * texel;
        let centre = match wall.heights {
            Some(h) => [
                (u_of(0) + u_of(SECTOR_UNITS)) / 2.0,
                (v_of(h[0]) + v_of(h[1]) + v_of(h[2]) + v_of(h[3])) / 4.0,
            ],
            None => {
                let n = wall.ring.len() as f64;
                [
                    wall.ring.iter().map(|p| u_of(p[0])).sum::<f64>() / n,
                    wall.ring.iter().map(|p| v_of(p[1])).sum::<f64>() / n,
                ]
            }
        };
        let front = wall.edge.front_direction();
        let toward = [f64::from(front[0]), 0.0, f64::from(front[1])];
        let uv: FaceUv = wall.uv.unwrap_or_default();
        let corner = |u: i32, gy: i32| {
            let [px, pz] = point(u);
            Corner {
                p: editor_point(scale, y_offset, px, gy, pz),
                g: [px, y_offset + gy, pz],
                uv: place(&uv, centre, [u_of(u), v_of(gy)]),
            }
        };
        let kind = GridFaceKind::Derived {
            edge: wall.edge,
            kind: wall.kind,
        };
        let rectangle = wall
            .heights
            .filter(|h| h[0] == h[1] && h[3] == h[2] && h[3] > h[0]);
        let Some(h) = rectangle else {
            let ring: Vec<Corner> = match wall.heights {
                Some(h) => vec![
                    corner(0, h[0]),
                    corner(SECTOR_UNITS, h[1]),
                    corner(SECTOR_UNITS, h[2]),
                    corner(0, h[3]),
                ],
                None => wall.ring.iter().map(|p| corner(p[0], p[1])).collect(),
            };
            self.push(kind, ring, toward, wall.material, false);
            return;
        };
        // Bands on absolute multiples of one sector in world height.
        let (bottom, top) = (y_offset + h[0], y_offset + h[3]);
        let mut start = bottom;
        while start < top {
            let next = ((start.div_euclid(SECTOR_UNITS) + 1) * SECTOR_UNITS).min(top);
            let (lo, hi) = (start - y_offset, next - y_offset);
            let ring = vec![
                corner(0, lo),
                corner(SECTOR_UNITS, lo),
                corner(SECTOR_UNITS, hi),
                corner(0, hi),
            ];
            self.push(kind, ring, toward, wall.material, false);
            start = next;
        }
    }
}
