//! Project-level brush world compilation into one complete PXBSP artifact.

use std::fmt;
use std::path::Path;

use crate::brush::{Brush, BrushContents, BRUSH_UV_UNITS_PER_TEXEL};
use crate::brush_collision_hulls::{
    compile_collision_hulls, CollisionHullBounds, CollisionHullCompileError, CompiledCollisionHulls,
};
use crate::brush_compile::{build_surface_bsp, compile_csg_surfaces, subdivide_surfaces_to_extent};
use crate::brush_light::{
    bake_brush_vertex_lighting, BrushLightError, BrushMaterialTint, BrushPointLight,
};
use crate::brush_pack::{pack_bsp_geometry, BrushPackError, BspLighting, PackedBspGeometry};
use crate::brush_portal::{classify_bsp_leaves, portalize_surface_bsp};
use crate::brush_pxbsp::{
    build_pxbsp_with_submodels, pxbsp_material_slots, CompiledPxbsp, PxbspBuildError,
    PxbspEntityInput, PxbspMapPayloads, PxbspSubmodel,
};
use crate::units::ENGINE_SURFACE_EXTENT_UNITS;
use crate::{
    resolve_material_texture_psxt, LogicNodeKind, MaterialAnimationMode, MaterialFaceSidedness,
    NodeId, NodeKind, ProjectDocument, PsxBlendMode, ResourceData, ResourceId, Scene,
};

use psx_bsp::collision::{CollisionHull, CONTENTS_SOLID};
use psx_bsp::collision_provider::{select_body_hull, CookedBodyHull};
use psx_bsp::pxbsp::{
    entity_class, entity_flags, material_animation, material_blend, material_flags, PxbspBrushDoor,
    PxbspBrushDoorError, PxbspEntity, PxbspMaterial,
};
use psx_bsp::{ClipNode, Node, Plane, RecordSlice, Vec3I32};
use psxed_format::texture::Depth;

// Characterless-fallback contract. These two envelope pairs persist on
// purpose: DEBUG 16/56 is the body every characterless BSP project (and any
// player spawn that resolves no Character) compiles into hull one, and
// LEGACY_BIG 32/96 floors hull two so old fixtures keep their hash anchors.
// The cook has no fixture-name knowledge; a project with real placed
// characters derives both hulls fully from its authored data and never sees
// these numbers.
const DEBUG_BODY_RADIUS: i32 = 1;
const DEBUG_BODY_HEIGHT: i32 = 4;
const LEGACY_BIG_HULL_RADIUS: i32 = 2;
const LEGACY_BIG_HULL_HEIGHT: i32 = 6;
const DEFAULT_LIGHT_RADIUS_UNITS: f64 = 1024.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum BrushWorldCookMode {
    /// Fast fullbright compile for iteration and embedded Play.
    #[default]
    Draft,
    /// Bake authored point lighting into the PXBSP vertex stream.
    Release,
}

