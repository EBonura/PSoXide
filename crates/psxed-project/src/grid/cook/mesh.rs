//! Is the cooked room surface watertight? An edge a closed room's quads do
//! not share with exactly one other quad is a place the GPU can show the
//! void: a crack where two faces meet with different corners (a
//! T-junction), a riser the derivation left out, a bow-tie that draws as one
//! triangle.
//!
//! The check reads the cooked tables, so it judges what Play draws, not what
//! the editor meant.

use std::collections::BTreeMap;

use psx_grid::write::RoomChunk;
use psx_grid::{QUAD_PLANE, QUAD_TRI};

/// An edge of the cooked surface that does not meet exactly one other quad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenEdge {
    /// Room-local end points.
    pub a: [i16; 3],
    pub b: [i16; 3],
    /// Quads using the edge (1 is a boundary, 3 or more an overlap).
    pub uses: usize,
    /// Another vertex of the room lies strictly inside the segment: the
    /// neighbour was split there and this edge was not (a T-junction).
    pub t_junction: bool,
    /// Part of the edge has no other open or shared edge along it: the surface ends
    /// here with nothing beside it, a hole the GPU shows the void through.
    /// False when the open edges beside it cover all of it (a T-junction
    /// chain: no hole, at worst a sub-pixel seam).
    pub hole: bool,
    /// First quad using it.
    pub quad: usize,
}

fn position(chunk: &RoomChunk, v: u16) -> [i16; 3] {
    let v = chunk.vertices[usize::from(v)];
    [v.x, v.y, v.z]
}

/// Whether `p` lies strictly inside the segment `a`-`b`, collinear exactly.
fn strictly_inside(a: [i16; 3], b: [i16; 3], p: [i16; 3]) -> bool {
    let (a, b, p) = (a.map(i64::from), b.map(i64::from), p.map(i64::from));
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let e = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let cross = [
        d[1] * e[2] - d[2] * e[1],
        d[2] * e[0] - d[0] * e[2],
        d[0] * e[1] - d[1] * e[0],
    ];
    if cross != [0, 0, 0] {
        return false;
    }
    let dot = d[0] * e[0] + d[1] * e[1] + d[2] * e[2];
    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    dot > 0 && dot < len2
}

/// Every edge of the room's quads (free planes excluded: they are loose
/// sheets by design) that is not shared by exactly two quads.
pub fn open_edges(chunk: &RoomChunk) -> Vec<OpenEdge> {
    let mut edges: BTreeMap<(u16, u16), (usize, usize)> = BTreeMap::new();
    for (index, quad) in chunk.quads.iter().enumerate() {
        if quad.flags & QUAD_PLANE != 0 {
            continue;
        }
        let corners = if quad.flags & QUAD_TRI != 0 { 3 } else { 4 };
        for k in 0..corners {
            let (a, b) = (quad.v[k], quad.v[(k + 1) % corners]);
            if a == b {
                continue;
            }
            let entry = edges.entry((a.min(b), a.max(b))).or_insert((0, index));
            entry.0 += 1;
        }
    }
    let mut open = Vec::new();
    for (&(a, b), &(uses, quad)) in &edges {
        if uses == 2 {
            continue;
        }
        let (pa, pb) = (position(chunk, a), position(chunk, b));
        let t_junction = chunk
            .vertices
            .iter()
            .any(|v| strictly_inside(pa, pb, [v.x, v.y, v.z]));
        open.push(OpenEdge {
            a: pa,
            b: pb,
            uses,
            t_junction,
            hole: false,
            quad,
        });
    }
    // An open edge is a hole where no other open edge lies along it.
    let boundary: Vec<usize> = (0..open.len()).filter(|&i| open[i].uses == 1).collect();
    for &i in &boundary {
        let (a, b) = (open[i].a.map(i64::from), open[i].b.map(i64::from));
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        // Intervals of the edge (as dot products from `a`) another open
        // edge runs along.
        let mut spans: Vec<(i64, i64)> = Vec::new();
        // Edges three or more quads share (the walls round a saddle corner
        // all meet on one vertical edge) cover a boundary edge as well.
        for (j, other) in open.iter().enumerate() {
            if j == i {
                continue;
            }
            let (c, e) = (other.a.map(i64::from), other.b.map(i64::from));
            // Within a runtime unit of the line counts as along it: a split
            // vertex rounded to the lattice is not exactly on the long edge
            // but nothing the GPU draws can open between them.
            let collinear = |p: [i64; 3]| {
                let w = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
                let cross = [
                    d[1] * w[2] - d[2] * w[1],
                    d[2] * w[0] - d[0] * w[2],
                    d[0] * w[1] - d[1] * w[0],
                ];
                let c2 = cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2];
                // distance^2 = |cross|^2 / |d|^2 <= 1
                c2 <= len2
            };
            if !collinear(c) || !collinear(e) {
                continue;
            }
            let t =
                |p: [i64; 3]| d[0] * (p[0] - a[0]) + d[1] * (p[1] - a[1]) + d[2] * (p[2] - a[2]);
            let (t0, t1) = (t(c).min(t(e)), t(c).max(t(e)));
            if t1 > 0 && t0 < len2 {
                spans.push((t0.max(0), t1.min(len2)));
            }
        }
        spans.sort_unstable();
        let mut reach = 0i64;
        for (t0, t1) in spans {
            if t0 > reach {
                break;
            }
            reach = reach.max(t1);
        }
        open[i].hole = reach < len2;
    }
    open
}
