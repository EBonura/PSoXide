//! Normal-playtest ownership of one cooked resident PXBSP world.
//!
//! The editor manifest chooses this backend explicitly. Grid projects never
//! construct it; a malformed BSP manifest fails during scene initialization
//! rather than silently falling back to the synthetic grid room.

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use psx_bsp::collision::{BrushTransform, LiquidContentsSample, TraceScratch};
use psx_bsp::collision_provider::{
    select_body_hull, valid_pxbsp_body_hulls, PxbspCollisionModel, PxbspCollisionProvider,
};
use psx_bsp::mover::{BrushDoorSet, BrushDoorSetError};
use psx_bsp::pxbsp::PXBSP_MAX_VISIBILITY_BYTES;
use psx_bsp::pxbsp_resident::{PxbspMapLoadError, PxbspResidentMap};
use psx_bsp::render::{load_pxbsp_view, Camera, PxbspTextureBinding, Renderer};
use psx_bsp::{SliceReadError, Vec3I32};
use psx_engine::{
    commit_body_step_with_trace_provider, trace_collision, BodyStep, CharacterBlockerTraceProvider,
    CharacterCollisionAabb, CharacterCollisionCylinder, CharacterMotorConfig, CharacterMotorFrame,
    CharacterMotorInput, CharacterMotorState, CollisionQueryError, CollisionTraceQuery,
    CollisionTraceShape, OtFrame, PrimitivePacketArena, RoomPoint, ThirdPersonCameraConfig,
    ThirdPersonCameraFrame, ThirdPersonCameraInput, ThirdPersonCameraState,
    ThirdPersonCameraTarget, WorldCamera,
};
use psx_level::{find_asset_of_kind, AssetId, AssetKind};

use crate::generated::{
    ASSETS, PXBSP_BODY_HULLS, PXBSP_MOVER_MODEL_INDICES, PXBSP_MOVER_NODE_IDS, PXBSP_WORLD,
};
use crate::{ensure_room_texture_uploaded, find_room_texture_vram_slot, PROJECTION};

pub(super) const MAX_BSP_DOORS: usize = 16;
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
pub(super) const BSP_FALLBACK_CAMERA_DISTANCE: i32 = 192;
pub(super) const BSP_FALLBACK_CAMERA_HEIGHT: i32 = 128;
pub(super) const BSP_FALLBACK_CAMERA_TARGET_HEIGHT: i32 = 64;
pub(super) const BSP_FALLBACK_CAMERA_CLEARANCE: i32 = 16;
pub(super) const BSP_FALLBACK_CAMERA_MARGIN: i32 = 16;
pub(super) const BSP_USE_DISTANCE: i32 = 256;

#[derive(Debug)]
pub(super) enum BspRuntimeInitError {
    EmptyWorld,
    NoMaterials,
    Map(PxbspMapLoadError<SliceReadError>),
    Doors(BrushDoorSetError),
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
            Self::Map(error) => write!(formatter, "PXBSP map load failed: {error:?}"),
            Self::Doors(error) => write!(formatter, "PXBSP mover load failed: {error:?}"),
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
    materials: Vec<Option<PxbspTextureBinding>>,
    trace_scratch: TraceScratch,
    activation_visibility: [u8; PXBSP_MAX_VISIBILITY_BYTES],
    activation_leaf: Option<usize>,
    activation_visible_leaves: usize,
}

impl BspRuntime {
    pub(super) fn load_manifest() -> Result<Self, BspRuntimeInitError> {
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
        let mut doors = BrushDoorSet::EMPTY;
        doors
            .init_from_map(&map)
            .map_err(BspRuntimeInitError::Doors)?;
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
        }

