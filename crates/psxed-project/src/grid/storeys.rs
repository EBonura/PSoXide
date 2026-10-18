//! Storeys: the floors stacked in the columns of a world.
//!
//! A column of spans holds its floors itself (`docs/grid/multi-storey.md`,
//! 4.2 invariants and 4.7). A storey is not stored: [`storeys`] reads the
//! floor heights of every span and keeps the ones many cells share. Heights
//! are grid units, the same as everywhere in the grid; the constants below
//! are the ones the design recorded and Manny accepted.

use std::collections::BTreeMap;

use super::sector::GridSector;
use super::units::CLICK_UNITS;
use super::world::GridWorld;

/// Thickness of a slab the storey verbs insert: one click (20 runtime units).
pub const SLAB_THICKNESS: i32 = CLICK_UNITS;

/// Least headroom a span keeps at every corner: three clicks, 60 runtime
/// units, the 48 unit body with a margin. A thinner span is a sliver.
pub const MIN_HEADROOM: i32 = 3 * CLICK_UNITS;

/// Cells that must share a floor height before [`storeys`] calls it a storey.
pub const STOREY_MIN_CELLS: usize = 8;

/// The storey pitch of the test level: twelve clicks, 240 runtime units.
pub const STOREY_PITCH: i32 = 12 * CLICK_UNITS;

/// One storey: a floor height that many leaves of the world share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Storey {
    /// Floor height in grid units, room offsets included.
    pub height: i32,
    /// Leaves with a floor at that height, over every room.
    pub cells: usize,
}

/// The storeys of `world`, lowest first.
///
/// Every span floor counts once per leaf it sits on, in a histogram of its
/// height rounded to the click. A height with at least
/// [`STOREY_MIN_CELLS`] is a candidate, and candidates less than a slab and
/// the minimum headroom apart (four clicks) are one storey, the one with
/// more cells winning. Stairs put one or two cells at each step, so they
/// never make a storey.
pub fn storeys(world: &GridWorld) -> Vec<Storey> {
    let mut counts: BTreeMap<i32, usize> = BTreeMap::new();
    for room in &world.rooms {
        for (_, leaf) in room.leaves.iter() {
            for span in leaf.spans() {
                let Some(floor) = span.floor.as_ref() else {
                    continue;
                };
                let low = floor.height_range().0 + room.y_offset;
                *counts.entry(click_of(low)).or_default() += 1;
            }
        }
    }
    let gap = (MIN_HEADROOM + SLAB_THICKNESS) / CLICK_UNITS;
    let mut out: Vec<(i32, usize)> = Vec::new();
    for (click, cells) in counts {
        if cells < STOREY_MIN_CELLS {
            continue;
        }
        match out.last_mut() {
            Some(last) if click - last.0 < gap => {
                if cells > last.1 {
                    *last = (click, cells);
                }
            }
            _ => out.push((click, cells)),
        }
    }
    out.into_iter()
        .map(|(click, cells)| Storey {
            height: click * CLICK_UNITS,
            cells,
        })
        .collect()
}

/// The lowest storey floor above `y`, grid units with room offsets included.
pub fn storey_above(storeys: &[Storey], y: i32) -> Option<i32> {
    storeys.iter().map(|s| s.height).find(|&h| h > y)
}

/// The highest storey floor below `y`.
pub fn storey_below(storeys: &[Storey], y: i32) -> Option<i32> {
    storeys.iter().rev().map(|s| s.height).find(|&h| h < y)
}

/// The storey a surface at `y` belongs to: the highest floor at or below
/// `y + slack`. A little slack lets a surface just under a floor's height
/// (a stair landing, a rounded hit point) count as on it, while a ceiling a
/// click under the next floor still belongs to the storey below.
pub fn storey_at(storeys: &[Storey], y: i32, slack: i32) -> Option<i32> {
    let limit = y.saturating_add(slack);
    storeys.iter().rev().map(|s| s.height).find(|&h| h <= limit)
}

/// Height rounded to the nearest click, as a click index.
fn click_of(height: i32) -> i32 {
    (height + CLICK_UNITS / 2).div_euclid(CLICK_UNITS)
}

/// Whether a span is too thin to stand in: its headroom is under
/// [`MIN_HEADROOM`] at every corner. Conservative: a span is judged by its
/// highest ceiling over its lowest floor, which is never more than the true
/// headroom of any corner, so a span removed here is a sliver at every corner.
/// A span with no floor (a pit) or no ceiling (open sky) is never a sliver.
pub fn is_sliver(sector: &GridSector, index: usize) -> bool {
    let span = &sector.spans()[index];
    match (span.floor.as_ref(), span.ceiling.as_ref()) {
        (Some(floor), Some(ceiling)) => {
            ceiling.height_range().1 - floor.height_range().0 < MIN_HEADROOM
        }
        _ => false,
    }
}

/// Remove the slivers of a column (the sliver rule of `multi-storey.md` 4.2):
/// the solid fills their place. A column keeps at least one span, so a
/// lone sliver stays. Returns the number of spans removed.
pub fn normalize_column(sector: &mut GridSector) -> usize {
    let mut removed = 0;
    let mut index = 0;
    while sector.span_count() > 1 && index < sector.span_count() {
        if is_sliver(sector, index) {
            sector.remove_span(index);
            removed += 1;
        } else {
            index += 1;
        }
    }
    removed
}
