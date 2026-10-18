//! Storey edits on the columns of spans: insert a slab, cut a hole, fill a
//! hole and shift everything above a height (`docs/grid/multi-storey.md`,
//! 4.6).
//!
//! Each verb is a pure function over one room and a rectangle of cells. It
//! changes what it can, counts what it left alone and says why, and a verb
//! that would break the column order refuses before it touches anything.
//! The editor and the MCP call it between an undo snapshot and the edit, so
//! one call is one undo step.
//!
//! Only whole-sector cells take part in S1. A room with fine leaves refuses
//! with [`SpanRefusal::MixedCells`] until the storey verbs learn the leaf
//! tree.

use crate::ResourceId;

use super::super::room::GridRoom;
use super::super::sector::{GridSector, GridSpan, WallEdge, MAX_SPANS_PER_CELL};
use super::super::storeys::{normalize_column, MIN_HEADROOM};
use super::super::surface::GridSurface;
use super::super::world::GridWorld;
use super::rects::CellRect;

/// Why one cell was left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanSkip {
    /// No span of the column contains the height, or the cell is not in the room.
    NoSpanAtHeight,
    /// A span's surface crosses the height over the cell, so a flat slab
    /// would cut through it.
    Sloped,
    /// The span above the new floor, or the floor left below the slab, would
    /// keep less than [`MIN_HEADROOM`].
    Headroom,
    /// The column already holds [`MAX_SPANS_PER_CELL`] spans.
    SpanCap,
    /// No flat floor sits at the height over the cell.
    NoSlab,
}

/// How many cells each reason skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpanSkips {
    pub no_span: usize,
    pub sloped: usize,
    pub headroom: usize,
    pub span_cap: usize,
    pub no_slab: usize,
}

impl SpanSkips {
    fn add(&mut self, skip: SpanSkip) {
        match skip {
            SpanSkip::NoSpanAtHeight => self.no_span += 1,
            SpanSkip::Sloped => self.sloped += 1,
            SpanSkip::Headroom => self.headroom += 1,
            SpanSkip::SpanCap => self.span_cap += 1,
            SpanSkip::NoSlab => self.no_slab += 1,
        }
    }

    /// Cells skipped for any reason.
    pub fn total(&self) -> usize {
        self.no_span + self.sloped + self.headroom + self.span_cap + self.no_slab
    }
}

/// Why a verb changed nothing at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanRefusal {
    /// The room index does not exist.
    NoSuchRoom,
    /// The room has a leaf finer or coarser than a sector.
    MixedCells,
    /// The slab thickness is negative or thicker than the headroom allows.
    BadThickness,
    /// The shifted columns would overlap at this cell.
    Interpenetrates { cell: [i32; 2] },
}

/// What a storey verb did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanOutcome {
    /// Cells changed.
    pub changed: usize,
    /// Cells left alone, by reason.
    pub skipped: SpanSkips,
    /// Set when nothing was done.
    pub refused: Option<SpanRefusal>,
}

impl SpanOutcome {
    fn refused(reason: SpanRefusal) -> Self {
        Self {
            refused: Some(reason),
            ..Self::default()
        }
    }
}

/// Which slabs a cut takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutScope {
    /// The slab whose floor is at the height.
    One,
    /// Every slab at or above the height.
    AllAbove,
}

fn floor_bounds(span: &GridSpan) -> Option<(i32, i32)> {
    span.floor.as_ref().map(GridSurface::height_range)
}

fn ceiling_bounds(span: &GridSpan) -> Option<(i32, i32)> {
    span.ceiling.as_ref().map(GridSurface::height_range)
}

