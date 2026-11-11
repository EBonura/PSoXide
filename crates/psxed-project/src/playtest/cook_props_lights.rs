use super::*;
use crate::prop_surfaces::bake_prop_uv;
use crate::{generate_material_texture_psxt, UiFontChoice};

pub(crate) fn resolve_material_texture_asset(
    project: &ProjectDocument,
    project_root: &Path,
    label: &str,
    material_id: ResourceId,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<(usize, [u8; 3])> {
    let Some(material_resource) = project.resource(material_id) else {
        report.warn(format!(
            "{label} references missing Material #{} - skipped",
            material_id.raw()
        ));
        return None;
    };
    let ResourceData::Material(material) = &material_resource.data else {
        report.warn(format!(
            "{label} references '{}' but it is not a Material - skipped",
            material_resource.name
        ));
        return None;
    };
    let (texture_key, bytes) =
        match material_texture_bytes(project, material_resource, project_root) {
            Ok(Some(source)) => source,
            Ok(None) => {
                report.warn(format!(
                    "{label} material '{}' has no Texture - skipped",
                    material_resource.name
                ));
                return None;
            }
            Err(msg) => {
                report.warn(format!("{label}: {msg} - skipped"));
                return None;
            }
        };
    let texture_asset_index = if let Some(&existing) = texture_asset_for_path.get(&texture_key) {
        existing
    } else {
        if let Err(msg) = expect_room_material_depth(&material_resource.name, &bytes) {
            report.warn(format!("{label}: {msg} - skipped"));
            return None;
        }
        let texture_index = texture_asset_for_path.len();
        let new_index = assets.len();
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: format!("texture_{texture_index:03}.psxt"),
            source_label: material_resource.name.clone(),
            streamed_class: StreamedClass::None,
        });
        texture_asset_for_path.insert(texture_key, new_index);
        new_index
    };
    Some((texture_asset_index, material.tint))
}

/// Resolve a Model Renderer's Material into the cooked override the
/// instance/character record carries. A Material without a `.psxt`
/// keeps the model atlas; a textured Material becomes a covering
/// texture requirement. Blend mode, tint, and sidedness apply in
/// either case.
pub(crate) fn resolve_model_material_override(
    project: &ProjectDocument,
    project_root: &Path,
    label: &str,
    material_id: ResourceId,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<PlaytestModelMaterialOverride> {
    let Some(material_resource) = project.resource(material_id) else {
        report.warn(format!(
            "{label} references missing Material #{} - skipped",
            material_id.raw()
        ));
        return None;
    };
    let ResourceData::Material(material) = &material_resource.data else {
        report.warn(format!(
            "{label} references '{}' but it is not a Material - skipped",
            material_resource.name
        ));
        return None;
    };
    let texture_asset_index = if matches!(
        material.texture_mode,
        crate::MaterialTextureMode::Generated | crate::MaterialTextureMode::Transition
    ) || material
        .psxt_path
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty())
    {
        Some(
            resolve_material_texture_asset(
                project,
                project_root,
                label,
                material_id,
                texture_asset_for_path,
                assets,
                report,
            )?
            .0,
        )
    } else {
        None
    };
    let secondary_layer = material.enabled_secondary_layer().map(|layer| {
        let texture_asset_index = resolve_model_secondary_texture_asset(
            project,
            project_root,
            material_id,
            &material_resource.name,
            layer,
            texture_asset_for_path,
            assets,
            report,
        );
        PlaytestModelSecondaryLayer {
            texture_asset_index,
            blend_mode: layer.blend_mode,
            tint_rgb: layer.tint,
            motion: layer.motion,
            reflection_probe: (layer.texture_mode == crate::MaterialTextureMode::ReflectiveProbe
                || layer.reflection.enabled)
                .then_some(layer.reflection),
        }
    });
    Some(PlaytestModelMaterialOverride {
        texture_asset_index,
        blend_mode: material.blend_mode,
        tint_rgb: material.tint,
        motion: crate::MaterialUvMotion {
            enabled: material.animation.mode == crate::MaterialAnimationMode::UvScroll,
            ..material.animation.uv_scroll
        },
        secondary_layer,
        reflection_probe: (material.texture_mode == crate::MaterialTextureMode::ReflectiveProbe
            || material.reflection.enabled)
            .then_some(material.reflection),
        face_sidedness: material.sidedness(),
    })
}

fn resolve_model_secondary_texture_asset(
    project: &ProjectDocument,
    project_root: &Path,
    owner: ResourceId,
    material_name: &str,
    layer: &crate::ModelSecondaryLayer,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<usize> {
    let (cache_key, source_label, bytes) = match layer.texture_mode {
        crate::MaterialTextureMode::SimpleImage => {
            let path = layer.psxt_path.as_deref().unwrap_or_default().trim();
            if path.is_empty() {
                report.warn(format!(
                    "Material '{material_name}' layer 2 has no texture path - layer skipped"
                ));
                return None;
            }
            if let Some(&existing) = texture_asset_for_path.get(path) {
                return Some(existing);
            }
            let bytes = match load_psxt_bytes(material_name, path, project_root) {
                Ok(bytes) => bytes,
                Err(msg) => {
                    report.warn(format!(
                        "Material '{material_name}' secondary layer: {msg} - skipped"
                    ));
                    return None;
                }
            };
            (
                path.to_string(),
                format!("{material_name} secondary"),
                bytes,
            )
        }
        crate::MaterialTextureMode::Generated => {
            let cache_key = format!("@material-layer-2:{:?}", layer.generated);
            if let Some(&existing) = texture_asset_for_path.get(&cache_key) {
                return Some(existing);
            }
            (
                cache_key,
                format!("{material_name} generated layer 2"),
                generate_material_texture_psxt(layer.generated),
            )
        }
        crate::MaterialTextureMode::Transition => {
            let bytes = match crate::generate_transition_material_texture_psxt(
                project,
                layer.transition,
                project_root,
                Some(owner),
            ) {
                Ok(bytes) => bytes,
                Err(error) => {
                    report.warn(format!(
                        "Material '{material_name}' secondary transition: {error} - skipped"
                    ));
                    return None;
                }
            };
            let checksum = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
            });
            (
                format!("@material-layer-2-transition:{checksum:016x}"),
                format!("{material_name} transition layer 2"),
                bytes,
            )
        }
        crate::MaterialTextureMode::ReflectiveProbe => return None,
    };
    if let Err(msg) = expect_room_material_depth(&source_label, &bytes) {
        report.warn(format!(
            "Material '{material_name}' secondary layer: {msg} - skipped"
        ));
        return None;
    }
    let texture_index = texture_asset_for_path.len();
    let asset_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: format!("texture_{texture_index:03}.psxt"),
        source_label,
        streamed_class: StreamedClass::None,
    });
    texture_asset_for_path.insert(cache_key, asset_index);
    Some(asset_index)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_image_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    material: Option<ResourceId>,
    width: u16,
    height: u16,
    cylindrical_billboard: bool,
    collision_enabled: bool,
    collision_size: [u16; 3],
    destructible: u16,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    image_props: &mut Vec<PlaytestImageProp>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(material_id) = material else {
        report.warn(format!(
            "Image Prop '{node_name}' has no Material - skipped"
        ));
        return true;
    };
    let label = format!("Image Prop '{node_name}'");
    let Some((texture_asset_index, tint_rgb)) = resolve_material_texture_asset(
        project,
        project_root,
        &label,
        material_id,
        texture_asset_for_path,
        assets,
        report,
    ) else {
        return true;
    };
    let (collision_min, collision_max) = if collision_enabled {
        image_prop_collision_aabb(
            pos,
            height.max(1),
            collision_size,
            pitch,
            yaw,
            roll,
            cylindrical_billboard,
        )
    } else {
        ([0; 3], [0; 3])
    };
    image_props.push(PlaytestImageProp {
        room: room_index,
        texture_asset_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        pitch,
        yaw,
        roll,
        width: width.max(1),
        height: height.max(1),
        tint_rgb,
        baked_vertex_rgb: [rgb_tuple(tint_rgb); 4],
        collision_min,
        collision_max,
        destructible,
        flags: (if cylindrical_billboard {
            image_prop_flags::CYLINDRICAL_BILLBOARD
        } else {
            0
        }) | (if collision_enabled {
            image_prop_flags::COLLISION_ENABLED
        } else {
            0
        }),
    });
    true
}

