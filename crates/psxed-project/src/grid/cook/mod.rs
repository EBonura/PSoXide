//! The PGL cook: grid document in, level core and room chunks out.
//!
//! Input is the [`GridWorld`] document alone (plus the lights, a material
//! binder and an occlusion query for the bake). Output is the bytes of the
//! level core, one chunk per room and a size report. Everything here is a
//! pure function of the input and deterministic: cooking twice gives
//! identical bytes.
//!
//! Layout and meaning of every record: `docs/grid/level-format-and-runtime.md`
//! and `psx_grid::format`.

pub mod collision;
pub mod leaf_collision;
pub mod lod;
pub mod mesh;
pub mod mesh_collision;
pub mod occupancy;
pub mod plane_collision;
pub mod portals;
pub mod render;
pub mod sky_bake;
pub mod units;

use psx_grid::write::{LevelCore, RoomChunk};
use psx_grid::{
    Material, RoomRecord, CHUNK_HAS_LOD, CHUNK_HAS_PLANES, LEVEL_HAS_PLANES, LEVEL_HAS_SKY,
    LEVEL_RING_STREAM, ROOM_FOG, ROOM_OUTDOOR, ROOM_PROBES, ROOM_SPANS, ROOM_TREE, SECTION_COUNT,
    SECTOR_BYTES,
};

pub use render::{BoundMaterial, MaterialBinder};
pub use units::RuntimeUnits;

use crate::grid::light::GridLight;
use crate::grid::limits;
use crate::grid::world::GridWorld;

/// Most pages (2 KiB) one room chunk may take: the core's `max_room_pages`
/// is a byte, and a streamed unit must fit a pool run. This is a per-room
/// cap. The level as a whole has none: only the rooms around the player are
/// resident at once. Only a level with no draw distance (and no ring) wants
/// every room at once, so only it can trip [`ALL_RESIDENT_ADVISORY_PAGES`].
pub const MAX_ROOM_PAGES: usize = 255;
/// Total chunk pages above which the cook warns that a level with an
/// unbounded draw distance, which keeps every room resident, may not fit.
pub const ALL_RESIDENT_ADVISORY_PAGES: usize = 512;
/// KiB of room pool the editor playtest guest can link: the hex lane
/// measured 546 to 552 KiB as the point where it stops linking. A level that
/// streams by flood ([`crate::grid::world::FloodSettings`]) is refused when
/// its busiest position needs more, and a level whose rooms total less is
/// held whole.
pub const GUEST_POOL_CEILING_KIB: usize = 550;

/// Everything the cook needs besides the document.
pub struct CookInput<'a> {
    /// The grid world.
    pub world: &'a GridWorld,
    /// Binds a document material to a texture asset.
    pub binder: &'a mut dyn MaterialBinder,
    /// Point lights in world runtime units.
    pub lights: &'a [GridLight],
    /// First `WORLD.PAK` sector of the first room chunk.
    pub first_pack_sector: u32,
    /// Distance in sectors beyond which blocks draw merged far quads; 0
    /// writes no LOD.
    pub lod_far_cells: u8,
    /// Whether primitives may repeat their texture through the GPU texture
    /// window, up to four tiles per axis ([`texture_repeat_default`]).
    pub texture_repeat: bool,
}

