//! Host-side generation for compact model material layer textures.

use crate::{
    GeneratedMaterialTexture, GeneratedTextureUv, GridDirection, MaterialTextureMode,
    ProceduralNoiseTexture, ProjectDocument, ResourceData, ResourceId, TransitionMaskShape,
    TransitionMaterialTexture, WorldGrid,
};
use std::{collections::HashMap, path::Path};

const MATERIAL_NEUTRAL_TINT: u8 = 128;

/// Fixed texture size for the generated secondary model layer.
///
/// 128x128 is four times the texel area of the old 64x64 layer while still
/// fitting one PS1 GP0(E2) texture window and a compact 4bpp VRAM allocation.
pub const MODEL_NOISE_TEXTURE_SIZE: u16 = 128;

/// Square, texture-window-friendly environment map baked for every runtime
/// room when at least one reflective material is used by the project.
pub const ROOM_REFLECTION_PROBE_SIZE: u16 = 64;

#[derive(Clone)]
struct ProbeTexture {
    width: u16,
    height: u16,
    indices: Vec<u8>,
    palette: [[u8; 3]; 16],
    tint: [u8; 3],
}

/// Bake a compact room panorama from the active grid's authored surfaces.
///
/// This is intentionally a host-side operation. Each horizontal probe column
/// walks from the room centre toward one compass direction; the upper band
/// samples ceilings, the middle band samples the far wall, and the lower band
/// samples floors from near to far. The result is a deterministic 4bpp texture
/// with enough spatial structure to move convincingly under screen-space
/// reflection UVs without asking the PS1 to capture or quantise a framebuffer.
// TODO(reflection-probe-panorama): This is a material-band approximation, not
// a rendered panoramic image, so it cannot produce a convincing mirror even
// with better runtime UV mapping. Resume by rendering the textured room from
// the probe position in six host-side views, stitching a 128x64 equirectangular
// panorama, then quantising it to 4bpp. Keep that bake off the PS1; dynamic
// objects and multi-probe parallax can remain explicit later limitations.
pub fn generate_room_reflection_probe_psxt(
    project: &ProjectDocument,
    grid: &WorldGrid,
    project_root: &Path,
) -> Result<Vec<u8>, String> {
    let size = usize::from(ROOM_REFLECTION_PROBE_SIZE);
    let mut cache: HashMap<ResourceId, Option<ProbeTexture>> = HashMap::new();
    let mut rgba = Vec::with_capacity(size * size);

    for y in 0..size {
        for x in 0..size {
            let angle = (x as f64 + 0.5) * std::f64::consts::TAU / size as f64;
            let dir_x = angle.sin();
            let dir_z = angle.cos();
            let depth = if y >= 44 { (y - 44) as f64 / 19.0 } else { 1.0 };
            let sector = probe_sector_along_ray(grid, dir_x, dir_z, depth);
            let material = sector.and_then(|sector| {
                if y < 20 {
                    sector.ceiling.as_ref().and_then(|face| face.material)
                } else if y < 44 {
                    probe_wall_material(sector, dir_x, dir_z)
                        .or_else(|| sector.floor.as_ref().and_then(|face| face.material))
                } else {
                    sector.floor.as_ref().and_then(|face| face.material)
                }
            });
            let sampled = material.and_then(|material| {
                probe_texture(project, project_root, material, &mut cache).and_then(|texture| {
                    texture.as_ref().map(|texture| {
                        let u = ((x * usize::from(texture.width)) / size) as u16;
                        let band_y = match y {
                            0..=19 => y * 3,
                            20..=43 => (y - 20) * 3,
                            _ => (y - 44) * 3,
                        };
                        let v = (band_y % usize::from(texture.height)) as u16;
                        sample_probe_texture(texture, u, v)
                    })
                })
            });
            let rgb = sampled.unwrap_or_else(|| probe_fallback_rgb(grid, x, y, size));
            rgba.push([rgb[0], rgb[1], rgb[2], 255]);
        }
    }

    let (palette, indices) = psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
        .map_err(|error| format!("could not quantise room reflection probe: {error}"))?;
    psxed_tex::encode_indexed_psxt(
        ROOM_REFLECTION_PROBE_SIZE,
        ROOM_REFLECTION_PROBE_SIZE,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        false,
    )
    .map_err(|error| format!("could not encode room reflection probe: {error}"))
}

fn probe_sector_along_ray(
    grid: &WorldGrid,
    dir_x: f64,
    dir_z: f64,
    depth: f64,
) -> Option<&crate::GridSector> {
    let centre_x = (f64::from(grid.width) - 1.0) * 0.5;
    let centre_z = (f64::from(grid.depth) - 1.0) * 0.5;
    let steps = usize::from(grid.width.max(grid.depth)).max(1) * 6;
    let stop = ((steps as f64 * depth.clamp(0.0, 1.0)).round() as usize).max(1);
    let mut last = None;
    for step in 0..=stop {
        let distance = step as f64 / 3.0;
        let x = (centre_x + dir_x * distance).round() as i32;
        let z = (centre_z + dir_z * distance).round() as i32;
        if x < 0 || z < 0 || x >= i32::from(grid.width) || z >= i32::from(grid.depth) {
            break;
        }
        if let Some(sector) = grid.sector(x as u16, z as u16) {
            last = Some(sector);
        }
    }
    last.or_else(|| grid.sectors.iter().flatten().next())
}

fn probe_wall_material(sector: &crate::GridSector, dir_x: f64, dir_z: f64) -> Option<ResourceId> {
    let primary = if dir_x.abs() >= dir_z.abs() {
        if dir_x >= 0.0 {
            GridDirection::East
        } else {
            GridDirection::West
        }
    } else if dir_z >= 0.0 {
        GridDirection::North
    } else {
        GridDirection::South
    };
    sector
        .walls
        .get(primary)
        .last()
        .and_then(|wall| wall.material)
        .or_else(|| {
            [
                GridDirection::North,
                GridDirection::East,
                GridDirection::South,
                GridDirection::West,
            ]
            .into_iter()
            .find_map(|direction| {
                sector
                    .walls
                    .get(direction)
                    .last()
                    .and_then(|wall| wall.material)
            })
        })
}

fn probe_texture<'a>(
    project: &ProjectDocument,
    project_root: &Path,
    material: ResourceId,
    cache: &'a mut HashMap<ResourceId, Option<ProbeTexture>>,
) -> Option<&'a Option<ProbeTexture>> {
    if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(material) {
        let decoded = project.resource(material).and_then(|resource| {
            let tint = match &resource.data {
                ResourceData::Material(material) => material.tint,
                ResourceData::Texture { .. } => [MATERIAL_NEUTRAL_TINT; 3],
                _ => return None,
            };
            let bytes = resolve_material_texture_psxt(project, material, project_root)
                .ok()??
                .1;
            decode_probe_texture(&bytes, tint)
        });
        e.insert(decoded);
    }
    cache.get(&material)
}

fn decode_probe_texture(bytes: &[u8], tint: [u8; 3]) -> Option<ProbeTexture> {
    let texture = psx_asset::Texture::from_bytes(bytes).ok()?;
    if texture.depth() != psxed_format::texture::Depth::Bit4 || texture.clut_entries() < 16 {
        return None;
    }
    let mut palette = [[0u8; 3]; 16];
    for (index, color) in palette.iter_mut().enumerate() {
        let offset = index * 2;
        let raw = u16::from_le_bytes([
            texture.clut_bytes()[offset],
            texture.clut_bytes()[offset + 1],
        ]);
        *color = [
            expand_5bit((raw & 0x1f) as u8),
            expand_5bit(((raw >> 5) & 0x1f) as u8),
            expand_5bit(((raw >> 10) & 0x1f) as u8),
        ];
    }
    let mut indices =
        Vec::with_capacity(usize::from(texture.width()) * usize::from(texture.height()));
    for y in 0..texture.height() {
        for x in 0..texture.width() {
            indices.push(texture_4bpp_index(texture, x, y));
        }
    }
    Some(ProbeTexture {
        width: texture.width(),
        height: texture.height(),
        indices,
        palette,
        tint,
    })
}

