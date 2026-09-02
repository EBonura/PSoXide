//! Normal-playtest ownership of one cooked resident PXBSP world.
//!
//! The editor manifest chooses this backend explicitly. Grid projects never
//! construct it; a malformed BSP manifest fails during scene initialization
//! rather than silently falling back to the synthetic grid room.

use core::fmt;
use core::mem::MaybeUninit;

use psx_asset::Texture;
use psx_bsp::collision::{LiquidContentsSample, TraceScratch};
use psx_bsp::collision_provider::{
    select_body_hull, valid_pxbsp_body_hulls, PxbspCollisionModel, PxbspCollisionProvider,
};
use psx_bsp::destructible::{BrushDestructibleSet, BrushDestructibleSetError};
use psx_bsp::mover::{BrushDoorSet, BrushDoorSetError};
use psx_bsp::pxbsp::{
    material_animation, material_blend, material_flags, PXBSP_MAX_VISIBILITY_BYTES,
};
use psx_bsp::pxbsp_resident::{PxbspMapLoadError, PxbspResidentMap};
use psx_bsp::render::{
    load_pxbsp_view_rotation, Camera, FrustumPlanes, PxbspTextureBinding, Renderer,
};
use psx_bsp::{SliceReadError, Vec3I32};
use psx_engine::Mat3I16;
use psx_engine::{
    commit_body_direction_with_trace_provider, commit_body_step_with_trace_provider,
    trace_collision, BodyStep, CharacterBlockerTraceProvider, CharacterCollisionAabb,
    CharacterCollisionCylinder, CharacterMotorConfig, CharacterMotorFrame, CharacterMotorInput,
    CharacterMotorState, CollisionQueryError, CollisionTrace, CollisionTraceQuery,
    CollisionTraceShape, CullMode, DepthPolicy, OtFrame, PrimitivePacketArena, PrimitiveSink,
    RoomPoint, ThirdPersonCameraConfig, ThirdPersonCameraFrame, ThirdPersonCameraInput,
    ThirdPersonCameraState, ThirdPersonCameraTarget, ViewVertex, WorldCamera, WorldProjection,
    WorldRenderPass, WorldSurfaceOptions,
};
use psx_game_runtime::destructibles::{DamageChannel, DamageOutcome, RuntimeDestructibles};
use psx_gpu::{
    material::{BlendMode, TextureMaterial, TextureWindow},
    prim::TriTextured,
};
use psx_level::{
    find_asset_of_kind, sky_flags, world_object_flags, AssetId, AssetKind, LevelWorldObjectRecord,
    MAX_ROOM_MATERIALS,
};
use psx_math::{cos_q12, sin_q12};

use crate::generated::{
    ASSETS, DESTRUCTIBLES, PXBSP_BODY_HULLS, PXBSP_MOVER_MODEL_INDICES, PXBSP_MOVER_NODE_IDS,
    PXBSP_WORLD, ROOMS, WORLD_OBJECTS,
};
use crate::world_objects_runtime::WorldObjectVisibility;
use crate::{
    ensure_room_texture_uploaded, ensure_texture_uploaded, pxbsp_frame_face_chain_arena,
    pxbsp_visible_face_chain_arena, PROJECTION,
};

pub(super) const MAX_BSP_DOORS: usize = 16;
pub(super) const MAX_BSP_DESTRUCTIBLES: usize = psx_level::MAX_DESTRUCTIBLES;
pub(super) const MAX_BSP_DYNAMIC_MODELS: usize = MAX_BSP_DOORS + MAX_BSP_DESTRUCTIBLES;
pub(super) const BSP_POINT_HULL_INDEX: usize = 0;

/// Body baked by `compile_brush_world` into hull one.
pub(super) const BSP_PLAYER_RADIUS: i32 = 1;
/// Body baked by `compile_brush_world` into hull one.
pub(super) const BSP_PLAYER_HEIGHT: i32 = 4;
/// Preserve the first-playable fixture's authored walking cadence when it has
/// no Character resource. Character-backed projects keep their authored speed.
pub(super) const BSP_FALLBACK_PLAYER_SPEED: i32 = 4 << 8;
/// Compact third-person rig for the characterless BSP debug controller. The
/// ordinary authored camera remains authoritative once a Character is bound.
/// Keep the boom close to the invisible debug body while it turns; this reads
/// as a player-height exploration camera and prevents annex columns from
/// filling the frame while the collision-driven route rounds a doorway.
pub(super) const BSP_FALLBACK_CAMERA_DISTANCE: i32 = 24;
pub(super) const BSP_FALLBACK_CAMERA_HEIGHT: i32 = 40;
pub(super) const BSP_FALLBACK_CAMERA_TARGET_HEIGHT: i32 = 32;
pub(super) const BSP_FALLBACK_CAMERA_CLEARANCE: i32 = 8;
/// Boom-to-wall margin for the point-traced follow camera in brush worlds.
pub(super) const BSP_CAMERA_WALL_MARGIN: i32 = 12;
pub(super) const BSP_FALLBACK_CAMERA_MARGIN: i32 = 4;
pub(super) const BSP_USE_DISTANCE: i32 = 256;
const BSP_BOUNDS_VISIBILITY_CACHE_SIZE: usize = 16;
const BSP_DESTRUCTIBLE_FRAGMENT_COLUMNS: usize = 4;
const BSP_DESTRUCTIBLE_FRAGMENT_ROWS: usize = 4;
const BSP_DESTRUCTIBLE_FRAGMENT_SETTLE_TICKS: u8 = 96;
const BSP_DESTRUCTIBLE_FRAGMENT_MOTION_TICKS: i32 = 48;
/// Largest distance one fragment can travel from the authored brush bounds.
const FRAGMENT_TRAVEL_MARGIN: i32 = 256;

/// Stack-owned list of the transformed brush submodels one collision query
/// composes over the static world.
///
/// The capacity is the authored door plus destructible ceiling, but a map only
/// ever fills as many slots as it has live movers. Declaring the buffer as a
/// fully-initialised `[PxbspCollisionModel; MAX_BSP_DYNAMIC_MODELS]` made every
/// collision entry point splat a 36-byte identity template across all 48 slots
/// first: 1,728 bytes of stack stores per call, on a map with no doors and two
/// destructibles. Only the written prefix is ever observable, so the tail is
/// left uninitialised and is never read.
struct CollisionModels {
    models: [MaybeUninit<PxbspCollisionModel>; MAX_BSP_DYNAMIC_MODELS],
    count: usize,
}

impl CollisionModels {
    /// An empty buffer. Materialises no slot storage.
    ///
    /// Deliberately not a `const fn`: constant-evaluating this whole value let
    /// codegen lower the uninitialised slots to a zero constant and splat it
    /// with `memset` at every call, which measured worse than the array fill
    /// it replaced (memset samples tripled). Built as a runtime value, the
    /// slot array costs nothing and only `count` is stored.
    fn new() -> Self {
        Self {
            // SAFETY: an array of `MaybeUninit` is itself always initialised;
            // the elements it holds are not, and `push` is the only writer.
            models: unsafe { MaybeUninit::uninit().assume_init() },
            count: 0,
        }
    }

    /// Append one submodel. Overflowing the authored ceiling is a cooker
    /// contract violation and panics exactly like the old fixed array's
    /// out-of-bounds write did.
    fn push(&mut self, model: PxbspCollisionModel) {
        self.models[self.count].write(model);
        self.count += 1;
    }

    /// The written prefix, in push order.
    fn as_slice(&self) -> &[PxbspCollisionModel] {
        let written = &self.models[..self.count];
        // SAFETY: `push` is the only way to advance `count`, and it writes the
        // slot it advances past. `MaybeUninit<T>` and `T` share layout, so the
        // written prefix is a valid `[PxbspCollisionModel]`.
        unsafe {
            &*(written as *const [MaybeUninit<PxbspCollisionModel>]
                as *const [PxbspCollisionModel])
        }
    }
}

/// One byte per target is enough to keep a brush fracture alive: zero means
/// intact/no event, 1..95 is the live ballistic phase, and 96 is settled floor
/// debris. Keeping the terminal age lets fragments remain in the world without
/// an allocation or a second persistent prop representation.

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct BspDestructibleFragmentEvent {
    age: u8,
}

impl BspDestructibleFragmentEvent {
    const EMPTY: Self = Self { age: 0 };
}

/// Dynamic world bounds used to link actors/instances to every BSP leaf they
/// touch. Coordinates use the playtest's integer engine-unit convention; the
/// BSP runtime converts them to Q20.12 only at the tree boundary.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BspVisibilityBounds {
    min: [i32; 3],
    max: [i32; 3],
}

impl BspVisibilityBounds {
    pub(super) const EMPTY: Self = Self {
        min: [0; 3],
        max: [0; 3],
    };

    pub(super) fn cylinder(position: [i32; 3], radius: i32, height: i32) -> Self {
        let radius = radius.max(0);
        let height = height.max(1);
        Self {
            min: [
                position[0].saturating_sub(radius),
                position[1],
                position[2].saturating_sub(radius),
            ],
            max: [
                position[0].saturating_add(radius),
                position[1].saturating_add(height),
                position[2].saturating_add(radius),
            ],
        }
    }