/// Insert a slab into one column: its walking surface is flat at `y`, its
/// underside at `y - thickness`.
///
/// The span whose open volume holds `y` is split. The span below keeps its
/// floor and takes the slab underside as its ceiling; the span above takes the
/// slab top as its floor and keeps the old ceiling. If the span below would
/// keep less than [`MIN_HEADROOM`], it is a sliver: its floor rises to `y`
/// instead and no span is made below (the sliver rule). The span above must
/// keep [`MIN_HEADROOM`] under the old ceiling or the cell is skipped.
pub fn insert_slab_column(
    sector: &mut GridSector,
    y: i32,
    thickness: i32,
    material: Option<ResourceId>,
) -> Result<(), SpanSkip> {
    let mut found = None;
    for (index, span) in sector.spans().iter().enumerate() {
        let floor = floor_bounds(span);
        let ceiling = ceiling_bounds(span);
        let floor_below = floor.is_none_or(|(_, high)| high < y);
        let ceiling_above = ceiling.is_none_or(|(low, _)| low > y);
        if floor_below && ceiling_above {
            found = Some(index);
            break;
        }
        let crosses =
            floor.is_none_or(|(low, _)| low < y) && ceiling.is_none_or(|(_, high)| high > y);
        if crosses {
            return Err(SpanSkip::Sloped);
        }
    }
    let index = found.ok_or(SpanSkip::NoSpanAtHeight)?;
    let span = sector.spans()[index].clone();
    if ceiling_bounds(&span).is_some_and(|(low, _)| low - y < MIN_HEADROOM) {
        return Err(SpanSkip::Headroom);
    }
    let low = y - thickness;
    let lower_ok = floor_bounds(&span).is_none_or(|(_, high)| low - high >= MIN_HEADROOM);
    if !lower_ok {
        // The sliver rule: the floor rises to the slab top.
        sector.spans_mut()[index].floor = Some(GridSurface::flat_auto(y, material));
        return Ok(());
    }
    if sector.span_count() >= MAX_SPANS_PER_CELL {
        return Err(SpanSkip::SpanCap);
    }
    let underside = span
        .ceiling
        .as_ref()
        .and_then(|ceiling| ceiling.tri(0).material)
        .or(material);
    let upper = GridSpan {
        floor: Some(GridSurface::flat_auto(y, material)),
        ceiling: span.ceiling.clone(),
        wall_material: span.wall_material,
    };
    sector.spans_mut()[index].ceiling = Some(GridSurface::flat_auto(low, underside));
    sector.insert_span(index + 1, upper);
    Ok(())
}

/// Cut the slab (or slabs) at `y` out of one column: the span above a flat
/// floor at `y` merges into the span below it, taking its ceiling. Returns
/// how many slabs were cut.
pub fn cut_hole_column(
    sector: &mut GridSector,
    y: i32,
    scope: CutScope,
) -> Result<usize, SpanSkip> {
    let mut cut = 0;
    loop {
        let found = (1..sector.span_count()).find(|&index| {
            floor_bounds(&sector.spans()[index]).is_some_and(|(low, high)| {
                low == high
                    && match scope {
                        CutScope::One => low == y,
                        CutScope::AllAbove => low >= y,
                    }
            })
        });
        let Some(index) = found else {
            break;
        };
        let ceiling = sector.spans_mut()[index].ceiling.take();
        sector.spans_mut()[index - 1].ceiling = ceiling;
        sector.remove_span(index);
        cut += 1;
        if scope == CutScope::One {
            break;
        }
    }
    if cut == 0 {
        Err(SpanSkip::NoSlab)
    } else {
        Ok(cut)
    }
}

/// Add `delta` to every corner of `heights` at or above `y`.
fn shift_corners(heights: &mut [i32], y: i32, delta: i32) {
    for h in heights.iter_mut() {
        if *h >= y {
            *h += delta;
        }
    }
}

fn shift_surface(surface: &mut GridSurface, y: i32, delta: i32) {
    shift_corners(&mut surface.heights, y, delta);
    if let Some(heights) = surface.heights_b.as_mut() {
        shift_corners(heights, y, delta);
    }
}

/// Move every floor, ceiling and authored wall corner of a column at or above
/// `y` by `delta`. A surface that crosses `y` stretches.
pub fn shift_above_column(sector: &mut GridSector, y: i32, delta: i32) {
    for span in sector.spans_mut() {
        if let Some(floor) = span.floor.as_mut() {
            shift_surface(floor, y, delta);
        }
        if let Some(ceiling) = span.ceiling.as_mut() {
            shift_surface(ceiling, y, delta);
        }
    }
    for edge in WallEdge::ALL {
        for wall in sector.walls_mut(edge).iter_mut() {
            shift_corners(&mut wall.heights, y, delta);
        }
    }
}

/// Whether a column is in order: each span's floor is at or below its
/// ceiling, and each ceiling at or below the floor of the span above it.
pub fn column_is_ordered(sector: &GridSector) -> bool {
    let spans = sector.spans();
    for (index, span) in spans.iter().enumerate() {
        if let (Some((_, floor_high)), Some((ceiling_low, _))) =
            (floor_bounds(span), ceiling_bounds(span))
        {
            if floor_high > ceiling_low {
                return false;
            }
        }
        if let (Some((_, ceiling_high)), Some(next)) = (ceiling_bounds(span), spans.get(index + 1))
        {
            if let Some((next_low, _)) = floor_bounds(next) {
                if ceiling_high > next_low {
                    return false;
                }
            }
        }
    }
    true
}

fn with_room<T>(
    world: &mut GridWorld,
    room: usize,
    refuse: impl FnOnce(SpanRefusal) -> T,
    run: impl FnOnce(&mut GridRoom) -> T,
) -> T {
    let Some(target) = world.rooms.get_mut(room) else {
        return refuse(SpanRefusal::NoSuchRoom);
    };
    if !target.leaves.is_sectors_only() {
        return refuse(SpanRefusal::MixedCells);
    }
    run(target)
}

