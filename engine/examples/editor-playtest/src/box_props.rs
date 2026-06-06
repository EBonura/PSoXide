use super::*;
mod rendering;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct BoxPropBreakEvent {
    prop_index: u16,
    age: u8,
    impulse_x_q8: i16,
    impulse_z_q8: i16,
    /// Room-floor Y beneath the broken box (from its runtime). Shard
    /// vertices clamp to this, so fragments settle on the ground instead
    /// of floating at an elevated box's bottom.
    ground_y: i32,
    /// Y offset the box had when it broke (its fall offset on impact; 0
    /// for an in-place break). Shards spawn from this landed position.
    y_offset: i32,
}

impl BoxPropBreakEvent {
    pub(super) const EMPTY: Self = Self {
        prop_index: u16::MAX,
        age: 0,
        impulse_x_q8: 0,
        impulse_z_q8: 0,
        ground_y: i32::MIN,
        y_offset: 0,
    };

    const fn is_active(self) -> bool {
        self.prop_index != u16::MAX
    }
}

/// Per-box dynamic fall state. A box becomes `falling` once whatever held
/// it up (the floor or a box beneath) is gone; it then accelerates down by
/// `vel` until its bottom reaches the room floor, where it breaks.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct BoxPropFallState {
    falling: bool,
    /// Downward offset applied to the box's geometry while falling (<= 0).
    fall_y: i32,
    /// Current downward speed in room units per vblank.
    vel: i32,
}

