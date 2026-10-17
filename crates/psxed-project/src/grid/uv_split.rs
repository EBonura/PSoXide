//! Cutting a face at texture tile boundaries.
//!
//! A packet's UVs are bytes and the grid renderer maps a 64 by 64 texture
//! onto one repeat, so one primitive can hold one tile per axis. A face whose
//! texels run across more (a fit mapping of 2x2 or more, a dense world
//! mapping, an offset that straddles a boundary) is cut along the tile lines
//! of texel space into pieces that each fit. Texels stay continuous across
//! the cut, so the picture does not change; only the primitive count does.
//!
//! The cut runs in texel space on the face's own polygon, which is planar
//! with an affine texel map, so every new corner lies on an original edge or
//! on a cut line between two such points and is rounded once to a grid unit.
//! Two pieces that share a cut compute their shared corners from the same
//! two end points in the same order, so they agree to the unit.

use super::faces::GridFace;
use super::units::GridScale;
use super::uv::TILE_TEXELS;

/// Whether faces on the plain legacy mapping are cut too. They are not: a
/// tall sloped wall at the old 32 texels per sector can span several tiles,
/// and the cook has always clamped it into one (the editor preview wraps it
/// instead). Cutting them would fix that and add primitives to levels made
/// before the UV model (measured: Cathedral +38, level008 +81, Dungeon +8,
/// Cave +18), so it is off until the levels are re-baselined; flip it to
/// make the cook match the preview.
pub const SPLIT_LEGACY: bool = false;

/// Most pieces one face is cut into; a face asking for more is left whole
/// (and counted as an overflow by the cook).
pub const MAX_PIECES: i64 = 1024;

/// Whether the texels of a face span more than one tile along an axis once
/// shifted so the lowest lands in the first tile (the same rule the packet
/// rebase uses).
pub fn needs_split(texels: &[[f64; 2]]) -> bool {
    (0..2).any(|axis| {
        let min = texels.iter().map(|t| t[axis]).fold(f64::MAX, f64::min);
        let max = texels.iter().map(|t| t[axis]).fold(f64::MIN, f64::max);
        let shift = (min / TILE_TEXELS).floor() * TILE_TEXELS;
        max - shift > TILE_TEXELS + 0.5
    })
}

/// Whether the cook cuts `face`: its texels span more than a tile and its
/// mapping is not the legacy one (unless [`SPLIT_LEGACY`]).
pub fn wants_split(face: &GridFace) -> bool {
    (SPLIT_LEGACY || !face.legacy_uv) && needs_split(&face.texels)
}

#[derive(Clone, Copy, Debug)]
struct Vert {
    g: [f64; 3],
    t: [f64; 2],
}

/// Total order on endpoints, so an edge is cut identically whichever way it
/// is walked.
fn before(a: &Vert, b: &Vert) -> bool {
    let key = |v: &Vert| [v.t[0], v.t[1], v.g[0], v.g[1], v.g[2]];
    key(a)
        .iter()
        .zip(key(b))
        .find(|(x, y)| **x != *y)
        .is_some_and(|(x, y)| *x < y)
}

fn lerp(a: &Vert, b: &Vert, axis: usize, bound: f64) -> Vert {
    let (a, b) = if before(a, b) { (a, b) } else { (b, a) };
    let span = b.t[axis] - a.t[axis];
    let k = if span == 0.0 {
        0.0
    } else {
        (bound - a.t[axis]) / span
    };
    let mut out = Vert {
        g: [0.0; 3],
        t: [0.0; 2],
    };
    for i in 0..3 {
        out.g[i] = a.g[i] + (b.g[i] - a.g[i]) * k;
    }
    for i in 0..2 {
        out.t[i] = a.t[i] + (b.t[i] - a.t[i]) * k;
    }
    // Land exactly on the cut line.
    out.t[axis] = bound;
    out
}