    pub(super) const fn aabb(min: [i32; 3], max: [i32; 3]) -> Self {
        Self { min, max }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct BspBoundsVisibilityCacheEntry {
    bounds: BspVisibilityBounds,
    observer_leaf: u16,
    visible: bool,
    valid: bool,
}

impl BspBoundsVisibilityCacheEntry {
    const EMPTY: Self = Self {
        bounds: BspVisibilityBounds::EMPTY,
        observer_leaf: 0,
        visible: false,
        valid: false,
    };
}

fn bounds_visibility_cache_slot(bounds: BspVisibilityBounds, observer_leaf: usize) -> usize {
    let mut hash = observer_leaf as u32;
    for component in bounds.min.into_iter().chain(bounds.max) {
        hash = hash.rotate_left(5) ^ component as u32;
    }
    hash as usize & (BSP_BOUNDS_VISIBILITY_CACHE_SIZE - 1)
}

const fn inset_toward(value: i32, observer: i32) -> i32 {
    if observer < value {
        value.saturating_sub(1)
    } else if observer > value {
        value.saturating_add(1)
    } else {
        value
    }
}

#[derive(Debug)]
pub(super) enum BspRuntimeInitError {
    EmptyWorld,
    NoMaterials,
    TooManyMaterials {
        count: usize,
        capacity: usize,
    },
    Map(PxbspMapLoadError<SliceReadError>),
    Doors(BrushDoorSetError),
    Destructibles(BrushDestructibleSetError),
    InvalidDestructibleTarget {
        target: usize,
        state: usize,
    },
    MoverMappingLength,
    MoverCount {
        cooked: usize,
        runtime: usize,
    },
    MoverModel {
        mover: usize,
        cooked: u16,
        runtime: usize,
    },
    DuplicateMoverNode(u32),
    InvalidBodyHullTable,
    MissingWorldHull(usize),
    MissingMoverHull {
        mover: usize,
        hull: usize,
    },
}

impl fmt::Display for BspRuntimeInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWorld => {
                formatter.write_str("manifest selected PXBSP but embedded no world")
            }
            Self::NoMaterials => formatter.write_str("PXBSP world contains no materials"),
            Self::TooManyMaterials { count, capacity } => write!(
                formatter,
                "PXBSP world contains {count} materials, exceeding runtime capacity {capacity}"
            ),
            Self::Map(error) => write!(formatter, "PXBSP map load failed: {error:?}"),
            Self::Doors(error) => write!(formatter, "PXBSP mover load failed: {error:?}"),
            Self::Destructibles(error) => {
                write!(formatter, "PXBSP destructible load failed: {error:?}")
            }
            Self::InvalidDestructibleTarget { target, state } => write!(
                formatter,
                "PXBSP destructible target {target} references missing shared state {state}"
            ),
            Self::MoverMappingLength => {
                formatter.write_str("PXBSP mover node/model mapping arrays have different lengths")
            }
            Self::MoverCount { cooked, runtime } => write!(
                formatter,
                "PXBSP mover mapping count {cooked} does not match runtime count {runtime}"
            ),
            Self::MoverModel {
                mover,
                cooked,
                runtime,
            } => write!(
                formatter,
                "PXBSP mover {mover} maps cooked model {cooked} but runtime loaded {runtime}"
            ),
            Self::DuplicateMoverNode(node) => {
                write!(
                    formatter,
                    "PXBSP mover mapping duplicates authored node {node}"
                )
            }
            Self::InvalidBodyHullTable => formatter
                .write_str("PXBSP body hull table must describe valid, unique hulls one and two"),
            Self::MissingWorldHull(hull) => {
                write!(
                    formatter,
                    "PXBSP world model is missing collision hull {hull}"
                )
            }
            Self::MissingMoverHull { mover, hull } => write!(
                formatter,
                "PXBSP mover {mover} is missing collision hull {hull}"
            ),
        }
    }
}

/// Resident brush world embedded in the ordinary [`crate::Playtest`] scene.
pub(super) struct BspRuntime {
    map: PxbspResidentMap,
    renderer: Renderer,
    doors: BrushDoorSet<MAX_BSP_DOORS>,
    destructible_targets: BrushDestructibleSet<MAX_BSP_DESTRUCTIBLES>,
    /// VRAM bindings are bounded by the same cooker/runtime material contract
    /// as grid rooms. Keeping them inline makes a resident BSP genuinely
    /// allocation-free at gameplay entry instead of depending on the tiny
    /// residual linker gap between static data and the reserved stack.
    materials: [Option<PxbspTextureBinding>; MAX_ROOM_MATERIALS],
    /// Set once every material has resolved to a ready VRAM slot; see
    /// [`BspRuntime::refresh_materials`] for why the table is final after that.
    materials_latched: bool,
    trace_scratch: TraceScratch,
    activation_visibility: [u8; PXBSP_MAX_VISIBILITY_BYTES],
    activation_leaf: Option<usize>,
    activation_visible_leaves: usize,
    /// Last observer point handed to [`Self::activation_row`] and the leaf it
    /// resolved to. One gameplay tick builds three activation masks (entities,
    /// model instances, logic points) from the same observer, and each used to
    /// repeat the same root-to-leaf descent of the render BSP.
    activation_observer: Option<RoomPoint>,
    activation_observer_leaf: Option<usize>,
    bounds_visibility_cache: [BspBoundsVisibilityCacheEntry; BSP_BOUNDS_VISIBILITY_CACHE_SIZE],
    fragment_events: [BspDestructibleFragmentEvent; MAX_BSP_DESTRUCTIBLES],
}

impl BspRuntime {
    pub(super) fn load_manifest() -> Result<Self, BspRuntimeInitError> {
        crate::game_trace("editor-playtest: bsp manifest begin");
        if PXBSP_WORLD.is_empty() {
            return Err(BspRuntimeInitError::EmptyWorld);
        }
        if PXBSP_MOVER_NODE_IDS.len() != PXBSP_MOVER_MODEL_INDICES.len() {
            return Err(BspRuntimeInitError::MoverMappingLength);
        }
        for (index, &node) in PXBSP_MOVER_NODE_IDS.iter().enumerate() {
            if PXBSP_MOVER_NODE_IDS[..index].contains(&node) {
                return Err(BspRuntimeInitError::DuplicateMoverNode(node));
            }
        }

        let map =
            PxbspResidentMap::from_static(0, PXBSP_WORLD).map_err(BspRuntimeInitError::Map)?;
        crate::game_trace("editor-playtest: bsp map ok");
        let mut doors = BrushDoorSet::EMPTY;
        doors
            .init_from_map(&map)
            .map_err(BspRuntimeInitError::Doors)?;
        crate::game_trace("editor-playtest: bsp doors ok");
        let mut destructible_targets = BrushDestructibleSet::EMPTY;
        destructible_targets
            .init_from_map(&map)
            .map_err(BspRuntimeInitError::Destructibles)?;
        for (target, destructible) in destructible_targets.iter().enumerate() {
            if destructible.destructible_index() >= DESTRUCTIBLES.len() {
                return Err(BspRuntimeInitError::InvalidDestructibleTarget {
                    target,
                    state: destructible.destructible_index(),
                });
            }
        }
        crate::game_trace("editor-playtest: bsp destructibles ok");
        if doors.len() != PXBSP_MOVER_MODEL_INDICES.len() {
            return Err(BspRuntimeInitError::MoverCount {
                cooked: PXBSP_MOVER_MODEL_INDICES.len(),
                runtime: doors.len(),
            });
        }
        for (index, (&model, door)) in PXBSP_MOVER_MODEL_INDICES
            .iter()
            .zip(doors.iter())
            .enumerate()
        {
            if usize::from(model) != door.model_index() {
                return Err(BspRuntimeInitError::MoverModel {
                    mover: index,
                    cooked: model,
                    runtime: door.model_index(),
                });
            }
        }
        if !valid_pxbsp_body_hulls(PXBSP_BODY_HULLS) {
            return Err(BspRuntimeInitError::InvalidBodyHullTable);
        }
        if map.model_collision_hull(0, BSP_POINT_HULL_INDEX).is_none() {
            return Err(BspRuntimeInitError::MissingWorldHull(BSP_POINT_HULL_INDEX));
        }
        for (mover, door) in doors.iter().enumerate() {
            if map
                .model_collision_hull(door.model_index(), BSP_POINT_HULL_INDEX)
                .is_none()
            {
                return Err(BspRuntimeInitError::MissingMoverHull {
                    mover,
                    hull: BSP_POINT_HULL_INDEX,
                });
            }
        }
        for (index, destructible) in destructible_targets.iter().enumerate() {
            if map
                .model_collision_hull(destructible.model_index(), BSP_POINT_HULL_INDEX)
                .is_none()
            {
                return Err(BspRuntimeInitError::MissingMoverHull {
                    mover: doors.len().saturating_add(index),
                    hull: BSP_POINT_HULL_INDEX,
                });
            }
        }
        for body_hull in PXBSP_BODY_HULLS {
            let hull = body_hull.hull_index;
            if map.model_collision_hull(0, hull).is_none() {
                return Err(BspRuntimeInitError::MissingWorldHull(hull));
            }
            for (mover, door) in doors.iter().enumerate() {
                if map.model_collision_hull(door.model_index(), hull).is_none() {
                    return Err(BspRuntimeInitError::MissingMoverHull { mover, hull });
                }
            }
            for (index, destructible) in destructible_targets.iter().enumerate() {
                if map
                    .model_collision_hull(destructible.model_index(), hull)
                    .is_none()
                {
                    return Err(BspRuntimeInitError::MissingMoverHull {
                        mover: doors.len().saturating_add(index),
                        hull,
                    });
                }
            }
        }

        let material_count = map.materials().len();
        if material_count == 0 {
            return Err(BspRuntimeInitError::NoMaterials);
        }
        if material_count > MAX_ROOM_MATERIALS {
            return Err(BspRuntimeInitError::TooManyMaterials {
                count: material_count,
                capacity: MAX_ROOM_MATERIALS,
            });
        }
        crate::game_trace("editor-playtest: bsp renderer begin");
        let mut renderer = Renderer::new_pxbsp_with_external_face_chains(
            map.faces().len(),
            map.nodes().len(),
            pxbsp_visible_face_chain_arena(),
            pxbsp_frame_face_chain_arena(),
        );
        crate::game_trace("editor-playtest: bsp renderer ok");
        // The frustum clip must match the projection this example renders
        // with (H and screen half-extents), or it clips too much or too little.
        renderer.set_view_projection(psx_bsp::render::ViewProjection {
            focal_length: PROJECTION.focal_length,
            half_width: i32::from(PROJECTION.screen_x),
            half_height: i32::from(PROJECTION.screen_y),
            ..psx_bsp::render::ViewProjection::DEFAULT
        });
        renderer.set_track_sky_apertures(
            ROOMS
                .first()
                .is_some_and(|room| room.sky.flags & sky_flags::THROUGH_SKY_SURFACES != 0),
        );
        crate::game_trace("editor-playtest: bsp runtime assemble");
        Ok(Self {
            map,
            renderer,
            doors,
            destructible_targets,
            materials: [None; MAX_ROOM_MATERIALS],
            materials_latched: false,
            trace_scratch: TraceScratch::new(),
            activation_visibility: [0; PXBSP_MAX_VISIBILITY_BYTES],
            activation_leaf: None,
            activation_visible_leaves: 0,
            activation_observer: None,
            activation_observer_leaf: None,
            bounds_visibility_cache: [BspBoundsVisibilityCacheEntry::EMPTY;
                BSP_BOUNDS_VISIBILITY_CACHE_SIZE],
            fragment_events: [BspDestructibleFragmentEvent::EMPTY; MAX_BSP_DESTRUCTIBLES],
        })
    }