/// Whether the cook lets primitives repeat their texture through the GPU
/// texture window. On by default; `PSXED_GRID_NO_REPEAT=1` cooks every
/// primitive at one tile, the way format version 2 levels were cooked, for
/// A/B measurements.
pub fn texture_repeat_default() -> bool {
    !std::env::var("PSXED_GRID_NO_REPEAT").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Size and census report of one cook.
#[derive(Clone, Debug, Default)]
pub struct CookReport {
    /// Runtime units per sector.
    pub sector_units: u32,
    /// Level core bytes.
    pub core_bytes: usize,
    /// Per room.
    pub rooms: Vec<RoomReport>,
    /// Distinct materials.
    pub materials: usize,
    /// Portals (directed).
    pub portals: usize,
    /// Pages (2 KiB) of every room chunk together, the footprint of an
    /// all-resident run.
    pub pool_pages: usize,
    /// Pages of the largest room chunk, the biggest pool run one room needs.
    pub max_room_pages: usize,
    /// Non-fatal findings.
    pub warnings: Vec<String>,
    /// What the global light solve did, when the world gathers its light.
    pub global_light: Option<GlobalLightNote>,
}

/// What the global light solve of one cook did.
#[derive(Clone, Copy, Debug, Default)]
pub struct GlobalLightNote {
    /// Corner samples solved.
    pub samples: usize,
    /// Rays traced over all passes.
    pub rays: u64,
    /// Passes (the first gather plus the bounces).
    pub passes: usize,
    /// Wall time of the solve, milliseconds.
    pub millis: f64,
}

/// Size and census report of one room chunk.
#[derive(Clone, Debug, Default)]
pub struct RoomReport {
    pub name: String,
    pub width: usize,
    pub depth: usize,
    pub faces: usize,
    pub quads: usize,
    pub triangles: usize,
    pub vertices: usize,
    pub collapsed: usize,
    pub uv_overflow: usize,
    /// Primitives added by cutting faces at texture tile boundaries.
    pub uv_split_quads: usize,
    /// Primitives that repeat their texture through the texture window.
    pub repeat_quads: usize,
    /// Quads whose corner order was turned so the GPU splits them along the
    /// diagonal that fits the baked light.
    pub flipped_diagonals: usize,
    /// Merged far quads and the cell quads they stand in for.
    pub lod_quads: usize,
    pub lod_covered: usize,
    /// Unpadded chunk bytes.
    pub bytes: usize,
    /// Bytes per section, in `psx_grid::section` order.
    pub section_bytes: [usize; SECTION_COUNT],
    /// Bytes of the light probe block (record and tables), zero without one.
    pub probe_bytes: usize,
    /// Light probes (sectors with a floor, one more per extra floor).
    pub probes: usize,
    /// Free-plane collision records in the chunk (`COL_PLANES`). Point rays
    /// scan all of them for each trace of the room, so this is a run time
    /// cost, not only bytes.
    pub plane_records: usize,
    /// Distinct collidable planes (a mesh box is five).
    pub planes: usize,
    /// Plane records the sector and leaf lists name together: what the table
    /// would hold if each list had a run of its own.
    pub plane_listed: usize,
}

impl CookReport {
    /// Total chunk bytes over all rooms, unpadded.
    pub fn chunk_bytes(&self) -> usize {
        self.rooms.iter().map(|r| r.bytes).sum()
    }

    /// One block of text for the build log.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "grid level: {} rooms, {} materials, {} portals, S={} | core {} B, chunks {} B ({} pages)\n",
            self.rooms.len(),
            self.materials,
            self.portals,
            self.sector_units,
            self.core_bytes,
            self.chunk_bytes(),
            self.pool_pages
        );
        if let Some(g) = &self.global_light {
            text.push_str(&format!(
                "  global light: {} corner samples, {} rays over {} passes, {:.0} ms\n",
                g.samples, g.rays, g.passes, g.millis
            ));
        }
        for (i, r) in self.rooms.iter().enumerate() {
            let c = |ids: &[usize]| ids.iter().map(|&k| r.section_bytes[k]).sum::<usize>();
            text.push_str(&format!(
                "  room {i} {:?} {}x{}: {} faces -> {} quads + {} tris{}, {} verts, {} lod quads over {}, {} B (collision {} B{}, vertices {} B, quads {} B, cells {} B, {} light probes {} B)\n",
                r.name,
                r.width,
                r.depth,
                r.faces,
                r.quads,
                r.triangles,
                {
                    let mut notes = Vec::new();
                    if r.uv_split_quads > 0 {
                        notes.push(format!("{} from texture tile cuts", r.uv_split_quads));
                    }
                    if r.repeat_quads > 0 {
                        notes.push(format!("{} repeating through the texture window", r.repeat_quads));
                    }
                    if r.flipped_diagonals > 0 {
                        notes.push(format!("{} diagonals turned to fit the light", r.flipped_diagonals));
                    }
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", notes.join(", "))
                    }
                },
                r.vertices,
                r.lod_quads,
                r.lod_covered,
                r.bytes,
                c(&[0, 1, 2, 3, 4, 5, 6, 7]),
                if r.plane_records == 0 {
                    String::new()
                } else {
                    // Every point ray of the room scans all the records.
                    format!(
                        ": {} plane records for {} planes, {} listed by sectors and leaves",
                        r.plane_records, r.planes, r.plane_listed
                    )
                },
                c(&[9]),
                c(&[10]),
                c(&[11]),
                r.probes,
                r.probe_bytes,
            ));
        }
        text
    }
}

