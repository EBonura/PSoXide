//! Fixed-budget third-person camera controller.
//!
//! The controller is designed for PS1-scale rooms: no heap, no dynamic
//! dispatch, bounded ray work, integer math, and collision probes that
//! read the cooked grid room through [`RoomCollision`]. It supplies the
//! common action-camera pieces a game wants on top of [`WorldCamera`]:
//! manual orbit cooldown, optional automatic re-alignment, camera lag,
//! lock-on facing, and a spring-arm collision solve that shortens the
//! boom without taking yaw control away from the player.

use crate::{Angle, RoomCollision, RoomPoint, WorldCamera, WorldProjection, Q12};

const RAY_STEPS_MAX: i32 = 8;
const RAY_STEPS_MIN: i32 = 3;
const RAY_NEIGHBORHOOD_CELLS: usize = 9;
const MAX_RAY_CHECKED_CELLS: usize = RAY_STEPS_MAX as usize * RAY_NEIGHBORHOOD_CELLS;
const CHECKED_CAMERA_CELL_BITS: usize = 512;
const CHECKED_CAMERA_CELL_WORDS: usize = CHECKED_CAMERA_CELL_BITS / 32;
const MAX_CAMERA_CATCHUP_VBLANKS: u16 = 4;

// Mirrors psxed_format::world::direction::* without adding a direct
// psxed-format dependency just for byte constants.
const DIR_NORTH: u8 = 0;
const DIR_EAST: u8 = 1;
const DIR_SOUTH: u8 = 2;
const DIR_WEST: u8 = 3;
const DIR_NORTH_WEST_SOUTH_EAST: u8 = 4;
const DIR_NORTH_EAST_SOUTH_WEST: u8 = 5;
const SPLIT_NE_SW: u8 = psx_asset::WORLD_SPLIT_NORTH_EAST_SOUTH_WEST;

/// Tunables for [`ThirdPersonCameraState`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraConfig {
    /// Preferred trailing distance from focus to camera.
    pub distance: i32,
    /// Closest the collision solver may pull the camera.
    pub min_distance: i32,
    /// Furthest distance the camera may ease back out to.
    pub max_distance: i32,
    /// Vertical camera offset above the player origin.
    pub height: i32,
    /// Vertical look-at offset above the player origin.
    pub target_height: i32,
    /// Minimum camera origin height above the sampled floor.
    pub min_floor_clearance: i32,
    /// Extra clearance kept between the camera ray and blocking geometry.
    pub collision_margin: i32,
    /// Lowest manual pitch, in signed Q0.12 turn units.
    pub pitch_min_q12: i16,
    /// Highest manual pitch, in signed Q0.12 turn units.
    pub pitch_max_q12: i16,
    /// Display frames before auto-alignment resumes after manual camera input.
    pub manual_cooldown_frames: u8,
    /// Maximum auto-align yaw movement per display frame.
    pub auto_align_step: Angle,
    /// When true, ease the unlocked camera behind player yaw while moving.
    pub auto_align_when_moving: bool,
    /// Maximum lock-on yaw movement per display frame.
    pub lock_on_align_step: Angle,
    /// Position lag strength as a power-of-two divisor.
    pub position_lag_shift: u8,
    /// Focus lag strength as a power-of-two divisor.
    pub focus_lag_shift: u8,
    /// Ease-out strength when collision lets the camera extend again.
    pub distance_lag_shift: u8,
    /// Display frames to hold the shortened boom before easing out.
    pub collision_release_delay_frames: u8,
}

impl ThirdPersonCameraConfig {
    /// Build a camera config from the authored Character camera fields.
    pub const fn character(distance: i32, height: i32, target_height: i32) -> Self {
        Self {
            distance,
            min_distance: 384,
            max_distance: distance,
            height,
            target_height,
            min_floor_clearance: 0,
            collision_margin: 160,
            pitch_min_q12: -192,
            pitch_max_q12: 704,
            manual_cooldown_frames: 42,
            auto_align_step: Angle::from_q12(18),
            auto_align_when_moving: false,
            lock_on_align_step: Angle::from_q12(128),
            position_lag_shift: 2,
            focus_lag_shift: 2,
            distance_lag_shift: 3,
            collision_release_delay_frames: 4,
        }
    }
}

/// Per-display-frame camera input.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThirdPersonCameraInput {
    /// Signed manual yaw delta in Q0.12 angle units.
    pub yaw_delta_q12: i16,
    /// Signed manual pitch delta in Q0.12 angle units.
    /// Positive raises the camera above the focus point.
    pub pitch_delta_q12: i16,
    /// When true, force the camera to begin easing back behind the player.
    pub recenter: bool,
}

/// Player and optional lock-on target data consumed by the camera.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraTarget {
    /// Player/root position in room-local world units.
    pub player: RoomPoint,
    /// Player facing yaw.
    pub player_yaw: Angle,
    /// True while the player is intentionally moving.
    pub moving: bool,
    /// Optional lock-on target position in room-local world units.
    pub lock_target: Option<RoomPoint>,
}

/// Camera solve result for the current frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraFrame {
    /// Render camera ready for world/model draw calls.
    pub camera: WorldCamera,
    /// Lagged focus point used by the camera.
    pub focus: RoomPoint,
    /// Camera orbit yaw.
    pub yaw: Angle,
    /// Camera pitch, signed Q0.12 turn units.
    pub pitch_q12: i16,
    /// Current camera distance after collision.
    pub distance: i32,
    /// True when the camera was shortened by collision this frame.
    pub collision_pull_in: bool,
    /// Reserved for older debug overlays; spring-arm collision no
    /// longer steers yaw, so this is currently always false.
    pub collision_rotated: bool,
}

