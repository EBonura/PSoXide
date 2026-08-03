use super::*;
use crate::generate_material_texture_psxt;

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
            reflection_probe: (layer.texture_mode == crate::MaterialTextureMode::ReflectiveProbe)
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
        reflection_probe: (material.texture_mode == crate::MaterialTextureMode::ReflectiveProbe)
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
        flags: if cylindrical_billboard {
            image_prop_flags::CYLINDRICAL_BILLBOARD
        } else {
            0
        },
    });
    true
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
    uvs: &[crate::GridUvTransform; crate::BOX_PROP_FACE_COUNT],
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
            let u_max = texture.width().saturating_sub(1).min(255) as u8;
            let v_max = texture.height().saturating_sub(1).min(255) as u8;
            cooked_uvs[face] =
                uvs[face].apply_to_quad([(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)]);
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
        let mut world_vertices = [[0i32; 3]; 4];
        for (index, local) in quad.vertices.iter().enumerate() {
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
            world_vertices[index] = [
                pos[0].saturating_add(rotated[0]),
                pos[1].saturating_add(rotated[1]),
                pos[2].saturating_add(rotated[2]),
            ];
        }
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
        surface_first: surface_first as u16,
        surface_count: (box_prop_surfaces.len() - surface_first) as u16,
        tint_rgb,
        baked_vertex_rgb,
        flags,
    });
    true
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
    uvs: &[crate::GridUvTransform; crate::CYLINDER_PROP_MATERIAL_COUNT],
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
            let u_max = texture.width().saturating_sub(1).min(255) as u8;
            let v_max = texture.height().saturating_sub(1).min(255) as u8;
            cooked_uvs[slot] =
                uvs[slot].apply_to_quad([(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)]);
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
        let mut world_vertices = [[0i32; 3]; 4];
        for (index, local) in surface.vertices.iter().enumerate() {
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
            world_vertices[index] = [
                pos[0].saturating_add(rotated[0]),
                pos[1].saturating_add(rotated[1]),
                pos[2].saturating_add(rotated[2]),
            ];
            if index < vertex_count {
                for axis in 0..3 {
                    bounds_min[axis] = bounds_min[axis].min(world_vertices[index][axis]);
                    bounds_max[axis] = bounds_max[axis].max(world_vertices[index][axis]);
                }
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

fn bake_prop_uv(corners: [(u8, u8); 4], uv_q8: [u8; 2]) -> [u8; 2] {
    let u = u32::from(uv_q8[0]);
    let v = u32::from(uv_q8[1]);
    let inv_u = 255 - u;
    let inv_v = 255 - v;
    let interpolate = |axis: usize| {
        let values = if axis == 0 {
            [
                u32::from(corners[0].0),
                u32::from(corners[1].0),
                u32::from(corners[2].0),
                u32::from(corners[3].0),
            ]
        } else {
            [
                u32::from(corners[0].1),
                u32::from(corners[1].1),
                u32::from(corners[2].1),
                u32::from(corners[3].1),
            ]
        };
        let top = values[0] * inv_u + values[1] * u;
        let bottom = values[3] * inv_u + values[2] * u;
        ((top * inv_v + bottom * v + 32_512) / 65_025).min(255) as u8
    };
    [interpolate(0), interpolate(1)]
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
    uvs: &[crate::GridUvTransform; crate::ARCH_PROP_MATERIAL_COUNT],
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
            let u_max = texture.width().saturating_sub(1).min(255) as u8;
            let v_max = texture.height().saturating_sub(1).min(255) as u8;
            cooked_uvs[slot] =
                uvs[slot].apply_to_quad([(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)]);
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

    let generated = crate::generate_arch_prop_surfaces(geometry, sector_size);
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
        for collision in crate::generate_arch_prop_collision_boxes(geometry, sector_size) {
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
    grid: &crate::WorldGrid,
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
    if radius <= 0.0 {
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
    let radius_world = spatial::light_radius_record_units(grid, radius);
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

pub(crate) fn push_interactable(
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    component: InteractableComponent<'_>,
    names: &mut NameInterner,
    messages: &mut Vec<PlaytestInteractableMessage>,
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
    };
    let message = messages.len().min(u16::MAX as usize) as u16;
    messages.push(PlaytestInteractableMessage { title, body });
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
        prompt: prompt.to_string(),
        message,
        logic: paired_logic,
        checkpoint_id,
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
        spawnflags: 0,
        targetname: names.intern(node_name),
        target: psx_level::LOGIC_NAME_NONE,
        killtarget: psx_level::LOGIC_NAME_NONE,
        master: psx_level::LOGIC_NAME_NONE,
        delay_ticks: 0,
        wait_ticks: 0,
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
        },
    });
    true
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
    names: &mut NameInterner,
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
        } => {
            if box_prop.trim().is_empty() {
                report.error(format!(
                    "Door '{node_name}' names no Box Prop - the door's link \
                     is the box it opens"
                ));
                return false;
            }
            record.kind = psx_level::logic_kind::DOOR;
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
            "Enemy on '{node_name}' has max health 0 (must be > 0)"
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
    game_entities.push(PlaytestGameEntity {
        room: room_index,
        kind: names.intern(archetype_name),
        targetname: names.intern(node_name),
        model_instance: model_instance.unwrap_or(psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE),
        idle_clip: state_clips.idle,
        walk_clip: state_clips.walk,
        run_clip: state_clips.run,
        attack_clip: state_clips.attack,
        stagger_clip: state_clips.stagger,
        death_clip: state_clips.death,
        combat_capsule_first,
        combat_capsule_count,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        radius: settings.radius,
        height: settings.height,
        walk_speed: settings.walk_speed,
        run_speed: settings.run_speed,
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
        decision_interval_ticks: enemy.decision_interval_ticks,
        circle_chance: enemy.circle_chance,
        attack_priority: enemy.attack_priority,
        attack_cooldown_ticks: enemy.attack_cooldown_ticks,
        group_attack_delay_ticks: enemy.group_attack_delay_ticks,
        windup_ticks: enemy.windup_ticks,
        recovery_ticks: enemy.recovery_ticks,
        poise: enemy.poise,
        touch_damage: enemy.touch_damage,
        max_health: enemy.max_health,
        flags: psx_level::game_entity_flags::ENABLED,
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
    }
}

pub(crate) const fn particle_blend_mode_code(mode: PsxBlendMode) -> u8 {
    match mode {
        PsxBlendMode::Opaque | PsxBlendMode::Average => 0,
        PsxBlendMode::Add => 1,
        PsxBlendMode::Subtract => 2,
        PsxBlendMode::AddQuarter => 3,
    }
}

pub(crate) fn expand_lights_across_chunks(
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) -> Vec<PlaytestLight> {
    let mut out = Vec::new();
    for light in lights {
        let Some(source_room) = rooms.iter().find(|room| room.room_index == light.room) else {
            out.push(*light);
            continue;
        };
        let source_origin = room_origin_units(source_room);
        let global_x = source_origin[0].saturating_add(light.x);
        let global_z = source_origin[1].saturating_add(light.z);
        // Absolute Y of the light (source floor elevation + local Y).
        // `light.y` already carries the entity's floor via its authored
        // transform, so a floor-1 light sits in floor 1's band.
        let global_y = source_room.origin_y.saturating_add(light.y);
        let mut emitted = false;
        for target_room in rooms {
            if !light_overlaps_room_chunk(global_x, global_z, light.radius, target_room) {
                continue;
            }
            // Keep light on its own level: only spill to a target floor
            // whose band contains the light's Y (within its radius). This
            // stops a floor-1 light from lighting floor 0's overlapping
            // chunk and vice versa. Same-floor chunks share `origin_y`, so
            // intra-floor spill is unaffected.
            let dy = global_y.saturating_sub(target_room.origin_y);
            if i64::from(dy).saturating_mul(i64::from(dy))
                > i64::from(light.radius).saturating_mul(i64::from(light.radius))
            {
                continue;
            }
            let target_origin = room_origin_units(target_room);
            out.push(PlaytestLight {
                room: target_room.room_index,
                x: global_x.saturating_sub(target_origin[0]),
                y: light.y,
                z: global_z.saturating_sub(target_origin[1]),
                radius: light.radius,
                intensity_q8: light.intensity_q8,
                color: light.color,
            });
            emitted = true;
        }
        if !emitted {
            out.push(*light);
        }
    }
    out
}

pub(crate) fn light_overlaps_room_chunk(
    global_x: i32,
    global_z: i32,
    radius: u16,
    room: &CookedRoomBakeInput,
) -> bool {
    let origin = room_origin_units(room);
    let min_x = origin[0] as i64;
    let min_z = origin[1] as i64;
    let max_x =
        origin[0].saturating_add((room.cooked.width as i32) * room.cooked.sector_size) as i64;
    let max_z =
        origin[1].saturating_add((room.cooked.depth as i32) * room.cooked.sector_size) as i64;
    let x = global_x as i64;
    let z = global_z as i64;
    let closest_x = x.clamp(min_x, max_x);
    let closest_z = z.clamp(min_z, max_z);
    let dx = x - closest_x;
    let dz = z - closest_z;
    let radius = radius as i64;
    dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)) <= radius.saturating_mul(radius)
}

