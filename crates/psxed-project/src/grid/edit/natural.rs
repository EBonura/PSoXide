//! Natural-terrain edits: cracks (a surface whose two triangles sit at
//! different heights), void triangles, and direct wall heights.
//!
//! These are the primitives the Break verb, the outline tools and the MCP all
//! write with; each is a plain function over a [`GridWorld`] that changes
//! ordinary document fields and nothing else.

use super::super::sector::{GridSector, WallEdge};
use super::super::surface::{GridSurface, VOID_A, VOID_B};
use super::super::world::GridWorld;
use super::refs::SurfaceKind;

/// Which triangles an edit touches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Triangles {
    /// The first triangle.
    A,
    /// The second triangle.
    B,
    /// Both.
    Both,
}

impl Triangles {
    /// The triangle indices.
    pub fn indices(self) -> &'static [usize] {
        match self {
            Self::A => &[0],
            Self::B => &[1],
            Self::Both => &[0, 1],
        }
    }

    /// The matching [`GridSurface::void`] bits.
    pub const fn bits(self) -> u8 {
        match self {
            Self::A => VOID_A,
            Self::B => VOID_B,
            Self::Both => VOID_A | VOID_B,
        }
    }
}

fn surface_mut(sector: &mut GridSector, kind: SurfaceKind) -> Option<&mut GridSurface> {
    match kind {
        SurfaceKind::Floor => sector.floor.as_mut(),
        SurfaceKind::Ceiling => sector.ceiling.as_mut(),
    }
}

fn sector_mut(world: &mut GridWorld, room: usize, cell: [i32; 2]) -> Option<&mut GridSector> {
    let r = world.rooms.get_mut(room)?;
    let [x, z] = r.local(cell)?;
    r.sector_mut(x, z)
}

/// Make `surface` split-level: its second triangle gets its own heights,
/// a copy of the first's. Returns whether it changed.
pub fn make_split_level(surface: &mut GridSurface) -> bool {
    if surface.heights_b.is_some() {
        return false;
    }
    surface.heights_b = Some(surface.heights);
    // A split-level surface keeps the diagonal it has: the heights of the two
    // triangles no longer decide it.
    surface.auto_split = false;
    true
}

/// Move triangle `tri` of the surface by `delta` grid units, making the
/// surface split-level first. Returns whether anything changed.
pub fn crack(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    kind: SurfaceKind,
    tri: usize,
    delta: i32,
) -> bool {
    let Some(surface) = sector_mut(world, room, cell).and_then(|s| surface_mut(s, kind)) else {
        return false;
    };
    if delta == 0 {
        return false;
    }
    make_split_level(surface);
    let heights = if tri == 0 {
        &mut surface.heights
    } else {
        surface.heights_b.get_or_insert(surface.heights)
    };
    for h in heights.iter_mut() {
        *h = h.saturating_add(delta);
    }
    true
}

/// Set triangle `tri` of the surface to the absolute heights `heights`,
/// making it split-level first.
pub fn set_triangle_heights(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    kind: SurfaceKind,
    tri: usize,
    heights: [i32; 4],
) -> bool {
    let Some(surface) = sector_mut(world, room, cell).and_then(|s| surface_mut(s, kind)) else {
        return false;
    };
    make_split_level(surface);
    let slot = if tri == 0 {
        &mut surface.heights
    } else {
        surface.heights_b.get_or_insert(surface.heights)
    };
    let changed = *slot != heights;
    *slot = heights;
    changed
}

/// Merge a split-level surface back into one plane (the first triangle's
/// heights win). Returns whether it changed.
pub fn heal(world: &mut GridWorld, room: usize, cell: [i32; 2], kind: SurfaceKind) -> bool {
    let Some(surface) = sector_mut(world, room, cell).and_then(|s| surface_mut(s, kind)) else {
        return false;
    };
    surface.heights_b.take().is_some()
}

/// Void or restore triangles of a surface. Returns whether anything changed.
pub fn set_void(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    kind: SurfaceKind,
    which: Triangles,
    void: bool,
) -> bool {
    let Some(surface) = sector_mut(world, room, cell).and_then(|s| surface_mut(s, kind)) else {
        return false;
    };
    let before = surface.void;
    if void {
        surface.void |= which.bits();
    } else {
        surface.void &= !which.bits();
    }
    surface.void != before
}

