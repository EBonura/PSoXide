//! Project-level brush world compilation into one complete PXBSP artifact.

use std::fmt;
use std::path::Path;

use crate::brush::{paraxial_uv, rebase_texel_uvs, Brush, BrushContents, BRUSH_UV_UNITS_PER_TEXEL};
use crate::brush_collision_hulls::{
    compile_collision_hulls, CollisionHullBounds, CollisionHullCompileError, CompiledCollisionHulls,
};
use crate::brush_compile::{
    build_surface_bsp, compile_authored_surfaces, compile_csg_surfaces, pack_normalized_plane,
    replace_bsp_render_surfaces, subdivide_polygon_for_lighting, subdivide_surfaces_to_budget,
    CompiledSurface, CompiledSurfaceBsp,
};
use crate::brush_light::{
    bake_brush_vertex_lighting, BrushLightError, BrushMaterialTint, BrushPointLight,
};
use crate::brush_pack::{
    pack_bsp_geometry_with_visibility, BrushPackError, BspLighting, BspVisibility,
    PackedBspGeometry,
};
use crate::brush_portal::{
    classify_bsp_leaves, fill_outside_bsp_leaves, portalize_surface_bsp, CompiledPortal,
    OutsideFillResult,
};
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
    entity_class, entity_flags, material_animation, material_blend, material_flags,
    PxbspBrushDestructible, PxbspBrushDoor, PxbspBrushDoorError, PxbspEntity, PxbspMaterial,
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
// A resident PS1 level must share 2 MiB with code, runtime state, models, and
// packet arenas. The wire format permits 32,767 faces, but that theoretical
// index limit is not a viable embedded-world memory budget: four baked-light
// vertices plus the face record already cost about 58 bytes per quad.
const MAX_RESIDENT_WORLD_FACES: usize = 6 * 1024;

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
    pub size: [u16; 2],
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
    /// Quake-style pointfile path when the occupied world reaches outside.
    pub leak_path: Vec<[i32; 3]>,
    /// Surfaces the u8 texel window forced apart, world and submodels summed.
    pub uv_window: UvWindowStats,
}

/// Cook-time census of surfaces whose texel span exceeded the GPU's u8 UV
/// window and had to be split (or could not be).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UvWindowStats {
    /// Surfaces split because a texel axis spanned more than 255.
    pub split_surfaces: usize,
    /// Extra render faces those splits produced.
    pub added_faces: usize,
    /// Surfaces still wrapping: unknown texture dimensions, or the split
    /// floor was reached without fitting.
    pub unfixable_surfaces: usize,
}