    /// Resolve `observer`'s PVS leaf and make sure `activation_visibility`
    /// holds that leaf's decompressed row.
    ///
    /// Both the point-mask and bounds-mask queries need exactly this, and a
    /// gameplay tick runs three of them from one observer. The leaf descent
    /// walks only the static world model, so remembering the last observer
    /// point and its leaf is exact, not an approximation: the same point in
    /// the same map always lands in the same leaf. Doors, destructibles and
    /// other movers are separate brush models and cannot move this tree.
    fn activation_row(&mut self, observer: RoomPoint) -> Option<usize> {
        let q12 = |value: i32| value.saturating_mul(4096);
        let observer_leaf = if self.activation_observer == Some(observer) {
            self.activation_observer_leaf
        } else {
            let leaf = self.map.point_leaf_index(Vec3I32 {
                x: q12(observer.x),
                y: q12(observer.y),
                z: q12(observer.z),
            });
            self.activation_observer = Some(observer);
            self.activation_observer_leaf = leaf;
            leaf
        };
        let Some(observer_leaf) = observer_leaf else {
            self.activation_leaf = None;
            self.activation_visible_leaves = 0;
            return None;
        };
        if self.activation_leaf != Some(observer_leaf) {
            let Some(visible_leaves) = self
                .map
                .leaf_visibility_into(observer_leaf, &mut self.activation_visibility)
            else {
                self.activation_leaf = None;
                self.activation_visible_leaves = 0;
                return None;
            };
            self.activation_leaf = Some(observer_leaf);
            self.activation_visible_leaves = visible_leaves;
        }
        Some(observer_leaf)
    }

    /// Return one bit per world-space point visible from `observer` through
    /// the cooked PXBSP PVS. Invalid/solid points and malformed visibility
    /// fail closed. Positions are engine units; the map lookup consumes Q20.12.
    // psx-numeric-allow-next-line: one bit per queried point; the width IS the caller's point capacity
    pub(super) fn visible_points_mask(&mut self, observer: RoomPoint, points: &[[i32; 3]]) -> u64 {
        let q12 = |value: i32| value.saturating_mul(4096);
        if self.activation_row(observer).is_none() {
            return 0;
        }
        let visible_leaves = self.activation_visible_leaves;
        let mut mask = 0u64;
        for (index, point) in points.iter().enumerate().take(64) {
            let Some(leaf) = self.map.point_leaf_index(Vec3I32 {
                x: q12(point[0]),
                y: q12(point[1]),
                z: q12(point[2]),
            }) else {
                continue;
            };
            if leaf == 0 || leaf > visible_leaves {
                continue;
            }
            let visible_index = leaf - 1;
            if self.activation_visibility[visible_index >> 3] & (1 << (visible_index & 7)) != 0 {
                mask |= 1u64 << index;
            }
        }
        mask
    }

    /// Return one bit per actor/instance whose complete bounds touch at least
    /// one visible BSP leaf. This mirrors Quake's multi-leaf entity linking;
    /// origin-only PVS tests incorrectly suppress large or doorway-straddling
    /// actors while part of their geometry is still visible.
    // psx-numeric-allow-next-line: one bit per queried bounds; the width IS the caller's capacity
    pub(super) fn visible_bounds_mask(
        &mut self,
        observer: RoomPoint,
        bounds: &[BspVisibilityBounds],
        // psx-numeric-allow-next-line: one bit per queried bounds; return width is the caller's fixed capacity
    ) -> u64 {
        let q12 = |value: i32| value.saturating_mul(4096);
        let Some(observer_leaf) = self.activation_row(observer) else {
            return 0;
        };

        let mut mask = 0u64;
        for (index, bounds) in bounds.iter().enumerate().take(64) {
            let cache_slot = bounds_visibility_cache_slot(*bounds, observer_leaf);
            let cached = self.bounds_visibility_cache[cache_slot];
            if cached.valid
                && cached.observer_leaf as usize == observer_leaf
                && cached.bounds == *bounds
            {
                if cached.visible {
                    mask |= 1u64 << index;
                }
                continue;
            }
            let mins = Vec3I32 {
                x: q12(bounds.min[0]),
                y: q12(bounds.min[1]),
                z: q12(bounds.min[2]),
            };
            let maxs = Vec3I32 {
                x: q12(bounds.max[0]),
                y: q12(bounds.max[1]),
                z: q12(bounds.max[2]),
            };
            let visible = self.map.aabb_touches_visible_leaf(
                mins,
                maxs,
                &self.activation_visibility,
                self.activation_visible_leaves,
            );
            self.bounds_visibility_cache[cache_slot] = BspBoundsVisibilityCacheEntry {
                bounds: *bounds,
                observer_leaf: observer_leaf as u16,
                visible,
                valid: true,
            };
            if visible {
                mask |= 1u64 << index;
            }
        }
        mask
    }

