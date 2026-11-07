//! What every verb shares when it writes: smart paint rules, the wall
//! material of a shaped sector and the ceiling that follows a floor.

use crate::grid::bands::{Anchor, Band, BandSet, BandV};
use crate::grid::cook::RuntimeUnits;
use crate::grid::paint_rules::PaintRules;
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSector;
use crate::grid::slope::{surface_class, SlopeClass};
use crate::grid::surface::GridSurface;
use crate::grid::units::CLICK_UNITS;
use crate::ResourceId;
use psx_engine::WalkLimits;

/// What a floor was before a verb moved it, for the rules: its slope class
/// and the height of its middle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloorState {
    /// The slope class, `None` without a floor or a cookable scale.
    pub class: Option<SlopeClass>,
    /// Mean of the first triangle's corner heights, room-relative grid units.
    pub height: i32,
}

/// The state of a sector's floor.
pub fn floor_class(units: Option<&RuntimeUnits>, y_offset: i32, sector: &GridSector) -> FloorState {
    let Some(floor) = sector.ground_floor() else {
        return FloorState {
            class: None,
            height: 0,
        };
    };
    FloorState {
        class: units.map(|u| surface_class(u, y_offset, floor, &WalkLimits::DEFAULT)),
        height: floor.heights.iter().sum::<i32>() / 4,
    }
}

/// Set the material of both triangles of `surface`.
pub fn paint_surface(surface: &mut GridSurface, material: ResourceId) {
    surface.tri_a.material = Some(material);
    if let Some(b) = surface.tri_b.as_mut() {
        b.material = Some(material);
    }
}

/// Apply the floor rules to a sector the verb shaped: the class rule when
/// its slope class changed (`before`), the height rule when its class or
/// height did, unless it was painted by hand.
pub fn apply_floor_rule(
    rules: &PaintRules,
    units: Option<&RuntimeUnits>,
    y_offset: i32,
    before: FloorState,
    sector: &mut GridSector,
) {
    let after = floor_class(units, y_offset, sector);
    if before == after {
        return;
    }
    let (Some(class), Some(floor)) = (after.class, sector.ground_floor_mut()) else {
        return;
    };
    if floor.paint_locked {
        return;
    }
    let by_class = (before.class != after.class)
        .then(|| rules.floor_for(class))
        .flatten();
    if let Some(material) = rules.floor_for_height(class, after.height).or(by_class) {
        paint_surface(floor, material);
    }
}

/// Apply the floor rules to `cells` regardless of what changed: the
/// "reapply" of the paint rules setter. Skips floors painted by hand.
/// Returns how many floors were painted.
pub fn reapply_floor_rules(
    world: &mut crate::grid::world::GridWorld,
    room: usize,
    cells: &[[i32; 2]],
) -> usize {
    let units = RuntimeUnits::new(world.scale).ok();
    let Some(r) = world.rooms.get_mut(room) else {
        return 0;
    };
    let rules = r.defaults.rules;
    let y_offset = r.y_offset;
    let mut painted = 0;
    for &cell in cells {
        let Some(sector) = r.local(cell).and_then(|[x, z]| r.sector_mut(x, z)) else {
            continue;
        };
        let state = floor_class(units.as_ref(), y_offset, sector);
        let (Some(class), Some(floor)) = (state.class, sector.ground_floor_mut()) else {
            continue;
        };
        if floor.paint_locked {
            continue;
        }
        let material = rules
            .floor_for_height(class, state.height)
            .or_else(|| rules.floor_for(class));
        if let Some(material) = material {
            let before = floor.clone();
            paint_surface(floor, material);
            painted += usize::from(*floor != before);
        }
    }
    painted
}

/// Give a shaped sector its derived-wall material: the palette choice, else
/// the room's rule, else leave it so the material chain falls through.
pub fn stamp_wall_material(
    sector: &mut GridSector,
    palette: Option<ResourceId>,
    rules: &PaintRules,
) {
    if let Some(material) = palette.or(rules.wall) {
        sector.set_ground_wall_material(Some(material));
    }
}