/// Cook the editor's oriented ImageProp collision wireframe into the
/// conservative room-local AABB consumed by the PS1 character motor.
///
/// The visual plane is bottom-centred, while its collision box is centred at
/// half the visual height. Cylindrical billboards deliberately keep an
/// axis-aligned collision box because their visual yaw changes every frame;
/// static cards use the same X -> Y -> Z Q12 rotation as preview and runtime
/// rendering before their eight corners are enclosed.
pub(crate) fn image_prop_collision_aabb(
    origin: [i32; 3],
    visual_height: u16,
    collision_size: [u16; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    cylindrical_billboard: bool,
) -> ([i32; 3], [i32; 3]) {
    let half = collision_size.map(|component| ((component as i32) / 2).max(1));
    let center_y = (visual_height as i32) / 2;
    if cylindrical_billboard {
        let center = [origin[0], origin[1].saturating_add(center_y), origin[2]];
        return (
            [
                center[0].saturating_sub(half[0]),
                center[1].saturating_sub(half[1]),
                center[2].saturating_sub(half[2]),
            ],
            [
                center[0].saturating_add(half[0]),
                center[1].saturating_add(half[1]),
                center[2].saturating_add(half[2]),
            ],
        );
    }

    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    for z in [-half[2], half[2]] {
        for y in [
            center_y.saturating_sub(half[1]),
            center_y.saturating_add(half[1]),
        ] {
            for x in [-half[0], half[0]] {
                let rotated = crate::spatial::rotate_euler_local_q12(
                    [x, y, z],
                    pitch as u16,
                    yaw as u16,
                    roll as u16,
                );
                let world = [
                    origin[0].saturating_add(rotated[0]),
                    origin[1].saturating_add(rotated[1]),
                    origin[2].saturating_add(rotated[2]),
                ];
                let mut axis = 0usize;
                while axis < 3 {
                    min[axis] = min[axis].min(world[axis]);
                    max[axis] = max[axis].max(world[axis]);
                    axis += 1;
                }
            }
        }
    }
    (min, max)
}

/// Build the shared spatial registry consumed by play.
/// Specialized payload tables stay compact, but no prop/marker is allowed to
/// bypass this room/bounds/policy contract.
pub(crate) fn cook_world_objects(
    image_props: &[PlaytestImageProp],
    box_props: &[PlaytestBoxProp],
    cylinder_props: &[PlaytestCylinderProp],
    arch_props: &[PlaytestArchProp],
    arch_surfaces: &[PlaytestArchPropSurface],
    interactables: &[PlaytestInteractable],
    report: &mut PlaytestValidationReport,
) -> Vec<PlaytestWorldObject> {
    use psx_level::{world_object_flags as flags, world_object_kind as kind};

    let expected = image_props
        .len()
        .saturating_add(box_props.len())
        .saturating_add(cylinder_props.len())
        .saturating_add(arch_props.len())
        .saturating_add(
            interactables
                .iter()
                .filter(|item| item.kind == PlaytestInteractableKind::PointOfInterest)
                .count(),
        );
    if expected > psx_level::MAX_WORLD_OBJECTS {
        report.error(format!(
            "Level cooks {expected} spatial world objects, exceeding the PS1 runtime contract cap of {}",
            psx_level::MAX_WORLD_OBJECTS,
        ));
        return Vec::new();
    }

    let mut objects = Vec::with_capacity(expected);
    let mut push = |room: u16,
                    object_kind: u8,
                    source_index: usize,
                    object_flags: u8,
                    destructible: u16,
                    bounds_min: [i32; 3],
                    bounds_max: [i32; 3]| {
        let Ok(source_index) = u16::try_from(source_index) else {
            report.error("World-object typed source index exceeds u16");
            return;
        };
        if (0..3).any(|axis| bounds_min[axis] >= bounds_max[axis]) {
            report.error(format!(
                "World object kind {object_kind} source {source_index} has invalid cooked bounds {bounds_min:?}..{bounds_max:?}"
            ));
            return;
        }
        objects.push(PlaytestWorldObject {
            room,
            kind: object_kind,
            flags: object_flags,
            source_index,
            destructible,
            bounds_min,
            bounds_max,
        });
    };

    for (index, prop) in image_props.iter().enumerate() {
        let visual_depth = if prop.flags & image_prop_flags::CYLINDRICAL_BILLBOARD != 0 {
            prop.width
        } else {
            2
        };
        let (mut bounds_min, mut bounds_max) = image_prop_collision_aabb(
            [prop.x, prop.y, prop.z],
            prop.height,
            [prop.width, prop.height, visual_depth],
            prop.pitch,
            prop.yaw,
            prop.roll,
            prop.flags & image_prop_flags::CYLINDRICAL_BILLBOARD != 0,
        );
        let collidable = prop.flags & image_prop_flags::COLLISION_ENABLED != 0;
        if collidable {
            union_bounds(
                &mut bounds_min,
                &mut bounds_max,
                prop.collision_min,
                prop.collision_max,
            );
        }
        push(
            prop.room,
            kind::IMAGE_PROP,
            index,
            flags::RENDERED
                | flags::DIRECT_OCCLUSION
                | if collidable { flags::COLLIDABLE } else { 0 },
            prop.destructible,
            bounds_min,
            bounds_max,
        );
    }

    for (index, prop) in box_props.iter().enumerate() {
        let collidable = prop.flags & psx_level::box_prop_flags::COLLISION_ENABLED != 0;
        push(
            prop.room,
            kind::BOX_PROP,
            index,
            flags::RENDERED
                | flags::DIRECT_OCCLUSION
                | flags::DYNAMIC
                | if collidable { flags::COLLIDABLE } else { 0 },
            psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE,
            prop.collision_min,
            prop.collision_max,
        );
    }

    for (index, prop) in cylinder_props.iter().enumerate() {
        let collidable = prop.flags & psx_level::cylinder_prop_flags::COLLISION_ENABLED != 0;
        push(
            prop.room,
            kind::CYLINDER_PROP,
            index,
            flags::RENDERED
                | flags::DIRECT_OCCLUSION
                | if collidable { flags::COLLIDABLE } else { 0 },
            psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE,
            prop.bounds_min,
            prop.bounds_max,
        );
    }

    for (index, prop) in arch_props.iter().enumerate() {
        let first = usize::from(prop.surface_first);
        let end = first
            .saturating_add(usize::from(prop.surface_count))
            .min(arch_surfaces.len());
        let mut bounds_min = [i32::MAX; 3];
        let mut bounds_max = [i32::MIN; 3];
        for surface in arch_surfaces.get(first..end).unwrap_or(&[]) {
            for vertex in surface.vertices {
                for axis in 0..3 {
                    bounds_min[axis] = bounds_min[axis].min(vertex[axis]);
                    bounds_max[axis] = bounds_max[axis].max(vertex[axis]);
                }
            }
        }
        if bounds_min[0] == i32::MAX {
            let radius = prop.cull_radius.max(1);
            bounds_min = prop.center.map(|value| value.saturating_sub(radius));
            bounds_max = prop.center.map(|value| value.saturating_add(radius));
        }
        let collidable = prop.collision_count != 0;
        push(
            prop.room,
            kind::ARCH_PROP,
            index,
            flags::RENDERED
                | flags::DIRECT_OCCLUSION
                | if collidable { flags::COLLIDABLE } else { 0 },
            psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE,
            bounds_min,
            bounds_max,
        );
    }

    for (index, interactable) in interactables.iter().enumerate() {
        if interactable.kind != PlaytestInteractableKind::PointOfInterest {
            continue;
        }
        // Must mirror marker_runtime::archive_beacon_world_height. The beacon
        // rotates inside this conservative square prism and hangs from the
        // authored floor anchor.
        let height = i32::from(interactable.marker_height).clamp(6, 32);
        let half = (height / 2).max(3);
        let depth = (height / 12).clamp(1, 3);
        let pivot = (height / 6).clamp(1, 4);
        let radial = half.saturating_add(depth).saturating_add(1);
        push(
            interactable.room,
            kind::POINT_OF_INTEREST_BEACON,
            index,
            flags::RENDERED | flags::DIRECT_OCCLUSION | flags::DYNAMIC,
            psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE,
            [
                interactable.x.saturating_sub(radial),
                interactable
                    .y
                    .saturating_sub(height.saturating_add(pivot).saturating_add(1)),
                interactable.z.saturating_sub(radial),
            ],
            [
                interactable.x.saturating_add(radial),
                interactable.y.saturating_add(1),
                interactable.z.saturating_add(radial),
            ],
        );
    }

    objects
}

fn union_bounds(min: &mut [i32; 3], max: &mut [i32; 3], other_min: [i32; 3], other_max: [i32; 3]) {
    for axis in 0..3 {
        min[axis] = min[axis].min(other_min[axis]);
        max[axis] = max[axis].max(other_max[axis]);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_box_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    ground_y: i32,
    pitch: i16,
    yaw: i16,
    roll: i16,
    materials: &[Option<ResourceId>; crate::BOX_PROP_FACE_COUNT],
    uvs: &[crate::UvTransform; crate::BOX_PROP_FACE_COUNT],
    vertices: [[i16; 3]; crate::BOX_PROP_VERTEX_COUNT],
    collision_enabled: bool,
    break_flags: u16,
    erosion: crate::BoxPropErosion,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    box_props: &mut Vec<PlaytestBoxProp>,
    box_prop_surfaces: &mut Vec<PlaytestBoxPropSurface>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut texture_asset_indices = [None; psx_level::BOX_PROP_FACE_COUNT];
    let mut blend_modes = [psx_level::model_override_blend::OPAQUE; psx_level::BOX_PROP_FACE_COUNT];
    let mut cooked_uvs = [[(0, 0); 4]; psx_level::BOX_PROP_FACE_COUNT];
    let mut tint_rgb = [[128, 128, 128]; psx_level::BOX_PROP_FACE_COUNT];
    let mut valid_faces = 0usize;
    for (face, material) in materials.iter().enumerate() {
        let Some(material_id) = *material else {
            continue;
        };
        let label = format!(
            "Box Prop '{node_name}' {} face",
            crate::BOX_PROP_FACE_NAMES[face]
        );
        let Some((texture_asset_index, tint)) = resolve_material_texture_asset(
            project,
            project_root,
            &label,
            material_id,
            texture_asset_for_path,
            assets,
            report,
        ) else {
            continue;
        };
        texture_asset_indices[face] = Some(texture_asset_index);
        if let Ok(texture) = psx_asset::Texture::from_bytes(&assets[texture_asset_index].bytes) {
            cooked_uvs[face] =
                crate::prop_surfaces::prop_uv_corners(uvs[face], texture.width(), texture.height());
        }
        tint_rgb[face] = tint;
        blend_modes[face] = project
            .resource(material_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Material(material) => Some(
                    super::manifest::model_override_blend_code(material.blend_mode),
                ),
                _ => None,
            })
            .unwrap_or(psx_level::model_override_blend::OPAQUE);
        valid_faces += 1;
    }

    if valid_faces == 0 {
        report.warn(format!(
            "Box Prop '{node_name}' has no drawable Material faces - skipped"
        ));
        return true;
    }

    let mut baked_vertex_rgb = [[rgb_tuple([128, 128, 128]); 4]; psx_level::BOX_PROP_FACE_COUNT];
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        baked_vertex_rgb[face] = [rgb_tuple(tint_rgb[face]); 4];
    }

    let mut flags = break_flags & box_prop_flags::BREAK_ON_MASK;
    if collision_enabled {
        flags |= box_prop_flags::COLLISION_ENABLED;
    }

    let (collision_min, collision_max) = box_prop_collision_aabb(pos, pitch, yaw, roll, vertices);
    if collision_enabled && (0..3).any(|axis| collision_min[axis] >= collision_max[axis]) {
        report.error(format!(
            "Box Prop '{node_name}' has degenerate collision bounds"
        ));
        return false;
    }

    let generated = crate::generate_box_prop_erosion_quads(vertices, erosion);
    let surface_first = box_prop_surfaces.len();
    if surface_first > u16::MAX as usize
        || generated.len() > u16::MAX as usize
        || surface_first.saturating_add(generated.len()) > u16::MAX as usize
    {
        report.error(format!(
            "Box Prop '{node_name}' generated surface table exceeds 65535 entries"
        ));
        return false;
    }
    for quad in generated {
        let world_vertices = quad
            .vertices
            .map(|local| crate::prop_surfaces::prop_local_to_world(pos, local, pitch, yaw, roll));
        let center = box_prop_surface_center(world_vertices);
        let normal = box_prop_surface_normal(world_vertices);
        let face = usize::from(quad.source_face).min(crate::BOX_PROP_FACE_COUNT - 1);
        let baked_uv = quad.uv_q8.map(|uv| bake_prop_uv(cooked_uvs[face], uv));
        box_prop_surfaces.push(PlaytestBoxPropSurface {
            vertices: world_vertices,
            center,
            normal,
            uv_q8: baked_uv,
            baked_vertex_rgb: [rgb_tuple(tint_rgb[face]); 4],
            source_face: face as u8,
            flags: psx_level::box_prop_surface_flags::UV_BAKED,
        });
    }

    box_props.push(PlaytestBoxProp {
        room: room_index,
        texture_asset_indices,
        blend_modes,
        uvs: cooked_uvs,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        ground_y,
        pitch,
        yaw,
        roll,
        vertices,
        collision_min,
        collision_max,
        surface_first: surface_first as u16,
        surface_count: (box_prop_surfaces.len() - surface_first) as u16,
        tint_rgb,
        baked_vertex_rgb,
        flags,
    });
    true
}

