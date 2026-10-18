//! Aim an edit at one span of a column.
//!
//! The surface and level verbs (`create_surfaces`, `raise`, `ramp`, the height
//! setters) edit the ground span of a column, the lowest. [`with_span_at`]
//! lets one of them act on another: for the duration of the edit, the span
//! open at a height stands in as the ground span of every column of a
//! rectangle, and the columns are put back in order afterwards
//! (`docs/grid/multi-storey.md`, 6.1: a tool names the storey it means and
//! never guesses). The edit sees an ordinary world; it needs no span
//! parameter of its own.
//!
//! A column the edit leaves out of order, or changes the span count of,
//! refuses the whole call and the room is put back as it was.

use super::super::sector::GridSector;
use super::super::world::GridWorld;
use super::rects::CellRect;
use super::spans::column_is_ordered;

/// Why [`with_span_at`] changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusRefusal {
    /// The room index does not exist.
    NoSuchRoom,
    /// The room has a leaf finer or coarser than a sector.
    MixedCells,
    /// No span of the column at this sector is open at the height.
    NoSpan { cell: [i32; 2] },
    /// The edit left the column at this sector out of order.
    OutOfOrder { cell: [i32; 2] },
    /// The edit added or removed a span of the column at this sector.
    SpansChanged { cell: [i32; 2] },
}

impl FocusRefusal {
    /// The refusal as a sentence.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "no such room".to_string(),
            Self::MixedCells => {
                "the room has cells of several sizes: aiming at a span works on whole sectors"
                    .to_string()
            }
            Self::NoSpan { cell } => format!(
                "sector [{}, {}] has no span open at that height; read it with grid_column",
                cell[0], cell[1]
            ),
            Self::OutOfOrder { cell } => format!(
                "the edit would leave the column at sector [{}, {}] out of order (a floor through the slab above it or below it); nothing changed",
                cell[0], cell[1]
            ),
            Self::SpansChanged { cell } => format!(
                "the edit would add or remove a span of the column at sector [{}, {}]; nothing changed",
                cell[0], cell[1]
            ),
        }
    }
}

/// A column the edit runs with another span in its ground slot.
struct Focused {
    cell: [i32; 2],
    /// The span that stands in as the ground span.
    index: usize,
    /// How many spans the column held.
    count: usize,
}

/// Run `edit` with the span of each column of `rect` that is open at `y`
/// (room-relative grid units) standing in as the column's ground span.
///
/// Sectors of `rect` without a column are left to the edit (it may create
/// one); a column with no span open at `y` refuses the call. On a refusal the
/// room is exactly as it was.
pub fn with_span_at<R>(
    world: &mut GridWorld,
    room: usize,
    rect: CellRect,
    y: i32,
    edit: impl FnOnce(&mut GridWorld) -> R,
) -> Result<R, FocusRefusal> {
    let target = world.rooms.get(room).ok_or(FocusRefusal::NoSuchRoom)?;
    if !target.leaves.is_sectors_only() {
        return Err(FocusRefusal::MixedCells);
    }
    let mut focused: Vec<Focused> = Vec::new();
    for cell in rect.cells() {
        let Some(sector) = target.local(cell).and_then(|[x, z]| target.sector(x, z)) else {
            continue;
        };
        let index = sector.span_at(y, 0).ok_or(FocusRefusal::NoSpan { cell })?;
        if index != 0 {
            focused.push(Focused {
                cell,
                index,
                count: sector.span_count(),
            });
        }
    }
    let backup = target.clone();
    for f in &focused {
        if let Some(sector) = sector_at(world, room, f.cell) {
            sector.spans_mut().swap(0, f.index);
        }
    }
    let result = edit(world);
    let mut refusal = None;
    for f in &focused {
        let Some(sector) = sector_at(world, room, f.cell) else {
            refusal.get_or_insert(FocusRefusal::SpansChanged { cell: f.cell });
            continue;
        };
        if sector.span_count() != f.count {
            refusal.get_or_insert(FocusRefusal::SpansChanged { cell: f.cell });
            continue;
        }
        sector.spans_mut().swap(0, f.index);
        if !column_is_ordered(sector) {
            refusal.get_or_insert(FocusRefusal::OutOfOrder { cell: f.cell });
        }
    }
    match refusal {
        None => Ok(result),
        Some(refusal) => {
            world.rooms[room] = backup;
            Err(refusal)
        }
    }
}

fn sector_at(world: &mut GridWorld, room: usize, cell: [i32; 2]) -> Option<&mut GridSector> {
    let target = world.rooms.get_mut(room)?;
    let [x, z] = target.local(cell)?;
    target.sector_mut(x, z)
}
