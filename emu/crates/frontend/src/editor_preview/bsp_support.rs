use super::*;

/// Material state resolved once per BSP face before packet emission.
#[derive(Copy, Clone)]
pub(super) enum FaceShade {
    Flat {
        rgb: (u8, u8, u8),
        sidedness: psxed_project::MaterialFaceSidedness,
    },
    Textured {
        slot: MaterialSlot,
        tint: (u8, u8, u8),
        blend_mode: BlendMode,
        sidedness: psxed_project::MaterialFaceSidedness,
    },
}

impl FaceShade {
    pub(super) const fn sidedness(self) -> psxed_project::MaterialFaceSidedness {
        match self {
            Self::Flat { sidedness, .. } | Self::Textured { sidedness, .. } => sidedness,
        }
    }
}

pub(super) fn preview_vertices_in_front(
    camera: psx_engine::WorldCamera,
    vertices: &[[i32; 3]],
) -> bool {
    vertices.iter().all(|vertex| {
        camera
            .view_vertex(psx_engine::WorldVertex::new(
                vertex[0], vertex[1], vertex[2],
            ))
            .z
            >= camera.projection.near_z
    })
}

pub(super) fn face_shade(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    fallback: (u8, u8, u8),
    textures: &EditorTextures,
) -> FaceShade {
    let tint = material_color(project, material, fallback);
    let sidedness = material_sidedness(project, material);
    if let Some(id) = material {
        if let Some(slot) = textures.slot(id) {
            return FaceShade::Textured {
                slot,
                tint: material_texture_tint(project, id),
                blend_mode: material_blend_mode(project, Some(id)),
                sidedness,
            };
        }
    }
    FaceShade::Flat {
        rgb: tint,
        sidedness,
    }
}

pub(super) fn material_texture_tint(
    project: &ProjectDocument,
    material: ResourceId,
) -> (u8, u8, u8) {
    project
        .resource(material)
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.tint),
            _ => None,
        })
        .map(|[r, g, b]| (r, g, b))
        .unwrap_or((0x80, 0x80, 0x80))
}

pub(super) fn material_blend_mode(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> BlendMode {
    material
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(psx_blend_mode(material.blend_mode)),
            _ => None,
        })
        .unwrap_or(BlendMode::Opaque)
}

pub(super) const fn psx_blend_mode(mode: psxed_project::PsxBlendMode) -> BlendMode {
    match mode {
        psxed_project::PsxBlendMode::Opaque => BlendMode::Opaque,
        psxed_project::PsxBlendMode::Average => BlendMode::Average,
        psxed_project::PsxBlendMode::Add => BlendMode::Add,
        psxed_project::PsxBlendMode::Subtract => BlendMode::Subtract,
        psxed_project::PsxBlendMode::AddQuarter => BlendMode::AddQuarter,
    }
}

/// BSP preview models currently use the scene's unfogged render path. Keeping
/// the neutral transform here lets the shared PSX model renderer stay intact
/// without retaining room fog authoring.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct PreviewFog;

impl PreviewFog {
    pub(super) const fn apply_rgb(self, rgb: (u8, u8, u8), _depth: i32) -> (u8, u8, u8) {
        rgb
    }
}

pub(super) fn collect_bsp_preview_bake_lights(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<psxed_project::brush_light::BrushPointLight> {
    let scene = project.active_scene();
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1) as f64;
    let mut out = Vec::new();
    for light in preview_lights(scene, hidden_scene_nodes) {
        if light.radius <= 0.0 || !light.intensity.is_finite() || light.intensity < 0.0 {
            continue;
        }
        out.push(psxed_project::brush_light::BrushPointLight {
            position: light.transform.translation.map(f64::from),
            radius: f64::from(light.radius) * radius_units,
            intensity_q8: (light.intensity * 256.0).clamp(0.0, f32::from(u16::MAX)) as u16,
            color: light.color,
        });
    }
    out
}

pub(super) fn collect_bsp_preview_lights(
    project: &ProjectDocument,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<psx_engine::PointLightSample> {
    let scene = project.active_scene();
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1) as f32;
    let mut out = Vec::new();
    for light in preview_lights(scene, hidden_scene_nodes) {
        if light.radius <= 0.0 || !light.intensity.is_finite() || light.intensity < 0.0 {
            continue;
        }
        let world = light
            .transform
            .translation
            .map(|value| value.round() as i32);
        let intensity_q8 = (light.intensity * 256.0).clamp(0.0, u16::MAX as f32) as u32;
        out.push(psx_engine::PointLightSample::from_rgb_intensity(
            world,
            (light.radius * radius_units) as i32,
            psx_engine::Rgb8::from_array(light.color),
            psx_engine::Q8::from_raw(intensity_q8),
        ));
    }
    out
}

#[derive(Clone, Copy)]
pub(super) struct PreviewLightMeta {
    pub(super) host_id: NodeId,
    pub(super) transform: Transform3,
    pub(super) color: [u8; 3],
    pub(super) intensity: f32,
    pub(super) radius: f32,
}

pub(super) fn preview_lights(
    scene: &Scene,
    hidden_scene_nodes: &HashSet<NodeId>,
) -> Vec<PreviewLightMeta> {
    scene
        .nodes()
        .iter()
        .filter(|node| !scene_node_hidden(scene, hidden_scene_nodes, node.id))
        .filter_map(|node| {
            let NodeKind::PointLight {
                color,
                intensity,
                radius,
            } = &node.kind
            else {
                return None;
            };
            Some(PreviewLightMeta {
                host_id: node.id,
                transform: node.transform,
                color: *color,
                intensity: *intensity,
                radius: *radius,
            })
        })
        .collect()
}

pub(super) fn material_color(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    fallback: (u8, u8, u8),
) -> (u8, u8, u8) {
    let Some(resource) = material.and_then(|id| project.resource(id)) else {
        return fallback;
    };
    let name = resource.name.as_bytes();
    let contains = |needle: &[u8]| {
        name.windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    };
    if contains(b"brick") {
        (0xC8, 0x70, 0x40)
    } else if contains(b"floor") || contains(b"stone") {
        (0xB6, 0xAC, 0x96)
    } else if contains(b"glass") {
        (0x70, 0xA8, 0xC0)
    } else if contains(b"wood") {
        (0x90, 0x60, 0x40)
    } else if contains(b"metal") {
        (0x90, 0x96, 0x9A)
    } else if let ResourceData::Material(material) = &resource.data {
        if material.tint != [0x80, 0x80, 0x80] {
            let [r, g, b] = material.tint;
            (r, g, b)
        } else {
            fallback
        }
    } else {
        fallback
    }
}

fn material_sidedness(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> psxed_project::MaterialFaceSidedness {
    material
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.sidedness()),
            _ => None,
        })
        .unwrap_or_default()
}
