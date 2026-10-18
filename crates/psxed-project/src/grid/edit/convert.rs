//! Converting a legacy room to derived walls.
//!
//! The only migration there is, explicit and undoable. A room created before
//! derived walls keeps every wall record until this runs. Converting never
//! changes how the room looks or plays in [`ConvertMode::PreserveLook`]:
//!
//! * an authored wall that is exactly a derived face (same side, same four
//!   heights) is removed and its material, placement and solidity move into
//!   the segment's override;
//! * every derived face with no such counterpart is hidden, and a closure
//!   wall that the room never had is also made a ghost, so nothing new
//!   appears and nothing new blocks;
//! * authored walls that match nothing stay as they are.
//!
//! [`ConvertMode::FillNewWalls`] converts the same walls but leaves the new
//! faces visible, which closes cliffs and room edges the legacy room left
//! open.

use std::collections::BTreeMap;

use crate::face_uv::FaceUv;

use super::super::derive::{self, DerivedKind, DerivedWall};
use super::super::overrides::{EdgeKey, EdgeOverride, WallMode};
use super::super::room::GridRoom;
use super::super::sector::WallEdge;
use super::super::surface::{GridWall, WallFacing};
use super::super::units::{SECTOR_UNITS, TEXELS_PER_SECTOR};
use super::super::world::GridWorld;

/// What to do with derived faces the room never had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvertMode {
    /// Hide them: the room looks and plays exactly as before.
    PreserveLook,
    /// Show them.
    FillNewWalls,
}

/// What a conversion did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConvertReport {
    /// Authored walls that became derived faces.
    pub converted: usize,
    /// Authored walls that stayed (they match no derived face).
    pub kept_authored: usize,
    /// Derived faces hidden to keep the look.
    pub hidden: usize,
    /// Derived faces left visible that the room did not have before.
    pub filled: usize,
}

/// One change to a segment's override.
type OverrideEdit = Box<dyn FnOnce(&mut EdgeOverride)>;

/// Where an authored wall that matches a derived face lives.
struct Match {
    cell: [i32; 2],
    edge: WallEdge,
    index: usize,
    /// The wall is stored on the far side and faces back at the host.
    mirrored: bool,
}

/// Texel `u` of the centre of an edge's face, for the mirror compensation.
fn centre_u(cell: [i32; 2], edge: WallEdge) -> f64 {
    let texel = f64::from(TEXELS_PER_SECTOR) / f64::from(SECTOR_UNITS);
    let (a, b) = edge.endpoints();
    let at = |e: [i32; 2]| {
        [
            cell[0] * SECTOR_UNITS + e[0] * SECTOR_UNITS,
            cell[1] * SECTOR_UNITS + e[1] * SECTOR_UNITS,
        ]
    };
    let u = |p: [i32; 2]| match edge {
        WallEdge::North => f64::from(p[0]) * texel,
        WallEdge::East => f64::from(p[1]) * texel,
        WallEdge::South => -f64::from(p[0]) * texel,
        _ => -f64::from(p[1]) * texel,
    };
    (u(at(a)) + u(at(b))) / 2.0
}

/// The authored wall that is exactly `wall` seen from its host, if any.
fn find_match(room: &GridRoom, wall: &DerivedWall) -> Option<Match> {
    let heights = wall.heights?;
    let candidates: Vec<([i32; 2], WallEdge, WallFacing, bool)> = if wall.edge.is_diagonal() {
        // A diagonal has no far side. Only a face seen from the front of
        // the edge reads the same texels as the authored wall on it; one
        // seen from the back stays as it is (the face is hidden instead).
        if wall.back {
            return None;
        }
        vec![(wall.host, wall.edge, WallFacing::Front, false)]
    } else {
        let (opposite, offset) = wall.edge.opposite()?;
        vec![
            (wall.host, wall.edge, WallFacing::Front, false),
            (
                [wall.host[0] + offset[0], wall.host[1] + offset[1]],
                opposite,
                WallFacing::Back,
                true,
            ),
        ]
    };
    for (cell, edge, facing, mirrored) in candidates {
        let [x, z] = room.local(cell)?;
        let Some(sector) = room.sector(x, z) else {
            continue;
        };
        let want = if mirrored {
            [heights[1], heights[0], heights[3], heights[2]]
        } else {
            heights
        };
        let found = sector
            .walls(edge)
            .iter()
            .position(|w| w.heights == want && w.facing == facing);
        if let Some(index) = found {
            return Some(Match {
                cell,
                edge,
                index,
                mirrored,
            });
        }
    }
    None
}

