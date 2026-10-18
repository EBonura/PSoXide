//! The resolution lens: run a level verb on cells of any size.
//!
//! The level verbs (Raise, Ramp, Stairs, Break) work on a uniform lattice of
//! sectors. A **lens** maps a footprint of cells of one level (halves or
//! quarters of a sector, or sectors) onto a temporary uniform room whose
//! sectors *are* those cells, runs the verb on it unchanged, and writes the
//! result back into the real leaves.
//!
//! The mapping keeps the physical shape of the terrain, so slopes, walkable
//! limits and step heights mean what they do in the real room: a tile of
//! `1 / f` of a sector is stood on as a whole sector of a world whose heights
//! are `f` times larger and whose scale divides runtime units by `f`
//! (`scale.den * f`). Heights come back divided by `f`; a height the verb did
//! not change is restored exactly and one it did is snapped to the fine step
//! (64 grid units), the precision the tools write.
//!
//! What a lens does not carry: the room's edge overrides (a verb that writes
//! one, Break's ragged wall tops, writes it to the copy and the real room
//! keeps its own), planes, meshes and lights. Cells in the one-tile margin
//! around the footprint are read-only context (walkability, neighbours).

use std::collections::{BTreeMap, BTreeSet};

use super::super::leaf::{edge_clicks, LeafKey, CLICKS_PER_SECTOR};
use super::super::room::GridRoom;
use super::super::sector::{GridSector, WallEdge};
use super::super::tree::sector_split::split_sector;
use super::super::tree::{normalize, subdivide_to};
use super::super::units::{snap_to, GridScale, FINE_UNITS};
use super::super::world::GridWorld;
use super::region::Region;

/// Tiles of context read around the footprint.
const MARGIN: i32 = 2;

/// An open lens: the real room being worked on and what the copy held.
#[derive(Clone, Debug)]
pub struct Lens {
    room: usize,
    level: u8,
    /// Tiles per sector edge: 1, 2 or 4.
    factor: i32,
    footprint: Vec<[i32; 2]>,
    /// The scaled sectors of every tile of the copy when it was made.
    before: BTreeMap<[i32; 2], Option<GridSector>>,
    /// The real room with the footprint split to tiles.
    work: GridRoom,
}