impl BoxPropFallState {
    pub(super) const EMPTY: Self = Self {
        falling: false,
        fall_y: 0,
        vel: 0,
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropBreakShard {
    face: u8,
    u0_q8: u16,
    v0_q8: u16,
    u1_q8: u16,
    v1_q8: u16,
    drift_q8_per_frame: i8,
    lift_per_frame: i8,
    impulse_per_frame: u8,
    twist_q8_per_frame: i8,
    delay: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropBreakShardRuntime {
    face: u8,
    base_quad: [WorldVertex; 4],
    center: WorldVertex,
    edge_u: [i32; 3],
    edge_v: [i32; 3],
    face_delta: [i32; 3],
    colors: [(u8, u8, u8); 4],
}

impl BoxPropBreakShardRuntime {
    const EMPTY: Self = Self {
        face: 0,
        base_quad: [WorldVertex::ZERO; 4],
        center: WorldVertex::ZERO,
        edge_u: [0; 3],
        edge_v: [0; 3],
        face_delta: [0; 3],
        colors: [(0, 0, 0); 4],
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropFloorDebrisChip {
    face: u8,
    offset_x_q8: i16,
    offset_z_q8: i16,
    half_length_q8: u16,
    half_width_q8: u16,
    yaw_q12: u16,
    u0_q8: u16,
    v0_q8: u16,
    u1_q8: u16,
    v1_q8: u16,
    lift: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropDebrisBounds {
    center_x: i32,
    center_z: i32,
    span_x: i32,
    span_z: i32,
}

impl BoxPropDebrisBounds {
    const EMPTY: Self = Self {
        center_x: 0,
        center_z: 0,
        span_x: 64,
        span_z: 64,
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct BoxPropFaceRuntime {
    vertices: [WorldVertex; 4],
    center: WorldVertex,
    normal: [i32; 3],
}

impl BoxPropFaceRuntime {
    const EMPTY: Self = Self {
        vertices: [WorldVertex::ZERO; 4],
        center: WorldVertex::ZERO,
        normal: [0, 0, 0],
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct BoxPropRuntime {
    faces: [BoxPropFaceRuntime; psx_level::BOX_PROP_FACE_COUNT],
    break_shards: [BoxPropBreakShardRuntime; BOX_PROP_BREAK_SHARD_COUNT],
    cull_center: WorldVertex,
    cull_radius: i32,
    /// The box's own lowest vertex Y (its authored bottom).
    floor_y: i32,
    /// Room-floor Y beneath the box (baked). Fragments settle here and
    /// an unsupported box falls to here; equals `floor_y` for a box that
    /// already rests on the floor.
    ground_y: i32,
    debris_bounds: BoxPropDebrisBounds,
    aabb_min: RoomPoint,
    aabb_max: RoomPoint,
}

impl BoxPropRuntime {
    pub(super) const EMPTY: Self = Self {
        faces: [BoxPropFaceRuntime::EMPTY; psx_level::BOX_PROP_FACE_COUNT],
        break_shards: [BoxPropBreakShardRuntime::EMPTY; BOX_PROP_BREAK_SHARD_COUNT],
        cull_center: WorldVertex::ZERO,
        cull_radius: 32,
        floor_y: 0,
        ground_y: 0,
        debris_bounds: BoxPropDebrisBounds::EMPTY,
        aabb_min: RoomPoint::ZERO,
        aabb_max: RoomPoint::ZERO,
    };
}

impl Playtest {
    pub(super) fn rebuild_box_prop_runtime(&mut self) {
        self.box_prop_runtime = [BoxPropRuntime::EMPTY; MAX_BOX_PROP_STATE];
        self.box_prop_fall = [BoxPropFallState::EMPTY; MAX_BOX_PROP_STATE];
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if index >= self.box_prop_runtime.len() {
                break;
            }
            self.box_prop_runtime[index] = build_box_prop_runtime(prop);
        }
    }

    fn is_box_prop_broken(&self, index: usize) -> bool {
        let Some((word, mask)) = box_prop_state_bit(index) else {
            return false;
        };
        self.box_prop_broken[word] & mask != 0
    }

    fn mark_box_prop_broken(&mut self, index: usize, impulse_x_q8: i16, impulse_z_q8: i16) -> bool {
        let Some((word, mask)) = box_prop_state_bit(index) else {
            return false;
        };
        if self.box_prop_broken[word] & mask != 0 {
            return false;
        }
        self.box_prop_broken[word] |= mask;
        self.spawn_box_prop_break_event(index, impulse_x_q8, impulse_z_q8);
        // Anything that was resting on this box (or on a box this one was
        // holding up) has lost its support: let it fall and break in turn.
        self.start_unsupported_box_falls();
        true
    }

    fn spawn_box_prop_break_event(&mut self, index: usize, impulse_x_q8: i16, impulse_z_q8: i16) {
        let prop_index = index.min(u16::MAX as usize) as u16;
        let ground_y = self
            .box_prop_runtime
            .get(index)
            .map_or(i32::MIN, |runtime| runtime.ground_y);
        // Spawn shards from wherever the box actually is: a box that fell
        // and broke on impact carries its landed downward offset.
        let y_offset = self.box_prop_fall.get(index).map_or(0, |fall| fall.fall_y);
        let replacement = BoxPropBreakEvent {
            prop_index,
            age: 0,
            impulse_x_q8,
            impulse_z_q8,
            ground_y,
            y_offset,
        };
        let mut target = 0usize;
        let mut oldest_age = 0u8;
        for (slot, event) in self.box_prop_break_events.iter().enumerate() {
            if !event.is_active() {
                self.box_prop_break_events[slot] = replacement;
                return;
            }
            if event.age >= oldest_age {
                oldest_age = event.age;
                target = slot;
            }
        }
        self.box_prop_break_events[target] = replacement;
    }

    pub(super) fn advance_box_prop_break_events(&mut self, delta_vblanks: u16) {
        let step = delta_vblanks.max(1).min(u8::MAX as u16) as u8;
        for event in &mut self.box_prop_break_events {
            if !event.is_active() {
                continue;
            }
            event.age = event.age.saturating_add(step);
            if event.age >= BOX_PROP_BREAK_FRAMES {
                *event = BoxPropBreakEvent::EMPTY;
            }
        }
    }

    /// Mark every box that has lost its support as falling, to a fixpoint
    /// so a toppling stack cascades: the box whose base is gone falls,
    /// which in turn unsupports the box above it. Cheap and event-driven
    /// (only runs when a box breaks), so it adds no steady-state cost.
    fn start_unsupported_box_falls(&mut self) {
        let count = BOX_PROPS.len().min(self.box_prop_runtime.len());
        loop {
            let mut changed = false;
            for index in 0..count {
                // Only the active room's boxes are live; never start a fall
                // in an unloaded room from a break over here.
                if BOX_PROPS[index].room != self.room_index {
                    continue;
                }
                if self.is_box_prop_broken(index) || self.box_prop_fall[index].falling {
                    continue;
                }
                if self.box_prop_supported(index, count) {
                    continue;
                }
                self.box_prop_fall[index].falling = true;
                self.box_prop_fall[index].vel = 0;
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    /// True if box `index` rests on the room floor or on a still-standing
    /// (unbroken, not-falling) box directly beneath it.
    fn box_prop_supported(&self, index: usize, count: usize) -> bool {
        let runtime = self.box_prop_runtime[index];
        // Grounded: its bottom sits at (or below) the baked room floor.
        if runtime.aabb_min.y <= runtime.ground_y.saturating_add(BOX_PROP_SUPPORT_TOLERANCE) {
            return true;
        }
        let room = BOX_PROPS[index].room;
        for other in 0..count {
            if other == index
                || BOX_PROPS[other].room != room
                || self.is_box_prop_broken(other)
                || self.box_prop_fall[other].falling
            {
                continue;
            }
            let support = self.box_prop_runtime[other];
            // `other` holds up `index` when its top meets `index`'s bottom
            // and their footprints overlap in X/Z.
            let gap = runtime.aabb_min.y.saturating_sub(support.aabb_max.y).abs();
            if gap <= BOX_PROP_SUPPORT_TOLERANCE
                && box_prop_aabb_overlaps_xz(
                    runtime.aabb_min,
                    runtime.aabb_max,
                    support.aabb_min,
                    support.aabb_max,
                )
            {
                return true;
            }
        }
        false
    }

    /// Advance every falling box under gravity and break it on impact with
    /// the room floor. Only touches boxes flagged `falling`, so it is
    /// transient work (no steady-state cost while boxes sit still).
    pub(super) fn advance_box_prop_falls(&mut self, delta_vblanks: u16) {
        let step = delta_vblanks.max(1) as i32;
        let count = BOX_PROPS.len().min(self.box_prop_runtime.len());
        for index in 0..count {
            if !self.box_prop_fall[index].falling || self.is_box_prop_broken(index) {
                continue;
            }
            let runtime = self.box_prop_runtime[index];
            let mut fall = self.box_prop_fall[index];
            fall.vel = fall
                .vel
                .saturating_add(BOX_PROP_FALL_GRAVITY.saturating_mul(step))
                .min(BOX_PROP_FALL_MAX_VEL);
            fall.fall_y = fall.fall_y.saturating_sub(fall.vel.saturating_mul(step));
            if runtime.aabb_min.y.saturating_add(fall.fall_y) <= runtime.ground_y {
                // Snap so the box's bottom rests exactly on the floor, then
                // break on impact. `mark_box_prop_broken` reads this final
                // fall_y for the shard spawn offset and cascades to whatever
                // was stacked on top of this box.
                fall.fall_y = runtime.ground_y.saturating_sub(runtime.aabb_min.y);
                fall.falling = false;
                self.box_prop_fall[index] = fall;
                self.mark_box_prop_broken(index, 0, 0);
            } else {
                self.box_prop_fall[index] = fall;
            }
        }
    }

    pub(super) fn break_box_props_for_movement(
        &mut self,
        trigger: u16,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) {
        let current = self.motor.position();
        let target = box_prop_movement_probe_target(
            current,
            self.motor.yaw(),
            input,
            config,
            trigger,
            delta_vblanks,
        );
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if prop.room != self.room_index
                || prop.flags & trigger == 0
                || self.is_box_prop_broken(index)
            {
                continue;
            }
            let Some(box_runtime) = self.box_prop_runtime.get(index) else {
                continue;
            };
            let (min, max) = (box_runtime.aabb_min, box_runtime.aabb_max);
            if character_body_overlaps_aabb(current, config.radius, config.height, min, max)
                || character_body_overlaps_aabb(target, config.radius, config.height, min, max)
            {
                let (impulse_x_q8, impulse_z_q8) = box_prop_break_impulse_from_delta(
                    target.x.saturating_sub(current.x),
                    target.z.saturating_sub(current.z),
                );
                self.mark_box_prop_broken(index, impulse_x_q8, impulse_z_q8);
            }
        }
    }

    pub(super) fn break_box_props_for_attack(&mut self, config: CharacterMotorConfig) {
        let origin = self.motor.position();
        let yaw = self.motor.yaw();
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if prop.room != self.room_index
                || prop.flags & box_prop_flags::BREAK_ON_ATTACK == 0
                || self.is_box_prop_broken(index)
            {
                continue;
            }
            let Some(box_runtime) = self.box_prop_runtime.get(index) else {
                continue;
            };
            let (min, max) = (box_runtime.aabb_min, box_runtime.aabb_max);
            if box_prop_intersects_attack_volume(origin, yaw, config, min, max) {
                let center_x = min.x.saturating_add(max.x) / 2;
                let center_z = min.z.saturating_add(max.z) / 2;
                let mut impulse = box_prop_break_impulse_from_delta(
                    center_x.saturating_sub(origin.x),
                    center_z.saturating_sub(origin.z),
                );
                if impulse == (0, 0) {
                    impulse = box_prop_break_impulse_from_yaw(yaw);
                }
                self.mark_box_prop_broken(index, impulse.0, impulse.1);
            }
        }
    }

    pub(super) fn collect_box_prop_collision_blockers(
        &self,
        out: &mut [CharacterCollisionAabb; MAX_BOX_PROP_BLOCKERS],
    ) -> usize {
        let mut count = 0usize;
        for (index, prop) in BOX_PROPS.iter().enumerate() {
            if prop.room != self.room_index
                || prop.flags & box_prop_flags::COLLISION_ENABLED == 0
                || self.is_box_prop_broken(index)
                || count >= out.len()
            {
                continue;
            }
            let Some(box_runtime) = self.box_prop_runtime.get(index) else {
                continue;
            };
            let (min, max) = (box_runtime.aabb_min, box_runtime.aabb_max);
            out[count] = CharacterCollisionAabb::new(min, max);
            count += 1;
        }
        count
    }
}

const BOX_PROP_FACE_VERTEX_INDICES: [[usize; 4]; psx_level::BOX_PROP_FACE_COUNT] = [
    [4, 5, 1, 0],
    [5, 6, 2, 1],
    [6, 7, 3, 2],
    [7, 4, 0, 3],
    [7, 6, 5, 4],
    [0, 1, 2, 3],
];

const BOX_PROP_BREAK_SHARDS: [BoxPropBreakShard; BOX_PROP_BREAK_SHARD_COUNT] = [
    BoxPropBreakShard {
        face: 0,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 84,
        v1_q8: 256,
        drift_q8_per_frame: -3,
        lift_per_frame: 28,
        impulse_per_frame: 34,
        twist_q8_per_frame: -4,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 0,
        u0_q8: 172,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 4,
        lift_per_frame: 24,
        impulse_per_frame: 31,
        twist_q8_per_frame: -6,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 1,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 86,
        v1_q8: 256,
        drift_q8_per_frame: -4,
        lift_per_frame: 30,
        impulse_per_frame: 36,
        twist_q8_per_frame: 6,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 1,
        u0_q8: 170,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 5,
        lift_per_frame: 26,
        impulse_per_frame: 32,
        twist_q8_per_frame: 5,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 2,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 84,
        v1_q8: 256,
        drift_q8_per_frame: -5,
        lift_per_frame: 24,
        impulse_per_frame: 30,
        twist_q8_per_frame: 5,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 3,
        u0_q8: 86,
        v0_q8: 0,
        u1_q8: 170,
        v1_q8: 256,
        drift_q8_per_frame: 1,
        lift_per_frame: 40,
        impulse_per_frame: 41,
        twist_q8_per_frame: 4,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 4,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 128,
        v1_q8: 128,
        drift_q8_per_frame: -3,
        lift_per_frame: 48,
        impulse_per_frame: 26,
        twist_q8_per_frame: 5,
        delay: 0,
    },
    BoxPropBreakShard {
        face: 4,
        u0_q8: 128,
        v0_q8: 128,
        u1_q8: 256,
        v1_q8: 256,
        drift_q8_per_frame: 3,
        lift_per_frame: 50,
        impulse_per_frame: 30,
        twist_q8_per_frame: 6,
        delay: 0,
    },
];

const BOX_PROP_FLOOR_DEBRIS_CHIPS: [BoxPropFloorDebrisChip; 12] = [
    BoxPropFloorDebrisChip {
        face: 0,
        offset_x_q8: -80,
        offset_z_q8: -72,
        half_length_q8: 46,
        half_width_q8: 13,
        yaw_q12: 384,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 84,
        v1_q8: 256,
        lift: 6,
    },
    BoxPropFloorDebrisChip {
        face: 0,
        offset_x_q8: 38,
        offset_z_q8: -94,
        half_length_q8: 58,
        half_width_q8: 12,
        yaw_q12: 960,
        u0_q8: 84,
        v0_q8: 0,
        u1_q8: 172,
        v1_q8: 256,
        lift: 8,
    },
    BoxPropFloorDebrisChip {
        face: 1,
        offset_x_q8: 104,
        offset_z_q8: -24,
        half_length_q8: 42,
        half_width_q8: 15,
        yaw_q12: 1328,
        u0_q8: 170,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        lift: 7,
    },
    BoxPropFloorDebrisChip {
        face: 1,
        offset_x_q8: 42,
        offset_z_q8: 72,
        half_length_q8: 54,
        half_width_q8: 13,
        yaw_q12: 1888,
        u0_q8: 0,
        v0_q8: 16,
        u1_q8: 86,
        v1_q8: 240,
        lift: 10,
    },
    BoxPropFloorDebrisChip {
        face: 2,
        offset_x_q8: -96,
        offset_z_q8: 44,
        half_length_q8: 50,
        half_width_q8: 11,
        yaw_q12: 2384,
        u0_q8: 84,
        v0_q8: 16,
        u1_q8: 172,
        v1_q8: 240,
        lift: 9,
    },
    BoxPropFloorDebrisChip {
        face: 2,
        offset_x_q8: -28,
        offset_z_q8: 104,
        half_length_q8: 34,
        half_width_q8: 16,
        yaw_q12: 3040,
        u0_q8: 172,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 256,
        lift: 6,
    },
    BoxPropFloorDebrisChip {
        face: 3,
        offset_x_q8: -132,
        offset_z_q8: -10,
        half_length_q8: 44,
        half_width_q8: 13,
        yaw_q12: 3536,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 86,
        v1_q8: 256,
        lift: 8,
    },
    BoxPropFloorDebrisChip {
        face: 3,
        offset_x_q8: 116,
        offset_z_q8: 92,
        half_length_q8: 32,
        half_width_q8: 14,
        yaw_q12: 256,
        u0_q8: 86,
        v0_q8: 24,
        u1_q8: 170,
        v1_q8: 232,
        lift: 11,
    },
    BoxPropFloorDebrisChip {
        face: 4,
        offset_x_q8: -24,
        offset_z_q8: -8,
        half_length_q8: 62,
        half_width_q8: 24,
        yaw_q12: 704,
        u0_q8: 0,
        v0_q8: 0,
        u1_q8: 128,
        v1_q8: 128,
        lift: 5,
    },
    BoxPropFloorDebrisChip {
        face: 4,
        offset_x_q8: 82,
        offset_z_q8: 36,
        half_length_q8: 40,
        half_width_q8: 20,
        yaw_q12: 2656,
        u0_q8: 128,
        v0_q8: 0,
        u1_q8: 256,
        v1_q8: 128,
        lift: 7,
    },
    BoxPropFloorDebrisChip {
        face: 5,
        offset_x_q8: -54,
        offset_z_q8: 84,
        half_length_q8: 36,
        half_width_q8: 18,
        yaw_q12: 1536,
        u0_q8: 0,
        v0_q8: 128,
        u1_q8: 128,
        v1_q8: 256,
        lift: 6,
    },
    BoxPropFloorDebrisChip {
        face: 5,
        offset_x_q8: 8,
        offset_z_q8: -126,
        half_length_q8: 30,
        half_width_q8: 16,
        yaw_q12: 3264,
        u0_q8: 128,
        v0_q8: 128,
        u1_q8: 256,
        v1_q8: 256,
        lift: 9,
    },
];

fn box_prop_state_bit(index: usize) -> Option<(usize, u32)> {
    if index >= MAX_BOX_PROP_STATE {
        return None;
    }
    Some((index / 32, 1u32 << (index % 32)))
}

fn box_prop_broken_in_words(broken: &[u32; BOX_PROP_BROKEN_WORDS], index: usize) -> bool {
    let Some((word, mask)) = box_prop_state_bit(index) else {
        return false;
    };
    broken[word] & mask != 0
}

#[inline(always)]
pub(super) fn box_prop_profile_begin(stage_id: u16) {
    if BOX_PROP_PROFILE_ENABLED {
        telemetry::stage_begin(stage_id);
    }
}

#[inline(always)]
pub(super) fn box_prop_profile_end(stage_id: u16) {
    if BOX_PROP_PROFILE_ENABLED {
        telemetry::stage_end(stage_id);
    }
}

pub(super) fn draw_box_props<T>(
    props: &[LevelBoxPropRecord],
    broken: &[u32; BOX_PROP_BROKEN_WORDS],
    runtime: &[BoxPropRuntime; MAX_BOX_PROP_STATE],
    fall: &[BoxPropFallState; MAX_BOX_PROP_STATE],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    rendering::draw_box_props(
        props,
        broken,
        runtime,
        fall,
        current_room,
        camera,
        options,
        lighting,
        triangles,
        world,
    );
}

pub(super) fn draw_box_prop_floor_debris<T>(
    props: &[LevelBoxPropRecord],
    broken: &[u32; BOX_PROP_BROKEN_WORDS],
    runtime: &[BoxPropRuntime; MAX_BOX_PROP_STATE],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    rendering::draw_box_prop_floor_debris(
        props,
        broken,
        runtime,
        current_room,
        camera,
        options,
        lighting,
        triangles,
        world,
    );
}

pub(super) fn draw_box_prop_break_events<T>(
    events: &[BoxPropBreakEvent; MAX_BOX_PROP_BREAK_EVENTS],
    props: &[LevelBoxPropRecord],
    runtime: &[BoxPropRuntime; MAX_BOX_PROP_STATE],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    rendering::draw_box_prop_break_events(
        events,
        props,
        runtime,
        current_room,
        camera,
        options,
        lighting,
        triangles,
        world,
    );
}

fn box_prop_face_point_q8(face: [WorldVertex; 4], u_q8: u16, v_q8: u16) -> WorldVertex {
    let left = lerp_world_vertex_q8(face[0], face[3], v_q8);
    let right = lerp_world_vertex_q8(face[1], face[2], v_q8);
    lerp_world_vertex_q8(left, right, u_q8)
}

fn box_prop_quad_center(quad: [WorldVertex; 4]) -> WorldVertex {
    WorldVertex::new(
        average4_i32(quad[0].x, quad[1].x, quad[2].x, quad[3].x),
        average4_i32(quad[0].y, quad[1].y, quad[2].y, quad[3].y),
        average4_i32(quad[0].z, quad[1].z, quad[2].z, quad[3].z),
    )
}

fn box_prop_face_color_at(
    prop: &LevelBoxPropRecord,
    face: usize,
    u_q8: u16,
    v_q8: u16,
) -> (u8, u8, u8) {
    let colors = prop.baked_vertex_rgb[face];
    let top = lerp_rgb_q8(colors[0], colors[1], u_q8);
    let bottom = lerp_rgb_q8(colors[3], colors[2], u_q8);
    lerp_rgb_q8(top, bottom, v_q8)
}

fn lerp_world_vertex_q8(a: WorldVertex, b: WorldVertex, t_q8: u16) -> WorldVertex {
    WorldVertex::new(
        lerp_i32_q8(a.x, b.x, t_q8),
        lerp_i32_q8(a.y, b.y, t_q8),
        lerp_i32_q8(a.z, b.z, t_q8),
    )
}

fn lerp_rgb_q8(a: (u8, u8, u8), b: (u8, u8, u8), t_q8: u16) -> (u8, u8, u8) {
    (
        lerp_i32_q8(a.0 as i32, b.0 as i32, t_q8) as u8,
        lerp_i32_q8(a.1 as i32, b.1 as i32, t_q8) as u8,
        lerp_i32_q8(a.2 as i32, b.2 as i32, t_q8) as u8,
    )
}

fn lerp_i32_q8(a: i32, b: i32, t_q8: u16) -> i32 {
    let t = t_q8.min(256) as i32;
    a.saturating_add(b.saturating_sub(a).saturating_mul(t) / 256)
}

fn uv_from_q8(max: u8, t_q8: u16) -> u8 {
    ((max as u16).saturating_mul(t_q8.min(256)) >> 8) as u8
}

fn shrink_world_vertex_around(
    vertex: WorldVertex,
    center: WorldVertex,
    scale_q8: i32,
) -> WorldVertex {
    WorldVertex::new(
        center.x.saturating_add(scale_q8_i32_signed(
            vertex.x.saturating_sub(center.x),
            scale_q8,
        )),
        center.y.saturating_add(scale_q8_i32_signed(
            vertex.y.saturating_sub(center.y),
            scale_q8,
        )),
        center.z.saturating_add(scale_q8_i32_signed(
            vertex.z.saturating_sub(center.z),
            scale_q8,
        )),
    )
}

fn world_vertex_delta(from: WorldVertex, to: WorldVertex) -> [i32; 3] {
    [
        to.x.saturating_sub(from.x),
        to.y.saturating_sub(from.y),
        to.z.saturating_sub(from.z),
    ]
}

fn scale_world_delta_q8(delta: [i32; 3], scale_q8: i32) -> [i32; 3] {
    [
        scale_q8_i32_signed(delta[0], scale_q8),
        scale_q8_i32_signed(delta[1], scale_q8),
        scale_q8_i32_signed(delta[2], scale_q8),
    ]
}

fn add_world_vertex_offset(vertex: WorldVertex, offset: [i32; 3]) -> WorldVertex {
    WorldVertex::new(
        vertex.x.saturating_add(offset[0]),
        vertex.y.saturating_add(offset[1]),
        vertex.z.saturating_add(offset[2]),
    )
}

fn scale_q8_i32_signed(value: i32, scale_q8: i32) -> i32 {
    value.saturating_mul(scale_q8) / 256
}

pub(super) fn average_vertex_rgb(colors: [(u8, u8, u8); 4]) -> (u8, u8, u8) {
    let mut r = 0u16;
    let mut g = 0u16;
    let mut b = 0u16;
    for color in colors {
        r += color.0 as u16;
        g += color.1 as u16;
        b += color.2 as u16;
    }
    ((r / 4) as u8, (g / 4) as u8, (b / 4) as u8)
}

fn box_prop_vertices(prop: &LevelBoxPropRecord) -> [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT] {
    let mut out = [WorldVertex::new(0, 0, 0); psx_level::BOX_PROP_VERTEX_COUNT];
    let mut i = 0usize;
    while i < prop.vertices.len() {
        let local = prop.vertices[i];
        let rotated = rotate_z_q12(
            rotate_y_q12(
                rotate_x_q12(
                    [local[0] as i32, local[1] as i32, local[2] as i32],
                    prop.pitch as u16,
                ),
                prop.yaw as u16,
            ),
            prop.roll as u16,
        );
        out[i] = WorldVertex::new(
            prop.x.saturating_add(rotated[0]),
            prop.y.saturating_add(rotated[1]),
            prop.z.saturating_add(rotated[2]),
        );
        i += 1;
    }
    out
}

fn box_prop_faces(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT] {
    let mut out = [[WorldVertex::new(0, 0, 0); 4]; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        let mut corner = 0usize;
        while corner < 4 {
            out[face][corner] = vertices[BOX_PROP_FACE_VERTEX_INDICES[face][corner]];
            corner += 1;
        }
        face += 1;
    }
    out
}

fn box_prop_face_runtime(face: [WorldVertex; 4]) -> BoxPropFaceRuntime {
    let abx = face[1].x.saturating_sub(face[0].x);
    let aby = face[1].y.saturating_sub(face[0].y);
    let abz = face[1].z.saturating_sub(face[0].z);
    let acx = face[2].x.saturating_sub(face[0].x);
    let acy = face[2].y.saturating_sub(face[0].y);
    let acz = face[2].z.saturating_sub(face[0].z);
    let nx = aby
        .saturating_mul(acz)
        .saturating_sub(abz.saturating_mul(acy))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let ny = abz
        .saturating_mul(acx)
        .saturating_sub(abx.saturating_mul(acz))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    let nz = abx
        .saturating_mul(acy)
        .saturating_sub(aby.saturating_mul(acx))
        >> BOX_PROP_FACE_NORMAL_SHIFT;
    BoxPropFaceRuntime {
        vertices: face,
        center: WorldVertex::new(
            average4_i32(face[0].x, face[1].x, face[2].x, face[3].x),
            average4_i32(face[0].y, face[1].y, face[2].y, face[3].y),
            average4_i32(face[0].z, face[1].z, face[2].z, face[3].z),
        ),
        normal: [nx, ny, nz],
    }
}

fn build_box_prop_runtime(prop: &LevelBoxPropRecord) -> BoxPropRuntime {
    let vertices = box_prop_vertices(prop);
    let raw_faces = box_prop_faces(vertices);
    let mut faces = [BoxPropFaceRuntime::EMPTY; psx_level::BOX_PROP_FACE_COUNT];
    let mut face = 0usize;
    while face < psx_level::BOX_PROP_FACE_COUNT {
        faces[face] = box_prop_face_runtime(raw_faces[face]);
        face += 1;
    }
    let (cull_center, cull_radius) = box_prop_cull_bounds(vertices);
    let break_shards = box_prop_break_shard_runtime(prop, raw_faces, cull_center);
    let (aabb_min, aabb_max) = box_prop_aabb_from_vertices(vertices);
    let floor_y = box_prop_floor_y(vertices);
    BoxPropRuntime {
        faces,
        break_shards,
        cull_center,
        cull_radius,
        floor_y,
        // Never let the baked ground sit above the box's own bottom (a box
        // rests on or above its floor); guards against a stale cook value.
        ground_y: prop.ground_y.min(floor_y),
        debris_bounds: box_prop_debris_bounds(vertices),
        aabb_min,
        aabb_max,
    }
}

fn box_prop_break_shard_runtime(
    prop: &LevelBoxPropRecord,
    faces: [[WorldVertex; 4]; psx_level::BOX_PROP_FACE_COUNT],
    box_center: WorldVertex,
) -> [BoxPropBreakShardRuntime; BOX_PROP_BREAK_SHARD_COUNT] {
    let mut out = [BoxPropBreakShardRuntime::EMPTY; BOX_PROP_BREAK_SHARD_COUNT];
    let mut shard_index = 0usize;
    while shard_index < BOX_PROP_BREAK_SHARD_COUNT {
        let shard = BOX_PROP_BREAK_SHARDS[shard_index];
        let face = shard.face as usize;
        if face < psx_level::BOX_PROP_FACE_COUNT {
            let face_vertices = faces[face];
            let base_quad = [
                box_prop_face_point_q8(face_vertices, shard.u0_q8, shard.v0_q8),
                box_prop_face_point_q8(face_vertices, shard.u1_q8, shard.v0_q8),
                box_prop_face_point_q8(face_vertices, shard.u1_q8, shard.v1_q8),
                box_prop_face_point_q8(face_vertices, shard.u0_q8, shard.v1_q8),
            ];
            let face_center = box_prop_quad_center(face_vertices);
            out[shard_index] = BoxPropBreakShardRuntime {
                face: shard.face,
                base_quad,
                center: box_prop_quad_center(base_quad),
                edge_u: world_vertex_delta(face_vertices[0], face_vertices[1]),
                edge_v: world_vertex_delta(face_vertices[0], face_vertices[3]),
                face_delta: world_vertex_delta(box_center, face_center),
                colors: [
                    box_prop_face_color_at(prop, face, shard.u0_q8, shard.v0_q8),
                    box_prop_face_color_at(prop, face, shard.u1_q8, shard.v0_q8),
                    box_prop_face_color_at(prop, face, shard.u1_q8, shard.v1_q8),
                    box_prop_face_color_at(prop, face, shard.u0_q8, shard.v1_q8),
                ],
            };
        }
        shard_index += 1;
    }
    out
}

fn box_prop_cull_bounds(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> (WorldVertex, i32) {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_y = vertices[0].y;
    let mut max_y = vertices[0].y;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    let center = WorldVertex::new(
        min_x.saturating_add(max_x) / 2,
        min_y.saturating_add(max_y) / 2,
        min_z.saturating_add(max_z) / 2,
    );
    let radius = abs_delta_i32(max_x, min_x)
        .saturating_add(abs_delta_i32(max_y, min_y))
        .saturating_add(abs_delta_i32(max_z, min_z))
        >> 1;
    (center, radius.max(32))
}

fn box_prop_floor_y(vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT]) -> i32 {
    let mut floor_y = vertices[0].y;
    for vertex in vertices {
        floor_y = floor_y.min(vertex.y);
    }
    floor_y
}

fn box_prop_debris_bounds(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> BoxPropDebrisBounds {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    BoxPropDebrisBounds {
        center_x: min_x.saturating_add(max_x) / 2,
        center_z: min_z.saturating_add(max_z) / 2,
        span_x: max_x.saturating_sub(min_x).max(64),
        span_z: max_z.saturating_sub(min_z).max(64),
    }
}

fn box_prop_aabb_from_vertices(
    vertices: [WorldVertex; psx_level::BOX_PROP_VERTEX_COUNT],
) -> (RoomPoint, RoomPoint) {
    let mut min_x = vertices[0].x;
    let mut max_x = vertices[0].x;
    let mut min_y = vertices[0].y;
    let mut max_y = vertices[0].y;
    let mut min_z = vertices[0].z;
    let mut max_z = vertices[0].z;
    for vertex in vertices {
        min_x = min_x.min(vertex.x);
        max_x = max_x.max(vertex.x);
        min_y = min_y.min(vertex.y);
        max_y = max_y.max(vertex.y);
        min_z = min_z.min(vertex.z);
        max_z = max_z.max(vertex.z);
    }
    (
        RoomPoint::new(min_x, min_y, min_z),
        RoomPoint::new(max_x, max_y, max_z),
    )
}

/// Whether two box AABBs overlap in the X/Z (floor) plane. Used to decide
/// if one box sits over another for stacked-support detection.
fn box_prop_aabb_overlaps_xz(
    a_min: RoomPoint,
    a_max: RoomPoint,
    b_min: RoomPoint,
    b_max: RoomPoint,
) -> bool {
    a_min.x <= b_max.x && a_max.x >= b_min.x && a_min.z <= b_max.z && a_max.z >= b_min.z
}

/// Shift a box-face quad down (or up) by `dy` room units. Used to draw a
/// falling box at its current offset without rebuilding its runtime.
fn box_prop_offset_quad_y(quad: [WorldVertex; 4], dy: i32) -> [WorldVertex; 4] {
    if dy == 0 {
        return quad;
    }
    let mut out = quad;
    for vertex in out.iter_mut() {
        *vertex = WorldVertex::new(vertex.x, vertex.y.saturating_add(dy), vertex.z);
    }
    out
}

pub(super) fn box_prop_movement_break_trigger(
    input: CharacterMotorInput,
    config: CharacterMotorConfig,
    stamina_q12: i32,
) -> Option<u16> {
    let moving = input.move_x.raw() != 0 || input.move_z.raw() != 0 || input.walk != 0;
    if !moving {
        return None;
    }
    if input.sprint && stamina_q12 > 0 && config.run_speed > config.walk_speed {
        Some(box_prop_flags::BREAK_ON_RUN)
    } else {
        Some(box_prop_flags::BREAK_ON_WALK)
    }
}

fn box_prop_movement_probe_target(
    origin: RoomPoint,
    yaw: Angle,
    input: CharacterMotorInput,
    config: CharacterMotorConfig,
    trigger: u16,
    delta_vblanks: u16,
) -> RoomPoint {
    let base_speed = if trigger == box_prop_flags::BREAK_ON_RUN {
        config.run_speed
    } else {
        config.walk_speed
    };
    let speed = base_speed.saturating_mul(delta_vblanks.max(1).min(4) as i32);
    let dx = input.move_x.mul_i32(speed);
    let dz = input.move_z.mul_i32(speed);
    if dx != 0 || dz != 0 {
        return RoomPoint::new(
            origin.x.saturating_add(dx),
            origin.y,
            origin.z.saturating_add(dz),
        );
    }
    if input.walk == 0 || speed == 0 {
        return origin;
    }
    let signed_speed = if input.walk < 0 { -speed } else { speed };
    RoomPoint::new(
        origin.x.saturating_add(yaw.sin().mul_i32(signed_speed)),
        origin.y,
        origin.z.saturating_add(yaw.cos().mul_i32(signed_speed)),
    )
}

fn box_prop_break_impulse_from_delta(dx: i32, dz: i32) -> (i16, i16) {
    let denom = abs_i32_saturating(dx).saturating_add(abs_i32_saturating(dz));
    if denom <= 0 {
        return (0, 0);
    }
    let x = dx.saturating_mul(256) / denom;
    let z = dz.saturating_mul(256) / denom;
    (x.clamp(-256, 256) as i16, z.clamp(-256, 256) as i16)
}

fn box_prop_break_impulse_from_yaw(yaw: Angle) -> (i16, i16) {
    ((yaw.sin().raw() / 16) as i16, (yaw.cos().raw() / 16) as i16)
}

fn character_body_overlaps_aabb(
    position: RoomPoint,
    radius: i32,
    height: i32,
    min: RoomPoint,
    max: RoomPoint,
) -> bool {
    if max.y < position.y || min.y > position.y.saturating_add(height.max(1)) {
        return false;
    }
    let closest_x = position.x.clamp(min.x, max.x);
    let closest_z = position.z.clamp(min.z, max.z);
    let dx = position.x.saturating_sub(closest_x);
    let dz = position.z.saturating_sub(closest_z);
    square_i32_saturating(dx).saturating_add(square_i32_saturating(dz))
        <= square_i32_saturating(radius.max(0))
}

fn box_prop_intersects_attack_volume(
    origin: RoomPoint,
    yaw: Angle,
    config: CharacterMotorConfig,
    min: RoomPoint,
    max: RoomPoint,
) -> bool {
    let body_top = origin.y.saturating_add(config.height.max(1));
    if max.y < origin.y.saturating_sub(128) || min.y > body_top.saturating_add(128) {
        return false;
    }
    let center_x = min.x.saturating_add(max.x) / 2;
    let center_z = min.z.saturating_add(max.z) / 2;
    let dx = center_x.saturating_sub(origin.x);
    let dz = center_z.saturating_sub(origin.z);
    let sin_yaw = yaw.sin();
    let cos_yaw = yaw.cos();
    let forward = sin_yaw.mul_i32(dx).saturating_add(cos_yaw.mul_i32(dz));
    let lateral = cos_yaw.mul_i32(dx).saturating_sub(sin_yaw.mul_i32(dz));
    let prop_extent = abs_delta_i32(max.x, min.x).saturating_add(abs_delta_i32(max.z, min.z)) >> 1;
    let reach = BOX_PROP_BREAK_ATTACK_REACH
        .saturating_add(config.radius.max(0))
        .saturating_add(prop_extent);
    let half_width = BOX_PROP_BREAK_ATTACK_WIDTH
        .saturating_add(config.radius.max(0))
        .saturating_add(prop_extent);
    forward >= -prop_extent && forward <= reach && abs_i32_saturating(lateral) <= half_width
}
