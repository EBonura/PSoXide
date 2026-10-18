//! Splitting a leaf into four children and joining four back.
//!
//! Splitting is lossless: the children carry the same surfaces (see
//! [`super::surface_split`]) and each outer-edge wall is cut at its middle,
//! the first half going to the child at the wall's `a` end. Joining is the
//! inverse and only succeeds when splitting the result gives the four
//! children back exactly, so a merge never changes what the document
//! describes.

use super::super::sector::{GridSector, GridSpan, WallEdge};
use super::super::surface::GridWall;
use super::surface_split::{half, join_surface, split_surface};

/// The children (Morton order) holding the first and the second half of a
/// wall on `edge`, walking `a -> b`.
pub const fn edge_children(edge: WallEdge) -> [usize; 2] {
    match edge {
        WallEdge::North => [0, 1],
        WallEdge::East => [1, 3],
        WallEdge::South => [3, 2],
        WallEdge::West => [2, 0],
        WallEdge::Nwse => [0, 3],
        WallEdge::Nesw => [1, 2],
    }
}

/// The two halves of a wall, `a -> mid` and `mid -> b`.
pub fn split_wall(wall: &GridWall) -> [GridWall; 2] {
    let [ba, bb, tb, ta] = wall.heights;
    let (bm, tm) = (half(ba, bb), half(ta, tb));
    [
        GridWall {
            heights: [ba, bm, tm, ta],
            ..wall.clone()
        },
        GridWall {
            heights: [bm, bb, tb, tm],
            ..wall.clone()
        },
    ]
}

/// The wall whose halves are `first` and `second`, if there is one.
pub fn join_wall(first: &GridWall, second: &GridWall) -> Option<GridWall> {
    let joined = GridWall {
        heights: [
            first.heights[0],
            second.heights[1],
            second.heights[2],
            first.heights[3],
        ],
        ..first.clone()
    };
    let [a, b] = split_wall(&joined);
    (a == *first && b == *second).then_some(joined)
}

fn edge_walls(sector: &GridSector, edge: WallEdge) -> &[GridWall] {
    sector.walls(edge)
}

/// The four children of `sector`, Morton order. The walls of the parent's
/// edges are cut between the two children on that edge; the walls the
/// parent has on a diagonal go to the two children the diagonal crosses.
pub fn split_sector(sector: &GridSector) -> [GridSector; 4] {
    let mut spans: [Vec<GridSpan>; 4] = Default::default();
    for span in sector.spans() {
        let floors = span.floor.as_ref().map(split_surface);
        let ceilings = span.ceiling.as_ref().map(split_surface);
        for c in 0..4 {
            spans[c].push(GridSpan {
                floor: floors.as_ref().map(|f| f[c].clone()),
                ceiling: ceilings.as_ref().map(|f| f[c].clone()),
                wall_material: span.wall_material,
            });
        }
    }
    let mut out: [GridSector; 4] = core::array::from_fn(|c| {
        let mut child = GridSector::default();
        for (i, span) in core::mem::take(&mut spans[c]).into_iter().enumerate() {
            if i == 0 {
                child.spans_mut()[0] = span;
            } else {
                child.insert_span(i, span);
            }
        }
        child.rough = sector.rough;
        child
    });
    for edge in WallEdge::ALL {
        let [first, second] = edge_children(edge);
        for wall in edge_walls(sector, edge) {
            let [a, b] = split_wall(wall);
            out[first].walls_mut(edge).push(a);
            out[second].walls_mut(edge).push(b);
        }
    }
    out
}

/// Why four leaves cannot become one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinBlock {
    /// The children do not have the same number of spans.
    Spans,
    /// A floor or ceiling is not what splitting the parent would give:
    /// not one plane, or different materials, split or flags.
    Surface,
    /// A wall on an edge between two children, or a wall that is not the
    /// two halves of one wall on the parent's edge.
    Wall,
    /// The children differ in roughness.
    Rough,
}

impl JoinBlock {
    /// A short phrase for the status bar.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Spans => "the cells have different storeys",
            Self::Surface => "the surfaces are not one plane with one look",
            Self::Wall => "a wall splits the cells",
            Self::Rough => "the cells differ in roughness",
        }
    }
}