fn sample_probe_texture(texture: &ProbeTexture, x: u16, y: u16) -> [u8; 3] {
    let x = x % texture.width.max(1);
    let y = y % texture.height.max(1);
    let index = texture.indices[usize::from(y) * usize::from(texture.width) + usize::from(x)];
    let rgb = texture.palette[usize::from(index)];
    [
        modulate(rgb[0], texture.tint[0]),
        modulate(rgb[1], texture.tint[1]),
        modulate(rgb[2], texture.tint[2]),
    ]
}

fn probe_fallback_rgb(grid: &WorldGrid, x: usize, y: usize, size: usize) -> [u8; 3] {
    let horizon = grid.fog_color;
    let ambient = grid.ambient_color;
    let vertical = y as u16 * 255 / (size.saturating_sub(1).max(1) as u16);
    let glint = (((x * 4) % size) as i32 - (size / 2) as i32).unsigned_abs();
    let glint = 24u8.saturating_sub((glint.min(24)) as u8);
    let mut out = [0u8; 3];
    for channel in 0..3 {
        let top = u16::from(horizon[channel]);
        let bottom = u16::from(ambient[channel]) / 2;
        out[channel] = ((top * (255 - vertical) + bottom * vertical + 127) / 255) as u8;
        out[channel] = out[channel].saturating_add(glint / 3);
    }
    out
}

/// Bake deterministic, seamless multi-octave value noise into a 128x128
/// CLUT16 PSXT.
///
/// Palette entry zero remains transparent, while entries 1..15 form a neutral
/// grayscale ramp. Runtime material tint supplies the authored colour.
pub fn generate_model_noise_psxt(settings: ProceduralNoiseTexture) -> Vec<u8> {
    let indices = generate_model_noise_indices(settings);
    let mut palette = [[0u8; 3]; 16];
    for (index, rgb) in palette.iter_mut().enumerate().skip(1) {
        let value = (index as u8).saturating_mul(17);
        *rgb = [value, value, value];
    }
    psxed_tex::encode_indexed_psxt(
        MODEL_NOISE_TEXTURE_SIZE,
        MODEL_NOISE_TEXTURE_SIZE,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        true,
    )
    .expect("fixed-size procedural noise PSXT input is valid")
}

/// Generate the raw 4bpp indices used by [`generate_model_noise_psxt`].
pub fn generate_model_noise_indices(settings: ProceduralNoiseTexture) -> Vec<u8> {
    generate_noise_indices(
        MODEL_NOISE_TEXTURE_SIZE,
        settings,
        GeneratedTextureUv::default(),
    )
}

/// Bake a Material Lab base-colour plus noise recipe into one opaque 4bpp PSXT.
pub fn generate_material_texture_psxt(settings: GeneratedMaterialTexture) -> Vec<u8> {
    let size = normalize_generated_texture_size(settings.size);
    let indices = if settings.noise_enabled {
        generate_noise_indices(size, settings.noise, settings.noise_uv)
    } else {
        vec![0; usize::from(size) * usize::from(size)]
    };
    let mut palette = [[0u8; 3]; 16];
    for (index, rgb) in palette.iter_mut().enumerate() {
        if settings.noise_enabled {
            let t = index as u16;
            for (channel, out) in rgb.iter_mut().enumerate() {
                let a = u16::from(settings.base_color[channel]);
                let b = u16::from(settings.noise_color[channel]);
                *out = ((a * (15 - t) + b * t + 7) / 15) as u8;
            }
        } else {
            *rgb = settings.base_color;
        }
    }
    psxed_tex::encode_indexed_psxt(
        size,
        size,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        false,
    )
    .expect("normalized generated material PSXT input is valid")
}

/// Clamp arbitrary saved values to the sizes supported by the room texture
/// window and the Material Lab preset buttons.
pub const fn normalize_generated_texture_size(size: u16) -> u16 {
    if size <= 8 {
        8
    } else if size <= 16 {
        16
    } else if size <= 32 {
        32
    } else if size <= 64 {
        64
    } else {
        128
    }
}

/// Resolve any static authoring material source to its cooked PSXT bytes.
///
/// Transition materials recurse through their two source materials with cycle
/// detection. The returned key is stable for cook-time deduplication; runtime
/// code only receives the final bytes and never sees the authoring recipe.
pub fn resolve_material_texture_psxt(
    project: &ProjectDocument,
    material_id: ResourceId,
    project_root: &Path,
) -> Result<Option<(String, Vec<u8>)>, String> {
    resolve_material_texture_psxt_inner(project, material_id, project_root, &mut Vec::new())
}

fn resolve_material_texture_psxt_inner(
    project: &ProjectDocument,
    material_id: ResourceId,
    project_root: &Path,
    stack: &mut Vec<ResourceId>,
) -> Result<Option<(String, Vec<u8>)>, String> {
    if stack.contains(&material_id) {
        let chain = stack
            .iter()
            .chain(std::iter::once(&material_id))
            .map(|id| format!("#{}", id.raw()))
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(format!("transition material source cycle: {chain}"));
    }
    let resource = project
        .resource(material_id)
        .ok_or_else(|| format!("material #{} does not exist", material_id.raw()))?;
    stack.push(material_id);
    let result = match &resource.data {
        ResourceData::Texture { psxt_path } => {
            load_material_psxt(&resource.name, psxt_path, project_root)
                .map(|bytes| Some((psxt_path.clone(), bytes)))
        }
        ResourceData::Material(material) => {
            let flipbook = material.animation.flipbook.normalized();
            if material.animation.mode == crate::MaterialAnimationMode::Flipbook
                && (flipbook.source_a.is_some() || flipbook.source_b.is_some())
            {
                bake_two_material_flipbook(project, flipbook, project_root, stack).map(Some)
            } else {
                match material.texture_mode {
                    MaterialTextureMode::SimpleImage => match material.psxt_path.as_deref() {
                        Some(path) if !path.trim().is_empty() => {
                            load_material_psxt(&resource.name, path, project_root)
                                .map(|bytes| Some((path.to_string(), bytes)))
                        }
                        _ => Ok(None),
                    },
                    MaterialTextureMode::Generated => Ok(Some((
                        format!("@material-generated:{:?}", material.generated),
                        generate_material_texture_psxt(material.generated),
                    ))),
                    MaterialTextureMode::Transition => {
                        bake_transition_material(project, material.transition, project_root, stack)
                            .map(Some)
                    }
                    // Room/model rendering replaces this with the room-specific probe
                    // where that context exists. Keep resolving the authored image as
                    // a fallback for generic texture consumers and for the base room
                    // material path, matching the pre-transition behaviour.
                    MaterialTextureMode::ReflectiveProbe => match material.psxt_path.as_deref() {
                        Some(path) if !path.trim().is_empty() => {
                            load_material_psxt(&resource.name, path, project_root)
                                .map(|bytes| Some((path.to_string(), bytes)))
                        }
                        _ => Ok(None),
                    },
                }
            }
        }
        _ => Err(format!(
            "'{}' is not a texture-bearing resource",
            resource.name
        )),
    };
    stack.pop();
    result
}

/// Bake an edited transition recipe without first mutating its owning project
/// resource. Material Lab uses this for immediate, accurate preview.
pub fn generate_transition_material_texture_psxt(
    project: &ProjectDocument,
    recipe: TransitionMaterialTexture,
    project_root: &Path,
    owner: Option<ResourceId>,
) -> Result<Vec<u8>, String> {
    let mut stack = owner.into_iter().collect::<Vec<_>>();
    bake_transition_material(project, recipe, project_root, &mut stack).map(|(_, bytes)| bytes)
}