/// Runtime state for the third-person camera.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThirdPersonCameraState {
    yaw: Angle,
    pitch_q12: i16,
    distance: i32,
    position: RoomPoint,
    focus: RoomPoint,
    manual_cooldown: u8,
    collision_release_delay: u8,
    initialized: bool,
    last_pull_in: bool,
    last_rotated: bool,
}

impl ThirdPersonCameraState {
    /// Create a camera state with an initial orbit yaw.
    pub const fn new(yaw: Angle) -> Self {
        Self {
            yaw,
            pitch_q12: 0,
            distance: 0,
            position: RoomPoint::ZERO,
            focus: RoomPoint::ZERO,
            manual_cooldown: 0,
            collision_release_delay: 0,
            initialized: false,
            last_pull_in: false,
            last_rotated: false,
        }
    }

    /// Reset the camera immediately behind a player position.
    pub fn snap_to_player(
        &mut self,
        target: ThirdPersonCameraTarget,
        config: ThirdPersonCameraConfig,
    ) {
        self.snap_to_player_with_yaw(target, config, target.player_yaw.add(Angle::HALF));
    }

    /// Reset the camera around a player position using an explicit
    /// orbit yaw. Useful for editor/playtest starts where the
    /// authored player yaw should affect the model facing without
    /// the camera immediately hiding that rotation by moving behind
    /// the player.
    pub fn snap_to_player_with_yaw(
        &mut self,
        target: ThirdPersonCameraTarget,
        config: ThirdPersonCameraConfig,
        yaw: Angle,
    ) {
        let config = normalize_config(config);
        self.yaw = yaw;
        self.distance = config
            .distance
            .clamp(config.min_distance, config.max_distance);
        self.pitch_q12 = default_pitch_q12(config);
        self.focus = player_focus(target.player, config.target_height);
        self.position = camera_position(self.focus, self.distance, self.yaw, self.pitch_q12);
        self.manual_cooldown = 0;
        self.collision_release_delay = 0;
        self.initialized = true;
        self.last_pull_in = false;
        self.last_rotated = false;
    }

    /// Advance the controller by one display tick and build a render camera.
    pub fn update(
        &mut self,
        projection: WorldProjection,
        collision: Option<RoomCollision<'_, '_>>,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
    ) -> ThirdPersonCameraFrame {
        let config = normalize_config(config);
        self.advance_one_vblank(collision, target, input, config);
        self.current_frame(projection)
    }

    /// Advance the controller by elapsed display ticks and build a render camera.
    ///
    /// Heavy render paths can miss VBlanks. The camera catches up
    /// with bounded fixed substeps so yaw limits, cooldowns, easing,
    /// and collision recovery keep their authored display-time speed.
    pub fn update_vblanks(
        &mut self,
        projection: WorldProjection,
        collision: Option<RoomCollision<'_, '_>>,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
        delta_vblanks: u16,
    ) -> ThirdPersonCameraFrame {
        let steps = delta_vblanks.max(1).min(MAX_CAMERA_CATCHUP_VBLANKS);
        let config = normalize_config(config);
        let mut i = 0;
        while i < steps {
            self.advance_one_vblank(collision, target, input, config);
            i += 1;
        }
        self.current_frame(projection)
    }

    fn advance_one_vblank(
        &mut self,
        collision: Option<RoomCollision<'_, '_>>,
        target: ThirdPersonCameraTarget,
        input: ThirdPersonCameraInput,
        config: ThirdPersonCameraConfig,
    ) {
        if !self.initialized {
            self.snap_to_player(target, config);
        }

        let focus_goal = camera_focus_goal(target, config);

        if input.yaw_delta_q12 != 0 || input.pitch_delta_q12 != 0 {
            self.yaw = self.yaw.add_signed_q12(input.yaw_delta_q12);
            self.pitch_q12 = self
                .pitch_q12
                .saturating_add(input.pitch_delta_q12)
                .clamp(config.pitch_min_q12, config.pitch_max_q12);
            self.manual_cooldown = config.manual_cooldown_frames;
        } else if self.manual_cooldown != 0 {
            self.manual_cooldown -= 1;
        }

        let player_back_yaw = target.player_yaw.add(Angle::HALF);
        let (desired_yaw, yaw_step) = if let Some(lock) = target.lock_target {
            (
                yaw_to_point(target.player, lock).add(Angle::HALF),
                config.lock_on_align_step,
            )
        } else if input.recenter
            || (config.auto_align_when_moving && target.moving && self.manual_cooldown == 0)
        {
            (player_back_yaw, config.auto_align_step)
        } else {
            (self.yaw, config.auto_align_step)
        };
        self.yaw = self.yaw.approach_q12(desired_yaw, yaw_step.as_q12());
        if input.recenter {
            self.pitch_q12 = approach_i16(
                self.pitch_q12,
                default_pitch_q12(config),
                config.auto_align_step.as_q12() as i16,
            );
        }

        self.focus = if target.lock_target.is_some() {
            approach_vertex_shift(
                self.focus,
                focus_goal,
                config.focus_lag_shift.saturating_sub(1),
            )
        } else {
            approach_vertex_shift(self.focus, focus_goal, config.focus_lag_shift)
        };

        let collision_solve =
            solve_camera_collision(collision, self.focus, self.yaw, self.pitch_q12, config);

        if collision_solve.distance < self.distance {
            self.distance = collision_solve.distance;
            self.collision_release_delay = config.collision_release_delay_frames;
        } else if self.collision_release_delay != 0 {
            self.collision_release_delay -= 1;
        } else {
            self.distance = approach_i32_shift(
                self.distance,
                collision_solve.distance,
                config.distance_lag_shift,
            );
        }

        let desired_position = clamp_camera_to_floor(
            collision,
            camera_position(self.focus, self.distance, self.yaw, self.pitch_q12),
            config.min_floor_clearance,
        );
        if collision_solve.pull_in {
            self.position = desired_position;
        } else {
            self.position =
                approach_vertex_shift(self.position, desired_position, config.position_lag_shift);
        }
        self.position = clamp_camera_to_floor(collision, self.position, config.min_floor_clearance);

        self.last_pull_in = collision_solve.pull_in;
        self.last_rotated = false;
    }