impl BrushWorldCookMode {
    pub const ALL: [Self; 2] = [Self::Draft, Self::Release];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Release => "Release",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Draft => "Fast compile: lights bake without shadows.",
            Self::Release => "Full bake: lights cast shadows (occlusion tested).",
        }
    }
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
    /// Exact body envelopes used to compile PXBSP collision hulls one and two.
    pub body_hulls: [CookedBodyHull; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrushWorldCookError {
    EmptyStaticWorld,
    InvalidBrush {
        brush: usize,
        face: Option<usize>,
    },
    MissingMover {
        brush: usize,
        node: NodeId,
    },
    BrushOwnerIsNotDoor {
        brush: usize,
        node: NodeId,
    },
    LiquidMover {
        brush: usize,
        node: NodeId,
        contents: BrushContents,
    },
    UnsupportedMoverTransform(NodeId),
    MoverOriginOutOfRange(NodeId),
    MoverOriginInSolid(NodeId),
    InvalidPlayerSpawnTransform(NodeId),
    PlayerSpawnInSolid(NodeId),
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
    Bsp29GeometryRead {
        path: String,
        error: String,
    },
    Bsp29Geometry {
        path: String,
        error: String,
    },
    /// Submodel table overflowed while compiling this mover's brush model.
    ModelIndexOverflow(NodeId),
    /// Texture asset table overflowed while interning this material's texture.
    /// `None` is the built-in default brush texture, which has no resource.
    TextureAssetOverflow {
        material: Option<ResourceId>,
    },
    Pack(BrushPackError),
    Collision(CollisionHullCompileError),
    /// Lighting bake failure, with the PointLight node responsible when the
    /// inner error names a light index.
    Light {
        node: Option<NodeId>,
        error: BrushLightError,
    },
    Pxbsp(PxbspBuildError),
}

impl fmt::Display for BrushWorldCookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyStaticWorld => formatter.write_str("scene has no static world brushes"),
            Self::InvalidBrush { brush, face } => match face {
                Some(face) => write!(formatter, "brush {brush} has invalid face {face}"),
                None => write!(formatter, "brush {brush} does not enclose a valid solid"),
            },
            Self::MissingMover { brush, node } => {
                write!(
                    formatter,
                    "brush {brush} references missing mover node {node:?}"
                )
            }
            Self::BrushOwnerIsNotDoor { brush, node } => write!(
                formatter,
                "brush {brush} is bound to node {node:?}, which is not a Door"
            ),
            Self::LiquidMover {
                brush,
                node,
                contents,
            } => write!(
                formatter,
                "brush {brush} uses {} contents but is bound to Door node {node:?}; liquid movers are unsupported",
                contents.label()
            ),
            Self::UnsupportedMoverTransform(node) => write!(
                formatter,
                "Door node {node:?} uses rotation or scale unsupported by brush movers"
            ),
            Self::MoverOriginOutOfRange(node) => {
                write!(
                    formatter,
                    "Door node {node:?} origin is outside the PXBSP range"
                )
            }
            Self::MoverOriginInSolid(node) => {
                write!(
                    formatter,
                    "Door node {node:?} origin is inside solid world geometry"
                )
            }
            Self::InvalidPlayerSpawnTransform(node) => write!(
                formatter,
                "Player Spawn node {node:?} has a non-finite or out-of-range transform"
            ),
            Self::PlayerSpawnInSolid(node) => {
                write!(
                    formatter,
                    "Player Spawn node {node:?} body overlaps solid geometry"
                )
            }
            Self::InvalidDoorMotion { node, error } => {
                write!(
                    formatter,
                    "Door node {node:?} has invalid motion: {error:?}"
                )
            }
            Self::InvalidWorldTree => formatter.write_str("compiled BSP world tree is invalid"),
            Self::MissingMaterial(material) => {
                write!(formatter, "brush references missing material {material:?}")
            }
            Self::ResourceIsNotMaterial(resource) => {
                write!(formatter, "brush resource {resource:?} is not a Material")
            }
            Self::MaterialTexture { material, error } => {
                write!(formatter, "material {material:?} texture failed: {error}")
            }
            Self::InvalidTexture { material, error } => match material {
                Some(material) => write!(formatter, "material {material:?} is invalid: {error}"),
                None => write!(formatter, "default brush texture is invalid: {error}"),
            },
            Self::Bsp29GeometryRead { path, error } => {
                write!(formatter, "could not read BSP29 geometry sidecar {path}: {error}")
            }
            Self::Bsp29Geometry { path, error } => {
                write!(formatter, "BSP29 geometry sidecar {path} is invalid: {error}")
            }
            Self::ModelIndexOverflow(node) => {
                write!(formatter, "PXBSP model index exceeds u16 at node {node:?}")
            }
            Self::TextureAssetOverflow { material } => match material {
                Some(material) => write!(
                    formatter,
                    "PXBSP texture asset index exceeds u16 at material {material:?}"
                ),
                None => formatter.write_str("PXBSP texture asset index exceeds u16"),
            },
            Self::Pack(error) => write!(formatter, "BSP geometry packing failed: {error:?}"),
            Self::Collision(error) => write!(formatter, "BSP collision compile failed: {error:?}"),
            Self::Light { error, .. } => write!(formatter, "BSP lighting failed: {error:?}"),
            Self::Pxbsp(error) => write!(formatter, "PXBSP assembly failed: {error:?}"),
        }
    }
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

impl From<PxbspBuildError> for BrushWorldCookError {
    fn from(value: PxbspBuildError) -> Self {
        Self::Pxbsp(value)
    }
}