/// Cook a resized and rotated BoxProp's exact conservative AABB once on the
/// host. Runtime rendering still owns the oriented faces, but collision and
/// support logic consume these integer bounds without rebuilding trigonometry.
pub(crate) fn box_prop_collision_aabb(
    origin: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    vertices: [[i16; 3]; crate::BOX_PROP_VERTEX_COUNT],
) -> ([i32; 3], [i32; 3]) {
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    for local in vertices {
        let rotated = crate::spatial::rotate_euler_local_q12(
            [
                i32::from(local[0]),
                i32::from(local[1]),
                i32::from(local[2]),
            ],
            pitch as u16,
            yaw as u16,
            roll as u16,
        );
        let world = [
            origin[0].saturating_add(rotated[0]),
            origin[1].saturating_add(rotated[1]),
            origin[2].saturating_add(rotated[2]),
        ];
        for axis in 0..3 {
            min[axis] = min[axis].min(world[axis]);
            max[axis] = max[axis].max(world[axis]);
        }
    }
    (min, max)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_cylinder_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    materials: &[Option<ResourceId>; crate::CYLINDER_PROP_MATERIAL_COUNT],
    uvs: &[crate::UvTransform; crate::CYLINDER_PROP_MATERIAL_COUNT],
    geometry: crate::CylinderPropGeometry,
    collision_enabled: bool,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    cylinder_props: &mut Vec<PlaytestCylinderProp>,
    cylinder_prop_surfaces: &mut Vec<PlaytestCylinderPropSurface>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut texture_asset_indices = [None; psx_level::CYLINDER_PROP_MATERIAL_COUNT];
    let mut blend_modes =
        [psx_level::model_override_blend::OPAQUE; psx_level::CYLINDER_PROP_MATERIAL_COUNT];
    let mut cooked_uvs = [[(0, 0); 4]; psx_level::CYLINDER_PROP_MATERIAL_COUNT];
    let mut tint_rgb = [[128, 128, 128]; psx_level::CYLINDER_PROP_MATERIAL_COUNT];
    let mut valid_materials = 0usize;
    for (slot, material) in materials.iter().enumerate() {
        let Some(material_id) = *material else {
            continue;
        };
        let label = format!(
            "Cylinder Prop '{node_name}' {}",
            crate::CYLINDER_PROP_MATERIAL_NAMES[slot]
        );
        let Some((texture_asset_index, tint)) = resolve_material_texture_asset(
            project,
            project_root,
            &label,
            material_id,
            texture_asset_for_path,
            assets,
            report,
        ) else {
            continue;
        };
        texture_asset_indices[slot] = Some(texture_asset_index);
        if let Ok(texture) = psx_asset::Texture::from_bytes(&assets[texture_asset_index].bytes) {
            cooked_uvs[slot] =
                crate::prop_surfaces::prop_uv_corners(uvs[slot], texture.width(), texture.height());
        }
        tint_rgb[slot] = tint;
        blend_modes[slot] = project
            .resource(material_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Material(material) => Some(
                    super::manifest::model_override_blend_code(material.blend_mode),
                ),
                _ => None,
            })
            .unwrap_or(psx_level::model_override_blend::OPAQUE);
        valid_materials += 1;
    }
    if valid_materials == 0 {
        report.warn(format!(
            "Cylinder Prop '{node_name}' has no drawable Materials - skipped"
        ));
        return true;
    }

    let generated = crate::generate_cylinder_prop_surfaces(geometry);
    let surface_first = cylinder_prop_surfaces.len();
    if surface_first > u16::MAX as usize
        || generated.len() > u16::MAX as usize
        || surface_first.saturating_add(generated.len()) > u16::MAX as usize
    {
        report.error(format!(
            "Cylinder Prop '{node_name}' generated surface table exceeds 65535 entries"
        ));
        return false;
    }

    let mut bounds_min = [i32::MAX; 3];
    let mut bounds_max = [i32::MIN; 3];
    for surface in generated {
        let vertex_count = usize::from(surface.vertex_count.clamp(3, 4));
        let world_vertices = surface
            .vertices
            .map(|local| crate::prop_surfaces::prop_local_to_world(pos, local, pitch, yaw, roll));
        for world_vertex in &world_vertices[..vertex_count] {
            for axis in 0..3 {
                bounds_min[axis] = bounds_min[axis].min(world_vertex[axis]);
                bounds_max[axis] = bounds_max[axis].max(world_vertex[axis]);
            }
        }
        let center = polygon_surface_center(world_vertices, vertex_count);
        let normal = box_prop_surface_normal(world_vertices);
        let material_slot =
            usize::from(surface.material_slot).min(crate::CYLINDER_PROP_MATERIAL_COUNT - 1);
        let baked_uv = surface
            .uv_q8
            .map(|uv| bake_prop_uv(cooked_uvs[material_slot], uv));
        cylinder_prop_surfaces.push(PlaytestCylinderPropSurface {
            vertices: world_vertices,
            center,
            normal,
            uv_q8: baked_uv,
            baked_vertex_rgb: [rgb_tuple(tint_rgb[material_slot]); 4],
            material_slot: material_slot as u8,
            vertex_count: vertex_count as u8,
        });
    }
    if bounds_min[0] == i32::MAX {
        report.warn(format!(
            "Cylinder Prop '{node_name}' generated no surfaces - skipped"
        ));
        return true;
    }
    let center = [
        bounds_min[0].saturating_add(bounds_max[0]) / 2,
        bounds_min[1].saturating_add(bounds_max[1]) / 2,
        bounds_min[2].saturating_add(bounds_max[2]) / 2,
    ];
    let half = [
        i64::from(bounds_max[0] - bounds_min[0]) / 2,
        i64::from(bounds_max[1] - bounds_min[1]) / 2,
        i64::from(bounds_max[2] - bounds_min[2]) / 2,
    ];
    let cull_radius = ((half[0] * half[0] + half[1] * half[1] + half[2] * half[2]) as f64)
        .sqrt()
        .ceil()
        .clamp(1.0, i32::MAX as f64) as i32;
    cylinder_props.push(PlaytestCylinderProp {
        room: room_index,
        texture_asset_indices,
        blend_modes,
        uvs: cooked_uvs,
        tint_rgb,
        surface_first: surface_first as u16,
        surface_count: (cylinder_prop_surfaces.len() - surface_first) as u16,
        center,
        cull_radius,
        bounds_min,
        bounds_max,
        flags: if collision_enabled {
            psx_level::cylinder_prop_flags::COLLISION_ENABLED
        } else {
            0
        },
    });
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_arch_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    sector_size: i32,
    materials: &[Option<ResourceId>; crate::ARCH_PROP_MATERIAL_COUNT],
    uvs: &[crate::UvTransform; crate::ARCH_PROP_MATERIAL_COUNT],
    geometry: crate::ArchPropGeometry,
    collision_enabled: bool,
    texture_asset_for_path: &mut HashMap<String, usize>,
    assets: &mut Vec<PlaytestAsset>,
    arch_props: &mut Vec<PlaytestArchProp>,
    arch_prop_surfaces: &mut Vec<PlaytestArchPropSurface>,
    arch_prop_collisions: &mut Vec<PlaytestArchPropCollision>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut texture_asset_indices = [None; psx_level::ARCH_PROP_MATERIAL_COUNT];
    let mut blend_modes =
        [psx_level::model_override_blend::OPAQUE; psx_level::ARCH_PROP_MATERIAL_COUNT];
    let mut cooked_uvs = [[(0, 0); 4]; psx_level::ARCH_PROP_MATERIAL_COUNT];
    let mut tint_rgb = [[128, 128, 128]; psx_level::ARCH_PROP_MATERIAL_COUNT];
    let mut valid_materials = 0;
    for (slot, material) in materials.iter().enumerate() {
        let Some(material_id) = *material else {
            continue;
        };
        let label = format!(
            "Arch Prop '{node_name}' {}",
            crate::ARCH_PROP_MATERIAL_NAMES[slot]
        );
        let Some((texture_asset_index, tint)) = resolve_material_texture_asset(
            project,
            project_root,
            &label,
            material_id,
            texture_asset_for_path,
            assets,
            report,
        ) else {
            continue;
        };
        texture_asset_indices[slot] = Some(texture_asset_index);
        if let Ok(texture) = psx_asset::Texture::from_bytes(&assets[texture_asset_index].bytes) {
            cooked_uvs[slot] =
                crate::prop_surfaces::prop_uv_corners(uvs[slot], texture.width(), texture.height());
        }
        tint_rgb[slot] = tint;
        blend_modes[slot] = project
            .resource(material_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Material(material) => Some(
                    super::manifest::model_override_blend_code(material.blend_mode),
                ),
                _ => None,
            })
            .unwrap_or(psx_level::model_override_blend::OPAQUE);
        valid_materials += 1;
    }
    if valid_materials == 0 {
        report.warn(format!(
            "Arch Prop '{node_name}' has no drawable Materials - skipped"
        ));
        return true;
    }

    let height_quantum = crate::HEIGHT_QUANTUM / crate::units::WORLD_UNIT_DIVISOR;
    let generated =
        crate::generate_arch_prop_surfaces_with_quantum(geometry, sector_size, height_quantum);
    let surface_first = arch_prop_surfaces.len();
    if surface_first.saturating_add(generated.len()) > u16::MAX as usize {
        report.error(format!(
            "Arch Prop '{node_name}' generated surface table exceeds 65535 entries"
        ));
        return false;
    }
    let mut bounds_min = [i32::MAX; 3];
    let mut bounds_max = [i32::MIN; 3];
    for surface in generated {
        let vertices = surface.vertices.map(|local| {
            let rotated = crate::spatial::rotate_euler_local_q12(
                [
                    i32::from(local[0]),
                    i32::from(local[1]),
                    i32::from(local[2]),
                ],
                pitch as u16,
                yaw as u16,
                roll as u16,
            );
            [
                pos[0].saturating_add(rotated[0]),
                pos[1].saturating_add(rotated[1]),
                pos[2].saturating_add(rotated[2]),
            ]
        });
        for vertex in vertices {
            for axis in 0..3 {
                bounds_min[axis] = bounds_min[axis].min(vertex[axis]);
                bounds_max[axis] = bounds_max[axis].max(vertex[axis]);
            }
        }
        let material_slot =
            usize::from(surface.material_slot).min(crate::ARCH_PROP_MATERIAL_COUNT - 1);
        arch_prop_surfaces.push(PlaytestArchPropSurface {
            vertices,
            center: polygon_surface_center(vertices, 4),
            normal: box_prop_surface_normal(vertices),
            uv_q8: surface.uv_q8,
            baked_vertex_rgb: [rgb_tuple(tint_rgb[material_slot]); 4],
            material_slot: material_slot as u8,
        });
    }

    let collision_first = arch_prop_collisions.len();
    if collision_enabled {
        for collision in crate::generate_arch_prop_collision_boxes_with_quantum(
            geometry,
            sector_size,
            height_quantum,
        ) {
            let mut world_min = [i32::MAX; 3];
            let mut world_max = [i32::MIN; 3];
            for x in [collision.min[0], collision.max[0]] {
                for y in [collision.min[1], collision.max[1]] {
                    for z in [collision.min[2], collision.max[2]] {
                        let rotated = crate::spatial::rotate_euler_local_q12(
                            [i32::from(x), i32::from(y), i32::from(z)],
                            pitch as u16,
                            yaw as u16,
                            roll as u16,
                        );
                        let world = [
                            pos[0].saturating_add(rotated[0]),
                            pos[1].saturating_add(rotated[1]),
                            pos[2].saturating_add(rotated[2]),
                        ];
                        for axis in 0..3 {
                            world_min[axis] = world_min[axis].min(world[axis]);
                            world_max[axis] = world_max[axis].max(world[axis]);
                        }
                    }
                }
            }
            arch_prop_collisions.push(PlaytestArchPropCollision {
                min: world_min,
                max: world_max,
            });
        }
    }
    let collision_count = arch_prop_collisions.len() - collision_first;
    if collision_first.saturating_add(collision_count) > u16::MAX as usize
        || collision_count > u8::MAX as usize
    {
        report.error(format!(
            "Arch Prop '{node_name}' collision table exceeds its fixed runtime indices"
        ));
        return false;
    }
    let center = [
        bounds_min[0].saturating_add(bounds_max[0]) / 2,
        bounds_min[1].saturating_add(bounds_max[1]) / 2,
        bounds_min[2].saturating_add(bounds_max[2]) / 2,
    ];
    let half = [
        i64::from(bounds_max[0] - bounds_min[0]) / 2,
        i64::from(bounds_max[1] - bounds_min[1]) / 2,
        i64::from(bounds_max[2] - bounds_min[2]) / 2,
    ];
    let cull_radius = ((half[0] * half[0] + half[1] * half[1] + half[2] * half[2]) as f64)
        .sqrt()
        .ceil()
        .clamp(1.0, i32::MAX as f64) as i32;
    arch_props.push(PlaytestArchProp {
        room: room_index,
        texture_asset_indices,
        blend_modes,
        uvs: cooked_uvs,
        tint_rgb,
        surface_first: surface_first as u16,
        surface_count: (arch_prop_surfaces.len() - surface_first) as u16,
        collision_first: collision_first as u16,
        collision_count: collision_count as u8,
        center,
        cull_radius,
        flags: if collision_enabled {
            psx_level::arch_prop_flags::COLLISION_ENABLED
        } else {
            0
        },
    });
    true
}