impl UvWindowStats {
    fn add(&mut self, other: Self) {
        self.split_surfaces += other.split_surfaces;
        self.added_faces += other.added_faces;
        self.unfixable_surfaces += other.unfixable_surfaces;
    }
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
    InvalidDestructibleHealth(NodeId),
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
    /// Fitting surfaces to the u8 texel window pushed the render-face count
    /// past the resident budget that the extent coarsening had just met.
    UvWindowFaceBudget {
        faces: usize,
        max: usize,
        split_surfaces: usize,
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
                "brush {brush} is bound to node {node:?}, which is neither a Door nor a Destructible"
            ),
            Self::LiquidMover {
                brush,
                node,
                contents,
            } => write!(
                formatter,
                "brush {brush} uses {} contents but is bound to brush-model node {node:?}; liquid submodels are unsupported",
                contents.label()
            ),
            Self::UnsupportedMoverTransform(node) => write!(
                formatter,
                "brush-model node {node:?} uses rotation or scale unsupported by brush submodels"
            ),
            Self::MoverOriginOutOfRange(node) => {
                write!(
                    formatter,
                    "brush-model node {node:?} origin is outside the PXBSP range"
                )
            }
            Self::MoverOriginInSolid(node) => {
                write!(
                    formatter,
                    "brush-model node {node:?} origin is inside solid world geometry"
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
            Self::InvalidDestructibleHealth(node) => write!(
                formatter,
                "Destructible node {node:?} must have at least one health point"
            ),
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
            Self::UvWindowFaceBudget { faces, max, split_surfaces } => write!(
                formatter,
                "{faces} render faces exceed the {max} resident budget after splitting \
                 {split_surfaces} surfaces whose texel span exceeded the 255-texel UV window; \
                 reduce texture scale on the largest faces or raise the budget"
            ),
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
                    } | NodeKind::Destructible { .. }
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
    let uv_window_skip = sky_aperture_materials(project);
    let occupant_points = player_occupant_points(scene);
    let (mut world_geometry, world_collision, leak_path, mut uv_window) = compile_model(
        &static_brushes,
        &all_brushes,
        &occupant_points,
        &lights,
        &light_nodes,
        &material_tints,
        &texture_dims,
        &uv_window_skip,
        options.mode,
        options.ambient,
        &collision_hulls,
    )?;
    let collision_planes = RecordSlice::<Plane>::new(&world_collision.planes)
        .ok_or(BrushWorldCookError::InvalidWorldTree)?;
    let collision_nodes = RecordSlice::<ClipNode>::new(&world_collision.clipnodes)
        .ok_or(BrushWorldCookError::InvalidWorldTree)?;

    let mut submodels = Vec::new();
    let mut movers = Vec::new();
    let mut entities = Vec::new();
    let destructible_nodes: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter_map(|node| matches!(node.kind, NodeKind::Destructible { .. }).then_some(node.id))
        .collect();
    for node in scene.nodes() {
        enum OwnerKind {
            Door {
                start_open: bool,
                open_offset: [i16; 3],
                travel_ticks: u16,
                enabled: bool,
            },
            Destructible {
                max_health: u16,
                runtime_index: u16,
            },
        }
        let owner_kind = match &node.kind {
            NodeKind::Logic {
                kind:
                    LogicNodeKind::Door {
                        start_open,
                        open_offset,
                        travel_ticks,
                        ..
                    },
                enabled,
                ..
            } => OwnerKind::Door {
                start_open: *start_open,
                open_offset: *open_offset,
                travel_ticks: *travel_ticks,
                enabled: *enabled,
            },
            NodeKind::Destructible { max_health, .. } => {
                let runtime_index = destructible_nodes
                    .iter()
                    .position(|id| *id == node.id)
                    .and_then(|index| u16::try_from(index).ok())
                    .ok_or(BrushWorldCookError::ModelIndexOverflow(node.id))?;
                OwnerKind::Destructible {
                    max_health: *max_health,
                    runtime_index,
                }
            }
            _ => continue,
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
        let (geometry, collision, _, submodel_uv_window) = compile_model(
            &local_brushes,
            &local_occluders,
            &[],
            &local_lights,
            &light_nodes,
            &material_tints,
            &texture_dims,
            &uv_window_skip,
            options.mode,
            options.ambient,
            &collision_hulls,
        )?;
        uv_window.add(submodel_uv_window);
        let model_index = u16::try_from(submodels.len() + 1)
            .map_err(|_| BrushWorldCookError::ModelIndexOverflow(node.id))?;
        let leaf_probe = model_center_world_q12(origin, geometry.mins, geometry.maxs);
        // ponytail: PXBSP links one representative leaf. Replace this
        // with a touched-leaf span before full entity PVS activation ships.
        let leaf = packed_point_leaf(&world_geometry, leaf_probe)?;
        if leaf == 0 {
            return Err(BrushWorldCookError::MoverOriginInSolid(node.id));
        }
        let entity_origin = Vec3I32 {
            x: origin[0] * 4096,
            y: origin[1] * 4096,
            z: origin[2] * 4096,
        };
        match owner_kind {
            OwnerKind::Door {
                start_open,
                open_offset,
                travel_ticks,
                enabled,
            } => {
                let motion = PxbspBrushDoor::new(
                    Vec3I32 {
                        x: i32::from(open_offset[0]) * 4096,
                        y: i32::from(open_offset[1]) * 4096,
                        z: i32::from(open_offset[2]) * 4096,
                    },
                    travel_ticks,
                );
                motion
                    .validate()
                    .map_err(|error| BrushWorldCookError::InvalidDoorMotion {
                        node: node.id,
                        error,
                    })?;
                let mut flags = 0;
                if enabled {
                    flags |= entity_flags::ENABLED;
                }
                if start_open {
                    flags |= entity_flags::START_OPEN;
                }
                entities.push(PxbspEntityInput {
                    entity: PxbspEntity {
                        class_id: entity_class::BRUSH_DOOR,
                        flags,
                        model: model_index,
                        leaf,
                        origin: entity_origin,
                        ..PxbspEntity::default()
                    },
                    payload: motion.to_le_bytes().to_vec(),
                });
                movers.push(CompiledBrushMover {
                    node: node.id,
                    model_index,
                    origin,
                    open_offset: open_offset.map(i32::from),
                    travel_ticks,
                    start_open,
                    enabled,
                });
            }
            OwnerKind::Destructible {
                max_health,
                runtime_index,
            } => {
                if max_health == 0 {
                    return Err(BrushWorldCookError::InvalidDestructibleHealth(node.id));
                }
                let payload = PxbspBrushDestructible::new(runtime_index);
                entities.push(PxbspEntityInput {
                    entity: PxbspEntity {
                        class_id: entity_class::BRUSH_DESTRUCTIBLE,
                        // Visibility/collision enabled state is authoritative in
                        // the shared manifest record, not duplicated in PXBSP.
                        flags: 0,
                        model: model_index,
                        leaf,
                        origin: entity_origin,
                        ..PxbspEntity::default()
                    },
                    payload: payload.to_le_bytes().to_vec(),
                });
            }
        }
        submodels.push(PxbspSubmodel {
            geometry,
            collision,
            origin: origin.map(|value| value as i16),
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
                .and_then(|hull| hull.point_contents(origin))
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
    let page_texture_dims = page_local_texture_dims(
        project,
        &slots,
        &world_geometry,
        &submodels,
        &texture_dims,
        options.project_root,
    );
    mark_page_local_faces(&mut world_geometry, &page_texture_dims);
    for submodel in &mut submodels {
        mark_page_local_faces(&mut submodel.geometry, &page_texture_dims);
    }
    let (materials, textures) = resolve_materials(project, &slots, &options, &page_texture_dims)?;
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
        leak_path,
        uv_window,
    })
}

/// Largest texel span a packed surface may carry on one axis: the u8 UV
/// window minus nothing, since `rebase_texel_uvs` already parks the minimum
/// at or above zero.
const UV_WINDOW_TEXELS: f64 = 255.0;
/// Do not split a surface below this world extent chasing the window; a face
/// this small with a span over 255 texels has a texture scale that cannot be
/// represented and is reported as unfixable instead.
const UV_WINDOW_MIN_SPLIT_UNITS: f64 = 8.0;
/// Most pieces one surface may become while chasing the window. A 2048-unit
/// face at one texel per unit needs 64 pieces of 256 units; anything past
/// this is a texture scale the u8 window cannot carry and is reported.
const UV_WINDOW_MAX_PIECES: usize = 64;

/// Materials whose faces are sky apertures: they never submit a textured
/// polygon, so the UV-window pass leaves them alone.
fn sky_aperture_materials(
    project: &ProjectDocument,
) -> std::collections::HashSet<Option<ResourceId>> {
    project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Material(material) if material.sky_aperture => Some(Some(resource.id)),
            _ => None,
        })
        .collect()
}

/// Whether every packed texel coordinate of `surface` lands inside the u8
/// window, computed exactly as `brush_pack::pack_bsp_geometry` will. `None`
/// dimensions mean the packer keeps the historic wrap for this material.
fn surface_fits_uv_window(surface: &CompiledSurface, dims: Option<&[u16; 2]>) -> Option<bool> {
    let dims = dims?;
    let mut uvs: Vec<[f64; 2]> = surface
        .vertices
        .iter()
        .map(|&vertex| {
            let raw_uv = paraxial_uv(&surface.plane, vertex);
            surface.uv.apply([
                raw_uv[0] / BRUSH_UV_UNITS_PER_TEXEL,
                raw_uv[1] / BRUSH_UV_UNITS_PER_TEXEL,
            ])
        })
        .collect();
    rebase_texel_uvs(
        &mut uvs,
        [f64::from(dims[0].max(1)), f64::from(dims[1].max(1))],
    );
    Some(uvs.iter().all(|uv| {
        uv.iter().all(|&value| {
            value.is_finite() && value.round() >= 0.0 && value.round() <= UV_WINDOW_TEXELS
        })
    }))
}

/// Split every surface whose texel UVs do not fit the u8 window until they
/// do, halving along the widest world axis each round. Surfaces that fit
/// pass through untouched, so a level that never wrapped cooks identically.
fn fit_surfaces_to_uv_window(
    surfaces: Vec<CompiledSurface>,
    texture_dims: &std::collections::HashMap<Option<ResourceId>, [u16; 2]>,
    skip_materials: &std::collections::HashSet<Option<ResourceId>>,
) -> (Vec<CompiledSurface>, UvWindowStats) {
    let mut stats = UvWindowStats::default();
    let mut out = Vec::with_capacity(surfaces.len());
    let always_lit = [([0.0; 3], f64::INFINITY)];
    for surface in surfaces {
        // Sky apertures reveal the scene sky and never submit their polygon,
        // so their (deliberately huge, densely scaled) UVs are irrelevant.
        if skip_materials.contains(&surface.material) {
            out.push(surface);
            continue;
        }
        let dims = texture_dims.get(&surface.material);
        match surface_fits_uv_window(&surface, dims) {
            Some(true) => {
                out.push(surface);
                continue;
            }
            None => {
                stats.unfixable_surfaces += 1;
                out.push(surface);
                continue;
            }
            Some(false) => {}
        }
        let before = out.len();
        let mut queue = vec![surface.clone()];
        let mut overflow = false;
        while let Some(piece) = queue.pop() {
            if out.len() - before + queue.len() >= UV_WINDOW_MAX_PIECES {
                overflow = true;
                break;
            }
            if surface_fits_uv_window(&piece, dims) == Some(true) {
                out.push(piece);
                continue;
            }
            let mut min = [f64::MAX; 3];
            let mut max = [f64::MIN; 3];
            for vertex in &piece.vertices {
                for axis in 0..3 {
                    min[axis] = min[axis].min(vertex[axis]);
                    max[axis] = max[axis].max(vertex[axis]);
                }
            }
            let widest = (0..3).map(|axis| max[axis] - min[axis]).fold(0.0, f64::max);
            if widest < UV_WINDOW_MIN_SPLIT_UNITS {
                overflow = true;
                break;
            }
            let pieces =
                subdivide_polygon_for_lighting(piece.vertices.clone(), widest * 0.5, &always_lit);
            if pieces.len() < 2 {
                overflow = true;
                break;
            }
            for vertices in pieces {
                let mut child = piece.clone();
                child.vertices = vertices;
                queue.push(child);
            }
        }
        if overflow {
            // A face whose texture is denser than the window can represent
            // at any sane split count: keep it whole (it wraps, as before)
            // and report it instead of flooding the face budget.
            out.truncate(before);
            out.push(surface);
            stats.unfixable_surfaces += 1;
            continue;
        }
        stats.split_surfaces += 1;
        stats.added_faces += (out.len() - before).saturating_sub(1);
    }
    (out, stats)
}

fn player_occupant_points(scene: &Scene) -> Vec<[f64; 3]> {
    scene
        .nodes()
        .iter()
        .filter(|node| match node.kind {
            NodeKind::SpawnPoint { player: true, .. } => true,
            NodeKind::Entity => node.children.iter().any(|&child| {
                scene.node(child).is_some_and(|child| {
                    matches!(
                        child.kind,
                        NodeKind::CharacterController { player: true, .. }
                    )
                })
            }),
            _ => false,
        })
        .map(|node| {
            let mut point = node.transform.translation.map(f64::from);
            // Player transforms use a feet pivot, while Quake's outside fill
            // needs an occupant point strictly inside an empty leaf. A spawn
            // resting exactly on a floor plane descends to the solid/back
            // child (`d > 0` is Quake's front-side rule), so sample one world
            // unit inside the authored body instead of misclassifying a
            // sealed map as having no occupants.
            point[1] += 1.0;
            point
        })
        .filter(|point| point.iter().all(|value| value.is_finite()))
        .collect()
}

/// Recompute only the Quake-style pointfile for an authored BSP project.
///
/// This is the editor's inexpensive live diagnostic path: it shares the
/// cooker's exact CSG, BSP, portalization, leaf classification, and outside
/// fill, but deliberately skips lighting, texture IO, collision hulls, VIS,
/// and packing. All returned coordinates are authored/editor units.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrushWorldLeakDiagnostic {
    /// Complete Quake pointfile route from the occupant to the exterior.
    pub path: Vec<[i32; 3]>,
    /// Outer boundary of the connected coplanar empty-portal component that
    /// contains the narrowest portal crossed by `path`. A single BSP portal
    /// is only a leaf-partition fragment and can be much smaller than the
    /// authored breach; merging its component makes the editor marker match
    /// the visible empty channel while `path` remains authoritative.
    pub likely_opening: Vec<[i32; 3]>,
    /// Point in `path` that belongs to the seed portal inside
    /// `likely_opening`; camera navigation uses it to approach the region.
    pub likely_opening_path_index: Option<usize>,
}