    fn current_frame(&self, projection: WorldProjection) -> ThirdPersonCameraFrame {
        ThirdPersonCameraFrame {
            camera: camera_from_position_focus(projection, self.position, self.focus),
            focus: self.focus,
            yaw: self.yaw,
            pitch_q12: self.pitch_q12,
            distance: self.distance,
            collision_pull_in: self.last_pull_in,
            collision_rotated: self.last_rotated,
        }
    }

    /// Current orbit yaw.
    pub const fn yaw(&self) -> Angle {
        self.yaw
    }

    /// Current orbit pitch in signed Q0.12 units.
    pub const fn pitch_q12(&self) -> i16 {
        self.pitch_q12
    }

    /// Current camera position.
    pub const fn position(&self) -> RoomPoint {
        self.position
    }

    /// Current lagged focus point.
    pub const fn focus(&self) -> RoomPoint {
        self.focus
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CollisionSolve {
    distance: i32,
    pull_in: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CameraRay {
    from: RoomPoint,
    to: RoomPoint,
    dx: i32,
    dy: i32,
    dz: i32,
    distance: i32,
    sector_size: i32,
    room_width: i32,
    room_depth: i32,
    vertical_margin: i32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CheckedCameraCells {
    bitset: [u32; CHECKED_CAMERA_CELL_WORDS],
    cells: [u32; MAX_RAY_CHECKED_CELLS],
    len: usize,
}

impl CheckedCameraCells {
    const EMPTY_CELL: u32 = u32::MAX;

    const fn new() -> Self {
        Self {
            bitset: [0; CHECKED_CAMERA_CELL_WORDS],
            cells: [Self::EMPTY_CELL; MAX_RAY_CHECKED_CELLS],
            len: 0,
        }
    }

    fn visit(&mut self, key: u32) -> bool {
        let word = (key / 32) as usize;
        if word < self.bitset.len() {
            let mask = 1u32 << (key & 31);
            if self.bitset[word] & mask != 0 {
                return false;
            }
            self.bitset[word] |= mask;
            return true;
        }

        let mut i = 0;
        while i < self.len {
            if self.cells[i] == key {
                return false;
            }
            i += 1;
        }
        if self.len < self.cells.len() {
            self.cells[self.len] = key;
            self.len += 1;
        }
        true
    }
}

fn normalize_config(mut config: ThirdPersonCameraConfig) -> ThirdPersonCameraConfig {
    config.min_distance = config.min_distance.max(128);
    config.max_distance = config.max_distance.max(config.min_distance);
    config.distance = config
        .distance
        .clamp(config.min_distance, config.max_distance);
    config.collision_margin = config.collision_margin.max(0);
    config.min_floor_clearance = config.min_floor_clearance.max(0);
    if config.pitch_min_q12 > config.pitch_max_q12 {
        let pitch = config.pitch_min_q12;
        config.pitch_min_q12 = config.pitch_max_q12;
        config.pitch_max_q12 = pitch;
    }
    if config.auto_align_step == Angle::ZERO {
        config.auto_align_step = Angle::from_q12(1);
    }
    if config.lock_on_align_step == Angle::ZERO {
        config.lock_on_align_step = config.auto_align_step;
    }
    config.position_lag_shift = config.position_lag_shift.min(6);
    config.focus_lag_shift = config.focus_lag_shift.min(6);
    config.distance_lag_shift = config.distance_lag_shift.min(6);
    config
}

fn player_focus(player: RoomPoint, target_height: i32) -> RoomPoint {
    RoomPoint::new(player.x, player.y.saturating_add(target_height), player.z)
}

fn camera_focus_goal(
    target: ThirdPersonCameraTarget,
    config: ThirdPersonCameraConfig,
) -> RoomPoint {
    player_focus(target.player, config.target_height)
}

fn clamp_camera_to_floor(
    collision: Option<RoomCollision<'_, '_>>,
    position: RoomPoint,
    min_floor_clearance: i32,
) -> RoomPoint {
    let Some(room) = collision else {
        return position;
    };
    if min_floor_clearance <= 0 {
        return position;
    }
    let Some(floor_y) = floor_height_at(room, position.x, position.z) else {
        return position;
    };
    let min_y = floor_y.saturating_add(min_floor_clearance);
    if position.y < min_y {
        RoomPoint::new(position.x, min_y, position.z)
    } else {
        position
    }
}

fn floor_height_at(room: RoomCollision<'_, '_>, x: i32, z: i32) -> Option<i32> {
    let s = room.sector_size();
    if s <= 0 || x < 0 || z < 0 {
        return None;
    }
    let sx = x / s;
    let sz = z / s;
    if sx < 0 || sz < 0 || sx >= room.width() as i32 || sz >= room.depth() as i32 {
        return None;
    }
    let sector = room.sector(sx as u16, sz as u16)?;
    if !sector.has_floor() {
        return None;
    }
    let local_x = (x - sx * s).clamp(0, s);
    let local_z = (z - sz * s).clamp(0, s);
    let triangle = psx_asset::world_topology::horizontal_triangle_at_local(
        sector.floor_split(),
        local_x,
        local_z,
        s,
    );
    if !sector.floor_triangle_present(triangle) {
        return None;
    }
    let heights = triangle_heights_to_quad(
        sector.floor_heights(),
        sector.floor_split(),
        triangle,
        sector.floor_triangle_heights(triangle),
    );
    Some(height_at_local(
        heights,
        sector.floor_split(),
        local_x,
        local_z,
        s,
    ))
}

fn triangle_heights_to_quad(
    mut fallback: [i32; 4],
    split: u8,
    triangle: usize,
    heights: [i32; 3],
) -> [i32; 4] {
    let corners = psx_asset::world_topology::split_triangles(split)[triangle.min(1)];
    fallback[corners[0]] = heights[0];
    fallback[corners[1]] = heights[1];
    fallback[corners[2]] = heights[2];
    fallback
}

fn height_at_local(heights: [i32; 4], split: u8, local_x: i32, local_z: i32, sector: i32) -> i32 {
    let u = local_x.clamp(0, sector);
    let v = local_z.clamp(0, sector);
    let [nw, ne, se, sw] = heights;
    if split == SPLIT_NE_SW {
        if u + v <= sector {
            nw.saturating_add(mul_sector(ne.saturating_sub(nw), u, sector))
                .saturating_add(mul_sector(sw.saturating_sub(nw), v, sector))
        } else {
            sw.saturating_add(mul_sector(se.saturating_sub(sw), u, sector))
                .saturating_add(mul_sector(ne.saturating_sub(se), sector - v, sector))
        }
    } else if v <= u {
        nw.saturating_add(mul_sector(ne.saturating_sub(nw), u - v, sector))
            .saturating_add(mul_sector(se.saturating_sub(nw), v, sector))
    } else {
        nw.saturating_add(mul_sector(se.saturating_sub(sw), u, sector))
            .saturating_add(mul_sector(sw.saturating_sub(nw), v, sector))
    }
}

fn mul_sector(delta: i32, amount: i32, sector: i32) -> i32 {
    if sector <= 0 {
        0
    } else {
        let num = (delta as i64).saturating_mul(amount as i64);
        num.checked_div(sector as i64)
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or_else(|| {
                if num.is_negative() {
                    i32::MIN
                } else {
                    i32::MAX
                }
            })
    }
}

fn solve_camera_collision(
    collision: Option<RoomCollision<'_, '_>>,
    focus: RoomPoint,
    yaw: Angle,
    pitch_q12: i16,
    config: ThirdPersonCameraConfig,
) -> CollisionSolve {
    let Some(room) = collision else {
        return CollisionSolve {
            distance: config.distance,
            pull_in: false,
        };
    };

    let desired = camera_position(focus, config.distance, yaw, pitch_q12);
    let clear = probe_clear_distance(room, focus, desired, config.distance, config);
    let distance = clear.clamp(config.min_distance, config.distance);
    CollisionSolve {
        distance,
        pull_in: distance < config.distance,
    }
}

fn probe_clear_distance(
    room: RoomCollision<'_, '_>,
    from: RoomPoint,
    to: RoomPoint,
    max_distance: i32,
    config: ThirdPersonCameraConfig,
) -> i32 {
    let max_distance = max_distance.max(1);
    let sector = room.sector_size().max(1);
    let ray = CameraRay {
        from,
        to,
        dx: to.x.saturating_sub(from.x),
        dy: to.y.saturating_sub(from.y),
        dz: to.z.saturating_sub(from.z),
        distance: max_distance,
        sector_size: sector,
        room_width: room.width() as i32,
        room_depth: room.depth() as i32,
        vertical_margin: config.collision_margin,
    };
    let mut steps = (max_distance / (sector / 4).max(1)).clamp(RAY_STEPS_MIN, RAY_STEPS_MAX);
    if steps <= 0 {
        steps = RAY_STEPS_MIN;
    }

    let mut nearest = max_distance;
    let mut checked_cells = CheckedCameraCells::new();
    let mut i = 1;
    while i <= steps {
        let sample = lerp_vertex(from, to, i, steps);
        if point_outside_camera_space(room, sample, sector, ray.room_width, ray.room_depth) {
            nearest = ((max_distance * i) / steps).min(nearest);
            break;
        }
        if let Some(hit) = nearest_wall_hit_around(room, sample, ray, &mut checked_cells) {
            nearest = hit.min(nearest);
            break;
        }
        i += 1;
    }

    nearest
        .saturating_sub(config.collision_margin)
        .clamp(config.min_distance, config.distance)
}

fn point_outside_camera_space(
    room: RoomCollision<'_, '_>,
    point: RoomPoint,
    sector_size: i32,
    room_width: i32,
    room_depth: i32,
) -> bool {
    if point.x < 0 || point.z < 0 {
        return true;
    }
    let sx = point.x / sector_size;
    let sz = point.z / sector_size;
    if sx < 0 || sz < 0 || sx >= room_width || sz >= room_depth {
        return true;
    }
    match room.sector_probe(sx as u16, sz as u16) {
        Some(sector) => !sector.has_floor(),
        None => true,
    }
}

fn nearest_wall_hit_around(
    room: RoomCollision<'_, '_>,
    sample: RoomPoint,
    ray: CameraRay,
    checked_cells: &mut CheckedCameraCells,
) -> Option<i32> {
    if sample.x < 0 || sample.z < 0 {
        return None;
    }
    let sx = sample.x / ray.sector_size;
    let sz = sample.z / ray.sector_size;
    let mut nearest: Option<i32> = None;
    let mut ox = -1;
    while ox <= 1 {
        let mut oz = -1;
        while oz <= 1 {
            let cx = sx + ox;
            let cz = sz + oz;
            if cx >= 0 && cz >= 0 && cx < ray.room_width && cz < ray.room_depth {
                let key = (cx as u32)
                    .saturating_mul(ray.room_depth as u32)
                    .saturating_add(cz as u32);
                if !checked_cells.visit(key) {
                    oz += 1;
                    continue;
                }
                if let Some(sector) = room.sector_probe(cx as u16, cz as u16) {
                    let mut i = 0;
                    while i < sector.wall_count() {
                        if let Some(wall) = room.sector_probe_wall(sector, i) {
                            if wall.solid() {
                                if let Some(hit) = segment_wall_hit_distance(
                                    ray,
                                    cx,
                                    cz,
                                    wall.direction(),
                                    wall.heights(),
                                ) {
                                    nearest = Some(match nearest {
                                        Some(prev) => prev.min(hit),
                                        None => hit,
                                    });
                                }
                            }
                        }
                        i += 1;
                    }
                }
            }
            oz += 1;
        }
        ox += 1;
    }
    nearest
}

fn segment_wall_hit_distance(
    ray: CameraRay,
    sx: i32,
    sz: i32,
    direction: u8,
    heights: [i32; 4],
) -> Option<i32> {
    if ray.distance <= 0 {
        return None;
    }
    let sector_size = ray.sector_size;
    let x0 = sx.saturating_mul(sector_size);
    let x1 = x0.saturating_add(sector_size);
    let z0 = sz.saturating_mul(sector_size);
    let z1 = z0.saturating_add(sector_size);
    let diagonal_axis_q12 = match direction {
        DIR_NORTH_WEST_SOUTH_EAST => {
            intersect_segment_q12(ray.from.x, ray.from.z, ray.dx, ray.dz, x0, z0, x1, z1)
        }
        DIR_NORTH_EAST_SOUTH_WEST => {
            intersect_segment_q12(ray.from.x, ray.from.z, ray.dx, ray.dz, x1, z0, x0, z1)
        }
        _ => None,
    };
    let t_q12 = match direction {
        DIR_NORTH => intersect_horizontal_q12(ray.from.z, ray.dz, z0),
        DIR_SOUTH => intersect_horizontal_q12(ray.from.z, ray.dz, z1),
        DIR_EAST => intersect_vertical_q12(ray.from.x, ray.dx, x1),
        DIR_WEST => intersect_vertical_q12(ray.from.x, ray.dx, x0),
        DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => diagonal_axis_q12.map(|(t, _)| t),
        _ => None,
    }?;
    if !(0..=Q12::SCALE).contains(&t_q12) {
        return None;
    }
    let t = Q12::from_raw(t_q12);
    let x_at = ray.from.x.saturating_add(t.mul_i32(ray.dx));
    let y_at = ray.from.y.saturating_add(t.mul_i32(ray.dy));
    let z_at = ray.from.z.saturating_add(t.mul_i32(ray.dz));
    let wall_axis_q12 = match direction {
        DIR_NORTH | DIR_SOUTH => {
            if x_at < x0 || x_at > x1 {
                return None;
            }
            (x_at.saturating_sub(x0))
                .saturating_mul(Q12::SCALE)
                .checked_div(sector_size.max(1))?
        }
        DIR_EAST | DIR_WEST => {
            if z_at < z0 || z_at > z1 {
                return None;
            }
            (z_at.saturating_sub(z0))
                .saturating_mul(Q12::SCALE)
                .checked_div(sector_size.max(1))?
        }
        DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => diagonal_axis_q12?.1,
        _ => return None,
    };
    let axis = Q12::from_raw(wall_axis_q12.clamp(0, Q12::SCALE));
    let (bottom, top) = match direction {
        DIR_NORTH | DIR_EAST | DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => (
            lerp_i32(heights[0], heights[1], axis),
            lerp_i32(heights[3], heights[2], axis),
        ),
        DIR_SOUTH | DIR_WEST => (
            lerp_i32(heights[1], heights[0], axis),
            lerp_i32(heights[2], heights[3], axis),
        ),
        _ => return None,
    };
    let min_y = bottom.min(top).saturating_sub(ray.vertical_margin);
    let max_y = bottom.max(top).saturating_add(ray.vertical_margin);
    if y_at < min_y || y_at > max_y {
        return None;
    }
    Some(t.mul_i32(ray.distance))
}

fn intersect_segment_q12(
    from_x: i32,
    from_z: i32,
    dx: i32,
    dz: i32,
    ax: i32,
    az: i32,
    bx: i32,
    bz: i32,
) -> Option<(i32, i32)> {
    let sx = bx.saturating_sub(ax);
    let sz = bz.saturating_sub(az);
    let qx = ax.saturating_sub(from_x);
    let qz = az.saturating_sub(from_z);
    let denom = cross_i64(dx, dz, sx, sz);
    if denom == 0 {
        return None;
    }
    let t_num = cross_i64(qx, qz, sx, sz);
    let u_num = cross_i64(qx, qz, dx, dz);
    let t_q12 = div_q12_signed(t_num, denom)?;
    let u_q12 = div_q12_signed(u_num, denom)?;
    if !(0..=Q12::SCALE).contains(&t_q12) || !(0..=Q12::SCALE).contains(&u_q12) {
        return None;
    }
    Some((t_q12, u_q12))
}

fn cross_i64(ax: i32, az: i32, bx: i32, bz: i32) -> i64 {
    (ax as i64)
        .saturating_mul(bz as i64)
        .saturating_sub((az as i64).saturating_mul(bx as i64))
}

fn div_q12_signed(num: i64, denom: i64) -> Option<i32> {
    if denom == 0 {
        return None;
    }
    num.saturating_mul(Q12::SCALE as i64)
        .checked_div(denom)
        .and_then(|v| i32::try_from(v).ok())
}

fn intersect_horizontal_q12(from_z: i32, dz: i32, wall_z: i32) -> Option<i32> {
    if dz == 0 {
        return None;
    }
    let delta = wall_z.saturating_sub(from_z);
    if !delta_within_segment(delta, dz) {
        return None;
    }
    delta.saturating_mul(Q12::SCALE).checked_div(dz)
}

fn intersect_vertical_q12(from_x: i32, dx: i32, wall_x: i32) -> Option<i32> {
    if dx == 0 {
        return None;
    }
    let delta = wall_x.saturating_sub(from_x);
    if !delta_within_segment(delta, dx) {
        return None;
    }
    delta.saturating_mul(Q12::SCALE).checked_div(dx)
}

fn delta_within_segment(delta: i32, axis_delta: i32) -> bool {
    if axis_delta > 0 {
        delta >= 0 && delta <= axis_delta
    } else {
        delta <= 0 && delta >= axis_delta
    }
}

fn camera_position(focus: RoomPoint, distance: i32, yaw: Angle, pitch_q12: i16) -> RoomPoint {
    let sin_yaw = yaw.sin();
    let cos_yaw = yaw.cos();
    let pitch = signed_q12_angle(pitch_q12);
    let sin_pitch = pitch.sin();
    let cos_pitch = pitch.cos();
    let horizontal = cos_pitch.mul_i32(distance);
    RoomPoint::new(
        focus.x.saturating_add(sin_yaw.mul_i32(horizontal)),
        focus.y.saturating_add(sin_pitch.mul_i32(distance)),
        focus.z.saturating_add(cos_yaw.mul_i32(horizontal)),
    )
}

fn camera_from_position_focus(
    projection: WorldProjection,
    position: RoomPoint,
    focus: RoomPoint,
) -> WorldCamera {
    let dx = position.x.saturating_sub(focus.x);
    let dz = position.z.saturating_sub(focus.z);
    let radius = isqrt_i32(dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz))).max(1);
    let target_dy = focus.y.saturating_sub(position.y);
    let pitch_len = isqrt_i32(
        radius
            .saturating_mul(radius)
            .saturating_add(target_dy.saturating_mul(target_dy)),
    )
    .max(1);
    WorldCamera {
        position: position.to_world_vertex(),
        projection,
        sin_yaw: Q12::from_ratio(dx, radius),
        cos_yaw: Q12::from_ratio(dz, radius),
        sin_pitch: Q12::from_ratio(target_dy, pitch_len),
        cos_pitch: Q12::from_ratio(radius, pitch_len),
    }
}

fn default_pitch_q12(config: ThirdPersonCameraConfig) -> i16 {
    pitch_from_vertical_distance(
        config.height.saturating_sub(config.target_height),
        config.distance,
    )
    .clamp(config.pitch_min_q12, config.pitch_max_q12)
}

fn pitch_from_vertical_distance(vertical: i32, horizontal: i32) -> i16 {
    if vertical == 0 {
        return 0;
    }
    let ay = abs_i32(vertical);
    let ax = abs_i32(horizontal).max(1);
    let base = if ay <= ax {
        ay.saturating_mul(512) / ax
    } else {
        1024 - (ax.saturating_mul(512) / ay.max(1))
    }
    .min(1024);
    let signed = if vertical < 0 { -base } else { base };
    signed.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn signed_q12_angle(q12: i16) -> Angle {
    Angle::from_q12(((q12 as i32) & 0x0FFF) as u16)
}

fn lerp_i32(a: i32, b: i32, t: Q12) -> i32 {
    a.saturating_add(t.mul_i32(b.saturating_sub(a)))
}

fn yaw_to_point(from: RoomPoint, to: RoomPoint) -> Angle {
    let dx = to.x.saturating_sub(from.x);
    let dz = to.z.saturating_sub(from.z);
    if dx == 0 && dz == 0 {
        return Angle::ZERO;
    }
    let ax = abs_i32(dx);
    let az = abs_i32(dz);
    let base = if ax <= az {
        ax.saturating_mul(512) / az.max(1)
    } else {
        1024 - (az.saturating_mul(512) / ax.max(1))
    };
    let angle = if dz >= 0 {
        if dx >= 0 {
            base
        } else {
            4096 - base
        }
    } else if dx >= 0 {
        2048 - base
    } else {
        2048 + base
    };
    Angle::from_q12((angle & 0x0FFF) as u16)
}

fn approach_i16(current: i16, target: i16, step: i16) -> i16 {
    let step = step.max(1);
    let delta = target.saturating_sub(current);
    if abs_i16(delta) <= step {
        target
    } else if delta > 0 {
        current.saturating_add(step)
    } else {
        current.saturating_sub(step)
    }
}

fn approach_i32_shift(current: i32, target: i32, shift: u8) -> i32 {
    if current == target {
        return current;
    }
    let shift = shift.min(6);
    let delta = target.saturating_sub(current);
    let step = if shift == 0 { delta } else { delta >> shift };
    if step == 0 {
        current.saturating_add(delta.signum())
    } else {
        current.saturating_add(step)
    }
}

fn approach_vertex_shift(current: RoomPoint, target: RoomPoint, shift: u8) -> RoomPoint {
    RoomPoint::new(
        approach_i32_shift(current.x, target.x, shift),
        approach_i32_shift(current.y, target.y, shift),
        approach_i32_shift(current.z, target.z, shift),
    )
}

fn lerp_vertex(from: RoomPoint, to: RoomPoint, num: i32, den: i32) -> RoomPoint {
    RoomPoint::new(
        from.x + ((to.x - from.x) * num) / den,
        from.y + ((to.y - from.y) * num) / den,
        from.z + ((to.z - from.z) * num) / den,
    )
}

fn isqrt_i32(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let mut bit = 1 << 30;
    let mut rest = n;
    let mut root = 0;
    while bit > rest {
        bit >>= 2;
    }
    while bit != 0 {
        if rest >= root + bit {
            rest -= root + bit;
            root = (root >> 1) + bit;
        } else {
            root >>= 1;
        }
        bit >>= 2;
    }
    root
}

fn abs_i16(value: i16) -> i16 {
    if value == i16::MIN {
        i16::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

fn abs_i32(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else {
        value.abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeRoom;

    fn test_ray(
        from: RoomPoint,
        to: RoomPoint,
        distance: i32,
        sector_size: i32,
        vertical_margin: i32,
    ) -> CameraRay {
        CameraRay {
            from,
            to,
            dx: to.x.saturating_sub(from.x),
            dy: to.y.saturating_sub(from.y),
            dz: to.z.saturating_sub(from.z),
            distance,
            sector_size,
            room_width: 1,
            room_depth: 1,
            vertical_margin,
        }
    }

    fn flat_floor_world() -> [u8; 92] {
        const ASSET_HEADER: usize = 12;
        const WORLD_HEADER: usize = 20;
        const SECTOR_RECORD: usize = 60;
        const SECTOR0: usize = ASSET_HEADER + WORLD_HEADER;
        let payload_len = (WORLD_HEADER + SECTOR_RECORD) as u32;
        let mut buf = [0u8; 92];
        buf[0..4].copy_from_slice(b"PSXW");
        buf[4..6].copy_from_slice(&3u16.to_le_bytes());
        buf[8..12].copy_from_slice(&payload_len.to_le_bytes());
        buf[12..14].copy_from_slice(&1u16.to_le_bytes());
        buf[14..16].copy_from_slice(&1u16.to_le_bytes());
        buf[16..20].copy_from_slice(&1024i32.to_le_bytes());
        buf[20..22].copy_from_slice(&1u16.to_le_bytes());
        buf[22..24].copy_from_slice(&1u16.to_le_bytes());

        buf[SECTOR0] = 1 | 4;
        buf[SECTOR0 + 4..SECTOR0 + 6].copy_from_slice(&0u16.to_le_bytes());
        buf
    }

    #[test]
    fn yaw_to_point_matches_cardinal_axes() {
        let origin = RoomPoint::ZERO;
        assert_eq!(yaw_to_point(origin, RoomPoint::new(0, 0, 10)), Angle::ZERO);
        assert_eq!(
            yaw_to_point(origin, RoomPoint::new(10, 0, 0)),
            Angle::QUARTER
        );
        assert_eq!(yaw_to_point(origin, RoomPoint::new(0, 0, -10)), Angle::HALF);
        assert_eq!(
            yaw_to_point(origin, RoomPoint::new(-10, 0, 0)),
            Angle::THREE_QUARTER
        );
    }

    #[test]
    fn approach_angle_takes_shortest_wrapping_path() {
        assert_eq!(
            Angle::from_q12(4090).approach_q12(Angle::from_q12(8), 16),
            Angle::from_q12(8)
        );
        assert_eq!(
            Angle::from_q12(20).approach_q12(Angle::from_q12(4000), 16),
            Angle::from_q12(4)
        );
    }

    #[test]
    fn segment_wall_hit_finds_cardinal_crossing() {
        let from = RoomPoint::new(512, 0, 512);
        let to = RoomPoint::new(1536, 0, 512);
        let heights = [-512, -512, 512, 512];
        let ray = test_ray(from, to, 1024, 1024, 0);
        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_EAST, heights),
            Some(512)
        );
        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_NORTH, heights),
            None
        );
    }

    #[test]
    fn segment_wall_hit_finds_diagonal_crossing() {
        let from = RoomPoint::new(512, 0, 0);
        let to = RoomPoint::new(512, 0, 1024);
        let heights = [-512, -512, 512, 512];
        let ray = test_ray(from, to, 1024, 1024, 0);

        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_NORTH_WEST_SOUTH_EAST, heights),
            Some(512)
        );
        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_NORTH_EAST_SOUTH_WEST, heights),
            Some(512)
        );
    }

    #[test]
    fn segment_wall_hit_ignores_camera_ray_above_wall() {
        let from = RoomPoint::new(512, 900, 512);
        let to = RoomPoint::new(1536, 900, 512);
        let heights = [0, 0, 512, 512];
        let ray = test_ray(from, to, 1024, 1024, 0);

        assert_eq!(
            segment_wall_hit_distance(ray, 0, 0, DIR_EAST, heights),
            None
        );
    }

    #[test]
    fn movement_does_not_auto_align_by_default() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: true,
            lock_target: None,
        };
        camera.snap_to_player_with_yaw(target, config, Angle::HALF.add_signed_q12(128));

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
    }

    #[test]
    fn manual_input_sets_cooldown_and_prevents_configured_auto_align() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.auto_align_when_moving = true;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: true,
            lock_target: None,
        };
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 128,
                pitch_delta_q12: 0,
                recenter: false,
            },
            config,
        );
        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
        assert_eq!(frame.pitch_q12, default_pitch_q12(config));
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );
        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
    }

    #[test]
    fn recenter_eases_camera_behind_player_yaw() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        camera.snap_to_player_with_yaw(target, config, Angle::QUARTER);

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 0,
                recenter: true,
            },
            config,
        );

        assert_eq!(frame.yaw, Angle::QUARTER.add(config.auto_align_step));
    }

    #[test]
    fn character_height_offsets_raise_camera_and_focus() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 400);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(128, 32, -64),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        camera.snap_to_player(target, config);

        assert_eq!(camera.focus.y, target.player.y + config.target_height);
        assert!(camera.position.y > camera.focus.y);
        assert_eq!(camera.pitch_q12, default_pitch_q12(config));
    }

    #[test]
    fn camera_floor_clearance_lifts_low_camera_position() {
        let bytes = flat_floor_world();
        let room = RuntimeRoom::from_bytes(&bytes).expect("test room parses");
        let mut camera = ThirdPersonCameraState::new(Angle::ZERO);
        let mut config = ThirdPersonCameraConfig::character(384, 0, 0);
        config.min_floor_clearance = 64;
        config.pitch_min_q12 = 0;
        config.pitch_max_q12 = 0;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(512, 0, 640),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            Some(room.collision()),
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(frame.camera.position.y, 64);
    }

    #[test]
    fn explicit_start_yaw_does_not_follow_player_yaw() {
        let mut camera = ThirdPersonCameraState::new(Angle::ZERO);
        let config = ThirdPersonCameraConfig::character(1400, 700, 0);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::QUARTER,
            moving: false,
            lock_target: None,
        };

        camera.snap_to_player_with_yaw(target, config, Angle::HALF);

        assert_eq!(camera.yaw(), Angle::HALF);
    }

    #[test]
    fn lock_on_keeps_focus_on_player() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let config = ThirdPersonCameraConfig::character(1400, 700, 400);
        let mut target = ThirdPersonCameraTarget {
            player: RoomPoint::new(128, 32, -64),
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };
        camera.snap_to_player(target, config);

        target.lock_target = Some(RoomPoint::new(4096, 1024, 4096));
        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(
            frame.focus,
            player_focus(target.player, config.target_height)
        );
    }

    #[test]
    fn lock_on_uses_dedicated_fast_yaw_step() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.auto_align_step = Angle::from_q12(18);
        config.lock_on_align_step = Angle::from_q12(128);
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: Some(RoomPoint::new(4096, 0, 0)),
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput::default(),
            config,
        );

        assert_eq!(frame.yaw, Angle::HALF.add_signed_q12(128));
    }

    #[test]
    fn vblank_delta_matches_repeated_camera_updates() {
        let mut stepped = ThirdPersonCameraState::new(Angle::ZERO);
        let mut caught_up = ThirdPersonCameraState::new(Angle::ZERO);
        let projection = WorldProjection::new(160, 120, 320, 64);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.auto_align_step = Angle::from_q12(32);
        config.auto_align_when_moving = true;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::new(1024, 0, 1024),
            player_yaw: Angle::QUARTER,
            moving: true,
            lock_target: None,
        };
        let input = ThirdPersonCameraInput::default();

        stepped.snap_to_player_with_yaw(target, config, Angle::ZERO);
        caught_up.snap_to_player_with_yaw(target, config, Angle::ZERO);
        let _ = stepped.update(projection, None, target, input, config);
        let expected = stepped.update(projection, None, target, input, config);
        let actual = caught_up.update_vblanks(projection, None, target, input, config, 2);

        assert_eq!(actual, expected);
        assert_eq!(caught_up.yaw(), stepped.yaw());
        assert_eq!(caught_up.position(), stepped.position());
        assert_eq!(caught_up.focus(), stepped.focus());
    }

    #[test]
    fn manual_pitch_input_clamps_to_config_limits() {
        let mut camera = ThirdPersonCameraState::new(Angle::HALF);
        let mut config = ThirdPersonCameraConfig::character(1400, 700, 0);
        config.pitch_min_q12 = -64;
        config.pitch_max_q12 = 96;
        let target = ThirdPersonCameraTarget {
            player: RoomPoint::ZERO,
            player_yaw: Angle::ZERO,
            moving: false,
            lock_target: None,
        };

        let frame = camera.update(
            WorldProjection::new(160, 120, 320, 64),
            None,
            target,
            ThirdPersonCameraInput {
                yaw_delta_q12: 0,
                pitch_delta_q12: 512,
                recenter: false,
            },
            config,
        );

        assert_eq!(frame.pitch_q12, 96);
    }
}
