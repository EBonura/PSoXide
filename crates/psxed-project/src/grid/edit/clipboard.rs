//! Copying a selection and pasting it elsewhere, flipped or turned.
//!
//! The clipboard holds the selected parts of each sector at offsets from the
//! selection's smallest corner. Mirroring and turning rewrite everything
//! that has a direction: corner heights, diagonal splits, wall edges and the
//! side a wall faces. Heights are kept as authored, room-relative.

use std::collections::BTreeMap;

use super::super::limits::MAX_WALLS_PER_EDGE;
use super::super::sector::{GridSector, WallEdge};
use super::super::surface::{GridSplit, GridSurface, GridWall, WallFacing, VOID_A};
use super::super::world::GridWorld;
use super::rects::{grow_room_to, CellRect, RectRefusal};
use super::refs::{FaceRef, FaceTarget, SurfaceKind};

/// A reflection or quarter turn of the sector grid, as seen from above with
/// north up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    /// Mirror east to west.
    FlipH,
    /// Mirror north to south.
    FlipV,
    /// Turn 90 degrees clockwise.
    RotateCw,
}

impl Transform {
    /// Map a corner position inside a sector, `[u, v]` each `0` or `1`.
    fn corner(self, p: [i32; 2]) -> [i32; 2] {
        match self {
            Self::FlipH => [1 - p[0], p[1]],
            Self::FlipV => [p[0], 1 - p[1]],
            Self::RotateCw => [1 - p[1], p[0]],
        }
    }

    /// Map a direction vector.
    fn direction(self, d: [i32; 2]) -> [i32; 2] {
        match self {
            Self::FlipH => [-d[0], d[1]],
            Self::FlipV => [d[0], -d[1]],
            Self::RotateCw => [-d[1], d[0]],
        }
    }

    /// Map a sector offset inside a clip of `size` sectors.
    fn cell(self, p: [i32; 2], size: [i32; 2]) -> [i32; 2] {
        match self {
            Self::FlipH => [size[0] - 1 - p[0], p[1]],
            Self::FlipV => [p[0], size[1] - 1 - p[1]],
            Self::RotateCw => [size[1] - 1 - p[1], p[0]],
        }
    }

    fn size(self, size: [i32; 2]) -> [i32; 2] {
        match self {
            Self::RotateCw => [size[1], size[0]],
            _ => size,
        }
    }
}

const SURFACE_CORNERS: [[i32; 2]; 4] = [[0, 0], [1, 0], [1, 1], [0, 1]];

fn corner_index(p: [i32; 2]) -> usize {
    SURFACE_CORNERS
        .iter()
        .position(|c| *c == p)
        .expect("corner positions are 0 or 1")
}

fn transform_surface(surface: &GridSurface, t: Transform) -> GridSurface {
    let perm: [usize; 4] = core::array::from_fn(|i| corner_index(t.corner(SURFACE_CORNERS[i])));
    let remap = |h: [i32; 4]| {
        let mut out = [0; 4];
        for i in 0..4 {
            out[perm[i]] = h[i];
        }
        out
    };
    // The diagonal's end points decide the new split.
    let [d0, d1] = match surface.split {
        GridSplit::NwSe => [0usize, 2],
        GridSplit::NeSw => [1, 3],
    };
    let ends = [perm[d0], perm[d1]];
    let split = if ends.contains(&0) {
        GridSplit::NwSe
    } else {
        GridSplit::NeSw
    };
    // Which new triangle each old triangle became.
    let new_tri = |old: usize| -> usize {
        let mut mapped: Vec<usize> = surface
            .split
            .triangle(old)
            .into_iter()
            .map(|c| perm[c])
            .collect();
        mapped.sort_unstable();
        (0..2)
            .find(|k| {
                let mut corners = split.triangle(*k).to_vec();
                corners.sort_unstable();
                corners == mapped
            })
            .unwrap_or(old)
    };
    let swapped = new_tri(0) == 1;
    let mut out = surface.clone();
    out.split = split;
    out.heights = remap(surface.heights);
    out.heights_b = surface.heights_b.map(remap);
    if swapped {
        // The old first triangle is now the second: trade the records.
        out.tri_a = *surface.tri(1);
        out.tri_b = Some(surface.tri_a);
        if let Some(b) = out.heights_b {
            out.heights_b = Some(out.heights);
            out.heights = b;
        }
    }
    if out.tri_b == Some(out.tri_a) {
        out.tri_b = None;
    }
    // A void triangle goes with its corners.
    out.void = (0..2)
        .filter(|old| surface.is_void(*old))
        .fold(0, |bits, old| bits | (VOID_A << new_tri(old)));
    out
}