impl BrushWorldLeakDiagnostic {
    pub fn is_empty(&self) -> bool {
        self.path.is_empty()
    }
}

pub fn diagnose_brush_world_leak(
    mut project: ProjectDocument,
) -> Result<BrushWorldLeakDiagnostic, BrushWorldCookError> {
    crate::units::scale_project_to_engine_units(&mut project);
    let scene = project.active_scene();
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
                    } | NodeKind::Destructible { .. }
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

    let occupant_points = player_occupant_points(scene);
    let (topology_surfaces, _) = compile_model_surfaces(&static_brushes);
    let (_, _, engine_diagnostic) =
        compile_model_topology(&topology_surfaces, &static_brushes, &occupant_points, false);
    let scale_point = |point: [i32; 3]| {
        point.map(|coordinate| coordinate.saturating_mul(crate::units::WORLD_UNIT_DIVISOR))
    };
    Ok(BrushWorldLeakDiagnostic {
        path: engine_diagnostic
            .path
            .into_iter()
            .map(scale_point)
            .collect(),
        likely_opening: engine_diagnostic
            .likely_opening
            .into_iter()
            .map(scale_point)
            .collect(),
        likely_opening_path_index: engine_diagnostic.likely_opening_path_index,
    })
}

fn compile_model(
    brushes: &[Brush],
    light_occluders: &[Brush],
    occupant_points: &[[f64; 3]],
    lights: &[BrushPointLight],
    light_nodes: &[NodeId],
    material_tints: &[BrushMaterialTint],
    texture_dims: &std::collections::HashMap<Option<ResourceId>, [u16; 2]>,
    uv_window_skip: &std::collections::HashSet<Option<ResourceId>>,
    mode: BrushWorldCookMode,
    ambient: [u8; 3],
    collision_hulls: &[CollisionHullBounds; 3],
) -> Result<
    (
        PackedBspGeometry,
        CompiledCollisionHulls,
        Vec<[i32; 3]>,
        UvWindowStats,
    ),
    BrushWorldCookError,
> {
    let (topology_surfaces, render_surfaces) = compile_model_surfaces(brushes);
    let (mut bsp, portals, leak_diagnostic) =
        compile_model_topology(&topology_surfaces, brushes, occupant_points, true);
    // qbsp parity: EVERY final face is capped to SURFACE_EXTENT_UNITS,
    // lights or not. Build exact leaves from the unsplit CSG surfaces, then
    // keep the PS1-sized render surfaces as single-owner records referenced
    // by leaf marks; partition fragments are visibility construction data,
    // not additional draw geometry. Small final faces are what make the
    // runtime's GTE emitter safe at the eye plane (a crossing triangle
    // saturates wider than the GPU's 1023px draw limit and is skipped
    // as a sub-face-sized hole instead of rasterizing as a screen-wide
    // wrap), and they are also what lets the vertex bake resolve
    // hotspots and shadow edges mid-face. This supersedes the old
    // light-gated LIGHT_SUBDIVISION_UNITS pass: the cap is finer than
    // the light grid was, so lit and lightless scenes now share one
    // subdivision rule.
    let light_spheres: Vec<_> = lights
        .iter()
        .map(|light| (light.position, light.radius))
        .collect();
    let render_surfaces = subdivide_surfaces_to_budget(
        &render_surfaces,
        ENGINE_SURFACE_EXTENT_UNITS,
        MAX_RESIDENT_WORLD_FACES,
        &light_spheres,
    );
    // The packer stores vertex UVs as u8 texels (`brush_pack::pack_vertex`
    // wraps them modulo 256), so a surface whose texel span exceeds the
    // GPU's 255-texel window rasterises one stretched repeat instead of
    // tiling. The budget loop above coarsens the extent cap on a large
    // level until the face budget is met, which is exactly when surfaces
    // grow past that span. Split those back down before packing; the
    // editor preview samples float UVs and never sees the wrap, which is
    // why this only ever showed in-game.
    let budgeted_faces = render_surfaces.len();
    let (render_surfaces, uv_window) =
        fit_surfaces_to_uv_window(render_surfaces, texture_dims, uv_window_skip);
    if render_surfaces.len() > MAX_RESIDENT_WORLD_FACES
        && budgeted_faces <= MAX_RESIDENT_WORLD_FACES
    {
        return Err(BrushWorldCookError::UvWindowFaceBudget {
            faces: render_surfaces.len(),
            max: MAX_RESIDENT_WORLD_FACES,
            split_surfaces: uv_window.split_surfaces,
        });
    }
    replace_bsp_render_surfaces(&mut bsp, render_surfaces);
    // Small editor BSPs can afford the same separator-flow VIS used by a
    // release cook. Larger interactive cooks retain Quake's conservative
    // base-portal `mightsee` stage instead of collapsing the entire connected
    // world to one all-visible row. A leak remains a real opening in the
    // portal graph, so both paths stay conservative while still rejecting
    // geometry that cannot be seen through that opening.
    let visible_leaf_count = bsp
        .leaves
        .iter()
        .filter(|leaf| leaf.contents.is_visible())
        .count();
    let visibility = match mode {
        BrushWorldCookMode::Draft if visible_leaf_count <= 512 && portals.len() <= 10_000 => {
            BspVisibility::PortalFlow
        }
        BrushWorldCookMode::Draft => BspVisibility::PortalFast,
        BrushWorldCookMode::Release => BspVisibility::PortalFlow,
    };
    // Lighting bakes in BOTH modes so lights always interact with
    // brushes out of the box: Draft skips only the occlusion (shadow)
    // tests, the expensive part; Release adds shadows. A scene with no
    // lights at all keeps the historic fullbright look instead of
    // dropping to bare ambient.
    let geometry = if lights.is_empty() {
        pack_bsp_geometry_with_visibility(
            &bsp,
            &portals,
            BspLighting::Fullbright,
            visibility,
            texture_dims,
        )?
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
        pack_bsp_geometry_with_visibility(
            &bsp,
            &portals,
            BspLighting::Baked(&lighting),
            visibility,
            texture_dims,
        )?
    };
    let collision = compile_runtime_collision_hulls(brushes, collision_hulls)?;
    Ok((geometry, collision, leak_diagnostic.path, uv_window))
}

fn compile_model_surfaces(brushes: &[Brush]) -> (Vec<CompiledSurface>, Vec<CompiledSurface>) {
    let csg_surfaces = compile_csg_surfaces(brushes);
    let authored_surfaces = compile_authored_surfaces(brushes);
    if csg_surfaces.len() <= i16::MAX as usize {
        // Keep the actual union boundary whenever it fits the resident face
        // budget, even if retaining each authored face would be marginally
        // smaller. Authored polygons can cross into an overlapping brush; a
        // PS1 painter's algorithm then has no valid whole-triangle order and
        // produces diagonal wedges around trims, arches, and pillars. The
        // authored fallback exists only for large maps where CSG fragmentation
        // itself exceeds the resident budget. CSG remains preferable above
        // that threshold too when it is already the smaller representation.
        let render = if prefer_csg_render_surfaces(csg_surfaces.len(), authored_surfaces.len()) {
            csg_surfaces.clone()
        } else {
            authored_surfaces
        };
        (csg_surfaces, render)
    } else {
        (authored_surfaces.clone(), authored_surfaces)
    }
}