        let material_count = map.materials().len();
        if material_count == 0 {
            return Err(BspRuntimeInitError::NoMaterials);
        }
        let renderer = Renderer::new_pxbsp(map.faces().len());
        Ok(Self {
            map,
            renderer,
            doors,
            materials: vec![None; material_count],
            trace_scratch: TraceScratch::new(),
            activation_visibility: [0; PXBSP_MAX_VISIBILITY_BYTES],
            activation_leaf: None,
            activation_visible_leaves: 0,
        })
    }

    /// Return one bit per world-space point visible from `observer` through
    /// the cooked PXBSP PVS. Invalid/solid points and malformed visibility
    /// fail closed. Positions are engine units; the map lookup consumes Q20.12.
    // psx-numeric-allow-next-line: one bit per queried point; the width IS the caller's point capacity
    pub(super) fn visible_points_mask(&mut self, observer: RoomPoint, points: &[[i32; 3]]) -> u64 {
        let q12 = |value: i32| value.saturating_mul(4096);
        let observer = Vec3I32 {
            x: q12(observer.x),
            y: q12(observer.y),
            z: q12(observer.z),
        };
        let Some(observer_leaf) = self.map.point_leaf_index(observer) else {
            self.activation_leaf = None;
            self.activation_visible_leaves = 0;
            return 0;
        };
        if self.activation_leaf != Some(observer_leaf) {
            let Some(visible_leaves) = self
                .map
                .leaf_visibility_into(observer_leaf, &mut self.activation_visibility)
            else {
                self.activation_leaf = None;
                self.activation_visible_leaves = 0;
                return 0;
            };
            self.activation_leaf = Some(observer_leaf);
            self.activation_visible_leaves = visible_leaves;
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
    pub(super) fn refresh_materials(&mut self) -> bool {
        let mut ready = true;
        for (index, material) in self.map.materials().iter().enumerate() {
            let asset_id = AssetId(material.texture_asset);
            let slot = match find_room_texture_vram_slot(asset_id) {
                Some(slot) => Some(slot),
                None => {
                    let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture)
                        .unwrap_or_else(|| {
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
                    ensure_room_texture_uploaded(asset_id, asset.bytes)
                }
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
                texture_size: [width, height],
            });
        }
        ready && self.materials.iter().all(Option::is_some)
    }

    pub(super) fn materials_ready(&self) -> bool {
        !self.materials.is_empty() && self.materials.iter().all(Option::is_some)
    }

    pub(super) fn tick_doors(&mut self) {
        self.doors.tick();
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
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<CharacterMotorFrame, CollisionQueryError> {
        let mut models = [PxbspCollisionModel::new(0, BrushTransform::IDENTITY); MAX_BSP_DOORS];
        let count = self.collision_models(&mut models);
        let shape = CollisionTraceShape::Body {
            radius: config.radius,
            height: config.height,
        };
        let hull_index = select_body_hull(PXBSP_BODY_HULLS, config.radius, config.height)
            .ok_or(CollisionQueryError)?;
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            hull_index,
            &models[..count],
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
        blockers: &[CharacterCollisionCylinder],
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<BodyStep, CollisionQueryError> {
        let mut models = [PxbspCollisionModel::new(0, BrushTransform::IDENTITY); MAX_BSP_DOORS];
        let count = self.collision_models(&mut models);
        let radius = radius.max(0);
        let height = height.max(1);
        let shape = CollisionTraceShape::Body { radius, height };
        let hull_index =
            select_body_hull(PXBSP_BODY_HULLS, radius, height).ok_or(CollisionQueryError)?;
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            hull_index,
            &models[..count],
            shape,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP entity collision provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, blockers, aabb_blockers);
        commit_body_step_with_trace_provider(&mut provider, start, dx, dz, radius, height)
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
    ) -> bool {
        let mut models = [PxbspCollisionModel::new(0, BrushTransform::IDENTITY); MAX_BSP_DOORS];
        let count = self.collision_models(&mut models);
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            BSP_POINT_HULL_INDEX,
            &models[..count],
            CollisionTraceShape::Point,
            &mut self.trace_scratch,
        )
        .expect("validated PXBSP melee occlusion provider");
        let mut provider =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, &[], aabb_blockers);
        match trace_collision(&mut provider, CollisionTraceQuery::point(from, to)) {
            Ok(trace) => !trace.hit(),
            Err(_) => false,
        }
    }

    pub(super) fn update_camera(
        &mut self,
        camera: &mut ThirdPersonCameraState,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
        aabb_blockers: &[CharacterCollisionAabb],
    ) -> Result<ThirdPersonCameraFrame, CollisionQueryError> {
        let mut models = [PxbspCollisionModel::new(0, BrushTransform::IDENTITY); MAX_BSP_DOORS];
        let count = self.collision_models(&mut models);
        let mut provider = PxbspCollisionProvider::new(
            &self.map,
            BSP_POINT_HULL_INDEX,
            &models[..count],
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
        primitive_packets: &mut PrimitivePacketArena<'_>,
        ot: &mut OtFrame<'_, DEPTH>,
    ) {
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
        let camera = pxbsp_camera(camera);
        let view = load_pxbsp_view(camera);
        let capacity = primitive_packets.remaining_words();
        let Some(mut reservation) = primitive_packets.reserve_packet_words(capacity) else {
            return;
        };
        let (used_words, packet_count) = {
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
            }
            (used_words, packet_count)
        };
        let stream = reservation
            .commit(used_words, packet_count)
            .expect("PXBSP renderer reported an invalid shared-arena stream");
        unsafe {
            ot.add_committed_tagged_packet_stream_unchecked(stream);
        }
    }

    fn collision_models(&self, output: &mut [PxbspCollisionModel; MAX_BSP_DOORS]) -> usize {
        for (index, door) in self.doors.iter().enumerate() {
            output[index] = PxbspCollisionModel::new(
                u16::try_from(door.model_index()).expect("validated PXBSP mover index"),
                door.transform(),
            );
        }
        self.doors.len()
    }
}

fn pxbsp_camera(camera: WorldCamera) -> Camera {
    let orbit_yaw = angle_q12_from_basis(camera.sin_yaw.raw(), camera.cos_yaw.raw());
    // WorldCamera stores the target-to-camera orbit angle with the engine's
    // `x = sin(yaw), z = cos(yaw)` convention. PXBSP stores the actual view
    // direction with `x = cos(yaw), z = sin(yaw)`, so the axes and handedness
    // both change here. The previous quarter-turn-only conversion happened to
    // work on the Z cardinals while reversing every X-facing camera.
    let yaw = 3072u16.wrapping_sub(orbit_yaw) & 0x0fff;
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

#[cfg(test)]
mod tests {
    use super::*;
    use psx_engine::{WorldProjection, Q12};

    #[test]
    fn engine_view_cardinals_map_to_pxbsp_yaw() {
        let projection = WorldProjection::new(160, 120, 320, 64);
        for (sin_yaw, cos_yaw, expected) in [
            (Q12::NEG_ONE, Q12::ZERO, 0),
            (Q12::ZERO, Q12::NEG_ONE, 1024),
            (Q12::ONE, Q12::ZERO, 2048),
            (Q12::ZERO, Q12::ONE, 3072),
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
}
