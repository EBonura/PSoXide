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
use super::super::storeys::MIN_HEADROOM;
use super::super::surface::GridSurface;
use super::super::units::CLICK_UNITS;
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
    /// Count one more cell skipped for `skip`.
    pub fn add(&mut self, skip: SpanSkip) {
        match skip {
            SpanSkip::NoSpanAtHeight => self.no_span += 1,
            SpanSkip::Sloped => self.sloped += 1,
            SpanSkip::Headroom => self.headroom += 1,
            SpanSkip::SpanCap => self.span_cap += 1,
            SpanSkip::NoSlab => self.no_slab += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.no_span += other.no_span;
        self.sloped += other.sloped;
        self.headroom += other.headroom;
        self.span_cap += other.span_cap;
        self.no_slab += other.no_slab;
    }

    /// Cells skipped for any reason.
    pub fn total(&self) -> usize {
        self.no_span + self.sloped + self.headroom + self.span_cap + self.no_slab
    }

    /// The counts as words, for a reply: "4 with no span at that height, 2
    /// at the span cap". "none" when nothing was skipped.
    pub fn describe(&self) -> String {
        let parts: Vec<String> = [
            (self.no_span, "with no span at that height".to_string()),
            (self.sloped, "where a sloped surface crosses it".to_string()),
            (
                self.headroom,
                format!(
                    "with less than {} clicks of headroom",
                    MIN_HEADROOM / CLICK_UNITS
                ),
            ),
            (self.span_cap, "at the span cap".to_string()),
            (self.no_slab, "with no slab at that height".to_string()),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(", ")
        }
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

impl SpanRefusal {
    /// The refusal as a sentence.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "no such room".to_string(),
            Self::MixedCells => {
                "the room has cells of several sizes: the storey verbs work on whole sectors"
                    .to_string()
            }
            Self::BadThickness => format!(
                "the slab thickness must be between 0 and the minimum headroom ({MIN_HEADROOM} grid units)"
            ),
            Self::Interpenetrates { cell } => format!(
                "the shift would make the column at sector [{}, {}] overlap itself; nothing changed",
                cell[0], cell[1]
            ),
        }
    }
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

    /// Fold the outcome of another rectangle into this one. The first
    /// refusal is kept.
    pub fn merge(&mut self, other: Self) {
        self.changed += other.changed;
        self.skipped.merge(other.skipped);
        self.refused = self.refused.or(other.refused);
    }
}

/// Which slabs a cut takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutScope {
    /// The slab whose floor is at the height.
    One,
    /// The first slab whose floor is above the height: the one over a body
    /// standing at it.
    Next,
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
    // A slab keeps the diagonal and the void triangles of the span it splits:
    // the half cell of a chamfer is empty above and below the slab too.
    let surface_like = |height: i32, material: Option<ResourceId>, from: Option<&GridSurface>| {
        let mut surface = GridSurface::flat_auto(height, material);
        if let Some(from) = from.filter(|from| from.void != 0) {
            surface.split = from.split;
            surface.void = from.void;
            surface.auto_split = false;
        }
        surface
    };
    let like_floor = span.floor.as_ref().or(span.ceiling.as_ref());
    let like_ceiling = span.ceiling.as_ref().or(span.floor.as_ref());
    if !lower_ok {
        // The sliver rule: the floor rises to the slab top.
        sector.spans_mut()[index].floor = Some(surface_like(y, material, like_floor));
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
        floor: Some(surface_like(y, material, like_floor)),
        ceiling: span.ceiling.clone(),
        wall_material: span.wall_material,
    };
    sector.spans_mut()[index].ceiling = Some(surface_like(low, underside, like_ceiling));
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
                        CutScope::Next => low > y,
                        CutScope::AllAbove => low >= y,
                    }
            })
        });
        let Some(index) = found else {
            break;
        };
        remove_boundary(sector, index);
        cut += 1;
        if scope != CutScope::AllAbove {
            break;
        }
    }
    if cut == 0 {
        Err(SpanSkip::NoSlab)
    } else {
        Ok(cut)
    }
}

/// Remove the boundary between span `index - 1` and span `index`: the
/// floor of the span above and the ceiling of the span below go, and the two
/// merge into one open volume that takes the upper span's ceiling. This is
/// the slab between them cut out, whatever its shape. Returns whether there
/// was a boundary (`index` is a span above the ground span).
pub fn remove_boundary(sector: &mut GridSector, index: usize) -> bool {
    if index == 0 || index >= sector.span_count() {
        return false;
    }
    let ceiling = sector.spans_mut()[index].ceiling.take();
    sector.spans_mut()[index - 1].ceiling = ceiling;
    sector.remove_span(index);
    true
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

/// The room a verb works in: it must exist and hold whole-sector cells only.
fn room_of(world: &mut GridWorld, room: usize) -> Result<&mut GridRoom, SpanRefusal> {
    let target = world.rooms.get_mut(room).ok_or(SpanRefusal::NoSuchRoom)?;
    if target.leaves.is_sectors_only() {
        Ok(target)
    } else {
        Err(SpanRefusal::MixedCells)
    }
}

/// Run `edit` on the column of every cell of `rect`. A cell outside the room
/// or without a column is skipped for `missing`; a column `edit` leaves alone
/// is skipped for the reason it gives.
fn over_cells(
    target: &mut GridRoom,
    rect: CellRect,
    missing: SpanSkip,
    mut edit: impl FnMut(&mut GridSector) -> Result<(), SpanSkip>,
) -> SpanOutcome {
    let mut outcome = SpanOutcome::default();
    for cell in rect.cells() {
        let result = match target.local(cell) {
            Some([x, z]) => target.sector_mut(x, z).map_or(Err(missing), &mut edit),
            None => Err(missing),
        };
        match result {
            Ok(()) => outcome.changed += 1,
            Err(skip) => outcome.skipped.add(skip),
        }
    }
    outcome
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
    let target = match room_of(world, room) {
        Ok(target) => target,
        Err(reason) => return SpanOutcome::refused(reason),
    };
    if !(0..=MIN_HEADROOM).contains(&thickness) {
        return SpanOutcome::refused(SpanRefusal::BadThickness);
    }
    over_cells(target, rect, SpanSkip::NoSpanAtHeight, |sector| {
        insert_slab_column(sector, y, thickness, material)
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
    let target = match room_of(world, room) {
        Ok(target) => target,
        Err(reason) => return SpanOutcome::refused(reason),
    };
    over_cells(target, rect, SpanSkip::NoSlab, |sector| {
        cut_hole_column(sector, y, scope).map(|_| ())
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
    let target = match room_of(world, room) {
        Ok(target) => target,
        Err(reason) => return SpanOutcome::refused(reason),
    };
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
}
