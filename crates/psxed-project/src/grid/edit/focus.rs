//! Aim an edit at one span of a column.
//!
//! The surface and level verbs (`create_surfaces`, `raise`, `ramp`, the height
//! setters, stairs) edit the ground span of a column, the lowest. A [`Focus`]
//! lets one of them act on another: for the duration of the edit, the span
//! open at a height stands in as the ground span of every column of a set of
//! cells, and the columns are put back in order afterwards
//! (`docs/grid/multi-storey.md`, 6.1: a tool names the storey it means and
//! never guesses). The edit sees an ordinary world; it needs no span
//! parameter of its own.
//!
//! A [`Focus`] is captured once, from the height and the document as they are
//! then, and fixes which span of each column is meant. A gesture that applies
//! its edit again and again (a drag) keeps its focus, so a floor that moves
//! out from under the height is still the span the drag began on.
//!
//! A column the edit leaves out of order, or changes the span count of,
//! refuses the whole call and the room is put back as it was. A span the edit
//! leaves too thin to stand in (a sliver) is filled solid afterwards, and the
//! number of them is reported, never silently dropped.

use super::super::sector::GridSector;
use super::super::storeys::normalize_column;
use super::super::world::GridWorld;
use super::rects::CellRect;
use super::spans::column_is_ordered;

/// Why an aimed edit changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusRefusal {
    /// The room index does not exist.
    NoSuchRoom,
    /// The room has a leaf finer or coarser than a sector.
    MixedCells,
    /// No span of the column at this sector is open at the height.
    NoSpan { cell: [i32; 2] },
    /// There is no height to aim at and the column at this sector holds
    /// several spans, so there is no telling which one is meant.
    SeveralSpans { cell: [i32; 2], count: usize },
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
            Self::SeveralSpans { cell, count } => format!(
                "sector [{}, {}] has {count} spans and no level is set to say which one is meant",
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

/// What an aimed edit gave back besides its own result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aimed<R> {
    /// What the edit returned.
    pub value: R,
    /// Spans the edit left too thin to stand in, filled solid.
    pub slivers: usize,
    /// The lowest floor height (grid units, room-relative) of the span the
    /// edit was aimed at, over the first column that holds several spans,
    /// after the edit. Where the cursor goes when it follows the edit; `None`
    /// when every column held one span.
    pub floor: Option<i32>,
}

/// A column that holds several spans, with the one the aim names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Focused {
    cell: [i32; 2],
    /// The span that stands in as the ground span.
    index: usize,
    /// How many spans the column held.
    count: usize,
}

/// Which span of each multi-span column an edit is aimed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Focus {
    room: usize,
    columns: Vec<Focused>,
}

impl Focus {
    /// Aim at the span open at `y` (room-relative grid units) in every
    /// column of `cells`, or, with no height, require that no column holds
    /// several spans.
    ///
    /// Cells without a column are left to the edit (it may create one); a
    /// column with no span open at `y` refuses. Only columns of several
    /// spans are tracked: a lone span is the ground span whatever the height.
    pub fn capture(
        world: &GridWorld,
        room: usize,
        cells: impl IntoIterator<Item = [i32; 2]>,
        y: Option<i32>,
    ) -> Result<Self, FocusRefusal> {
        let target = world.rooms.get(room).ok_or(FocusRefusal::NoSuchRoom)?;
        if !target.leaves.is_sectors_only() {
            return Err(FocusRefusal::MixedCells);
        }
        let mut columns = Vec::new();
        for cell in cells {
            let Some(sector) = target.local(cell).and_then(|[x, z]| target.sector(x, z)) else {
                continue;
            };
            if sector.is_single_span() {
                continue;
            }
            let count = sector.span_count();
            let Some(y) = y else {
                return Err(FocusRefusal::SeveralSpans { cell, count });
            };
            let index = sector.span_at(y, 0).ok_or(FocusRefusal::NoSpan { cell })?;
            columns.push(Focused { cell, index, count });
        }
        Ok(Self { room, columns })
    }

    /// An aim that changes nothing: every column is the one it names.
    pub fn none(room: usize) -> Self {
        Self {
            room,
            columns: Vec::new(),
        }
    }

    /// The room the focus is for.
    pub fn room(&self) -> usize {
        self.room
    }

    /// Whether any column holds several spans (the aim matters).
    pub fn is_aimed(&self) -> bool {
        !self.columns.is_empty()
    }

    /// The span of the column at `cell` the edit is aimed at; `0` for a
    /// column that holds one span or none.
    pub fn index_at(&self, cell: [i32; 2]) -> usize {
        self.columns
            .iter()
            .find(|c| c.cell == cell)
            .map_or(0, |c| c.index)
    }

    /// Put each focused span in the ground slot (a swap; doing it twice puts
    /// the column back).
    fn swap(&self, world: &mut GridWorld) {
        for f in &self.columns {
            if f.index != 0 {
                if let Some(sector) = sector_at(world, self.room, f.cell) {
                    sector.spans_mut().swap(0, f.index);
                }
            }
        }
    }

    /// A copy of the world with the focused spans in the ground slot: what
    /// an edit sees. A gesture that captures a snapshot of the cells it will
    /// edit takes it from this view, so a restore puts back the focused order
    /// and [`run`](Self::run) swaps it out again afterwards.
    pub fn view(&self, world: &GridWorld) -> GridWorld {
        let mut view = world.clone();
        self.swap(&mut view);
        view
    }

    /// Run `edit` with the focused spans standing in as the ground spans.
    /// On a refusal the room is exactly as it was.
    pub fn run<R>(
        &self,
        world: &mut GridWorld,
        edit: impl FnOnce(&mut GridWorld) -> R,
    ) -> Result<Aimed<R>, FocusRefusal> {
        if self.columns.is_empty() {
            return Ok(Aimed {
                value: edit(world),
                slivers: 0,
                floor: None,
            });
        }
        let backup = world
            .rooms
            .get(self.room)
            .ok_or(FocusRefusal::NoSuchRoom)?
            .clone();
        self.swap(world);
        let value = edit(world);
        let mut refusal = None;
        let mut floor = None;
        for f in &self.columns {
            let Some(sector) = sector_at(world, self.room, f.cell) else {
                refusal.get_or_insert(FocusRefusal::SpansChanged { cell: f.cell });
                continue;
            };
            if sector.span_count() != f.count {
                refusal.get_or_insert(FocusRefusal::SpansChanged { cell: f.cell });
                continue;
            }
            if f.index != 0 {
                sector.spans_mut().swap(0, f.index);
            }
            if !column_is_ordered(sector) {
                refusal.get_or_insert(FocusRefusal::OutOfOrder { cell: f.cell });
            }
            if floor.is_none() {
                floor = sector.spans()[f.index]
                    .floor
                    .as_ref()
                    .map(|s| s.height_range().0);
            }
        }
        if let Some(refusal) = refusal {
            world.rooms[self.room] = backup;
            return Err(refusal);
        }
        let mut slivers = 0;
        for f in &self.columns {
            if let Some(sector) = sector_at(world, self.room, f.cell) {
                slivers += normalize_column(sector);
            }
        }
        Ok(Aimed {
            value,
            slivers,
            floor,
        })
    }
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
) -> Result<Aimed<R>, FocusRefusal> {
    Focus::capture(world, room, rect.cells(), Some(y))?.run(world, edit)
}

fn sector_at(world: &mut GridWorld, room: usize, cell: [i32; 2]) -> Option<&mut GridSector> {
    let target = world.rooms.get_mut(room)?;
    let [x, z] = target.local(cell)?;
    target.sector_mut(x, z)
}
