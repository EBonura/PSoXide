//! What the guest's heap must hold for a level, written next to the manifest.
//!
//! The editor Play guest links its room pool, its arenas and its code first and gives the guest
//! heap whatever is left of the RAM, so a level whose rooms are large leaves the heap short and the
//! guest runs out of heap in the middle of a load (a failed allocation, then a silent halt). The
//! cook knows what the heap must hold: the renderer's scratch is sized from the level's maxima, the
//! resident table from its room count. The guest build knows what the link left. This file carries
//! the first to the second: `heap-guard` in `tools/psoxide-hazard` compares them after the link and
//! refuses the build with a message when the heap is short.

use super::{PlaytestPackage, StreamedClass};
use psx_grid::render::GridRenderer;
use psx_grid::LevelView;

/// File the cook writes in the generated directory.
pub const HEAP_BUDGET_FILENAME: &str = "heap_budget.txt";

/// Bytes the heap keeps beyond the cooked need (the same figure `heap-guard` holds it to).
pub const MARGIN_BYTES: usize = 4096;
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
    #[must_use]
    pub fn need(&self) -> usize {
        self.renderer + self.resident_table + self.fixed
    }
}

/// Bytes of RAM the guest's image and heap share: `__heap_end - LOAD_ADDR` in `sdk/psoxide.ld`.
const GUEST_RAM_BYTES: usize = 0x1F_7F00 - 0x1_0000;

/// What the guest's image takes besides its room pool, measured on a linked guest and split into
/// the part that does not depend on the level and the part that does.
///
/// The guest links `.text`, `.data` and `.bss` into one region; `ROOM_POOL` is the largest static
/// in it and is sized from the cook (`GRID_ROOM_POOL_PAGES`). Everything else is either fixed
/// (code, the engine's statics, the scene, the VRAM runtime) or sized by the level's models,
/// clips, UI images and packet budget: the persistent asset arena
/// ([`persistent_arena_bytes`]) is the one that moves by hundreds of KiB, the rest by a few.
///
/// Measured 2026-11-12 on the toolkit (rib-room-h plus a 20 by 20 room, plain build with
/// `cd-stream-bench host-tty`), from the link of a guest that overflowed: `.text` 968,620 B,
/// `.data` 143,240 B, `.bss` 953,936 B of which `ROOM_POOL` 483,328 B, whose persistent arena was
/// 130 pages and 114 slots: the image without the pool was 1,582,480 B, 1,310,312 B without the
/// persistent arena. (The Citadel linked at 1.52 MB with its pool, a one-room starter at 1.47 MB.)
const IMAGE_FIXED_BYTES: usize = 1_310_312;
/// Bytes a level's other manifest tables and arenas may add beyond the toolkit's. The cook cannot
/// see the guest's `.data` or its small level-sized statics, so it assumes this many more bytes
/// than it measured: it refuses a level this much early rather than let the link refuse it. The
/// exact answer is the link (and `heap-guard` after it).
const IMAGE_SLACK_BYTES: usize = 32 * 1024;
/// Bytes of one persistent asset slot in the guest: its offset, length, read-job entry and the
/// streamer's bookkeeping (52 on the 32-bit guest, rounded up).
const PERSISTENT_SLOT_BYTES: usize = 56;

/// The guest's persistent asset arena for `package`: the page storage the PersistentGameplay assets
/// fill, and the table of every asset, as `manifest.rs` sizes them
/// (`PERSISTENT_ASSET_PAGE_COUNT`, `PERSISTENT_ASSET_SLOT_COUNT`). It shares its RAM with the menu's UI
/// image cache (a union), so it is the larger of the two.
#[must_use]
pub fn persistent_arena_bytes(package: &PlaytestPackage) -> usize {
    let pages = package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == StreamedClass::PersistentGameplay)
        .map(|asset| asset.bytes.len().next_multiple_of(4))
        .sum::<usize>()
        .div_ceil(2048)
        .max(1);
    let slots = package.assets.len().max(1);
    let gameplay = pages * 2048 + slots * PERSISTENT_SLOT_BYTES;
    let ui_chunks = package
        .assets
        .iter()
        .filter(|asset| asset.streamed_class == StreamedClass::UiImage);
    let ui = ui_chunks
        .clone()
        .map(|asset| asset.bytes.len())
        .max()
        .unwrap_or(0)
        * ui_chunks.count();
    gameplay.max(ui)
}