pub(crate) fn room_origin_units(room: &CookedRoomBakeInput) -> [i32; 2] {
    [
        room.world_origin[0].saturating_mul(room.cooked.sector_size),
        room.world_origin[1].saturating_mul(room.cooked.sector_size),
    ]
}

pub(crate) fn bake_static_surface_lights(
    rooms: &mut [CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for room in rooms {
        room.cooked.static_vertex_lighting = true;
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == room.room_index)
            .collect();
        let depth = room.cooked.depth as usize;
        let sector_size = room.cooked.sector_size;
        let ambient = room.cooked.ambient_color;
        let materials = room.cooked.materials.clone();
        for (idx, sector) in room.cooked.sectors.iter_mut().enumerate() {
            let Some(sector) = sector else {
                continue;
            };
            let sx = (idx / depth) as u16;
            let sz = (idx % depth) as u16;
            if let Some(face) = &mut sector.floor {
                let verts = horizontal_vertices(sx, sz, sector_size, face.heights);
                face.baked_vertex_rgb = bake_surface_vertex_rgb(
                    &materials,
                    ambient,
                    verts,
                    face.material,
                    &room_lights,
                );
            }
            if let Some(face) = &mut sector.ceiling {
                let verts =
                    reverse_quad_vertices(horizontal_vertices(sx, sz, sector_size, face.heights));
                face.baked_vertex_rgb = bake_surface_vertex_rgb(
                    &materials,
                    ambient,
                    verts,
                    face.material,
                    &room_lights,
                );
            }

            for (direction, walls) in [
                (psxw::direction::NORTH, sector.walls.north.as_mut_slice()),
                (psxw::direction::EAST, sector.walls.east.as_mut_slice()),
                (psxw::direction::SOUTH, sector.walls.south.as_mut_slice()),
                (psxw::direction::WEST, sector.walls.west.as_mut_slice()),
                (
                    psxw::direction::NORTH_WEST_SOUTH_EAST,
                    sector.walls.north_west_south_east.as_mut_slice(),
                ),
                (
                    psxw::direction::NORTH_EAST_SOUTH_WEST,
                    sector.walls.north_east_south_west.as_mut_slice(),
                ),
            ] {
                for wall in walls {
                    if let Some(verts) = wall_vertices(sx, sz, sector_size, direction, wall.heights)
                    {
                        wall.baked_vertex_rgb = bake_surface_vertex_rgb(
                            &materials,
                            ambient,
                            verts,
                            wall.material,
                            &room_lights,
                        );
                    }
                }
            }
        }
    }
}