fn authored_body_hulls(project: &ProjectDocument) -> [CookedBodyHull; 2] {
    let scene = project.active_scene();
    let character_resources: Vec<_> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character),
            _ => None,
        })
        .collect();
    let mut bodies = Vec::new();
    for node in scene.nodes() {
        match &node.kind {
            NodeKind::CharacterController {
                character,
                settings,
                ..
            } => {
                // No override means the body comes from the Character, the
                // same resolution the cook does. Missing both leaves the hull
                // to the other authored bodies.
                let body = settings.map(|s| (s.radius, s.height)).or_else(|| {
                    character
                        .and_then(|id| project.resource(id))
                        .and_then(|resource| match &resource.data {
                            ResourceData::Character(character) => {
                                Some((character.radius, character.height))
                            }
                            _ => None,
                        })
                });
                if let Some((radius, height)) = body {
                    if radius > 0 && height > 0 {
                        bodies.push((i32::from(radius), i32::from(height)));
                    }
                }
            }
            NodeKind::SpawnPoint { player: true, .. } => {
                if let Some(body) = player_spawn_body(project, node, &character_resources) {
                    bodies.push(body);
                }
            }
            _ => {}
        }
    }
    if bodies.is_empty() {
        bodies.push((DEBUG_BODY_RADIUS, DEBUG_BODY_HEIGHT));
    }

    cooked_body_hulls_for(&bodies)
}

fn cooked_body_hulls_for(bodies: &[(i32, i32)]) -> [CookedBodyHull; 2] {
    let largest_authored = bodies
        .iter()
        .copied()
        .max_by_key(|&(radius, height)| {
            (
                (radius as u32).saturating_mul(height as u32),
                radius,
                height,
            )
        })
        .expect("body list is non-empty");
    // PXBSP has two body hulls. Keep hull one useful for the ordinary body
    // cluster rather than shrinking it to the smallest authored NPC: remove
    // the complete largest-envelope cluster, then cover every remaining body
    // component-wise. Small and standard actors share this tighter hull; the
    // outlier cluster uses hull two. With only one distinct authored envelope,
    // hull one is exact.
    let mut standard = (0, 0);
    for &(radius, height) in bodies {
        if (radius, height) == largest_authored {
            continue;
        }
        standard.0 = standard.0.max(radius);
        standard.1 = standard.1.max(height);
    }
    if standard.0 <= 0 || standard.1 <= 0 {
        standard = largest_authored;
    }
    let largest = bodies.iter().copied().fold(
        (LEGACY_BIG_HULL_RADIUS, LEGACY_BIG_HULL_HEIGHT),
        |(max_radius, max_height), (radius, height)| {
            (max_radius.max(radius), max_height.max(height))
        },
    );
    [
        CookedBodyHull::new(1, standard.0, standard.1),
        CookedBodyHull::new(2, largest.0, largest.1),
    ]
}

fn player_spawn_body(
    project: &ProjectDocument,
    node: &crate::SceneNode,
    character_resources: &[&crate::CharacterResource],
) -> Option<(i32, i32)> {
    let NodeKind::SpawnPoint {
        player: true,
        character,
    } = &node.kind
    else {
        return None;
    };
    let resolved = character
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character),
            _ => None,
        })
        .or_else(|| {
            (character.is_none() && character_resources.len() == 1).then(|| character_resources[0])
        });
    match resolved {
        Some(character) if character.radius > 0 && character.height > 0 => {
            Some((i32::from(character.radius), i32::from(character.height)))
        }
        Some(_) => None,
        // Characterless BSP projects use the debug motor.
        None => Some((DEBUG_BODY_RADIUS, DEBUG_BODY_HEIGHT)),
    }
}

fn collision_hull_bounds(body_hulls: [CookedBodyHull; 2]) -> [CollisionHullBounds; 3] {
    let bounds = |hull: CookedBodyHull| CollisionHullBounds {
        mins: [-hull.radius, 0, -hull.radius],
        maxs: [hull.radius, hull.height, hull.radius],
    };
    [
        CollisionHullBounds::POINT,
        bounds(body_hulls[0]),
        bounds(body_hulls[1]),
    ]
}

