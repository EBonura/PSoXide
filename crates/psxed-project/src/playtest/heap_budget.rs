//! What the guest's heap must hold for a level, written next to the manifest.
//!
//! The editor Play guest links its room pool, its arenas and its code first and gives the guest
//! heap whatever is left of the RAM, so a level whose rooms are large leaves the heap short and the
//! guest runs out of heap in the middle of a load (a failed allocation, then a silent halt). The
//! cook knows what the heap must hold: the renderer's scratch is sized from the level's maxima, the
//! resident table from its room count. The guest build knows what the link left. This file carries
//! the first to the second: `heap-guard` in `tools/psoxide-hazard` compares them after the link and
//! refuses the build with a message when the heap is short.
//!
//! The cook runs the same check before the link: the heap is the RAM less the guest image, and the
//! cook can say how big the image will be ([`image_bytes`]: the room pool, the persistent asset
//! arena, and the code and arrays each table of the level's records pulls in), so `cook_check` and
//! Play refuse a level whose heap would be short with the guard's own arithmetic
//! (`psoxide_hazard::heap_guard::verdict`) and the guard's numbers. A level the cook passes is one
//! the guard passes, to within the model's error (the guard prints it on every build).

use super::{PlaytestPackage, StreamedClass};
use psx_grid::render::GridRenderer;
use psx_grid::LevelView;

/// File the cook writes in the generated directory.
pub const HEAP_BUDGET_FILENAME: &str = "heap_budget.txt";

/// Bytes the heap keeps beyond the cooked need (the same figure `heap-guard` holds it to).
pub const MARGIN_BYTES: usize = psoxide_hazard::heap_guard::MARGIN_BYTES as usize;
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
const GUEST_RAM_BYTES: usize = psoxide_hazard::heap_guard::RAM_BYTES as usize;

/// What the guest's image takes besides its room pool, split into what does not depend on the
/// level, what the level's asset arena takes and what each table of the level's records pulls in.
///
/// The guest links `.text`, `.data` and `.bss` into one region; `ROOM_POOL` is the largest static
/// in it and is sized from the cook (`GRID_ROOM_POOL_PAGES`). The persistent asset arena
/// ([`persistent_arena_bytes`]) moves by hundreds of KiB. A table of the level's records the level
/// has none of is the empty slice, a constant, so the code that walks it is left out of the image
/// (`engine/examples/editor-playtest/src/level_data.rs`): a table with one record pulls in its
/// whole system, and one more record only adds the table's arrays ([`TABLE_IMAGE`]).
///
/// Measured 2026-11-12 on the toolkit with the Laser tunnel's checkpoint and fragments
/// (`cd-stream-bench`, plain build): the linked image was 1,801,104 B, of which the pool 149,504 B,
/// the persistent arena 297,816 B (142 pages, 125 slots), the tables below 71,910 B and the chunk
/// sky 50,848 B ([`CHUNK_SKY_CODE_BYTES`] and its data); the rest, with every table empty and no
/// sky, is this constant. `psoxide-hazard`'s `heap-guard` prints the model's
/// error against the link on every build.
const IMAGE_FIXED_BYTES: usize = 1_248_000;
/// Bytes the model may be short by: what `.data` and the small level-sized statics of another level
/// may add beyond the tables below, plus the layout noise of a different link (about 1 KiB between
/// builds of the same level). The cook assumes this many more bytes than it modelled: it refuses a
/// level this much early rather than let the link refuse it.
const IMAGE_SLACK_BYTES: usize = 16 * 1024;