/// Bake an edited two-material cycle without first mutating its owning
/// project resource. Material Lab uses this for immediate, accurate preview.
pub fn generate_two_material_flipbook_psxt(
    project: &ProjectDocument,
    recipe: crate::MaterialFlipbook,
    project_root: &Path,
    owner: Option<ResourceId>,
) -> Result<Vec<u8>, String> {
    let mut stack = owner.into_iter().collect::<Vec<_>>();
    bake_two_material_flipbook(project, recipe.normalized(), project_root, &mut stack)
        .map(|(_, bytes)| bytes)
}

fn bake_two_material_flipbook(
    project: &ProjectDocument,
    recipe: crate::MaterialFlipbook,
    project_root: &Path,
    stack: &mut Vec<ResourceId>,
) -> Result<(String, Vec<u8>), String> {
    let source_a =
        resolve_flipbook_source(project, recipe.source_a, "Material A", project_root, stack)?;
    let source_b =
        resolve_flipbook_source(project, recipe.source_b, "Material B", project_root, stack)?;
    let bytes = compose_two_material_flipbook_psxt(&source_a.bytes, &source_b.bytes)?;
    let key = format!("@material-cycle:a={}:b={}", source_a.key, source_b.key);
    Ok((key, bytes))
}

struct FlipbookSource {
    key: String,
    bytes: Vec<u8>,
}

fn resolve_flipbook_source(
    project: &ProjectDocument,
    source: Option<ResourceId>,
    label: &str,
    project_root: &Path,
    stack: &mut Vec<ResourceId>,
) -> Result<FlipbookSource, String> {
    let source = source.ok_or_else(|| format!("cycle {label} is not assigned"))?;
    let resource = project.resource(source).ok_or_else(|| {
        format!(
            "cycle {label} references missing material #{}",
            source.raw()
        )
    })?;
    if !matches!(
        &resource.data,
        ResourceData::Material(_) | ResourceData::Texture { .. }
    ) {
        return Err(format!(
            "cycle {label} '{}' is not a Material or Texture",
            resource.name
        ));
    }
    let Some((key, bytes)) =
        resolve_material_texture_psxt_inner(project, source, project_root, stack)?
    else {
        return Err(format!(
            "cycle {label} '{}' has no static image",
            resource.name
        ));
    };
    Ok(FlipbookSource { key, bytes })
}

fn bake_transition_material(
    project: &ProjectDocument,
    recipe: TransitionMaterialTexture,
    project_root: &Path,
    stack: &mut Vec<ResourceId>,
) -> Result<(String, Vec<u8>), String> {
    let source_a =
        resolve_transition_source(project, recipe.source_a, "Source A", project_root, stack)?;
    let source_b =
        resolve_transition_source(project, recipe.source_b, "Source B", project_root, stack)?;
    let bytes = compose_transition_psxt(
        &source_a.bytes,
        source_a.tint,
        &source_b.bytes,
        source_b.tint,
        recipe,
    )?;
    let key = format!(
        "@material-transition:{recipe:?}:a={}:{:?}:b={}:{:?}",
        source_a.key, source_a.tint, source_b.key, source_b.tint
    );
    Ok((key, bytes))
}

struct TransitionSource {
    key: String,
    bytes: Vec<u8>,
    tint: [u8; 3],
}

fn resolve_transition_source(
    project: &ProjectDocument,
    source: Option<ResourceId>,
    label: &str,
    project_root: &Path,
    stack: &mut Vec<ResourceId>,
) -> Result<TransitionSource, String> {
    let source = source.ok_or_else(|| format!("transition {label} is not assigned"))?;
    let resource = project.resource(source).ok_or_else(|| {
        format!(
            "transition {label} references missing material #{}",
            source.raw()
        )
    })?;
    let tint = match &resource.data {
        ResourceData::Material(material) => material.tint,
        ResourceData::Texture { .. } => [MATERIAL_NEUTRAL_TINT; 3],
        _ => {
            return Err(format!(
                "transition {label} '{}' is not a Material or Texture",
                resource.name
            ))
        }
    };
    let Some((key, bytes)) =
        resolve_material_texture_psxt_inner(project, source, project_root, stack)?
    else {
        return Err(format!(
            "transition {label} '{}' has no static image",
            resource.name
        ));
    };
    Ok(TransitionSource { key, bytes, tint })
}

fn load_material_psxt(label: &str, stored: &str, project_root: &Path) -> Result<Vec<u8>, String> {
    let path = if Path::new(stored).is_absolute() {
        Path::new(stored).to_path_buf()
    } else {
        project_root.join(stored)
    };
    std::fs::read(&path).map_err(|error| {
        format!(
            "failed to read texture '{label}' at {}: {error}",
            path.display()
        )
    })
}

/// Combine two single-CLUT 4bpp textures using a crisp coverage mask and
/// jointly quantise the result into one standard 4bpp PSXT.
pub fn compose_transition_psxt(
    source_a_bytes: &[u8],
    source_a_tint: [u8; 3],
    source_b_bytes: &[u8],
    source_b_tint: [u8; 3],
    recipe: TransitionMaterialTexture,
) -> Result<Vec<u8>, String> {
    let source_a = psx_asset::Texture::from_bytes(source_a_bytes)
        .map_err(|error| format!("transition Source A is not a valid PSXT: {error:?}"))?;
    let source_b = psx_asset::Texture::from_bytes(source_b_bytes)
        .map_err(|error| format!("transition Source B is not a valid PSXT: {error:?}"))?;
    validate_fusion_texture("transition Source A", source_a)?;
    validate_fusion_texture("transition Source B", source_b)?;

    let size = normalize_generated_texture_size(recipe.size);
    let mut rgba = Vec::with_capacity(usize::from(size) * usize::from(size));
    for y in 0..size {
        for x in 0..size {
            let use_b = transition_uses_source_b(x, y, size, recipe);
            let (texture, tint) = if use_b {
                (source_b, source_b_tint)
            } else {
                (source_a, source_a_tint)
            };
            let sample_x = x % texture.width();
            let sample_y = y % texture.height();
            let index = texture_4bpp_index(texture, sample_x, sample_y);
            let transparent = texture.index_zero_transparent() && index == 0;
            let rgb = tinted_clut_rgb(texture, index, tint);
            rgba.push([rgb[0], rgb[1], rgb[2], if transparent { 0 } else { 255 }]);
        }
    }

    let has_transparency = rgba.iter().any(|pixel| pixel[3] == 0);
    let (palette, indices) = if has_transparency {
        psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
            .map_err(|error| format!("could not quantise transition material: {error}"))?
    } else {
        let rgb = rgba
            .iter()
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect::<Vec<_>>();
        psxed_tex::quantize_rgb(&rgb, 16)
            .map_err(|error| format!("could not quantise transition material: {error}"))?
    };
    psxed_tex::encode_indexed_psxt(
        size,
        size,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        has_transparency,
    )
    .map_err(|error| format!("could not encode transition material: {error}"))
}