/// Compile the active brush scene, including every brush-bound Door submodel.
pub fn compile_brush_world(
    project: &ProjectDocument,
    options: BrushWorldCookOptions<'_>,
) -> Result<CompiledBrushWorld, BrushWorldCookError> {
    let scene = project.active_scene();
    let body_hulls = authored_body_hulls(project);
    let collision_hulls = collision_hull_bounds(body_hulls);
    let mut static_brushes = Vec::new();
    for (brush_index, brush) in scene.brushes.iter().enumerate() {
        let solved = brush.solve();
        if !solved.is_valid() {
            let face = brush.faces.iter().enumerate().find_map(|(face, authored)| {
                (crate::brush::Plane::from_points(authored.points).is_none()
                    || solved.polygons.get(face).is_none_or(Option::is_none))
                .then_some(face)
            });
            return Err(BrushWorldCookError::InvalidBrush {
                brush: brush_index,
                face,
            });
        }
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
                if !brush.contents.is_solid() {
                    return Err(BrushWorldCookError::LiquidMover {
                        brush: brush_index,
                        node,
                        contents: brush.contents,
                    });
                }
            }
        }
    }
    if static_brushes.is_empty() {
        return Err(BrushWorldCookError::EmptyStaticWorld);
    }

    // Only structural solids occlude baked light. Liquid boundary faces are
    // rendered and classified, but the volume must not cast an opaque block
    // through every surface submerged inside it.
    let all_brushes: Vec<_> = scene
        .brushes
        .iter()
        .filter(|brush| brush.contents.is_solid())
        .cloned()
        .collect();
    let (lights, light_nodes) = scene_lights(scene);
    let material_tints = material_tints(project);
    let texture_dims = brush_texture_dims(project, scene, &options);
    let (world_geometry, world_collision) = if let Some(relative) = &project.bsp29_geometry_path {
        let path = Path::new(relative);
        if path.is_absolute() {
            return Err(BrushWorldCookError::Bsp29Geometry {
                path: relative.clone(),
                error: "path must be relative to the project directory".to_string(),
            });
        }
        let source_path = options.project_root.join(path);
        let bytes = std::fs::read(&source_path).map_err(|error| {
            BrushWorldCookError::Bsp29GeometryRead {
                path: relative.clone(),
                error: error.to_string(),
            }
        })?;
        let material = static_brushes
            .iter()
            .flat_map(|brush| &brush.faces)
            .find_map(|face| face.material);
        crate::quake_bsp29::import_quake_bsp29_world(
            &bytes,
            f64::from(project.bsp29_geometry_scale)
                / f64::from(crate::units::WORLD_UNIT_DIVISOR),
            material,
        )
        .map_err(|error| BrushWorldCookError::Bsp29Geometry {
            path: relative.clone(),
            error: error.to_string(),
        })?
    } else {
        compile_model(
            &static_brushes,
            &all_brushes,
            &lights,
            &light_nodes,
            &material_tints,
            &texture_dims,
            options.mode,
            options.ambient,
            &collision_hulls,
        )?
    };
    let collision_planes = RecordSlice::<Plane>::new(&world_collision.planes)
        .ok_or(BrushWorldCookError::InvalidWorldTree)?;
    let collision_nodes = RecordSlice::<ClipNode>::new(&world_collision.clipnodes)
        .ok_or(BrushWorldCookError::InvalidWorldTree)?;

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
            &light_nodes,
            &material_tints,
            &texture_dims,
            options.mode,
            options.ambient,
            &collision_hulls,
        )?;
        let model_index = u16::try_from(submodels.len() + 1)
            .map_err(|_| BrushWorldCookError::ModelIndexOverflow(node.id))?;
        let leaf_probe = model_center_world_q12(origin, geometry.mins, geometry.maxs);
        // ponytail: PXBSP links one representative leaf. Replace this
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

    let character_resources: Vec<_> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character),
            _ => None,
        })
        .collect();
    for node in scene.nodes() {
        if !matches!(node.kind, NodeKind::SpawnPoint { player: true, .. }) {
            continue;
        }
        let origin = point_entity_origin(node.id, node.transform.translation)?;
        let leaf = packed_point_leaf(&world_geometry, origin)?;
        if leaf == 0 {
            return Err(BrushWorldCookError::PlayerSpawnInSolid(node.id));
        }
        if let Some((radius, height)) = player_spawn_body(project, node, &character_resources) {
            let hull_index = select_body_hull(&body_hulls, radius, height)
                .ok_or(BrushWorldCookError::PlayerSpawnInSolid(node.id))?;
            let head_node = *world_collision
                .head_nodes
                .get(hull_index)
                .ok_or(BrushWorldCookError::InvalidWorldTree)?;
            if CollisionHull::new(collision_planes, collision_nodes, head_node)
                .point_contents(origin)
                .is_none_or(|contents| contents == CONTENTS_SOLID)
            {
                return Err(BrushWorldCookError::PlayerSpawnInSolid(node.id));
            }
        }
        let angles = node
            .transform
            .rotation_degrees
            .map(|degrees| crate::spatial::euler_degrees_to_q12(degrees) as i16);
        entities.push(PxbspEntityInput {
            entity: PxbspEntity {
                class_id: entity_class::PLAYER_SPAWN,
                flags: entity_flags::ENABLED,
                model: u16::MAX,
                leaf,
                origin,
                angles: psx_bsp::Vec3I16 {
                    x: angles[0],
                    y: angles[1],
                    z: angles[2],
                },
                ..PxbspEntity::default()
            },
            payload: Vec::new(),
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
        body_hulls,
    })
}

