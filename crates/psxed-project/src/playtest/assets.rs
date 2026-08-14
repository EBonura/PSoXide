//! Asset and resource loading helpers for playtest cooking.

use super::*;
use crate::Resource;

pub(super) fn resolve_path(stored: &str, project_root: &Path) -> PathBuf {
    if Path::new(stored).is_absolute() {
        PathBuf::from(stored)
    } else {
        project_root.join(stored)
    }
}

/// Strip a free-form name down to a filesystem-safe stem.
pub(super) fn sanitise_model_dirname(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "model".to_string()
    } else {
        trimmed
    }
}

pub(super) fn find_resource(project: &ProjectDocument, id: ResourceId) -> Option<&Resource> {
    project.resources.iter().find(|r| r.id == id)
}

/// Validate a room-material `.psxt` blob is 4bpp (16-entry
/// CLUT). Both the editor preview material upload path and the
/// runtime room material slots assume 4bpp; other depths
/// render with wrong colours.
pub(super) fn expect_room_material_depth(label: &str, bytes: &[u8]) -> Result<(), String> {
    let texture = psx_asset::Texture::from_bytes(bytes)
        .map_err(|e| format!("texture '{label}' parse failed: {e:?}"))?;
    if texture.clut_entries() != 16 {
        return Err(format!(
            "texture '{label}' must be 4bpp (16-entry CLUT) for room materials; found {} entries",
            texture.clut_entries(),
        ));
    }
    if !is_supported_room_material_dimension(texture.width())
        || !is_supported_room_material_dimension(texture.height())
    {
        return Err(format!(
            "texture '{label}' must be a power-of-two room material no larger than 128x128 texels and aligned to 8-texel texture-window units; found {}x{}",
            texture.width(),
            texture.height(),
        ));
    }
    Ok(())
}

fn is_supported_room_material_dimension(size: u16) -> bool {
    (8..=128).contains(&size) && size.is_power_of_two() && size.is_multiple_of(8)
}

/// Resolve a Material Lab source to a stable cook-cache key and PSXT bytes.
/// Generated recipes are baked on the host, so they cost no CPU on PS1 and
/// travel through the same validated 4bpp runtime path as imported images.
pub(super) fn material_texture_bytes(
    project: &ProjectDocument,
    resource: &Resource,
    project_root: &Path,
) -> Result<Option<(String, Vec<u8>)>, String> {
    crate::resolve_material_texture_psxt(project, resource.id, project_root)
}

/// Read a material's `.psxt` bytes from disk. Resolves `psxt_path`
/// first as-is (absolute paths), then relative to `project_root`.
/// Returns a string error rather than `io::Error` so callers can
/// prepend room/material context. `label` names the owning resource
/// in error messages.
pub(super) fn load_psxt_bytes(
    label: &str,
    psxt_path: &str,
    project_root: &Path,
) -> Result<Vec<u8>, String> {
    if psxt_path.is_empty() {
        return Err(format!("texture resource '{label}' has empty path"));
    }
    let path = if Path::new(psxt_path).is_absolute() {
        PathBuf::from(psxt_path)
    } else {
        project_root.join(psxt_path)
    };
    std::fs::read(&path).map_err(|e| {
        format!(
            "failed to read texture '{label}' at {}: {e}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_material_uses_the_standard_room_texture_cook_path() {
        let mut project = ProjectDocument::new("transition-cook");
        let generated = |color| {
            ResourceData::Material(crate::MaterialResource {
                texture_mode: crate::MaterialTextureMode::Generated,
                generated: crate::GeneratedMaterialTexture {
                    size: 32,
                    base_color: color,
                    noise_enabled: false,
                    ..crate::GeneratedMaterialTexture::default()
                },
                ..crate::MaterialResource::opaque(None)
            })
        };
        let sand = project.add_resource("Sand", generated([176, 136, 80]));
        let stone = project.add_resource("Stone", generated([72, 80, 96]));
        let transition = project.add_resource(
            "Sand over stone",
            ResourceData::Material(crate::MaterialResource {
                texture_mode: crate::MaterialTextureMode::Transition,
                transition: crate::TransitionMaterialTexture {
                    source_a: Some(stone),
                    source_b: Some(sand),
                    ..crate::TransitionMaterialTexture::default()
                },
                ..crate::MaterialResource::opaque(None)
            }),
        );
        let resource = project.resource(transition).expect("transition resource");
        let (key, bytes) = material_texture_bytes(&project, resource, Path::new("."))
            .expect("cook resolves")
            .expect("transition has texture bytes");
        assert!(key.starts_with("@material-transition:"));
        expect_room_material_depth("transition", &bytes).expect("normal room PSXT accepted");
    }

    #[test]
    fn room_material_rejects_non_texture_window_dimensions() {
        let mut bytes = std::fs::read(
            crate::legacy_grid_starter_dir().join("assets/textures/delven_01_slateflr1a_q2.psxt"),
        )
        .expect("starter Delven texture exists");
        // AssetHeader is 12 bytes; TextureHeader width/height live at
        // payload offsets 2/4. Mutating only the dimensions is enough
        // to exercise the room-material contract.
        bytes[14..16].copy_from_slice(&48u16.to_le_bytes());

        let error = expect_room_material_depth("Odd Tile", &bytes).expect_err("48-wide rejected");
        assert!(error.contains("power-of-two room material"));
    }
}