pub(crate) fn push_point_light(
    node_name: &str,
    sector_size: i32,
    room_index: u16,
    pos: [i32; 3],
    color: [u8; 3],
    intensity: f32,
    radius: f32,
    lights: &mut Vec<PlaytestLight>,
    report: &mut PlaytestValidationReport,
) -> bool {
    // Reject obviously broken lights at cook time -- radius 0
    // contributes nothing, negative intensity is meaningless.
    // Clamp the rest into the wire format's u16 ranges.
    if !radius.is_finite() || radius <= 0.0 {
        report.error(format!(
            "Light '{node_name}' has radius {radius} (must be > 0)"
        ));
        return false;
    }
    if !intensity.is_finite() || intensity < 0.0 {
        report.error(format!(
            "Light '{node_name}' has invalid intensity {intensity}"
        ));
        return false;
    }
    // Editor radius is in *sector units* -- convert to world
    // units (engine units) at cook time so the runtime record
    // stays in one canonical unit regardless of room sector size.
    let radius_world = (radius * sector_size.max(1) as f32).clamp(1.0, u16::MAX as f32) as u16;
    let intensity_q8 = (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u16;
    lights.push(PlaytestLight {
        room: room_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        radius: radius_world,
        intensity_q8,
        color,
    });
    true
}

pub(crate) fn push_particle_emitter(
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    settings: &ParticleEmitterSettings,
    particle_emitters: &mut Vec<PlaytestParticleEmitter>,
    report: &mut PlaytestValidationReport,
) -> bool {
    if !settings.enabled {
        return true;
    }
    if settings.max_particles == 0 {
        report.warn(format!(
            "Particle Emitter '{node_name}' has max_particles=0 -- skipped"
        ));
        return true;
    }
    if settings.lifetime_frames == 0 {
        report.warn(format!(
            "Particle Emitter '{node_name}' has lifetime_frames=0 -- skipped"
        ));
        return true;
    }
    particle_emitters.push(PlaytestParticleEmitter {
        room: room_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        max_particles: settings.max_particles,
        spawn_rate_q8: settings.spawn_rate_q8,
        lifetime_frames: settings.lifetime_frames,
        start_size: settings.start_size,
        end_size: settings.end_size,
        start_color: settings.start_color,
        end_color: settings.end_color,
        blend_mode: particle_blend_mode_code(settings.blend_mode),
        base_velocity_q4: settings.base_velocity_q4,
        random_velocity_q4: settings.random_velocity_q4,
        acceleration_q4: settings.acceleration_q4,
        spawn_radius: settings.spawn_radius,
        flags: particle_emitter_flags::ENABLED,
    });
    true
}

/// Append one single-page message to the interactable message tables and
/// return its index.
fn push_single_page_message(
    messages: &mut Vec<PlaytestInteractableMessage>,
    message_pages: &mut Vec<String>,
    message_pages_it: &mut Vec<String>,
    title: String,
    body: String,
) -> u16 {
    let message = messages.len().min(u16::MAX as usize) as u16;
    let page_first = message_pages.len().min(u16::MAX as usize) as u16;
    message_pages.push(body.clone());
    message_pages_it.push(String::new());
    messages.push(PlaytestInteractableMessage {
        title,
        body,
        page_first,
        page_count: 1,
    });
    message
}

/// Ticks (60 Hz) a toggle switch refuses a second press, so a mashed button
/// cannot flip a moving door back and forth every frame.
const SWITCH_TOGGLE_COOLDOWN_TICKS: i16 = 30;

// Borrow the existing cook tables without allocating an intermediate context.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_interactable(
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    component: InteractableComponent<'_>,
    names: &mut NameInterner,
    messages: &mut Vec<PlaytestInteractableMessage>,
    message_pages: &mut Vec<String>,
    message_pages_it: &mut Vec<String>,
    interactables: &mut Vec<PlaytestInteractable>,
    logic: &mut Vec<PlaytestLogic>,
    report: &mut PlaytestValidationReport,
) -> bool {
    if component.radius == 0 {
        report.error(format!(
            "Interactable on '{node_name}' has radius 0 (must be > 0)"
        ));
        return false;
    }

    let prompt = non_empty_or(
        component.prompt,
        default_prompt_for_interactable(component.kind),
    );
    let (kind, logic_kind, title, body, checkpoint_id) = match component.kind {
        crate::InteractableKind::Message { title, body } => (
            PlaytestInteractableKind::Message,
            psx_level::logic_kind::MESSAGE,
            non_empty_or(title, "ECHO REMNANT").to_string(),
            body.clone(),
            String::new(),
        ),
        crate::InteractableKind::Checkpoint {
            checkpoint_id,
            title,
            body,
        } => (
            PlaytestInteractableKind::Checkpoint,
            psx_level::logic_kind::CHECKPOINT,
            non_empty_or(title, "SYNC RELAY").to_string(),
            non_empty_or(body, "Relay synchronized.").to_string(),
            non_empty_or(checkpoint_id, node_name).to_string(),
        ),
        crate::InteractableKind::Switch { target, .. } => {
            if target.trim().is_empty() {
                report.error(format!(
                    "Switch on '{node_name}' has no target - a lever that fires \
                     nothing is dead content"
                ));
                return false;
            }
            (
                PlaytestInteractableKind::Switch,
                psx_level::logic_kind::SWITCH,
                String::new(),
                String::new(),
                String::new(),
            )
        }
    };
    // A switch shows no text, so it takes no slot in the message tables.
    let message = if kind == PlaytestInteractableKind::Switch {
        psx_level::INTERACTABLE_MESSAGE_NONE
    } else {
        push_single_page_message(messages, message_pages, message_pages_it, title, body)
    };
    // A switch's wiring: its target, and whether it retires after one press
    // (wait -1) or cools down between presses.
    let (switch_target, switch_wait, switch_spawnflags) = match component.kind {
        crate::InteractableKind::Switch {
            target,
            behavior,
            unlocks,
            ..
        } => (
            names.intern(target),
            match behavior {
                crate::SwitchBehavior::OneShot => -1,
                crate::SwitchBehavior::Toggle => SWITCH_TOGGLE_COOLDOWN_TICKS,
            },
            if *unlocks {
                psx_level::switch_flags::UNLOCK
            } else {
                0
            },
        ),
        _ => (psx_level::LOGIC_NAME_NONE, 0, 0),
    };
    let flags = if component.enabled {
        psx_level::interactable_flags::ENABLED
    } else {
        0
    };
    // The paired event-graph record lands at this index; the
    // interactable stores it so the interact prompt can fire the
    // record directly (and the effect dispatch can walk back).
    let paired_logic = logic.len().min(u16::MAX as usize) as u16;
    interactables.push(PlaytestInteractable {
        room: room_index,
        kind,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        radius: component.radius,
        marker_height: 0,
        prompt: prompt.to_string(),
        message,
        logic: paired_logic,
        checkpoint_id,
        read_flag: psx_level::POI_FLAG_NONE,
        reward_flag: psx_level::POI_FLAG_NONE,
        reward_resource: psx_level::POI_REWARD_NONE,
        reward_quantity: 0,
        flags,
    });
    // The paired event-graph record: same authored source, interned
    // name, XZ-radius bounds (zero height -- interactable range tests
    // are XZ-only, and the runtime keeps that semantics for these
    // kinds). Target/kill/master stay NONE for interactable-paired
    // records; placed Logic nodes carry the graph edges, which can
    // point AT this record by node name.
    let radius = i32::from(component.radius);
    logic.push(PlaytestLogic {
        room: room_index,
        kind: logic_kind,
        spawnflags: switch_spawnflags,
        targetname: names.intern(node_name),
        target: switch_target,
        killtarget: psx_level::LOGIC_NAME_NONE,
        master: psx_level::LOGIC_NAME_NONE,
        delay_ticks: 0,
        wait_ticks: switch_wait,
        arg0: 0,
        arg1: 0,
        link: psx_level::LOGIC_LINK_NONE,
        message,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        min: [pos[0] - radius, pos[1], pos[2] - radius],
        max: [pos[0] + radius, pos[1], pos[2] + radius],
        flags: if component.enabled {
            psx_level::logic_flags::ENABLED
        } else {
            0
        } | if matches!(
            component.kind,
            crate::InteractableKind::Switch {
                persistent: true,
                ..
            }
        ) {
            psx_level::logic_flags::PERSISTENT
        } else {
            0
        },
    });
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_point_of_interest(
    project: &ProjectDocument,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    component: PointOfInterestComponent<'_>,
    read_flag: u16,
    reward_flag: u16,
    messages: &mut Vec<PlaytestInteractableMessage>,
    message_pages: &mut Vec<String>,
    message_pages_it: &mut Vec<String>,
    interactables: &mut Vec<PlaytestInteractable>,
    boost_modules: &mut Vec<PlaytestBoostModule>,
    report: &mut PlaytestValidationReport,
) -> bool {
    if component.radius == 0 {
        report.error(format!(
            "Point of Interest on '{node_name}' has radius 0 (must be > 0)"
        ));
        return false;
    }
    if component.marker_height == 0 {
        report.error(format!(
            "Point of Interest on '{node_name}' has beacon scale 0 (must be > 0)"
        ));
        return false;
    }
    if !validate_message_pages(
        &format!("Point of Interest on '{node_name}'"),
        component.pages,
        2,
        runtime_message_font(project),
        report,
    ) {
        return false;
    }
    if message_pages.len().saturating_add(component.pages.len()) > u16::MAX as usize {
        report.error("Point-of-interest message page table exceeds 65535 entries");
        return false;
    }
    let Some(pages_it) = italian_pages(
        &format!("Point of Interest on '{node_name}'"),
        component.pages,
        component.pages_it,
        2,
        runtime_message_font(project),
        report,
    ) else {
        return false;
    };

    let (reward_resource, reward_quantity) = match component.reward {
        None => (psx_level::POI_REWARD_NONE, 0),
        Some(reward) => {
            if reward.quantity != 1 {
                report.error(format!(
                    "Point of Interest on '{node_name}' grants a unique module, so reward quantity must be 1"
                ));
                return false;
            }
            if boost_modules.len() >= psx_level::MAX_BOOST_MODULES {
                report.error(format!(
                    "Project exceeds the {} unique-module runtime limit",
                    psx_level::MAX_BOOST_MODULES
                ));
                return false;
            }

            let (name, description, modifiers) = if !reward.item_name.trim().is_empty() {
                (
                    reward.item_name.trim().to_string(),
                    reward.description.trim().to_string(),
                    reward.modifiers.clone(),
                )
            } else {
                let Some(module_id) = reward.module else {
                    report.error(format!(
                        "Point of Interest on '{node_name}' has a reward but no item name"
                    ));
                    return false;
                };
                let Some(resource) = project.resource(module_id) else {
                    report.error(format!(
                        "Point of Interest on '{node_name}' references missing reward resource #{}",
                        module_id.raw()
                    ));
                    return false;
                };
                let ResourceData::BoostModule(module) = &resource.data else {
                    report.error(format!(
                        "Point of Interest on '{node_name}' reward '{}' is not a Boost Module",
                        resource.name
                    ));
                    return false;
                };
                let modifiers = if module.modifiers.is_empty() {
                    vec![legacy_boost_modifier(module.kind)]
                } else {
                    module.modifiers.clone()
                };
                (
                    resource.name.trim().to_string(),
                    module.description.trim().to_string(),
                    modifiers,
                )
            };
            if name.is_empty() {
                report.error(format!(
                    "Point of Interest on '{node_name}' has a reward with an empty item name"
                ));
                return false;
            }
            if boost_modules
                .iter()
                .any(|module| module.name.eq_ignore_ascii_case(&name))
            {
                report.error(format!(
                    "Point of Interest on '{node_name}' reuses unique module name '{name}'"
                ));
                return false;
            }
            if modifiers.is_empty() {
                report.error(format!(
                    "Point of Interest on '{node_name}' module '{name}' needs at least one stat modifier"
                ));
                return false;
            }
            let mut percentages = [0i16; psx_level::boost_stat::COUNT];
            for modifier in &modifiers {
                if !(-100..=500).contains(&modifier.percent) {
                    report.error(format!(
                        "Point of Interest on '{node_name}' module '{name}' has {}% outside the supported -100..500 range",
                        modifier.percent
                    ));
                    return false;
                }
                let lane = modifier.stat.runtime_index();
                percentages[lane] = percentages[lane].saturating_add(modifier.percent);
            }
            if percentages.iter().all(|percent| *percent == 0) {
                report.error(format!(
                    "Point of Interest on '{node_name}' module '{name}' has no non-zero stat effect"
                ));
                return false;
            }
            let effect_summary = boost_effect_summary(percentages);
            let index = boost_modules.len() as u16;
            boost_modules.push(PlaytestBoostModule {
                assignment_label: format!("ASSIGN {name}: CHOOSE SLOT"),
                remove_label: format!("REMOVE {name}"),
                name,
                description,
                name_it: reward.item_name_it.trim().to_string(),
                description_it: reward.description_it.trim().to_string(),
                effect_summary,
                percentages,
            });
            (index, 1)
        }
    };

    let message = messages.len().min(u16::MAX as usize) as u16;
    let page_first = message_pages.len() as u16;
    message_pages.extend(component.pages.iter().cloned());
    message_pages_it.extend(pages_it);
    let page_count = component.pages.len() as u16;
    messages.push(PlaytestInteractableMessage {
        title: String::new(),
        body: component.pages[0].clone(),
        page_first,
        page_count,
    });
    let mut flags = if component.enabled {
        psx_level::interactable_flags::ENABLED
    } else {
        0
    };
    if component.repeatable {
        flags |= psx_level::interactable_flags::REPEATABLE;
    }
    interactables.push(PlaytestInteractable {
        room: room_index,
        kind: PlaytestInteractableKind::PointOfInterest,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        radius: component.radius,
        marker_height: component.marker_height,
        prompt: point_of_interest_action_verb(component.prompt).to_string(),
        message,
        logic: psx_level::INTERACTABLE_LOGIC_NONE,
        checkpoint_id: component.persistence_id.to_string(),
        read_flag,
        reward_flag: if reward_resource == psx_level::POI_REWARD_NONE {
            psx_level::POI_FLAG_NONE
        } else {
            reward_flag
        },
        reward_resource,
        reward_quantity,
        flags,
    });
    true
}

fn legacy_boost_modifier(kind: crate::BoostModuleKind) -> crate::BoostStatModifier {
    let stat = match kind {
        crate::BoostModuleKind::Rupture => crate::BoostStatKind::HorizonAttack,
        crate::BoostModuleKind::Shell => crate::BoostStatKind::Defence,
        crate::BoostModuleKind::Surge => crate::BoostStatKind::MovementSpeed,
    };
    crate::BoostStatModifier { stat, percent: 10 }
}

fn boost_effect_summary(percentages: [i16; psx_level::boost_stat::COUNT]) -> String {
    const LABELS: [&str; psx_level::boost_stat::COUNT] =
        ["HRZ ATK", "ZTH ATK", "DEF", "MOVE", "ATK SPD", "REGEN"];
    let mut summary = String::new();
    for (index, percent) in percentages.iter().enumerate() {
        if *percent == 0 {
            continue;
        }
        if !summary.is_empty() {
            summary.push_str(" / ");
        }
        summary.push_str(LABELS[index]);
        summary.push(' ');
        if *percent > 0 {
            summary.push('+');
        }
        summary.push_str(&percent.to_string());
        summary.push('%');
    }
    summary
}

/// Font occupying runtime UI slot zero, which also draws Archive messages.
/// This mirrors `collect_ui_fonts`: UI scenes and their hierarchy rows retain
/// authored order, while projects without text fall back to Basic.
pub(crate) fn runtime_message_font(project: &ProjectDocument) -> UiFontChoice {
    project
        .ui_scenes
        .iter()
        .flat_map(|scene| {
            scene
                .hierarchy_node_ids()
                .into_iter()
                .map(move |id| (scene, id))
        })
        .find_map(|(scene, id)| match &scene.node(id)?.kind {
            UiNodeKind::Label { font, .. } | UiNodeKind::Button { font, .. } => Some(*font),
            _ => None,
        })
        .unwrap_or(UiFontChoice::Basic)
}

/// Canonicalize old POI prompt copy into the action verb expected by the
/// shared prompt panel. The panel owns the `X - ` control prefix; stripping
/// the exact legacy spelling preserves old scenes without producing
/// `X - X - READ` at runtime.
fn point_of_interest_action_verb(prompt: &str) -> &str {
    let prompt = non_empty_or(prompt, "READ").trim();
    let verb = prompt.strip_prefix("X -").map(str::trim).unwrap_or(prompt);
    if verb.is_empty() {
        "READ"
    } else {
        verb
    }
}

/// The Italian column for one message: one page per English page, each
/// validated like the English copy, or all-empty entries when no Italian was
/// authored (an editor-created list of blank pages counts as none). `None`
/// means the report already carries the error.
pub(crate) fn italian_pages(
    label: &str,
    pages: &[String],
    pages_it: &[String],
    max_lines: usize,
    font: UiFontChoice,
    report: &mut PlaytestValidationReport,
) -> Option<Vec<String>> {
    if pages_it.iter().all(|page| page.trim().is_empty()) {
        return Some(vec![String::new(); pages.len()]);
    }
    if pages_it.len() != pages.len() {
        report.error(format!(
            "{label} has {} Italian pages for {} English pages; they must match one to one",
            pages_it.len(),
            pages.len()
        ));
        return None;
    }
    if !validate_message_pages(
        &format!("{label} (Italian)"),
        pages_it,
        max_lines,
        font,
        report,
    ) {
        return None;
    }
    Some(pages_it.to_vec())
}

/// Validate authored pagination against the native 320x240 Archive panel.
/// Pages are explicit so no copy can disappear behind the runtime's bounded
/// two/three-line renderer. Wrapping uses the exact bitmap advances of the
/// same font slot and 244-pixel inner width as the runtime presentation.
pub(crate) fn validate_message_pages(
    label: &str,
    pages: &[String],
    max_lines: usize,
    font: UiFontChoice,
    report: &mut PlaytestValidationReport,
) -> bool {
    if pages.is_empty() {
        report.error(format!("{label} needs at least one message page"));
        return false;
    }
    for (page_index, page) in pages.iter().enumerate() {
        if page.trim().is_empty() {
            report.error(format!("{label} page {} is blank", page_index + 1));
            return false;
        }
        let visible_lines = wrapped_message_line_count(font.bitmap_font(), page, 244);
        if visible_lines > max_lines {
            report.error(format!(
                "{label} page {} wraps to {visible_lines} lines in {}; this panel supports {max_lines}. Split the copy into another page",
                page_index + 1,
                font.label(),
            ));
            return false;
        }
    }
    true
}

fn wrapped_message_line_count(font: &psx_font::BitmapFont, text: &str, width: u16) -> usize {
    let mut start = 0usize;
    let mut lines = 0usize;
    while start < text.len() {
        while matches!(text.as_bytes().get(start), Some(b' ' | b'\n' | b'\r')) {
            start += 1;
        }
        if start >= text.len() {
            break;
        }
        let end = wrapped_message_line_end(font, text, start, width);
        if end <= start {
            break;
        }
        lines += 1;
        start = end;
    }
    lines.max(1)
}

fn wrapped_message_line_end(
    font: &psx_font::BitmapFont,
    text: &str,
    start: usize,
    width: u16,
) -> usize {
    if start >= text.len() || !text.is_char_boundary(start) {
        return start;
    }
    let mut last_space = None;
    for (offset, ch) in text[start..].char_indices() {
        let end = start + offset;
        if ch == '\n' {
            return end;
        }
        let next = end + ch.len_utf8();
        if ch == ' ' {
            last_space = Some(end);
        }
        if next > start && font.text_width(&text[start..next]) > width {
            return last_space
                .filter(|space| *space > start)
                .unwrap_or(if end > start { end } else { next });
        }
    }
    text.len()
}

/// Door link waiting on the box-prop table: cooked while walking the
/// scene, resolved to a `BOX_PROPS` index after every Box Prop has
/// been pushed (a door may name a box that cooks later).
pub(crate) struct PendingDoorLink {
    /// Index into the cooked logic table.
    pub logic_index: usize,
    /// Authored Box Prop node name.
    pub box_prop: String,
    /// Door node name, for error text.
    pub node_name: String,
}

/// Cook one placed Logic node (trigger volume / relay / multisource /
/// door) into a [`PlaytestLogic`] record. Door box-prop links resolve
/// after the scene walk through `pending_door_links`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_logic_node(
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    kind: &crate::LogicNodeKind,
    target: &str,
    killtarget: &str,
    master: &str,
    delay_ticks: u16,
    wait_ticks: i16,
    enabled: bool,
    persistent: bool,
    names: &mut NameInterner,
    messages: &mut Vec<PlaytestInteractableMessage>,
    message_pages: &mut Vec<String>,
    message_pages_it: &mut Vec<String>,
    logic: &mut Vec<PlaytestLogic>,
    pending_door_links: &mut Vec<PendingDoorLink>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut record = PlaytestLogic {
        room: room_index,
        kind: psx_level::logic_kind::NONE,
        spawnflags: 0,
        targetname: names.intern(node_name),
        // The interner maps empty/whitespace to LOGIC_NAME_NONE.
        target: names.intern(target),
        killtarget: names.intern(killtarget),
        master: names.intern(master),
        delay_ticks,
        wait_ticks,
        arg0: 0,
        arg1: 0,
        link: psx_level::LOGIC_LINK_NONE,
        message: psx_level::INTERACTABLE_MESSAGE_NONE,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        min: [pos[0], pos[1], pos[2]],
        max: [pos[0], pos[1], pos[2]],
        flags: if enabled {
            psx_level::logic_flags::ENABLED
        } else {
            0
        },
    };
    if persistent {
        if !matches!(kind, crate::LogicNodeKind::Door { .. }) {
            report.error(format!(
                "'{node_name}' is marked persistent, but only Doors (and Switch \
                 interactables) can be saved"
            ));
            return false;
        }
        record.flags |= psx_level::logic_flags::PERSISTENT;
    }
    match kind {
        crate::LogicNodeKind::TriggerVolume { size } => {
            if size[0] == 0 || size[1] == 0 || size[2] == 0 {
                report.error(format!(
                    "Trigger Volume '{node_name}' has a zero-size extent \
                     (must be > 0 on every axis)"
                ));
                return false;
            }
            if record.target == psx_level::LOGIC_NAME_NONE
                && record.killtarget == psx_level::LOGIC_NAME_NONE
            {
                report.error(format!(
                    "Trigger Volume '{node_name}' has no target - a volume \
                     that fires nothing is dead content"
                ));
                return false;
            }
            record.kind = psx_level::logic_kind::TRIGGER_VOLUME;
            // Floor-anchored center: XZ centered on the node, Y up
            // from the anchor.
            let half_x = i32::from(size[0]) / 2;
            let half_z = i32::from(size[2]) / 2;
            record.min = [pos[0] - half_x, pos[1], pos[2] - half_z];
            record.max = [
                pos[0] + half_x,
                pos[1] + i32::from(size[1]),
                pos[2] + half_z,
            ];
        }
        crate::LogicNodeKind::Relay => {
            if record.target == psx_level::LOGIC_NAME_NONE
                && record.killtarget == psx_level::LOGIC_NAME_NONE
            {
                report.error(format!(
                    "Relay '{node_name}' has no target - a relay that fires \
                     nothing is dead content"
                ));
                return false;
            }
            record.kind = psx_level::logic_kind::RELAY;
        }
        crate::LogicNodeKind::Multisource { required } => {
            if *required == 0 {
                report.error(format!(
                    "Multisource '{node_name}' requires 0 inputs (must be > 0)"
                ));
                return false;
            }
            record.kind = psx_level::logic_kind::MULTISOURCE;
            record.arg0 = *required;
        }
        crate::LogicNodeKind::Door {
            box_prop,
            start_open,
            slide,
            lock,
            refusal,
        } => {
            if box_prop.trim().is_empty() {
                report.error(format!(
                    "Door '{node_name}' names no Box Prop - the door's link \
                     is the box it opens"
                ));
                return false;
            }
            record.kind = psx_level::logic_kind::DOOR;
            if let Some(slide) = slide {
                // Distances are already in engine units (the cook scales the
                // project first); the runtime keeps offsets as i16.
                if slide.distance == 0 || slide.ticks == 0 || slide.distance > i16::MAX as u16 {
                    report.error(format!(
                        "Door '{node_name}' has a slide of distance {} over {} ticks \
                         (both must be > 0, and the distance at most {} engine units)",
                        slide.distance,
                        slide.ticks,
                        i16::MAX
                    ));
                    return false;
                }
                record.spawnflags |= match slide.direction {
                    crate::DoorDirection::Up => psx_level::door_flags::DIR_UP,
                    crate::DoorDirection::Down => psx_level::door_flags::DIR_DOWN,
                    crate::DoorDirection::East => psx_level::door_flags::DIR_POS_X,
                    crate::DoorDirection::West => psx_level::door_flags::DIR_NEG_X,
                    crate::DoorDirection::South => psx_level::door_flags::DIR_POS_Z,
                    crate::DoorDirection::North => psx_level::door_flags::DIR_NEG_Z,
                };
                record.arg0 = slide.distance;
                record.arg1 = slide.ticks;
            }
            let default_refusal = match lock {
                crate::DoorLock::Unlocked => None,
                crate::DoorLock::Locked => {
                    record.spawnflags |= psx_level::door_flags::LOCKED;
                    Some("LOCKED.")
                }
                crate::DoorLock::OneWay { refuse_from } => {
                    record.spawnflags |= (match refuse_from {
                        crate::DoorSide::East => psx_level::door_flags::REFUSE_EAST,
                        crate::DoorSide::West => psx_level::door_flags::REFUSE_WEST,
                        crate::DoorSide::South => psx_level::door_flags::REFUSE_SOUTH,
                        crate::DoorSide::North => psx_level::door_flags::REFUSE_NORTH,
                    }) << psx_level::door_flags::REFUSE_SHIFT;
                    Some("IT WILL NOT OPEN FROM THIS SIDE.")
                }
            };
            match default_refusal {
                Some(stock) => {
                    record.message = push_single_page_message(
                        messages,
                        message_pages,
                        message_pages_it,
                        "DOOR".to_string(),
                        non_empty_or(refusal, stock).to_string(),
                    );
                }
                None if !refusal.trim().is_empty() => report.warn(format!(
                    "Door '{node_name}' is not locked, so its refusal text is never shown"
                )),
                None => {}
            }
            if *start_open {
                record.flags |= psx_level::logic_flags::START_ON;
            }
            pending_door_links.push(PendingDoorLink {
                logic_index: logic.len(),
                box_prop: box_prop.clone(),
                node_name: node_name.to_string(),
            });
        }
    }
    logic.push(record);
    true
}

