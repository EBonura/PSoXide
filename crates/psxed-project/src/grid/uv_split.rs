//! Cutting a face at texture tile boundaries.
//!
//! A packet's UVs are bytes. A plain primitive holds one 64 texel tile per
//! axis (its UVs are material-relative and the renderer adds the texture's
//! page origin); a primitive flagged `REPEAT` keeps raw UVs and draws through
//! the GPU texture window (GP0 E2), which wraps them inside the texture, so
//! it holds up to [`MAX_TILES_PER_PRIMITIVE`] tiles per axis: 255 texels,
//! less the phase when the texture starts part way into a tile.
//!
//! A face whose texels run across more (a fit mapping of 8x8, a dense world
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

/// Most tiles of a texture one `REPEAT` primitive can hold per axis (the
/// packet UVs are bytes: four 64 texel tiles end at 256, whose last texel is
/// clamped to 255).
pub const MAX_TILES_PER_PRIMITIVE: i64 = 4;

/// Most pieces one face is cut into; a face asking for more is left whole
/// (and counted as an overflow by the cook).
pub const MAX_PIECES: i64 = 1024;

/// Tolerance, in texels, before a span counts as entering one more tile (so
/// a face ending on a tile line, or a hair past it from rounding, does not).
const SLACK: f64 = 0.5;

/// Tiles of one axis that texels `min..max` touch, counted from the tile the
/// lowest texel lies in: 1 for a span inside that tile.
pub fn tiles_touched(min: f64, max: f64) -> i64 {
    let shift = (min / TILE_TEXELS).floor() * TILE_TEXELS;
    (((max - shift - SLACK) / TILE_TEXELS).ceil() as i64).max(1)
}

fn bounds(texels: &[[f64; 2]], axis: usize) -> (f64, f64) {
    let min = texels.iter().map(|t| t[axis]).fold(f64::MAX, f64::min);
    let max = texels.iter().map(|t| t[axis]).fold(f64::MIN, f64::max);
    (min, max)
}

/// Whether the texels of a face fit one primitive that holds `tiles` tiles
/// per axis (1 for a plain primitive, [`MAX_TILES_PER_PRIMITIVE`] for a
/// repeating one).
pub fn fits(texels: &[[f64; 2]], tiles: i64) -> bool {
    (0..2).all(|axis| {
        let (min, max) = bounds(texels, axis);
        tiles_touched(min, max) <= tiles
    })
}

/// Whether the cook cuts `face`: its texels span more than a primitive of
/// the available kind holds. `repeat` is whether the face's material can
/// repeat through the texture window.
pub fn wants_split(face: &GridFace, repeat: bool) -> bool {
    !fits(&face.texels, tiles_per_primitive(repeat))
}