/// Bytes of RAM the guest's image takes without its room pool, as far as the cook can tell: what
/// was measured, the level's persistent arena, and a [`IMAGE_SLACK_BYTES`] allowance.
#[must_use]
pub fn image_bytes(package: &PlaytestPackage) -> usize {
    IMAGE_FIXED_BYTES + persistent_arena_bytes(package) + IMAGE_SLACK_BYTES
}

/// KiB of room pool the guest can hold for `package`, whatever the cook does with its rooms: the
/// RAM less the image, the heap the level needs ([`HeapBudget::need`], with every room resident,
/// the most it can be) and its margin. The cook decides what stays resident from this, not from
/// a fixed ceiling. Whole pages (2 KiB), `None` for a package without a grid world.
#[must_use]
pub fn pool_headroom_kib(package: &PlaytestPackage) -> Option<usize> {
    let budget = plan(package)?;
    let taken = image_bytes(package) + budget.need() + MARGIN_BYTES;
    Some(GUEST_RAM_BYTES.saturating_sub(taken) / 2048 * 2)
}

/// The cook's refusal for a level that cannot fit the guest, `None` when it fits. The check is an
/// estimate of the link ([`image_bytes`]) that errs high; the link-time `heap-guard` stays the
/// exact answer for the heap.
#[must_use]
pub fn refusal(package: &PlaytestPackage) -> Option<String> {
    let budget = plan(package)?;
    let image = image_bytes(package);
    let wanted = budget.pool + image + budget.need() + MARGIN_BYTES;
    (wanted > GUEST_RAM_BYTES).then(|| {
        let mut rooms: Vec<(usize, usize)> = package
            .world_geometry
            .room_chunks
            .iter()
            .enumerate()
            .map(|(room, chunk)| (room, chunk.len().div_ceil(2048)))
            .collect();
        rooms.sort_by_key(|&(room, pages)| (std::cmp::Reverse(pages), room));
        let biggest: Vec<String> = rooms
            .iter()
            .take(3)
            .map(|(room, pages)| format!("room {room} ({pages} pages, {} KiB)", pages * 2))
            .collect();
        format!(
            "grid world: level '{}' cannot fit the guest's {} KiB of RAM: its room pool alone is {} KiB (the rooms the guest holds at once, of {} rooms), the rest of the guest takes about {} KiB (fixed {} KiB, this level's asset arena {} KiB, an allowance of {} KiB the cook cannot see) and its heap needs {} KiB, {} KiB over. \
             The biggest rooms are {}. A level that cannot hold all its rooms at once should stream by flood (a flood radius in the world settings, and portals or separate pieces so that only the rooms around the player are wanted); the largest room ({} B of renderer scratch) sets the heap.",
            package.save_title.replace('\n', " "),
            GUEST_RAM_BYTES / 1024,
            budget.pool / 1024,
            budget.rooms,
            image / 1024,
            IMAGE_FIXED_BYTES / 1024,
            persistent_arena_bytes(package) / 1024,
            IMAGE_SLACK_BYTES / 1024,
            (budget.need() + MARGIN_BYTES) / 1024,
            (wanted - GUEST_RAM_BYTES).div_ceil(1024),
            biggest.join(", "),
            budget.renderer,
        )
    })
}