/// Resolve every pending door link against the cooked box-prop name
/// table. Loud failures: an unresolved or ambiguous name is a cook
/// error, never a silently unlinked door.
pub(crate) fn resolve_door_links(
    pending: &[PendingDoorLink],
    box_prop_indices_by_name: &HashMap<String, Vec<u16>>,
    box_props: &[PlaytestBoxProp],
    logic: &mut [PlaytestLogic],
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut ok = true;
    for link in pending {
        match box_prop_indices_by_name
            .get(&link.box_prop)
            .map(Vec::as_slice)
        {
            Some([index]) => {
                if let Some(record) = logic.get_mut(link.logic_index) {
                    record.link = *index;
                    // The door's plane is its box's bottom-centre: the point
                    // the one-way side test compares the player against.
                    if let Some(prop) = box_props.get(usize::from(*index)) {
                        record.x = prop.x;
                        record.y = prop.y;
                        record.z = prop.z;
                    }
                }
            }
            Some(indices) => {
                report.error(format!(
                    "Door '{}' names Box Prop '{}', but {} placed boxes share \
                     that name - rename the box so the link is unambiguous",
                    link.node_name,
                    link.box_prop,
                    indices.len()
                ));
                ok = false;
            }
            None => {
                report.error(format!(
                    "Door '{}' names Box Prop '{}', which is not a placed \
                     Box Prop in any cooked room",
                    link.node_name, link.box_prop
                ));
                ok = false;
            }
        }
    }
    ok
}