/// The strata rule: a room with a wall-by-height rule gets a default band
/// set "Strata" with an absolute cut line at each threshold and the rule's
/// materials as its bands (a band that names none keeps the wall's own). An
/// existing "Strata" set is updated in place, so running a verb again never
/// piles up sets; a room whose default set is another one is left alone.
/// Returns whether the room changed.
pub fn ensure_strata(room: &mut GridRoom) -> bool {
    let rule = room.defaults.rules.wall_by_height;
    if rule.bands() < 2 {
        return false;
    }
    let strata = room.band_sets.iter().position(|s| s.name == "Strata");
    if let (Some(default), Some(strata)) = (room.defaults.bands, strata) {
        if usize::from(default) != strata {
            return false;
        }
    } else if room.defaults.bands.is_some() {
        return false;
    }
    let set = BandSet {
        name: "Strata".to_string(),
        lines: rule.lines().into_iter().map(Anchor::Abs).collect(),
        bands: (0..rule.bands())
            .map(|i| Band {
                material: rule.materials[i],
                ..Band::default()
            })
            .collect(),
    };
    let index = match strata {
        Some(i) => {
            let changed = room.band_sets[i] != set;
            room.band_sets[i] = set;
            if room.defaults.bands == Some(i as u8) {
                return changed;
            }
            i
        }
        None => {
            if room.band_sets.len() >= crate::grid::bands::MAX_BAND_SETS {
                return false;
            }
            room.band_sets.push(set);
            room.band_sets.len() - 1
        }
    };
    room.defaults.bands = Some(index as u8);
    true
}

/// The trim rule: a room with no default band set gets one whose first band,
/// at the foot of every wall, is the rule's material. Does nothing when the
/// rule is unset or the room already has a default set.
pub fn ensure_trim(room: &mut GridRoom) {
    let Some(material) = room.defaults.rules.trim else {
        return;
    };
    if room.defaults.bands.is_some() || room.band_sets.len() >= crate::grid::bands::MAX_BAND_SETS {
        return;
    }
    let set = BandSet {
        name: "Trim".to_string(),
        lines: vec![Anchor::Bottom(CLICK_UNITS)],
        bands: vec![
            Band {
                material: Some(material),
                v: BandV::FromBottom,
                ..Band::default()
            },
            Band::default(),
        ],
    };
    room.band_sets.push(set);
    room.defaults.bands = Some((room.band_sets.len() - 1) as u8);
}

/// Move a sector's ceiling by the same amount as each of its floor corners
/// moved (`deltas` per triangle, corner order `[NW, NE, SE, SW]`), keeping
/// headroom constant.
pub fn move_ceiling_with_floor(sector: &mut GridSector, deltas: [[i32; 4]; 2]) {
    let Some(ceiling) = sector.ground_ceiling_mut() else {
        return;
    };
    for (h, d) in ceiling.heights.iter_mut().zip(deltas[0]) {
        *h = h.saturating_add(d);
    }
    if let Some(b) = ceiling.heights_b.as_mut() {
        for (h, d) in b.iter_mut().zip(deltas[1]) {
            *h = h.saturating_add(d);
        }
    }
}

/// Ceiling corner heights `[triangle 0, triangle 1]` of a sector, `None`
/// without a ceiling.
pub fn ceiling_corners(sector: &GridSector) -> Option<[[i32; 4]; 2]> {
    let c = sector.ground_ceiling()?;
    Some([c.heights_for(0), c.heights_for(1)])
}

/// Hold floor corners at least one click under the ceiling they had.
/// `old` is the floor before the verb, `new` the heights it wants (both
/// `[triangle 0, triangle 1]`). Returns the held heights and whether any
/// corner was held back; a corner that was already at or over the limit keeps
/// moving only the way it was going.
pub fn clamp_under_ceiling(
    ceiling: [[i32; 4]; 2],
    old: [[i32; 4]; 2],
    new: [[i32; 4]; 2],
) -> ([[i32; 4]; 2], bool) {
    let mut out = new;
    let mut held = false;
    for t in 0..2 {
        for i in 0..4 {
            let limit = ceiling[t][i] - CLICK_UNITS;
            if new[t][i] > limit && new[t][i] > old[t][i] {
                out[t][i] = limit.max(old[t][i]);
                held = true;
            }
        }
    }
    (out, held)
}

/// Floor heights `[triangle 0, triangle 1]` of a sector.
pub fn floor_corners(sector: &GridSector) -> Option<[[i32; 4]; 2]> {
    let f = sector.ground_floor()?;
    Some([f.heights_for(0), f.heights_for(1)])
}

/// Write floor heights `[triangle 0, triangle 1]` back; the second set only
/// where the floor is split-level.
pub fn set_floor_corners(surface: &mut GridSurface, heights: [[i32; 4]; 2]) {
    surface.heights = heights[0];
    if surface.heights_b.is_some() {
        surface.heights_b = Some(heights[1]);
    }
}