/// Keep the part of `poly` with `t[axis] >= bound` (`keep_above`) or `<=`.
fn clip(poly: &[Vert], axis: usize, bound: f64, keep_above: bool) -> Vec<Vert> {
    let inside = |v: &Vert| {
        if keep_above {
            v.t[axis] >= bound
        } else {
            v.t[axis] <= bound
        }
    };
    let mut out = Vec::with_capacity(poly.len() + 2);
    for i in 0..poly.len() {
        let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
        match (inside(a), inside(b)) {
            (true, true) => out.push(*b),
            (true, false) => out.push(lerp(a, b, axis, bound)),
            (false, true) => {
                out.push(lerp(a, b, axis, bound));
                out.push(*b);
            }
            (false, false) => {}
        }
    }
    out
}

/// Cut `face` at the 64 texel lines. Returns the face itself when it needs
/// no cut or would need more than [`MAX_PIECES`].
pub fn split(face: &GridFace, scale: GridScale) -> Vec<GridFace> {
    if !wants_split(face) {
        return vec![face.clone()];
    }
    let range = |axis: usize| {
        let min = face.texels.iter().map(|t| t[axis]).fold(f64::MAX, f64::min);
        let max = face.texels.iter().map(|t| t[axis]).fold(f64::MIN, f64::max);
        let first = (min / TILE_TEXELS).floor() as i64;
        // A face ending exactly on a line does not enter the next tile.
        let last = ((max / TILE_TEXELS) - 1e-9).ceil() as i64 - 1;
        (first, last.max(first))
    };
    let ((u0, u1), (v0, v1)) = (range(0), range(1));
    if (u1 - u0 + 1) * (v1 - v0 + 1) > MAX_PIECES {
        return vec![face.clone()];
    }
    let ring: Vec<Vert> = face
        .grid
        .iter()
        .zip(&face.texels)
        .map(|(g, t)| Vert {
            g: g.map(f64::from),
            t: *t,
        })
        .collect();
    let mut out = Vec::new();
    for tv in v0..=v1 {
        for tu in u0..=u1 {
            let (lo, hi) = (
                [tu as f64 * TILE_TEXELS, tv as f64 * TILE_TEXELS],
                [(tu + 1) as f64 * TILE_TEXELS, (tv + 1) as f64 * TILE_TEXELS],
            );
            let mut poly = ring.clone();
            for (axis, (low, high)) in [(0, (lo[0], hi[0])), (1, (lo[1], hi[1]))] {
                poly = clip(&poly, axis, low, true);
                poly = clip(&poly, axis, high, false);
                if poly.len() < 3 {
                    break;
                }
            }
            if poly.len() < 3 {
                continue;
            }
            // Round once to grid units and drop repeated corners.
            let mut corners: Vec<([i32; 3], [f64; 2])> = Vec::new();
            for v in &poly {
                let g = v.g.map(|c| c.round() as i32);
                if corners.last().map(|c| c.0) != Some(g) {
                    corners.push((g, v.t));
                }
            }
            while corners.len() > 1 && corners.first().map(|c| c.0) == corners.last().map(|c| c.0) {
                corners.pop();
            }
            if corners.len() < 3 {
                continue;
            }
            // Fan anything past a quad into triangles.
            let groups: Vec<Vec<usize>> = if corners.len() <= 4 {
                vec![(0..corners.len()).collect()]
            } else {
                (1..corners.len() - 1).map(|i| vec![0, i, i + 1]).collect()
            };
            for group in groups {
                let mut piece = face.clone();
                piece.grid = group.iter().map(|&i| corners[i].0).collect();
                piece.points = piece
                    .grid
                    .iter()
                    .map(|g| g.map(|c| scale.to_editor(c)))
                    .collect();
                piece.texels = group.iter().map(|&i| corners[i].1).collect();
                out.push(piece);
            }
        }
    }
    if out.is_empty() {
        return vec![face.clone()];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face_uv::{FaceUv, UvMode};
    use crate::grid::faces::all_faces;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::world::GridWorld;

    fn floor_with(uv: FaceUv) -> GridWorld {
        let mut room = GridRoom::new("r", 1, 1);
        let mut floor = GridSurface::flat(0, None);
        floor.tri_a.uv = uv;
        room.sector_or_insert(0, 0).unwrap().floor = Some(floor);
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
        }
    }

    fn area(face: &GridFace) -> f64 {
        // Plan area (floors are flat in these tests).
        let n = face.grid.len();
        (0..n)
            .map(|i| {
                let (a, b) = (face.grid[i], face.grid[(i + 1) % n]);
                f64::from(a[0]) * f64::from(b[2]) - f64::from(b[0]) * f64::from(a[2])
            })
            .sum::<f64>()
            .abs()
            / 2.0
    }

    #[test]
    fn a_face_within_one_tile_is_left_alone() {
        for mode in [UvMode::Inherit, UvMode::fit(1, 1)] {
            let faces = all_faces(&floor_with(FaceUv {
                mode,
                ..FaceUv::default()
            }));
            assert!(!needs_split(&faces[0].texels));
            assert_eq!(split(&faces[0], GridScale { num: 1, den: 1 }).len(), 1);
        }
    }

    #[test]
    fn fit_2x2_and_4x4_cut_into_the_tiles_and_cover_the_face_exactly() {
        for n in [2u16, 4] {
            let faces = all_faces(&floor_with(FaceUv {
                mode: UvMode::fit(n, n),
                ..FaceUv::default()
            }));
            assert_eq!(faces.len(), 1);
            let pieces = split(&faces[0], GridScale { num: 1, den: 1 });
            assert_eq!(pieces.len(), usize::from(n * n));
            let total: f64 = pieces.iter().map(area).sum();
            assert!((total - 1024.0 * 1024.0).abs() < 1.0, "{total}");
            for piece in &pieces {
                // Each piece is one tile: texels span at most 64 on each
                // axis and start on a tile boundary.
                for axis in 0..2 {
                    let lo = piece
                        .texels
                        .iter()
                        .map(|t| t[axis])
                        .fold(f64::MAX, f64::min);
                    let hi = piece
                        .texels
                        .iter()
                        .map(|t| t[axis])
                        .fold(f64::MIN, f64::max);
                    assert!(hi - lo <= 64.0 + 1e-9, "{piece:?}");
                    assert!((lo / 64.0 - (lo / 64.0).round()).abs() < 1e-9);
                }
                assert!(!needs_split(&piece.texels));
            }
        }
    }

    #[test]
    fn neighbouring_pieces_share_exact_corners() {
        let faces = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(3, 3),
            ..FaceUv::default()
        }));
        let pieces = split(&faces[0], GridScale { num: 1, den: 1 });
        assert_eq!(pieces.len(), 9);
        // No corner of one piece lies strictly inside the edge of another
        // without being one of its corners (no T-junctions).
        let all: Vec<[i32; 3]> = pieces.iter().flat_map(|p| p.grid.clone()).collect();
        for piece in &pieces {
            let n = piece.grid.len();
            for i in 0..n {
                let (a, b) = (piece.grid[i], piece.grid[(i + 1) % n]);
                for q in &all {
                    if *q == a || *q == b {
                        continue;
                    }
                    let cross = i64::from(b[0] - a[0]) * i64::from(q[2] - a[2])
                        - i64::from(b[2] - a[2]) * i64::from(q[0] - a[0]);
                    let within =
                        (q[0] - a[0]) * (q[0] - b[0]) <= 0 && (q[2] - a[2]) * (q[2] - b[2]) <= 0;
                    assert!(!(cross == 0 && within), "{q:?} inside {a:?}-{b:?}");
                }
            }
        }
    }

    #[test]
    fn an_offset_that_straddles_a_boundary_cuts_there() {
        let faces = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(1, 1),
            offset_texels: [16, 0],
            ..FaceUv::default()
        }));
        let pieces = split(&faces[0], GridScale { num: 1, den: 1 });
        assert_eq!(pieces.len(), 2);
        let total: f64 = pieces.iter().map(area).sum();
        assert!((total - 1024.0 * 1024.0).abs() < 1.0);
    }

    #[test]
    fn a_turned_free_rotation_still_cuts_into_convex_pieces() {
        let faces = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(2, 2),
            rotation_deg: 30,
            ..FaceUv::default()
        }));
        let pieces = split(&faces[0], GridScale { num: 1, den: 1 });
        assert!(pieces.len() > 4);
        let total: f64 = pieces.iter().map(area).sum();
        assert!((total - 1024.0 * 1024.0).abs() < 8.0, "{total}");
        assert!(pieces.iter().all(|p| p.grid.len() <= 4));
    }
}