/// What a table of the level's records adds to the image: `code` bytes once it holds any record,
/// `per_record` bytes of arrays for each record. Measured by linking the toolkit's cooked manifest
/// with the table's length changed (0, 1 and 8 to 30 records), plain build. Tables not named
/// (rooms, messages, pages, slots, surfaces, collisions) ride with the table that owns them.
const TABLE_IMAGE: &[(&str, usize, usize)] = &[
    ("room_reflection_probes", 800, 0),
    ("lights", 400, 0),
    ("particle_emitters", 4_600, 0),
    ("interactables", 25_200, 30),
    ("vitality_circles", 8_900, 0),
    ("logic", 18_300, 300),
    ("visual_links", 800, 0),
    ("game_entities", 68_500, 2_500),
    ("entities", 24_600, 60),
    ("destructibles", 1_500, 0),
    ("world_objects", 19_000, 0),
    ("image_props", 20_300, 0),
    // Box props are not linear (1: 62.1 KB, 8: 89.1 KB, 30: 136.2 KB over none), so the line is
    // drawn above all three.
    ("box_props", 68_000, 2_600),
    ("cylinder_props", 25_100, 0),
    ("arch_props", 22_900, 0),
    ("model_instances", 30_900, 920),
    ("grid_uv_motions", 600, 20),
];
/// What a world that bakes chunk skies adds besides the sky data itself (the background rows and
/// runs and the initial mask, which sit in the image too): the streaming window the sky's reads
/// land in (33,540 B) and the code that steps it. Measured by emptying the toolkit's three sky
/// tables: 50,848 B in all, 11,834 B of it the toolkit's data.
const CHUNK_SKY_CODE_BYTES: usize = 39_100;
/// What a model instance adds on top of its own table when enemies exist as well: the two systems
/// share less code than their separate costs suggest (the toolkit with one enemy, both enemy
/// policies and one model instance linked 31 KB above the sum of the four measured alone).
const ACTOR_MODEL_SURCHARGE_BYTES: usize = 31_200;
/// What a level with enemies adds beyond the sum of its tables: the template levels with an enemy
/// linked 34 KB above the sum of the tables measured one at a time (a toolkit manifest edited to
/// their shape agrees), the actor code the tables share.
const ENEMY_BUNDLE_BYTES: usize = 34_500;
/// What an enemy using the tactical policy (`ANY_TACTICAL_ENTITY`) adds to the image.
const TACTICAL_CODE_BYTES: usize = 26_400;
/// What a training-pen enemy (`ANY_TRAINING_ENTITY`) adds to the image.
const TRAINING_CODE_BYTES: usize = 11_400;

/// Bytes of the image the level's record tables, enemy systems and chunk sky add
/// ([`TABLE_IMAGE`]).
#[must_use]
pub fn table_bytes(package: &PlaytestPackage) -> usize {
    let any = |flag: u16| package.game_entities.iter().any(|e| e.flags & flag != 0);
    let sky = &package.world_geometry.sky_tables;
    let sky_bytes = if sky.background_rows.is_empty() {
        0
    } else {
        CHUNK_SKY_CODE_BYTES
            + sky.background_rows.len() * 2
            + sky.background_runs.len()
            + sky.initial_mask.len() * 4
    };
    sky_bytes
        + table_bytes_of(
            &package.world_geometry.record_lens,
            any(psx_level::game_entity_flags::TACTICAL),
            any(psx_level::game_entity_flags::TRAINING),
        )
}

/// [`table_bytes`] from the record counts (by `psx_level::wire::TABLE_NAMES` order) and the two
/// enemy flags.
#[must_use]
pub fn table_bytes_of(lens: &[usize], tactical: bool, training: bool) -> usize {
    let mut bytes = 0;
    for &(name, code, per_record) in TABLE_IMAGE {
        let count = psx_level::wire::TABLE_NAMES
            .iter()
            .position(|table| *table == name)
            .and_then(|i| lens.get(i))
            .copied()
            .unwrap_or(0);
        if count > 0 {
            bytes += code + per_record * count;
        }
    }
    let count = |name: &str| {
        psx_level::wire::TABLE_NAMES
            .iter()
            .position(|table| *table == name)
            .and_then(|i| lens.get(i))
            .copied()
            .unwrap_or(0)
    };
    if count("game_entities") > 0 {
        bytes += ENEMY_BUNDLE_BYTES;
        if count("model_instances") > 0 {
            bytes += ACTOR_MODEL_SURCHARGE_BYTES;
        }
    }
    if tactical {
        bytes += TACTICAL_CODE_BYTES;
    }
    if training {
        bytes += TRAINING_CODE_BYTES;
    }
    bytes
}

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
/// was measured, the level's persistent arena, what its tables pull in, and a
/// [`IMAGE_SLACK_BYTES`] allowance.
#[must_use]
pub fn image_bytes(package: &PlaytestPackage) -> usize {
    modelled_image_bytes(package) + IMAGE_SLACK_BYTES
}

