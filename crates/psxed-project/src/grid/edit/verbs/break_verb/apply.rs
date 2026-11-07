//! Applying a plan, under the walkability mask.

use std::collections::BTreeSet;

use crate::grid::auto_split;
use crate::grid::cook::RuntimeUnits;
use crate::grid::derive;
use crate::grid::edit::natural::make_split_level;
use crate::grid::sector::GridSector;
use crate::grid::surface::{GridSurface, VOID_A, VOID_B};
use crate::grid::walk::{self, analyse, LocalProblems};
use crate::grid::world::GridWorld;

use super::super::rules::{apply_floor_rule, floor_class};
use super::plan::{plan, Plan, Surf};
use super::{BreakParams, BreakReport, WalkCount};

/// Retries of an operation the mask refuses, each at half the size.
const RETRIES: u32 = 4;
/// Reasons listed in a report.
const MAX_REJECTS: usize = 12;

fn sector_mut(world: &mut GridWorld, room: usize, cell: [i32; 2]) -> Option<&mut GridSector> {
    let r = world.rooms.get_mut(room)?;
    let [x, z] = r.local(cell)?;
    r.sector_mut(x, z)
}

fn surface_of(sector: &mut GridSector, surf: Surf) -> Option<&mut GridSurface> {
    match surf {
        Surf::Floor => sector.ground_floor_mut(),
        Surf::Ceiling => sector.ground_ceiling_mut(),
    }
}

/// `delta` shrunk by `shrink` halvings, rounded towards zero to a multiple of
/// `step`.
fn shrunk(delta: i32, shrink: u32, step: i32) -> i32 {
    delta / (1 << shrink) / step * step
}

fn corner_at(cell: [i32; 2], at: [i32; 2]) -> usize {
    match (at[0] - cell[0], at[1] - cell[1]) {
        (0, 0) => 0,
        (1, 0) => 1,
        (1, 1) => 2,
        _ => 3,
    }
}

/// What an operation changes, taken so a refused one can be put back.
struct Saved {
    sectors: Vec<([i32; 2], Option<GridSector>)>,
    overrides: crate::grid::overrides::EdgeOverrides,
}

fn save(world: &GridWorld, room: usize, cells: &[[i32; 2]]) -> Saved {
    let r = &world.rooms[room];
    Saved {
        sectors: cells
            .iter()
            .map(|&c| (c, r.local(c).and_then(|[x, z]| r.sector(x, z)).cloned()))
            .collect(),
        overrides: r.edge_overrides.clone(),
    }
}

fn put_back(world: &mut GridWorld, room: usize, saved: Saved) {
    let r = &mut world.rooms[room];
    for (cell, sector) in saved.sectors {
        if let Some([x, z]) = r.local(cell) {
            r.set_sector(x, z, sector);
        }
    }
    r.edge_overrides = saved.overrides;
}

/// Apply one operation at `shrink` halvings. Returns whether it changed
/// anything.
fn apply_one(
    world: &mut GridWorld,
    room: usize,
    op: &Plan,
    shrink: u32,
    params: &BreakParams,
    units: Option<&RuntimeUnits>,
) -> bool {
    let step = params.snap.step();
    let rules = world.rooms[room].defaults.rules;
    let y_offset = world.rooms[room].y_offset;
    let mut changed = false;
    match op {
        Plan::Bite {
            cell,
            surf,
            tri,
            split,
            delta,
            from,
            ..
        } => {
            let delta = shrunk(*delta, shrink, step);
            if delta == 0 {
                return false;
            }
            let material = from.filter(|_| params.mix_materials).and_then(|o| {
                let r = &world.rooms[room];
                r.local(o)
                    .and_then(|[x, z]| r.sector(x, z))
                    .and_then(|s| s.ground_floor())
                    .and_then(|f| f.tri_a.material)
            });
            let Some(sector) = sector_mut(world, room, *cell) else {
                return false;
            };
            let before = floor_class(units, y_offset, sector);
            let Some(surface) = surface_of(sector, *surf) else {
                return false;
            };
            if surface.heights_b.is_some() {
                return false;
            }
            surface.split = *split;
            surface.auto_split = false;
            make_split_level(surface);
            let heights = if *tri == 0 {
                &mut surface.heights
            } else {
                surface.heights_b.get_or_insert(surface.heights)
            };
            for h in heights.iter_mut() {
                *h = h.saturating_add(delta);
            }
            if let (Some(m), Surf::Floor) = (material, surf) {
                surface.tri_mut(*tri).material = Some(m);
            }
            changed = true;
            if *surf == Surf::Floor {
                apply_floor_rule(&rules, units, y_offset, before, sector);
            }
        }
        Plan::Vertex {
            at,
            surf,
            delta,
            cells,
            ..
        } => {
            let delta = shrunk(*delta, shrink, step);
            if delta == 0 {
                return false;
            }
            for &cell in cells {
                let Some(sector) = sector_mut(world, room, cell) else {
                    continue;
                };
                let before = floor_class(units, y_offset, sector);
                let Some(surface) = surface_of(sector, *surf) else {
                    continue;
                };
                let c = corner_at(cell, *at);
                surface.heights[c] = surface.heights[c].saturating_add(delta);
                if let Some(b) = surface.heights_b.as_mut() {
                    b[c] = b[c].saturating_add(delta);
                }
                auto_split::resplit(surface);
                changed = true;
                if *surf == Surf::Floor {
                    apply_floor_rule(&rules, units, y_offset, before, sector);
                }
            }
        }
        Plan::Top {
            cell, edge, ends, ..
        } => {
            let ends = ends.map(|e| shrunk(i32::from(e), shrink, step) as i16);
            if ends == [0, 0] {
                return false;
            }
            let key = derive::edge_key(*cell, *edge);
            world.rooms[room]
                .edge_overrides
                .edit(key, |o| o.top_delta = ends);
            changed = true;
        }
        Plan::Gap { cell } => {
            let Some(sector) = sector_mut(world, room, *cell) else {
                return false;
            };
            let (floor, ceiling) = sector.ground_floor_and_ceiling_mut();
            for surface in [floor, ceiling].into_iter().flatten() {
                surface.void = VOID_A | VOID_B;
                changed = true;
            }
        }
    }
    changed
}