fn edge_with_endpoints(a: [i32; 2], b: [i32; 2]) -> WallEdge {
    WallEdge::ALL
        .into_iter()
        .find(|e| {
            let (p, q) = e.endpoints();
            (p == a && q == b) || (p == b && q == a)
        })
        .expect("a transformed edge is an edge")
}

fn transform_wall(edge: WallEdge, wall: &GridWall, t: Transform) -> (WallEdge, GridWall) {
    let (a, b) = edge.endpoints();
    let (na, nb) = (t.corner(a), t.corner(b));
    let target = edge_with_endpoints(na, nb);
    let mut out = wall.clone();
    if target.endpoints() != (na, nb) {
        // The walking direction reversed: swap the a and b ends.
        let h = wall.heights;
        out.heights = [h[1], h[0], h[3], h[2]];
    }
    if t.direction(edge.front_direction()) != target.front_direction() {
        out.facing = match wall.facing {
            WallFacing::Front => WallFacing::Back,
            WallFacing::Back => WallFacing::Front,
            WallFacing::Both => WallFacing::Both,
        };
    }
    (target, out)
}

fn transform_sector(sector: &GridSector, t: Transform) -> GridSector {
    let mut out = GridSector {
        floor: sector.floor.as_ref().map(|s| transform_surface(s, t)),
        ceiling: sector.ceiling.as_ref().map(|s| transform_surface(s, t)),
        ..GridSector::default()
    };
    for edge in WallEdge::ALL {
        for wall in sector.walls(edge) {
            let (target, moved) = transform_wall(edge, wall, t);
            out.walls_mut(target).push(moved);
        }
    }
    out
}

/// Parts of sectors, ready to paste.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GridClipboard {
    /// Sector offsets from the clip's smallest corner, `[x, z]`, with the
    /// copied parts of each sector.
    items: BTreeMap<[i32; 2], GridSector>,
    /// Extent in sectors, `[width, depth]`.
    size: [i32; 2],
}

/// What a paste did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PasteOutcome {
    /// Faces that now exist where the clip landed.
    pub faces: Vec<FaceRef>,
    /// Set when the room would have outgrown its cap and nothing changed.
    pub refused: Option<RectRefusal>,
}

impl GridClipboard {
    /// Copy `faces` (all in `room`). `None` when nothing could be copied.
    pub fn capture(world: &GridWorld, faces: &[FaceRef]) -> Option<Self> {
        let room = faces.first()?.room;
        let faces: Vec<&FaceRef> = faces
            .iter()
            .filter(|f| f.room == room && f.exists(world))
            .collect();
        let min = [
            faces.iter().map(|f| f.cell[0]).min()?,
            faces.iter().map(|f| f.cell[1]).min()?,
        ];
        let max = [
            faces.iter().map(|f| f.cell[0]).max()?,
            faces.iter().map(|f| f.cell[1]).max()?,
        ];
        let mut items: BTreeMap<[i32; 2], GridSector> = BTreeMap::new();
        // Walls are copied in index order so a stack keeps its order.
        let mut ordered = faces.clone();
        ordered.sort();
        for face in ordered {
            let source = face.sector(world)?;
            let slot = items
                .entry([face.cell[0] - min[0], face.cell[1] - min[1]])
                .or_default();
            match face.target {
                FaceTarget::Surface(SurfaceKind::Floor) => slot.floor = source.floor.clone(),
                FaceTarget::Surface(SurfaceKind::Ceiling) => {
                    slot.ceiling = source.ceiling.clone();
                }
                FaceTarget::Wall { edge, index } => {
                    if let Some(wall) = source.walls(edge).get(index) {
                        slot.walls_mut(edge).push(wall.clone());
                    }
                }
                // Derived faces come back from the copied floors.
                FaceTarget::Derived { .. } => {}
            }
        }
        Some(Self {
            items,
            size: [max[0] - min[0] + 1, max[1] - min[1] + 1],
        })
    }