pub(crate) fn bake_static_image_prop_lights(
    image_props: &mut [PlaytestImageProp],
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for prop in image_props {
        let Some(room) = rooms.iter().find(|room| room.room_index == prop.room) else {
            continue;
        };
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == prop.room)
            .collect();
        let ambient = room.cooked.ambient_color;
        let base = prop.tint_rgb;
        prop.baked_vertex_rgb = if prop.flags & image_prop_flags::CYLINDRICAL_BILLBOARD != 0 {
            let bottom = [prop.x, prop.y, prop.z];
            let top = [prop.x, prop.y.saturating_add(prop.height as i32), prop.z];
            let top_rgb = rgb_tuple(bake_static_vertex_rgb(top, base, ambient, &room_lights));
            let bottom_rgb = rgb_tuple(bake_static_vertex_rgb(bottom, base, ambient, &room_lights));
            [top_rgb, top_rgb, bottom_rgb, bottom_rgb]
        } else {
            let vertices = image_prop_static_vertices(prop);
            [
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[0],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[1],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[2],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    vertices[3],
                    base,
                    ambient,
                    &room_lights,
                )),
            ]
        };
    }
}

pub(crate) fn bake_static_box_prop_lights(
    box_props: &mut [PlaytestBoxProp],
    box_prop_surfaces: &mut [PlaytestBoxPropSurface],
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for prop in box_props {
        let Some(room) = rooms.iter().find(|room| room.room_index == prop.room) else {
            continue;
        };
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == prop.room)
            .collect();
        let ambient = room.cooked.ambient_color;
        let face_vertices = box_prop_static_face_vertices(prop);
        for (face, verts) in face_vertices.iter().enumerate() {
            let base = prop.tint_rgb[face];
            prop.baked_vertex_rgb[face] = [
                rgb_tuple(bake_static_vertex_rgb(
                    verts[0],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    verts[1],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    verts[2],
                    base,
                    ambient,
                    &room_lights,
                )),
                rgb_tuple(bake_static_vertex_rgb(
                    verts[3],
                    base,
                    ambient,
                    &room_lights,
                )),
            ];
        }
        let first = usize::from(prop.surface_first);
        let end = first
            .saturating_add(usize::from(prop.surface_count))
            .min(box_prop_surfaces.len());
        for surface in &mut box_prop_surfaces[first..end] {
            let face = usize::from(surface.source_face).min(psx_level::BOX_PROP_FACE_COUNT - 1);
            let base = prop.tint_rgb[face];
            for (color, vertex) in surface
                .baked_vertex_rgb
                .iter_mut()
                .zip(surface.vertices.iter())
            {
                *color = rgb_tuple(bake_static_vertex_rgb(*vertex, base, ambient, &room_lights));
            }
        }
    }
}

