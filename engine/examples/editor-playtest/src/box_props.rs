use super::*;
mod geometry;
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
            self.box_prop_runtime[index] = geometry::build_box_prop_runtime(prop);
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
                && geometry::box_prop_aabb_overlaps_xz(
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