/// Insert a slab at `y` with `thickness` over the cells of `rect` (see
/// [`insert_slab_column`]). Cells whose column holds no span at `y` are
/// skipped and counted.
pub fn insert_slab(
    world: &mut GridWorld,
    room: usize,
    rect: CellRect,
    y: i32,
    thickness: i32,
    material: Option<ResourceId>,
) -> SpanOutcome {
    with_room(world, room, SpanOutcome::refused, |target| {
        if !(0..=MIN_HEADROOM).contains(&thickness) {
            return SpanOutcome::refused(SpanRefusal::BadThickness);
        }
        let mut outcome = SpanOutcome::default();
        for cell in rect.cells() {
            let Some([x, z]) = target.local(cell) else {
                outcome.skipped.no_span += 1;
                continue;
            };
            let Some(sector) = target.sector_mut(x, z) else {
                outcome.skipped.no_span += 1;
                continue;
            };
            match insert_slab_column(sector, y, thickness, material) {
                Ok(()) => outcome.changed += 1,
                Err(skip) => outcome.skipped.add(skip),
            }
        }
        outcome
    })
}

/// Cut the slab at `y` (or every slab at or above it) over `rect`. Cells
/// with no flat floor at the height are skipped and counted.
pub fn cut_hole(
    world: &mut GridWorld,
    room: usize,
    rect: CellRect,
    y: i32,
    scope: CutScope,
) -> SpanOutcome {
    with_room(world, room, SpanOutcome::refused, |target| {
        let mut outcome = SpanOutcome::default();
        for cell in rect.cells() {
            let Some([x, z]) = target.local(cell) else {
                outcome.skipped.no_slab += 1;
                continue;
            };
            let Some(sector) = target.sector_mut(x, z) else {
                outcome.skipped.no_slab += 1;
                continue;
            };
            match cut_hole_column(sector, y, scope) {
                Ok(_) => outcome.changed += 1,
                Err(skip) => outcome.skipped.add(skip),
            }
        }
        outcome
    })
}

/// Fill a hole: the inverse of [`cut_hole`], a slab inserted at `y` where the
/// column is open through it. The same primitive as [`insert_slab`]; the
/// separate name is the Slab verb's Fill mode.
pub fn fill_hole(
    world: &mut GridWorld,
    room: usize,
    rect: CellRect,
    y: i32,
    thickness: i32,
    material: Option<ResourceId>,
) -> SpanOutcome {
    insert_slab(world, room, rect, y, thickness, material)
}

/// Move every surface at or above `y` in `room` by `delta` (grid units), over
/// `rect` or the whole room. Refuses before changing anything if a shifted
/// column would overlap itself.
pub fn shift_above(
    world: &mut GridWorld,
    room: usize,
    rect: Option<CellRect>,
    y: i32,
    delta: i32,
) -> SpanOutcome {
    with_room(world, room, SpanOutcome::refused, |target| {
        let mut next: Vec<([usize; 2], GridSector)> = Vec::new();
        let origin = target.origin;
        for (x, z, sector) in target.iter() {
            let cell = [origin[0] + x as i32, origin[1] + z as i32];
            if rect.is_some_and(|rect| !rect.contains(cell)) {
                continue;
            }
            let mut shifted = sector.clone();
            shift_above_column(&mut shifted, y, delta);
            if !column_is_ordered(&shifted) {
                return SpanOutcome::refused(SpanRefusal::Interpenetrates { cell });
            }
            if shifted != *sector {
                next.push(([x, z], shifted));
            }
        }
        let mut outcome = SpanOutcome::default();
        for ([x, z], sector) in next {
            if let Some(slot) = target.sector_mut(x, z) {
                *slot = sector;
                outcome.changed += 1;
            }
        }
        outcome
    })
}

/// Remove the slivers of every column of `rect` (or of the room), see
/// [`normalize_column`]. Returns how many spans went.
pub fn normalize_room(world: &mut GridWorld, room: usize, rect: Option<CellRect>) -> usize {
    let Some(target) = world.rooms.get_mut(room) else {
        return 0;
    };
    let origin = target.origin;
    let cells: Vec<[usize; 2]> = target.iter().map(|(x, z, _)| [x, z]).collect();
    let mut removed = 0;
    for [x, z] in cells {
        let cell = [origin[0] + x as i32, origin[1] + z as i32];
        if rect.is_some_and(|rect| !rect.contains(cell)) {
            continue;
        }
        if let Some(sector) = target.sector_mut(x, z) {
            removed += normalize_column(sector);
        }
    }
    removed
}