/// Pack two equally-sized single-CLUT 4bpp material images into one horizontal
/// runtime atlas and jointly quantise them to one shared 16-colour CLUT.
///
/// The separate materials remain the authoring model. Only the cooker and PS1
/// runtime see this atlas, avoiding a second texture upload or draw pass while
/// preserving identical UVs for both frames. Palette correspondence is learned
/// from same-position texels before the joint quantisation. Texels whose source
/// indices form a confident, visually compatible correspondence are then locked
/// to frame A's exact output index, so independently quantised source palettes
/// cannot make an unchanged casing or background shimmer between frames.
pub fn compose_two_material_flipbook_psxt(
    source_a_bytes: &[u8],
    source_b_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let source_a = psx_asset::Texture::from_bytes(source_a_bytes)
        .map_err(|error| format!("cycle Material A is not a valid PSXT: {error:?}"))?;
    let source_b = psx_asset::Texture::from_bytes(source_b_bytes)
        .map_err(|error| format!("cycle Material B is not a valid PSXT: {error:?}"))?;
    validate_fusion_texture("cycle Material A", source_a)?;
    validate_fusion_texture("cycle Material B", source_b)?;
    if source_a.width() != source_b.width() || source_a.height() != source_b.height() {
        return Err(format!(
            "cycled materials must have matching texture dimensions to preserve UVs; found {}x{} and {}x{}",
            source_a.width(),
            source_a.height(),
            source_b.width(),
            source_b.height(),
        ));
    }
    let atlas_width = source_a
        .width()
        .checked_mul(2)
        .ok_or_else(|| "cycled material width overflows the runtime texture window".to_string())?;
    if atlas_width > 128 || source_a.height() > 128 {
        return Err(format!(
            "two-frame material cook needs each frame to fit a horizontal 128-texel atlas; found {}x{} frames",
            source_a.width(),
            source_a.height(),
        ));
    }

    let atlas_height = source_a.height();
    let frame_texels = usize::from(source_a.width()) * usize::from(atlas_height);
    let mut frame_a_rgba = Vec::with_capacity(frame_texels);
    let mut frame_b_rgba = Vec::with_capacity(frame_texels);
    let mut frame_a_indices = Vec::with_capacity(frame_texels);
    let mut frame_b_indices = Vec::with_capacity(frame_texels);
    for y in 0..atlas_height {
        for x in 0..source_a.width() {
            let index_a = texture_4bpp_index(source_a, x, y);
            let index_b = texture_4bpp_index(source_b, x, y);
            frame_a_indices.push(index_a);
            frame_b_indices.push(index_b);
            frame_a_rgba.push(cycle_source_rgba(source_a, index_a));
            frame_b_rgba.push(cycle_source_rgba(source_b, index_b));
        }
    }

    let stable_texels = stable_cycle_texels(
        source_a,
        source_b,
        &frame_a_indices,
        &frame_b_indices,
        &frame_a_rgba,
        &frame_b_rgba,
    );
    for (offset, stable) in stable_texels.iter().copied().enumerate() {
        if stable {
            frame_b_rgba[offset] = frame_a_rgba[offset];
        }
    }
    let frame_width = usize::from(source_a.width());
    let atlas_width_usize = usize::from(atlas_width);
    let mut rgba = Vec::with_capacity(frame_texels * 2);
    for y in 0..usize::from(atlas_height) {
        let row_start = y * frame_width;
        let row_end = row_start + frame_width;
        rgba.extend_from_slice(&frame_a_rgba[row_start..row_end]);
        rgba.extend_from_slice(&frame_b_rgba[row_start..row_end]);
    }

    let has_transparency = rgba.iter().any(|pixel| pixel[3] == 0);
    let (palette, mut indices) = if has_transparency {
        psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
            .map_err(|error| format!("could not quantise cycled materials: {error}"))?
    } else {
        let rgb = rgba
            .iter()
            .map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect::<Vec<_>>();
        psxed_tex::quantize_rgb(&rgb, 16)
            .map_err(|error| format!("could not quantise cycled materials: {error}"))?
    };
    for y in 0..usize::from(atlas_height) {
        for x in 0..frame_width {
            let source_offset = y * frame_width + x;
            if stable_texels[source_offset] {
                let atlas_a = y * atlas_width_usize + x;
                let atlas_b = atlas_a + frame_width;
                indices[atlas_b] = indices[atlas_a];
            }
        }
    }
    psxed_tex::encode_indexed_psxt(
        atlas_width,
        atlas_height,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        has_transparency,
    )
    .map_err(|error| format!("could not encode cycled materials: {error}"))
}

fn cycle_source_rgba(texture: psx_asset::Texture<'_>, index: u8) -> [u8; 4] {
    let transparent = texture.index_zero_transparent() && index == 0;
    let rgb = tinted_clut_rgb(texture, index, [MATERIAL_NEUTRAL_TINT; 3]);
    [rgb[0], rgb[1], rgb[2], if transparent { 0 } else { 255 }]
}

/// Infer which texels describe the same authored surface despite the two input
/// PSXTs having been quantised separately. For every palette entry in B, the
/// dominant same-position entry in A is its candidate correspondence. Requiring
/// both a clear majority and nearby RGB values prevents real animation changes
/// from being mistaken for palette noise.
fn stable_cycle_texels(
    source_a: psx_asset::Texture<'_>,
    source_b: psx_asset::Texture<'_>,
    indices_a: &[u8],
    indices_b: &[u8],
    rgba_a: &[[u8; 4]],
    rgba_b: &[[u8; 4]],
) -> Vec<bool> {
    debug_assert_eq!(indices_a.len(), indices_b.len());
    debug_assert_eq!(indices_a.len(), rgba_a.len());
    debug_assert_eq!(indices_a.len(), rgba_b.len());

    let mut co_occurrence = [[0u32; 16]; 16];
    let mut b_totals = [0u32; 16];
    for (&index_a, &index_b) in indices_a.iter().zip(indices_b) {
        co_occurrence[usize::from(index_b)][usize::from(index_a)] += 1;
        b_totals[usize::from(index_b)] += 1;
    }

    let mut correspondence = [None; 16];
    for index_b in 0..16 {
        let (index_a, matches) = co_occurrence[index_b]
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|&(index_a, count)| (count, core::cmp::Reverse(index_a)))
            .unwrap_or((0, 0));
        let total = b_totals[index_b];
        if total == 0 || matches.saturating_mul(2) < total {
            continue;
        }

        let rgb_a = tinted_clut_rgb(source_a, index_a as u8, [MATERIAL_NEUTRAL_TINT; 3]);
        let rgb_b = tinted_clut_rgb(source_b, index_b as u8, [MATERIAL_NEUTRAL_TINT; 3]);
        if rgb_max_channel_delta(rgb_a, rgb_b) <= 64 {
            correspondence[index_b] = Some(index_a as u8);
        }
    }

    indices_a
        .iter()
        .zip(indices_b)
        .zip(rgba_a.iter().zip(rgba_b))
        .map(|((&index_a, &index_b), (pixel_a, pixel_b))| {
            pixel_a[3] == pixel_b[3] && correspondence[usize::from(index_b)] == Some(index_a)
        })
        .collect()
}

