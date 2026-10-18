//! Splitting a floor or ceiling into the surfaces of its four children, and
//! joining four back when the parent represents them exactly.
//!
//! A surface is the **triangulated** quad of its corner heights, so the
//! children sample the two triangle planes of the parent. A child that
//! straddles the parent's diagonal keeps the parent's split and gets one
//! plane per triangle; a child on either side of it lies wholly inside one
//! parent triangle and is planar. Splitting therefore never changes the
//! surface, only how it is stored.
//!
//! Children are numbered in Morton order, north-west, north-east,
//! south-west, south-east (see [`LeafKey::children`](super::super::leaf::LeafKey)).
//! Heights are integers and midpoints round half up ([`half`]); a surface
//! whose corner heights are all even splits exactly, which every height the
//! tools write is (the fine step is 64).

use super::super::surface::{GridSplit, GridSurface, GridTri, VOID_A, VOID_B};

/// Corner indices (`[NW, NE, SE, SW]`) of child `c` in Morton order.
const fn child_cell(c: usize) -> (i32, i32) {
    ((c & 1) as i32, (c >> 1) as i32)
}

/// Midpoint of two heights, rounded half up. The one rule every consumer
/// uses (subdivide, hanging corners, stitching), so they agree to the unit.
pub const fn half(a: i32, b: i32) -> i32 {
    (a + b + 1).div_euclid(2)
}

/// Round `num / den` half up (`den > 0`).
pub const fn round_div(num: i64, den: i64) -> i64 {
    (2 * num + den).div_euclid(2 * den)
}

/// The parent triangle (`0` or `1`) that child `c`'s triangle `t` lies in.
/// A child on the parent's diagonal keeps its triangle numbers; the other
/// two lie wholly inside one parent triangle.
pub const fn parent_triangle(split: GridSplit, c: usize, t: usize) -> usize {
    match (split, c) {
        (GridSplit::NwSe, 0 | 3) | (GridSplit::NeSw, 1 | 2) => t,
        (GridSplit::NwSe, 1) | (GridSplit::NeSw, 0) => 0,
        _ => 1,
    }
}

/// Whether child `c` straddles the parent's diagonal.
pub const fn on_diagonal(split: GridSplit, c: usize) -> bool {
    matches!(
        (split, c),
        (GridSplit::NwSe, 0 | 3) | (GridSplit::NeSw, 1 | 2)
    )
}

/// Twice the height of triangle `t` of a quad with corner heights `h` and
/// diagonal `split`, at the half lattice point `(i, j)` (`0..=2` across and
/// down). The plane of the triangle, extended over the whole quad.
fn plane2(h: [i32; 4], split: GridSplit, t: usize, i: i32, j: i32) -> i64 {
    let [nw, ne, se, sw] = h.map(i64::from);
    let (i, j) = (i64::from(i), i64::from(j));
    match (split, t) {
        (GridSplit::NwSe, 0) => 2 * nw + i * (ne - nw) + j * (se - ne),
        (GridSplit::NwSe, _) => 2 * nw + i * (se - sw) + j * (sw - nw),
        (GridSplit::NeSw, 0) => 2 * nw + i * (ne - nw) + j * (sw - nw),
        (GridSplit::NeSw, _) => 2 * se + (2 - i) * (sw - se) + (2 - j) * (ne - se),
    }
}

/// Height of triangle `t`'s plane at the half lattice point `(i, j)`.
fn plane(h: [i32; 4], split: GridSplit, t: usize, i: i32, j: i32) -> i32 {
    round_div(plane2(h, split, t, i, j), 2) as i32
}

/// Height of the triangulated surface at the point `(dx, dz)` inside a leaf
/// of edge `n` (both in `0..=n`, in the same unit as `n`), rounded half up.
/// On the diagonal the two triangles agree for a continuous surface; a
/// split-level one answers with triangle 0.
pub fn height_in_leaf(s: &GridSurface, dx: i32, dz: i32, n: i32) -> i32 {
    let tri = match s.split {
        GridSplit::NwSe => usize::from(dx < dz),
        GridSplit::NeSw => usize::from(dx + dz > n),
    };
    let h = s.heights_for(tri).map(i64::from);
    let [nw, ne, se, sw] = h;
    let (x, z, n) = (i64::from(dx), i64::from(dz), i64::from(n));
    let num = match (s.split, tri) {
        (GridSplit::NwSe, 0) => n * nw + x * (ne - nw) + z * (se - ne),
        (GridSplit::NwSe, _) => n * nw + x * (se - sw) + z * (sw - nw),
        (GridSplit::NeSw, 0) => n * nw + x * (ne - nw) + z * (sw - nw),
        (GridSplit::NeSw, _) => n * se + (n - x) * (sw - se) + (n - z) * (ne - se),
    };
    round_div(num, n) as i32
}