/// Every switch must fire something that exists: a target naming no cooked
/// logic record is a cook error, never a lever that silently does nothing.
pub(crate) fn check_switch_targets(
    logic: &[PlaytestLogic],
    names: &NameInterner,
    report: &mut PlaytestValidationReport,
) -> bool {
    let mut ok = true;
    for (index, record) in logic.iter().enumerate() {
        if record.kind != psx_level::logic_kind::SWITCH {
            continue;
        }
        let resolves = logic
            .iter()
            .enumerate()
            .any(|(other, candidate)| other != index && candidate.targetname == record.target);
        // A persistent lever aimed at something that is not saved comes back
        // out of step with it after a death.
        if record.flags & psx_level::logic_flags::PERSISTENT != 0 {
            let unsaved = logic.iter().enumerate().any(|(other, candidate)| {
                other != index
                    && candidate.targetname == record.target
                    && candidate.flags & psx_level::logic_flags::PERSISTENT == 0
            });
            if unsaved {
                let switch = names.name_of(record.targetname).unwrap_or("?");
                let target = names.name_of(record.target).unwrap_or("?");
                report.warn(format!(
                    "Persistent switch '{switch}' targets '{target}', which is not \
                     persistent: after a death the lever and the target disagree"
                ));
            }
        }
        if !resolves {
            let switch = names.name_of(record.targetname).unwrap_or("?");
            let target = names.name_of(record.target).unwrap_or("?");
            report.error(format!(
                "Switch '{switch}' targets '{target}', which is not a placed \
                 Logic node (Door, Relay, ...) or another interactable"
            ));
            ok = false;
        }
    }
    ok
}

/// Give every persistent logic record its save slot, collision free.
///
/// `previous` is the project's stored table: a record keeps the slot it had
/// as long as it still exists and nothing else (a destructible or point of
/// interest, which hash into the same bitset) took it. A record without one
/// starts at the slot its stable id hashes to (so a project with no
/// collisions gets the slots the hash alone used to give, and saves written
/// then stay valid) and walks forward to the next free bit. More persistent
/// records than the save holds is a cook error. Returns the slot of every
/// logic record (`PERSISTENT_SLOT_NONE` for a transient one) and the table
/// to store back in the project.
pub(crate) fn assign_logic_persistence_slots(
    logic: &[PlaytestLogic],
    logic_ids: &[u32],
    previous: &[crate::PersistentSlot],
    names: &NameInterner,
    taken: &mut HashMap<u16, String>,
    report: &mut PlaytestValidationReport,
) -> (Vec<u16>, Vec<crate::PersistentSlot>) {
    let capacity = psx_level::POI_PERSISTENT_FLAG_CAPACITY;
    let mut slots = vec![psx_level::PERSISTENT_SLOT_NONE; logic.len()];
    let persistent = |index: usize| logic[index].flags & psx_level::logic_flags::PERSISTENT != 0;
    let owner_of = |index: usize| {
        format!(
            "logic '{}' (stable id {:#010x})",
            names.name_of(logic[index].targetname).unwrap_or("?"),
            logic_ids.get(index).copied().unwrap_or(0)
        )
    };
    // Pass 1: records that keep their stored slot.
    for index in (0..logic.len()).filter(|i| persistent(*i)) {
        let Some(id) = logic_ids.get(index) else {
            continue;
        };
        let Some(kept) = previous.iter().find(|entry| entry.id == *id) else {
            continue;
        };
        if usize::from(kept.slot) < capacity && !taken.contains_key(&kept.slot) {
            taken.insert(kept.slot, owner_of(index));
            slots[index] = kept.slot;
        }
    }
    // Pass 2: the rest, from their hash slot to the next free bit.
    let unplaced: Vec<usize> = (0..logic.len())
        .filter(|i| persistent(*i) && slots[*i] == psx_level::PERSISTENT_SLOT_NONE)
        .collect();
    for index in unplaced {
        let Some(id) = logic_ids.get(index) else {
            continue;
        };
        let start = usize::from(psx_level::persistent_flag_slot(psx_level::StableId(*id)));
        let free = (0..capacity)
            .map(|step| ((start + step) % capacity) as u16)
            .find(|slot| !taken.contains_key(slot));
        match free {
            Some(slot) => {
                taken.insert(slot, owner_of(index));
                slots[index] = slot;
            }
            None => {
                report.error(format!(
                    "{} needs a persistent save slot but all {capacity} are taken (doors, levers, \
                     breakables and points of interest share them); a world this size needs a \
                     larger save block",
                    owner_of(index)
                ));
                break;
            }
        }
    }
    let table = logic_ids
        .iter()
        .zip(&slots)
        .filter(|(_, slot)| **slot != psx_level::PERSISTENT_SLOT_NONE)
        .map(|(id, slot)| crate::PersistentSlot {
            id: *id,
            slot: *slot,
        })
        .collect();
    (slots, table)
}

/// Cook one souls-like game entity from a non-player Character
/// Controller that opted in with `EnemyBehaviorSettings`. `kind` is
/// the interned Character resource name so every placement of one
/// archetype shares the tag; the spawn is patrol anchor zero and
/// `patrol_offset` authors anchor one relative to it. Body radius/
/// height and walk/run speeds cook from the controller's effective
/// `CharacterControllerSettings` -- the same source the player motor
/// config uses -- so the runtime's movement is Character-bound (the
/// phase-3 seam note).
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_game_entity(
    node_name: &str,
    archetype_name: &str,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    settings: &crate::CharacterControllerSettings,
    enemy: crate::EnemyBehaviorSettings,
    model_instance: Option<u16>,
    state_clips: GameEntityStateClips,
    combat_capsule_first: u16,
    combat_capsule_count: u8,
    attack_active_ticks: u16,
    heavy_attack_active_ticks: u16,
    ranged_attack_active_ticks: u16,
    stagger_ticks: u16,
    projectile_attack_range: Option<(u16, u16)>,
    names: &mut NameInterner,
    game_entities: &mut Vec<PlaytestGameEntity>,
    report: &mut PlaytestValidationReport,
) -> bool {
    if enemy.aggro_radius == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has aggro radius 0 (must be > 0)"
        ));
        return false;
    }
    if enemy.windup_ticks == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has windup 0 ticks - a souls-like \
             attack must telegraph (must be > 0)"
        ));
        return false;
    }
    if enemy.preferred_distance == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has preferred distance 0 (must be > 0)"
        ));
        return false;
    }
    if enemy.spacing_tolerance > enemy.preferred_distance {
        report.error(format!(
            "Enemy on '{node_name}' has spacing tolerance larger than its preferred distance"
        ));
        return false;
    }
    if enemy.decision_interval_ticks == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has decision interval 0 ticks (must be > 0)"
        ));
        return false;
    }
    if enemy.circle_chance > 100 {
        report.error(format!(
            "Enemy on '{node_name}' has circle chance {} (must be 0..=100)",
            enemy.circle_chance
        ));
        return false;
    }
    if enemy.max_health == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has Horizon health 0 (must be > 0)"
        ));
        return false;
    }
    if enemy.max_health_secondary == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has Zenith health 0 (must be > 0)"
        ));
        return false;
    }
    // The same > 0 contract the player controller cook enforces:
    // entity movement divides through these at runtime.
    if settings.radius == 0 || settings.height == 0 {
        report.error(format!(
            "Enemy on '{node_name}' has a zero body radius/height - the \
             Character Controller capsule must be > 0"
        ));
        return false;
    }
    if settings.walk_speed <= 0 || settings.run_speed <= 0 {
        report.error(format!(
            "Enemy on '{node_name}' has non-positive walk/run speed - the \
             Character Controller speeds drive patrol and chase"
        ));
        return false;
    }
    let mut flags = psx_level::game_entity_flags::ENABLED;
    if enemy.training {
        flags |= psx_level::game_entity_flags::TRAINING;
    }
    if enemy.tactical {
        flags |= psx_level::game_entity_flags::TACTICAL;
    }
    if state_clips.run_supported {
        flags |= psx_level::game_entity_flags::CAN_RUN;
    }
    if projectile_attack_range.is_some() {
        flags |= psx_level::game_entity_flags::RANGED_ATTACK;
    }
    let (attack_min_range, attack_max_range) = projectile_attack_range.unwrap_or((0, 0));
    game_entities.push(PlaytestGameEntity {
        room: room_index,
        kind: names.intern(archetype_name),
        targetname: names.intern(node_name),
        model_instance: model_instance.unwrap_or(psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE),
        idle_clip: state_clips.idle,
        alert_clip: state_clips.alert,
        turn_clip: state_clips.turn,
        walk_clip: state_clips.walk,
        walk_backward_clip: state_clips.walk_backward,
        strafe_left_clip: state_clips.strafe_left,
        strafe_right_clip: state_clips.strafe_right,
        run_clip: state_clips.run,
        attack_clip: state_clips.attack,
        attack_speed_q8: state_clips.attack_speed_q8,
        attack_frame_range: state_clips.attack_frame_range,
        heavy_attack_clip: state_clips.heavy_attack,
        heavy_attack_speed_q8: state_clips.heavy_attack_speed_q8,
        heavy_attack_frame_range: state_clips.heavy_attack_frame_range,
        ranged_attack_clip: state_clips.ranged_attack,
        ranged_attack_speed_q8: state_clips.ranged_attack_speed_q8,
        ranged_attack_frame_range: state_clips.ranged_attack_frame_range,
        stagger_clip: state_clips.stagger,
        stagger_speed_q8: state_clips.stagger_speed_q8,
        stagger_frame_range: state_clips.stagger_frame_range,
        stagger_ticks,
        death_clip: state_clips.death,
        combat_capsule_first,
        combat_capsule_count,
        ranged_attack_action: state_clips.ranged_attack_action,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        radius: settings.radius,
        height: settings.height,
        // Enemy movement is whole units per tick; settings arrive in Q8.
        walk_speed: (settings.walk_speed >> 8).max(1),
        run_speed: (settings.run_speed >> 8).max(1),
        patrol: [
            pos[0].saturating_add(enemy.patrol_offset[0]),
            pos[1].saturating_add(enemy.patrol_offset[1]),
            pos[2].saturating_add(enemy.patrol_offset[2]),
        ],
        patrol_wait_ticks: enemy.patrol_wait_ticks,
        aggro_radius: enemy.aggro_radius,
        reaction_ticks: enemy.reaction_ticks,
        preferred_distance: enemy.preferred_distance,
        spacing_tolerance: enemy.spacing_tolerance,
        spacing_speed_percent: enemy.spacing_speed_percent.clamp(1, 100),
        decision_interval_ticks: enemy.decision_interval_ticks,
        circle_chance: enemy.circle_chance,
        attack_priority: enemy.attack_priority,
        attack_cooldown_ticks: enemy.attack_cooldown_ticks,
        group_attack_delay_ticks: enemy.group_attack_delay_ticks,
        windup_ticks: enemy.windup_ticks,
        attack_active_ticks,
        heavy_attack_active_ticks,
        ranged_attack_active_ticks,
        recovery_ticks: enemy.recovery_ticks,
        attack_min_range,
        attack_max_range,
        poise: enemy.poise,
        touch_damage: enemy.touch_damage,
        max_health: enemy.max_health,
        max_health_secondary: enemy.max_health_secondary,
        soul_value: enemy.soul_value,
        flags,
    });
    true
}

pub(crate) fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

pub(crate) fn default_prompt_for_interactable(kind: &crate::InteractableKind) -> &'static str {
    match kind {
        crate::InteractableKind::Message { .. } => "READ ECHO",
        crate::InteractableKind::Checkpoint { .. } => "SYNCHRONIZE",
        crate::InteractableKind::Switch { .. } => "USE",
    }
}

fn box_prop_surface_center(vertices: [[i32; 3]; 4]) -> [i32; 3] {
    polygon_surface_center(vertices, 4)
}

fn polygon_surface_center(vertices: [[i32; 3]; 4], vertex_count: usize) -> [i32; 3] {
    let count = vertex_count.clamp(1, 4);
    std::array::from_fn(|axis| {
        vertices[..count]
            .iter()
            .map(|vertex| i64::from(vertex[axis]))
            .sum::<i64>()
            .div_euclid(count as i64) as i32
    })
}

fn box_prop_surface_normal(vertices: [[i32; 3]; 4]) -> [i32; 3] {
    let ab: [i64; 3] =
        std::array::from_fn(|axis| i64::from(vertices[1][axis]) - i64::from(vertices[0][axis]));
    let ac: [i64; 3] =
        std::array::from_fn(|axis| i64::from(vertices[2][axis]) - i64::from(vertices[0][axis]));
    [
        ((ab[1] * ac[2] - ab[2] * ac[1]) >> 10).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
        ((ab[2] * ac[0] - ab[0] * ac[2]) >> 10).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
        ((ab[0] * ac[1] - ab[1] * ac[0]) >> 10).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
    ]
}