fn rgb_max_channel_delta(a: [u8; 3], b: [u8; 3]) -> u8 {
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

fn transition_uses_source_b(x: u16, y: u16, size: u16, recipe: TransitionMaterialTexture) -> bool {
    if recipe.coverage == 0 {
        return false;
    }
    if recipe.coverage == u8::MAX {
        return true;
    }
    let last = size.saturating_sub(1).max(1);
    let mut u = u32::from(x) * 255 / u32::from(last);
    let mut v = u32::from(y) * 255 / u32::from(last);
    match recipe.rotation_quarters & 3 {
        1 => (u, v) = (255 - v, u),
        2 => (u, v) = (255 - u, 255 - v),
        3 => (u, v) = (v, 255 - u),
        _ => {}
    }
    if recipe.flip_x {
        u = 255 - u;
    }
    if recipe.flip_y {
        v = 255 - v;
    }
    let base = match recipe.shape {
        TransitionMaskShape::Straight => u,
        TransitionMaskShape::Diagonal => (u + v) / 2,
        TransitionMaskShape::Corner => u.max(v),
        TransitionMaskShape::Island => {
            let du = u.abs_diff(128);
            let dv = v.abs_diff(128);
            du.max(dv).saturating_mul(2).min(255)
        }
        TransitionMaskShape::Connected => connected_transition_base(u, v, recipe.connected_edges),
    } as i32;
    let breakup = i32::from(recipe.edge_breakup.min(96));
    // Connected Paint masks meet at opposite texture borders. Make their
    // detail noise periodic so u=0/u=255 and v=0/v=255 sample identically;
    // recipes can then keep organic exposed edges without opening hairline
    // seams between adjacent painted tiles.
    let (noise_u, noise_v) = if recipe.shape == TransitionMaskShape::Connected {
        (u % 255, v % 255)
    } else {
        (u, v)
    };
    let random = transition_hash(recipe.seed, noise_u, noise_v) as i32 & 0xff;
    let jitter = (random - 128) * breakup / 128;
    let threshold = (base + jitter).clamp(1, 254);
    i32::from(recipe.coverage) >= threshold
}

fn connected_transition_base(u: u32, v: u32, connected_edges: u8) -> u32 {
    const NORTH: u8 = 1 << 0;
    const EAST: u8 = 1 << 1;
    const SOUTH: u8 = 1 << 2;
    const WEST: u8 = 1 << 3;

    let closed_edge_distances = [(NORTH, v), (EAST, 255 - u), (SOUTH, 255 - v), (WEST, u)]
        .into_iter()
        .filter_map(|(edge, distance)| (connected_edges & edge == 0).then_some(distance));
    let Some(nearest_closed_edge) = closed_edge_distances.min() else {
        // Surrounded by the same painted material: the entire tile is B.
        return 0;
    };

    // Closed edges retain Source A for roughly the outer quarter of the
    // tile, then transition into B. Connected edges are omitted from the
    // distance field, allowing B to flow across them without a seam.
    255u32.saturating_sub(nearest_closed_edge.saturating_mul(2).min(255))
}

fn transition_hash(seed: u32, x: u32, y: u32) -> u32 {
    let mut value = seed ^ x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

fn generate_noise_indices(
    size: u16,
    settings: ProceduralNoiseTexture,
    uv: GeneratedTextureUv,
) -> Vec<u8> {
    let size = normalize_generated_texture_size(size);
    let size_usize = usize::from(size);
    let feature_size = settings.feature_size.clamp(2, 64) as u32;
    let octaves = settings.octaves.clamp(1, 5);
    let contrast = settings.contrast.max(1) as i32;
    let mut pixels = vec![0u8; size_usize * size_usize];

    for y in 0..u32::from(size) {
        for x in 0..u32::from(size) {
            let (sample_x, sample_y, scale_u_q8, scale_v_q8) =
                transformed_noise_uv(i64::from(x), i64::from(y), size, uv);
            let mut value = 0u32;
            let mut weight_sum = 0u32;
            let mut weight = 256u32;
            for octave in 0..octaves {
                let cells_u =
                    periodic_noise_cell_count(u32::from(size), feature_size, scale_u_q8, octave);
                let cells_v =
                    periodic_noise_cell_count(u32::from(size), feature_size, scale_v_q8, octave);
                value = value.saturating_add(
                    periodic_value_noise_2d(
                        settings.seed ^ u32::from(octave).wrapping_mul(0x9e37_79b9),
                        sample_x,
                        sample_y,
                        u32::from(size),
                        cells_u,
                        cells_v,
                    )
                    .saturating_mul(weight),
                );
                weight_sum = weight_sum.saturating_add(weight);
                weight = (weight / 2).max(1);
            }
            let normalized = (value / weight_sum.max(1)) as i32;
            let contrasted = 128 + ((normalized - 128) * contrast / 128);
            let index = ((contrasted.clamp(0, 255) * 15 + 127) / 255) as u8;
            pixels[y as usize * size_usize + x as usize] = index;
        }
    }
    pixels
}

fn transformed_noise_uv(x: i64, y: i64, size: u16, uv: GeneratedTextureUv) -> (i64, i64, u32, u32) {
    let mut scale_u = u32::from(uv.scale_u_q8.clamp(16, 2048));
    let mut scale_v = u32::from(uv.scale_v_q8.clamp(16, 2048));
    let mut u = x + i64::from(uv.offset_u);
    let mut v = y + i64::from(uv.offset_v);
    let limit = i64::from(size);
    match uv.rotation_quarters & 3 {
        1 => {
            (u, v) = (limit - 1 - v, u);
            (scale_u, scale_v) = (scale_v, scale_u);
        }
        2 => (u, v) = (limit - 1 - u, limit - 1 - v),
        3 => {
            (u, v) = (v, limit - 1 - u);
            (scale_u, scale_v) = (scale_v, scale_u);
        }
        _ => {}
    }
    (u, v, scale_u, scale_v)
}

fn periodic_noise_cell_count(size: u32, feature_size: u32, scale_q8: u32, octave: u8) -> u32 {
    let broad_cells = ((size + feature_size / 2) / feature_size).max(1);
    let scaled_cells = ((broad_cells.saturating_mul(scale_q8) + 128) / 256).max(1);
    scaled_cells
        .checked_shl(u32::from(octave))
        .unwrap_or(u32::MAX)
        .min(size)
}

/// Collapse an `Average` primary pass followed by an `AddQuarter` secondary
/// pass into one 4bpp texture for an `Average` draw.
///
/// The PS1 blend equation is preserved for pixels covered by both layers:
/// `background / 2 + primary / 2 + secondary / 4` becomes
/// `background / 2 + (primary + secondary / 2) / 2`. The authored tints are
/// baked into the fused palette, so the runtime material must use neutral
/// `[128, 128, 128]` modulation afterwards.
pub fn fuse_average_add_quarter_psxt(
    primary_bytes: &[u8],
    primary_tint: [u8; 3],
    secondary_bytes: &[u8],
    secondary_tint: [u8; 3],
) -> Result<Vec<u8>, String> {
    let primary = psx_asset::Texture::from_bytes(primary_bytes)
        .map_err(|error| format!("primary texture is not a valid PSXT: {error:?}"))?;
    let secondary = psx_asset::Texture::from_bytes(secondary_bytes)
        .map_err(|error| format!("secondary texture is not a valid PSXT: {error:?}"))?;
    validate_fusion_texture("primary", primary)?;
    validate_fusion_texture("secondary", secondary)?;

    let width = primary.width();
    let height = primary.height();
    let mut rgba = Vec::with_capacity(usize::from(width) * usize::from(height));
    for y in 0..height {
        for x in 0..width {
            let primary_index = texture_4bpp_index(primary, x, y);
            let secondary_index =
                texture_4bpp_index(secondary, x % secondary.width(), y % secondary.height());
            let primary_visible = !(primary.index_zero_transparent() && primary_index == 0);
            let secondary_visible = !(secondary.index_zero_transparent() && secondary_index == 0);
            if !primary_visible && !secondary_visible {
                rgba.push([0, 0, 0, 0]);
                continue;
            }

            let primary_rgb = if primary_visible {
                tinted_clut_rgb(primary, primary_index, primary_tint)
            } else {
                [0; 3]
            };
            let secondary_rgb = if secondary_visible {
                tinted_clut_rgb(secondary, secondary_index, secondary_tint)
            } else {
                [0; 3]
            };
            rgba.push([
                primary_rgb[0].saturating_add(secondary_rgb[0] / 2),
                primary_rgb[1].saturating_add(secondary_rgb[1] / 2),
                primary_rgb[2].saturating_add(secondary_rgb[2] / 2),
                255,
            ]);
        }
    }

    let (palette, indices) = psxed_tex::quantize_rgba_with_transparent_zero(&rgba, 16)
        .map_err(|error| format!("could not quantise fused material: {error}"))?;
    psxed_tex::encode_indexed_psxt(
        width,
        height,
        psxed_tex::PsxtDepth::Bit4,
        &indices,
        &palette,
        true,
    )
    .map_err(|error| format!("could not encode fused material: {error}"))
}

/// Modulation value the runtime should use after
/// [`fuse_average_add_quarter_psxt`] bakes both layer tints.
pub const fn fused_material_neutral_tint() -> [u8; 3] {
    [MATERIAL_NEUTRAL_TINT; 3]
}

fn validate_fusion_texture(label: &str, texture: psx_asset::Texture<'_>) -> Result<(), String> {
    if texture.depth() != psxed_format::texture::Depth::Bit4 || texture.clut_entries() != 16 {
        return Err(format!("{label} texture must be a single-CLUT 4bpp PSXT"));
    }
    if texture.width() == 0 || texture.height() == 0 {
        return Err(format!("{label} texture has zero dimensions"));
    }
    Ok(())
}

fn texture_4bpp_index(texture: psx_asset::Texture<'_>, x: u16, y: u16) -> u8 {
    let halfword = usize::from(y) * usize::from(texture.halfwords_per_row()) + usize::from(x / 4);
    let offset = halfword * 2;
    let packed = u16::from_le_bytes([
        texture.pixel_bytes()[offset],
        texture.pixel_bytes()[offset + 1],
    ]);
    ((packed >> ((x & 3) * 4)) & 0x0f) as u8
}

fn tinted_clut_rgb(texture: psx_asset::Texture<'_>, index: u8, tint: [u8; 3]) -> [u8; 3] {
    let offset = usize::from(index) * 2;
    let raw = u16::from_le_bytes([
        texture.clut_bytes()[offset],
        texture.clut_bytes()[offset + 1],
    ]);
    let rgb = [
        expand_5bit((raw & 0x1f) as u8),
        expand_5bit(((raw >> 5) & 0x1f) as u8),
        expand_5bit(((raw >> 10) & 0x1f) as u8),
    ];
    [
        modulate(rgb[0], tint[0]),
        modulate(rgb[1], tint[1]),
        modulate(rgb[2], tint[2]),
    ]
}

const fn expand_5bit(value: u8) -> u8 {
    (value << 3) | (value >> 2)
}

fn modulate(value: u8, tint: u8) -> u8 {
    let product = value as u16 * tint as u16;
    ((product + 64) / 128).min(255) as u8
}

fn periodic_value_noise_2d(
    seed: u32,
    x: i64,
    y: i64,
    domain_size: u32,
    cells_u: u32,
    cells_v: u32,
) -> u32 {
    let domain = i64::from(domain_size.max(1));
    let cells_u = cells_u.max(1);
    let cells_v = cells_v.max(1);
    let x_q8 = x
        .saturating_mul(i64::from(cells_u))
        .saturating_mul(256)
        .div_euclid(domain);
    let y_q8 = y
        .saturating_mul(i64::from(cells_v))
        .saturating_mul(256)
        .div_euclid(domain);
    let x0 = x_q8.div_euclid(256).rem_euclid(i64::from(cells_u)) as u32;
    let y0 = y_q8.div_euclid(256).rem_euclid(i64::from(cells_v)) as u32;
    let fx = x_q8.rem_euclid(256) as u32;
    let fy = y_q8.rem_euclid(256) as u32;
    let x1 = (x0 + 1) % cells_u;
    let y1 = (y0 + 1) % cells_v;
    let sx = smooth_q8(fx);
    let sy = smooth_q8(fy);
    let top = lerp_q8(hash_noise(seed, x0, y0), hash_noise(seed, x1, y0), sx);
    let bottom = lerp_q8(hash_noise(seed, x0, y1), hash_noise(seed, x1, y1), sx);
    lerp_q8(top, bottom, sy)
}

fn smooth_q8(value: u32) -> u32 {
    // 3t² - 2t³, in Q8.
    let square = value.saturating_mul(value) >> 8;
    square.saturating_mul(768u32.saturating_sub(value.saturating_mul(2))) >> 8
}

fn lerp_q8(a: u32, b: u32, t: u32) -> u32 {
    if b >= a {
        a + (((b - a) * t) >> 8)
    } else {
        a - (((a - b) * t) >> 8)
    }
}

fn hash_noise(seed: u32, x: u32, y: u32) -> u32 {
    let mut value = seed ^ x.wrapping_mul(0x85eb_ca6b) ^ y.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value & 0xff
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_4bpp(rgb: [u8; 3]) -> Vec<u8> {
        solid_4bpp_size(rgb, 8, 8)
    }

    fn solid_4bpp_size(rgb: [u8; 3], width: u16, height: u16) -> Vec<u8> {
        psxed_tex::encode_indexed_psxt(
            width,
            height,
            psxed_tex::PsxtDepth::Bit4,
            &vec![0; usize::from(width) * usize::from(height)],
            &[rgb; 16],
            false,
        )
        .expect("solid test texture encodes")
    }

    fn indexed_4bpp(width: u16, height: u16, indices: &[u8], palette: &[[u8; 3]]) -> Vec<u8> {
        psxed_tex::encode_indexed_psxt(
            width,
            height,
            psxed_tex::PsxtDepth::Bit4,
            indices,
            palette,
            false,
        )
        .expect("indexed test texture encodes")
    }

    fn sampled_rgb(bytes: &[u8], x: u16, y: u16) -> [u8; 3] {
        let texture = psx_asset::Texture::from_bytes(bytes).expect("test PSXT parses");
        let index = texture_4bpp_index(texture, x, y);
        tinted_clut_rgb(texture, index, [128; 3])
    }

    #[test]
    fn transition_coverage_endpoints_are_exact_source_images() {
        let red = solid_4bpp([255, 0, 0]);
        let blue = solid_4bpp([0, 0, 255]);
        let recipe = TransitionMaterialTexture {
            size: 8,
            coverage: 0,
            ..TransitionMaterialTexture::default()
        };
        let all_a = compose_transition_psxt(&red, [128; 3], &blue, [128; 3], recipe)
            .expect("A endpoint bakes");
        let a = sampled_rgb(&all_a, 4, 4);
        assert!(a[0] > 240 && a[2] < 16);

        let all_b = compose_transition_psxt(
            &red,
            [128; 3],
            &blue,
            [128; 3],
            TransitionMaterialTexture {
                coverage: 255,
                ..recipe
            },
        )
        .expect("B endpoint bakes");
        let b = sampled_rgb(&all_b, 4, 4);
        assert!(b[2] > 240 && b[0] < 16);
    }

    #[test]
    fn two_material_cycle_is_jointly_cooked_into_a_hidden_runtime_atlas() {
        let red = solid_4bpp([255, 0, 0]);
        let blue = solid_4bpp([0, 0, 255]);
        let atlas =
            compose_two_material_flipbook_psxt(&red, &blue).expect("matching material images cook");
        let texture = psx_asset::Texture::from_bytes(&atlas).expect("atlas parses");
        assert_eq!((texture.width(), texture.height()), (16, 8));
        let a = sampled_rgb(&atlas, 4, 4);
        let b = sampled_rgb(&atlas, 12, 4);
        assert!(a[0] > 240 && a[2] < 16);
        assert!(b[2] > 240 && b[0] < 16);
    }

    #[test]
    fn two_material_cycle_locks_independently_quantised_stable_texels() {
        let mut indices_a = vec![1; 64];
        let mut indices_b = vec![7; 64];
        indices_a[27] = 2;
        indices_b[27] = 9;

        let mut palette_a = vec![[0, 0, 0]; 16];
        palette_a[1] = [48, 56, 64];
        palette_a[2] = [8, 16, 24];
        let mut palette_b = vec![[0, 0, 0]; 16];
        palette_b[7] = [64, 48, 40];
        palette_b[9] = [0, 224, 255];

        let source_a = indexed_4bpp(8, 8, &indices_a, &palette_a);
        let source_b = indexed_4bpp(8, 8, &indices_b, &palette_b);
        let atlas = compose_two_material_flipbook_psxt(&source_a, &source_b)
            .expect("independently quantised materials cook");
        let texture = psx_asset::Texture::from_bytes(&atlas).expect("atlas parses");

        assert_eq!(
            texture_4bpp_index(texture, 2, 2),
            texture_4bpp_index(texture, 10, 2),
            "unchanged casing must reuse the exact frame-A texel index"
        );
        assert_ne!(
            texture_4bpp_index(texture, 3, 3),
            texture_4bpp_index(texture, 11, 3),
            "the changed monitor texel must remain animated"
        );
    }

    #[test]
    fn two_material_cycle_rejects_uv_incompatible_dimensions() {
        let small = solid_4bpp_size([255, 0, 0], 8, 8);
        let wide = solid_4bpp_size([0, 0, 255], 16, 8);
        let error = compose_two_material_flipbook_psxt(&small, &wide)
            .expect_err("mismatched material images must fail");
        assert!(error.contains("matching texture dimensions"));
        assert!(error.contains("8x8 and 16x8"));
    }

    #[test]
    fn material_resolver_cooks_selected_cycle_sources_without_authored_atlas() {
        let mut project = ProjectDocument::new("cycle-resolve");
        let generated = |color| {
            ResourceData::Material(crate::MaterialResource {
                texture_mode: MaterialTextureMode::Generated,
                generated: GeneratedMaterialTexture {
                    size: 32,
                    base_color: color,
                    noise_enabled: false,
                    ..GeneratedMaterialTexture::default()
                },
                ..crate::MaterialResource::opaque(None)
            })
        };
        let source_a = project.add_resource("Frame A", generated([224, 32, 24]));
        let source_b = project.add_resource("Frame B", generated([24, 64, 224]));
        let mut cycle = crate::MaterialResource::opaque(None);
        cycle.animation.mode = crate::MaterialAnimationMode::Flipbook;
        cycle.animation.flipbook.source_a = Some(source_a);
        cycle.animation.flipbook.source_b = Some(source_b);
        let cycle = project.add_resource("Cycle", ResourceData::Material(cycle));

        let (key, bytes) = resolve_material_texture_psxt(&project, cycle, Path::new("."))
            .expect("cycle resolves")
            .expect("cycle has a cooked image");
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("cooked atlas parses");
        assert_eq!((texture.width(), texture.height()), (64, 32));
        assert!(key.starts_with("@material-cycle:"));
    }

    #[test]
    fn transition_midpoint_is_crisp_deterministic_and_rotatable() {
        let red = solid_4bpp([255, 0, 0]);
        let blue = solid_4bpp([0, 0, 255]);
        let recipe = TransitionMaterialTexture {
            size: 32,
            coverage: 128,
            edge_breakup: 0,
            ..TransitionMaterialTexture::default()
        };
        let horizontal = compose_transition_psxt(&red, [128; 3], &blue, [128; 3], recipe)
            .expect("midpoint bakes");
        assert_eq!(
            horizontal,
            compose_transition_psxt(&red, [128; 3], &blue, [128; 3], recipe)
                .expect("midpoint rebakes")
        );
        let left = sampled_rgb(&horizontal, 2, 16);
        let right = sampled_rgb(&horizontal, 29, 16);
        assert!(left[2] > left[0], "Source B should cover the low-mask side");
        assert!(
            right[0] > right[2],
            "Source A should remain beyond the edge"
        );

        let vertical = compose_transition_psxt(
            &red,
            [128; 3],
            &blue,
            [128; 3],
            TransitionMaterialTexture {
                rotation_quarters: 1,
                ..recipe
            },
        )
        .expect("rotated midpoint bakes");
        assert_ne!(horizontal, vertical);
        assert_ne!(
            sampled_rgb(&vertical, 16, 2),
            sampled_rgb(&vertical, 16, 29)
        );
    }

    #[test]
    fn connected_transition_opens_only_shared_painted_edges() {
        let east = TransitionMaterialTexture {
            size: 64,
            coverage: 128,
            shape: TransitionMaskShape::Connected,
            edge_breakup: 0,
            connected_edges: 1 << 1,
            ..TransitionMaterialTexture::default()
        };
        assert!(transition_uses_source_b(63, 32, 64, east));
        assert!(!transition_uses_source_b(0, 32, 64, east));
        assert!(!transition_uses_source_b(32, 0, 64, east));
        assert!(transition_uses_source_b(32, 32, 64, east));

        let horizontal_strip = TransitionMaterialTexture {
            connected_edges: (1 << 1) | (1 << 3),
            ..east
        };
        assert!(transition_uses_source_b(0, 32, 64, horizontal_strip));
        assert!(transition_uses_source_b(63, 32, 64, horizontal_strip));
        assert!(!transition_uses_source_b(32, 0, 64, horizontal_strip));

        let surrounded = TransitionMaterialTexture {
            connected_edges: 0b1111,
            ..east
        };
        assert!(transition_uses_source_b(0, 0, 64, surrounded));
        assert!(transition_uses_source_b(63, 63, 64, surrounded));
    }

    #[test]
    fn connected_transition_edge_detail_is_periodic_across_tile_seams() {
        let east = TransitionMaterialTexture {
            size: 64,
            coverage: 128,
            shape: TransitionMaskShape::Connected,
            edge_breakup: 72,
            seed: 0x1234_5678,
            connected_edges: 1 << 1,
            ..TransitionMaterialTexture::default()
        };
        let west = TransitionMaterialTexture {
            connected_edges: 1 << 3,
            ..east
        };
        for y in 0..64 {
            assert_eq!(
                transition_uses_source_b(63, y, 64, east),
                transition_uses_source_b(0, y, 64, west),
                "connected seam disagreed at row {y}"
            );
        }
    }

    #[test]
    fn transition_material_resolves_sources_and_rejects_cycles() {
        let mut project = ProjectDocument::new("transition-resolve");
        let source_a = project.add_resource(
            "Sand",
            ResourceData::Material(crate::MaterialResource {
                texture_mode: MaterialTextureMode::Generated,
                generated: GeneratedMaterialTexture {
                    size: 16,
                    base_color: [184, 144, 88],
                    noise_enabled: false,
                    ..GeneratedMaterialTexture::default()
                },
                ..crate::MaterialResource::opaque(None)
            }),
        );
        let source_b = project.add_resource(
            "Stone",
            ResourceData::Material(crate::MaterialResource {
                texture_mode: MaterialTextureMode::Generated,
                generated: GeneratedMaterialTexture {
                    size: 16,
                    base_color: [72, 80, 96],
                    noise_enabled: false,
                    ..GeneratedMaterialTexture::default()
                },
                ..crate::MaterialResource::opaque(None)
            }),
        );
        let transition = project.add_resource(
            "Sand over stone",
            ResourceData::Material(crate::MaterialResource {
                texture_mode: MaterialTextureMode::Transition,
                transition: TransitionMaterialTexture {
                    source_a: Some(source_a),
                    source_b: Some(source_b),
                    ..TransitionMaterialTexture::default()
                },
                ..crate::MaterialResource::opaque(None)
            }),
        );

        let (_, bytes) = resolve_material_texture_psxt(&project, transition, Path::new("."))
            .expect("transition resolves")
            .expect("transition has bytes");
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("transition PSXT parses");
        assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit4);
        assert_eq!(texture.clut_entries(), 16);
        assert_eq!((texture.width(), texture.height()), (64, 64));

        let ResourceData::Material(material) =
            &mut project.resource_mut(source_a).expect("source exists").data
        else {
            unreachable!()
        };
        material.texture_mode = MaterialTextureMode::Transition;
        material.transition.source_a = Some(transition);
        material.transition.source_b = Some(source_b);
        let error = resolve_material_texture_psxt(&project, transition, Path::new("."))
            .expect_err("cycle rejected");
        assert!(error.contains("cycle"));
    }

    #[test]
    fn room_reflection_probe_is_deterministic_opaque_4bpp() {
        let project = ProjectDocument::new("Probe test");
        let mut grid = WorldGrid::stone_room(3, 2, 1024, None, None);
        grid.ambient_color = [32, 48, 64];
        grid.fog_color = [96, 80, 72];

        let first = generate_room_reflection_probe_psxt(&project, &grid, Path::new("."))
            .expect("room probe bakes");
        let second = generate_room_reflection_probe_psxt(&project, &grid, Path::new("."))
            .expect("room probe rebakes");
        assert_eq!(first, second);

        let texture = psx_asset::Texture::from_bytes(&first).expect("room probe PSXT parses");
        assert_eq!(texture.width(), ROOM_REFLECTION_PROBE_SIZE);
        assert_eq!(texture.height(), ROOM_REFLECTION_PROBE_SIZE);
        assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit4);
        assert_eq!(texture.clut_entries(), 16);
        assert!(!texture.index_zero_transparent());

        grid.fog_color[0] = grid.fog_color[0].saturating_add(40);
        let changed = generate_room_reflection_probe_psxt(&project, &grid, Path::new("."))
            .expect("changed room probe bakes");
        assert_ne!(first, changed, "room lighting must affect the baked probe");
    }

    #[test]
    fn generated_noise_is_deterministic_and_uses_full_4bpp_range() {
        let settings = ProceduralNoiseTexture::default();
        let first = generate_model_noise_indices(settings);
        let second = generate_model_noise_indices(settings);
        assert_eq!(first, second);
        assert!(first.contains(&0));
        assert!(first.iter().any(|&index| index >= 14));
        assert!(first.iter().all(|&index| index < 16));
    }

    #[test]
    fn value_noise_domain_wraps_exactly_on_both_axes() {
        let size = i64::from(MODEL_NOISE_TEXTURE_SIZE);
        for &(x, y) in &[(-19, 7), (0, 0), (31, 93), (127, -41)] {
            let sample = periodic_value_noise_2d(0x1234_5678, x, y, size as u32, 11, 7);
            assert_eq!(
                sample,
                periodic_value_noise_2d(0x1234_5678, x + size, y, size as u32, 11, 7)
            );
            assert_eq!(
                sample,
                periodic_value_noise_2d(0x1234_5678, x, y + size, size as u32, 11, 7)
            );
        }
    }

    #[test]
    fn generated_noise_has_no_harder_wrap_than_its_internal_edges() {
        let pixels = generate_model_noise_indices(ProceduralNoiseTexture::default());
        let size = usize::from(MODEL_NOISE_TEXTURE_SIZE);
        let mut max_internal_delta = 0u8;
        let mut max_wrap_delta = 0u8;
        for y in 0..size {
            for x in 1..size {
                max_internal_delta =
                    max_internal_delta.max(pixels[y * size + x].abs_diff(pixels[y * size + x - 1]));
            }
            max_wrap_delta =
                max_wrap_delta.max(pixels[y * size].abs_diff(pixels[y * size + size - 1]));
        }
        for x in 0..size {
            for y in 1..size {
                max_internal_delta = max_internal_delta
                    .max(pixels[y * size + x].abs_diff(pixels[(y - 1) * size + x]));
            }
            max_wrap_delta = max_wrap_delta.max(pixels[x].abs_diff(pixels[(size - 1) * size + x]));
        }
        assert!(
            max_wrap_delta <= max_internal_delta,
            "wrap delta {max_wrap_delta} exceeds internal delta {max_internal_delta}"
        );
    }

    #[test]
    fn seed_changes_generated_texture() {
        let first = generate_model_noise_indices(ProceduralNoiseTexture::default());
        let second = generate_model_noise_indices(ProceduralNoiseTexture {
            seed: 2,
            ..ProceduralNoiseTexture::default()
        });
        assert_ne!(first, second);
    }

    #[test]
    fn generated_noise_psxt_is_4bpp_clut16() {
        let bytes = generate_model_noise_psxt(ProceduralNoiseTexture::default());
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("generated PSXT parses");
        assert_eq!(texture.width(), MODEL_NOISE_TEXTURE_SIZE);
        assert_eq!(texture.height(), MODEL_NOISE_TEXTURE_SIZE);
        assert_eq!(texture.clut_entries(), 16);
        assert!(texture.index_zero_transparent());
    }

    #[test]
    fn material_lab_recipe_bakes_requested_opaque_4bpp_size() {
        let settings = GeneratedMaterialTexture {
            size: 16,
            base_color: [12, 34, 56],
            noise_color: [210, 220, 230],
            noise_uv: GeneratedTextureUv {
                scale_u_q8: 512,
                offset_v: 7,
                rotation_quarters: 1,
                ..GeneratedTextureUv::default()
            },
            ..GeneratedMaterialTexture::default()
        };
        let bytes = generate_material_texture_psxt(settings);
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("generated PSXT parses");
        assert_eq!((texture.width(), texture.height()), (16, 16));
        assert_eq!(texture.clut_entries(), 16);
        assert!(!texture.index_zero_transparent());
        assert_eq!(bytes, generate_material_texture_psxt(settings));
        assert_ne!(
            bytes,
            generate_material_texture_psxt(GeneratedMaterialTexture {
                noise_uv: GeneratedTextureUv::default(),
                ..settings
            })
        );
    }

    #[test]
    fn material_lab_recipe_supports_a_128_square_texture_window() {
        let bytes = generate_material_texture_psxt(GeneratedMaterialTexture {
            size: 128,
            ..GeneratedMaterialTexture::default()
        });
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("generated PSXT parses");
        assert_eq!((texture.width(), texture.height()), (128, 128));
        assert_eq!(texture.pixel_bytes().len(), 128 * 128 / 2);
    }

    #[test]
    fn disabled_generated_noise_bakes_a_flat_base_independent_of_noise_recipe() {
        let settings = GeneratedMaterialTexture {
            size: 16,
            base_color: [24, 48, 72],
            noise_enabled: false,
            noise_color: [255, 0, 255],
            ..GeneratedMaterialTexture::default()
        };
        let first = generate_material_texture_psxt(settings);
        let second = generate_material_texture_psxt(GeneratedMaterialTexture {
            noise_color: [0, 255, 0],
            noise: ProceduralNoiseTexture {
                seed: 99,
                feature_size: 2,
                octaves: 5,
                contrast: 255,
            },
            ..settings
        });
        assert_eq!(first, second);
        let texture = psx_asset::Texture::from_bytes(&first).expect("flat PSXT parses");
        for y in 0..texture.height() {
            for x in 0..texture.width() {
                assert_eq!(texture_4bpp_index(texture, x, y), 0);
            }
        }
    }

    #[test]
    fn compatible_layers_fuse_to_repeating_4bpp_texture() {
        let primary = psxed_tex::encode_indexed_psxt(
            4,
            2,
            psxed_tex::PsxtDepth::Bit4,
            &[0, 1, 1, 1, 1, 1, 1, 1],
            &[[0, 0, 0], [128, 64, 32]],
            true,
        )
        .unwrap();
        let secondary = psxed_tex::encode_indexed_psxt(
            2,
            1,
            psxed_tex::PsxtDepth::Bit4,
            &[0, 1],
            &[[0, 0, 0], [64, 128, 192]],
            true,
        )
        .unwrap();

        let fused =
            fuse_average_add_quarter_psxt(&primary, [128, 128, 128], &secondary, [128, 128, 128])
                .unwrap();
        let texture = psx_asset::Texture::from_bytes(&fused).unwrap();
        assert_eq!((texture.width(), texture.height()), (4, 2));
        assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit4);
        assert_eq!(texture.clut_entries(), 16);
        assert!(texture.index_zero_transparent());
        assert_eq!(texture_4bpp_index(texture, 0, 0), 0);
        assert_ne!(texture_4bpp_index(texture, 1, 0), 0);
        assert_ne!(texture_4bpp_index(texture, 2, 0), 0);
    }

    #[test]
    fn fusion_rejects_non_4bpp_input() {
        let indexed_8bpp = psxed_tex::encode_indexed_psxt(
            2,
            1,
            psxed_tex::PsxtDepth::Bit8,
            &[1, 1],
            &[[0, 0, 0], [255, 255, 255]],
            true,
        )
        .unwrap();
        let secondary = generate_model_noise_psxt(ProceduralNoiseTexture::default());
        assert!(
            fuse_average_add_quarter_psxt(&indexed_8bpp, [128; 3], &secondary, [128; 3]).is_err()
        );
    }
}
