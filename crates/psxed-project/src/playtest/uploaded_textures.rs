//! Which level textures the loading screen uploads and then lets go of.
//!
//! A level material's texture is read off the disc with the session's
//! persistent assets and stays in RAM for the whole session, yet the guest
//! reads it only to upload it to VRAM, where it stays until gameplay ends. A
//! texture nothing else uses is cooked [`StreamedClass::UploadedGameplay`]
//! instead: the loading screen reads those first, uploads every one, and the
//! persistent assets are then read into the same RAM
//! (`PersistentAssetStreamer::begin_group`), so the arena holds the larger of
//! the two groups rather than both.

use std::collections::BTreeSet;

use super::{PlaytestAssetKind, PlaytestModelMaterialOverride, PlaytestPackage, StreamedClass};

/// Cook every level texture that only the level's materials use as
/// [`StreamedClass::UploadedGameplay`]. Returns how many there are.
pub(super) fn mark(package: &mut PlaytestPackage) -> usize {
    let elsewhere = texture_refs_outside_the_level_materials(package);
    let mut count = 0;
    for &index in &package.world_geometry.texture_asset_indices {
        if elsewhere.contains(&index) {
            continue;
        }
        let Some(asset) = package.assets.get_mut(index) else {
            continue;
        };
        if asset.kind == PlaytestAssetKind::Texture
            && asset.streamed_class == StreamedClass::PersistentGameplay
        {
            asset.streamed_class = StreamedClass::UploadedGameplay;
            count += 1;
        }
    }
    count
}

/// Level materials whose texture the loading screen uploads: the VRAM slots
/// the guest may hold beyond what it held when each room bound its own
/// materials as it arrived. A level whose rooms are all resident at once
/// bound every material on its first frame already, so it needs none.
#[must_use]
pub fn extra_vram_slots(package: &PlaytestPackage) -> usize {
    let world = &package.world_geometry;
    if world.pool.pool_pages >= world.pool.total_pages {
        return 0;
    }
    world
        .texture_asset_indices
        .iter()
        .filter(|&&index| {
            package
                .assets
                .get(index)
                .is_some_and(|asset| asset.streamed_class == StreamedClass::UploadedGameplay)
        })
        .count()
}

/// Every texture asset the package names anywhere but in the level's
/// materials: room skies and probes, models, material overrides, props, UI
/// nodes and particles.
fn texture_refs_outside_the_level_materials(package: &PlaytestPackage) -> BTreeSet<usize> {
    fn add_override(refs: &mut BTreeSet<usize>, material: &PlaytestModelMaterialOverride) {
        refs.extend(material.texture_asset_index);
        if let Some(layer) = material.secondary_layer {
            refs.extend(layer.texture_asset_index);
        }
    }
    let mut refs = BTreeSet::new();
    for room in &package.rooms {
        refs.extend(room.reflection_probe_asset_index);
        refs.extend(room.sky.texture_asset_index);
        refs.extend(room.sky.cloud_layer.texture_asset_index);
    }
    for model in &package.models {
        refs.insert(model.mesh_asset_index);
        refs.extend(model.texture_asset_index);
    }
    for instance in &package.model_instances {
        if let Some(material) = &instance.material_override {
            add_override(&mut refs, material);
        }
    }
    for character in &package.characters {
        if let Some(material) = &character.material_override {
            add_override(&mut refs, material);
        }
    }
    for prop in &package.image_props {
        refs.insert(prop.texture_asset_index);
    }
    for prop in &package.box_props {
        refs.extend(prop.texture_asset_indices.iter().flatten());
    }
    for prop in &package.cylinder_props {
        refs.extend(prop.texture_asset_indices.iter().flatten());
    }
    for prop in &package.arch_props {
        refs.extend(prop.texture_asset_indices.iter().flatten());
    }
    for node in &package.ui_nodes {
        refs.extend(node.texture_asset);
    }
    for emitter in &package.particle_emitters {
        refs.extend(emitter.texture_asset_index);
    }
    refs
}
