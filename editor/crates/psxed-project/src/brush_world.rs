//! Project-level brush world compilation into one complete PXBSP artifact.

use std::path::Path;

use crate::brush::{Brush, BRUSH_UV_UNITS_PER_TEXEL};
use crate::brush_collision_hulls::{
    compile_collision_hulls, CollisionHullBounds, CollisionHullCompileError, CompiledCollisionHulls,
};
use crate::brush_compile::{build_surface_bsp, compile_csg_surfaces};
use crate::brush_light::{
    bake_brush_vertex_lighting, BrushLightError, BrushMaterialTint, BrushPointLight,
};
use crate::brush_pack::{pack_bsp_geometry, BrushPackError, BspLighting, PackedBspGeometry};
use crate::brush_portal::{classify_bsp_leaves, portalize_surface_bsp};
use crate::brush_pxbsp::{
    build_pxbsp_with_submodels, pxbsp_material_slots, CompiledPxbsp, PxbspBuildError,
    PxbspEntityInput, PxbspMapPayloads, PxbspSubmodel,
};
use crate::{
    resolve_material_texture_psxt, LogicNodeKind, MaterialAnimationMode, MaterialFaceSidedness,
    NodeId, NodeKind, ProjectDocument, PsxBlendMode, ResourceData, ResourceId, Scene,
};

use psx_bsp::pxbsp::{
    entity_class, entity_flags, material_animation, material_blend, material_flags, PxbspBrushDoor,
    PxbspBrushDoorError, PxbspEntity, PxbspMaterial,
};
use psx_bsp::{Node, Plane, RecordSlice, Vec3I32};
use psxed_format::texture::Depth;

const PLAYER_HULL: CollisionHullBounds = CollisionHullBounds {
    mins: [-16, 0, -16],
    maxs: [16, 56, 16],
};
const BIG_HULL: CollisionHullBounds = CollisionHullBounds {
    mins: [-32, 0, -32],
    maxs: [32, 96, 32],
};
const WORLD_HULLS: [CollisionHullBounds; 3] = [CollisionHullBounds::POINT, PLAYER_HULL, BIG_HULL];
const DEFAULT_LIGHT_RADIUS_UNITS: f64 = 1024.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushWorldCookMode {
    Draft,
    Release,
}

