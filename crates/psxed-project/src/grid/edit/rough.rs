//! Rough: painting the rock level of sectors and the room's jitter settings.

use std::collections::BTreeSet;

use super::super::rough::{Rough, MAX_LEVEL};
use super::super::world::GridWorld;

/// Make sure the room has amplitudes to draw with: when none is set, the
/// defaults of the world's scale (a quarter, a half and the whole of the
/// largest the character's radius allows).
fn ensure_amp(world: &mut GridWorld, room: usize) {
    let scale = world.scale;
    if let Some(r) = world.rooms.get_mut(room) {
        if !r.rough.jitters() {
            r.rough.amp = Rough::default_amp(scale);
        }
    }
}

/// Set the rough level (0 to 3) of `cells`. Returns how many changed. A level
/// above zero gives a room without amplitudes the default ones.
pub fn set_level(world: &mut GridWorld, room: usize, cells: &[[i32; 2]], level: u8) -> usize {
    let level = level.min(MAX_LEVEL);
    if level > 0 {
        ensure_amp(world, room);
    }
    let Some(r) = world.rooms.get_mut(room) else {
        return 0;
    };
    let mut changed = 0;
    for &cell in cells {
        if let Some(sector) = r.local(cell).and_then(|[x, z]| r.sector_mut(x, z)) {
            if sector.rough != level {
                sector.rough = level;
                changed += 1;
            }
        }
    }
    changed
}

/// Set the rough level (0 to 3) of the cells of `cell_level` in `lo..=hi`
/// (cell coordinates at that level), splitting larger leaves to fit. Returns
/// how many leaves changed.
pub fn set_level_cells(
    world: &mut GridWorld,
    room: usize,
    cell_level: u8,
    lo: [i32; 2],
    hi: [i32; 2],
    level: u8,
) -> usize {
    let level = level.min(MAX_LEVEL);
    if level > 0 {
        ensure_amp(world, room);
    }
    let Some(r) = world.rooms.get_mut(room) else {
        return 0;
    };
    let rect = super::fit::ClickRect::of_cells(cell_level, lo, hi);
    let keys = super::fit::split_to_fit(r, rect, cell_level);
    let mut changed = 0;
    for key in keys {
        if let Some(leaf) = r.leaf_mut(key) {
            if leaf.rough != level {
                leaf.rough = level;
                changed += 1;
            }
        }
    }
    super::super::tree::normalize(r);
    changed
}

/// Raise the rough level of `cells` to at least `level` (what Break's jitter
/// option does to the sectors it broke).
pub fn raise_level(
    world: &mut GridWorld,
    room: usize,
    cells: &BTreeSet<[i32; 2]>,
    level: u8,
) -> usize {
    let level = level.min(MAX_LEVEL);
    if level == 0 {
        return 0;
    }
    ensure_amp(world, room);
    let Some(r) = world.rooms.get_mut(room) else {
        return 0;
    };
    let mut changed = 0;
    for &cell in cells {
        if let Some(sector) = r.local(cell).and_then(|[x, z]| r.sector_mut(x, z)) {
            if sector.rough < level {
                sector.rough = level;
                changed += 1;
            }
        }
    }
    changed
}

/// Change the room's rough settings: any of the seed, the amplitudes (grid
/// units at levels 1, 2, 3) and the tint (percent).
pub fn set_params(
    world: &mut GridWorld,
    room: usize,
    seed: Option<u16>,
    amp: Option<[u8; 3]>,
    tint: Option<u8>,
) -> bool {
    let Some(r) = world.rooms.get_mut(room) else {
        return false;
    };
    let before = r.rough;
    if let Some(seed) = seed {
        r.rough.seed = seed;
    }
    if let Some(amp) = amp {
        r.rough.amp = amp;
    }
    if let Some(tint) = tint {
        r.rough.tint = tint.min(100);
    }
    r.rough != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;

    fn world() -> GridWorld {
        let mut room = GridRoom::new_derived("r", 2, 2);
        for z in 0..2 {
            for x in 0..2 {
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(0, None)));
            }
        }
        GridWorld {
            scale: GridScale { num: 5, den: 4 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    #[test]
    fn a_level_gives_the_room_default_amplitudes_and_zero_clears_it() {
        let mut w = world();
        assert!(!w.rooms[0].rough.jitters());
        assert_eq!(set_level(&mut w, 0, &[[0, 0], [1, 0]], 2), 2);
        assert_eq!(w.rooms[0].rough.amp, [13, 26, 51]);
        assert_eq!(w.rooms[0].sector(1, 0).unwrap().rough, 2);
        assert_eq!(set_level(&mut w, 0, &[[0, 0]], 0), 1);
        // Raising never lowers.
        let cells: BTreeSet<[i32; 2]> = [[1, 0], [1, 1]].into_iter().collect();
        assert_eq!(raise_level(&mut w, 0, &cells, 1), 1);
        assert_eq!(w.rooms[0].sector(1, 0).unwrap().rough, 2);
        assert_eq!(w.rooms[0].sector(1, 1).unwrap().rough, 1);
        // Levels are two bits.
        set_level(&mut w, 0, &[[0, 1]], 9);
        assert_eq!(w.rooms[0].sector(0, 1).unwrap().rough, MAX_LEVEL);
    }

    #[test]
    fn rough_cells_of_any_size_split_big_leaves_to_fit() {
        let mut w = world();
        crate::grid::tree::merge_all(&mut w.rooms[0], None, 1);
        assert_eq!(w.rooms[0].leaf_count(), 1);
        // One quarter cell (click 1, 1) of the two by two sector room.
        assert_eq!(set_level_cells(&mut w, 0, 4, [1, 1], [1, 1], 3), 1);
        let (key, leaf) = w.rooms[0].leaf_at([1, 1]).unwrap();
        assert_eq!((key.level, leaf.rough), (4, 3));
        assert_eq!(w.rooms[0].leaf_at([3, 3]).unwrap().1.rough, 0);
        assert!(crate::grid::tree::check(&w.rooms[0]).is_empty());
    }

    #[test]
    fn settings_round_trip_and_defaults_stay_out_of_the_file() {
        let mut w = world();
        let text = ron::to_string(&w.rooms[0]).unwrap();
        assert!(!text.contains("rough"), "{text}");
        assert!(set_params(&mut w, 0, Some(9), Some([5, 10, 20]), Some(40)));
        assert!(!set_params(&mut w, 0, Some(9), None, None));
        let text = ron::to_string(&w.rooms[0]).unwrap();
        let back: GridRoom = ron::from_str(&text).unwrap();
        assert_eq!(back.rough, w.rooms[0].rough);
        assert_eq!(back.rough.tint, 40);
    }
}