/// Make the dead corner of each height set equal to the live one of the
/// other, so two equal surfaces compare equal whatever a tool left in the
/// corner a triangle does not use.
pub fn canonical(mut s: GridSurface) -> GridSurface {
    if let Some(hb) = s.heights_b {
        let own = s.split.triangle(0);
        let other = s.split.triangle(1);
        let mut a = s.heights;
        let mut b = hb;
        for k in 0..4 {
            if !own.contains(&k) {
                a[k] = hb[k];
            }
            if !other.contains(&k) {
                b[k] = a[k];
            }
        }
        s.heights = a;
        s.heights_b = Some(b);
    }
    s
}

/// The four child surfaces of `s`, Morton order.
pub fn split_surface(s: &GridSurface) -> [GridSurface; 4] {
    core::array::from_fn(|c| {
        let (cx, cz) = child_cell(c);
        // Corner points of the child in the parent's half lattice, in the
        // order NW, NE, SE, SW.
        let at = [(cx, cz), (cx + 1, cz), (cx + 1, cz + 1), (cx, cz + 1)];
        let sample = |t: usize, k: usize| {
            let p = parent_triangle(s.split, c, t);
            plane(s.heights_for(p), s.split, p, at[k].0, at[k].1)
        };
        let in0 = s.split.triangle(0);
        let in1 = s.split.triangle(1);
        let v0: [i32; 4] = core::array::from_fn(|k| sample(0, k));
        let v1: [i32; 4] = core::array::from_fn(|k| sample(1, k));
        // Corners both triangles use must agree for a single height set.
        let shared_agree = (0..4)
            .filter(|k| in0.contains(k) && in1.contains(k))
            .all(|k| v0[k] == v1[k]);
        let heights: [i32; 4] =
            core::array::from_fn(|k| if in0.contains(&k) { v0[k] } else { v1[k] });
        let heights_b = (!shared_agree)
            .then(|| core::array::from_fn(|k| if in1.contains(&k) { v1[k] } else { heights[k] }));
        let tri = |t: usize| *s.tri(parent_triangle(s.split, c, t));
        let (a, b) = (tri(0), tri(1));
        let mut void = 0;
        for t in 0..2 {
            if s.is_void(parent_triangle(s.split, c, t)) {
                void |= if t == 0 { VOID_A } else { VOID_B };
            }
        }
        canonical(GridSurface {
            heights,
            split: s.split,
            heights_b,
            tri_a: a,
            tri_b: (a != b).then_some(b),
            walkable: s.walkable,
            auto_split: s.auto_split,
            paint_locked: s.paint_locked,
            void,
        })
    })
}