/// Set the four heights `[bottom a, bottom b, top b, top a]` of authored wall
/// `index` on `edge` of `cell`. Returns whether it changed.
pub fn set_wall_heights(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    edge: WallEdge,
    index: usize,
    heights: [i32; 4],
) -> bool {
    let Some(wall) = sector_mut(world, room, cell).and_then(|s| s.walls_mut(edge).get_mut(index))
    else {
        return false;
    };
    let changed = wall.heights != heights;
    wall.heights = heights;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridWall;
    use crate::grid::units::GridScale;

    fn world() -> GridWorld {
        let mut room = GridRoom::new_derived("r", 2, 1);
        for x in 0..2 {
            let s = room.sector_or_insert(x, 0).unwrap();
            s.floor = Some(GridSurface::flat_auto(0, None));
            s.ceiling = Some(GridSurface::flat_auto(1024, None));
        }
        room.sector_or_insert(0, 0)
            .unwrap()
            .north
            .push(GridWall::span(0, 512, None));
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn floor(w: &GridWorld, x: usize) -> &GridSurface {
        w.rooms[0].sector(x, 0).unwrap().floor.as_ref().unwrap()
    }

    #[test]
    fn a_crack_makes_the_second_triangle_its_own_level() {
        let mut w = world();
        assert!(crack(&mut w, 0, [0, 0], SurfaceKind::Floor, 1, 256));
        let f = floor(&w, 0);
        assert_eq!(f.heights, [0; 4]);
        assert_eq!(f.heights_b, Some([256; 4]));
        assert!(!f.auto_split, "a split-level surface keeps its diagonal");
        // Cracking the first triangle moves only the first.
        assert!(crack(&mut w, 0, [0, 0], SurfaceKind::Floor, 0, 64));
        assert_eq!(floor(&w, 0).heights, [64; 4]);
        assert_eq!(floor(&w, 0).heights_b, Some([256; 4]));
        // A zero crack, a missing surface and a missing cell change nothing.
        assert!(!crack(&mut w, 0, [0, 0], SurfaceKind::Floor, 1, 0));
        assert!(!crack(&mut w, 0, [5, 5], SurfaceKind::Floor, 1, 64));
        assert!(heal(&mut w, 0, [0, 0], SurfaceKind::Floor));
        assert_eq!(floor(&w, 0).heights_b, None);
    }

    #[test]
    fn voiding_and_restoring_triangles() {
        let mut w = world();
        assert!(set_void(
            &mut w,
            0,
            [1, 0],
            SurfaceKind::Floor,
            Triangles::A,
            true
        ));
        assert_eq!(floor(&w, 1).void, VOID_A);
        assert!(!set_void(
            &mut w,
            0,
            [1, 0],
            SurfaceKind::Floor,
            Triangles::A,
            true
        ));
        assert!(set_void(
            &mut w,
            0,
            [1, 0],
            SurfaceKind::Floor,
            Triangles::Both,
            true
        ));
        assert_eq!(floor(&w, 1).void, VOID_A | VOID_B);
        assert!(set_void(
            &mut w,
            0,
            [1, 0],
            SurfaceKind::Floor,
            Triangles::Both,
            false
        ));
        assert_eq!(floor(&w, 1).void, 0);
        // The document does not grow a field when nothing is void.
        let text = ron::to_string(floor(&w, 1)).unwrap();
        assert!(!text.contains("void"), "{text}");
    }

    #[test]
    fn wall_heights_are_set_directly() {
        let mut w = world();
        assert!(set_wall_heights(
            &mut w,
            0,
            [0, 0],
            WallEdge::North,
            0,
            [0, 128, 512, 256]
        ));
        assert_eq!(
            w.rooms[0].sector(0, 0).unwrap().north[0].heights,
            [0, 128, 512, 256]
        );
        assert!(!set_wall_heights(
            &mut w,
            0,
            [0, 0],
            WallEdge::North,
            3,
            [0; 4]
        ));
    }
}