/// Round half away from zero.
fn round_div(num: i32, den: i32) -> i32 {
    let den = den.max(1);
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

fn scale_sector(sector: &mut GridSector, f: i32) {
    for surface in sector.surfaces_mut() {
        surface.heights = surface.heights.map(|h| h * f);
        if let Some(b) = surface.heights_b.as_mut() {
            *b = b.map(|h| h * f);
        }
    }
    for edge in WallEdge::ALL {
        for wall in sector.walls_mut(edge) {
            wall.heights = wall.heights.map(|h| h * f);
        }
    }
}

/// `after` scaled back by `f`: values equal to `before`'s are restored
/// exactly, changed ones rounded and snapped to the fine step.
fn unscale_sector(after: &GridSector, before: Option<&GridSector>, f: i32) -> GridSector {
    let un = |a: i32, b: Option<i32>| -> i32 {
        if b == Some(a) {
            a / f
        } else {
            snap_to(round_div(a, f), FINE_UNITS)
        }
    };
    let mut out = after.clone();
    let before_surfaces: Vec<&super::super::surface::GridSurface> =
        before.map_or_else(Vec::new, |b| b.surfaces().collect());
    for (i, surface) in out.surfaces_mut().enumerate() {
        let was = before_surfaces.get(i);
        for k in 0..4 {
            surface.heights[k] = un(surface.heights[k], was.map(|w| w.heights[k]));
        }
        if let Some(b) = surface.heights_b.as_mut() {
            for k in 0..4 {
                b[k] = un(b[k], was.and_then(|w| w.heights_b).map(|w| w[k]));
            }
        }
    }
    for edge in WallEdge::ALL {
        for (i, wall) in out.walls_mut(edge).iter_mut().enumerate() {
            let was = before.and_then(|b| b.walls(edge).get(i));
            for k in 0..4 {
                wall.heights[k] = un(wall.heights[k], was.map(|w| w.heights[k]));
            }
        }
    }
    out
}

/// The cell `tile` of `level` as a sector of its own: the leaf that covers
/// it, split down to the tile when it is larger.
fn tile_sector(room: &GridRoom, level: u8, tile: [i32; 2]) -> Option<GridSector> {
    let e = edge_clicks(level);
    let click = [tile[0] * e, tile[1] * e];
    let (mut key, leaf) = room.leaf_at(click)?;
    let mut sector = leaf.clone();
    while key.level < level {
        let children = split_sector(&sector);
        let keys = key.children()?;
        let at = keys.iter().position(|k| k.contains_click(click))?;
        sector = children[at].clone();
        key = keys[at];
    }
    (key.level == level).then_some(sector)
}

/// The finest leaf level among the leaves touching `rect` (inclusive clicks).
fn finest_level(room: &GridRoom, lo: [i32; 2], hi: [i32; 2]) -> u8 {
    let mut finest = 0;
    for (key, _) in room.leaves() {
        let [x1, z1] = key.end_click();
        if key.x <= hi[0] && key.z <= hi[1] && x1 > lo[0] && z1 > lo[1] {
            finest = finest.max(key.level);
        }
    }
    finest
}

impl Lens {
    /// Open a lens on `footprint`, cells of `level` (`click / edge`) of
    /// `room`. Returns the lens, the temporary world holding the uniform copy
    /// and the region of the footprint in it. `None` for a missing room or an
    /// empty footprint.
    pub fn open(
        world: &GridWorld,
        room: usize,
        level: u8,
        footprint: &[[i32; 2]],
    ) -> Option<(Self, GridWorld, Region)> {
        let real = world.rooms.get(room)?;
        if footprint.is_empty() {
            return None;
        }
        let e0 = edge_clicks(level);
        let (mut lo, mut hi) = (footprint[0], footprint[0]);
        for t in footprint {
            lo = [lo[0].min(t[0]), lo[1].min(t[1])];
            hi = [hi[0].max(t[0]), hi[1].max(t[1])];
        }
        // The cells of the copy are as fine as the finest leaf that comes
        // near the footprint, so every tile is an exact cut of one leaf.
        let near_lo = [(lo[0] - MARGIN) * e0, (lo[1] - MARGIN) * e0];
        let near_hi = [(hi[0] + MARGIN + 1) * e0 - 1, (hi[1] + MARGIN + 1) * e0 - 1];
        let level = level.max(finest_level(real, near_lo, near_hi));
        let (ratio, e) = (e0 / edge_clicks(level), edge_clicks(level));
        let tiles: Vec<[i32; 2]> = footprint
            .iter()
            .flat_map(|t| {
                (0..ratio).flat_map(move |dz| (0..ratio).map(move |dx| [t[0] * ratio + dx, t[1] * ratio + dz]))
            })
            .collect();
        let mut work = real.clone();
        for t in &tiles {
            // Split the leaf under the tile down to the tile's size.
            subdivide_to(&mut work, [t[0] * e, t[1] * e], level).ok()?;
        }
        let f = CLICKS_PER_SECTOR / e;
        let (mut tlo, mut thi) = (tiles[0], tiles[0]);
        for t in &tiles {
            tlo = [tlo[0].min(t[0]), tlo[1].min(t[1])];
            thi = [thi[0].max(t[0]), thi[1].max(t[1])];
        }
        let (rlo, rhi) = ([tlo[0] - MARGIN, tlo[1] - MARGIN], [thi[0] + MARGIN, thi[1] + MARGIN]);
        let (w, d) = ((rhi[0] - rlo[0] + 1) as usize, (rhi[1] - rlo[1] + 1) as usize);
        let mut virt = GridRoom::new(work.name.clone(), w, d);
        virt.origin = rlo;
        virt.y_offset = work.y_offset * f;
        virt.ambient = work.ambient;
        virt.fog = work.fog;
        virt.wall_mode = work.wall_mode;
        virt.defaults = work.defaults;
        virt.defaults.open_wall *= f;
        let rules = &mut virt.defaults.rules;
        for rule in [&mut rules.floor_by_height, &mut rules.wall_by_height] {
            rule.at = rule.at.map(|v| v.saturating_mul(f as i16));
        }
        virt.band_sets = work.band_sets.clone();
        for set in &mut virt.band_sets {
            for line in &mut set.lines {
                *line = line.with_value(line.value() * f);
            }
        }
        virt.rough = work.rough;
        virt.rough.amp = virt.rough.amp.map(|a| (i32::from(a) * f).min(255) as u8);
        let mut before = BTreeMap::new();
        for tz in rlo[1]..=rhi[1] {
            for tx in rlo[0]..=rhi[0] {
                let mut sector = tile_sector(&work, level, [tx, tz]);
                if let Some(s) = sector.as_mut() {
                    scale_sector(s, f);
                }
                virt.set_sector((tx - rlo[0]) as usize, (tz - rlo[1]) as usize, sector.clone());
                before.insert([tx, tz], sector);
            }
        }
        let vworld = GridWorld {
            scale: GridScale {
                num: world.scale.num,
                den: world.scale.den.saturating_mul(f),
            },
            rooms: vec![virt],
            ..GridWorld::default()
        };
        let region = Region::from_cells(0, tiles.iter().copied());
        let lens = Self {
            room,
            level,
            factor: f,
            footprint: tiles,
            before,
            work,
        };
        Some((lens, vworld, region))
    }

    /// Tiles per sector edge (1, 2 or 4): what amounts and heights are scaled by.
    pub fn factor(&self) -> i32 {
        self.factor
    }

    /// The cell level the copy is made of (it can be finer than asked).
    pub fn level(&self) -> u8 {
        self.level
    }

    /// The footprint as tiles of [`Self::level`].
    pub fn footprint(&self) -> &[[i32; 2]] {
        &self.footprint
    }

    /// The copy's world sector (a tile) that holds the real world sector
    /// `cell`'s middle, for mapping spawns.
    pub fn tile_of_sector(&self, cell: [i32; 2]) -> [i32; 2] {
        let f = self.factor;
        [cell[0] * f + f / 2, cell[1] * f + f / 2]
    }

    /// Write the copy back into `world`: every footprint tile becomes the
    /// real leaf it was cut from, then the tree is normalised. Returns the
    /// number of leaves whose content changed.
    pub fn close(self, world: &mut GridWorld, virt: &GridWorld) -> usize {
        let Some(room) = virt.rooms.first() else {
            return 0;
        };
        let e = edge_clicks(self.level);
        let mut work = self.work;
        let mut changed = 0;
        for tile in &self.footprint {
            let after = room
                .local(*tile)
                .and_then(|[x, z]| room.sector(x, z))
                .cloned();
            let key = LeafKey::containing(self.level, [tile[0] * e, tile[1] * e]);
            let Some(after) = after else { continue };
            let before = self.before.get(tile).and_then(Option::as_ref);
            let real = unscale_sector(&after, before, self.factor);
            if let Some(slot) = work.leaf_mut(key) {
                if *slot != real {
                    *slot = real;
                    changed += 1;
                }
            }
        }
        normalize(&mut work);
        if let Some(target) = world.rooms.get_mut(self.room) {
            *target = work;
        }
        changed
    }
}

/// Every tile of `cells` (a set of sectors at `level` 2) is not needed: a
/// footprint is given in tiles of the level it was picked at.
pub fn dedup_tiles(tiles: &[[i32; 2]]) -> Vec<[i32; 2]> {
    tiles
        .iter()
        .copied()
        .collect::<BTreeSet<[i32; 2]>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::edit::verbs::raise::{raise_region, RaiseMode, RaiseParams};
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale as Scale;

    fn world() -> GridWorld {
        let mut room = GridRoom::new_derived("r", 4, 4);
        for z in 0..4 {
            for x in 0..4 {
                let s = room.sector_or_insert(x, z).unwrap();
                s.set_ground_floor(Some(GridSurface::flat(0, None)));
                s.set_ground_ceiling(Some(GridSurface::flat(2048, None)));
            }
        }
        GridWorld {
            scale: Scale { num: 5, den: 4 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    #[test]
    fn the_copy_has_the_same_shape_in_runtime_units() {
        let w = world();
        let (lens, virt, region) = Lens::open(&w, 0, 4, &[[5, 6]]).unwrap();
        assert_eq!(lens.factor(), 4);
        assert_eq!(region.len(), 1);
        // A tile of the copy is a quarter of a sector, with heights four
        // times larger and a scale that divides runtime units by four: the
        // same height in runtime units, a quarter of the width.
        let units = crate::grid::cook::RuntimeUnits::new(w.scale).unwrap();
        let vunits = crate::grid::cook::RuntimeUnits::new(virt.scale).unwrap();
        assert_eq!(vunits.sector() * 4, units.sector());
        assert_eq!(vunits.rt(4 * 512), units.rt(512));
    }

    #[test]
    fn a_verb_through_the_lens_changes_only_its_cells() {
        let mut w = world();
        let before = w.clone();
        let (lens, mut virt, region) = Lens::open(&w, 0, 4, &[[5, 6], [6, 6]]).unwrap();
        let params = RaiseParams {
            mode: RaiseMode::Offset,
            follow_ceiling: true,
            wall_material: None,
            reference: 0,
        };
        // Raise by one click of the real world (256 grid units).
        let applied = raise_region(&mut virt, &region, params, 256 * lens.factor()).unwrap();
        assert_eq!(applied.moved, 2);
        let changed = lens.close(&mut w, &virt);
        assert_eq!(changed, 2);
        let room = &w.rooms[0];
        for (key, leaf) in room.leaves() {
            let h = leaf.ground_floor().unwrap().heights;
            let up = key.level == 4 && key.z == 6 && (key.x == 5 || key.x == 6);
            assert_eq!(h, [if up { 256 } else { 0 }; 4], "{key:?}");
        }
        assert!(crate::grid::tree::check(room).is_empty());
        // The room outside is what it was.
        assert_ne!(*room, before.rooms[0]);
    }

    #[test]
    fn a_footprint_next_to_finer_cells_uses_their_level() {
        let mut w = world();
        let keys: Vec<LeafKey> = w.rooms[0].leaves.in_sector([2, 2]).iter().map(|(k, _)| *k).collect();
        crate::grid::tree::subdivide_all_to(&mut w.rooms[0], &keys, 4);
        let (lens, _, region) = Lens::open(&w, 0, 3, &[[3, 3]]).unwrap();
        // The half cell asked for is four quarter tiles of the copy.
        assert_eq!(lens.level(), 4);
        assert_eq!(region.len(), 4);
    }
}