/// The parent of four child surfaces, if splitting it gives exactly them.
/// `None` when the children differ in anything the parent could not hold:
/// the split, a material, a plane that is not the parent's.
pub fn join_surface(children: [&GridSurface; 4]) -> Option<GridSurface> {
    let split = children[0].split;
    if children.iter().any(|c| c.split != split) {
        return None;
    }
    // Each parent corner is a corner of exactly one child. The height of the
    // triangle that uses it comes from that child's triangle on that side.
    // Corner k of the parent is corner `own` of child `owner`.
    const OWNER: [(usize, usize); 4] = [(0, 0), (1, 1), (3, 2), (2, 3)];
    let mut heights = [0; 4];
    let mut heights_b = [0; 4];
    for k in 0..4 {
        let (child, corner) = OWNER[k];
        heights[k] = children[child].heights_for(0)[corner];
        heights_b[k] = children[child].heights_for(1)[corner];
    }
    // A corner only one triangle uses is written by the child's triangle
    // that has it; take it from the matching height set.
    let in0 = split.triangle(0);
    let in1 = split.triangle(1);
    let mut a = [0; 4];
    let mut b = [0; 4];
    for k in 0..4 {
        a[k] = if in0.contains(&k) {
            heights[k]
        } else {
            heights_b[k]
        };
        b[k] = if in1.contains(&k) { heights_b[k] } else { a[k] };
    }
    let first = children[0];
    let mut void = 0;
    // Triangle t of the parent is void when the children inside it are.
    // Read it from a child that lies wholly in that triangle.
    let probe = |t: usize| -> usize {
        (0..4)
            .find(|c| parent_triangle(split, *c, 0) == t && !on_diagonal(split, *c))
            .unwrap_or(0)
    };
    for t in 0..2 {
        if children[probe(t)].is_void(0) {
            void |= if t == 0 { VOID_A } else { VOID_B };
        }
    }
    let tri_of = |t: usize| -> GridTri { *children[probe(t)].tri(0) };
    let (ta, tb) = (tri_of(0), tri_of(1));
    let planar_single = a == b;
    let candidate = canonical(GridSurface {
        heights: a,
        split,
        heights_b: (!planar_single).then_some(b),
        tri_a: ta,
        tri_b: (ta != tb).then_some(tb),
        walkable: first.walkable,
        auto_split: first.auto_split,
        paint_locked: first.paint_locked,
        void,
    });
    // The only test: splitting the candidate reproduces the children.
    let back = split_surface(&candidate);
    let same = (0..4).all(|c| back[c] == canonical(children[c].clone()));
    same.then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surf(h: [i32; 4], split: GridSplit) -> GridSurface {
        GridSurface {
            split,
            heights: h,
            ..GridSurface::flat(0, None)
        }
    }

    #[test]
    fn midpoints_round_half_up() {
        assert_eq!(half(0, 64), 32);
        assert_eq!(half(1, 2), 2);
        assert_eq!(half(-1, -2), -1);
        assert_eq!(round_div(3, 2), 2);
        assert_eq!(round_div(-3, 2), -1);
    }

    #[test]
    fn a_planar_surface_splits_into_four_planar_children() {
        let s = surf([0, 128, 256, 128], GridSplit::NwSe);
        let c = split_surface(&s);
        assert_eq!(c[0].heights, [0, 64, 128, 64]);
        assert_eq!(c[1].heights, [64, 128, 192, 128]);
        assert_eq!(c[2].heights, [64, 128, 192, 128]);
        assert_eq!(c[3].heights, [128, 192, 256, 192]);
        assert!(c.iter().all(|c| c.heights_b.is_none() && c.is_planar()));
    }

    #[test]
    fn a_folded_surface_keeps_its_fold_on_the_diagonal_children() {
        // NW 0, NE 0, SE 256, SW 0: folded along NW-SE.
        let s = surf([0, 0, 256, 0], GridSplit::NwSe);
        let c = split_surface(&s);
        // The diagonal children are folded like the parent, the others flat
        // or sloped in one plane.
        assert!(!c[0].is_planar());
        assert!(!c[3].is_planar());
        assert!(c[1].is_planar());
        assert!(c[2].is_planar());
        // The centre is the middle of the diagonal.
        assert_eq!(c[0].heights[2], 128);
        assert_eq!(c[3].heights[0], 128);
    }

    #[test]
    fn split_then_join_is_the_identity_for_planar_and_folded_surfaces() {
        for split in [GridSplit::NwSe, GridSplit::NeSw] {
            for h in [
                [0, 0, 0, 0],
                [0, 128, 256, 128],
                [64, 64, 192, 192],
                [0, 0, 256, 0],
                [256, 0, 0, 256],
                [128, 0, 0, 0],
            ] {
                let s = surf(h, split);
                let c = split_surface(&s);
                let back = join_surface([&c[0], &c[1], &c[2], &c[3]]).unwrap();
                assert_eq!(back, canonical(s), "{split:?} {h:?}");
            }
        }
    }

    #[test]
    fn a_split_level_surface_with_a_void_triangle_round_trips() {
        let mut s = surf([0, 0, 0, 0], GridSplit::NwSe);
        s.heights_b = Some([0, 0, 256, 256]);
        s.set_void(1, true);
        let c = split_surface(&s);
        // The south-west child lies wholly inside the void triangle.
        assert_eq!(c[2].void, VOID_A | VOID_B);
        let back = join_surface([&c[0], &c[1], &c[2], &c[3]]).unwrap();
        assert_eq!(back, canonical(s));
    }

    #[test]
    fn children_that_are_not_one_plane_do_not_join() {
        let s = surf([0, 128, 256, 128], GridSplit::NwSe);
        let mut c = split_surface(&s);
        c[1].heights[1] += 64;
        assert!(join_surface([&c[0], &c[1], &c[2], &c[3]]).is_none());
    }

    #[test]
    fn different_materials_on_the_children_do_not_join() {
        let s = surf([0; 4], GridSplit::NwSe);
        let mut c = split_surface(&s);
        c[3].tri_a.material = Some(ron::from_str::<crate::ResourceId>("(7)").unwrap());
        assert!(join_surface([&c[0], &c[1], &c[2], &c[3]]).is_none());
    }
}
