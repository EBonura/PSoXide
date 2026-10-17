//! Entity records for the sockets of placed meshes.
//!
//! A hook socket of an asset becomes one `HookPoint` entity per instance, so
//! the runtime's perch code (`hook_runtime`) needs no knowledge of meshes: it
//! finds an ordinary hook point at the right place. The record is placed so the
//! hand's grip (the marker the runtime puts `MARKER_HEIGHT` above the record)
//! lands on the socket's anchor.

use super::schema::{PlaytestEntity, PlaytestEntityKind};
use super::PlaytestValidationReport;
use crate::grid::cook::units::RuntimeUnits;
use crate::grid::mesh::{Socket, HOOK_MARKER_HEIGHT_RU};
use crate::grid::GridWorld;

/// Append one hook-point entity per hook socket of every enabled instance, in
/// room then instance order (so the node-authored hook points keep the slots
/// they always had). Cap overflow is a cook error.
pub(super) fn push_hook_entities(
    world: &GridWorld,
    entities: &mut Vec<PlaytestEntity>,
    report: &mut PlaytestValidationReport,
) {
    let Ok(units) = RuntimeUnits::new(world.scale) else {
        return;
    };
    for room in &world.rooms {
        for mesh in room.meshes.iter().filter(|m| m.enabled) {
            let Some(asset) = world.mesh_assets.get(usize::from(mesh.asset)) else {
                continue;
            };
            let Ok(built) = asset.build() else {
                continue;
            };
            for socket in &built.sockets {
                let Socket::Hook { anchor, yaw } = *socket;
                let hooks = entities
                    .iter()
                    .filter(|e| e.kind == PlaytestEntityKind::HookPoint)
                    .count();
                if hooks >= psx_level::MAX_HOOK_POINTS {
                    report.error(format!(
                        "A project supports at most {} hook points; mesh {} ({}) would add another",
                        psx_level::MAX_HOOK_POINTS,
                        mesh.id,
                        asset.name
                    ));
                    return;
                }
                let g = mesh.place(anchor);
                let heading = (mesh.yaw() + yaw) as f32;
                entities.push(PlaytestEntity {
                    room: 0,
                    kind: PlaytestEntityKind::HookPoint,
                    x: units.rt(g[0]),
                    y: units.rt(room.y_offset + g[1]) - HOOK_MARKER_HEIGHT_RU,
                    z: units.rt(g[2]),
                    yaw: crate::spatial::euler_degrees_to_q12(heading) as i16,
                    resource_slot: 0,
                    flags: psx_level::entity_flags::HOOK_BARE,
                });
            }
        }
    }
}
