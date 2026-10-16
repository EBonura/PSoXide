//! Which project materials the grid editor's palette offers.
//!
//! A grid project is based on a game project, so its resource list also
//! holds HUD and menu images, button prompts, signal and cable kits, skies
//! and character textures. The palette shows only what a wall, floor or
//! ceiling is likely to use: the materials the world already uses, the
//! images imported with a level (`level_textures/`) and the Graybox set the
//! starter room is made of. A switch shows everything else.

use crate::{MaterialResource, ProjectDocument, ResourceId};

/// Folder an importer puts a level's textures in.
const LEVEL_TEXTURE_FOLDER: &str = "level_textures/";
/// Name prefix of the Graybox surface set.
const GRAYBOX_PREFIX: &str = "Graybox";

/// Whether a material belongs in the palette without being used yet.
pub fn is_world_material(name: &str, material: &MaterialResource) -> bool {
    if material.sky_aperture || material.layered_sky || material.directional_sky {
        return false;
    }
    if name.starts_with(GRAYBOX_PREFIX) {
        return true;
    }
    material
        .psxt_path
        .as_deref()
        .is_some_and(|path| path.replace('\\', "/").starts_with(LEVEL_TEXTURE_FOLDER))
}

/// The palette entries: world materials plus any material the world already
/// uses, or every material with `all`.
pub fn palette_materials(project: &ProjectDocument, all: bool) -> Vec<(ResourceId, String)> {
    let used = project
        .active_scene()
        .world
        .grid()
        .map(|w| w.material_ids())
        .unwrap_or_default();
    project
        .material_options()
        .into_iter()
        .filter(|(id, name)| {
            all || used.contains(id)
                || project.resource(*id).is_some_and(|r| match &r.data {
                    crate::ResourceData::Material(m) => is_world_material(name, m),
                    _ => false,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(path: Option<&str>) -> MaterialResource {
        MaterialResource::opaque(path.map(str::to_string))
    }

    #[test]
    fn level_images_and_graybox_pass_and_everything_else_does_not() {
        assert!(is_world_material(
            "Graybox / Ground 64",
            &material(Some("assets/textures/picotron-grid-64.psxt"))
        ));
        assert!(is_world_material(
            "brick_04",
            &material(Some("level_textures/tiny_texture_pack__brick_04.psxt"))
        ));
        for (name, path) in [
            (
                "MENU_MAIN_PIECE_FULL",
                "../default/assets/textures/menu_main_piece_full.psxt",
            ),
            (
                "UI Prompt: Cross",
                "../default/assets/textures/ui/prompts/kenney_ps_cross_16.psxt",
            ),
            (
                "DP City / Bright Green Signal",
                "../default/assets/textures/dp_city_kit/dp_city_signal_green.psxt",
            ),
            (
                "Aletha Crystal",
                "assets/textures/aletha_mirror_128_4bit.psxt",
            ),
            (
                "DP Fog City Cube Sky",
                "../default/assets/textures/sky/dp_fog_city_cube_4bpp.psxt",
            ),
        ] {
            assert!(!is_world_material(name, &material(Some(path))), "{name}");
        }
        let mut aperture = material(Some("level_textures/x.psxt"));
        aperture.sky_aperture = true;
        assert!(!is_world_material("Any", &aperture));
    }
}