    /// Resolve the cooker's shared non-BSP world-object registry against this
    /// resident brush world. Bounds first use Quake-style multi-leaf PVS; the
    /// objects that request strict occlusion then need at least one clear
    /// camera-to-bounds sample through the exact point collision hull.
    pub(super) fn visible_world_objects(
        &mut self,
        camera: WorldCamera,
        objects: &[LevelWorldObjectRecord],
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> WorldObjectVisibility {
        let observer = RoomPoint::new(camera.position.x, camera.position.y, camera.position.z);
        // The strict-occlusion samples below trace from the camera to each
        // object across the whole level, up to seven long segments per
        // object per frame, and on the Cortex whole-level tape they were
        // most of the collision cost. An object outside the view frustum is
        // never drawn, so it is not traced either. This is the world pass's
        // own frustum, built the same way it builds it.
        let frustum = {
            let pxbsp = pxbsp_camera(camera);
            let view = load_pxbsp_view_rotation(pxbsp.origin, pxbsp_view_rotation(camera));
            FrustumPlanes::from_view(
                &view.rotation,
                view.translation,
                [
                    pxbsp.origin.x >> 12,
                    pxbsp.origin.y >> 12,
                    pxbsp.origin.z >> 12,
                ],
                self.renderer.view_projection(),
            )
        };
        let mut visibility = WorldObjectVisibility::NONE;
        let count = objects.len().min(psx_level::MAX_WORLD_OBJECTS);
        let mut first = 0usize;
        while first < count {
            let chunk_count = (count - first).min(u64::BITS as usize);
            let mut bounds = [BspVisibilityBounds::EMPTY; u64::BITS as usize];
            for local in 0..chunk_count {
                let object = &objects[first + local];
                bounds[local] = BspVisibilityBounds::aabb(object.bounds_min, object.bounds_max);
            }
            let pvs = self.visible_bounds_mask(observer, &bounds[..chunk_count]);
            for local in 0..chunk_count {
                if pvs & (1u64 << local) == 0 {
                    continue;
                }
                let object = &objects[first + local];
                if object.destructible != psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE
                    && !destructibles.alive(usize::from(object.destructible))
                {
                    continue;
                }
                if object.flags & world_object_flags::DIRECT_BRUSH_OCCLUSION != 0
                    && frustum.box_surely_outside(object.bounds_min, object.bounds_max)
                {
                    continue;
                }
                if object.flags & world_object_flags::DIRECT_BRUSH_OCCLUSION != 0
                    && !self.world_object_directly_visible(
                        observer,
                        object.bounds_min,
                        object.bounds_max,
                        destructibles,
                    )
                {
                    continue;
                }
                visibility.set(first + local);
            }
            first += chunk_count;
        }
        visibility
    }

    /// Direct brush visibility for gameplay queries such as interaction
    /// prompts. Rendering adds the broader leaf-PVS pass; a nearby prompt only
    /// needs to prove that the player and marker are not separated by a wall.
    pub(super) fn typed_world_object_directly_visible(
        &mut self,
        observer: RoomPoint,
        kind: u8,
        source_index: usize,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> bool {
        let Ok(source_index) = u16::try_from(source_index) else {
            return false;
        };
        let Ok(index) = WORLD_OBJECTS.binary_search_by_key(&(kind, source_index), |object| {
            (object.kind, object.source_index)
        }) else {
            return false;
        };
        let object = &WORLD_OBJECTS[index];
        if object.destructible != psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE
            && !destructibles.alive(usize::from(object.destructible))
        {
            return false;
        }
        self.world_object_directly_visible(
            observer,
            object.bounds_min,
            object.bounds_max,
            destructibles,
        )
    }

    fn world_object_directly_visible(
        &mut self,
        observer: RoomPoint,
        min: [i32; 3],
        max: [i32; 3],
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> bool {
        let observer_array = [observer.x, observer.y, observer.z];
        if (0..3).all(|axis| observer_array[axis] >= min[axis] && observer_array[axis] <= max[axis])
        {
            return true;
        }
        let center = core::array::from_fn::<_, 3, _>(|axis| {
            min[axis].saturating_add(max[axis].saturating_sub(min[axis]) / 2)
        });
        // Center plus the six face centers catches thin cards, compact
        // beacons, and a prop peeking around one side of a brush without the
        // cost of tracing all eight corners. Pull endpoints one unit toward
        // the eye so a card mounted flush on a wall is not rejected by contact
        // exactly at its authored support plane.
        let samples = [
            center,
            [min[0], center[1], center[2]],
            [max[0], center[1], center[2]],
            [center[0], min[1], center[2]],
            [center[0], max[1], center[2]],
            [center[0], center[1], min[2]],
            [center[0], center[1], max[2]],
        ];
        // A sample point whose leaf is not in the observer's potentially
        // visible set cannot be seen from the observer, PVS being a superset
        // of true visibility, so it is not worth a trace across the level.
        // The observer's row is normally already resolved by the visibility
        // pass; a gameplay caller with another observer resolves it here.
        let pvs_ready = self.activation_row(observer).is_some();
        let visible_leaves = self.activation_visible_leaves;
        for sample in samples {
            let endpoint = RoomPoint::new(
                inset_toward(sample[0], observer.x),
                inset_toward(sample[1], observer.y),
                inset_toward(sample[2], observer.z),
            );
            if pvs_ready {
                let q12 = |value: i32| value.saturating_mul(4096);
                let leaf = self.map.point_leaf_index(Vec3I32 {
                    x: q12(endpoint.x),
                    y: q12(endpoint.y),
                    z: q12(endpoint.z),
                });
                let potentially_visible = match leaf {
                    Some(leaf) if leaf != 0 && leaf <= visible_leaves => {
                        let visible_index = leaf - 1;
                        self.activation_visibility[visible_index >> 3] & (1 << (visible_index & 7))
                            != 0
                    }
                    // Solid, out of range or unresolved: never visible.
                    Some(_) => false,
                    None => false,
                };
                if !potentially_visible {
                    continue;
                }
            }
            if self
                .trace_point_segment(observer, endpoint, &[], destructibles)
                .is_ok_and(|trace| !trace.hit())
            {
                return true;
            }
        }
        false
    }

    /// Sample the static world's point hull at the player's feet, torso, and
    /// head. Liquid movers are rejected by the cooker, so model zero is the
    /// complete contents authority. A malformed query fails safely to no
    /// liquid behavior rather than inventing a hazard.
    pub(super) fn player_contents(
        &self,
        position: RoomPoint,
        height: i32,
    ) -> Option<LiquidContentsSample> {
        let hull = self.map.model_collision_hull(0, BSP_POINT_HULL_INDEX)?;
        let height = height.max(1);
        let sample_y = [
            position.y.saturating_add(1),
            position.y.saturating_add(height >> 1),
            position.y.saturating_add(height.saturating_sub(1)),
        ];
        let points = sample_y.map(|y| Vec3I32 {
            x: position.x.saturating_mul(4096),
            y: y.saturating_mul(4096),
            z: position.z.saturating_mul(4096),
        });
        hull.sample_liquid_contents(&points)
    }

    /// Resolve every PXBSP material through the normal playtest VRAM owner.
    /// Returns `true` only when every queued upload is ready.
    ///
    /// The caller re-runs this every background tick to pick up uploads that
    /// landed late, so it kept re-parsing every PSXT header and re-querying
    /// every VRAM slot for the whole level, rebuilding a byte-identical
    /// binding table on every frame of gameplay. Latch instead: the table is
    /// only rewritten while a material is still unresolved.
    ///
    /// A resolved table stays correct for as long as its VRAM slots do.
    /// `evict_unreferenced_vram` cannot reach them (it belongs to the grid room
    /// window, and the residency owner returns before it whenever `self.bsp` is
    /// resident), but `release_gameplay_vram` frees every slot when the scene
    /// leaves gameplay, and `Scene::init` runs once at boot rather than per
    /// entry, so this runtime outlives that. The scene calls
    /// [`invalidate_materials`](Self::invalidate_materials) on that edge.
    pub(super) fn refresh_materials(&mut self) -> bool {
        if self.materials_latched {
            return true;
        }
        let mut ready = true;
        for (index, material) in self.map.materials().iter().enumerate() {
            if material.flags & (material_flags::SKY_APERTURE | material_flags::DIRECTIONAL_SKY)
                != 0
            {
                self.materials[index] = None;
                continue;
            }
            let asset_id = AssetId(material.texture_asset);
            let asset =
                find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture).unwrap_or_else(|| {
                    panic!(
                        "PXBSP material {index} references missing texture asset {}",
                        material.texture_asset
                    )
                });
            assert!(
                !asset.bytes.is_empty(),
                "PXBSP texture asset {} has no baked bytes; streamed BSP textures are not implemented",
                material.texture_asset
            );
            let texture = Texture::from_bytes(asset.bytes).unwrap_or_else(|_| {
                panic!(
                    "PXBSP texture asset {} is not a valid PSXT",
                    material.texture_asset
                )
            });
            // Polygon blending and palette-zero cutout are independent PS1
            // features. An opaque material may still use CLUT entry zero as a
            // binary mask, so preserve an explicit PSXT transparent-zero flag
            // instead of forcing all opaque room materials to opaque-zero.
            let slot = if texture.index_zero_transparent() {
                ensure_texture_uploaded(asset_id, asset.bytes)
            } else if material.blend_mode == material_blend::OPAQUE {
                ensure_room_texture_uploaded(asset_id, asset.bytes)
            } else {
                ensure_texture_uploaded(asset_id, asset.bytes)
            };
            let Some(slot) = slot else {
                self.materials[index] = None;
                ready = false;
                continue;
            };
            if !slot.ready {
                self.materials[index] = None;
                ready = false;
                continue;
            }
            let width = u8::try_from(slot.texture_width)
                .expect("PXBSP texture width exceeds the packet UV contract");
            let height = u8::try_from(slot.texture_height)
                .expect("PXBSP texture height exceeds the packet UV contract");
            self.materials[index] = Some(PxbspTextureBinding {
                texture_page: slot.tpage_word,
                clut: slot.clut_word,
                texture_window_word: slot.texture_window.word(),
                uv_origin: [0, 0],
                page_uv_origin: slot.texture_window.origin_texels(),
                texture_size: [width, height],
            });
        }
        self.materials_latched = ready && self.materials_ready();
        self.materials_latched
    }

    /// Drop the resolved material table so the next `refresh_materials` rebuilds
    /// it. Call whenever the gameplay VRAM those bindings point at is released.
    pub(super) fn invalidate_materials(&mut self) {
        self.materials_latched = false;
        for binding in self.materials.iter_mut() {
            *binding = None;
        }
    }

    pub(super) fn materials_ready(&self) -> bool {
        !self.materials.is_empty()
            && self
                .map
                .materials()
                .iter()
                .zip(&self.materials)
                .all(|(material, binding)| {
                    material.flags
                        & (material_flags::SKY_APERTURE | material_flags::DIRECTIONAL_SKY)
                        != 0
                        || binding.is_some()
                })
    }

    pub(super) fn tick_doors(&mut self) {
        self.doors.tick();
    }

    /// Apply one authored melee capsule to every compatible destructible it
    /// touches. A bit per destructible prevents multiple active frames or
    /// multiple weapon capsules from damaging the same object twice during a
    /// single swing.
    pub(super) fn damage_brush_destructibles_with_capsule(
        &self,
        start: [i32; 3],
        end: [i32; 3],
        radius: u16,
        channel: DamageChannel,
        damage: u16,
        destructibles: &mut RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> DamageOutcome {
        let radius = i32::from(radius);
        let capsule_min = core::array::from_fn::<_, 3, _>(|axis| {
            start[axis].min(end[axis]).saturating_sub(radius)
        });
        let capsule_max = core::array::from_fn::<_, 3, _>(|axis| {
            start[axis].max(end[axis]).saturating_add(radius)
        });
        let mut result = DamageOutcome::default();
        let count = self.destructible_targets.len();
        for index in 0..count {
            let Some((bounds_min, bounds_max)) = self.destructible_bounds(index, destructibles)
            else {
                continue;
            };
            let overlaps = (0..3).all(|axis| {
                capsule_min[axis] <= bounds_max[axis] && capsule_max[axis] >= bounds_min[axis]
            });
            if !overlaps {
                continue;
            }
            let Some(target) = self.destructible_targets.get(index) else {
                continue;
            };
            let outcome = destructibles.apply_damage(target.destructible_index(), channel, damage);
            if outcome.connected {
                result.connected = true;
                result.broke |= outcome.broke;
            }
        }

        result
    }

    fn destructible_bounds(
        &self,
        index: usize,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> Option<([i32; 3], [i32; 3])> {
        let destructible = self.destructible_targets.get(index)?;
        if !destructibles.alive(destructible.destructible_index()) {
            return None;
        }
        self.destructible_bounds_unchecked(index)
    }

    fn destructible_bounds_unchecked(&self, index: usize) -> Option<([i32; 3], [i32; 3])> {
        let destructible = self.destructible_targets.get(index)?;
        let model = self.map.brush_models().get(destructible.model_index())?;
        let origin = destructible.transform().origin;
        let origin = [origin.x >> 12, origin.y >> 12, origin.z >> 12];
        Some((
            [
                origin[0].saturating_add(i32::from(model.mins.x)),
                origin[1].saturating_add(i32::from(model.mins.y)),
                origin[2].saturating_add(i32::from(model.mins.z)),
            ],
            [
                origin[0].saturating_add(i32::from(model.maxs.x)),
                origin[1].saturating_add(i32::from(model.maxs.y)),
                origin[2].saturating_add(i32::from(model.maxs.z)),
            ],
        ))
    }

    /// Center and conservative size for one shared destructible state. This
    /// deliberately remains available after health reaches zero so the break
    /// flare can be emitted where the now-hidden geometry used to be.
    pub(super) fn destructible_effect_origin(&self, state_index: usize) -> Option<([i32; 3], u16)> {
        for target_index in 0..self.destructible_targets.len() {
            let target = self.destructible_targets.get(target_index)?;
            if target.destructible_index() != state_index {
                continue;
            }
            let (min, max) = self.destructible_bounds_unchecked(target_index)?;
            let center = core::array::from_fn(|axis| min[axis].saturating_add(max[axis]) / 2);
            let largest_extent = (0..3)
                .map(|axis| max[axis].saturating_sub(min[axis]).unsigned_abs())
                .max()
                .unwrap_or(256);
            let radius = largest_extent.clamp(192, 768) as u16;
            return Some((center, radius));
        }
        None
    }

    /// Begin a real geometry fracture for every brush target sharing this
    /// destructible state. The intact BSP submodel disappears in the same
    /// update and these pieces start at its exact visible plane.
    pub(super) fn spawn_destructible_fragments(&mut self, state_index: usize) {
        for target_index in 0..self.destructible_targets.len() {
            let Some(target) = self.destructible_targets.get(target_index) else {
                continue;
            };
            if target.destructible_index() == state_index {
                self.fragment_events[target_index].age = 1;
            }
        }
    }

    pub(super) fn advance_destructible_fragments(&mut self, delta_vblanks: u16) {
        let delta = delta_vblanks.min(u16::from(u8::MAX)) as u8;
        for event in &mut self.fragment_events[..self.destructible_targets.len()] {
            if event.age != 0 && event.age < BSP_DESTRUCTIBLE_FRAGMENT_SETTLE_TICKS {
                event.age = event
                    .age
                    .saturating_add(delta)
                    .min(BSP_DESTRUCTIBLE_FRAGMENT_SETTLE_TICKS);
            }
        }
    }

    /// Draw the visible face of a broken brush as sixteen independently
    /// tumbling textured pieces. Collision still comes from the authored
    /// closed brush before it breaks; only the thin display face is fractured.
    pub(super) fn draw_destructible_fragments<const OT_DEPTH: usize>(
        &self,
        camera: &WorldCamera,
        options: WorldSurfaceOptions,
        triangles: &mut impl PrimitiveSink<TriTextured>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) {
        for target_index in 0..self.destructible_targets.len() {
            let age = self.fragment_events[target_index].age;
            if age == 0 {
                continue;
            }
            let Some(target) = self.destructible_targets.get(target_index) else {
                continue;
            };
            // Settled debris is resubmitted every frame for the rest of the
            // level, so an off-screen break kept paying for sixteen
            // double-sided quads forever. Reject the whole lattice when its
            // bounding sphere lies entirely outside the view frustum, before
            // the material scan and any per-piece work. The radius
            // over-estimates the half-diagonal and covers the launch excursion
            // `bsp_fragment_quad` adds.
            let Some((min, max)) = self.destructible_bounds_unchecked(target_index) else {
                continue;
            };
            let centre = RoomPoint::new(
                min[0].saturating_add(max[0]) / 2,
                min[1].saturating_add(max[1]) / 2,
                min[2].saturating_add(max[2]) / 2,
            );
            let radius = max[0]
                .saturating_sub(min[0])
                .max(max[1].saturating_sub(min[1]))
                .max(max[2].saturating_sub(min[2]))
                .saturating_add(FRAGMENT_TRAVEL_MARGIN);
            if sphere_outside_view_frustum(camera.view_vertex(centre), radius, camera.projection) {
                continue;
            }
            let Some(model) = self.map.brush_models().get(target.model_index()) else {
                continue;
            };
            let face_range = usize::from(model.first_face)
                ..usize::from(model.first_face.saturating_add(model.face_count));
            // Brushes with a deliberately thin display face still carry a
            // closed collision shell. Prefer the authored animated/blended
            // face over those shell faces, then fracture its exact material.
            // Every aspect-matched shard repeats one complete source tile;
            // this keeps the dense authored pattern continuous at the break
            // without deriving a bogus min/max from wrapped u8 UVs.
            let Some((_material_index, binding, material)) = face_range
                .clone()
                .filter_map(|face_index| {
                    let face = self.map.faces().get(face_index)?;
                    let material_index = usize::try_from(face.texture).ok()?;
                    Some((
                        material_index,
                        self.materials.get(material_index)?.as_ref()?,
                        self.map.materials().get(material_index)?,
                    ))
                })
                .max_by_key(|(_, _, material)| {
                    u8::from(material.blend_mode != material_blend::OPAQUE) * 2
                        + u8::from(material.animation_kind != material_animation::STATIC)
                })
            else {
                continue;
            };
            let blend = match material.blend_mode {
                material_blend::AVERAGE => BlendMode::Average,
                material_blend::ADD => BlendMode::Add,
                material_blend::SUBTRACT => BlendMode::Subtract,
                material_blend::ADD_QUARTER => BlendMode::AddQuarter,
                _ => BlendMode::Opaque,
            };
            let texture_window = TextureWindow::power_of_two_tile(
                binding.page_uv_origin[0],
                binding.page_uv_origin[1],
                binding.texture_size[0],
                binding.texture_size[1],
            );
            let fragment_material =
                TextureMaterial::opaque(binding.clut, binding.texture_page, (128, 128, 128))
                    .with_blend_mode(blend)
                    .with_texture_window(texture_window);
            let fragment_options = options
                .with_depth_policy(DepthPolicy::Average)
                .with_cull_mode(CullMode::None)
                .with_material_layer(fragment_material)
                .with_textured_triangle_splitting(true)
                .with_textured_triangle_max_edge(0);

            let x_span = max[0].saturating_sub(min[0]);
            let z_span = max[2].saturating_sub(min[2]);
            let horizontal_axis = if x_span >= z_span { 0 } else { 2 };
            let depth_axis = if horizontal_axis == 0 { 2 } else { 0 };
            // The author-facing display plane is the maximum side of the
            // otherwise invisible collision hull.
            let plane_depth = max[depth_axis];
            let horizontal_min = min[horizontal_axis];
            let horizontal_span = max[horizontal_axis].saturating_sub(horizontal_min);
            let vertical_span = max[1].saturating_sub(min[1]);
            let u_max = binding.texture_size[0].saturating_sub(1);
            let v_max = binding.texture_size[1].saturating_sub(1);

            for row in 0..BSP_DESTRUCTIBLE_FRAGMENT_ROWS {
                for column in 0..BSP_DESTRUCTIBLE_FRAGMENT_COLUMNS {
                    let h0 = horizontal_min.saturating_add(
                        horizontal_span.saturating_mul(column as i32)
                            / BSP_DESTRUCTIBLE_FRAGMENT_COLUMNS as i32,
                    );
                    let h1 = horizontal_min.saturating_add(
                        horizontal_span.saturating_mul((column + 1) as i32)
                            / BSP_DESTRUCTIBLE_FRAGMENT_COLUMNS as i32,
                    );
                    let y0 = min[1].saturating_add(
                        vertical_span.saturating_mul(row as i32)
                            / BSP_DESTRUCTIBLE_FRAGMENT_ROWS as i32,
                    );
                    let y1 = min[1].saturating_add(
                        vertical_span.saturating_mul((row + 1) as i32)
                            / BSP_DESTRUCTIBLE_FRAGMENT_ROWS as i32,
                    );
                    let quad = bsp_fragment_quad(
                        horizontal_axis,
                        plane_depth,
                        [h0, h1],
                        [y0, y1],
                        min[1],
                        age,
                        row,
                        column,
                    );
                    let _ = world.submit_textured_world_quad(
                        triangles,
                        *camera,
                        quad,
                        [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)],
                        fragment_material,
                        fragment_options,
                    );
                }
            }
        }
    }

    pub(super) fn set_door_open(&mut self, mover: usize, open: bool) {
        self.doors
            .get_mut(mover)
            .unwrap_or_else(|| panic!("logic references missing PXBSP mover {mover}"))
            .set_open(open);
    }

    pub(super) fn nearest_door(&self, player: RoomPoint, distance: i32) -> Option<usize> {
        // Squared engine-unit distances: a world axis reaches ~2^21, so the
        // square reaches ~2^42 and an i32 accumulator would wrap and report a
        // far door as adjacent. One interaction scan per use press, not a
        // per-frame path, and it narrows back before it leaves.
        // psx-numeric-allow-next-line: squared-distance accumulator, see above
        let limit = i64::from(distance).saturating_mul(i64::from(distance));
        self.doors
            .iter()
            .enumerate()
            .filter_map(|(index, door)| {
                if !door.enabled() {
                    return None;
                }
                let model = self.map.brush_models().get(door.model_index())?;
                let origin = door.transform().origin;
                let origin = [origin.x >> 12, origin.y >> 12, origin.z >> 12];
                let mins = [
                    origin[0].saturating_add(i32::from(model.mins.x)),
                    origin[1].saturating_add(i32::from(model.mins.y)),
                    origin[2].saturating_add(i32::from(model.mins.z)),
                ];
                let maxs = [
                    origin[0].saturating_add(i32::from(model.maxs.x)),
                    origin[1].saturating_add(i32::from(model.maxs.y)),
                    origin[2].saturating_add(i32::from(model.maxs.z)),
                ];
                let player = [player.x, player.y, player.z];
                let delta = core::array::from_fn::<_, 3, _>(|axis| {
                    if player[axis] < mins[axis] {
                        mins[axis].saturating_sub(player[axis])
                    } else if player[axis] > maxs[axis] {
                        player[axis].saturating_sub(maxs[axis])
                    } else {
                        0
                    }
                });
                // psx-numeric-allow-next-line: squared-distance accumulator
                let dx = i64::from(delta[0]);
                // psx-numeric-allow-next-line: squared-distance accumulator
                let dy = i64::from(delta[1]);
                // psx-numeric-allow-next-line: squared-distance accumulator
                let dz = i64::from(delta[2]);
                let squared = dx * dx + dy * dy + dz * dz;
                (squared <= limit).then_some((squared, index))
            })
            .min_by_key(|(squared, _)| *squared)
            .map(|(_, index)| index)
    }

    pub(super) fn update_motor(
        &mut self,
        motor: &mut CharacterMotorState,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        let mut models = CollisionModels::new();
        self.collision_models(&mut models, destructibles);
        let shape = CollisionTraceShape::Body {
            radius: config.radius,
            height: config.height,
        };
        let hull_index = select_body_hull(PXBSP_BODY_HULLS, config.radius, config.height)
            .ok_or(CollisionQueryError)?;
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            hull_index,
            models.as_slice(),
            shape,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP player collision provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        motor.update_vblanks_with_trace_provider(&mut provider, input, config, delta_vblanks)
    }

    /// Move one gameplay entity through the same static-world, transformed-
    /// mover, dynamic-cylinder, and authored-prop trace stack used by the
    /// player motor.
    pub(super) fn commit_body_step(
        &mut self,
        start: RoomPoint,
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<BodyStep, CollisionQueryError> {
        let mut models = CollisionModels::new();
        self.collision_models(&mut models, destructibles);
        let radius = radius.max(0);
        let height = height.max(1);
        let shape = CollisionTraceShape::Body { radius, height };
        let hull_index =
            select_body_hull(PXBSP_BODY_HULLS, radius, height).ok_or(CollisionQueryError)?;
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            hull_index,
            models.as_slice(),
            shape,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP entity collision provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        commit_body_step_with_trace_provider(&mut provider, start, dx, dz, radius, height)
    }

    /// Probe one Quake-style monster direction without the player's internal
    /// axis-slide retries. The chase search owns the cardinal alternatives.
    pub(super) fn commit_body_direction(
        &mut self,
        start: RoomPoint,
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<BodyStep, CollisionQueryError> {
        let mut models = CollisionModels::new();
        self.collision_models(&mut models, destructibles);
        let radius = radius.max(0);
        let height = height.max(1);
        let shape = CollisionTraceShape::Body { radius, height };
        let hull_index =
            select_body_hull(PXBSP_BODY_HULLS, radius, height).ok_or(CollisionQueryError)?;
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            hull_index,
            models.as_slice(),
            shape,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP entity collision provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        commit_body_direction_with_trace_provider(&mut provider, start, dx, dz, radius, height)
    }

    /// Whether a melee contact segment between two actor positions is free of
    /// static world, transformed mover geometry, and checked live prop AABBs.
    /// Actor cylinders are deliberately not composed: the attacker and
    /// defender are actors. Provider failure fails closed (blocked): malformed
    /// world or prop data must not grant damage through it.
    pub(super) fn melee_segment_clear(
        &mut self,
        from: RoomPoint,
        to: RoomPoint,
        aabb_blockers: &[CharacterCollisionAabb],
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> bool {
        self.trace_point_segment(from, to, aabb_blockers, destructibles)
            .is_ok_and(|trace| !trace.hit())
    }

    /// Trace a gameplay point sweep through static BSP, transformed brush
    /// movers, and checked live prop AABBs. Projectile actor collision still
    /// uses its authored swept-sphere radius; this provider owns only the
    /// world's centerline clip until arbitrary-radius BSP hulls are cooked.
    pub(super) fn trace_point_segment(
        &mut self,
        from: RoomPoint,
        to: RoomPoint,
        aabb_blockers: &[CharacterCollisionAabb],
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> Result<CollisionTrace, CollisionQueryError> {
        let mut models = CollisionModels::new();
        self.collision_models(&mut models, destructibles);
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            BSP_POINT_HULL_INDEX,
            models.as_slice(),
            CollisionTraceShape::Point,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP melee occlusion provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, &[], aabb_blockers);
        trace_collision(&mut provider, CollisionTraceQuery::point(from, to))
    }

    pub(super) fn update_camera(
        &mut self,
        camera: &mut ThirdPersonCameraState,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
        aabb_blockers: &[CharacterCollisionAabb],
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) -> Result<ThirdPersonCameraFrame, CollisionQueryError> {
        let mut models = CollisionModels::new();
        self.collision_models(&mut models, destructibles);
        // The camera is a point and the renderer now clips world polygons at
        // the near plane, so use Quake's balanced render BSP (hull 0) for the
        // spring-arm trace. The authored collision margin still stops the eye
        // before a wall. Walking the expanded body-hull brush chains here made
        // a single E1M1 camera solve roughly as expensive as rendering a room.
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            BSP_POINT_HULL_INDEX,
            models.as_slice(),
            CollisionTraceShape::Point,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP camera collision provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, &[], aabb_blockers);
        camera.update_vblanks_with_trace_provider(
            PROJECTION,
            &mut provider,
            target,
            input,
            config,
            delta_vblanks,
        )
    }

    pub(super) fn draw<const DEPTH: usize>(
        &mut self,
        camera: WorldCamera,
        material_tick: u32,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
        primitive_packets: &mut PrimitivePacketArena<'_>,
        ot: &mut OtFrame<'_, DEPTH>,
    ) -> bool {
        assert_eq!(
            DEPTH, 2048,
            "PXBSP packet tags require the canonical 2048-entry ordering table"
        );
        assert!(
            self.materials_ready(),
            "PXBSP render started before texture residency"
        );
        psx_gte::scene::set_screen_offset(
            i32::from(PROJECTION.screen_x) << 16,
            i32::from(PROJECTION.screen_y) << 16,
        );
        psx_gte::scene::set_projection_plane(
            PROJECTION.focal_length.clamp(1, i32::from(u16::MAX)) as u16
        );
        psx_gte::scene::set_avsz_weights(0x155, 0x100);
        let view_rotation = pxbsp_view_rotation(camera);
        let camera = pxbsp_camera(camera);
        let view = load_pxbsp_view_rotation(camera.origin, view_rotation);
        let capacity = primitive_packets.remaining_words();
        let Some(mut reservation) = primitive_packets.reserve_packet_words(capacity) else {
            return false;
        };
        let (used_words, packet_count, visible_sky_apertures) = {
            let packets = reservation.words_mut();
            let world = self.renderer.draw_pxbsp_world(
                &self.map,
                camera,
                view,
                &self.materials,
                material_tick,
                packets,
            );
            let mut used_words = world.packet_words;
            let mut packet_count = world.stats.packets as usize;
            let mut visible_sky_apertures = world.stats.visible_sky_apertures;

            for door in self.doors.iter() {
                let Some(frame) = self.renderer.draw_pxbsp_model(
                    &self.map,
                    door.model_index(),
                    door.transform(),
                    camera,
                    view,
                    &self.materials,
                    material_tick,
                    &mut packets[used_words..],
                ) else {
                    panic!("validated PXBSP mover model disappeared");
                };
                used_words = used_words
                    .checked_add(frame.packet_words)
                    .expect("PXBSP packet word count overflow");
                packet_count = packet_count
                    .checked_add(frame.stats.packets as usize)
                    .expect("PXBSP packet count overflow");
                visible_sky_apertures =
                    visible_sky_apertures.saturating_add(frame.stats.visible_sky_apertures);
            }
            for destructible in self
                .destructible_targets
                .iter()
                .filter(|target| destructibles.alive(target.destructible_index()))
            {
                let Some(frame) = self.renderer.draw_pxbsp_model(
                    &self.map,
                    destructible.model_index(),
                    destructible.transform(),
                    camera,
                    view,
                    &self.materials,
                    material_tick,
                    &mut packets[used_words..],
                ) else {
                    panic!("validated PXBSP destructible model disappeared");
                };
                used_words = used_words
                    .checked_add(frame.packet_words)
                    .expect("PXBSP packet word count overflow");
                packet_count = packet_count
                    .checked_add(frame.stats.packets as usize)
                    .expect("PXBSP packet count overflow");
                visible_sky_apertures =
                    visible_sky_apertures.saturating_add(frame.stats.visible_sky_apertures);
            }
            (used_words, packet_count, visible_sky_apertures)
        };
        let stream = reservation
            .commit(used_words, packet_count)
            .expect("PXBSP renderer reported an invalid shared-arena stream");
        unsafe {
            ot.add_committed_tagged_packet_stream_unchecked(stream);
        }
        visible_sky_apertures != 0
    }

    fn collision_models(
        &self,
        output: &mut CollisionModels,
        destructibles: &RuntimeDestructibles<{ psx_level::MAX_DESTRUCTIBLES }>,
    ) {
        for door in self.doors.iter() {
            output.push(PxbspCollisionModel::new(
                u16::try_from(door.model_index()).expect("validated PXBSP mover index"),
                door.transform(),
            ));
        }
        for destructible in self
            .destructible_targets
            .iter()
            .filter(|target| destructibles.alive(target.destructible_index()))
        {
            output.push(PxbspCollisionModel::new(
                u16::try_from(destructible.model_index())
                    .expect("validated PXBSP destructible model index"),
                destructible.transform(),
            ));
        }
    }
}

fn bsp_fragment_quad(
    horizontal_axis: usize,
    plane_depth: i32,
    horizontal: [i32; 2],
    vertical: [i32; 2],
    floor_y: i32,
    age: u8,
    row: usize,
    column: usize,
) -> [RoomPoint; 4] {
    let raw_motion_age =
        (i32::from(age.saturating_sub(1)) / 2).min(BSP_DESTRUCTIBLE_FRAGMENT_MOTION_TICKS);
    let settled = age >= BSP_DESTRUCTIBLE_FRAGMENT_SETTLE_TICKS;
    let center_h = horizontal[0].saturating_add(horizontal[1]) / 2;
    let base_center_y = vertical[0].saturating_add(vertical[1]) / 2;
    let piece_index = (row * BSP_DESTRUCTIBLE_FRAGMENT_COLUMNS + column) as i32;
    let scatter = (piece_index * 73 + row as i32 * 19 + column as i32 * 37 + 11) & 0xff;
    let launch_delay = scatter % 5;
    let motion_age = raw_motion_age.saturating_sub(launch_delay).max(0);
    let spread_sign = if column < BSP_DESTRUCTIBLE_FRAGMENT_COLUMNS / 2 {
        -1
    } else {
        1
    };
    let depth_sign = if scatter & 1 == 0 { -1 } else { 1 };
    // This is a crumbling lattice, not a grenade: keep pieces close to the
    // authored plane, while giving every shard a distinct launch vector.
    let outward_speed = 3 + ((scatter >> 3) % 5);
    let sideways_jitter = (scatter % 5) - 2;
    let travel_h = spread_sign
        * (motion_age.saturating_mul(motion_age) / 128
            + motion_age.saturating_mul(outward_speed) / 8)
        + sideways_jitter.saturating_mul(motion_age) / 12;
    let depth_speed = 1 + ((scatter >> 5) % 5);
    let travel_depth = depth_sign * motion_age.saturating_mul(depth_speed) / 6;
    let center_y = if settled {
        floor_y.saturating_add(3)
    } else {
        let launch = 8 + ((scatter >> 2) % 9);
        let airborne_y = base_center_y
            .saturating_add(motion_age.saturating_mul(launch))
            .saturating_sub(motion_age.saturating_mul(motion_age) / 3);
        let half_height = vertical[1].saturating_sub(vertical[0]).unsigned_abs() as i32 / 2;
        airborne_y.max(floor_y.saturating_add(half_height.max(3)))
    };
    let pitch = if settled {
        1024
    } else {
        let pitch_target = 768 + ((scatter >> 4) % 5) * 192;
        (motion_age.saturating_mul(pitch_target) / BSP_DESTRUCTIBLE_FRAGMENT_MOTION_TICKS) as u16
    };
    let yaw_direction = if scatter & 2 == 0 { -1 } else { 1 };
    let yaw_speed = 19 + (scatter % 41);
    let yaw = ((motion_age.saturating_mul(yaw_speed) * yaw_direction) & 0x0fff) as u16;
    let pitch_sin = sin_q12(pitch);
    let pitch_cos = cos_q12(pitch);
    let yaw_sin = sin_q12(yaw);
    let yaw_cos = cos_q12(yaw);
    let shrink_q8 = if motion_age == 0 { 256 } else { 232 };
    let base = [
        (horizontal[0], vertical[1]),
        (horizontal[1], vertical[1]),
        (horizontal[1], vertical[0]),
        (horizontal[0], vertical[0]),
    ];
    let mut quad = [RoomPoint::new(0, 0, 0); 4];
    for (index, (h, y)) in base.into_iter().enumerate() {
        let dh = h.saturating_sub(center_h).saturating_mul(shrink_q8) / 256;
        let dy = y.saturating_sub(base_center_y).saturating_mul(shrink_q8) / 256;
        let (mut dx, mut ry, mut dz) = if horizontal_axis == 0 {
            (
                dh,
                (dy.saturating_mul(pitch_cos)) >> 12,
                (dy.saturating_mul(pitch_sin)) >> 12,
            )
        } else {
            (
                -((dy.saturating_mul(pitch_sin)) >> 12),
                (dy.saturating_mul(pitch_cos)) >> 12,
                dh,
            )
        };
        let rotated_x = ((dx.saturating_mul(yaw_cos)) + (dz.saturating_mul(yaw_sin))) >> 12;
        let rotated_z = ((dz.saturating_mul(yaw_cos)) - (dx.saturating_mul(yaw_sin))) >> 12;
        dx = rotated_x;
        dz = rotated_z;
        ry = ry.saturating_add(center_y);
        let (x, z) = if horizontal_axis == 0 {
            (
                center_h.saturating_add(travel_h).saturating_add(dx),
                plane_depth.saturating_add(travel_depth).saturating_add(dz),
            )
        } else {
            (
                plane_depth.saturating_add(travel_depth).saturating_add(dx),
                center_h.saturating_add(travel_h).saturating_add(dz),
            )
        };
        quad[index] = RoomPoint::new(x, ry, z);
    }
    let lowest = quad.iter().map(|vertex| vertex.y).min().unwrap_or(floor_y);
    if lowest < floor_y {
        let correction = floor_y.saturating_sub(lowest);
        for vertex in &mut quad {
            vertex.y = vertex.y.saturating_add(correction);
        }
    }
    quad
}

/// The world pass view rotation from the camera's exact Q12 trig, in the
/// PXBSP convention [`pxbsp_camera`] derives its angles in: PXBSP yaw is
/// the orbit yaw plus a quarter turn and PXBSP pitch the negated look
/// pitch, so `sin(yaw + 90) = cos(yaw)`, `cos(yaw + 90) = -sin(yaw)`,
/// `sin(-pitch) = -sin(pitch)`. No table, no angle recovery: the world
/// rotates by exactly the camera the model pass renders with.
pub(super) fn pxbsp_view_rotation(camera: WorldCamera) -> Mat3I16 {
    let q12 = |value: i32| value.clamp(-0x1000, 0x1000) as i16;
    psx_bsp::render::pxbsp_view_rotation(
        q12(-camera.sin_pitch.raw()),
        q12(camera.cos_pitch.raw()),
        q12(camera.cos_yaw.raw()),
        q12(-camera.sin_yaw.raw()),
    )
}

pub(super) fn pxbsp_camera(camera: WorldCamera) -> Camera {
    let orbit_yaw = angle_q12_from_basis(camera.sin_yaw.raw(), camera.cos_yaw.raw());
    // WorldCamera stores the target-to-camera orbit angle with the engine's
    // `x = sin(yaw), z = cos(yaw)` convention; the view direction is the
    // opposite. PXBSP zero yaw looks along +X and its yaw turns the same way
    // as the engine's now that `load_pxbsp_view` is a proper rotation (the
    // old `3072 - yaw` compensated its mirrored remap).
    let yaw = orbit_yaw.wrapping_add(1024) & 0x0fff;
    let look_pitch = signed_quarter_angle_q12(camera.sin_pitch.raw(), camera.cos_pitch.raw());
    let pitch = (-look_pitch) as u16 & 0x0fff;
    Camera {
        origin: Vec3I32 {
            x: camera.position.x.saturating_mul(4096),
            y: camera.position.y.saturating_mul(4096),
            z: camera.position.z.saturating_mul(4096),
        },
        angles: [pitch as i16, yaw as i16, 0],
    }
}

fn angle_q12_from_basis(sin: i32, cos: i32) -> u16 {
    if sin == 0 && cos == 0 {
        return 0;
    }
    let ax = sin.saturating_abs();
    let az = cos.saturating_abs();
    let base = if ax <= az {
        ax.saturating_mul(512) / az.max(1)
    } else {
        1024 - az.saturating_mul(512) / ax.max(1)
    };
    let angle = if cos >= 0 {
        if sin >= 0 {
            base
        } else {
            4096 - base
        }
    } else if sin >= 0 {
        2048 - base
    } else {
        2048 + base
    };
    (angle & 0x0fff) as u16
}

fn signed_quarter_angle_q12(sin: i32, cos: i32) -> i32 {
    let sin_abs = sin.saturating_abs();
    let cos_abs = cos.saturating_abs();
    let angle = if sin_abs <= cos_abs {
        sin_abs.saturating_mul(512) / cos_abs.max(1)
    } else {
        1024 - cos_abs.saturating_mul(512) / sin_abs.max(1)
    };
    if sin < 0 {
        -angle
    } else {
        angle
    }
}

/// Is this bounding sphere entirely outside the view frustum?
///
/// `WorldProjection::project_view` maps a camera-space vertex to
/// `screen_x + x * focal / z`, `screen_y - y * focal / z`, so the visible rect
/// is bounded by the four planes `|x| * focal = screen_x * z` and
/// `|y| * focal = screen_y * z` plus the near plane. A sphere entirely outside
/// any one of them contains no point that projects inside the rect, so nothing
/// it could submit can produce a pixel and skipping it is output-identical.
///
/// The exact plane distance divides by `sqrt(focal^2 + half^2)`; this scales
/// the radius by `focal + half` instead, which is never smaller. The threshold
/// is therefore never too tight, so the test can only ever KEEP a sphere the
/// exact test would keep. No square root, five multiplies, no division.
fn sphere_outside_view_frustum(view: ViewVertex, radius: i32, projection: WorldProjection) -> bool {
    if view.z.saturating_add(radius) < projection.near_z {
        return true;
    }
    let focal = projection.focal_length;
    let half_width = i32::from(projection.screen_x);
    let half_height = i32::from(projection.screen_y);
    let x_focal = view.x.saturating_mul(focal);
    let y_focal = view.y.saturating_mul(focal);
    let z_width = half_width.saturating_mul(view.z);
    let z_height = half_height.saturating_mul(view.z);
    let margin_width = radius.saturating_mul(focal.saturating_add(half_width));
    let margin_height = radius.saturating_mul(focal.saturating_add(half_height));
    x_focal.saturating_sub(z_width) > margin_width
        || x_focal.saturating_add(z_width).saturating_neg() > margin_width
        || y_focal.saturating_sub(z_height) > margin_height
        || y_focal.saturating_add(z_height).saturating_neg() > margin_height
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::Q12;

    #[test]
    fn engine_view_cardinals_map_to_pxbsp_yaw() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        // Camera at -X (looks +X) -> pxbsp 0; at -Z (looks +Z) -> 3072;
        // at +X (looks -X) -> 2048; at +Z (looks -Z) -> 1024.
        for (sin_yaw, cos_yaw, expected) in [
            (Q12::NEG_ONE, Q12::ZERO, 0),
            (Q12::ZERO, Q12::NEG_ONE, 3072),
            (Q12::ONE, Q12::ZERO, 2048),
            (Q12::ZERO, Q12::ONE, 1024),
        ] {
            let camera = WorldCamera::from_basis(
                projection,
                psx_engine::WorldVertex::ZERO,
                sin_yaw,
                cos_yaw,
                Q12::ZERO,
                Q12::ONE,
            );
            assert_eq!(pxbsp_camera(camera).angles[1], expected);
        }
    }

    #[test]
    fn engine_view_pitch_keeps_pxbsp_up_and_down_orientation() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        for (sin_pitch, expected) in [(Q12::NEG_ONE, 1024), (Q12::ONE, 3072)] {
            let camera = WorldCamera::from_basis(
                projection,
                psx_engine::WorldVertex::ZERO,
                Q12::NEG_ONE,
                Q12::ZERO,
                sin_pitch,
                Q12::ZERO,
            );
            assert_eq!(pxbsp_camera(camera).angles[0], expected);
        }
    }

    #[test]
    fn brush_fragments_begin_on_the_visible_plane_and_settle_on_the_floor() {
        let initial = bsp_fragment_quad(0, -10368, [-100, 100], [896, 1296], 896, 1, 0, 1);
        assert_eq!(initial[0], RoomPoint::new(-100, 1296, -10368));
        assert_eq!(initial[2], RoomPoint::new(100, 896, -10368));

        let settled = bsp_fragment_quad(
            0,
            -10368,
            [-100, 100],
            [896, 1296],
            896,
            BSP_DESTRUCTIBLE_FRAGMENT_SETTLE_TICKS,
            0,
            1,
        );
        assert!(settled.iter().all(|vertex| vertex.y == 899));
    }

    /// The frustum reject must never drop a sphere that still has a point on
    /// screen. Sweep a grid of centres and radii, project the sphere's own
    /// axis-aligned extremes through the real `project_view`, and assert that
    /// whenever any of them lands inside the draw rect the sphere was kept.
    #[test]
    fn frustum_reject_never_drops_a_sphere_with_a_point_on_screen() {
        let projection = WorldProjection::new(160, 120, 320, 4);
        let width = i32::from(projection.screen_x) * 2;
        let height = i32::from(projection.screen_y) * 2;
        let mut rejected = 0usize;
        let mut kept = 0usize;
        for z in [4, 8, 17, 40, 96, 250, 640, 1600, 4000] {
            for x in (-4000..=4000).step_by(311) {
                for y in (-3000..=3000).step_by(287) {
                    for radius in [0, 1, 9, 64, 300, 1024] {
                        let view = ViewVertex::new(x, y, z);
                        let outside = sphere_outside_view_frustum(view, radius, projection);
                        if !outside {
                            kept += 1;
                            continue;
                        }
                        rejected += 1;
                        // Every axis extreme of the sphere, plus its centre.
                        for probe in [
                            (x, y, z),
                            (x - radius, y, z),
                            (x + radius, y, z),
                            (x, y - radius, z),
                            (x, y + radius, z),
                            (x, y, z - radius),
                            (x, y, z + radius),
                        ] {
                            let Some(point) =
                                projection.project_view(ViewVertex::new(probe.0, probe.1, probe.2))
                            else {
                                continue;
                            };
                            let on_screen = i32::from(point.sx) >= 0
                                && i32::from(point.sx) < width
                                && i32::from(point.sy) >= 0
                                && i32::from(point.sy) < height;
                            assert!(
                                !on_screen,
                                "rejected sphere centre ({x},{y},{z}) r={radius} still \
                                 projects {probe:?} to ({}, {})",
                                point.sx, point.sy
                            );
                        }
                    }
                }
            }
        }
        // The sweep has to exercise both outcomes or it proves nothing.
        assert!(
            rejected > 1000 && kept > 1000,
            "rejected={rejected} kept={kept}"
        );
    }

    /// Stronger than the axis-extreme probe above: sample the whole surface
    /// AND interior of the sphere on a lattice, and assert no sample of a
    /// REJECTED sphere lands on screen. The seven axis extremes can miss a
    /// sphere whose nearest on-screen point is off-axis.
    #[test]
    fn frustum_reject_survives_a_dense_sphere_lattice() {
        let projection = WorldProjection::new(160, 120, 320, 4);
        let width = i32::from(projection.screen_x) * 2;
        let height = i32::from(projection.screen_y) * 2;
        let mut checked = 0u64;
        for z in [5, 13, 61, 200, 900, 3000] {
            for x in (-3000..=3000).step_by(197) {
                for y in (-2200..=2200).step_by(173) {
                    for radius in [7, 55, 260, 900] {
                        if !sphere_outside_view_frustum(
                            ViewVertex::new(x, y, z),
                            radius,
                            projection,
                        ) {
                            continue;
                        }
                        let step = (radius / 4).max(1);
                        let mut dx = -radius;
                        while dx <= radius {
                            let mut dy = -radius;
                            while dy <= radius {
                                let mut dz = -radius;
                                while dz <= radius {
                                    if dx * dx + dy * dy + dz * dz <= radius * radius {
                                        checked += 1;
                                        if let Some(point) = projection
                                            .project_view(ViewVertex::new(x + dx, y + dy, z + dz))
                                        {
                                            assert!(
                                                !(i32::from(point.sx) >= 0
                                                    && i32::from(point.sx) < width
                                                    && i32::from(point.sy) >= 0
                                                    && i32::from(point.sy) < height),
                                                "rejected sphere ({x},{y},{z}) r={radius} has an \
                                                 on-screen point at (+{dx},+{dy},+{dz})"
                                            );
                                        }
                                    }
                                    dz += step;
                                }
                                dy += step;
                            }
                            dx += step;
                        }
                    }
                }
            }
        }
        // Guards the sweep itself: a rejection test that stopped rejecting
        // would make every assert above vacuous and still pass.
        assert!(checked > 100_000, "lattice samples checked: {checked}");
    }

    /// A sphere straddling the screen centre is never rejected, at any depth
    /// from the near plane out to the far end of the map.
    #[test]
    fn frustum_reject_keeps_the_sphere_under_the_crosshair() {
        let projection = WorldProjection::new(160, 120, 320, 4);
        for z in [4, 5, 16, 64, 512, 4096] {
            for radius in [0, 1, 256, 4096] {
                assert!(!sphere_outside_view_frustum(
                    ViewVertex::new(0, 0, z),
                    radius,
                    projection
                ));
            }
        }
    }
}