pub(crate) const fn rgb_tuple(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0], rgb[1], rgb[2])
}

pub(crate) const fn particle_blend_mode_code(mode: PsxBlendMode) -> u8 {
    match mode {
        PsxBlendMode::Opaque | PsxBlendMode::Average => 0,
        PsxBlendMode::Add => 1,
        PsxBlendMode::Subtract => 2,
        PsxBlendMode::AddQuarter => 3,
    }
}

#[cfg(test)]
mod prop_uv_tests {
    use super::bake_prop_uv;

    fn legacy_runtime_uv(corners: [(u8, u8); 4], uv_q8: [u8; 2]) -> [u8; 2] {
        let lerp = |a: u8, b: u8, t: u8| {
            let t = u32::from(t);
            ((u32::from(a) * (255 - t) + u32::from(b) * t + 127) / 255).min(255) as u8
        };
        let [u, v] = uv_q8;
        if v == 0 {
            return [
                lerp(corners[0].0, corners[1].0, u),
                lerp(corners[0].1, corners[1].1, u),
            ];
        }
        if v == 255 {
            return [
                lerp(corners[3].0, corners[2].0, u),
                lerp(corners[3].1, corners[2].1, u),
            ];
        }
        if u == 0 {
            return [
                lerp(corners[0].0, corners[3].0, v),
                lerp(corners[0].1, corners[3].1, v),
            ];
        }
        if u == 255 {
            return [
                lerp(corners[1].0, corners[2].0, v),
                lerp(corners[1].1, corners[2].1, v),
            ];
        }

        let u = u32::from(u);
        let v = u32::from(v);
        let inv_u = 255 - u;
        let inv_v = 255 - v;
        let axis = |values: [u8; 4]| {
            let top = u32::from(values[0]) * inv_u + u32::from(values[1]) * u;
            let bottom = u32::from(values[3]) * inv_u + u32::from(values[2]) * u;
            ((top * inv_v + bottom * v + 32_512) / 65_025).min(255) as u8
        };
        [
            axis([corners[0].0, corners[1].0, corners[2].0, corners[3].0]),
            axis([corners[0].1, corners[1].1, corners[2].1, corners[3].1]),
        ]
    }