/// The override placement that reproduces `authored`'s texels on a face
/// seen from `host`. `None` when the mirror compensation does not fit.
fn placement(
    authored: &GridWall,
    host: ([i32; 2], WallEdge),
    mirrored: bool,
) -> Option<Option<FaceUv>> {
    let mut uv = authored.uv;
    if mirrored {
        // The far side's texture runs the other way along the same line, and
        // about the opposite centre: mirror the axis and shift by twice the
        // centre. Exact while the centre is a whole texel, which it is for
        // whole-sector faces.
        let shift = 2.0 * centre_u(host.0, host.1);
        if shift.fract() != 0.0 {
            return None;
        }
        let offset = i32::from(uv.offset_texels[0]) - shift as i32;
        uv.offset_texels[0] = i16::try_from(offset).ok()?;
        uv.scale_q8[0] = if uv.scale_q8[0] == 0 {
            -256
        } else {
            uv.scale_q8[0].checked_neg()?
        };
    }
    Some((!uv.is_identity()).then_some(uv))
}

/// Convert `room` of `world` to derived walls. `None` for a missing room and
/// for one with cells of other sizes (merge them back first).
pub fn convert_to_derived(
    world: &mut GridWorld,
    room: usize,
    mode: ConvertMode,
) -> Option<ConvertReport> {
    let r = world.rooms.get(room)?;
    if r.wall_mode == WallMode::Derived {
        return Some(ConvertReport::default());
    }
    // The conversion reads whole sectors; merge the cells back first.
    if !r.leaves.is_sectors_only() {
        return None;
    }
    // Every derived face of the room, grouped by physical segment.
    let mut groups: BTreeMap<EdgeKey, Vec<DerivedWall>> = BTreeMap::new();
    for (x, z, _) in r.iter() {
        for wall in derive::cell_walls(r, r.world_cell(x, z)) {
            groups.entry(wall.key).or_default().push(wall);
        }
    }
    struct Plan {
        key: EdgeKey,
        matched: Option<(Match, Option<FaceUv>, GridWall)>,
        walls: Vec<DerivedWall>,
    }
    let mut plans: Vec<Plan> = Vec::new();
    for (key, walls) in groups {
        // Several faces on one segment share one override, so only a lone
        // face can carry an authored wall's look.
        let matched = (walls.len() == 1)
            .then(|| {
                let found = find_match(r, &walls[0])?;
                let [x, z] = r.local(found.cell)?;
                let authored = r.sector(x, z)?.walls(found.edge).get(found.index)?.clone();
                let uv = placement(&authored, (walls[0].host, walls[0].edge), found.mirrored)?;
                Some((found, uv, authored))
            })
            .flatten();
        plans.push(Plan {
            key,
            matched,
            walls,
        });
    }
    let mut report = ConvertReport::default();
    let mut removals: Vec<([i32; 2], WallEdge, usize)> = Vec::new();
    let mut edits: Vec<(EdgeKey, OverrideEdit)> = Vec::new();
    for plan in plans {
        if let Some((found, uv, authored)) = plan.matched {
            let wall = &plan.walls[0];
            report.converted += 1;
            removals.push((found.cell, found.edge, found.index));
            // No overrides exist yet, so this is the material the chain gives.
            let chained = wall.material;
            let kind = wall.kind;
            edits.push((
                plan.key,
                Box::new(move |o| {
                    if authored.material != chained {
                        o.material = authored.material;
                    }
                    o.uv = uv;
                    o.solid = match (kind, authored.solid) {
                        (DerivedKind::Closure, true) => None,
                        (DerivedKind::Closure, false) => Some(false),
                        (_, true) => Some(true),
                        (_, false) => None,
                    };
                }),
            ));
            continue;
        }
        for wall in &plan.walls {
            if mode == ConvertMode::FillNewWalls {
                report.filled += 1;
                continue;
            }
            report.hidden += 1;
            let closure = wall.kind == DerivedKind::Closure;
            edits.push((
                plan.key,
                Box::new(move |o| {
                    o.hidden = true;
                    if closure {
                        o.solid = Some(false);
                    }
                }),
            ));
        }
    }
    // Apply. Removals go from the highest stack index down.
    removals.sort_by_key(|&(cell, edge, index)| (cell, edge, std::cmp::Reverse(index)));
    let r = world.rooms.get_mut(room)?;
    for (cell, edge, index) in removals {
        let [x, z] = r.local(cell)?;
        let stack = r.sector_mut(x, z)?.walls_mut(edge);
        if index < stack.len() {
            stack.remove(index);
        }
    }
    for (key, edit) in edits {
        r.edge_overrides.edit(key, edit);
    }
    r.wall_mode = WallMode::Derived;
    report.kept_authored = r.iter().map(|(_, _, s)| s.wall_count()).sum();
    Some(report)
}
