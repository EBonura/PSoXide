//! Tops the ground fit lost from the towers that carry a bridge.
//!
//! The ground fit smooths the model into a heightfield and cuts the tops off
//! thin tall towers. Where the tower holds up a bridge, a gate or a wall
//! (it is big, or a bridge ends beside it) the lost height is built back as
//! slabs standing on the fitted ground. Small, lone spires stay lost: the
//! model's scattered pinnacles are scenery for billboards, not structure.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::columns::{
    ground_and_floating, snap, Frame, HitField, PlanCell, CELLS_PER_SECTOR, COLUMNS_PER_CELL,
    COLUMNS_PER_SECTOR,
};
use super::pack::components;
use super::FloatConfig;

/// What was found and what was kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapReport {
    /// Quarter-sector cells that lost at least the minimum height.
    pub lost_cells: usize,
    /// Patches of such cells.
    pub patches: usize,
    /// Cells of the patches kept as structural.
    pub kept_cells: usize,
    /// Cells of the patches left out as scenery.
    pub scenery_cells: usize,
    /// The cells left out, `[x, z]`, for drawing what the rule dropped.
    pub scenery: Vec<[i32; 2]>,
}

/// The quarter-sector cells (coordinates from the model's corner) to build
/// back, with `bottom` the lowest ground of the patch and `top` the model's.
///
/// `tops` is the height of the fitted ground per ray column in grid units
/// over the model's ground (see [`super::cover::raster_tops`]), keyed by world
/// column; `near` are the floating cells (quarter sectors) of the bridges.
pub fn cap_cells(
    field: &HitField,
    frame: &Frame,
    config: &FloatConfig,
    tops: &BTreeMap<(i32, i32), f64>,
    near: &[PlanCell],
) -> (Vec<PlanCell>, CapReport) {
    let per = COLUMNS_PER_SECTOR as i32;
    let rays = COLUMNS_PER_CELL * COLUMNS_PER_CELL;
    let need = (rays * config.cap_cover_percent as usize)
        .div_ceil(100)
        .max(1);
    let mut cells: Vec<PlanCell> = Vec::new();
    let mut lowest: HashMap<(i32, i32), i32> = HashMap::new();
    for cz in 0..field.rows() / COLUMNS_PER_CELL {
        for cx in 0..field.cols() / COLUMNS_PER_CELL {
            let mut model: Vec<i32> = Vec::new();
            let mut ground: Vec<i32> = Vec::new();
            for row in cz * COLUMNS_PER_CELL..(cz + 1) * COLUMNS_PER_CELL {
                for col in cx * COLUMNS_PER_CELL..(cx + 1) * COLUMNS_PER_CELL {
                    let (top, _) = ground_and_floating(field.column(row, col), frame, config);
                    let key = (
                        config.origin_sector[0] * per + col as i32,
                        config.origin_sector[1] * per + row as i32,
                    );
                    let Some(&fitted) = tops.get(&key) else {
                        continue;
                    };
                    let fitted = fitted.round() as i32;
                    if top - fitted >= config.cap_min_deficit {
                        model.push(top);
                        ground.push(fitted.max(0));
                    }
                }
            }
            if model.len() < need {
                continue;
            }
            model.sort_unstable();
            let top = snap(model[model.len() / 2]);
            let base = snap(ground.iter().copied().min().unwrap_or(0));
            lowest.insert((cx as i32, cz as i32), base);
            cells.push(PlanCell {
                x: cx as i32,
                z: cz as i32,
                bottom: base,
                top,
                clearance: 0,
            });
        }
    }
    let mut report = CapReport {
        lost_cells: cells.len(),
        ..CapReport::default()
    };
    let (labels, patches) = components(&cells);
    report.patches = patches.len();
    // Sector buckets of the bridges, for the reach test.
    let sector_of = |x: i32, z: i32| {
        (
            x.div_euclid(CELLS_PER_SECTOR),
            z.div_euclid(CELLS_PER_SECTOR),
        )
    };
    let bridge_sectors: HashSet<(i32, i32)> = near.iter().map(|c| sector_of(c.x, c.z)).collect();
    let reach = config.cap_reach;
    let mut base_of: Vec<i32> = vec![i32::MAX; patches.len()];
    let mut by_patch_near: Vec<bool> = vec![false; patches.len()];
    for (cell, &label) in cells.iter().zip(&labels) {
        base_of[label] = base_of[label].min(cell.bottom);
        let (sx, sz) = sector_of(cell.x, cell.z);
        if !by_patch_near[label]
            && (-reach..=reach)
                .any(|dz| (-reach..=reach).any(|dx| bridge_sectors.contains(&(sx + dx, sz + dz))))
        {
            by_patch_near[label] = true;
        }
    }
    let mut out = Vec::new();
    for (cell, &label) in cells.iter().zip(&labels) {
        let structural = patches[label].cells >= config.cap_min_area || by_patch_near[label];
        if !structural {
            report.scenery_cells += 1;
            report.scenery.push([cell.x, cell.z]);
            continue;
        }
        report.kept_cells += 1;
        let thick = (cell.top - base_of[label]).min(config.cap_max_thickness);
        out.push(PlanCell {
            bottom: cell.top - thick,
            ..*cell
        });
    }
    (out, report)
}