/// Tiles per axis a primitive holds.
pub const fn tiles_per_primitive(repeat: bool) -> i64 {
    if repeat {
        MAX_TILES_PER_PRIMITIVE
    } else {
        1
    }
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

/// Cut `face` at the tile lines, into pieces of at most
/// [`tiles_per_primitive`]`(repeat)` tiles per axis: the first piece of an
/// axis ends at the fourth (or first) tile line counted from the tile its
/// lowest texel lies in, the next ones follow in whole multiples, so the
/// piece count is the fewest the limit allows. Returns the face itself when
/// it needs no cut or would need more than [`MAX_PIECES`].
pub fn split(face: &GridFace, scale: GridScale, repeat: bool) -> Vec<GridFace> {
    if !wants_split(face, repeat) {
        return vec![face.clone()];
    }
    let step = tiles_per_primitive(repeat);
    let range = |axis: usize| {
        let (min, max) = bounds(&face.texels, axis);
        let first = (min / TILE_TEXELS).floor() as i64;
        (first, first + tiles_touched(min, max) - 1)
    };
    let ((u0, u1), (v0, v1)) = (range(0), range(1));
    let count = |lo: i64, hi: i64| (hi - lo) / step + 1;
    if count(u0, u1) * count(v0, v1) > MAX_PIECES {
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
    for tv in (v0..=v1).step_by(step as usize) {
        for tu in (u0..=u1).step_by(step as usize) {
            let (lo, hi) = (
                [tu as f64 * TILE_TEXELS, tv as f64 * TILE_TEXELS],
                [
                    (tu + step) as f64 * TILE_TEXELS,
                    (tv + step) as f64 * TILE_TEXELS,
                ],
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

    const UNIT: GridScale = GridScale { num: 1, den: 1 };

    fn floor_with(uv: FaceUv) -> GridWorld {
        let mut room = GridRoom::new("r", 1, 1);
        let mut floor = GridSurface::flat(0, None);
        floor.tri_a.uv = uv;
        room.sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(floor));
        GridWorld {
            scale: UNIT,
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn fit_face(h: u16, v: u16) -> GridFace {
        all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(h, v),
            ..FaceUv::default()
        }))
        .remove(0)
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

    fn span(piece: &GridFace, axis: usize) -> (f64, f64) {
        bounds(&piece.texels, axis)
    }

    #[test]
    fn a_face_within_one_tile_is_left_alone() {
        for mode in [UvMode::Inherit, UvMode::fit(1, 1)] {
            let faces = all_faces(&floor_with(FaceUv {
                mode,
                ..FaceUv::default()
            }));
            for repeat in [false, true] {
                assert!(!wants_split(&faces[0], repeat));
                assert_eq!(split(&faces[0], UNIT, repeat).len(), 1);
            }
        }
    }

    #[test]
    fn without_the_window_fit_2x2_and_4x4_cut_into_single_tiles() {
        for n in [2u16, 4] {
            let face = fit_face(n, n);
            let pieces = split(&face, UNIT, false);
            assert_eq!(pieces.len(), usize::from(n * n));
            let total: f64 = pieces.iter().map(area).sum();
            assert!((total - 1024.0 * 1024.0).abs() < 1.0, "{total}");
            for piece in &pieces {
                for axis in 0..2 {
                    let (lo, hi) = span(piece, axis);
                    assert!(hi - lo <= 64.0 + 1e-9, "{piece:?}");
                    assert!((lo / 64.0 - (lo / 64.0).round()).abs() < 1e-9);
                }
                assert!(fits(&piece.texels, 1));
            }
        }
    }

    #[test]
    fn with_the_window_a_4x4_face_is_one_primitive() {
        let face = fit_face(4, 4);
        assert!(!wants_split(&face, true));
        assert_eq!(split(&face, UNIT, true).len(), 1);
        // 4x4 is exactly the limit; 5x1 is not.
        assert!(wants_split(&fit_face(5, 1), true));
    }

    #[test]
    fn with_the_window_larger_fits_cut_into_the_fewest_pieces() {
        for (h, v, expect) in [(5, 1, 2), (8, 8, 4), (9, 4, 3), (16, 1, 4), (7, 7, 4)] {
            let face = fit_face(h, v);
            let pieces = split(&face, UNIT, true);
            assert_eq!(pieces.len(), expect, "{h}x{v}");
            let total: f64 = pieces.iter().map(area).sum();
            assert!((total - 1024.0 * 1024.0).abs() < 1.0, "{h}x{v}: {total}");
            for piece in &pieces {
                assert!(fits(&piece.texels, 4), "{h}x{v} {piece:?}");
            }
        }
    }

    #[test]
    fn a_start_part_way_into_a_tile_gives_three_and_a_bit_tiles_first() {
        // 4 tiles starting 16 texels in end at 272: the tile containing the
        // start holds the first three tiles and the 16 texels left over go
        // to a second primitive.
        let face = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(4, 1),
            offset_texels: [16, 0],
            ..FaceUv::default()
        }))
        .remove(0);
        assert!(wants_split(&face, true));
        let pieces = split(&face, UNIT, true);
        assert_eq!(pieces.len(), 2);
        let total: f64 = pieces.iter().map(area).sum();
        assert!((total - 1024.0 * 1024.0).abs() < 1.0);
        // Three tiles and a bit: 4 tiles from the tile line below the start.
        let u: Vec<f64> = pieces.iter().map(|p| span(p, 0).1 - span(p, 0).0).collect();
        assert!(
            (u[0] - 240.0).abs() < 1e-6 && (u[1] - 16.0).abs() < 1e-6,
            "{u:?}"
        );
        // Three whole tiles at a 16 texel phase fit; four do not.
        let three = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(3, 1),
            offset_texels: [16, 0],
            ..FaceUv::default()
        }))
        .remove(0);
        assert!(!wants_split(&three, true));
    }

    #[test]
    fn neighbouring_pieces_share_exact_corners() {
        let faces = [fit_face(3, 3), fit_face(6, 6)];
        for (face, repeat, expect) in [(&faces[0], false, 9), (&faces[1], true, 4)] {
            let pieces = split(face, UNIT, repeat);
            assert_eq!(pieces.len(), expect);
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
                        let within = (q[0] - a[0]) * (q[0] - b[0]) <= 0
                            && (q[2] - a[2]) * (q[2] - b[2]) <= 0;
                        assert!(!(cross == 0 && within), "{q:?} inside {a:?}-{b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_offset_that_straddles_a_boundary_cuts_there() {
        let face = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(1, 1),
            offset_texels: [16, 0],
            ..FaceUv::default()
        }))
        .remove(0);
        // One tile starting part way in touches two tiles: a plain primitive
        // cuts at the line, a repeating one holds both.
        let pieces = split(&face, UNIT, false);
        assert_eq!(pieces.len(), 2);
        let total: f64 = pieces.iter().map(area).sum();
        assert!((total - 1024.0 * 1024.0).abs() < 1.0);
        assert_eq!(split(&face, UNIT, true).len(), 1);
    }

    #[test]
    fn a_turned_free_rotation_still_cuts_into_convex_pieces() {
        let face = all_faces(&floor_with(FaceUv {
            mode: UvMode::fit(2, 2),
            rotation_deg: 30,
            ..FaceUv::default()
        }))
        .remove(0);
        for (repeat, at_least) in [(false, 4), (true, 1)] {
            let pieces = split(&face, UNIT, repeat);
            assert!(pieces.len() >= at_least, "{}", pieces.len());
            let total: f64 = pieces.iter().map(area).sum();
            assert!((total - 1024.0 * 1024.0).abs() < 8.0, "{total}");
            assert!(pieces.iter().all(|p| p.grid.len() <= 4));
        }
    }
}
