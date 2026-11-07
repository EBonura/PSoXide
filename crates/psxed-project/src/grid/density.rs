//! Switching a world's texel density.
//!
//! The setting is the world's (`GridWorld::texels_per_sector`), so changing
//! it moves every face that takes the world's default mapping. A face made
//! for a smaller texture (32 by 32 at 32 texels per sector is exactly one
//! texture per sector) would show it half size, so the switch can first pin
//! those faces' materials to the density they were laid at: the material's
//! own tiling default (`grid_uv`) is what a face with no mapping of its own
//! reads, derived walls and bands included.

use crate::face_uv::UvMode;
use crate::{ProjectDocument, ResourceData, ResourceId};

use super::plane::PlaneUvMode;
use super::units::SectorTexels;
use super::world::GridWorld;

/// What a switch keeps of how existing faces look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepLook {
    /// Nothing: every face on the world's default takes the new density.
    Nothing,
    /// The faces whose material texture is not 64 by 64. A 64 by 64 texture
    /// is what the grid renderer tiles, so those are the ones the new
    /// density is for; the others (32 by 32 and the like) keep their size.
    OtherTextureSizes,
    /// Every face, whatever its texture.
    Everything,
}

/// What a switch changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DensityChange {
    /// The density before.
    pub from: Option<SectorTexels>,
    /// Materials given their old density as their own tiling default.
    pub materials_pinned: usize,
    /// Tiled planes given their old density as their own mapping.
    pub planes_pinned: usize,
}

/// Set the active scene's world to `texels` texels per sector, keeping what
/// `keep` says of how existing faces look. `texture_width` answers how wide a
/// material's texture is (`None` when it has none). One change to the
/// document, so one undo step. `None` when the scene has no grid world.
pub fn set_texels_per_sector(
    project: &mut ProjectDocument,
    texels: SectorTexels,
    keep: KeepLook,
    texture_width: &dyn Fn(&ProjectDocument, ResourceId) -> Option<u16>,
) -> Option<DensityChange> {
    let world = project.active_scene().world.grid()?;
    let from = world.texels_per_sector;
    let mut change = DensityChange {
        from: Some(from),
        ..DensityChange::default()
    };
    if from == texels {
        return Some(change);
    }
    let pinned = |id: ResourceId| match keep {
        KeepLook::Nothing => false,
        KeepLook::Everything => true,
        KeepLook::OtherTextureSizes => texture_width(project, id).is_some_and(|w| w != 64),
    };
    let materials: Vec<ResourceId> = world
        .material_ids()
        .into_iter()
        .filter(|&id| {
            matches!(
                project.resource(id).map(|r| &r.data),
                Some(ResourceData::Material(m)) if m.grid_uv.is_inherit()
            ) && pinned(id)
        })
        .collect();
    let planes: Vec<(usize, usize)> = tiled_planes(world)
        .filter(|&(room, index)| {
            // A plane with no material shows nothing to keep.
            world.rooms[room].planes[index]
                .material
                .map_or(keep == KeepLook::Everything, pinned)
        })
        .collect();
    let old = UvMode::world(from.get());
    for id in &materials {
        if let Some(ResourceData::Material(m)) = project.resource_mut(*id).map(|r| &mut r.data) {
            m.grid_uv = old;
        }
    }
    change.materials_pinned = materials.len();
    change.planes_pinned = planes.len();
    let world = project.active_scene_mut().world.grid_mut()?;
    for (room, index) in planes {
        world.rooms[room].planes[index].uv.mode = old;
    }
    world.texels_per_sector = texels;
    Some(change)
}

/// The planes laid out by the world's default (`Tile` with no mapping of
/// their own), as `(room, index)`.
fn tiled_planes(world: &GridWorld) -> impl Iterator<Item = (usize, usize)> + '_ {
    world.rooms.iter().enumerate().flat_map(|(room, r)| {
        r.planes
            .iter()
            .enumerate()
            .filter(|(_, p)| p.uv.mode.is_inherit() && p.uv_mode == PlaneUvMode::Tile)
            .map(move |(index, _)| (room, index))
    })
}
