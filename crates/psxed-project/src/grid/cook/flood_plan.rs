//! The flood block of a level and the gates a flooded level must pass.
//!
//! A level that streams by flood ([`FloodSettings`]) is checked at cook time
//! with the same flood code the guest runs: the pool its busiest position
//! needs must fit the memory the guest can link, and the quads in the view
//! cone at the draw radius must fit the level's quad budget. A level that
//! fits the pool whole gets a block that says so (`pool_cap_pages` 0) and the
//! guest never floods it.

use psx_grid::write::{FloodBlock, LevelCore};
use psx_grid::{FloodHeader, FLOOD_WEIGHT_ONE};

use crate::grid::census::{draw_load, sample_cameras, Graph, LevelBlobs};
use crate::grid::world::FloodSettings;

use super::GUEST_POOL_CEILING_KIB;

/// Seed of the camera samples the quad gate measures from.
const SAMPLE_SEED: u64 = 12345;
/// How far, in sectors, the gate counts quads from the camera: the fog
/// distance the Citadel is cooked with.
const GATE_REACH_SECTORS: f64 = 70.3;
/// Rooms beyond the busiest position's that the guest's resident table is sized for: the room
/// being installed and the ones not yet evicted when the observer moves on.
const RESIDENT_SLACK_ROOMS: usize = 4;
/// Bytes in a pool page, in KiB.
const PAGE_KIB: usize = 2;

/// What the cook found out about a flooded level.
pub(super) struct FloodPlan {
    pub block: FloodBlock,
    /// Report lines for the cook's warnings.
    pub notes: Vec<String>,
}

/// The flood block of `level` and its gates. `chunks` are the cooked room
/// chunks, in room order.
pub(super) fn plan(
    level: &LevelCore,
    chunks: &[Vec<u8>],
    settings: &FloodSettings,
) -> Result<FloodPlan, String> {
    if settings.res_radius < settings.near_radius {
        return Err(format!(
            "flood streaming: the resident radius ({} sectors) is below the draw radius ({})",
            settings.res_radius, settings.near_radius
        ));
    }
    let pages: Vec<usize> = chunks.iter().map(|c| c.len().div_ceil(2048)).collect();
    let total: usize = pages.iter().sum();
    let largest = pages.iter().copied().max().unwrap_or(0);
    let ceiling_pages = GUEST_POOL_CEILING_KIB / PAGE_KIB;
    let mut block = FloodBlock {
        header: FloodHeader {
            pool_cap_pages: 0,
            near_radius: settings.near_radius,
            res_radius: settings.res_radius,
            bias: 0,
            peak_rooms: 0,
            reserved: [0; 2],
        },
        portal_weight: vec![FLOOD_WEIGHT_ONE; level.portals.len()],
    };
    let mut notes = Vec::new();
    if total <= ceiling_pages {
        notes.push(format!(
            "flood streaming: the rooms total {total} pages ({} KiB), which the {GUEST_POOL_CEILING_KIB} KiB pool holds whole; every room stays resident and the flood never runs",
            total * PAGE_KIB
        ));
        return Ok(FloodPlan { block, notes });
    }
    // The resident set may not take the page being installed's room.
    let budget = ceiling_pages.saturating_sub(largest).min(total);
    block.header.pool_cap_pages = u16::try_from(budget)
        .map_err(|_| format!("flood streaming: a budget of {budget} pages is above 65535"))?;

    let mut provisional = level.clone();
    provisional.flood = Some(block.clone());
    let core = provisional.try_to_bytes()?;
    let blobs = LevelBlobs::new(&core, chunks);
    let view = blobs.view();
    let pool = psx_grid::stream::plan_pool(&view, &blobs.pages());
    if pool.pool_pages * PAGE_KIB > GUEST_POOL_CEILING_KIB {
        return Err(format!(
            "flood streaming: the busiest position keeps {} cells, {} pages ({} KiB) resident and the pool is {} KiB with the largest room, above the {GUEST_POOL_CEILING_KIB} KiB the guest links; lower the resident radius ({}) or cut the rooms smaller",
            pool.peak_rooms,
            pool.peak_pages,
            pool.peak_pages * PAGE_KIB,
            pool.pool_pages * PAGE_KIB,
            settings.res_radius
        ));
    }
    block.header.peak_rooms =
        u8::try_from(pool.peak_rooms + RESIDENT_SLACK_ROOMS).unwrap_or(u8::MAX);
    let graph = Graph::new(&blobs);
    let samples = sample_cameras(&view, SAMPLE_SEED);
    let load = draw_load(
        &blobs,
        &graph,
        &samples,
        u32::from(settings.near_radius),
        GATE_REACH_SECTORS,
    );
    if settings.quad_budget > 0 && load.quads_max > u32::from(settings.quad_budget) {
        return Err(format!(
            "flood streaming: a camera in room {} sees {} quads in its view cone at the draw radius ({} sectors), above the budget of {}; lower the draw radius or raise quad_budget",
            load.worst_room, load.quads_max, settings.near_radius, settings.quad_budget
        ));
    }
    notes.push(format!(
        "flood streaming: draw {} sectors, resident {}: pool {} pages ({} KiB: busiest set {} cells, {} pages, plus the largest room); view cone quads at the draw radius mean {:.0}, p90 {:.0}, worst {} (room {})",
        settings.near_radius,
        settings.res_radius,
        pool.pool_pages,
        pool.pool_pages * PAGE_KIB,
        pool.peak_rooms,
        pool.peak_pages,
        load.quads_mean,
        load.quads_p90,
        load.quads_max,
        load.worst_room
    ));
    Ok(FloodPlan { block, notes })
}