/// The cooked level.
#[derive(Debug)]
pub struct CookedLevel {
    /// Level core file.
    pub core: Vec<u8>,
    /// One chunk per room, unpadded, in room order.
    pub chunks: Vec<Vec<u8>>,
    /// The structured chunks, for tests and tools.
    pub tables: Vec<RoomChunk>,
    /// The structured core.
    pub level: LevelCore,
    /// Level-global materials, in slot order.
    pub materials: Vec<(Option<crate::ResourceId>, BoundMaterial)>,
    /// Census.
    pub report: CookReport,
}

fn pages(bytes: usize) -> usize {
    bytes.div_ceil(SECTOR_BYTES)
}

/// Cook `input.world`. `occluded` answers whether a segment between two world
/// points (runtime units) is blocked, for the light bake; pass `None` for no
/// shadows.
pub fn cook_level(
    mut input: CookInput<'_>,
    occluded: Option<&dyn Fn([f64; 3], [f64; 3]) -> bool>,
) -> Result<CookedLevel, String> {
    let world = input.world;
    let issues = limits::validate(world);
    if let Some(error) = issues
        .iter()
        .find(|i| i.severity() == limits::Severity::Error)
    {
        return Err(format!("grid world: {}", error.describe()));
    }
    if world.rooms.is_empty() {
        return Err("the world has no rooms".to_string());
    }
    let units = RuntimeUnits::new(world.scale)?;
    let s = units.sector();
    let mut report = CookReport {
        sector_units: s,
        ..CookReport::default()
    };
    for issue in &issues {
        report
            .warnings
            .push(format!("grid world: {}", issue.describe()));
    }
    // Collision is cooked from the real leaves. Portals are still derived per
    // whole sector, so a room with leaves of other sizes is walked through
    // as its sector view (faces and collision never go through it).
    let sector_world = crate::grid::sector_view::sector_world(world).map(|(w, _)| w);
    let portal_world: &GridWorld = sector_world.as_ref().unwrap_or(world);
    // Collision first, for every room: the light bake shadows through the
    // same line of sight the game uses, which needs the whole world's solid
    // tables (stacked rooms shadow each other).
    let origins = crate::grid::lighting::probe::room_origins(world, &units);
    let mut cols: Vec<collision::Tables> = Vec::new();
    for (index, room) in world.rooms.iter().enumerate() {
        let binder = &*input.binder;
        cols.push(collision::build_with_meshes(
            &units,
            room,
            &world.mesh_assets,
            index,
            &mut |plane| {
                plane
                    .camera_pass
                    .unwrap_or_else(|| binder.see_through(plane.material))
            },
        )?);
    }
    let probe = crate::grid::lighting::probe::CollisionProbe::new(world, &units, &origins, &cols)?;
    // A world that gathers its light solves it before any quad is coloured.
    let field = if world.global_light.enabled {
        let started = std::time::Instant::now();
        let field = gather_field(&mut input, &units, &probe)?;
        report.global_light = Some(GlobalLightNote {
            samples: field.stats.samples,
            rays: field.stats.rays,
            passes: field.stats.passes,
            millis: started.elapsed().as_secs_f64() * 1000.0,
        });
        Some(field)
    } else {
        None
    };
    // The light probes sit at the floors the collision holds; a world that
    // gathers its light already planned them for its solve.
    let plan = match &field {
        Some(field) => field.probe_plan().clone(),
        None => crate::grid::lighting::ProbePlan::build(world, &units, &probe)?,
    };
    probe.with_occlusion(|own_los| {
        cook_rooms(
            input,
            field.as_ref(),
            &plan,
            occluded.unwrap_or(own_los),
            world,
            units,
            s,
            report,
            origins,
            cols,
            portal_world,
        )
    })?
}

