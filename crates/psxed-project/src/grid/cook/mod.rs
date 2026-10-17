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
pub mod lod;
pub mod plane_collision;
pub mod portals;
pub mod render;
pub mod units;

use psx_grid::write::{LevelCore, RoomChunk};
use psx_grid::{
    Material, RoomRecord, CHUNK_HAS_LOD, CHUNK_HAS_PLANES, LEVEL_HAS_PLANES, LEVEL_HAS_SKY,
    ROOM_FOG, ROOM_OUTDOOR, SECTION_COUNT, SECTOR_BYTES,
};

pub use render::{BoundMaterial, MaterialBinder};
pub use units::RuntimeUnits;

use crate::grid::light::GridLight;
use crate::grid::limits;
use crate::grid::world::GridWorld;

/// Most pages (2 KiB) the cook lets the resident set of a level reach. A
/// cook-time cap chosen to stay well inside the console's RAM; the runtime
/// reads the same number from the core's `pool_pages`.
pub const POOL_PAGE_CAP: usize = 512;
/// Portal hops of rooms kept resident around the player room.
pub const RESIDENT_HOPS: u8 = 2;
/// Extra hops prefetched.
pub const PREFETCH_HOPS: u8 = 1;

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
    /// Resident pages the level needs with everything loaded.
    pub pool_pages: usize,
    /// Non-fatal findings.
    pub warnings: Vec<String>,
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
    /// Merged far quads and the cell quads they stand in for.
    pub lod_quads: usize,
    pub lod_covered: usize,
    /// Unpadded chunk bytes.
    pub bytes: usize,
    /// Bytes per section, in `psx_grid::section` order.
    pub section_bytes: [usize; SECTION_COUNT],
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
        for (i, r) in self.rooms.iter().enumerate() {
            let c = |ids: &[usize]| ids.iter().map(|&k| r.section_bytes[k]).sum::<usize>();
            text.push_str(&format!(
                "  room {i} {:?} {}x{}: {} faces -> {} quads + {} tris, {} verts, {} lod quads over {}, {} B (collision {} B, vertices {} B, quads {} B, cells {} B)\n",
                r.name,
                r.width,
                r.depth,
                r.faces,
                r.quads,
                r.triangles,
                r.vertices,
                r.lod_quads,
                r.lod_covered,
                r.bytes,
                c(&[0, 1, 2, 3, 4, 5, 6, 7]),
                c(&[9]),
                c(&[10]),
                c(&[11]),
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
    input: CookInput<'_>,
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
    // Collision first, for every room: the light bake shadows through the
    // same line of sight the game uses, which needs the whole world's solid
    // tables (stacked rooms shadow each other).
    let mut origins: Vec<[i32; 3]> = Vec::new();
    let mut cols: Vec<collision::Tables> = Vec::new();
    for (index, room) in world.rooms.iter().enumerate() {
        origins.push([
            room.origin[0] * s as i32,
            units.rt(room.y_offset),
            room.origin[1] * s as i32,
        ]);
        cols.push(collision::build(&units, room, index)?);
    }
    let probe_core = LevelCore {
        sector_units: s as u16,
        rooms: world
            .rooms
            .iter()
            .zip(&cols)
            .zip(&origins)
            .map(|((room, col), origin)| {
                let (lo, hi) = col.range.unwrap_or((0, 0));
                RoomRecord {
                    origin: *origin,
                    width: room.width as u8,
                    depth: room.depth as u8,
                    min_y: lo as i16,
                    max_y: hi as i16,
                    ..RoomRecord::default()
                }
            })
            .collect(),
        ..LevelCore::default()
    };
    let probe_chunks: Vec<Vec<u32>> = world
        .rooms
        .iter()
        .zip(&cols)
        .enumerate()
        .map(|(index, (room, col))| {
            let (lo, hi) = col.range.unwrap_or((0, 0));
            let chunk = RoomChunk {
                room_id: index as u16,
                width: room.width as u8,
                depth: room.depth as u8,
                min_y: lo as i16,
                max_y: hi as i16,
                sectors: col.sectors.clone(),
                exts: col.exts.clone(),
                walls: col.walls.clone(),
                splitbs: col.splitbs.clone(),
                col_planes: col.planes.clone(),
                ..RoomChunk::default()
            };
            psx_grid::write::aligned_copy(&chunk.to_bytes())
        })
        .collect();
    let probe_core_bytes = psx_grid::write::aligned_copy(&probe_core.to_bytes());
    let probe_len = probe_core.to_bytes().len();
    let probe_level = psx_grid::LevelView::parse(psx_grid::write::words_as_bytes(
        &probe_core_bytes,
        probe_len,
    ))
    .map_err(|e| format!("collision probe core: {e:?}"))?;
    let probe_views: Vec<Option<psx_grid::CollisionView<'_>>> = probe_chunks
        .iter()
        .map(|words| {
            psx_grid::CollisionView::parse(psx_grid::write::words_as_bytes(words, words.len() * 4))
                .ok()
        })
        .collect();
    let world_collision = psx_grid::collision::GridCollision::new(probe_level, &probe_views)
        .ok_or_else(|| "collision world could not be built".to_string())?;
    let own_los = |from: [f64; 3], to: [f64; 3]| {
        let r = |p: [f64; 3]| p.map(|v| v.round() as i32);
        !world_collision.line_of_sight(r(from), r(to))
    };
    let occluded: &dyn Fn([f64; 3], [f64; 3]) -> bool = occluded.unwrap_or(&own_los);

    let mut materials = render::MaterialSet::default();
    let mut tables: Vec<RoomChunk> = Vec::new();
    let mut records: Vec<RoomRecord> = Vec::new();
    let mut any_sky = false;
    let mut any_planes = false;
    for (index, room) in world.rooms.iter().enumerate() {
        let origin = origins[index];
        let col = std::mem::take(&mut cols[index]);
        let mut cx = render::Context {
            world,
            units,
            lights: input.lights,
            binder: &mut *input.binder,
            materials: &mut materials,
            occluded,
        };
        let ambient = room.ambient;
        let mut ren = render::build(&mut cx, index, origin, ambient)?;
        let lod = lod::build(
            room.width,
            room.depth,
            s as i32,
            input.lod_far_cells > 0,
            &mut ren,
        );
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
            lod_quads: chunk.lod_quads.len(),
            lod_covered: chunk
                .quads
                .iter()
                .filter(|q| q.flags & psx_grid::QUAD_LOD_COVERED != 0)
                .count(),
            bytes: 0,
            section_bytes: chunk.section_bytes(),
        });
        let fog = &room.fog;
        let flags =
            (if outdoor { ROOM_OUTDOOR } else { 0 }) | (if fog.enabled { ROOM_FOG } else { 0 });
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
    let derived = portals::derive(world, &units)?;
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
    if total_pages > POOL_PAGE_CAP {
        return Err(format!(
            "the level needs {total_pages} pages ({} KiB) of room data resident, the cook cap is {POOL_PAGE_CAP}",
            total_pages * 2
        ));
    }
    let level = LevelCore {
        flags: (if any_sky { LEVEL_HAS_SKY } else { 0 })
            | (if any_planes { LEVEL_HAS_PLANES } else { 0 }),
        sector_units: s as u16,
        pool_pages: total_pages as u16,
        resident_hops: RESIDENT_HOPS,
        prefetch_hops: PREFETCH_HOPS,
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
                reserved: 0,
            })
            .collect(),
    };
    let core = level.to_bytes();
    report.core_bytes = core.len();
    report.materials = materials.slots.len();
    report.portals = level.portals.len();
    report.pool_pages = total_pages;
    Ok(CookedLevel {
        core,
        chunks,
        tables,
        level,
        materials: materials.slots,
        report,
    })
}

#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod derived_tests;
#[cfg(test)]
mod plane_tests;
#[cfg(test)]
mod spawn_tests;
#[cfg(test)]
mod tests;