fn compile_model(
    brushes: &[Brush],
    light_occluders: &[Brush],
    lights: &[BrushPointLight],
    light_nodes: &[NodeId],
    material_tints: &[BrushMaterialTint],
    texture_dims: &std::collections::HashMap<Option<ResourceId>, [u16; 2]>,
    mode: BrushWorldCookMode,
    ambient: [u8; 3],
    collision_hulls: &[CollisionHullBounds; 3],
) -> Result<(PackedBspGeometry, CompiledCollisionHulls), BrushWorldCookError> {
    let surfaces = compile_csg_surfaces(brushes);
    // qbsp parity: EVERY face is capped to SURFACE_EXTENT_UNITS before
    // the BSP build, lights or not, exactly like id's qbsp splits faces
    // to its lightmap surface extents. Small faces are what make the
    // runtime's GTE emitter safe at the eye plane (a crossing triangle
    // saturates wider than the GPU's 1023px draw limit and is skipped
    // as a sub-face-sized hole instead of rasterizing as a screen-wide
    // wrap), and they are also what lets the vertex bake resolve
    // hotspots and shadow edges mid-face. This supersedes the old
    // light-gated LIGHT_SUBDIVISION_UNITS pass: the cap is finer than
    // the light grid was, so lit and lightless scenes now share one
    // subdivision rule.
    let surfaces = subdivide_surfaces_to_extent(surfaces, ENGINE_SURFACE_EXTENT_UNITS);
    let mut bsp = build_surface_bsp(&surfaces);
    let portals = portalize_surface_bsp(&bsp);
    classify_bsp_leaves(&mut bsp, &portals, brushes);
    // Lighting bakes in BOTH modes so lights always interact with
    // brushes out of the box: Draft skips only the occlusion (shadow)
    // tests, the expensive part; Release adds shadows. A scene with no
    // lights at all keeps the historic fullbright look instead of
    // dropping to bare ambient.
    let geometry = if lights.is_empty() {
        pack_bsp_geometry(&bsp, &portals, BspLighting::Fullbright, texture_dims)?
    } else {
        let occluders: &[Brush] = match mode {
            BrushWorldCookMode::Draft => &[],
            BrushWorldCookMode::Release => light_occluders,
        };
        let lighting =
            bake_brush_vertex_lighting(&bsp.surfaces, occluders, ambient, lights, material_tints)
                .map_err(|error| BrushWorldCookError::Light {
                // `translate_lights` preserves scene order, so the reported
                // light index indexes the same list `scene_lights` built.
                node: match error {
                    BrushLightError::InvalidLight(index) => light_nodes.get(index).copied(),
                },
                error,
            })?;
        pack_bsp_geometry(&bsp, &portals, BspLighting::Baked(&lighting), texture_dims)?
    };
    let collision = compile_collision_hulls(brushes, collision_hulls)?;
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

fn point_entity_origin(
    node: NodeId,
    translation: [f32; 3],
) -> Result<Vec3I32, BrushWorldCookError> {
    if !translation.into_iter().all(f32::is_finite) {
        return Err(BrushWorldCookError::InvalidPlayerSpawnTransform(node));
    }
    let mut origin = [0; 3];
    for (output, value) in origin.iter_mut().zip(translation) {
        let scaled = f64::from(value) * 4096.0;
        if scaled < f64::from(i32::MIN) || scaled > f64::from(i32::MAX) {
            return Err(BrushWorldCookError::InvalidPlayerSpawnTransform(node));
        }
        *output = scaled.round() as i32;
    }
    Ok(Vec3I32 {
        x: origin[0],
        y: origin[1],
        z: origin[2],
    })
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

/// Point lights in scene order, paired with the authoring node each came
/// from so a bake failure can name the light the author has to fix.
fn scene_lights(scene: &Scene) -> (Vec<BrushPointLight>, Vec<NodeId>) {
    // Radius is authored in sector units. Use the scene's World sector size
    // so the bake agrees with the runtime PlaytestLight records, which scale
    // by the same value (cook_props_lights::push_point_light).
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .map(f64::from)
        .unwrap_or(DEFAULT_LIGHT_RADIUS_UNITS);
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
            Some((
                BrushPointLight {
                    position: node.transform.translation.map(f64::from),
                    radius: f64::from(*radius) * radius_units,
                    intensity_q8: (f64::from(*intensity) * 256.0)
                        .round()
                        .clamp(0.0, u16::MAX as f64) as u16,
                    color: *color,
                },
                node.id,
            ))
        })
        .unzip()
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

/// Texture repeat sizes per face material, parsed ahead of geometry
/// packing so surface UVs can rebase onto the u8 window (see
/// `brush::rebase_texel_uvs`). Unresolvable textures are simply
/// absent (the pack keeps the historic wrap for them); the real error
/// surfaces later in `resolve_materials`.
fn brush_texture_dims(
    project: &ProjectDocument,
    scene: &Scene,
    options: &BrushWorldCookOptions<'_>,
) -> std::collections::HashMap<Option<ResourceId>, [u16; 2]> {
    let mut dims = std::collections::HashMap::new();
    // The material-less fallback is the built-in 8x8 flat white.
    dims.insert(None, [8, 8]);
    for brush in &scene.brushes {
        for face in &brush.faces {
            let Some(id) = face.material else { continue };
            if dims.contains_key(&Some(id)) {
                continue;
            }
            let Ok(Some((_, bytes))) =
                resolve_material_texture_psxt(project, id, options.project_root)
            else {
                continue;
            };
            let Ok(parsed) = psx_asset::Texture::from_bytes(&bytes) else {
                continue;
            };
            dims.insert(Some(id), [parsed.width(), parsed.height()]);
        }
    }
    dims
}

fn resolve_materials(
    project: &ProjectDocument,
    slots: &[Option<ResourceId>],
    options: &BrushWorldCookOptions<'_>,
) -> Result<(Vec<PxbspMaterial>, Vec<CompiledBrushTexture>), BrushWorldCookError> {
    let mut textures = Vec::new();
    let mut materials = Vec::with_capacity(slots.len());
    for &slot in slots {
        let (key, bytes, tint, blend, sidedness, animation, layered_sky) = match slot {
            None => (
                "@brush-flat-white".to_string(),
                flat_white_psxt(),
                [128; 3],
                PsxBlendMode::Opaque,
                MaterialFaceSidedness::Front,
                crate::MaterialAnimation::default(),
                false,
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
                    material.layered_sky,
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
            layered_sky,
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
    let index = u16::try_from(textures.len())
        .map_err(|_| BrushWorldCookError::TextureAssetOverflow { material })?;
    let asset_id = base
        .checked_add(index)
        .ok_or(BrushWorldCookError::TextureAssetOverflow { material })?;
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
    layered_sky: bool,
    material: Option<ResourceId>,
) -> Result<PxbspMaterial, BrushWorldCookError> {
    let flags = match sidedness {
        MaterialFaceSidedness::Front => material_flags::FACE_FRONT,
        MaterialFaceSidedness::Back => material_flags::FACE_BACK,
        MaterialFaceSidedness::Both => material_flags::FACE_BOTH,
    } | if layered_sky {
        material_flags::LAYERED_SKY
    } else {
        0
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
    use crate::{brush::BrushContents, MaterialResource, Transform3};
    use psx_bsp::mover::BrushDoorSet;
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
        let spawn = scene.add_node(
            NodeId::ROOT,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        scene.node_mut(spawn).expect("spawn").transform = Transform3 {
            translation: [128.5, 128.0, 128.0],
            rotation_degrees: [0.0, 90.0, 0.0],
            ..Transform3::default()
        };

        project
    }

    #[test]
    fn draft_mode_bakes_lighting_when_lights_exist() {
        // A light inside the hollow room: Draft must bake it (without
        // shadows) instead of packing fullbright, so lights interact
        // with brushes out of the box. Vertex colors near the light are
        // brighter than far ones and none stay at the fullbright
        // sentinel; a lightless scene keeps fullbright.
        let mut project = authored_project();
        let scene = project.active_scene_mut();
        let light = scene.add_node(
            NodeId::ROOT,
            "Light",
            NodeKind::PointLight {
                color: [255, 255, 255],
                intensity: 1.0,
                radius: 2.0,
            },
        );
        scene.node_mut(light).expect("light").transform.translation = [512.0, 256.0, 512.0];
        let lit = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode: BrushWorldCookMode::Draft,
                ambient: [24; 3],
                texture_asset_base: 40,
            },
        )
        .expect("draft cook with a light");
        let unlit = authored_world(BrushWorldCookMode::Draft);
        assert_ne!(
            lit.pxbsp.bytes, unlit.pxbsp.bytes,
            "the light must change the packed vertex stream"
        );
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
    fn small_standard_and_large_bodies_use_two_deterministic_cooked_hulls() {
        let hulls = cooked_body_hulls_for(&[(8, 32), (16, 56), (32, 96)]);
        assert_eq!(
            hulls,
            [
                CookedBodyHull::new(1, 16, 56),
                CookedBodyHull::new(2, 32, 96),
            ]
        );
        assert_eq!(select_body_hull(&hulls, 8, 32), Some(1));
        assert_eq!(select_body_hull(&hulls, 16, 56), Some(1));
        assert_eq!(select_body_hull(&hulls, 32, 96), Some(2));
        assert_eq!(
            cooked_body_hulls_for(&[(32, 96), (8, 32), (16, 56)]),
            hulls,
            "authored traversal order must not change cooked envelopes"
        );
        assert_eq!(
            cooked_body_hulls_for(&[(32, 96), (8, 32), (32, 96), (16, 56)]),
            hulls,
            "duplicate largest bodies stay in the outlier cluster"
        );
        assert_eq!(
            cooked_body_hulls_for(&[(16, 56), (32, 96), (8, 32), (32, 96)]),
            hulls,
            "duplicate-largest clustering is permutation independent"
        );
        assert_eq!(
            cooked_body_hulls_for(&[(20, 72), (20, 72), (20, 72)]),
            [
                CookedBodyHull::new(1, 20, 72),
                CookedBodyHull::new(2, 20, 72),
            ],
            "an all-identical cluster retains an exact tight hull (the
             legacy floor is a probe-sized 2x6 at engine scale)"
        );
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
        assert_eq!(entities.len(), 2);
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

        let spawn = entities.get(1).expect("player spawn");
        assert_eq!(spawn.class_id, entity_class::PLAYER_SPAWN);
        assert_eq!(spawn.flags, entity_flags::ENABLED);
        assert_eq!(spawn.model, u16::MAX);
        assert_ne!(spawn.leaf, 0);
        assert_eq!(
            spawn.origin,
            Vec3I32 {
                x: 128 * 4096 + 2048,
                y: 128 * 4096,
                z: 128 * 4096,
            }
        );
        assert_eq!(spawn.angles.y, 1024);
        assert_eq!(entities.payload(1), Some(&[][..]));

        let mut doors = BrushDoorSet::<4>::default();
        doors.init_from_map(&map).expect("runtime doors");
        assert_eq!(doors.len(), 1);
        let hull = map
            .model_collision_hull(1, 0)
            .expect("door point hull")
            .transformed(doors.get(0).expect("runtime door").transform());
        assert_eq!(
            hull.point_contents(Vec3I32 {
                x: 512 * 4096,
                y: 96 * 4096,
                z: 512 * 4096,
            }),
            Some(psx_bsp::collision::CONTENTS_SOLID)
        );
        doors.get_mut(0).expect("runtime door").set_open(true);
        for _ in 0..60 {
            assert_eq!(doors.tick(), 1);
        }
        let open_hull = map
            .model_collision_hull(1, 0)
            .expect("door point hull")
            .transformed(doors.get(0).expect("runtime door").transform());
        assert_eq!(
            open_hull.point_contents(Vec3I32 {
                x: 512 * 4096,
                y: 96 * 4096,
                z: 512 * 4096,
            }),
            Some(psx_bsp::collision::CONTENTS_EMPTY)
        );
    }

    /// Hull 0 is served from the render BSP at runtime (Quake's hull 0);
    /// the cooked per-brush clipnode chain stays the reference. Point
    /// contents must agree everywhere inside the world, for the world
    /// model and for a mover model.
    #[test]
    fn render_bsp_point_hull_matches_the_cooked_clipnode_chain() {
        let compiled = authored_world(BrushWorldCookMode::Draft);
        let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
        map.load(9, &mut SliceReader::new(&compiled.pxbsp.bytes))
            .expect("resident PXBSP");
        for model_index in 0..map.brush_models().len() {
            let model = map.brush_models().get(model_index).expect("model");
            let render = map
                .model_collision_hull(model_index, 0)
                .expect("render-served point hull");
            let chain = psx_bsp::collision::CollisionHull::new(
                map.planes(),
                map.clip_nodes(),
                model.head_nodes[1],
            );
            let (mut checked, mut solid) = (0usize, 0usize);
            let mut y = model.mins.y as i32 + 1;
            while y < model.maxs.y as i32 {
                let mut z = model.mins.z as i32 + 1;
                while z < model.maxs.z as i32 {
                    let mut x = model.mins.x as i32 + 1;
                    while x < model.maxs.x as i32 {
                        let point = Vec3I32 {
                            x: x * 4096,
                            y: y * 4096,
                            z: z * 4096,
                        };
                        let expected = chain.point_contents(point);
                        assert_eq!(
                            render.point_contents(point),
                            expected,
                            "model {model_index} contents at ({x}, {y}, {z})"
                        );
                        checked += 1;
                        if expected == Some(psx_bsp::collision::CONTENTS_SOLID) {
                            solid += 1;
                        }
                        x += 8;
                    }
                    z += 8;
                }
                y += 8;
            }
            assert!(
                checked > 100 && solid > 0,
                "model {model_index}: {checked} points, {solid} solid"
            );
        }
    }

    #[test]
    fn cooked_world_point_hull_preserves_water_slime_and_lava_codes() {
        for (contents, expected) in [
            (BrushContents::Water, psx_bsp::collision::CONTENTS_WATER),
            (BrushContents::Slime, psx_bsp::collision::CONTENTS_SLIME),
            (BrushContents::Lava, psx_bsp::collision::CONTENTS_LAVA),
        ] {
            let mut project = authored_project();
            let material = project
                .resources
                .iter()
                .find(|resource| matches!(resource.data, ResourceData::Material(_)))
                .expect("material")
                .id;
            let mut liquid = Brush::cuboid([640, 64, 640], [896, 160, 896]);
            liquid.contents = contents;
            for face in &mut liquid.faces {
                face.material = Some(material);
            }
            project.active_scene_mut().brushes.push(liquid);
            let compiled = compile_brush_world(
                &project,
                BrushWorldCookOptions {
                    project_root: Path::new("."),
                    mode: BrushWorldCookMode::Draft,
                    ambient: [24; 3],
                    texture_asset_base: 40,
                },
            )
            .expect("liquid world");
            let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
            map.load(0, &mut SliceReader::new(&compiled.pxbsp.bytes))
                .expect("resident PXBSP");
            let hull = map.model_collision_hull(0, 0).expect("point hull");
            assert_eq!(
                hull.point_contents(Vec3I32 {
                    x: 768 * 4096,
                    y: 96 * 4096,
                    z: 768 * 4096,
                }),
                Some(expected),
                "{} point contents",
                contents.label()
            );
            assert_eq!(
                hull.point_contents(Vec3I32 {
                    x: 512 * 4096,
                    y: 96 * 4096,
                    z: 768 * 4096,
                }),
                Some(psx_bsp::collision::CONTENTS_EMPTY)
            );
        }
    }

    #[test]
    fn player_spawn_rejects_empty_point_when_authored_body_overlaps_wall() {
        let mut project = authored_project();
        let spawn = project
            .active_scene()
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::SpawnPoint { player: true, .. }))
            .expect("player spawn")
            .id;
        project
            .active_scene_mut()
            .node_mut(spawn)
            .expect("player spawn")
            .transform
            .translation = [65.0, 65.0, 128.0];
        crate::units::scale_project_to_engine_units(&mut project);

        assert_eq!(
            compile_brush_world(
                &project,
                BrushWorldCookOptions {
                    project_root: Path::new("."),
                    mode: BrushWorldCookMode::Draft,
                    ambient: [24; 3],
                    texture_asset_base: 0,
                },
            )
            .expect_err("body overlaps the inner X wall even though its origin is empty"),
            BrushWorldCookError::PlayerSpawnInSolid(spawn)
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
    fn liquid_brush_bound_to_door_fails_with_authored_target() {
        let mut project = authored_project();
        let brush = project.active_scene().brushes.len() - 1;
        let node = project.active_scene().brushes[brush]
            .mover
            .expect("door binding");
        project.active_scene_mut().brushes[brush].contents = BrushContents::Water;
        let error = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode: BrushWorldCookMode::Draft,
                ambient: [32; 3],
                texture_asset_base: 0,
            },
        )
        .expect_err("liquid mover");
        assert_eq!(
            error,
            BrushWorldCookError::LiquidMover {
                brush,
                node,
                contents: BrushContents::Water,
            }
        );
        assert!(error.to_string().contains("Water contents"));
    }

    #[test]
    fn invalid_brush_reports_authored_index_and_face() {
        let mut project = ProjectDocument::new("invalid brush");
        let world = Brush::cuboid([0, 0, 0], [256, 256, 256]);
        let mut invalid = Brush::cuboid([512, 0, 0], [768, 256, 256]);
        invalid.faces.truncate(3);
        invalid.faces[0].points = [[512, 0, 0]; 3];
        project.active_scene_mut().brushes = vec![world, invalid];

        let error = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode: BrushWorldCookMode::Draft,
                ambient: [32; 3],
                texture_asset_base: 0,
            },
        )
        .expect_err("invalid brush");

        assert_eq!(
            error,
            BrushWorldCookError::InvalidBrush {
                brush: 1,
                face: Some(0),
            }
        );
        assert_eq!(error.to_string(), "brush 1 has invalid face 0");
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