pub(crate) fn bake_static_cylinder_prop_lights(
    cylinder_props: &[PlaytestCylinderProp],
    cylinder_prop_surfaces: &mut [PlaytestCylinderPropSurface],
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for prop in cylinder_props {
        let Some(room) = rooms.iter().find(|room| room.room_index == prop.room) else {
            continue;
        };
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == prop.room)
            .collect();
        let ambient = room.cooked.ambient_color;
        let first = usize::from(prop.surface_first);
        let end = first
            .saturating_add(usize::from(prop.surface_count))
            .min(cylinder_prop_surfaces.len());
        for surface in &mut cylinder_prop_surfaces[first..end] {
            let slot =
                usize::from(surface.material_slot).min(psx_level::CYLINDER_PROP_MATERIAL_COUNT - 1);
            let base = prop.tint_rgb[slot];
            for index in 0..usize::from(surface.vertex_count.clamp(3, 4)) {
                surface.baked_vertex_rgb[index] = rgb_tuple(bake_static_vertex_rgb(
                    surface.vertices[index],
                    base,
                    ambient,
                    &room_lights,
                ));
            }
            if surface.vertex_count == 3 {
                surface.baked_vertex_rgb[3] = surface.baked_vertex_rgb[2];
            }
        }
    }
}

pub(crate) fn bake_static_arch_prop_lights(
    arch_props: &[PlaytestArchProp],
    arch_prop_surfaces: &mut [PlaytestArchPropSurface],
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) {
    for prop in arch_props {
        let Some(room) = rooms.iter().find(|room| room.room_index == prop.room) else {
            continue;
        };
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == prop.room)
            .collect();
        let ambient = room.cooked.ambient_color;
        let first = usize::from(prop.surface_first);
        let end = first
            .saturating_add(usize::from(prop.surface_count))
            .min(arch_prop_surfaces.len());
        for surface in &mut arch_prop_surfaces[first..end] {
            let slot =
                usize::from(surface.material_slot).min(psx_level::ARCH_PROP_MATERIAL_COUNT - 1);
            let base = prop.tint_rgb[slot];
            for (color, vertex) in surface
                .baked_vertex_rgb
                .iter_mut()
                .zip(surface.vertices.iter())
            {
                *color = rgb_tuple(bake_static_vertex_rgb(*vertex, base, ambient, &room_lights));
            }
        }
    }
}

fn box_prop_surface_center(vertices: [[i32; 3]; 4]) -> [i32; 3] {
    [
        vertices
            .iter()
            .map(|v| i64::from(v[0]))
            .sum::<i64>()
            .div_euclid(4) as i32,
        vertices
            .iter()
            .map(|v| i64::from(v[1]))
            .sum::<i64>()
            .div_euclid(4) as i32,
        vertices
            .iter()
            .map(|v| i64::from(v[2]))
            .sum::<i64>()
            .div_euclid(4) as i32,
    ]
}