    /// Extent in sectors, `[width, depth]`.
    pub fn size(&self) -> [i32; 2] {
        self.size
    }

    /// Whether the clip holds nothing.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The copied sectors with their offsets, for drawing a ghost.
    pub fn sectors(&self) -> impl Iterator<Item = ([i32; 2], &GridSector)> + '_ {
        self.items.iter().map(|(offset, sector)| (*offset, sector))
    }

    /// Mirror or turn the whole clip.
    pub fn transform(&mut self, t: Transform) {
        let size = self.size;
        self.items = std::mem::take(&mut self.items)
            .into_iter()
            .map(|(offset, sector)| (t.cell(offset, size), transform_sector(&sector, t)))
            .collect();
        self.size = t.size(size);
    }

    /// Paste with the clip's smallest corner at world sector `at`. Floors and
    /// ceilings replace what is there; walls are added to the stacks, except
    /// exact duplicates and any past the stack cap. The room grows to take
    /// the clip in; if that would pass the size cap nothing changes.
    pub fn paste(&self, world: &mut GridWorld, room: usize, at: [i32; 2]) -> PasteOutcome {
        let Some(target) = world.rooms.get_mut(room) else {
            return PasteOutcome {
                refused: Some(RectRefusal::NoSuchRoom),
                ..PasteOutcome::default()
            };
        };
        if self.items.is_empty() {
            return PasteOutcome::default();
        }
        let rect = CellRect {
            min: at,
            max: [at[0] + self.size[0] - 1, at[1] + self.size[1] - 1],
        };
        if let Err(refusal) = grow_room_to(target, rect) {
            return PasteOutcome {
                refused: Some(refusal),
                ..PasteOutcome::default()
            };
        }
        let mut faces = Vec::new();
        for (offset, clip) in &self.items {
            let cell = [at[0] + offset[0], at[1] + offset[1]];
            let Some([x, z]) = target.local(cell) else {
                continue;
            };
            let Some(sector) = target.sector_or_insert(x, z) else {
                continue;
            };
            if clip.floor.is_some() {
                sector.floor = clip.floor.clone();
                faces.push(FaceRef::floor(room, cell));
            }
            if clip.ceiling.is_some() {
                sector.ceiling = clip.ceiling.clone();
                faces.push(FaceRef::ceiling(room, cell));
            }
            for edge in WallEdge::ALL {
                for wall in clip.walls(edge) {
                    let stack = sector.walls_mut(edge);
                    let duplicate = stack
                        .iter()
                        .any(|w| w.heights == wall.heights && w.facing == wall.facing);
                    if !duplicate && stack.len() < MAX_WALLS_PER_EDGE {
                        stack.push(wall.clone());
                        faces.push(FaceRef::wall(room, cell, edge, stack.len() - 1));
                    }
                }
            }
        }
        PasteOutcome {
            faces,
            refused: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::units::GridScale;

    fn world_with(configure: impl FnOnce(&mut GridRoom)) -> GridWorld {
        let mut room = GridRoom::new("r", 3, 2);
        configure(&mut room);
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn sloped() -> GridSurface {
        let mut s = GridSurface::flat(0, None);
        s.heights = [0, 100, 200, 300];
        s
    }

    fn clip_of(world: &GridWorld) -> GridClipboard {
        let faces = crate::grid::edit::selection::all_faces_of_room(world, 0);
        GridClipboard::capture(world, &faces).unwrap()
    }

    #[test]
    fn capture_records_offsets_from_the_smallest_corner() {
        let w = world_with(|r| {
            r.sector_or_insert(1, 0).unwrap().floor = Some(sloped());
            r.sector_or_insert(2, 1).unwrap().ceiling = Some(GridSurface::flat(1024, None));
        });
        let clip = clip_of(&w);
        assert_eq!(clip.size(), [2, 2]);
        let offsets: Vec<[i32; 2]> = clip.sectors().map(|(o, _)| o).collect();
        assert_eq!(offsets, vec![[0, 0], [1, 1]]);
        assert!(GridClipboard::capture(&w, &[]).is_none());
    }

    #[test]
    fn selected_parts_only_are_copied() {
        let w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.floor = Some(sloped());
            s.ceiling = Some(GridSurface::flat(1024, None));
            s.north.push(GridWall::span(0, 256, None));
            s.north.push(GridWall::span(256, 512, None));
        });
        let clip = GridClipboard::capture(
            &w,
            &[
                FaceRef::ceiling(0, [0, 0]),
                FaceRef::wall(0, [0, 0], WallEdge::North, 1),
            ],
        )
        .unwrap();
        let (_, sector) = clip.sectors().next().unwrap();
        assert!(sector.floor.is_none() && sector.ceiling.is_some());
        assert_eq!(sector.north.len(), 1);
        assert_eq!(sector.north[0].heights[0], 256);
    }

    #[test]
    fn four_quarter_turns_and_double_flips_are_identity() {
        let w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.floor = Some(sloped());
            let mut wall = GridWall::span(0, 512, None);
            wall.heights = [0, 128, 512, 384];
            s.east.push(wall);
            s.nwse.push(GridWall::span(0, 256, None));
            r.sector_or_insert(2, 1).unwrap().floor = Some(GridSurface::flat(64, None));
        });
        let original = clip_of(&w);
        let mut turned = original.clone();
        for _ in 0..4 {
            turned.transform(Transform::RotateCw);
        }
        assert_eq!(turned, original);
        for t in [Transform::FlipH, Transform::FlipV] {
            let mut twice = original.clone();
            twice.transform(t);
            assert_ne!(twice, original, "{t:?}");
            twice.transform(t);
            assert_eq!(twice, original, "{t:?}");
        }
        // Mirror both ways equals two quarter turns.
        let mut both = original.clone();
        both.transform(Transform::FlipH);
        both.transform(Transform::FlipV);
        let mut half = original.clone();
        half.transform(Transform::RotateCw);
        half.transform(Transform::RotateCw);
        assert_eq!(both, half);
    }

    #[test]
    fn flipping_east_west_mirrors_heights_walls_and_split() {
        let w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            let mut floor = sloped();
            floor.split = GridSplit::NwSe;
            s.floor = Some(floor);
            let mut wall = GridWall::span(0, 512, None);
            wall.heights = [0, 128, 512, 384];
            s.east.push(wall);
        });
        let mut clip = clip_of(&w);
        clip.transform(Transform::FlipH);
        let (_, s) = clip.sectors().next().unwrap();
        let floor = s.floor.as_ref().unwrap();
        // NW,NE,SE,SW = 0,100,200,300 becomes NE,NW,SW,SE.
        assert_eq!(floor.heights, [100, 0, 300, 200]);
        assert_eq!(floor.split, GridSplit::NeSw);
        // The east wall is now a west wall with its ends swapped.
        assert!(s.east.is_empty());
        assert_eq!(s.west.len(), 1);
        assert_eq!(s.west[0].heights, [128, 0, 384, 512]);
        assert_eq!(s.west[0].facing, WallFacing::Front);
    }

    #[test]
    fn diagonal_walls_swap_diagonals_under_a_mirror_and_keep_facing() {
        let w = world_with(|r| {
            r.sector_or_insert(0, 0)
                .unwrap()
                .nwse
                .push(GridWall::span(0, 256, None));
        });
        let mut clip = clip_of(&w);
        clip.transform(Transform::FlipH);
        let (_, s) = clip.sectors().next().unwrap();
        assert!(s.nwse.is_empty() && s.nesw.len() == 1);
        assert_eq!(s.nesw[0].facing, WallFacing::Front);
        assert_eq!(s.nesw[0].heights, [0, 0, 256, 256]);
    }

    #[test]
    fn a_quarter_turn_moves_walls_round_the_compass() {
        let w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.north.push(GridWall::span(0, 256, None));
            let mut east = GridWall::span(0, 256, None);
            east.facing = WallFacing::Both;
            s.east.push(east);
        });
        let mut clip = clip_of(&w);
        clip.transform(Transform::RotateCw);
        let (_, s) = clip.sectors().next().unwrap();
        assert_eq!((s.east.len(), s.south.len()), (1, 1));
        assert!(s.north.is_empty() && s.west.is_empty());
        assert_eq!(s.south[0].facing, WallFacing::Both);
    }

    #[test]
    fn turned_clips_swap_their_extent_and_offsets() {
        let w = world_with(|r| {
            r.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
            r.sector_or_insert(2, 0).unwrap().floor = Some(GridSurface::flat(256, None));
        });
        let mut clip = clip_of(&w);
        assert_eq!(clip.size(), [3, 1]);
        clip.transform(Transform::RotateCw);
        assert_eq!(clip.size(), [1, 3]);
        let offsets: Vec<[i32; 2]> = clip.sectors().map(|(o, _)| o).collect();
        assert_eq!(offsets, vec![[0, 0], [0, 2]]);
        // The first cell (height 0) is now at the top after a clockwise turn.
        let heights: Vec<i32> = clip
            .sectors()
            .map(|(_, s)| s.floor.as_ref().unwrap().heights[0])
            .collect();
        assert_eq!(heights, vec![0, 256]);
    }

    #[test]
    fn paste_replaces_surfaces_adds_walls_and_grows_the_room() {
        let mut w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.floor = Some(sloped());
            s.north.push(GridWall::span(0, 512, None));
        });
        let clip = clip_of(&w);
        // Paste onto itself: floor replaced (same), wall is a duplicate.
        let out = clip.paste(&mut w, 0, [0, 0]);
        assert_eq!(out.faces, vec![FaceRef::floor(0, [0, 0])]);
        // Paste far away: the room grows, both parts land.
        let out = clip.paste(&mut w, 0, [-2, 3]);
        assert!(out.refused.is_none());
        let room = &w.rooms[0];
        assert_eq!(room.origin, [-2, 0]);
        assert_eq!(out.faces.len(), 2);
        assert!(out.faces.iter().all(|f| f.exists(&w)));
        // Pasting with a different wall adds it to the stack.
        let mut other = clip.clone();
        other.items.get_mut(&[0, 0]).unwrap().north[0].heights[3] = 999;
        let out = other.paste(&mut w, 0, [0, 0]);
        let sector = w.rooms[0].sector(2, 0).unwrap();
        assert_eq!(sector.north.len(), 2);
        assert_eq!(out.faces.len(), 2);
    }

    #[test]
    fn a_paste_that_would_overflow_the_room_changes_nothing() {
        let mut w = world_with(|r| {
            r.sector_or_insert(0, 0).unwrap().floor = Some(GridSurface::flat(0, None));
        });
        let clip = clip_of(&w);
        let before = w.clone();
        let out = clip.paste(&mut w, 0, [500, 0]);
        assert!(matches!(
            out.refused,
            Some(RectRefusal::RoomTooLarge { .. })
        ));
        assert_eq!(w, before);
        assert_eq!(
            clip.paste(&mut w, 9, [0, 0]).refused,
            Some(RectRefusal::NoSuchRoom)
        );
    }
}