fn compile_model_topology(
    topology_surfaces: &[CompiledSurface],
    brushes: &[Brush],
    occupant_points: &[[f64; 3]],
    log_result: bool,
) -> (
    CompiledSurfaceBsp,
    Vec<CompiledPortal>,
    BrushWorldLeakDiagnostic,
) {
    let mut bsp = build_surface_bsp(&topology_surfaces);
    let portals = portalize_surface_bsp(&bsp);
    classify_bsp_leaves(&mut bsp, &portals, brushes);
    let mut leak_diagnostic = BrushWorldLeakDiagnostic::default();
    match fill_outside_bsp_leaves(&mut bsp, &portals, occupant_points) {
        OutsideFillResult::Filled(filled) => {
            if log_result && filled > 0 {
                crate::playtest::emit_cook_output(format_args!(
                    "[brush-qbsp] filled {filled} unreachable exterior leaves"
                ));
            }
        }
        OutsideFillResult::Leaked(leak) => {
            leak_diagnostic.path = leak
                .points
                .into_iter()
                .map(|point| {
                    point.map(|coordinate| {
                        coordinate
                            .round()
                            .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                            as i32
                    })
                })
                .collect();
            if let Some((path_portal_index, portal_index, portal)) = leak
                .portal_indices
                .iter()
                .enumerate()
                .filter_map(|(path_portal_index, &portal_index)| {
                    let portal = portals.get(portal_index)?;
                    (portal.vertices.len() >= 3).then_some((
                        path_portal_index,
                        portal_index,
                        portal,
                    ))
                })
                .min_by(|(_, _, a), (_, _, b)| {
                    portal_area_measure_squared(&a.vertices)
                        .total_cmp(&portal_area_measure_squared(&b.vertices))
                })
            {
                let opening = connected_coplanar_portal_outline(&portals, portal_index, |leaf| {
                    bsp.leaves
                        .get(leaf)
                        .is_some_and(|leaf| leaf.contents.is_visible())
                });
                let opening = if opening.len() >= 3 {
                    opening.as_slice()
                } else {
                    portal.vertices.as_slice()
                };
                leak_diagnostic.likely_opening = opening
                    .iter()
                    .map(|point| {
                        point.map(|coordinate| {
                            coordinate
                                .round()
                                .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                                as i32
                        })
                    })
                    .collect();
                leak_diagnostic.likely_opening_path_index = Some(path_portal_index + 1);
            }
            if log_result {
                crate::playtest::emit_cook_output(format_args!(
                    "[brush-qbsp] leak from {:?}: occupied leaf reaches the exterior through {} pointfile points; retaining portal PVS through the opening",
                    leak_diagnostic.path.first().copied().unwrap_or([0; 3]),
                    leak_diagnostic.path.len(),
                ));
            }
        }
        OutsideFillResult::NoOccupants => {}
    }
    (bsp, portals, leak_diagnostic)
}

/// Reconstruct the outer boundary of all visible portal fragments connected
/// to `seed` on the same geometric plane. Portalization splits one physical
/// opening at every descendant BSP plane, so presenting only the seed makes a
/// large authored hole look like a tiny diagnostic square.
fn connected_coplanar_portal_outline(
    portals: &[CompiledPortal],
    seed: usize,
    leaf_visible: impl Fn(usize) -> bool,
) -> Vec<[f64; 3]> {
    let Some(seed_portal) = portals.get(seed) else {
        return Vec::new();
    };
    let candidates: Vec<_> = portals
        .iter()
        .enumerate()
        .filter_map(|(index, portal)| {
            (portal.vertices.len() >= 3
                && leaf_visible(portal.front_leaf)
                && leaf_visible(portal.back_leaf)
                && portal_planes_match(seed_portal.plane, portal.plane))
            .then_some(index)
        })
        .collect();
    let mut component = vec![seed];
    let mut cursor = 0;
    while cursor < component.len() {
        let current = component[cursor];
        for &candidate in &candidates {
            if component.contains(&candidate)
                || !portal_polygons_share_edge(
                    &portals[current].vertices,
                    &portals[candidate].vertices,
                )
            {
                continue;
            }
            component.push(candidate);
        }
        cursor += 1;
    }
    portal_component_outer_boundary(portals, &component)
}

fn portal_planes_match(left: crate::brush::Plane, right: crate::brush::Plane) -> bool {
    const EPSILON: f64 = 1.0e-8;
    let (left_normal, left_distance) = crate::brush_compile::normalized_plane(left);
    let (right_normal, right_distance) = crate::brush_compile::normalized_plane(right);
    let direct = left_normal
        .into_iter()
        .zip(right_normal)
        .all(|(left, right)| (left - right).abs() <= EPSILON)
        && (left_distance - right_distance).abs() <= EPSILON;
    let inverse = left_normal
        .into_iter()
        .zip(right_normal)
        .all(|(left, right)| (left + right).abs() <= EPSILON)
        && (left_distance + right_distance).abs() <= EPSILON;
    direct || inverse
}

fn portal_polygons_share_edge(left: &[[f64; 3]], right: &[[f64; 3]]) -> bool {
    left.iter()
        .copied()
        .zip(left.iter().copied().cycle().skip(1))
        .take(left.len())
        .any(|(left_a, left_b)| {
            right
                .iter()
                .copied()
                .zip(right.iter().copied().cycle().skip(1))
                .take(right.len())
                .any(|(right_a, right_b)| {
                    collinear_segments_overlap(left_a, left_b, right_a, right_b)
                })
        })
}

fn collinear_segments_overlap(
    left_a: [f64; 3],
    left_b: [f64; 3],
    right_a: [f64; 3],
    right_b: [f64; 3],
) -> bool {
    const EPSILON: f64 = 1.0 / 1024.0;
    let direction = subtract(left_b, left_a);
    let length_squared = dot_f64(direction, direction);
    if length_squared <= EPSILON * EPSILON {
        return false;
    }
    let right_direction = subtract(right_b, right_a);
    let parallel_error = cross_f64(direction, right_direction);
    if dot_f64(parallel_error, parallel_error)
        > EPSILON * EPSILON * length_squared * dot_f64(right_direction, right_direction)
    {
        return false;
    }
    let line_error = cross_f64(subtract(right_a, left_a), direction);
    if dot_f64(line_error, line_error) > EPSILON * EPSILON * length_squared {
        return false;
    }
    let right_t = [right_a, right_b]
        .map(|point| dot_f64(subtract(point, left_a), direction) / length_squared);
    let overlap_start = 0.0_f64.max(right_t[0].min(right_t[1]));
    let overlap_end = 1.0_f64.min(right_t[0].max(right_t[1]));
    overlap_end - overlap_start > EPSILON / length_squared.sqrt()
}

fn portal_component_outer_boundary(
    portals: &[CompiledPortal],
    component: &[usize],
) -> Vec<[f64; 3]> {
    const EPSILON: f64 = 1.0 / 1024.0;
    let mut vertices: Vec<[f64; 3]> = Vec::new();
    for portal in component.iter().filter_map(|&index| portals.get(index)) {
        for &vertex in &portal.vertices {
            if !vertices
                .iter()
                .any(|known| squared_distance_f64(*known, vertex) <= EPSILON * EPSILON)
            {
                vertices.push(vertex);
            }
        }
    }

    let mut edge_counts = std::collections::HashMap::<(usize, usize), usize>::new();
    for portal in component.iter().filter_map(|&index| portals.get(index)) {
        for (edge_a, edge_b) in portal
            .vertices
            .iter()
            .copied()
            .zip(portal.vertices.iter().copied().cycle().skip(1))
            .take(portal.vertices.len())
        {
            let direction = subtract(edge_b, edge_a);
            let length_squared = dot_f64(direction, direction);
            if length_squared <= EPSILON * EPSILON {
                continue;
            }
            let mut splits: Vec<_> = vertices
                .iter()
                .enumerate()
                .filter_map(|(index, &point)| {
                    let t = dot_f64(subtract(point, edge_a), direction) / length_squared;
                    if t < -EPSILON || t > 1.0 + EPSILON {
                        return None;
                    }
                    let nearest = add_f64(edge_a, scale_f64(direction, t));
                    (squared_distance_f64(nearest, point) <= EPSILON * EPSILON)
                        .then_some((t.clamp(0.0, 1.0), index))
                })
                .collect();
            splits.sort_by(|left, right| left.0.total_cmp(&right.0));
            splits.dedup_by_key(|(_, index)| *index);
            for pair in splits.windows(2) {
                let left = pair[0].1;
                let right = pair[1].1;
                if left == right
                    || squared_distance_f64(vertices[left], vertices[right]) <= EPSILON * EPSILON
                {
                    continue;
                }
                let edge = if left < right {
                    (left, right)
                } else {
                    (right, left)
                };
                *edge_counts.entry(edge).or_default() += 1;
            }
        }
    }

    let mut boundary_edges: Vec<_> = edge_counts
        .into_iter()
        .filter_map(|(edge, count)| (count % 2 == 1).then_some(edge))
        .collect();
    boundary_edges.sort_unstable();
    let mut adjacency = vec![Vec::new(); vertices.len()];
    for &(left, right) in &boundary_edges {
        adjacency[left].push(right);
        adjacency[right].push(left);
    }
    for adjacent in &mut adjacency {
        adjacent.sort_unstable();
        adjacent.dedup();
    }

    let mut unused: std::collections::HashSet<_> = boundary_edges.iter().copied().collect();
    let mut loops = Vec::new();
    for &(start, first) in &boundary_edges {
        if !unused.remove(&(start, first)) {
            continue;
        }
        let mut indices = vec![start];
        let mut previous = start;
        let mut current = first;
        while current != start && indices.len() <= boundary_edges.len() + 1 {
            indices.push(current);
            let Some(next) = adjacency[current].iter().copied().find(|&candidate| {
                candidate != previous
                    && unused.contains(&(current.min(candidate), current.max(candidate)))
            }) else {
                break;
            };
            unused.remove(&(current.min(next), current.max(next)));
            previous = current;
            current = next;
        }
        if current == start && indices.len() >= 3 {
            loops.push(
                indices
                    .into_iter()
                    .map(|index| vertices[index])
                    .collect::<Vec<_>>(),
            );
        }
    }
    loops
        .into_iter()
        .max_by(|left, right| {
            portal_area_measure_squared(left).total_cmp(&portal_area_measure_squared(right))
        })
        .unwrap_or_default()
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| left[axis] - right[axis])
}

