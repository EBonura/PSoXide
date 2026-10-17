//! Editing derived walls: what the user changes on a face the editor
//! computes lands in the room's [`EdgeOverride`](super::super::overrides::EdgeOverride)
//! for its segment, never in the sectors.

use crate::face_uv::FaceUv;
use crate::ResourceId;

use super::super::derive::{self, DerivedKind};
use super::super::overrides::{EdgeKey, EdgeOverride};
use super::super::world::GridWorld;
use super::refs::{FaceRef, FaceTarget};
use super::surfaces::UvOp;

/// The segment key and kind of a derived face, when `face` names one that
/// exists.
fn locate(world: &GridWorld, face: FaceRef) -> Option<(EdgeKey, DerivedKind)> {
    let wall = face.derived_wall(world)?;
    Some((wall.key, wall.kind))
}

/// Run `edit` on the override of `face`'s segment. Returns whether the
/// override changed.
fn edit_override(
    world: &mut GridWorld,
    face: FaceRef,
    edit: impl FnOnce(&mut EdgeOverride),
) -> bool {
    let Some((key, _)) = locate(world, face) else {
        return false;
    };
    let Some(room) = world.rooms.get_mut(face.room) else {
        return false;
    };
    let before = room.edge_overrides.get(&key).cloned().unwrap_or_default();
    room.edge_overrides.edit(key, edit);
    room.edge_overrides.get(&key).cloned().unwrap_or_default() != before
}

/// Paint a derived face: the override material replaces the chain.
pub fn set_derived_material(
    world: &mut GridWorld,
    face: FaceRef,
    material: Option<ResourceId>,
) -> bool {
    edit_override(world, face, |o| o.material = material)
}

/// Hide or show a derived face.
pub fn set_derived_hidden(world: &mut GridWorld, face: FaceRef, hidden: bool) -> bool {
    edit_override(world, face, |o| o.hidden = hidden)
}

/// Make a derived face solid or passable. `true` is the default for a
/// closure wall (stored as no override) and an explicit wall record for a
/// riser or drop; `false` makes a ghost wall.
pub fn set_derived_solid(world: &mut GridWorld, face: FaceRef, solid: bool) -> bool {
    let Some((_, kind)) = locate(world, face) else {
        return false;
    };
    edit_override(world, face, |o| {
        o.solid = match (kind, solid) {
            (DerivedKind::Closure, true) => None,
            (DerivedKind::Closure, false) => Some(false),
            (_, true) => Some(true),
            (_, false) => None,
        };
    })
}

/// Forget everything set on a derived face's segment.
pub fn reset_derived(world: &mut GridWorld, face: FaceRef) -> bool {
    let Some((key, _)) = locate(world, face) else {
        return false;
    };
    world
        .rooms
        .get_mut(face.room)
        .is_some_and(|r| r.edge_overrides.remove(&key).is_some())
}

/// Whether a derived face blocks movement with a wall record.
pub fn derived_solid(world: &GridWorld, face: FaceRef) -> Option<bool> {
    let wall = face.derived_wall(world)?;
    Some(wall.blocks())
}

/// The texture placement override of a derived face (identity when none).
pub fn derived_uv(world: &GridWorld, face: FaceRef) -> Option<FaceUv> {
    let wall = face.derived_wall(world)?;
    Some(wall.uv.unwrap_or_default())
}

/// Edit the texture placement of a derived face.
pub fn edit_derived_uv(world: &mut GridWorld, face: FaceRef, op: UvOp) -> bool {
    edit_override(world, face, |o| {
        let mut uv = o.uv.unwrap_or_default();
        op.apply(&mut uv);
        o.uv = (!uv.is_identity()).then_some(uv);
    })
}

/// The material a derived face resolves to.
pub fn derived_material(world: &GridWorld, face: FaceRef) -> Option<ResourceId> {
    face.derived_wall(world)?.material
}

/// Whether `face` is hidden.
pub fn is_derived_hidden(world: &GridWorld, face: FaceRef) -> bool {
    face.derived_wall(world).is_some_and(|w| w.hidden)
}

/// Drop the overrides of a room that no longer belong to any derived face
/// (a platform was lowered again). Returns how many were removed.
pub fn clean_overrides(world: &mut GridWorld, room: usize) -> usize {
    let Some(r) = world.rooms.get(room) else {
        return 0;
    };
    let live: std::collections::BTreeSet<EdgeKey> = r
        .iter()
        .flat_map(|(x, z, _)| derive::cell_walls(r, r.world_cell(x, z)))
        .map(|w| w.key)
        .collect();
    let Some(r) = world.rooms.get_mut(room) else {
        return 0;
    };
    let before = r.edge_overrides.len();
    r.edge_overrides.retain(|k| live.contains(k));
    before - r.edge_overrides.len()
}

/// Whether `face` is a derived face target.
pub const fn is_derived(face: &FaceRef) -> bool {
    matches!(face.target, FaceTarget::Derived { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::overrides::WallMode;
    use crate::grid::room::GridRoom;
    use crate::grid::sector::WallEdge;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;

    fn platform() -> GridWorld {
        let mut room = GridRoom::new_derived("r", 3, 3);
        for z in 0..3 {
            for x in 0..3 {
                let s = room.sector_or_insert(x, z).unwrap();
                s.floor = Some(GridSurface::flat(0, None));
                s.ceiling = Some(GridSurface::flat(2048, None));
            }
        }
        room.sector_or_insert(1, 1).unwrap().floor = Some(GridSurface::flat(512, None));
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn riser() -> FaceRef {
        FaceRef::derived(0, [1, 0], WallEdge::South, DerivedKind::Riser)
    }

    #[test]
    fn paint_uv_hide_and_solid_land_in_the_override_and_undo_cleanly() {
        let mut w = platform();
        let face = riser();
        assert!(face.exists(&w));
        let mat: ResourceId = ron::from_str("(9)").unwrap();
        assert!(set_derived_material(&mut w, face, Some(mat)));
        assert!(!set_derived_material(&mut w, face, Some(mat)));
        assert_eq!(derived_material(&w, face), Some(mat));
        assert!(edit_derived_uv(&mut w, face, UvOp::Rotate90));
        assert_eq!(derived_uv(&w, face).unwrap().rotation_deg, 90);
        assert!(set_derived_solid(&mut w, face, true));
        assert_eq!(derived_solid(&w, face), Some(true));
        assert!(set_derived_hidden(&mut w, face, true));
        assert!(is_derived_hidden(&w, face));
        // Undoing every edit leaves no override behind.
        assert!(set_derived_hidden(&mut w, face, false));
        assert!(set_derived_solid(&mut w, face, false));
        assert!(edit_derived_uv(&mut w, face, UvOp::Reset));
        assert!(set_derived_material(&mut w, face, None));
        assert!(w.rooms[0].edge_overrides.is_empty());
    }

    #[test]
    fn lowering_the_platform_orphans_overrides_until_cleaned() {
        let mut w = platform();
        assert!(set_derived_hidden(&mut w, riser(), true));
        w.rooms[0].sector_or_insert(1, 1).unwrap().floor = Some(GridSurface::flat(0, None));
        assert!(!riser().exists(&w));
        assert_eq!(w.rooms[0].edge_overrides.len(), 1);
        assert_eq!(clean_overrides(&mut w, 0), 1);
        assert!(w.rooms[0].edge_overrides.is_empty());
        assert_eq!(w.rooms[0].wall_mode, WallMode::Derived);
    }
}