fn polygon_surface_center(vertices: [[i32; 3]; 4], vertex_count: usize) -> [i32; 3] {
    let count = vertex_count.clamp(1, 4);
    [
        vertices[..count]
            .iter()
            .map(|v| i64::from(v[0]))
            .sum::<i64>()
            .div_euclid(count as i64) as i32,
        vertices[..count]
            .iter()
            .map(|v| i64::from(v[1]))
            .sum::<i64>()
            .div_euclid(count as i64) as i32,
        vertices[..count]
            .iter()
            .map(|v| i64::from(v[2]))
            .sum::<i64>()
            .div_euclid(count as i64) as i32,
    ]
}

fn box_prop_surface_normal(vertices: [[i32; 3]; 4]) -> [i32; 3] {
    let ab = [
        i64::from(vertices[1][0]) - i64::from(vertices[0][0]),
        i64::from(vertices[1][1]) - i64::from(vertices[0][1]),
        i64::from(vertices[1][2]) - i64::from(vertices[0][2]),
    ];
    let ac = [
        i64::from(vertices[2][0]) - i64::from(vertices[0][0]),
        i64::from(vertices[2][1]) - i64::from(vertices[0][1]),
        i64::from(vertices[2][2]) - i64::from(vertices[0][2]),
    ];
    [
        ((ab[1] * ac[2] - ab[2] * ac[1]) >> 10).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
        ((ab[2] * ac[0] - ab[0] * ac[2]) >> 10).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
        ((ab[0] * ac[1] - ab[1] * ac[0]) >> 10).clamp(i64::from(i32::MIN), i64::from(i32::MAX))
            as i32,
    ]
}

pub(crate) fn image_prop_static_vertices(prop: &PlaytestImageProp) -> [[i32; 3]; 4] {
    let half_width = (prop.width as i32) / 2;
    let height = prop.height as i32;
    let locals = [
        [-half_width, height, 0],
        [half_width, height, 0],
        [half_width, 0, 0],
        [-half_width, 0, 0],
    ];
    let mut out = [[0, 0, 0]; 4];
    for (idx, local) in locals.iter().enumerate() {
        let rotated = crate::spatial::rotate_euler_local_q12(
            *local,
            prop.pitch as u16,
            prop.yaw as u16,
            prop.roll as u16,
        );
        out[idx] = [
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        ];
    }
    out
}

pub(crate) fn box_prop_static_face_vertices(
    prop: &PlaytestBoxProp,
) -> [[[i32; 3]; 4]; psx_level::BOX_PROP_FACE_COUNT] {
    let mut vertices = [[0, 0, 0]; psx_level::BOX_PROP_VERTEX_COUNT];
    for (idx, local) in prop.vertices.iter().enumerate() {
        let rotated = crate::spatial::rotate_euler_local_q12(
            [local[0] as i32, local[1] as i32, local[2] as i32],
            prop.pitch as u16,
            prop.yaw as u16,
            prop.roll as u16,
        );
        vertices[idx] = [
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        ];
    }

    let mut faces = [[[0, 0, 0]; 4]; psx_level::BOX_PROP_FACE_COUNT];
    for face in 0..psx_level::BOX_PROP_FACE_COUNT {
        for corner in 0..4 {
            faces[face][corner] = vertices[crate::BOX_PROP_FACE_VERTEX_INDICES[face][corner]];
        }
    }
    faces
}

pub(crate) const fn rgb_tuple(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0], rgb[1], rgb[2])
}

pub(crate) fn bake_surface_vertex_rgb(
    materials: &[CookedWorldMaterial],
    ambient: [u8; 3],
    vertices: [[i32; 3]; 4],
    material_slot: u16,
    lights: &[&PlaytestLight],
) -> [[u8; 3]; 4] {
    let base = cooked_material_tint(materials, material_slot);
    [
        bake_static_vertex_rgb(vertices[0], base, ambient, lights),
        bake_static_vertex_rgb(vertices[1], base, ambient, lights),
        bake_static_vertex_rgb(vertices[2], base, ambient, lights),
        bake_static_vertex_rgb(vertices[3], base, ambient, lights),
    ]
}