fn add_f64(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| left[axis] + right[axis])
}

fn scale_f64(value: [f64; 3], scale: f64) -> [f64; 3] {
    value.map(|component| component * scale)
}

fn dot_f64(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.into_iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn cross_f64(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn squared_distance_f64(left: [f64; 3], right: [f64; 3]) -> f64 {
    dot_f64(subtract(left, right), subtract(left, right))
}

/// Squared Newell area vector. The constant scale factor is irrelevant when
/// comparing portals, and avoiding a square root keeps the live check cheap.
fn portal_area_measure_squared(vertices: &[[f64; 3]]) -> f64 {
    let mut area = [0.0; 3];
    for index in 0..vertices.len() {
        let a = vertices[index];
        let b = vertices[(index + 1) % vertices.len()];
        area[0] += (a[1] - b[1]) * (a[2] + b[2]);
        area[1] += (a[2] - b[2]) * (a[0] + b[0]);
        area[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    area.into_iter()
        .map(|component| component * component)
        .sum()
}

fn prefer_csg_render_surfaces(csg_count: usize, authored_count: usize) -> bool {
    csg_count <= MAX_RESIDENT_WORLD_FACES || csg_count <= authored_count
}

fn compile_runtime_collision_hulls(
    brushes: &[Brush],
    hulls: &[CollisionHullBounds; 3],
) -> Result<CompiledCollisionHulls, CollisionHullCompileError> {
    // Quake hull 0 is the classified render BSP itself. Do not duplicate the
    // entire point tree in clipnodes merely to satisfy the four-head model
    // record: the runtime never reads collision head zero. Keep one valid
    // empty sentinel head for format validation, followed by the two actual
    // box-expanded body hulls.
    let mut collision = compile_collision_hulls(brushes, &hulls[1..])?;
    let plane = if collision.planes.is_empty() {
        let (record, _) = pack_normalized_plane([1.0, 0.0, 0.0], 0.0)
            .ok_or(CollisionHullCompileError::InvalidPlane(None))?;
        collision.planes.extend_from_slice(&record);
        0i16
    } else {
        0i16
    };
    let node = collision.clipnodes.len() / 6;
    if node > i16::MAX as usize {
        return Err(CollisionHullCompileError::LimitExceeded {
            kind: "clipnodes",
            count: node + 1,
            max: i16::MAX as usize + 1,
        });
    }
    collision.clipnodes.extend_from_slice(&plane.to_le_bytes());
    collision
        .clipnodes
        .extend_from_slice(&psx_bsp::collision::CONTENTS_EMPTY.to_le_bytes());
    collision
        .clipnodes
        .extend_from_slice(&psx_bsp::collision::CONTENTS_EMPTY.to_le_bytes());
    collision.head_nodes.insert(0, node as i16);
    Ok(collision)
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

/// Byte budget for page-local texture promotion. Every promoted image is
/// tiled from its source, never stretched, so spending this budget cannot
/// change a single texel; it only moves faces from the windowed draw path
/// (a GP0(E2) selector plus reset per surface, and `19*15` reserved words per
/// triangle) onto the compact one (`19*13`).
///
/// The cost is paid twice, in VRAM and in the guest's `.data`, because a
/// promoted texture is both uploaded and embedded in the PSX-EXE. That second
/// cost is what pinned this at 8 KiB: the whole promotion set needs 12,288
/// bytes more than the 8 KiB selection, and the guest link had only ~2.3 KB of
/// RAM headroom, so 10 KiB already overflowed `.bss`. Sizing the break-time
/// debris cache to the project's breakable-prop budget returned 14,336 bytes
/// and let the full set land. Raise this further only against a fresh link:
/// the binding constraint is the MIPS RAM region, not the 1 MB of VRAM (the
/// complete set costs 1.2% of it).
///
/// It stays at 8 KiB because the full set was then measured and is not worth
/// the RAM. Against a frozen source closure, with `.text` byte-identical
/// across the pair and `.data` differing by exactly the +12,288 of promoted
/// texels, 32 KiB moved 703 -> 1085 compact faces (41.4% -> 63.9%) for
/// +0.11% fps and -0.18% room. The per-face saving is one selector plus two
/// reserved words, and too few of the 382 promoted faces are on screen at
/// once to matter. 12,288 bytes of the scarcest resource buys noise.
const PAGE_LOCAL_PROMOTION_BUDGET_BYTES: usize = 8 * 1024;
const PAGE_LOCAL_PROMOTION_MIN_FACE_GAIN: usize = 32;

#[derive(Clone, Copy, Debug)]
struct PageLocalPromotion {
    material: Option<ResourceId>,
    target: [u16; 2],
    extra_bytes: usize,
    face_gain: usize,
    order: usize,
}

/// Pick repeated 4bpp texture extents for materials where the saved packet
/// state materially outweighs the extra VRAM. This mirrors Quake's
/// cooker-owned page-local surface contract without stretching or changing
/// texels: every promoted image is tiled from the original source.
fn page_local_texture_dims(
    project: &ProjectDocument,
    slots: &[Option<ResourceId>],
    world: &PackedBspGeometry,
    submodels: &[PxbspSubmodel],
    source_dims: &std::collections::HashMap<Option<ResourceId>, [u16; 2]>,
    project_root: &Path,
) -> std::collections::HashMap<Option<ResourceId>, [u16; 2]> {
    let mut output = source_dims.clone();
    let mut candidates = Vec::new();
    for (order, &material) in slots.iter().enumerate() {
        let Some(material_id) = material else {
            continue;
        };
        let Some(resource) = project.resource(material_id) else {
            continue;
        };
        let ResourceData::Material(material_data) = &resource.data else {
            continue;
        };
        if material_data.blend_mode != PsxBlendMode::Opaque
            || material_data.animation.mode != MaterialAnimationMode::Static
            || material_data.sky_aperture
        {
            continue;
        }
        let Some(&source) = source_dims.get(&material) else {
            continue;
        };
        let Ok(Some((_, source_bytes))) =
            resolve_material_texture_psxt(project, material_id, project_root)
        else {
            continue;
        };
        let Ok(source_texture) = psx_asset::Texture::from_bytes(&source_bytes) else {
            continue;
        };
        if source_texture.depth() != Depth::Bit4
            || [source_texture.width(), source_texture.height()] != source
        {
            continue;
        }
        let mut requirements = Vec::new();
        collect_face_uv_requirements(world, material, &mut requirements);
        for submodel in submodels {
            collect_face_uv_requirements(&submodel.geometry, material, &mut requirements);
        }
        let Some((target, face_gain, extra_bytes)) =
            best_page_local_promotion(source, &requirements)
        else {
            continue;
        };
        if face_gain >= PAGE_LOCAL_PROMOTION_MIN_FACE_GAIN {
            candidates.push(PageLocalPromotion {
                material,
                target,
                extra_bytes,
                face_gain,
                order,
            });
        }
    }

    candidates.sort_by(|left, right| {
        let left_score = left.face_gain as u64 * right.extra_bytes as u64;
        let right_score = right.face_gain as u64 * left.extra_bytes as u64;
        right_score
            .cmp(&left_score)
            .then_with(|| left.order.cmp(&right.order))
    });
    let mut remaining = PAGE_LOCAL_PROMOTION_BUDGET_BYTES;
    for candidate in candidates {
        if candidate.extra_bytes > remaining {
            continue;
        }
        output.insert(candidate.material, candidate.target);
        remaining -= candidate.extra_bytes;
    }
    output
}

fn collect_face_uv_requirements(
    geometry: &PackedBspGeometry,
    material: Option<ResourceId>,
    output: &mut Vec<[u16; 2]>,
) {
    for face in geometry.faces.chunks_exact(10) {
        let texture = u16::from_le_bytes([face[4], face[5]]) as usize;
        if geometry.material_slots.get(texture).copied() != Some(material) {
            continue;
        }
        let first_vertex = u16::from_le_bytes([face[2], face[3]]) as usize;
        let vertex_count = face[7] as usize;
        let mut required = [1u16; 2];
        for vertex in geometry
            .vertices
            .chunks_exact(12)
            .skip(first_vertex)
            .take(vertex_count)
        {
            required[0] = required[0].max(u16::from(vertex[6]) + 1);
            required[1] = required[1].max(u16::from(vertex[7]) + 1);
        }
        output.push(required);
    }
}

fn best_page_local_promotion(
    source: [u16; 2],
    requirements: &[[u16; 2]],
) -> Option<([u16; 2], usize, usize)> {
    let baseline = requirements
        .iter()
        .filter(|required| required[0] <= source[0] && required[1] <= source[1])
        .count();
    let source_area = usize::from(source[0]) * usize::from(source[1]);
    let mut best: Option<([u16; 2], usize, usize)> = None;
    let mut width = source[0];
    while width <= 128 {
        let mut height = source[1];
        while height <= 128 {
            let eligible = requirements
                .iter()
                .filter(|required| required[0] <= width && required[1] <= height)
                .count();
            let area = usize::from(width) * usize::from(height);
            let face_gain = eligible.saturating_sub(baseline);
            let extra_bytes = area.saturating_sub(source_area) / 2;
            if face_gain != 0 && extra_bytes != 0 {
                let replace = best.is_none_or(|(_, best_gain, best_extra)| {
                    let score = face_gain as u64 * best_extra as u64;
                    let best_score = best_gain as u64 * extra_bytes as u64;
                    score > best_score || (score == best_score && face_gain > best_gain)
                });
                if replace {
                    best = Some(([width, height], face_gain, extra_bytes));
                }
            }
            let Some(next_height) = height.checked_mul(2).filter(|&value| value <= 128) else {
                break;
            };
            height = next_height;
        }
        let Some(next_width) = width.checked_mul(2).filter(|&value| value <= 128) else {
            break;
        };
        width = next_width;
    }
    best
}

fn mark_page_local_faces(
    geometry: &mut PackedBspGeometry,
    texture_dims: &std::collections::HashMap<Option<ResourceId>, [u16; 2]>,
) {
    for face in geometry.faces.chunks_exact_mut(10) {
        let texture = u16::from_le_bytes([face[4], face[5]]) as usize;
        let Some(material) = geometry.material_slots.get(texture).copied() else {
            continue;
        };
        let Some(&dims) = texture_dims.get(&material) else {
            continue;
        };
        let first_vertex = u16::from_le_bytes([face[2], face[3]]) as usize;
        let vertex_count = face[7] as usize;
        let page_local = geometry
            .vertices
            .chunks_exact(12)
            .skip(first_vertex)
            .take(vertex_count)
            .all(|vertex| u16::from(vertex[6]) < dims[0] && u16::from(vertex[7]) < dims[1]);
        if page_local {
            face[6] |= psx_bsp::FACE_PAGE_LOCAL_UV as u8;
        }
    }
}

fn resolve_materials(
    project: &ProjectDocument,
    slots: &[Option<ResourceId>],
    options: &BrushWorldCookOptions<'_>,
    page_texture_dims: &std::collections::HashMap<Option<ResourceId>, [u16; 2]>,
) -> Result<(Vec<PxbspMaterial>, Vec<CompiledBrushTexture>), BrushWorldCookError> {
    let mut textures = Vec::new();
    let mut materials = Vec::with_capacity(slots.len());
    for &slot in slots {
        let (mut key, mut bytes, tint, blend, sidedness, animation, sky_aperture) = match slot {
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
                let (key, bytes) = if material.sky_aperture {
                    // Apertures never submit their authored polygons. The
                    // World cooks its one sky source independently, so no
                    // per-face texture belongs in PXBSP or VRAM.
                    ("@sky-aperture".to_string(), Vec::new())
                } else {
                    let texture = resolve_material_texture_psxt(project, id, options.project_root)
                        .map_err(|error| BrushWorldCookError::MaterialTexture {
                            material: id,
                            error,
                        })?;
                    texture.unwrap_or_else(|| ("@brush-flat-white".to_string(), flat_white_psxt()))
                };
                (
                    key,
                    bytes,
                    material.tint,
                    material.blend_mode,
                    material.sidedness(),
                    material.animation,
                    material.sky_aperture,
                )
            }
        };
        if sky_aperture {
            materials.push(pack_material(
                u16::MAX,
                [1, 1],
                tint,
                blend,
                sidedness,
                animation,
                true,
                slot,
            )?);
            continue;
        }
        if let Some(&target) = page_texture_dims.get(&slot) {
            let parsed = psx_asset::Texture::from_bytes(&bytes).map_err(|error| {
                BrushWorldCookError::InvalidTexture {
                    material: slot,
                    error: format!("invalid PSXT before page-local promotion: {error:?}"),
                }
            })?;
            let source = [parsed.width(), parsed.height()];
            if target != source {
                bytes = tile_4bpp_texture(&bytes, target).map_err(|error| {
                    BrushWorldCookError::InvalidTexture {
                        material: slot,
                        error,
                    }
                })?;
                key = format!("{key}#page{}x{}", target[0], target[1]);
            }
        }
        let (texture_asset, texture_size) =
            intern_texture(&mut textures, options.texture_asset_base, slot, key, bytes)?;
        materials.push(pack_material(
            texture_asset,
            texture_size,
            tint,
            blend,
            sidedness,
            animation,
            false,
            slot,
        )?);
    }
    Ok((materials, textures))
}

fn tile_4bpp_texture(bytes: &[u8], target: [u16; 2]) -> Result<Vec<u8>, String> {
    let texture = psx_asset::Texture::from_bytes(bytes)
        .map_err(|error| format!("invalid source PSXT: {error:?}"))?;
    let source = [texture.width(), texture.height()];
    if texture.depth() != Depth::Bit4
        || target[0] < source[0]
        || target[1] < source[1]
        || target[0] > 128
        || target[1] > 128
        || !target[0].is_multiple_of(source[0])
        || !target[1].is_multiple_of(source[1])
    {
        return Err(format!(
            "page-local promotion requires a repeated 4bpp source, got {}x{} -> {}x{}",
            source[0], source[1], target[0], target[1]
        ));
    }
    if target == source {
        return Ok(bytes.to_vec());
    }

    let header_bytes = psxed_format::AssetHeader::SIZE + psxed_format::texture::TextureHeader::SIZE;
    if bytes.len() < header_bytes {
        return Err("truncated PSXT header".to_string());
    }
    let source_row_bytes = usize::from(texture.halfwords_per_row()) * 2;
    let target_row_bytes = usize::from(target[0]) / 2;
    if texture.pixel_bytes().len() != source_row_bytes * usize::from(source[1]) {
        return Err("4bpp PSXT pixel rows are not tightly packed".to_string());
    }
    let mut pixels = Vec::with_capacity(target_row_bytes * usize::from(target[1]));
    for target_y in 0..usize::from(target[1]) {
        let source_y = target_y % usize::from(source[1]);
        let row_start = source_y * source_row_bytes;
        let row = &texture.pixel_bytes()[row_start..row_start + source_row_bytes];
        for _ in 0..usize::from(target[0] / source[0]) {
            pixels.extend_from_slice(row);
        }
    }

    let mut output = bytes[..header_bytes].to_vec();
    output[14..16].copy_from_slice(&target[0].to_le_bytes());
    output[16..18].copy_from_slice(&target[1].to_le_bytes());
    output[20..24].copy_from_slice(&(pixels.len() as u32).to_le_bytes());
    output.extend_from_slice(&pixels);
    output.extend_from_slice(texture.clut_bytes());
    let payload_len = output.len() - psxed_format::AssetHeader::SIZE;
    output[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    Ok(output)
}

fn intern_texture(
    textures: &mut Vec<CompiledBrushTexture>,
    base: u16,
    material: Option<ResourceId>,
    key: String,
    bytes: Vec<u8>,
) -> Result<(u16, [u16; 2]), BrushWorldCookError> {
    let parsed = psx_asset::Texture::from_bytes(&bytes).map_err(|error| {
        BrushWorldCookError::InvalidTexture {
            material,
            error: format!("invalid PSXT: {error:?}"),
        }
    })?;
    let width = parsed.width();
    let height = parsed.height();
    let valid_size = (8..=128).contains(&width)
        && (8..=128).contains(&height)
        && width.is_power_of_two()
        && height.is_power_of_two();
    if parsed.depth() != Depth::Bit4 || !valid_size {
        return Err(BrushWorldCookError::InvalidTexture {
            material,
            error: format!("brush texture must be 4bpp power-of-two 8..128, got {width}x{height}"),
        });
    }
    if let Some(existing) = textures.iter().find(|texture| texture.key == key) {
        if existing.bytes != bytes {
            return Err(BrushWorldCookError::InvalidTexture {
                material,
                error: format!("texture key {key:?} resolved to different bytes"),
            });
        }
        return Ok((existing.asset_id, existing.size));
    }
    let index = u16::try_from(textures.len())
        .map_err(|_| BrushWorldCookError::TextureAssetOverflow { material })?;
    let asset_id = base
        .checked_add(index)
        .ok_or(BrushWorldCookError::TextureAssetOverflow { material })?;
    let size = [width, height];
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
    texture_size: [u16; 2],
    tint: [u8; 3],
    blend: PsxBlendMode,
    sidedness: MaterialFaceSidedness,
    animation: crate::MaterialAnimation,
    sky_aperture: bool,
    material: Option<ResourceId>,
) -> Result<PxbspMaterial, BrushWorldCookError> {
    let flags = match sidedness {
        MaterialFaceSidedness::Front => material_flags::FACE_FRONT,
        MaterialFaceSidedness::Back => material_flags::FACE_BACK,
        MaterialFaceSidedness::Both => material_flags::FACE_BOTH,
    } | if sky_aperture {
        material_flags::SKY_APERTURE
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
            if !texture_size[0].is_multiple_of(u16::from(flipbook.columns))
                || !texture_size[1].is_multiple_of(u16::from(flipbook.rows))
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
        MaterialAnimationMode::LightPulse => {
            let pulse = animation.light_pulse.normalized();
            (
                material_animation::LIGHT_PULSE,
                [
                    pulse.minimum_q7,
                    pulse.maximum_q7,
                    pulse.ticks_per_cycle,
                    pulse.phase,
                    0,
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
    use crate::DestructibleDamageAffinity;
    use crate::{brush::BrushContents, MaterialResource, Transform3};

    fn floor_surface(half_extent: i32) -> CompiledSurface {
        let e = half_extent;
        CompiledSurface {
            plane: crate::brush::Plane::from_points([[-e, 0, -e], [e, 0, -e], [e, 0, e]])
                .expect("floor plane"),
            vertices: vec![
                [-e as f64, 0.0, -e as f64],
                [e as f64, 0.0, -e as f64],
                [e as f64, 0.0, e as f64],
                [-e as f64, 0.0, e as f64],
            ],
            material: None,
            uv: crate::brush::FaceUv::default(),
            contents: BrushContents::Solid,
            source_brush: 0,
            source_face: 0,
        }
    }

    #[test]
    fn uv_window_fit_splits_only_surfaces_that_wrap() {
        let mut dims = std::collections::HashMap::new();
        dims.insert(None, [64u16, 64u16]);
        // 8192 units at 16 units per texel is a 512-texel span: wraps.
        let (pieces, stats) =
            fit_surfaces_to_uv_window(vec![floor_surface(4096)], &dims, &Default::default());
        assert_eq!(stats.split_surfaces, 1);
        assert_eq!(stats.unfixable_surfaces, 0);
        assert!(
            pieces.len() >= 4,
            "expected at least four pieces, got {}",
            pieces.len()
        );
        assert_eq!(stats.added_faces, pieces.len() - 1);
        for piece in &pieces {
            assert_eq!(surface_fits_uv_window(piece, dims.get(&None)), Some(true));
        }
        // 2048 units is a 128-texel span: untouched, byte-identical cook.
        let small = floor_surface(1024);
        let (pieces, stats) =
            fit_surfaces_to_uv_window(vec![small.clone()], &dims, &Default::default());
        assert_eq!(stats, UvWindowStats::default());
        assert_eq!(pieces, vec![small]);
        // Unknown texture dimensions keep the historic wrap and are counted.
        let (_, stats) = fit_surfaces_to_uv_window(
            vec![floor_surface(4096)],
            &Default::default(),
            &Default::default(),
        );
        assert_eq!(stats.unfixable_surfaces, 1);
        assert_eq!(stats.split_surfaces, 0);
    }

    #[test]
    fn repeated_page_texture_preserves_every_source_texel_and_clut() {
        let indices = (0..64).map(|index| (index % 16) as u8).collect::<Vec<_>>();
        let palette = (0..16)
            .map(|index| [index * 11, 255 - index * 7, index * 3])
            .collect::<Vec<_>>();
        let source = psxed_tex::encode_indexed_psxt(8, 8, Depth::Bit4, &indices, &palette, true)
            .expect("source texture");
        let promoted = tile_4bpp_texture(&source, [32, 16]).expect("repeated texture");
        let source = psx_asset::Texture::from_bytes(&source).expect("source parses");
        let promoted = psx_asset::Texture::from_bytes(&promoted).expect("promoted parses");

        assert_eq!((promoted.width(), promoted.height()), (32, 16));
        assert_eq!(promoted.clut_bytes(), source.clut_bytes());
        assert_eq!(promoted.flags(), source.flags());
        let source_row_bytes = usize::from(source.halfwords_per_row()) * 2;
        let promoted_row_bytes = usize::from(promoted.halfwords_per_row()) * 2;
        for (row_index, row) in promoted
            .pixel_bytes()
            .chunks_exact(promoted_row_bytes)
            .enumerate()
        {
            let source_y = row_index % usize::from(source.height());
            let source_row = &source.pixel_bytes()
                [source_y * source_row_bytes..(source_y + 1) * source_row_bytes];
            assert!(row
                .chunks_exact(source_row_bytes)
                .all(|copy| copy == source_row));
        }
    }

    #[test]
    fn page_texture_promotion_maximizes_faces_saved_per_extra_byte() {
        let mut requirements = vec![[20, 20]; 10];
        requirements.extend(vec![[60, 60]; 40]);
        requirements.extend(vec![[100, 100]; 80]);
        let (target, gain, extra_bytes) =
            best_page_local_promotion([32, 32], &requirements).expect("promotion");
        assert_eq!(target, [64, 64]);
        assert_eq!(gain, 40);
        assert_eq!(extra_bytes, (64 * 64 - 32 * 32) / 2);
    }
    use psx_bsp::destructible::BrushDestructibleSet;
    use psx_bsp::mover::BrushDoorSet;
    use psx_bsp::pxbsp::{entity_class, entity_flags, PxbspBrushDestructible, PxbspBrushDoor};
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

    #[test]
    fn csg_render_surfaces_win_until_fragmentation_exceeds_the_resident_budget() {
        assert!(prefer_csg_render_surfaces(494, 449));
        assert!(prefer_csg_render_surfaces(MAX_RESIDENT_WORLD_FACES, 32));
        assert!(prefer_csg_render_surfaces(8_000, 9_000));
        assert!(!prefer_csg_render_surfaces(8_000, 6_000));
    }

    #[test]
    fn layered_sky_atlas_is_not_interned_as_a_brush_face_texture() {
        let pixels = vec![1; 256 * 128];
        let bytes = psxed_tex::encode_indexed_psxt(
            256,
            128,
            Depth::Bit4,
            &pixels,
            &[[0, 0, 0], [96, 128, 192]],
            true,
        )
        .expect("Quake sky pair");
        let error = intern_texture(
            &mut Vec::new(),
            70,
            Some(ResourceId(9)),
            "sky-pair".to_string(),
            bytes,
        )
        .expect_err("scene sky atlas must not enter the brush texture table");
        assert!(error.to_string().contains("4bpp power-of-two 8..128"));
    }

    #[test]
    fn cube_sky_atlas_is_not_interned_as_a_brush_face_texture() {
        let palette_rows = (0..6)
            .map(|face| {
                (0..16)
                    .map(|entry| [face * 24, entry * 12, 96])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let bytes = psxed_tex::encode_indexed_psxt_with_clut_rows(
            1536,
            256,
            Depth::Bit4,
            &vec![1; 1536 * 256],
            &palette_rows,
            false,
        )
        .expect("directional cube atlas");
        let error = intern_texture(
            &mut Vec::new(),
            71,
            Some(ResourceId(10)),
            "directional-sky".to_string(),
            bytes,
        )
        .expect_err("scene cube atlas must not enter the brush texture table");
        assert!(error.to_string().contains("4bpp power-of-two 8..128"));
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
    fn live_leak_diagnostic_matches_sealed_and_open_authored_geometry() {
        let sealed = authored_project();
        assert!(
            diagnose_brush_world_leak(sealed)
                .expect("sealed live pointfile check")
                .is_empty(),
            "the complete hollow room must be sealed"
        );

        let mut open = authored_project();
        open.active_scene_mut().brushes.remove(0);
        let diagnostic = diagnose_brush_world_leak(open).expect("open live pointfile check");
        assert!(
            diagnostic.path.len() >= 3,
            "open room must report occupant, portal, and exterior points: {diagnostic:?}"
        );
        assert_eq!(
            diagnostic.path[0],
            [128, 144, 128],
            "live diagnostics return authored units and preserve the cooker's one-engine-unit occupant lift"
        );
        assert!(
            diagnostic.likely_opening.len() >= 3,
            "the connected portal component should be retained as an editor target"
        );
        assert!(
            diagnostic
                .likely_opening_path_index
                .is_some_and(|index| index > 0 && index + 1 < diagnostic.path.len()),
            "the opening centroid must identify an interior pointfile point"
        );
    }

    #[test]
    fn leak_opening_merges_connected_coplanar_portal_fragments() {
        let plane = crate::brush::Plane {
            normal: [1, 0, 0],
            dist: 0,
        };
        let portal = |front_leaf, back_leaf, vertices| CompiledPortal {
            plane,
            front_leaf,
            back_leaf,
            vertices,
        };
        let portals = vec![
            portal(
                0,
                1,
                vec![
                    [0.0, 0.0, 0.0],
                    [0.0, 10.0, 0.0],
                    [0.0, 10.0, 10.0],
                    [0.0, 0.0, 10.0],
                ],
            ),
            portal(
                1,
                2,
                vec![
                    [0.0, 10.0, 0.0],
                    [0.0, 20.0, 0.0],
                    [0.0, 20.0, 10.0],
                    [0.0, 10.0, 10.0],
                ],
            ),
            // One long edge meets the two shorter edges above. The boundary
            // builder must split and cancel that T-junction rather than leave
            // an internal line in the opening outline.
            portal(
                2,
                3,
                vec![
                    [0.0, 0.0, 10.0],
                    [0.0, 20.0, 10.0],
                    [0.0, 20.0, 20.0],
                    [0.0, 0.0, 20.0],
                ],
            ),
            portal(
                4,
                5,
                vec![
                    [0.0, 100.0, 0.0],
                    [0.0, 110.0, 0.0],
                    [0.0, 110.0, 10.0],
                    [0.0, 100.0, 10.0],
                ],
            ),
        ];

        let outline = connected_coplanar_portal_outline(&portals, 0, |_| true);
        assert!(outline.len() >= 4, "merged opening needs a closed outline");
        let minimum = std::array::from_fn::<_, 3, _>(|axis| {
            outline
                .iter()
                .map(|point| point[axis])
                .fold(f64::INFINITY, f64::min)
        });
        let maximum = std::array::from_fn::<_, 3, _>(|axis| {
            outline
                .iter()
                .map(|point| point[axis])
                .fold(f64::NEG_INFINITY, f64::max)
        });
        assert_eq!(minimum, [0.0, 0.0, 0.0]);
        assert_eq!(maximum, [0.0, 20.0, 20.0]);
        assert!(
            outline.iter().all(|point| point[1] < 100.0),
            "a disconnected coplanar opening must not be merged"
        );
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

    #[test]
    fn scene_cook_emits_channel_filtered_destructible_brush_model() {
        let mut project = authored_project();
        let material = project.active_scene().brushes[0].faces[0].material;
        let scene = project.active_scene_mut();
        let node = scene.add_node(
            NodeId::ROOT,
            "Zenith Crate",
            NodeKind::Destructible {
                max_health: 45,
                damage_affinity: DestructibleDamageAffinity::Zenith,
                enabled: true,
            },
        );
        scene
            .node_mut(node)
            .expect("destructible")
            .transform
            .translation = [256.0, 64.0, 256.0];
        let mut brush = Brush::cuboid([224, 64, 224], [288, 128, 288]);
        brush.mover = Some(node);
        for face in &mut brush.faces {
            face.material = material;
        }
        scene.brushes.push(brush);

        let compiled = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: Path::new("."),
                mode: BrushWorldCookMode::Draft,
                ambient: [24; 3],
                texture_asset_base: 40,
            },
        )
        .expect("destructible brush world");
        assert_eq!(
            compiled.movers.len(),
            1,
            "destructibles are not door movers"
        );

        let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
        map.load(10, &mut SliceReader::new(&compiled.pxbsp.bytes))
            .expect("resident PXBSP");
        assert_eq!(map.brush_models().len(), 3);
        let entities = map.entities();
        let entity_index = (0..entities.len())
            .find(|&index| {
                entities
                    .get(index)
                    .is_some_and(|entity| entity.class_id == entity_class::BRUSH_DESTRUCTIBLE)
            })
            .expect("destructible entity");
        let entity = entities.get(entity_index).expect("destructible entity");
        assert_eq!(entity.flags, 0);
        assert_eq!(entity.model, 2);
        let payload = entities
            .payload_record::<PxbspBrushDestructible>(entity_index)
            .expect("destructible payload");
        assert_eq!(payload.destructible_index, 0);

        let mut destructibles = BrushDestructibleSet::<4>::default();
        destructibles
            .init_from_map(&map)
            .expect("runtime destructibles");
        assert_eq!(destructibles.len(), 1);
        let item = destructibles.get(0).expect("runtime destructible target");
        assert_eq!(item.destructible_index(), 0);
    }

    /// Hull 0 is served from the render BSP at runtime (Quake's hull 0), so
    /// the clipnode table carries only a one-node empty format sentinel in
    /// that slot instead of duplicating every brush plane.
    #[test]
    fn runtime_point_hull_uses_render_bsp_without_a_duplicate_clip_tree() {
        let compiled = authored_world(BrushWorldCookMode::Draft);
        let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
        map.load(9, &mut SliceReader::new(&compiled.pxbsp.bytes))
            .expect("resident PXBSP");
        for model_index in 0..map.brush_models().len() {
            let model = map.brush_models().get(model_index).expect("model");
            let render = map
                .model_collision_hull(model_index, 0)
                .expect("render-served point hull");
            if model_index == 0 {
                assert_eq!(
                    render.point_contents(Vec3I32 {
                        x: 512 * 4096,
                        y: 256 * 4096,
                        z: 512 * 4096,
                    }),
                    Some(psx_bsp::collision::CONTENTS_EMPTY),
                    "sealed room cavity"
                );
                assert_eq!(
                    render.point_contents(Vec3I32 {
                        x: 512 * 4096,
                        y: 32 * 4096,
                        z: 512 * 4096,
                    }),
                    Some(psx_bsp::collision::CONTENTS_SOLID),
                    "sealed room floor"
                );
            }
            let clipnodes = map
                .clip_nodes()
                .as_native_clip_nodes()
                .expect("validated native clipnodes");
            // SAFETY: `map` is a loaded resident map, whose validation
            // range-checked every clip node and model head node.
            let sentinel = unsafe {
                psx_bsp::collision::CollisionHull::from_native_clip_nodes(
                    map.planes(),
                    clipnodes,
                    model.head_nodes[1],
                )
            };
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
                        assert_eq!(
                            sentinel.point_contents(point),
                            Some(psx_bsp::collision::CONTENTS_EMPTY)
                        );
                        let expected = render.point_contents(point);
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
            assert!(checked > 100, "model {model_index}: {checked} points");
            if model_index > 0 {
                assert!(
                    solid > 0,
                    "submodel {model_index}: {checked} points, {solid} solid"
                );
            }
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