/// The budget of the cooked grid world, `None` for a package without one.
#[must_use]
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
#[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::starter;
    use crate::grid::surface::GridSurface;
    use crate::grid::GridWorld;
    use crate::ProjectDocument;

    /// A starter project made beside its base so shared asset paths resolve.
    fn starter_project(tag: &str) -> (std::path::PathBuf, ProjectDocument) {
        let projects = crate::projects_dir();
        let out = projects.join(format!("test-budget-{tag}-{}", std::process::id()));
        let path = starter::create_project(&projects.join(starter::STARTER_BASE), &out, "Test")
            .expect("the starter project is created");
        (
            out,
            ProjectDocument::load_from_path(path).expect("the starter loads"),
        )
    }

    /// The starter's world with `count` more rooms of `side` sectors, `gap` sectors apart along
    /// X (a gap of 1 or more leaves no shared wall, so no portal).
    fn world_of(project: &ProjectDocument, count: usize, side: usize, gap: usize) -> GridWorld {
        let mut world = project
            .active_scene()
            .world
            .grid()
            .expect("a grid world")
            .clone();
        let material = world.rooms[0].defaults.wall_material;
        for i in 0..count {
            let mut room = GridRoom::new_derived(format!("Piece {i}"), side, side);
            room.origin = [20 + (i * (side + gap + 2)) as i32, 0];
            room.defaults.wall_material = material;
            for z in 0..side {
                for x in 0..side {
                    let sector = room.sector_or_insert(x, z).unwrap();
                    sector.set_ground_floor(Some(GridSurface::flat_auto(0, material)));
                    sector.set_ground_ceiling(Some(GridSurface::flat_auto(2048, material)));
                }
            }
            world.rooms.push(room);
        }
        world
    }

    fn pool_pages(package: &PlaytestPackage) -> usize {
        package.world_geometry.pool.pool_pages
    }

    #[test]
    fn a_level_of_separate_pieces_over_the_headroom_streams_and_its_pool_fits_the_guest() {
        let (dir, mut project) = starter_project("islands");
        let world = world_of(&project, 9, 24, 3);
        *project.active_scene_mut().world.grid_mut().unwrap() = world;
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let total = package.world_geometry.pool.total_pages;
        let headroom = pool_headroom_kib(&package).expect("a grid world") / 2;
        assert!(
            total > headroom,
            "the pieces must total more than the guest can hold ({total} vs {headroom} pages)"
        );
        // Streams: the pool is a piece and a room, not the sum, and the guest can hold it.
        assert!(
            pool_pages(&package) < total / 2,
            "{} of {total}",
            pool_pages(&package)
        );
        assert!(pool_pages(&package) <= headroom);
        assert_eq!(refusal(&package), None);
    }

    #[test]
    fn a_level_that_wants_every_room_at_once_over_the_headroom_is_refused() {
        let (dir, mut project) = starter_project("whole");
        let mut world = world_of(&project, 9, 24, 3);
        // No flood, no ring, no draw distance: every room is wanted at once.
        world.stream = crate::grid::world::StreamRadii {
            draw_far: 0,
            active_radius: 0,
            lead_radius: 0,
            ring: false,
            flood: crate::grid::world::FloodSettings::DEFAULT,
        };
        *project.active_scene_mut().world.grid_mut().unwrap() = world;
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(package.is_none());
        let message = report.error_messages().join("; ");
        assert!(message.contains("cannot fit the guest"), "{message}");
    }

    #[test]
    fn a_package_the_cook_passes_is_one_the_estimate_of_the_link_accepts() {
        // The invariant the editor relies on: whatever build_package returns fits.
        let (dir, mut project) = starter_project("accept");
        for (count, side) in [(2usize, 8usize), (5, 16), (9, 24)] {
            let world = world_of(&project, count, side, 3);
            *project.active_scene_mut().world.grid_mut().unwrap() = world;
            if let (Some(package), _) = super::super::build_package(&project, &dir) {
                assert_eq!(refusal(&package), None, "{count} rooms of {side}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_headroom_is_the_ram_less_the_image_the_heap_and_the_margin() {
        let (dir, project) = starter_project("math");
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let budget = plan(&package).unwrap();
        let headroom = pool_headroom_kib(&package).unwrap() * 1024;
        assert!(headroom + image_bytes(&package) + budget.need() + MARGIN_BYTES <= GUEST_RAM_BYTES);
        assert!(
            headroom + 2048 + image_bytes(&package) + budget.need() + MARGIN_BYTES
                > GUEST_RAM_BYTES - 2048,
            "within a page of the RAM"
        );
        // The measured toolkit: 130 persistent pages and 114 slots, a pool of 483,328 B overflowed
        // the link by 66,960 B on a 1,998,848 B region; the estimate must call that a refusal.
        let toolkit_image =
            IMAGE_FIXED_BYTES + 130 * 2048 + 114 * PERSISTENT_SLOT_BYTES + IMAGE_SLACK_BYTES;
        assert!(toolkit_image + 483_328 > GUEST_RAM_BYTES);
        assert!(toolkit_image >= 1_582_480, "never below the measured image");
    }
}
