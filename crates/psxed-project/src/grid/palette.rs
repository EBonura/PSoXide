//! Which project materials the grid editor's palette offers.
//!
//! A grid project is based on a game project, so its resource list also
//! holds HUD and menu images, button prompts, skies and character textures.
//! None of those belong on a wall. The palette shows the rest by default and
//! offers a switch to see everything.

use crate::{MaterialResource, ProjectDocument, ResourceData, ResourceId};

/// Texture file name prefixes of images that are not world surfaces.
const NON_WORLD_FILE_PREFIXES: [&str; 3] = ["menu_", "kenney_", "aletha"];
/// Material name prefixes of images that are not world surfaces.
const NON_WORLD_NAME_PREFIXES: [&str; 4] = ["UI ", "MENU", "Sky", "Aletha"];
/// Folders that hold images that are not world surfaces.
const NON_WORLD_FOLDERS: [&str; 3] = ["/ui/", "/sky/", "/prompts/"];

/// Whether a material is something a wall, floor or ceiling would use.
pub fn is_world_material(name: &str, material: &MaterialResource) -> bool {
    if material.sky_aperture || material.layered_sky || material.directional_sky {
        return false;
    }
    if NON_WORLD_NAME_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return false;
    }
    let Some(path) = material.psxt_path.as_deref() else {
        return true;
    };
    let path = path.replace('\\', "/");
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or("");
    !(NON_WORLD_FOLDERS.iter().any(|f| lower.contains(f))
        || path.starts_with("assets/ui/")
        || NON_WORLD_FILE_PREFIXES.iter().any(|p| file.starts_with(p)))
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
                    ResourceData::Material(m) => is_world_material(name, m),
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
    fn world_images_pass_and_hud_menu_sky_and_character_images_do_not() {
        assert!(is_world_material(
            "Graybox / Ground 64",
            &material(Some("assets/textures/picotron-grid-64.psxt"))
        ));
        assert!(is_world_material(
            "brick_04",
            &material(Some("level_textures/tiny_texture_pack__brick_04.psxt"))
        ));
        assert!(is_world_material("Flat", &material(None)));
        assert!(!is_world_material(
            "MENU_MAIN_PIECE_FULL",
            &material(Some("../default/assets/textures/menu_main_piece_full.psxt"))
        ));
        assert!(!is_world_material(
            "UI Prompt: Cross",
            &material(Some(
                "../default/assets/textures/ui/prompts/kenney_ps_cross_16.psxt"
            ))
        ));
        assert!(!is_world_material(
            "Aletha Crystal",
            &material(Some("assets/textures/aletha_mirror_128_4bit.psxt"))
        ));
        assert!(!is_world_material(
            "DP Fog City Cube Sky",
            &material(Some(
                "../default/assets/textures/sky/dp_fog_city_cube_4bpp.psxt"
            ))
        ));
        let mut aperture = material(None);
        aperture.sky_aperture = true;
        assert!(!is_world_material("Any", &aperture));
    }
}