pub struct BrushWorldCookOptions<'a> {
    pub project_root: &'a Path,
    pub mode: BrushWorldCookMode,
    pub ambient: [u8; 3],
    /// First caller-owned runtime asset-table slot reserved for brush textures.
    pub texture_asset_base: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledBrushTexture {
    pub asset_id: u16,
    pub key: String,
    pub bytes: Vec<u8>,
    pub size: [u8; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledBrushMover {
    pub node: NodeId,
    /// Model 0 is the static world; mover models begin at 1.
    pub model_index: u16,
    pub origin: [i32; 3],
    pub open_offset: [i32; 3],
    pub travel_ticks: u16,
    pub start_open: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledBrushWorld {
    pub pxbsp: CompiledPxbsp,
    pub textures: Vec<CompiledBrushTexture>,
    pub movers: Vec<CompiledBrushMover>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrushWorldCookError {
    EmptyStaticWorld,
    MissingMover {
        brush: usize,
        node: NodeId,
    },
    BrushOwnerIsNotDoor {
        brush: usize,
        node: NodeId,
    },
    UnsupportedMoverTransform(NodeId),
    MoverOriginOutOfRange(NodeId),
    MoverOriginInSolid(NodeId),
    InvalidDoorMotion {
        node: NodeId,
        error: PxbspBrushDoorError,
    },
    InvalidWorldTree,
    MissingMaterial(ResourceId),
    ResourceIsNotMaterial(ResourceId),
    MaterialTexture {
        material: ResourceId,
        error: String,
    },
    InvalidTexture {
        material: Option<ResourceId>,
        error: String,
    },
    ModelIndexOverflow,
    TextureAssetOverflow,
    Pack(BrushPackError),
    Collision(CollisionHullCompileError),
    Light(BrushLightError),
    Pxbsp(PxbspBuildError),
}

impl From<BrushPackError> for BrushWorldCookError {
    fn from(value: BrushPackError) -> Self {
        Self::Pack(value)
    }
}

impl From<CollisionHullCompileError> for BrushWorldCookError {
    fn from(value: CollisionHullCompileError) -> Self {
        Self::Collision(value)
    }
}

impl From<BrushLightError> for BrushWorldCookError {
    fn from(value: BrushLightError) -> Self {
        Self::Light(value)
    }
}

impl From<PxbspBuildError> for BrushWorldCookError {
    fn from(value: PxbspBuildError) -> Self {
        Self::Pxbsp(value)
    }
}

/// Compile the active brush scene, including every brush-bound Door submodel.
pub fn compile_brush_world(
    project: &ProjectDocument,
    options: BrushWorldCookOptions<'_>,
) -> Result<CompiledBrushWorld, BrushWorldCookError> {
    let scene = project.active_scene();
    let mut static_brushes = Vec::new();
    for (brush_index, brush) in scene.brushes.iter().enumerate() {
        match brush.mover {
            None => static_brushes.push(brush.clone()),
            Some(node) => {
                let Some(owner) = scene.node(node) else {
                    return Err(BrushWorldCookError::MissingMover {
                        brush: brush_index,
                        node,
                    });
                };
                if !matches!(
                    &owner.kind,
                    NodeKind::Logic {
                        kind: LogicNodeKind::Door { .. },
                        ..
                    }
                ) {
                    return Err(BrushWorldCookError::BrushOwnerIsNotDoor {
                        brush: brush_index,
                        node,
                    });
                }
            }
        }
    }
    if static_brushes.is_empty() {
        return Err(BrushWorldCookError::EmptyStaticWorld);
    }

    let all_brushes = scene.brushes.clone();
    let lights = scene_lights(scene);
    let material_tints = material_tints(project);
    let (world_geometry, world_collision) = compile_model(
        &static_brushes,
        &all_brushes,
        &lights,
        &material_tints,
        options.mode,
        options.ambient,
    )?;

    let mut submodels = Vec::new();
    let mut movers = Vec::new();
    let mut entities = Vec::new();
    for node in scene.nodes() {
        let NodeKind::Logic {
            kind:
                LogicNodeKind::Door {
                    start_open,
                    open_offset,
                    travel_ticks,
                    ..
                },
            enabled,
            ..
        } = &node.kind
        else {
            continue;
        };
        let bound: Vec<_> = scene
            .brushes
            .iter()
            .filter(|brush| brush.mover == Some(node.id))
            .cloned()
            .collect();
        if bound.is_empty() {
            continue;
        }
        validate_mover_transform(
            node.id,
            node.transform.rotation_degrees,
            node.transform.scale,
        )?;
        let origin = mover_origin(node.id, node.transform.translation)?;
        let local_brushes = translate_brushes(&bound, origin);
        let local_occluders = translate_brushes(&all_brushes, origin);
        let local_lights = translate_lights(&lights, origin);
        let (geometry, collision) = compile_model(
            &local_brushes,
            &local_occluders,
            &local_lights,
            &material_tints,
            options.mode,
            options.ambient,
        )?;
        let model_index = u16::try_from(submodels.len() + 1)
            .map_err(|_| BrushWorldCookError::ModelIndexOverflow)?;
        let leaf_probe = model_center_world_q12(origin, geometry.mins, geometry.maxs);
        // ponytail: PXBSP v1 links one representative leaf. Replace this
        // with a touched-leaf span before full entity PVS activation ships.
        let leaf = packed_point_leaf(&world_geometry, leaf_probe)?;
        if leaf == 0 {
            return Err(BrushWorldCookError::MoverOriginInSolid(node.id));
        }
        let motion = PxbspBrushDoor::new(
            Vec3I32 {
                x: i32::from(open_offset[0]) * 4096,
                y: i32::from(open_offset[1]) * 4096,
                z: i32::from(open_offset[2]) * 4096,
            },
            *travel_ticks,
        );
        motion
            .validate()
            .map_err(|error| BrushWorldCookError::InvalidDoorMotion {
                node: node.id,
                error,
            })?;
        let mut flags = 0;
        if *enabled {
            flags |= entity_flags::ENABLED;
        }
        if *start_open {
            flags |= entity_flags::START_OPEN;
        }
        entities.push(PxbspEntityInput {
            entity: PxbspEntity {
                class_id: entity_class::BRUSH_DOOR,
                flags,
                model: model_index,
                leaf,
                origin: Vec3I32 {
                    x: origin[0] * 4096,
                    y: origin[1] * 4096,
                    z: origin[2] * 4096,
                },
                ..PxbspEntity::default()
            },
            payload: motion.to_le_bytes().to_vec(),
        });
        submodels.push(PxbspSubmodel {
            geometry,
            collision,
            origin: origin.map(|value| value as i16),
        });
        movers.push(CompiledBrushMover {
            node: node.id,
            model_index,
            origin,
            open_offset: open_offset.map(i32::from),
            travel_ticks: *travel_ticks,
            start_open: *start_open,
            enabled: *enabled,
        });
    }

    let slots = pxbsp_material_slots(&world_geometry, &submodels);
    let (materials, textures) = resolve_materials(project, &slots, &options)?;
    let pxbsp = build_pxbsp_with_submodels(
        world_geometry,
        world_collision,
        submodels,
        PxbspMapPayloads {
            materials: &materials,
            entities: &entities,
            texture_data: &[],
            sound_data: &[],
            model_data: &[],
            strings: &[],
            streaming_index: &[],
        },
    )?;
    Ok(CompiledBrushWorld {
        pxbsp,
        textures,
        movers,
    })
}

fn compile_model(
    brushes: &[Brush],
    light_occluders: &[Brush],
    lights: &[BrushPointLight],
    material_tints: &[BrushMaterialTint],
    mode: BrushWorldCookMode,
    ambient: [u8; 3],
) -> Result<(PackedBspGeometry, CompiledCollisionHulls), BrushWorldCookError> {
    let surfaces = compile_csg_surfaces(brushes);
    let mut bsp = build_surface_bsp(&surfaces);
    let portals = portalize_surface_bsp(&bsp);
    classify_bsp_leaves(&mut bsp, &portals, brushes);
    let geometry = match mode {
        BrushWorldCookMode::Draft => pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright)?,
        BrushWorldCookMode::Release => {
            let lighting = bake_brush_vertex_lighting(
                &bsp.surfaces,
                light_occluders,
                ambient,
                lights,
                material_tints,
            )?;
            pack_bsp_geometry(&bsp, &portals, BspLighting::Baked(&lighting))?
        }
    };
    let collision = compile_collision_hulls(brushes, &WORLD_HULLS)?;
    Ok((geometry, collision))
}

fn validate_mover_transform(
    node: NodeId,
    rotation: [f32; 3],
    scale: [f32; 3],
) -> Result<(), BrushWorldCookError> {
    // ponytail: the initial editor rebase supports translated movers. Apply
    // the inverse authored rotation/scale to brush planes before enabling
    // those inspector controls for brush-owned models.
    let rotation_is_zero = rotation
        .into_iter()
        .all(|value| value.is_finite() && value.abs() <= 0.0001);
    let scale_is_one = scale
        .into_iter()
        .all(|value| value.is_finite() && (value - 1.0).abs() <= 0.0001);
    if rotation_is_zero && scale_is_one {
        Ok(())
    } else {
        Err(BrushWorldCookError::UnsupportedMoverTransform(node))
    }
}

fn mover_origin(node: NodeId, translation: [f32; 3]) -> Result<[i32; 3], BrushWorldCookError> {
    // Brush scenes author translations directly in world units. Grid scenes
    // keep their sector-based transform interpretation in the legacy cooker.
    if !translation.into_iter().all(f32::is_finite) {
        return Err(BrushWorldCookError::MoverOriginOutOfRange(node));
    }
    let origin = translation.map(|value| value.round() as i32);
    if origin.into_iter().all(|value| i16::try_from(value).is_ok()) {
        Ok(origin)
    } else {
        Err(BrushWorldCookError::MoverOriginOutOfRange(node))
    }
}

fn model_center_world_q12(origin: [i32; 3], mins: [i16; 3], maxs: [i16; 3]) -> Vec3I32 {
    let axis = |index: usize| {
        origin[index] * 4096 + (i32::from(mins[index]) + i32::from(maxs[index])) * 2048
    };
    Vec3I32 {
        x: axis(0),
        y: axis(1),
        z: axis(2),
    }
}

fn packed_point_leaf(
    geometry: &PackedBspGeometry,
    point: Vec3I32,
) -> Result<u16, BrushWorldCookError> {
    let nodes =
        RecordSlice::<Node>::new(&geometry.nodes).ok_or(BrushWorldCookError::InvalidWorldTree)?;
    let planes =
        RecordSlice::<Plane>::new(&geometry.planes).ok_or(BrushWorldCookError::InvalidWorldTree)?;
    let mut node_index = geometry.root_node;
    loop {
        if node_index < 0 {
            let leaf = -1i32 - i32::from(node_index);
            return u16::try_from(leaf).map_err(|_| BrushWorldCookError::InvalidWorldTree);
        }
        let node = nodes
            .get(node_index as usize)
            .ok_or(BrushWorldCookError::InvalidWorldTree)?;
        let plane = planes
            .get(node.plane as usize)
            .ok_or(BrushWorldCookError::InvalidWorldTree)?;
        let dot = match plane.kind {
            0 => point.x,
            1 => point.y,
            2 => point.z,
            _ => packed_mul_q12(point.x, i32::from(plane.normal.x))
                .saturating_add(packed_mul_q12(point.y, i32::from(plane.normal.y)))
                .saturating_add(packed_mul_q12(point.z, i32::from(plane.normal.z))),
        };
        node_index = node.children[usize::from(dot.saturating_sub(plane.distance) <= 0)];
    }
}

fn packed_mul_q12(value: i32, q12: i32) -> i32 {
    let product = (i64::from(value) * i64::from(q12)) >> 12;
    product.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn translate_brushes(brushes: &[Brush], origin: [i32; 3]) -> Vec<Brush> {
    brushes
        .iter()
        .cloned()
        .map(|mut brush| {
            brush.translate_with_uv_lock(
                [-origin[0], -origin[1], -origin[2]],
                BRUSH_UV_UNITS_PER_TEXEL,
            );
            brush
        })
        .collect()
}

fn scene_lights(scene: &Scene) -> Vec<BrushPointLight> {
    scene
        .nodes()
        .iter()
        .filter_map(|node| {
            let NodeKind::PointLight {
                color,
                intensity,
                radius,
            } = &node.kind
            else {
                return None;
            };
            Some(BrushPointLight {
                position: node.transform.translation.map(f64::from),
                // Existing authoring stores light radius in sector units.
                radius: f64::from(*radius) * DEFAULT_LIGHT_RADIUS_UNITS,
                intensity_q8: (f64::from(*intensity) * 256.0)
                    .round()
                    .clamp(0.0, u16::MAX as f64) as u16,
                color: *color,
            })
        })
        .collect()
}

fn translate_lights(lights: &[BrushPointLight], origin: [i32; 3]) -> Vec<BrushPointLight> {
    lights
        .iter()
        .copied()
        .map(|mut light| {
            for (value, origin) in light.position.iter_mut().zip(origin) {
                *value -= f64::from(origin);
            }
            light
        })
        .collect()
}

fn material_tints(project: &ProjectDocument) -> Vec<BrushMaterialTint> {
    let mut tints = vec![BrushMaterialTint {
        material: None,
        color: [128; 3],
    }];
    tints.extend(project.resources.iter().filter_map(|resource| {
        let ResourceData::Material(material) = &resource.data else {
            return None;
        };
        Some(BrushMaterialTint {
            material: Some(resource.id),
            color: material.tint,
        })
    }));
    tints
}

fn resolve_materials(
    project: &ProjectDocument,
    slots: &[Option<ResourceId>],
    options: &BrushWorldCookOptions<'_>,
) -> Result<(Vec<PxbspMaterial>, Vec<CompiledBrushTexture>), BrushWorldCookError> {
    let mut textures = Vec::new();
    let mut materials = Vec::with_capacity(slots.len());
    for &slot in slots {
        let (key, bytes, tint, blend, sidedness, animation) = match slot {
            None => (
                "@brush-flat-white".to_string(),
                flat_white_psxt(),
                [128; 3],
                PsxBlendMode::Opaque,
                MaterialFaceSidedness::Front,
                crate::MaterialAnimation::default(),
            ),
            Some(id) => {
                let resource = project
                    .resource(id)
                    .ok_or(BrushWorldCookError::MissingMaterial(id))?;
                let ResourceData::Material(material) = &resource.data else {
                    return Err(BrushWorldCookError::ResourceIsNotMaterial(id));
                };
                let texture = resolve_material_texture_psxt(project, id, options.project_root)
                    .map_err(|error| BrushWorldCookError::MaterialTexture {
                        material: id,
                        error,
                    })?;
                let (key, bytes) =
                    texture.unwrap_or_else(|| ("@brush-flat-white".to_string(), flat_white_psxt()));
                (
                    key,
                    bytes,
                    material.tint,
                    material.blend_mode,
                    material.sidedness(),
                    material.animation,
                )
            }
        };
        let (texture_asset, texture_size) =
            intern_texture(&mut textures, options.texture_asset_base, slot, key, bytes)?;
        materials.push(pack_material(
            texture_asset,
            texture_size,
            tint,
            blend,
            sidedness,
            animation,
            slot,
        )?);
    }
    Ok((materials, textures))
}

fn intern_texture(
    textures: &mut Vec<CompiledBrushTexture>,
    base: u16,
    material: Option<ResourceId>,
    key: String,
    bytes: Vec<u8>,
) -> Result<(u16, [u8; 2]), BrushWorldCookError> {
    if let Some(existing) = textures.iter().find(|texture| texture.key == key) {
        if existing.bytes != bytes {
            return Err(BrushWorldCookError::InvalidTexture {
                material,
                error: format!("texture key {key:?} resolved to different bytes"),
            });
        }
        return Ok((existing.asset_id, existing.size));
    }
    let parsed = psx_asset::Texture::from_bytes(&bytes).map_err(|error| {
        BrushWorldCookError::InvalidTexture {
            material,
            error: format!("invalid PSXT: {error:?}"),
        }
    })?;
    let width = parsed.width();
    let height = parsed.height();
    if parsed.depth() != Depth::Bit4
        || !(8..=128).contains(&width)
        || !(8..=128).contains(&height)
        || !width.is_power_of_two()
        || !height.is_power_of_two()
    {
        return Err(BrushWorldCookError::InvalidTexture {
            material,
            error: format!("brush texture must be 4bpp power-of-two 8..128, got {width}x{height}"),
        });
    }
    let index =
        u16::try_from(textures.len()).map_err(|_| BrushWorldCookError::TextureAssetOverflow)?;
    let asset_id = base
        .checked_add(index)
        .ok_or(BrushWorldCookError::TextureAssetOverflow)?;
    let size = [width as u8, height as u8];
    textures.push(CompiledBrushTexture {
        asset_id,
        key,
        bytes,
        size,
    });
    Ok((asset_id, size))
}

fn pack_material(
    texture_asset: u16,
    texture_size: [u8; 2],
    tint: [u8; 3],
    blend: PsxBlendMode,
    sidedness: MaterialFaceSidedness,
    animation: crate::MaterialAnimation,
    material: Option<ResourceId>,
) -> Result<PxbspMaterial, BrushWorldCookError> {
    let flags = match sidedness {
        MaterialFaceSidedness::Front => material_flags::FACE_FRONT,
        MaterialFaceSidedness::Back => material_flags::FACE_BACK,
        MaterialFaceSidedness::Both => material_flags::FACE_BOTH,
    };
    let blend_mode = match blend {
        PsxBlendMode::Opaque => material_blend::OPAQUE,
        PsxBlendMode::Average => material_blend::AVERAGE,
        PsxBlendMode::Add => material_blend::ADD,
        PsxBlendMode::Subtract => material_blend::SUBTRACT,
        PsxBlendMode::AddQuarter => material_blend::ADD_QUARTER,
    };
    let (animation_kind, animation_data) = match animation.mode {
        MaterialAnimationMode::Static => (material_animation::STATIC, [0; 7]),
        MaterialAnimationMode::UvScroll => {
            let motion = animation.uv_scroll;
            let speed_u = if motion.enabled { motion.speed_u_q8 } else { 0 };
            let speed_v = if motion.enabled { motion.speed_v_q8 } else { 0 };
            let mut data = [0; 7];
            data[0..2].copy_from_slice(&speed_u.to_le_bytes());
            data[2..4].copy_from_slice(&speed_v.to_le_bytes());
            data[4] = motion.phase_u;
            data[5] = motion.phase_v;
            (material_animation::UV_SCROLL, data)
        }
        MaterialAnimationMode::Flipbook => {
            let flipbook = animation.flipbook.normalized();
            if !texture_size[0].is_multiple_of(flipbook.columns)
                || !texture_size[1].is_multiple_of(flipbook.rows)
            {
                return Err(BrushWorldCookError::InvalidTexture {
                    material,
                    error: "flipbook grid does not divide texture dimensions".to_string(),
                });
            }
            (
                material_animation::FLIPBOOK,
                [
                    flipbook.columns,
                    flipbook.rows,
                    flipbook.frame_count,
                    flipbook.ticks_per_frame,
                    flipbook.phase,
                    0,
                    0,
                ],
            )
        }
    };
    Ok(PxbspMaterial {
        texture_asset,
        flags,
        tint,
        blend_mode,
        animation_kind,
        animation_data,
    })
}

fn flat_white_psxt() -> Vec<u8> {
    psxed_tex::encode_indexed_psxt(
        8,
        8,
        Depth::Bit4,
        &[1; 64],
        &[[0, 0, 0], [255, 255, 255]],
        false,
    )
    .expect("fixed brush fallback texture is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MaterialResource, Transform3};
    use psx_bsp::mover::BrushDoor;
    use psx_bsp::pxbsp::{entity_class, entity_flags, PxbspBrushDoor};
    use psx_bsp::pxbsp_resident::PxbspResidentMap;
    use psx_bsp::SliceReader;

    fn door_kind() -> NodeKind {
        NodeKind::Logic {
            kind: LogicNodeKind::Door {
                box_prop: String::new(),
                start_open: false,
                open_offset: crate::default_brush_door_open_offset(),
                travel_ticks: crate::default_brush_door_travel_ticks(),
            },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
        }
    }

    fn authored_project() -> ProjectDocument {
        let mut project = ProjectDocument::new("brush world");
        let material = project.add_resource(
            "Stone",
            ResourceData::Material(MaterialResource::opaque(None)),
        );
        let scene = project.active_scene_mut();
        let mut room = Brush::cuboid([0, 0, 0], [1024, 512, 1024])
            .hollow(64)
            .expect("room");
        for brush in &mut room {
            for face in &mut brush.faces {
                face.material = Some(material);
            }
        }
        scene.brushes = room;
        let door = scene.add_node(NodeId::ROOT, "Door", door_kind());
        scene.node_mut(door).expect("door").transform = Transform3 {
            translation: [512.0, 64.0, 512.0],
            ..Transform3::default()
        };
        let mut door_brush = Brush::cuboid([480, 64, 504], [544, 160, 520]);
        door_brush.mover = Some(door);
        for face in &mut door_brush.faces {
            face.material = Some(material);
        }
        scene.brushes.push(door_brush);
        scene.add_node(
            NodeId::ROOT,
            "Lamp",
            NodeKind::PointLight {
                color: [255, 160, 96],
                intensity: 1.0,
                radius: 1.0,
            },
        );

        project
    }

    fn authored_world(mode: BrushWorldCookMode) -> CompiledBrushWorld {
        let project = authored_project();
        compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode,
                ambient: [24; 3],
                texture_asset_base: 40,
            },
        )
        .expect("brush world")
    }

    #[test]
    fn scene_cook_emits_static_world_and_local_door_model() {
        let compiled = authored_world(BrushWorldCookMode::Draft);
        assert_eq!(compiled.textures.len(), 1);
        assert_eq!(compiled.textures[0].asset_id, 40);
        assert_eq!(compiled.movers.len(), 1);
        assert_eq!(compiled.movers[0].model_index, 1);
        assert_eq!(compiled.movers[0].origin, [512, 64, 512]);
        assert_eq!(compiled.movers[0].open_offset, [0, 128, 0]);
        assert_eq!(compiled.movers[0].travel_ticks, 60);
        assert!(compiled.movers[0].enabled);

        let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
        map.load(9, &mut SliceReader::new(&compiled.pxbsp.bytes))
            .expect("resident PXBSP");
        assert_eq!(map.brush_models().len(), 2);
        let door_model = map.brush_models().get(1).expect("door model");
        assert_eq!(
            [door_model.mins.x, door_model.mins.y, door_model.mins.z],
            [-32, 0, -8]
        );
        assert_eq!(
            [door_model.maxs.x, door_model.maxs.y, door_model.maxs.z],
            [32, 96, 8]
        );
        assert_eq!(map.materials().get(0).expect("material").texture_asset, 40);

        let entities = map.entities();
        assert_eq!(entities.len(), 1);
        let entity = entities.get(0).expect("door entity");
        assert_eq!(entity.class_id, entity_class::BRUSH_DOOR);
        assert_eq!(entity.flags, entity_flags::ENABLED);
        assert_eq!(entity.model, 1);
        assert_ne!(entity.leaf, 0);
        assert_eq!(
            entity.origin,
            Vec3I32 {
                x: 512 * 4096,
                y: 64 * 4096,
                z: 512 * 4096
            }
        );
        let payload = entities
            .payload_record::<PxbspBrushDoor>(0)
            .expect("door payload");
        assert_eq!(
            payload.open_offset,
            Vec3I32 {
                x: 0,
                y: 128 * 4096,
                z: 0
            }
        );
        assert_eq!(payload.travel_ticks, 60);

        let mut door = BrushDoor::from_entity(entity, payload).expect("runtime door");
        let hull = map
            .model_collision_hull(1, 0)
            .expect("door point hull")
            .transformed(door.transform());
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 512 * 4096,
                y: 96 * 4096,
                z: 512 * 4096,
            }),
            Some(psx_bsp::collision::CONTENTS_SOLID)
        );
        door.set_open(true);
        for _ in 0..60 {
            assert!(door.tick());
        }
        let open_hull = map
            .model_collision_hull(1, 0)
            .expect("door point hull")
            .transformed(door.transform());
        assert_eq!(
            open_hull.point_contents(Vec3I32 {
                x: 512 * 4096,
                y: 96 * 4096,
                z: 512 * 4096,
            }),
            Some(psx_bsp::collision::CONTENTS_EMPTY)
        );
    }

    #[test]
    fn draft_and_release_cooks_are_deterministic_and_distinct() {
        let draft = authored_world(BrushWorldCookMode::Draft);
        let release = authored_world(BrushWorldCookMode::Release);
        assert_eq!(draft, authored_world(BrushWorldCookMode::Draft));
        assert_eq!(release, authored_world(BrushWorldCookMode::Release));
        assert_ne!(draft.pxbsp.bytes, release.pxbsp.bytes);
    }

    #[test]
    fn missing_mover_binding_fails_loudly() {
        let mut project = ProjectDocument::new("bad binding");
        let mut world = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        world.mover = None;
        let mut bad = Brush::cuboid([64, 64, 64], [96, 96, 96]);
        bad.mover = Some(NodeId(999));
        project.active_scene_mut().brushes = vec![world, bad];
        let error = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode: BrushWorldCookMode::Draft,
                ambient: [32; 3],
                texture_asset_base: 0,
            },
        )
        .expect_err("bad mover");
        assert_eq!(
            error,
            BrushWorldCookError::MissingMover {
                brush: 1,
                node: NodeId(999),
            }
        );
    }

    #[test]
    fn invalid_door_motion_fails_loudly() {
        let mut project = authored_project();
        let door = project
            .active_scene()
            .nodes()
            .iter()
            .find(|node| {
                matches!(
                    &node.kind,
                    NodeKind::Logic {
                        kind: LogicNodeKind::Door { .. },
                        ..
                    }
                )
            })
            .expect("door")
            .id;
        let NodeKind::Logic {
            kind: LogicNodeKind::Door { open_offset, .. },
            ..
        } = &mut project
            .active_scene_mut()
            .node_mut(door)
            .expect("door")
            .kind
        else {
            panic!("door kind");
        };
        *open_offset = [0; 3];
        let error = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode: BrushWorldCookMode::Draft,
                ambient: [24; 3],
                texture_asset_base: 40,
            },
        )
        .expect_err("motionless door");
        assert_eq!(
            error,
            BrushWorldCookError::InvalidDoorMotion {
                node: door,
                error: PxbspBrushDoorError::ZeroOpenOffset,
            }
        );
    }
}