fn describe(before: LocalProblems, after: LocalProblems) -> &'static str {
    if after.steps > before.steps {
        "make a step taller than the character climbs"
    } else if after.steep > before.steep {
        "leave a floor too steep to stand on"
    } else if after.low_headroom > before.low_headroom {
        "leave less headroom than the body"
    } else {
        "break the route"
    }
}

fn label(op: &Plan) -> String {
    match op {
        Plan::Bite { cell, surf, .. } => format!(
            "{} crack at {cell:?}",
            if *surf == Surf::Floor {
                "floor"
            } else {
                "ceiling"
            }
        ),
        Plan::Vertex { at, surf, .. } => format!(
            "{} vertex at {at:?}",
            if *surf == Surf::Floor {
                "floor"
            } else {
                "ceiling"
            }
        ),
        Plan::Top { cell, edge, .. } => format!("wall top at {cell:?} {}", edge.label()),
        Plan::Gap { cell } => format!("gap at {cell:?}"),
    }
}

fn count(world: &GridWorld, params: &BreakParams) -> Option<WalkCount> {
    let analysis = analyse(world, &params.spawns, &params.limits)?;
    if !analysis.reach_ran {
        return None;
    }
    let c = analysis.counts();
    let standable = analysis
        .rooms
        .iter()
        .map(|r| r.standable_count())
        .sum::<usize>();
    Some(WalkCount {
        standable,
        unreachable: c[4],
        steps: c[2],
        steep: c[0],
    })
}

pub(super) fn run(
    world: &mut GridWorld,
    room: usize,
    cells: &[[i32; 2]],
    params: &BreakParams,
    intensity: u32,
    seed: u32,
    report: &mut BreakReport,
) {
    let units = RuntimeUnits::new(world.scale).ok();
    let before = count(world, params);
    let operations = plan(world, room, cells, params, intensity, seed);
    let mut touched: BTreeSet<[i32; 2]> = BTreeSet::new();
    for op in &operations {
        let op_cells = op.cells();
        let problems =
            |world: &GridWorld| walk::local_problems(world, room, &op_cells, &params.limits);
        let base = params.keep_walkable.then(|| problems(world)).flatten();
        let mut done = false;
        let mut last_reason = "";
        for shrink in 0..RETRIES {
            let saved = save(world, room, &op_cells);
            if !apply_one(world, room, op, shrink, params, units.as_ref()) {
                put_back(world, room, saved);
                break;
            }
            let mut ok = true;
            if let Some(base) = base {
                if let Some(now) = problems(world) {
                    if now.total() > base.total() {
                        ok = false;
                        last_reason = describe(base, now);
                    }
                }
            }
            // A gap can cut the route; only a flood can tell.
            if ok && params.keep_walkable && matches!(op, Plan::Gap { .. }) {
                let (worse, reason) = match (before, count(world, params)) {
                    (Some(b), Some(a)) => (a.unreachable > b.unreachable, "cut the route"),
                    _ => (true, "cut the route (no player spawn proves otherwise)"),
                };
                if worse {
                    ok = false;
                    last_reason = reason;
                }
            }
            if ok {
                done = true;
                report.shrunk += usize::from(shrink > 0);
                break;
            }
            put_back(world, room, saved);
        }
        if done {
            touched.extend(op_cells.iter().copied());
            tally(op, report);
        } else if !last_reason.is_empty() {
            report.rejected += 1;
            if report.rejects.len() < MAX_REJECTS {
                report
                    .rejects
                    .push(format!("{}: would {last_reason}", label(op)));
            }
        }
    }
    report.touched = touched.len();
    if !touched.is_empty() {
        super::super::rules::ensure_strata(&mut world.rooms[room]);
    }
    report.walk = match (before, count(world, params)) {
        (Some(b), Some(a)) => Some([b, a]),
        _ => None,
    };
}

fn tally(op: &Plan, report: &mut BreakReport) {
    match op {
        Plan::Bite { surf, lip, .. } => match (surf, lip) {
            (Surf::Ceiling, _) => report.ceiling += 1,
            (Surf::Floor, true) => report.bites += 1,
            (Surf::Floor, false) => report.cracked += 1,
        },
        Plan::Vertex { surf, lip, .. } => match (surf, lip) {
            (Surf::Ceiling, _) => report.ceiling += 1,
            (Surf::Floor, true) => report.crumbled += 1,
            (Surf::Floor, false) => report.tilted += 1,
        },
        Plan::Top { stub, .. } => {
            report.tops += 1;
            report.stubs += usize::from(*stub);
        }
        Plan::Gap { .. } => report.gaps += 1,
    }
}