/// Solve the global light of `input.world` for the cook: the albedo of each
/// material comes from the binder (the texture bytes the level carries), the
/// shadows from the same collision the quads are coloured through.
fn gather_field(
    input: &mut CookInput<'_>,
    units: &RuntimeUnits,
    probe: &crate::grid::lighting::CollisionProbe,
) -> Result<crate::grid::lighting::GiField, String> {
    let world = input.world;
    let mut table = crate::grid::lighting::AlbedoTable::default();
    let mut seen = std::collections::HashSet::new();
    for face in crate::grid::faces::all_faces_with(world, &()) {
        if seen.insert(face.material) {
            table.insert(face.material, input.binder.albedo(face.material));
            if input.binder.light_passes(face.material) {
                table.set_open(face.material);
            }
        }
    }
    crate::grid::lighting::gather::solve(&crate::grid::lighting::gather::GatherInput {
        world,
        units: *units,
        lights: input.lights,
        probe,
        materials: &table,
        cancel: None,
    })
    .ok_or_else(|| "the global light solve was cancelled".to_string())
}

/// The rest of [`cook_level`], once the collision tables and the shadow query
/// exist.
#[allow(clippy::too_many_arguments)]
fn cook_rooms(
    input: CookInput<'_>,
    field: Option<&crate::grid::lighting::GiField>,
    plan: &crate::grid::lighting::ProbePlan,
    occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    world: &GridWorld,
    units: RuntimeUnits,
    s: u32,
    mut report: CookReport,
    origins: Vec<[i32; 3]>,
    mut cols: Vec<collision::Tables>,
    portal_world: &GridWorld,
) -> Result<CookedLevel, String> {
    let mut materials = render::MaterialSet::default();
    let mut tables: Vec<RoomChunk> = Vec::new();
    let mut records: Vec<RoomRecord> = Vec::new();
    let mut any_sky = false;
    let mut any_planes = false;
    // The light probes, baked by the rig the quads are coloured with.
    let mut probe_tables = {
        let rig = crate::grid::lighting::BakeRig {
            units,
            lights: input.lights,
            shade: world.shading,
            occluded,
            gather: world
                .global_light
                .enabled
                .then_some(crate::grid::lighting::Gather { field }),
        };
        plan.tables(|index, site| {
            rig.probe(index, site, world.rooms[usize::from(site.room)].ambient)
        })
    };
    for (index, room) in world.rooms.iter().enumerate() {
        let origin = origins[index];
        let col = std::mem::take(&mut cols[index]);
        let mut cx = render::Context {
            world,
            units,
            rig: crate::grid::lighting::BakeRig {
                units,
                lights: input.lights,
                shade: world.shading,
                occluded,
                gather: world
                    .global_light
                    .enabled
                    .then_some(crate::grid::lighting::Gather { field }),
            },
            binder: &mut *input.binder,
            materials: &mut materials,
            repeat: input.texture_repeat,
        };
        let ambient = room.ambient;
        let mut ren = render::build(&mut cx, index, origin, ambient)?;
        let mut lod = lod::build(
            room.width,
            room.depth,
            s as i32,
            input.lod_far_cells > 0,
            &mut ren,
        );
        // The room's quads name its own material table (`MATERIAL_USE`), so a
        // room can be bound, loaded and evicted without the rest of the level.
        let mut local = [0u8; 256];
        for (rank, &level_id) in ren.material_use.iter().enumerate() {
            local[usize::from(level_id)] = rank as u8;
        }
        for quad in ren.quads.iter_mut().chain(lod.quads.iter_mut()) {
            quad.material = local[usize::from(quad.material)];
        }
        let (mut lo, mut hi) = col.range.unwrap_or((0, 0));
        for v in &ren.vertices {
            lo = lo.min(i32::from(v.y));
            hi = hi.max(i32::from(v.y));
        }
        let outdoor = ren
            .cells
            .iter()
            .any(|c| c.flags & psx_grid::CELL_OPEN_SKY != 0);
        any_sky |= outdoor;
        let plane_records = col.planes.len();
        let plane_listed = col.exts.iter().map(|e| usize::from(e.plane_count)).sum();
        let plane_ids = col
            .planes
            .iter()
            .map(|p| p.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let has_planes = !col.planes.is_empty() || !ren.plane_refs.is_empty();
        any_planes |= has_planes;
        report.warnings.extend(col.warnings.iter().cloned());
        let chunk = RoomChunk {
            room_id: index as u16,
            flags: (if lod.quads.is_empty() {
                0
            } else {
                CHUNK_HAS_LOD
            }) | (if has_planes { CHUNK_HAS_PLANES } else { 0 }),
            width: u8::try_from(room.width).map_err(|_| "room wider than 255".to_string())?,
            depth: u8::try_from(room.depth).map_err(|_| "room deeper than 255".to_string())?,
            min_y: lo as i16,
            max_y: hi as i16,
            lod_far_cells: if lod.quads.is_empty() {
                0
            } else {
                input.lod_far_cells
            },
            lod_shift: lod::LOD_SHIFT,
            sectors: col.sectors,
            tree: col.tree,
            spans: col.spans,
            probes: probe_tables[index].take(),
            exts: col.exts,
            walls: col.walls,
            splitbs: col.splitbs,
            col_planes: col.planes,
            plane_refs: ren.plane_refs,
            vertices: ren.vertices,
            quads: ren.quads,
            cells: ren.cells,
            material_use: ren.material_use,
            lod_blocks: lod.blocks,
            lod_quads: lod.quads,
            ..RoomChunk::default()
        };
        report.rooms.push(RoomReport {
            name: room.name.clone(),
            width: room.width,
            depth: room.depth,
            faces: ren.stats.faces,
            quads: ren.stats.quads,
            triangles: ren.stats.triangles,
            vertices: chunk.vertices.len(),
            collapsed: ren.stats.collapsed,
            uv_overflow: ren.stats.uv_overflow,
            uv_split_quads: ren.stats.uv_split_quads,
            repeat_quads: ren.stats.repeat_quads,
            flipped_diagonals: ren.stats.flipped_diagonals,
            lod_quads: chunk.lod_quads.len(),
            lod_covered: chunk
                .quads
                .iter()
                .filter(|q| q.flags & psx_grid::QUAD_LOD_COVERED != 0)
                .count(),
            bytes: 0,
            section_bytes: chunk.section_bytes(),
            probe_bytes: chunk.probes.as_ref().map_or(0, |p| p.bytes()),
            probes: plan
                .sites
                .iter()
                .filter(|site| usize::from(site.room) == index)
                .count(),
            plane_records,
            planes: plane_ids,
            plane_listed,
        });
        let fog = &room.fog;
        let flags = (if outdoor { ROOM_OUTDOOR } else { 0 })
            | (if fog.enabled { ROOM_FOG } else { 0 })
            | (if chunk.tree.is_some() { ROOM_TREE } else { 0 })
            | (if chunk.spans.is_some() { ROOM_SPANS } else { 0 })
            | (if chunk.probes.is_some() {
                ROOM_PROBES
            } else {
                0
            });
        records.push(RoomRecord {
            pack_sector: 0,
            pack_sectors: 0,
            flags,
            origin,
            width: chunk.width,
            depth: chunk.depth,
            portal_first: 0,
            portal_count: 0,
            min_y: chunk.min_y,
            max_y: chunk.max_y,
            fog_near: units.rt(fog.start).clamp(0, i32::from(u16::MAX)) as u16,
            fog_far: units
                .rt(fog.start.saturating_add(fog.falloff))
                .clamp(0, i32::from(u16::MAX)) as u16,
            fog_rgb: fog.color,
            reserved: [0; 3],
        });
        tables.push(chunk);
    }

    // Portals, grouped per source room.
    let derived = portals::derive(portal_world, &units)?;
    let mut all_portals = Vec::new();
    for (record, list) in records.iter_mut().zip(&derived) {
        record.portal_first =
            u16::try_from(all_portals.len()).map_err(|_| "more than 65535 portals".to_string())?;
        record.portal_count =
            u16::try_from(list.len()).map_err(|_| "too many portals".to_string())?;
        all_portals.extend_from_slice(list);
    }

    // Chunks, pack placement.
    let mut chunks = Vec::new();
    let mut next_sector = input.first_pack_sector;
    let mut total_pages = 0usize;
    for (i, chunk) in tables.iter().enumerate() {
        let bytes = chunk.to_bytes();
        let sectors = pages(bytes.len());
        records[i].pack_sector = next_sector;
        records[i].pack_sectors =
            u16::try_from(sectors).map_err(|_| format!("room {i} chunk is too large"))?;
        next_sector += sectors as u32;
        total_pages += sectors;
        report.rooms[i].bytes = bytes.len();
        chunks.push(bytes);
    }
    let max_room_pages = chunks.iter().map(|c| pages(c.len())).max().unwrap_or(0);
    if max_room_pages > MAX_ROOM_PAGES {
        let (room, big) = chunks
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| c.len())
            .map_or((0, 0), |(i, c)| (i, c.len()));
        return Err(format!(
            "room {room} cooks to {big} bytes ({max_room_pages} pages), at most {MAX_ROOM_PAGES} pages ({} KiB) per room; split the room",
            MAX_ROOM_PAGES * 2
        ));
    }
    let radii = world.stream;
    // A bounded draw distance or a ring streams a window of rooms; only
    // an unbounded level wants them all resident at once.
    if !radii.ring && radii.draw_far == 0 && total_pages > ALL_RESIDENT_ADVISORY_PAGES {
        report.warnings.push(format!(
            "the rooms total {total_pages} pages ({} KiB) and the draw distance is unbounded, so the runtime holds every chunk resident; a level this big may not fit (set a draw distance or ring streaming)",
            total_pages * 2
        ));
    }
    let mut level = LevelCore {
        occupancy: occupancy::derive(world),
        flags: (if any_sky { LEVEL_HAS_SKY } else { 0 })
            | (if any_planes { LEVEL_HAS_PLANES } else { 0 })
            | (if radii.ring { LEVEL_RING_STREAM } else { 0 }),
        sector_units: s as u16,
        draw_far: radii.draw_far,
        active_radius: radii.active_radius,
        lead_radius: radii.lead_radius,
        rooms: records,
        portals: all_portals,
        materials: materials
            .slots
            .iter()
            .map(|(_, m)| Material {
                texture_asset: m.asset,
                flags: m.flags,
                blend: m.blend,
                uv_motion: m.uv_motion,
                tex_shape: m.tex_shape(),
            })
            .collect(),
        ..LevelCore::default()
    };
    level.measure_chunks(&tables, chunks.iter().map(|c| pages(c.len())));
    if radii.flood.on {
        let plan = flood_plan::plan(&level, &chunks, &radii.flood)?;
        // The core's own radii stay meaningful for the observers outside any
        // room: the draw distance is the flood's, the lead the rest of it.
        level.draw_far = radii.flood.near_radius;
        level.lead_radius = radii.flood.res_radius - radii.flood.near_radius;
        level.flood = Some(plan.block);
        report.warnings.extend(plan.notes);
    }
    let core = level.to_bytes();
    report.core_bytes = core.len();
    report.materials = materials.slots.len();
    report.portals = level.portals.len();
    report.pool_pages = total_pages;
    report.max_room_pages = max_room_pages;
    Ok(CookedLevel {
        core,
        chunks,
        tables,
        level,
        materials: materials.slots,
        report,
    })
}

mod flood_plan;
pub mod skin;

#[cfg(test)]
mod band_cook_tests;
#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod derived_tests;
#[cfg(test)]
mod extrude_tests;
#[cfg(test)]
mod flood_plan_tests;
#[cfg(test)]
mod gap_tests;
#[cfg(test)]
mod leaf_tests;
#[cfg(test)]
mod light_tests;
#[cfg(test)]
mod mesh_tests;
#[cfg(test)]
mod motor_harness;
#[cfg(test)]
mod natural_tests;
#[cfg(test)]
mod occupancy_tests;
#[cfg(test)]
mod plane_share_tests;
#[cfg(test)]
mod plane_tests;
#[cfg(test)]
mod probe_tests;
#[cfg(test)]
mod ramp_tests;
#[cfg(test)]
mod skin_tests;
#[cfg(test)]
mod span_tests;
#[cfg(test)]
mod spawn_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
pub(crate) mod tests;
#[cfg(test)]
mod uv_pin_tests;
#[cfg(test)]
mod uv_tests;
