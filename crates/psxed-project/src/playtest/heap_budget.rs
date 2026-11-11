//! What the guest's heap must hold for a level, written next to the manifest.
//!
//! The editor Play guest links its room pool, its arenas and its code first and gives the guest
//! heap whatever is left of the RAM, so a level whose rooms are large leaves the heap short and the
//! guest runs out of heap in the middle of a load (a failed allocation, then a silent halt). The
//! cook knows what the heap must hold: the renderer's scratch is sized from the level's maxima, the
//! resident table from its room count. The guest build knows what the link left. This file carries
//! the first to the second: `heap-guard` in `tools/psoxide-hazard` compares them after the link and
//! refuses the build with a message when the heap is short.

use super::PlaytestPackage;
use psx_grid::render::GridRenderer;
use psx_grid::LevelView;

/// File the cook writes in the generated directory.
pub const HEAP_BUDGET_FILENAME: &str = "heap_budget.txt";

/// Bytes of one resident room slot on the guest (`RoomSlot` plus its table entry).
const SLOT_BYTES: usize = 228;
/// Bytes of heap every level needs whatever its size: the level core's tables, the record decode
/// buffers, the chunk read buffer, the material lists. Fitted to the guest's heap in use after a
/// long walk (`ReuseHeap` bump pointer less its free space): verbs-demo (7 rooms) 48.5 KB with
/// 31.6 KB of renderer scratch and table, Citadel (93 rooms) 62.7 KB with 17.1 KB, grid-starter
/// (1 room) 3.7 KB with 1.7 KB. The line through the first two is 14.6 KB plus 334 B a room; the
/// constants round both up.
const FIXED_BYTES: usize = 16 * 1024;
/// Bytes of heap per room beyond [`FIXED_BYTES`]: the per-room tables and lists.
const ROOM_BYTES: usize = 360;

/// The sizes `heap_budget.txt` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapBudget {
    /// Renderer scratch, from the core's maxima.
    pub renderer: usize,
    /// The resident table: slots for every room the streamer tracks.
    pub resident_table: usize,
    /// Everything else, a constant.
    pub fixed: usize,
    /// The room pool the guest reserves in its BSS (not heap, listed because it competes with it).
    pub pool: usize,
    /// Rooms in the level.
    pub rooms: usize,
}

impl HeapBudget {
    /// Bytes of heap the level needs.
    pub fn need(&self) -> usize {
        self.renderer + self.resident_table + self.fixed
    }
}

/// The budget of the cooked grid world, `None` for a package without one.
pub fn plan(package: &PlaytestPackage) -> Option<HeapBudget> {
    let world = &package.world_geometry;
    let level = LevelView::parse(&world.core).ok()?;
    let rooms = world.room_chunks.len();
    let entries = (world.pool.max_rooms + world.pool.max_rooms / 2 + 8)
        .min(rooms)
        .max(1);
    Some(HeapBudget {
        renderer: GridRenderer::with_maxima(&level.maxima()).working_bytes(),
        resident_table: entries * SLOT_BYTES,
        fixed: FIXED_BYTES + ROOM_BYTES * rooms,
        pool: world.pool.pool_pages.max(1) * 2048,
        rooms,
    })
}

/// The text of `heap_budget.txt` for `package`: `key=value` lines, `need=0` when there is no grid
/// world, so a stale file from an earlier cook never gates this one.
pub fn render(package: &PlaytestPackage) -> String {
    match plan(package) {
        Some(b) => format!(
            "level={}\nneed={}\nrenderer={}\nresident_table={}\nfixed={}\npool={}\nrooms={}\n",
            package.save_title.replace('\n', " "),
            b.need(),
            b.renderer,
            b.resident_table,
            b.fixed,
            b.pool,
            b.rooms
        ),
        None => "need=0\n".to_string(),
    }
}
