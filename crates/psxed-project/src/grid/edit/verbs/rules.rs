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

/// The slope class of a sector's floor, or `None` without a floor or a
/// cookable scale.
pub fn floor_class(
    units: Option<&RuntimeUnits>,
    y_offset: i32,
    sector: &GridSector,
) -> Option<SlopeClass> {
    let floor = sector.floor.as_ref()?;
    Some(surface_class(units?, y_offset, floor, &WalkLimits::DEFAULT))
}

/// Set the material of both triangles of `surface`.
pub fn paint_surface(surface: &mut GridSurface, material: ResourceId) {
    surface.tri_a.material = Some(material);
    if let Some(b) = surface.tri_b.as_mut() {
        b.material = Some(material);
    }
}

/// Apply the floor rule to a sector the verb shaped, when its slope class
/// changed (`before`) and its floor was not painted by hand.
pub fn apply_floor_rule(
    rules: &PaintRules,
    units: Option<&RuntimeUnits>,
    y_offset: i32,
    before: Option<SlopeClass>,
    sector: &mut GridSector,
) {
    let after = floor_class(units, y_offset, sector);
    if before == after {
        return;
    }
    let (Some(class), Some(floor)) = (after, sector.floor.as_mut()) else {
        return;
    };
    if floor.paint_locked {
        return;
    }
    if let Some(material) = rules.floor_for(class) {
        paint_surface(floor, material);
    }
}

/// Give a shaped sector its derived-wall material: the palette choice, else
/// the room's rule, else leave it so the material chain falls through.
pub fn stamp_wall_material(
    sector: &mut GridSector,
    palette: Option<ResourceId>,
    rules: &PaintRules,
) {
    if let Some(material) = palette.or(rules.wall) {
        sector.wall_material = Some(material);
    }
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
    let Some(ceiling) = sector.ceiling.as_mut() else {
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
    let c = sector.ceiling.as_ref()?;
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
    let f = sector.floor.as_ref()?;
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