/// The parent of four children (Morton order), if splitting it gives exactly
/// them.
pub fn join_sector(children: [&GridSector; 4]) -> Result<GridSector, JoinBlock> {
    let count = children[0].span_count();
    if children.iter().any(|c| c.span_count() != count) {
        return Err(JoinBlock::Spans);
    }
    if children.iter().any(|c| c.rough != children[0].rough) {
        return Err(JoinBlock::Rough);
    }
    let mut parent = GridSector::default();
    for i in 0..count {
        let spans: [&GridSpan; 4] = core::array::from_fn(|c| &children[c].spans()[i]);
        if spans
            .iter()
            .any(|s| s.wall_material != spans[0].wall_material)
        {
            return Err(JoinBlock::Surface);
        }
        let surface = |pick: fn(&GridSpan) -> &Option<super::super::surface::GridSurface>| match (
            pick(spans[0]),
            pick(spans[1]),
            pick(spans[2]),
            pick(spans[3]),
        ) {
            (None, None, None, None) => Ok(None),
            (Some(a), Some(b), Some(c), Some(d)) => join_surface([a, b, c, d])
                .map(Some)
                .ok_or(JoinBlock::Surface),
            _ => Err(JoinBlock::Surface),
        };
        let span = GridSpan {
            floor: surface(|s| &s.floor)?,
            ceiling: surface(|s| &s.ceiling)?,
            wall_material: spans[0].wall_material,
        };
        if i == 0 {
            parent.spans_mut()[0] = span;
        } else {
            parent.insert_span(i, span);
        }
    }
    parent.rough = children[0].rough;
    // Walls: the two children on each parent edge must hold the two halves
    // of the same walls; every other edge must hold none.
    let mut used = [[false; 6]; 4];
    for (e, edge) in WallEdge::ALL.into_iter().enumerate() {
        let [first, second] = edge_children(edge);
        used[first][e] = true;
        used[second][e] = true;
        let (a, b) = (
            edge_walls(children[first], edge),
            edge_walls(children[second], edge),
        );
        if a.len() != b.len() {
            return Err(JoinBlock::Wall);
        }
        for (wa, wb) in a.iter().zip(b) {
            parent
                .walls_mut(edge)
                .push(join_wall(wa, wb).ok_or(JoinBlock::Wall)?);
        }
    }
    for (c, child) in children.iter().enumerate() {
        for (e, edge) in WallEdge::ALL.into_iter().enumerate() {
            if !used[c][e] && !edge_walls(child, edge).is_empty() {
                return Err(JoinBlock::Wall);
            }
        }
    }
    Ok(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::GridSurface;

    fn floor_sector(h: [i32; 4]) -> GridSector {
        let mut s = GridSector::with_surfaces(Some(GridSurface::flat(0, None)), None);
        s.ground_floor_mut().unwrap().heights = h;
        s
    }

    #[test]
    fn a_wall_splits_at_its_middle_and_joins_back() {
        let wall = GridWall {
            heights: [0, 128, 512, 256],
            ..GridWall::span(0, 512, None)
        };
        let [a, b] = split_wall(&wall);
        assert_eq!(a.heights, [0, 64, 384, 256]);
        assert_eq!(b.heights, [64, 128, 512, 384]);
        assert_eq!(join_wall(&a, &b), Some(wall));
        // Halves from different walls do not join.
        let mut other = b.clone();
        other.heights[0] += 64;
        assert_eq!(join_wall(&a, &other), None);
    }

    #[test]
    fn walls_follow_their_edge_into_the_right_children() {
        let mut parent = floor_sector([0; 4]);
        parent.east.push(GridWall::span(0, 512, None));
        parent.nwse.push(GridWall::span(0, 256, None));
        let c = split_sector(&parent);
        // East edge, a = NE, b = SE: halves in NE then SE.
        assert_eq!(c[1].east.len(), 1);
        assert_eq!(c[3].east.len(), 1);
        assert!(c[0].east.is_empty() && c[2].east.is_empty());
        // The NW-SE diagonal crosses the NW and SE children.
        assert_eq!((c[0].nwse.len(), c[3].nwse.len()), (1, 1));
        assert!(c[1].nwse.is_empty() && c[2].nwse.is_empty());
        let back = join_sector([&c[0], &c[1], &c[2], &c[3]]).unwrap();
        assert_eq!(back, parent);
    }

    #[test]
    fn a_wall_between_the_children_blocks_the_join() {
        let parent = floor_sector([0, 128, 256, 128]);
        let mut c = split_sector(&parent);
        c[0].east.push(GridWall::span(0, 256, None));
        assert_eq!(
            join_sector([&c[0], &c[1], &c[2], &c[3]]),
            Err(JoinBlock::Wall)
        );
    }

    #[test]
    fn every_span_of_a_column_is_split() {
        let mut parent = floor_sector([0; 4]);
        parent.insert_span(
            1,
            GridSpan {
                floor: Some(GridSurface::flat(1024, None)),
                ceiling: Some(GridSurface::flat(1536, None)),
                wall_material: None,
            },
        );
        let c = split_sector(&parent);
        assert!(c.iter().all(|c| c.span_count() == 2));
        assert_eq!(c[3].spans()[1].floor.as_ref().unwrap().heights, [1024; 4]);
        let back = join_sector([&c[0], &c[1], &c[2], &c[3]]).unwrap();
        assert_eq!(back, parent);
    }
}
