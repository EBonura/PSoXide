//! Test support: join the course cuts of walls back together.
//!
//! A wall of a room made before derived walls is cut into courses at
//! absolute multiples of one sector in height; a derived wall is one face.
//! Both look the same (the texel mapping is continuous across a course
//! line), so a comparison that wants "the same look" merges the courses of
//! both sides first.

use super::faces::{GridFace, GridFaceKind};

fn is_wall(face: &GridFace) -> bool {
    matches!(
        face.kind,
        GridFaceKind::Wall { .. } | GridFaceKind::Derived { .. }
    )
}

/// The ring's corners by height: `(bottom y, top y)` of a vertical
/// rectangle, or `None` when the face is not one.
fn rails(face: &GridFace) -> Option<(i32, i32)> {
    if face.grid.len() != 4 {
        return None;
    }
    let ys: Vec<i32> = face.grid.iter().map(|p| p[1]).collect();
    let (lo, hi) = (*ys.iter().min()?, *ys.iter().max()?);
    let at = |y: i32| ys.iter().filter(|v| **v == y).count();
    (lo != hi && at(lo) == 2 && at(hi) == 2).then_some((lo, hi))
}

fn xz(p: [i32; 3]) -> [i32; 2] {
    [p[0], p[2]]
}

/// The two horizontal-line points of `face` at height `y`, sorted.
fn edge_at(face: &GridFace, y: i32) -> Vec<[i32; 2]> {
    let mut e: Vec<[i32; 2]> = face
        .grid
        .iter()
        .filter(|p| p[1] == y)
        .map(|p| xz(*p))
        .collect();
    e.sort();
    e
}

/// Join every pair of vertically adjacent wall rectangles of one material
/// whose shared edge matches, until none is left.
pub(crate) fn merge_courses(mut faces: Vec<GridFace>) -> Vec<GridFace> {
    'again: loop {
        for i in 0..faces.len() {
            let Some((_, top)) = is_wall(&faces[i]).then(|| rails(&faces[i])).flatten() else {
                continue;
            };
            for j in 0..faces.len() {
                if i == j || !is_wall(&faces[j]) {
                    continue;
                }
                let Some((bottom_j, top_j)) = rails(&faces[j]) else {
                    continue;
                };
                let (a, b) = (&faces[i], &faces[j]);
                if bottom_j != top
                    || a.material != b.material
                    || a.double_sided != b.double_sided
                    || a.room != b.room
                    || (0..3).any(|k| (a.normal[k] - b.normal[k]).abs() > 1e-9)
                    || edge_at(a, top) != edge_at(b, bottom_j)
                {
                    continue;
                }
                // `a`'s top corners take `b`'s top corners and texels.
                let mut merged = a.clone();
                for k in 0..merged.grid.len() {
                    if merged.grid[k][1] != top {
                        continue;
                    }
                    let Some(m) = (0..b.grid.len())
                        .find(|&m| b.grid[m][1] == top_j && xz(b.grid[m]) == xz(merged.grid[k]))
                    else {
                        continue 'again;
                    };
                    merged.grid[k] = b.grid[m];
                    merged.points[k] = b.points[m];
                    merged.texels[k] = b.texels[m];
                }
                faces[i] = merged;
                faces.remove(j);
                continue 'again;
            }
        }
        return faces;
    }
}