/// [`image_bytes`] without the allowance: the model's own figure, which `heap_budget.txt` carries
/// so `heap-guard` can print the model's error against the link.
#[must_use]
pub fn modelled_image_bytes(package: &PlaytestPackage) -> usize {
    IMAGE_FIXED_BYTES + persistent_arena_bytes(package) + table_bytes(package)
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

/// The guard's verdict on `package` before the link: the heap starts after the image the cook
/// estimates ([`image_bytes`], the pool included) and the check is the one `heap-guard` runs
/// after the link. `None` for a package without a grid world.
#[must_use]
pub fn estimate(package: &PlaytestPackage) -> Option<Result<String, String>> {
    let budget = plan(package)?;
    let heap_start =
        psoxide_hazard::heap_guard::IMAGE_BASE + (image_bytes(package) + budget.pool) as u64;
    Some(psoxide_hazard::heap_guard::verdict(
        &render(package),
        heap_start,
        None,
        " (the cook's estimate of the image, before the link)",
    ))
}

/// Spare heap under which the cook says so in its report: the model is within its slack, not exact.
pub const TIGHT_SPARE_BYTES: usize = 24 * 1024;

/// The cook's refusal for a level that cannot fit the guest, `None` when it fits. The check is the
/// guard's ([`estimate`]) over an estimate of the link that errs high; the link-time `heap-guard`
/// stays the exact answer.
#[must_use]
pub fn refusal(package: &PlaytestPackage) -> Option<String> {
    let Err(message) = estimate(package)? else {
        return None;
    };
    let budget = plan(package)?;
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
    Some(format!(
        "grid world: level '{}' cannot fit the guest's {} KiB of RAM: {message} \
         The cook counts a room pool of {} KiB (the rooms the guest holds at once, of {} rooms), an image without it of {} KiB (fixed {} KiB, this level's asset arena {} KiB, its record tables {} KiB, an allowance of {} KiB the cook cannot see). \
         The biggest rooms are {}. A level that cannot hold all its rooms at once should stream by flood (a flood radius in the world settings, and portals or separate pieces so that only the rooms around the player are wanted); the largest room ({} B of renderer scratch) sets the heap.",
        package.save_title.replace('\n', " "),
        GUEST_RAM_BYTES / 1024,
        budget.pool / 1024,
        budget.rooms,
        image_bytes(package) / 1024,
        IMAGE_FIXED_BYTES / 1024,
        persistent_arena_bytes(package) / 1024,
        table_bytes(package) / 1024,
        IMAGE_SLACK_BYTES / 1024,
        biggest.join(", "),
        budget.renderer,
    ))
}

/// A note for the cook's report when the level fits but its spare heap, by the estimate, is under
/// [`TIGHT_SPARE_BYTES`]: the guard's own line, so the numbers read the same at the check and at
/// the link.
#[must_use]
pub fn tight_note(package: &PlaytestPackage) -> Option<String> {
    let Ok(message) = estimate(package)? else {
        return None;
    };
    let heap = GUEST_RAM_BYTES.saturating_sub(image_bytes(package) + plan(package)?.pool);
    let spare = heap.saturating_sub(plan(package)?.need() + MARGIN_BYTES);
    (spare < TIGHT_SPARE_BYTES).then_some(message)
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
            "level={}\nneed={}\nrenderer={}\nresident_table={}\nfixed={}\npool={}\nrooms={}\nimage_estimate={}\n",
            package.save_title.replace('\n', " "),
            b.need(),
            b.renderer,
            b.resident_table,
            b.fixed,
            b.pool,
            b.rooms,
            modelled_image_bytes(package) + b.pool
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
        // The toolkit measured 2026-11-12 (rib-room-h with the tunnel's checkpoint and fragments):
        // 142 persistent pages, 125 slots, a pool of 149,504 B, 232 lights, 3 interactables, 7
        // logic records, 2 world objects, a chunk sky of 1537 rows, 7992 runs and 192 mask words:
        // linked at 1,801,104 B. The model must not fall below it, and must not be far above it.
        let mut lens = vec![0usize; psx_level::wire::TABLE_NAMES.len()];
        let set = |lens: &mut Vec<usize>, name: &str, n: usize| {
            lens[psx_level::wire::TABLE_NAMES
                .iter()
                .position(|t| *t == name)
                .unwrap()] = n;
        };
        for (name, n) in [
            ("rooms", 1),
            ("room_reflection_probes", 1),
            ("lights", 232),
            ("particle_emitters", 1),
            ("interactables", 3),
            ("logic", 7),
            ("visual_links", 3),
            ("world_objects", 2),
            ("grid_uv_motions", 1),
        ] {
            set(&mut lens, name, n);
        }
        let model = IMAGE_FIXED_BYTES
            + 142 * 2048
            + 125 * PERSISTENT_SLOT_BYTES
            + table_bytes_of(&lens, false, false)
            + CHUNK_SKY_CODE_BYTES
            + 1537 * 2
            + 7992
            + 192 * 4
            + 149_504;
        assert!(
            model >= 1_801_104,
            "the model never falls below the link: {model}"
        );
        assert!(
            model <= 1_801_104 + 20 * 1024,
            "and stays close to it: {model}"
        );
        // Without the interactables the link was 25,152 B smaller (measured the same way).
        set(&mut lens, "interactables", 0);
        let without = IMAGE_FIXED_BYTES
            + 142 * 2048
            + 125 * PERSISTENT_SLOT_BYTES
            + table_bytes_of(&lens, false, false)
            + CHUNK_SKY_CODE_BYTES
            + 1537 * 2
            + 7992
            + 192 * 4
            + 149_504;
        assert!(model - without >= 25_152);
    }

    #[test]
    fn the_cooks_estimate_is_the_guards_verdict_on_the_same_numbers() {
        let (dir, project) = starter_project("verdict");
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let budget = plan(&package).unwrap();
        let text = render(&package);
        assert!(text.contains("image_estimate="));
        // A link map whose heap starts where the estimate says gets the same figures from the
        // guard that runs after the link.
        let heap_start =
            psoxide_hazard::heap_guard::IMAGE_BASE + (image_bytes(&package) + budget.pool) as u64;
        let map = format!("{heap_start:x} {heap_start:x} 0 1 __heap_start = .\n");
        let linked = psoxide_hazard::heap_guard::check(&map, &text).unwrap();
        let cooked = estimate(&package).unwrap().unwrap();
        let spare = |line: &str| {
            line.split("heap is ")
                .nth(1)
                .and_then(|rest| rest.split(" B").next())
                .map(str::to_string)
        };
        assert_eq!(spare(&linked), spare(&cooked), "{linked} / {cooked}");
        assert!(linked.contains("B to spare") && cooked.contains("B to spare"));
    }

    #[test]
    fn a_level_whose_tables_leave_the_heap_short_is_refused_with_the_guards_numbers() {
        let (dir, project) = starter_project("tables");
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let mut package = package.expect("a package");
        assert_eq!(refusal(&package), None);
        // Enough props and enemies that their code and arrays no longer leave the heap its need.
        let lens = &mut package.world_geometry.record_lens;
        lens.resize(psx_level::wire::TABLE_NAMES.len(), 0);
        for name in ["box_props", "game_entities", "model_instances", "entities"] {
            let i = psx_level::wire::TABLE_NAMES
                .iter()
                .position(|t| *t == name)
                .unwrap();
            lens[i] = 100;
        }
        let message = refusal(&package).expect("refused");
        assert!(
            message.contains("will run out of heap") && message.contains("short by"),
            "{message}"
        );
    }
}