pub(crate) fn cooked_material_tint(materials: &[CookedWorldMaterial], slot: u16) -> [u8; 3] {
    materials
        .iter()
        .find(|material| material.slot == slot)
        .map(|material| material.tint)
        .unwrap_or([128, 128, 128])
}

pub(crate) fn horizontal_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
) -> [[i32; 3]; 4] {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    [
        [x0, heights[0], z0],
        [x1, heights[1], z0],
        [x1, heights[2], z1],
        [x0, heights[3], z1],
    ]
}

pub(crate) fn reverse_quad_vertices(vertices: [[i32; 3]; 4]) -> [[i32; 3]; 4] {
    [vertices[3], vertices[2], vertices[1], vertices[0]]
}

pub(crate) fn wall_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    heights: [i32; 4],
) -> Option<[[i32; 3]; 4]> {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    match direction {
        psxw::direction::NORTH => Some([
            [x0, heights[0], z0],
            [x1, heights[1], z0],
            [x1, heights[2], z0],
            [x0, heights[3], z0],
        ]),
        psxw::direction::EAST => Some([
            [x1, heights[0], z0],
            [x1, heights[1], z1],
            [x1, heights[2], z1],
            [x1, heights[3], z0],
        ]),
        psxw::direction::SOUTH => Some([
            [x1, heights[0], z1],
            [x0, heights[1], z1],
            [x0, heights[2], z1],
            [x1, heights[3], z1],
        ]),
        psxw::direction::WEST => Some([
            [x0, heights[0], z1],
            [x0, heights[1], z0],
            [x0, heights[2], z0],
            [x0, heights[3], z1],
        ]),
        psxw::direction::NORTH_WEST_SOUTH_EAST => Some([
            [x0, heights[0], z0],
            [x1, heights[1], z1],
            [x1, heights[2], z1],
            [x0, heights[3], z0],
        ]),
        psxw::direction::NORTH_EAST_SOUTH_WEST => Some([
            [x1, heights[0], z0],
            [x0, heights[1], z1],
            [x0, heights[2], z1],
            [x1, heights[3], z0],
        ]),
        _ => None,
    }
}

pub(crate) fn bake_static_vertex_rgb(
    point: [i32; 3],
    base: [u8; 3],
    ambient: [u8; 3],
    lights: &[&PlaytestLight],
) -> [u8; 3] {
    const LIGHTING_NEUTRAL: u32 = 128;
    const LIGHTING_MAX: u32 = 255;
    let mut accum = [ambient[0] as u32, ambient[1] as u32, ambient[2] as u32];
    for light in lights {
        let Some(weight_q8) =
            point_light_weight_q8(point, [light.x, light.y, light.z], light.radius)
        else {
            continue;
        };
        for (channel, color) in accum.iter_mut().zip(light.color) {
            let weighted = (color as u32).saturating_mul(light.intensity_q8 as u32);
            *channel = channel.saturating_add(weighted.saturating_mul(weight_q8) >> 16);
        }
    }
    [
        ((base[0] as u32 * accum[0].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
        ((base[1] as u32 * accum[1].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
        ((base[2] as u32 * accum[2].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
    ]
}

pub(crate) fn point_light_weight_q8(
    point: [i32; 3],
    light_position: [i32; 3],
    radius: u16,
) -> Option<u32> {
    let radius = radius as u32;
    if radius == 0 {
        return None;
    }
    let dx = point[0].abs_diff(light_position[0]);
    let dy = point[1].abs_diff(light_position[1]);
    let dz = point[2].abs_diff(light_position[2]);
    if dx >= radius || dy >= radius || dz >= radius {
        return None;
    }
    let d2 = dx
        .checked_mul(dx)?
        .checked_add(dy.checked_mul(dy)?)?
        .checked_add(dz.checked_mul(dz)?)?;
    let r2 = radius.checked_mul(radius)?;
    if d2 >= r2 {
        return None;
    }
    Some((radius - isqrt_u32(d2)).saturating_mul(256) / radius)
}

pub(crate) fn isqrt_u32(value: u32) -> u32 {
    let mut x = value;
    let mut r = 0u32;
    let mut bit = 1u32 << 30;
    while bit > x {
        bit >>= 2;
    }
    while bit != 0 {
        if x >= r + bit {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    r
}

pub(crate) const FULL_HEIGHT_BLOCKER_TOLERANCE: i32 = 32;