    #[test]
    fn cooked_prop_uv_matches_legacy_runtime_for_every_coordinate() {
        let corner_sets = [
            [(0, 0), (255, 0), (255, 255), (0, 255)],
            [(17, 231), (244, 7), (193, 161), (58, 99)],
            [(255, 255), (0, 255), (0, 0), (255, 0)],
            [(3, 251), (127, 129), (239, 11), (61, 197)],
        ];
        for corners in corner_sets {
            for u in 0..=255u8 {
                for v in 0..=255u8 {
                    let uv = [u, v];
                    assert_eq!(
                        bake_prop_uv(corners, uv),
                        legacy_runtime_uv(corners, uv),
                        "corners={corners:?} uv={uv:?}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod message_page_tests {
    use super::*;

    #[test]
    fn archive_pages_reject_blank_overlong_and_overflowing_copy() {
        let mut report = PlaytestValidationReport::default();
        assert!(!validate_message_pages(
            "POI",
            &["OK".to_string(), "   ".to_string()],
            2,
            UiFontChoice::Basic,
            &mut report,
        ));

        let mut report = PlaytestValidationReport::default();
        assert!(!validate_message_pages(
            "POI",
            &["1234567890123456789012345678901234567890123456789012345678901".to_string()],
            2,
            UiFontChoice::Basic,
            &mut report,
        ));

        let mut report = PlaytestValidationReport::default();
        assert!(!validate_message_pages(
            "POI",
            &["ONE\nTWO\nTHREE".to_string()],
            2,
            UiFontChoice::Basic,
            &mut report,
        ));

        let mut report = PlaytestValidationReport::default();
        assert!(validate_message_pages(
            "World",
            &["ONE\nTWO\nTHREE".to_string()],
            3,
            UiFontChoice::Basic,
            &mut report,
        ));
    }

    #[test]
    fn archive_page_validation_uses_the_runtime_font_width() {
        let page = vec!["123456789012345678901234567890123456789".to_string()];

        let mut narrow_report = PlaytestValidationReport::default();
        assert!(validate_message_pages(
            "POI",
            &page,
            1,
            UiFontChoice::Spleen5x8,
            &mut narrow_report,
        ));

        let mut wide_report = PlaytestValidationReport::default();
        assert!(!validate_message_pages(
            "POI",
            &page,
            1,
            UiFontChoice::Basic,
            &mut wide_report,
        ));
    }

    #[test]
    fn point_of_interest_prompt_is_always_a_verb() {
        assert_eq!(point_of_interest_action_verb("READ"), "READ");
        assert_eq!(point_of_interest_action_verb("TAKE"), "TAKE");
        assert_eq!(point_of_interest_action_verb("X - TAKE"), "TAKE");
        assert_eq!(point_of_interest_action_verb(" X - READ "), "READ");
        assert_eq!(point_of_interest_action_verb("X - "), "READ");
        assert_eq!(point_of_interest_action_verb("  "), "READ");
    }

    #[test]
    fn world_registry_links_image_destructible_and_cooks_beacon_bounds() {
        let image = PlaytestImageProp {
            room: 0,
            texture_asset_index: 0,
            x: 10,
            y: 20,
            z: 30,
            pitch: 0,
            yaw: 0,
            roll: 0,
            width: 16,
            height: 24,
            tint_rgb: [128; 3],
            baked_vertex_rgb: [(128, 128, 128); 4],
            collision_min: [2, 20, 29],
            collision_max: [18, 44, 31],
            destructible: 3,
            flags: image_prop_flags::COLLISION_ENABLED,
        };
        let poi = PlaytestInteractable {
            room: 0,
            kind: PlaytestInteractableKind::PointOfInterest,
            x: 50,
            y: 60,
            z: 70,
            yaw: 0,
            radius: 32,
            marker_height: 12,
            prompt: "READ".to_string(),
            message: 0,
            logic: psx_level::INTERACTABLE_LOGIC_NONE,
            checkpoint_id: String::new(),
            read_flag: 0,
            reward_flag: 1,
            reward_resource: psx_level::POI_REWARD_NONE,
            reward_quantity: 0,
            flags: psx_level::interactable_flags::ENABLED,
        };
        let mut report = PlaytestValidationReport::default();
        let objects = cook_world_objects(&[image], &[], &[], &[], &[], &[poi], &mut report);
        assert!(report.is_ok());
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0].kind, psx_level::world_object_kind::IMAGE_PROP);
        assert_eq!(objects[0].destructible, 3);
        assert_ne!(
            objects[0].flags & psx_level::world_object_flags::COLLIDABLE,
            0
        );
        assert_eq!(
            objects[1].kind,
            psx_level::world_object_kind::POINT_OF_INTEREST_BEACON
        );
        assert!(objects[1].bounds_min[0] < 50 && objects[1].bounds_max[0] > 50);
        assert!(objects[1].bounds_min[1] < 60 && objects[1].bounds_max[1] > 60);
    }
}

#[cfg(test)]
mod switch_cook_tests {
    use super::*;

    fn cook_switch(
        kind: crate::InteractableKind,
        names: &mut NameInterner,
        logic: &mut Vec<PlaytestLogic>,
        report: &mut PlaytestValidationReport,
    ) -> bool {
        let mut messages = Vec::new();
        let mut pages = Vec::new();
        let mut pages_it = Vec::new();
        let mut interactables = Vec::new();
        push_interactable(
            "lever",
            0,
            [0, 0, 0],
            0,
            InteractableComponent {
                kind: &kind,
                prompt: "PULL",
                radius: 128,
                enabled: true,
            },
            names,
            &mut messages,
            &mut pages,
            &mut pages_it,
            &mut interactables,
            logic,
            report,
        ) && messages.is_empty()
            && interactables[0].message == psx_level::INTERACTABLE_MESSAGE_NONE
    }

    #[test]
    fn switch_cooks_to_a_target_firing_record_without_a_message() {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        let toggle = crate::InteractableKind::Switch {
            target: "gate".into(),
            behavior: crate::SwitchBehavior::Toggle,
            unlocks: false,
            persistent: false,
        };
        assert!(cook_switch(toggle, &mut names, &mut logic, &mut report));
        let one_shot = crate::InteractableKind::Switch {
            target: "gate".into(),
            behavior: crate::SwitchBehavior::OneShot,
            unlocks: false,
            persistent: false,
        };
        assert!(cook_switch(one_shot, &mut names, &mut logic, &mut report));
        assert_eq!(logic[0].kind, psx_level::logic_kind::SWITCH);
        assert_eq!(logic[0].target, names.intern("gate"));
        assert_eq!(logic[0].wait_ticks, SWITCH_TOGGLE_COOLDOWN_TICKS);
        assert_eq!(logic[1].wait_ticks, -1);
    }

    #[test]
    fn switch_without_a_target_is_a_cook_error() {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        let kind = crate::InteractableKind::Switch {
            target: "  ".into(),
            behavior: crate::SwitchBehavior::Toggle,
            unlocks: false,
            persistent: false,
        };
        assert!(!cook_switch(kind, &mut names, &mut logic, &mut report));
        assert!(!report.is_ok());
        assert!(logic.is_empty());
    }

    #[test]
    fn switch_target_must_name_a_cooked_record() {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        let kind = crate::InteractableKind::Switch {
            target: "ghost".into(),
            behavior: crate::SwitchBehavior::Toggle,
            unlocks: false,
            persistent: false,
        };
        assert!(cook_switch(kind, &mut names, &mut logic, &mut report));
        assert!(!check_switch_targets(&logic, &names, &mut report));
        assert!(report.errors.iter().any(|e| e.contains("'ghost'")));

        // A placed Logic node of that name satisfies it.
        let mut report = PlaytestValidationReport::default();
        let mut pending = Vec::new();
        assert!(push_logic_node(
            "ghost",
            0,
            [0, 0, 0],
            &crate::LogicNodeKind::Relay,
            "lever",
            "",
            "",
            0,
            0,
            true,
            false,
            &mut names,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &mut logic,
            &mut pending,
            &mut report,
        ));
        assert!(check_switch_targets(&logic, &names, &mut report));
    }

    #[test]
    fn existing_interactables_still_parse_and_the_switch_reads_plainly() {
        let old: crate::InteractableKind =
            ron::from_str(r#"Message(title: "T", body: "B")"#).unwrap();
        assert!(matches!(old, crate::InteractableKind::Message { .. }));
        let lever: crate::InteractableKind =
            ron::from_str(r#"Switch(target: "gate_door", behavior: OneShot)"#).unwrap();
        assert_eq!(
            lever,
            crate::InteractableKind::Switch {
                target: "gate_door".into(),
                behavior: crate::SwitchBehavior::OneShot,
                unlocks: false,
                persistent: false,
            }
        );
        let bare: crate::InteractableKind = ron::from_str("Switch(target: \"d\")").unwrap();
        assert!(matches!(
            bare,
            crate::InteractableKind::Switch {
                behavior: crate::SwitchBehavior::Toggle,
                ..
            }
        ));
    }
}

#[cfg(test)]
mod door_cook_tests {
    use super::*;

    struct Cooked {
        record: Option<PlaytestLogic>,
        messages: Vec<PlaytestInteractableMessage>,
        report: PlaytestValidationReport,
        ok: bool,
    }

    fn cook_door(slide: Option<crate::DoorSlide>, lock: crate::DoorLock, refusal: &str) -> Cooked {
        let kind = crate::LogicNodeKind::Door {
            box_prop: "gate box".into(),
            start_open: false,
            slide,
            lock,
            refusal: refusal.into(),
        };
        let mut names = NameInterner::default();
        let (mut messages, mut pages, mut pages_it) = (Vec::new(), Vec::new(), Vec::new());
        let mut logic = Vec::new();
        let mut pending = Vec::new();
        let mut report = PlaytestValidationReport::default();
        let ok = push_logic_node(
            "gate",
            0,
            [0, 0, 0],
            &kind,
            "",
            "",
            "",
            0,
            0,
            true,
            false,
            &mut names,
            &mut messages,
            &mut pages,
            &mut pages_it,
            &mut logic,
            &mut pending,
            &mut report,
        );
        Cooked {
            record: logic.pop(),
            messages,
            report,
            ok,
        }
    }

    #[test]
    fn a_door_with_no_new_fields_cooks_exactly_as_before() {
        let cooked = cook_door(None, crate::DoorLock::Unlocked, "");
        assert!(cooked.ok);
        let record = cooked.record.unwrap();
        assert_eq!(record.kind, psx_level::logic_kind::DOOR);
        assert_eq!((record.spawnflags, record.arg0, record.arg1), (0, 0, 0));
        assert_eq!(record.message, psx_level::INTERACTABLE_MESSAGE_NONE);
        assert!(cooked.messages.is_empty());
    }

    #[test]
    fn a_slide_cooks_to_direction_distance_and_ticks() {
        let slide = crate::DoorSlide {
            direction: crate::DoorDirection::North,
            distance: 160,
            ticks: 90,
        };
        let cooked = cook_door(Some(slide), crate::DoorLock::Unlocked, "");
        assert!(cooked.ok);
        let record = cooked.record.unwrap();
        assert_eq!(record.spawnflags, psx_level::door_flags::DIR_NEG_Z);
        assert_eq!((record.arg0, record.arg1), (160, 90));
    }

    #[test]
    fn a_slide_of_nothing_or_too_far_is_a_cook_error() {
        for (distance, ticks) in [(0, 30), (80, 0), (u16::MAX, 30)] {
            let slide = crate::DoorSlide {
                direction: crate::DoorDirection::Up,
                distance,
                ticks,
            };
            let cooked = cook_door(Some(slide), crate::DoorLock::Unlocked, "");
            assert!(!cooked.ok && !cooked.report.is_ok(), "{distance} {ticks}");
        }
    }

    #[test]
    fn locks_cook_to_flags_and_a_refusal_message() {
        let locked = cook_door(None, crate::DoorLock::Locked, "");
        assert!(locked.ok);
        let record = locked.record.unwrap();
        assert_eq!(record.spawnflags, psx_level::door_flags::LOCKED);
        assert_eq!(locked.messages.len(), 1);
        assert_eq!(record.message, 0);
        assert_eq!(locked.messages[0].body, "LOCKED.");

        let one_way = cook_door(
            None,
            crate::DoorLock::OneWay {
                refuse_from: crate::DoorSide::West,
            },
            "Barred from this side.",
        );
        assert_eq!(
            one_way.record.unwrap().spawnflags,
            psx_level::door_flags::REFUSE_WEST << psx_level::door_flags::REFUSE_SHIFT
        );
        assert_eq!(one_way.messages[0].body, "Barred from this side.");
    }

    #[test]
    fn refusal_text_on_an_unlocked_door_warns() {
        let cooked = cook_door(None, crate::DoorLock::Unlocked, "never shown");
        assert!(cooked.ok && cooked.report.is_ok());
        assert!(!cooked.report.warnings.is_empty());
        assert!(cooked.messages.is_empty());
    }

    #[test]
    fn a_door_takes_its_plane_from_the_linked_box() {
        let mut cooked = cook_door(None, crate::DoorLock::Unlocked, "");
        let pending = [PendingDoorLink {
            logic_index: 0,
            box_prop: "gate box".into(),
            node_name: "gate".into(),
        }];
        let mut by_name = HashMap::new();
        by_name.insert("gate box".to_string(), vec![0u16]);
        let mut prop = test_box_prop();
        prop.x = 640;
        prop.y = 0;
        prop.z = 1920;
        let mut logic = vec![cooked.record.unwrap()];
        assert!(resolve_door_links(
            &pending,
            &by_name,
            &[prop],
            &mut logic,
            &mut cooked.report,
        ));
        assert_eq!((logic[0].x, logic[0].z, logic[0].link), (640, 1920, 0));
    }

    fn test_box_prop() -> PlaytestBoxProp {
        PlaytestBoxProp {
            room: 0,
            texture_asset_indices: [None; psx_level::BOX_PROP_FACE_COUNT],
            blend_modes: [0; psx_level::BOX_PROP_FACE_COUNT],
            uvs: [[(0, 0); 4]; psx_level::BOX_PROP_FACE_COUNT],
            x: 0,
            y: 0,
            z: 0,
            ground_y: 0,
            pitch: 0,
            yaw: 0,
            roll: 0,
            vertices: [[0; 3]; psx_level::BOX_PROP_VERTEX_COUNT],
            collision_min: [0; 3],
            collision_max: [0; 3],
            surface_first: 0,
            surface_count: 0,
            tint_rgb: [[0x80; 3]; psx_level::BOX_PROP_FACE_COUNT],
            baked_vertex_rgb: [[(0x80, 0x80, 0x80); 4]; psx_level::BOX_PROP_FACE_COUNT],
            flags: 0,
        }
    }

    #[test]
    fn the_authored_door_reads_plainly_and_scales_with_the_cook() {
        let kind: crate::LogicNodeKind = ron::from_str(
            r#"Door(box_prop: "gate", slide: Some((direction: Up, distance: 2560, ticks: 90)),
                    lock: OneWay(refuse_from: West), refusal: "No.")"#,
        )
        .unwrap();
        let crate::LogicNodeKind::Door {
            slide: Some(slide),
            lock,
            ..
        } = &kind
        else {
            panic!("a door with a slide");
        };
        assert_eq!(slide.distance, 2560);
        assert_eq!(
            *lock,
            crate::DoorLock::OneWay {
                refuse_from: crate::DoorSide::West
            }
        );

        let mut project = crate::ProjectDocument::new("units");
        let scene = project.active_scene_mut();
        let node = scene.add_node(
            scene.root,
            "gate",
            crate::NodeKind::Logic {
                kind,
                target: String::new(),
                killtarget: String::new(),
                master: String::new(),
                delay_ticks: 0,
                wait_ticks: 0,
                enabled: true,
                persistent: false,
            },
        );
        crate::units::scale_project_to_engine_units(&mut project);
        let scaled = project.active_scene().node(node).unwrap();
        let crate::NodeKind::Logic {
            kind: crate::LogicNodeKind::Door {
                slide: Some(slide), ..
            },
            ..
        } = &scaled.kind
        else {
            panic!("still a door");
        };
        assert_eq!(slide.distance, 160, "editor units / 16, like every length");
        assert_eq!(slide.ticks, 90, "ticks do not scale");
    }
}

#[cfg(test)]
mod persistence_cook_tests {
    use super::*;

    fn push_node(
        name: &str,
        kind: &crate::LogicNodeKind,
        persistent: bool,
        names: &mut NameInterner,
        logic: &mut Vec<PlaytestLogic>,
        report: &mut PlaytestValidationReport,
    ) -> bool {
        push_logic_node(
            name,
            0,
            [0, 0, 0],
            kind,
            if matches!(kind, crate::LogicNodeKind::Relay) {
                "x"
            } else {
                ""
            },
            "",
            "",
            0,
            0,
            true,
            persistent,
            names,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            logic,
            &mut Vec::new(),
            report,
        )
    }

    fn door() -> crate::LogicNodeKind {
        crate::LogicNodeKind::Door {
            box_prop: "box".into(),
            start_open: false,
            slide: None,
            lock: crate::DoorLock::Unlocked,
            refusal: String::new(),
        }
    }

    #[test]
    fn a_persistent_door_is_flagged_and_other_kinds_are_refused() {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        assert!(push_node(
            "gate",
            &door(),
            true,
            &mut names,
            &mut logic,
            &mut report
        ));
        assert_ne!(logic[0].flags & psx_level::logic_flags::PERSISTENT, 0);
        assert!(push_node(
            "plain",
            &door(),
            false,
            &mut names,
            &mut logic,
            &mut report
        ));
        assert_eq!(logic[1].flags & psx_level::logic_flags::PERSISTENT, 0);
        assert!(report.is_ok());

        let relay = crate::LogicNodeKind::Relay;
        assert!(!push_node(
            "relay",
            &relay,
            true,
            &mut names,
            &mut logic,
            &mut report
        ));
        assert!(report.errors.iter().any(|e| e.contains("'relay'")));
    }

    #[test]
    fn a_persistent_switch_cooks_with_the_flag() {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        let lever = |persistent| crate::InteractableKind::Switch {
            target: "gate".into(),
            behavior: crate::SwitchBehavior::OneShot,
            unlocks: false,
            persistent,
        };
        for kind in [lever(true), lever(false)] {
            let (mut messages, mut pages, mut pages_it) = (Vec::new(), Vec::new(), Vec::new());
            assert!(push_interactable(
                "lever",
                0,
                [0; 3],
                0,
                InteractableComponent {
                    kind: &kind,
                    prompt: "PULL",
                    radius: 8,
                    enabled: true,
                },
                &mut names,
                &mut messages,
                &mut pages,
                &mut pages_it,
                &mut Vec::new(),
                &mut logic,
                &mut report,
            ));
        }
        assert_ne!(logic[0].flags & psx_level::logic_flags::PERSISTENT, 0);
        assert_eq!(logic[1].flags & psx_level::logic_flags::PERSISTENT, 0);
        assert_ne!(logic[1].flags & psx_level::logic_flags::ENABLED, 0);
    }

    fn two_doors() -> (NameInterner, Vec<PlaytestLogic>) {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        for name in ["gate a", "gate b"] {
            assert!(push_node(
                name,
                &door(),
                true,
                &mut names,
                &mut logic,
                &mut report
            ));
        }
        (names, logic)
    }

    #[test]
    fn colliding_hashes_get_distinct_slots_instead_of_an_error() {
        let (names, logic) = two_doors();
        // Two ids 832 apart hash to one slot: the second walks to the next.
        let ids = [7, 7 + psx_level::POI_PERSISTENT_FLAG_CAPACITY as u32];
        let mut report = PlaytestValidationReport::default();
        let (slots, table) = assign_logic_persistence_slots(
            &logic,
            &ids,
            &[],
            &names,
            &mut HashMap::new(),
            &mut report,
        );
        assert!(report.is_ok(), "{:?}", report.error_messages());
        assert_eq!(slots, [7, 8]);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn a_stored_slot_survives_new_records_and_a_reordered_table() {
        let (names, logic) = two_doors();
        let mut report = PlaytestValidationReport::default();
        // The project stored gate a (id 7) at slot 100, off its hash slot.
        let stored = [crate::PersistentSlot { id: 7, slot: 100 }];
        let (slots, table) = assign_logic_persistence_slots(
            &logic,
            &[7, 8],
            &stored,
            &names,
            &mut HashMap::new(),
            &mut report,
        );
        assert_eq!(slots, [100, 8], "stored slot kept, the new door hashes");
        // Recook with the records swapped and the table fed back.
        let mut swapped = logic.clone();
        swapped.swap(0, 1);
        let (slots2, _) = assign_logic_persistence_slots(
            &swapped,
            &[8, 7],
            &table,
            &names,
            &mut HashMap::new(),
            &mut report,
        );
        assert_eq!(slots2, [8, 100]);
        assert!(report.is_ok());
    }

    #[test]
    fn a_slot_owned_by_another_kind_is_skipped_and_a_full_save_is_an_error() {
        let (names, logic) = two_doors();
        let mut report = PlaytestValidationReport::default();
        let mut taken = HashMap::new();
        taken.insert(7u16, "destructible 'wall'".to_string());
        let (slots, _) = assign_logic_persistence_slots(
            &logic,
            &[7, 8],
            &[crate::PersistentSlot { id: 7, slot: 7 }],
            &names,
            &mut taken,
            &mut report,
        );
        assert_eq!(slots, [8, 9], "the stored slot was taken, so both moved on");
        // Every slot taken: a clear error naming the record.
        let mut full: HashMap<u16, String> = (0..psx_level::POI_PERSISTENT_FLAG_CAPACITY as u16)
            .map(|slot| (slot, "other".to_string()))
            .collect();
        let mut report = PlaytestValidationReport::default();
        assign_logic_persistence_slots(&logic, &[7, 8], &[], &names, &mut full, &mut report);
        let message = report.error_messages().join(" ");
        assert!(
            message.contains("gate a") && message.contains("all 832"),
            "{message}"
        );
    }

    #[test]
    fn a_persistent_lever_at_an_unsaved_door_warns() {
        let mut names = NameInterner::default();
        let mut logic = Vec::new();
        let mut report = PlaytestValidationReport::default();
        assert!(push_node(
            "gate",
            &door(),
            false,
            &mut names,
            &mut logic,
            &mut report
        ));
        let kind = crate::InteractableKind::Switch {
            target: "gate".into(),
            behavior: crate::SwitchBehavior::Toggle,
            unlocks: false,
            persistent: true,
        };
        let (mut messages, mut pages, mut pages_it) = (Vec::new(), Vec::new(), Vec::new());
        assert!(push_interactable(
            "lever",
            0,
            [0; 3],
            0,
            InteractableComponent {
                kind: &kind,
                prompt: "PULL",
                radius: 8,
                enabled: true,
            },
            &mut names,
            &mut messages,
            &mut pages,
            &mut pages_it,
            &mut Vec::new(),
            &mut logic,
            &mut report,
        ));
        assert!(check_switch_targets(&logic, &names, &mut report));
        assert!(report.warnings.iter().any(|w| w.contains("not persistent")));
    }
}
