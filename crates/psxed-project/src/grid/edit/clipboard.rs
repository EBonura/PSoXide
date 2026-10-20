//! Copying a selection and pasting it elsewhere, flipped or turned.
//!
//! The clipboard holds the selected parts of each sector at offsets from the
//! selection's smallest corner. Mirroring and turning rewrite everything
//! that has a direction: corner heights, diagonal splits, wall edges and the
//! side a wall faces. Heights are kept as authored, room-relative.

use std::collections::BTreeMap;

use super::super::derive::edge_key;
use super::super::limits::MAX_WALLS_PER_EDGE;
use super::super::overrides::{Axis, EdgeKey, EdgeOverride, SEGMENT_CLICKS};
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
    let mut out = GridSector::with_surfaces(
        sector.ground_floor().map(|s| transform_surface(s, t)),
        sector.ground_ceiling().map(|s| transform_surface(s, t)),
    );
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
    /// What the user set on the derived wall segments of the copied sectors,
    /// keyed in clicks from the clip's smallest corner. Turning and
    /// mirroring move them with the sectors; a texture offset or turn inside
    /// one is kept as authored.
    overrides: Vec<(EdgeKey, EdgeOverride)>,
    /// The room the clip was copied from: band set indices only mean the
    /// same thing there.
    room: usize,
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
            faces.iter().map(|f| f.cell()[0]).min()?,
            faces.iter().map(|f| f.cell()[1]).min()?,
        ];
        let max = [
            faces.iter().map(|f| f.cell()[0]).max()?,
            faces.iter().map(|f| f.cell()[1]).max()?,
        ];
        let mut items: BTreeMap<[i32; 2], GridSector> = BTreeMap::new();
        // Walls are copied in index order so a stack keeps its order.
        let mut ordered = faces.clone();
        ordered.sort();
        for face in ordered {
            let source = face.sector(world)?;
            let slot = items
                .entry([face.cell()[0] - min[0], face.cell()[1] - min[1]])
                .or_default();
            match face.target {
                FaceTarget::Surface {
                    kind: SurfaceKind::Floor,
                    ..
                } => slot.set_ground_floor(face.surface(world).cloned()),
                FaceTarget::Surface {
                    kind: SurfaceKind::Ceiling,
                    ..
                } => {
                    slot.set_ground_ceiling(face.surface(world).cloned());
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
        let mut overrides: BTreeMap<EdgeKey, EdgeOverride> = BTreeMap::new();
        if let Some(source) = world.rooms.get(room) {
            let clicks = i32::from(SEGMENT_CLICKS);
            for offset in items.keys() {
                let cell = [min[0] + offset[0], min[1] + offset[1]];
                for edge in WallEdge::ALL {
                    let key = edge_key(cell, edge);
                    if let Some(found) = source.edge_overrides.get(&key) {
                        let relative = EdgeKey {
                            x: key.x - min[0] * clicks,
                            z: key.z - min[1] * clicks,
                            ..key
                        };
                        overrides.insert(relative, found.clone());
                    }
                }
            }
        }
        Some(Self {
            items,
            size: [max[0] - min[0] + 1, max[1] - min[1] + 1],
            overrides: overrides.into_iter().collect(),
            room,
        })
    }

    /// The clip with every height raised by `dy` grid units (surfaces and
    /// walls; the heights are room-relative, so this stamps the copy higher
    /// or lower than the original).
    pub fn lifted(&self, dy: i32) -> Self {
        let mut out = self.clone();
        if dy == 0 {
            return out;
        }
        let lift = |surface: &mut GridSurface| {
            surface.heights = surface.heights.map(|h| h + dy);
            surface.heights_b = surface.heights_b.map(|b| b.map(|h| h + dy));
        };
        for sector in out.items.values_mut() {
            if let Some(floor) = sector.ground_floor_mut() {
                lift(floor);
            }
            if let Some(ceiling) = sector.ground_ceiling_mut() {
                lift(ceiling);
            }
            for edge in WallEdge::ALL {
                for wall in sector.walls_mut(edge) {
                    wall.heights = wall.heights.map(|h| h + dy);
                }
            }
        }
        out
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
        let clicks = [
            size[0] * i32::from(SEGMENT_CLICKS),
            size[1] * i32::from(SEGMENT_CLICKS),
        ];
        for (key, found) in &mut self.overrides {
            let (moved, reversed) = transform_key(*key, t, clicks);
            *key = moved;
            if reversed {
                found.top_delta.reverse();
            }
        }
        self.overrides.sort_by_key(|(key, _)| *key);
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
            if clip.ground_floor().is_some() {
                sector.set_ground_floor(clip.ground_floor().cloned());
                faces.push(FaceRef::floor(room, cell));
            }
            if clip.ground_ceiling().is_some() {
                sector.set_ground_ceiling(clip.ground_ceiling().cloned());
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
        let clicks = i32::from(SEGMENT_CLICKS);
        for (key, found) in &self.overrides {
            let mut found = found.clone();
            if self.room != room {
                found.band_set = None;
            }
            target.edge_overrides.edit(
                EdgeKey {
                    x: key.x + at[0] * clicks,
                    z: key.z + at[1] * clicks,
                    ..*key
                },
                |slot| *slot = found,
            );
        }
        PasteOutcome {
            faces,
            refused: None,
        }
    }
}

/// Where a wall segment key goes under `t` in a clip `size` clicks across,
/// and whether the segment now runs the other way (so its end-to-end values
/// swap).
fn transform_key(key: EdgeKey, t: Transform, size: [i32; 2]) -> (EdgeKey, bool) {
    let len = i32::from(key.len);
    let (p, q) = match key.axis {
        Axis::X => ([key.x, key.z], [key.x + len, key.z]),
        Axis::Z => ([key.x, key.z], [key.x, key.z + len]),
        Axis::NwSe => ([key.x, key.z], [key.x + len, key.z + len]),
        Axis::NeSw => ([key.x + len, key.z], [key.x, key.z + len]),
    };
    let map = |p: [i32; 2]| match t {
        Transform::FlipH => [size[0] - p[0], p[1]],
        Transform::FlipV => [p[0], size[1] - p[1]],
        Transform::RotateCw => [size[1] - p[1], p[0]],
    };
    let (p, q) = (map(p), map(q));
    let low = [p[0].min(q[0]), p[1].min(q[1])];
    let (axis, reversed) = match key.axis {
        Axis::X | Axis::Z if p[1] == q[1] => (Axis::X, p[0] > q[0]),
        Axis::X | Axis::Z => (Axis::Z, p[1] > q[1]),
        // The diagonal that runs from the north-west corner stays one when
        // its ends are still the low and high corners.
        Axis::NwSe | Axis::NeSw if (p[0] < q[0]) == (p[1] < q[1]) => (Axis::NwSe, p[0] > q[0]),
        // Otherwise it runs from the north-east corner to the south-west.
        Axis::NwSe | Axis::NeSw => (Axis::NeSw, p[0] < q[0]),
    };
    (
        EdgeKey {
            x: low[0],
            z: low[1],
            axis,
            len: key.len,
        },
        reversed,
    )
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
            r.sector_or_insert(1, 0)
                .unwrap()
                .set_ground_floor(Some(sloped()));
            r.sector_or_insert(2, 1)
                .unwrap()
                .set_ground_ceiling(Some(GridSurface::flat(1024, None)));
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
            s.set_ground_floor(Some(sloped()));
            s.set_ground_ceiling(Some(GridSurface::flat(1024, None)));
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
        assert!(sector.ground_floor().is_none() && sector.ground_ceiling().is_some());
        assert_eq!(sector.north.len(), 1);
        assert_eq!(sector.north[0].heights[0], 256);
    }

    #[test]
    fn four_quarter_turns_and_double_flips_are_identity() {
        let w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.set_ground_floor(Some(sloped()));
            let mut wall = GridWall::span(0, 512, None);
            wall.heights = [0, 128, 512, 384];
            s.east.push(wall);
            s.nwse.push(GridWall::span(0, 256, None));
            r.sector_or_insert(2, 1)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(64, None)));
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
            s.set_ground_floor(Some(floor));
            let mut wall = GridWall::span(0, 512, None);
            wall.heights = [0, 128, 512, 384];
            s.east.push(wall);
        });
        let mut clip = clip_of(&w);
        clip.transform(Transform::FlipH);
        let (_, s) = clip.sectors().next().unwrap();
        let floor = s.ground_floor().unwrap();
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
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
            r.sector_or_insert(2, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(256, None)));
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
            .map(|(_, s)| s.ground_floor().unwrap().heights[0])
            .collect();
        assert_eq!(heights, vec![0, 256]);
    }

    #[test]
    fn paste_replaces_surfaces_adds_walls_and_grows_the_room() {
        let mut w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.set_ground_floor(Some(sloped()));
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
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
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

    fn hidden() -> EdgeOverride {
        EdgeOverride {
            hidden: true,
            ..EdgeOverride::default()
        }
    }

    #[test]
    fn a_lifted_clip_raises_surfaces_and_walls_and_leaves_the_original() {
        let w = world_with(|r| {
            let s = r.sector_or_insert(0, 0).unwrap();
            s.set_ground_floor(Some(sloped()));
            s.set_ground_ceiling(Some(GridSurface::flat(1024, None)));
            s.north.push(GridWall::span(0, 512, None));
        });
        let clip = clip_of(&w);
        let up = clip.lifted(256);
        let (_, s) = up.sectors().next().unwrap();
        assert_eq!(s.ground_floor().unwrap().heights, [256, 356, 456, 556]);
        assert_eq!(s.ground_ceiling().unwrap().heights, [1280; 4]);
        assert_eq!(s.north[0].heights, [256, 256, 768, 768]);
        let (_, s) = clip.sectors().next().unwrap();
        assert_eq!(s.ground_floor().unwrap().heights, [0, 100, 200, 300]);
        assert_eq!(clip.lifted(0), clip);
    }

    #[test]
    fn wall_overrides_travel_with_a_copy_and_land_at_the_paste_point() {
        let mut w = world_with(|r| {
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
            let north = edge_key([0, 0], WallEdge::North);
            r.edge_overrides.edit(north, |o| *o = hidden());
        });
        let clip = clip_of(&w);
        let out = clip.paste(&mut w, 0, [2, 1]);
        assert!(out.refused.is_none());
        let landed = w.rooms[0]
            .edge_overrides
            .get(&edge_key([2, 1], WallEdge::North));
        assert_eq!(landed, Some(&hidden()));
        // The original keeps its own.
        assert!(w.rooms[0]
            .edge_overrides
            .get(&edge_key([0, 0], WallEdge::North))
            .is_some());
    }

    #[test]
    fn overrides_follow_a_mirror_and_a_turn_and_swap_their_top_delta() {
        let w = world_with(|r| {
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
            r.sector_or_insert(1, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
            let north = edge_key([0, 0], WallEdge::North);
            r.edge_overrides.edit(north, |o| {
                o.hidden = true;
                o.top_delta = [4, 8];
            });
        });
        // A two by one clip; the north wall of its west sector.
        let mut flipped = clip_of(&w);
        flipped.transform(Transform::FlipH);
        let mut w2 = w.clone();
        w2.rooms[0].edge_overrides.retain(|_| false);
        flipped.paste(&mut w2, 0, [0, 0]);
        // Mirrored east to west it sits on the east sector's north edge,
        // running the other way.
        let found = w2.rooms[0]
            .edge_overrides
            .get(&edge_key([1, 0], WallEdge::North))
            .expect("moved to the east sector");
        assert!(found.hidden);
        assert_eq!(found.top_delta, [8, 4]);
        // A quarter turn puts a north wall on the east edge.
        let mut turned = clip_of(&w);
        turned.transform(Transform::RotateCw);
        let mut w3 = w.clone();
        w3.rooms[0].edge_overrides.retain(|_| false);
        turned.paste(&mut w3, 0, [0, 0]);
        let east = w3.rooms[0]
            .edge_overrides
            .get(&edge_key([0, 0], WallEdge::East))
            .expect("north became east");
        assert!(east.hidden);
        // Four turns bring everything home.
        let mut round = clip_of(&w);
        for _ in 0..4 {
            round.transform(Transform::RotateCw);
        }
        assert_eq!(round, clip_of(&w));
    }

    #[test]
    fn a_band_set_only_means_something_in_the_room_it_was_copied_from() {
        let mut w = world_with(|r| {
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
            r.edge_overrides
                .edit(edge_key([0, 0], WallEdge::West), |o| o.band_set = Some(3));
        });
        w.rooms.push(GridRoom::new("other", 3, 2));
        let clip = clip_of(&w);
        clip.paste(&mut w, 0, [1, 1]);
        assert_eq!(
            w.rooms[0]
                .edge_overrides
                .get(&edge_key([1, 1], WallEdge::West))
                .and_then(|o| o.band_set),
            Some(3)
        );
        clip.paste(&mut w, 1, [1, 1]);
        assert!(w.rooms[1]
            .edge_overrides
            .get(&edge_key([1, 1], WallEdge::West))
            .is_none());
    }
}
