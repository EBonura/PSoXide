//! Breakable box-prop state and rendering policy, carved out of
//! `editor-playtest`'s `box_props` module family (phase 2 of
//! docs/game-runtime-plan.md). [`BoxProps`] owns the broken/fall/event
//! state the example previously kept as scene fields; cooked prop
//! records arrive as `&'static` psx-level values, capacities as
//! `const N` generic parameters, and the example's VRAM slot resolver
//! as a closure. The break/fall tuning constants moved verbatim as
//! crate consts; promoting them to authored data is phase-3 work.

use psx_engine::{
    Angle, CharacterCollisionAabb, CharacterMotorConfig, CharacterMotorInput, RoomPoint,
    WorldVertex,
};
use psx_level::{box_prop_flags, LevelBoxPropRecord, LevelBoxPropSurfaceRecord, RoomIndex};
use psx_math::int32::{abs_i32, square_i32_saturating};

use crate::image_props::abs_delta_i32;

mod geometry;
mod rendering;

pub use self::rendering::{
    draw_box_prop_break_events, draw_box_prop_floor_debris, draw_box_props, DebrisCache,
};

/// Frames a break burst stays alive.
const BOX_PROP_BREAK_FRAMES: u8 = 24;
/// Frames of shard motion within a break burst.
const BOX_PROP_BREAK_MOTION_FRAMES: u8 = 20;
/// Baked shards spawned per broken box.
const BOX_PROP_BREAK_SHARD_COUNT: usize = 8;
/// Gravity applied to an unsupported, falling box (room units per vblank,
/// per vblank). Tuned so a stacked box drops over a handful of frames.
const BOX_PROP_FALL_GRAVITY: i32 = 28;
/// Per-vblank fall-speed cap so a tall drop cannot tunnel past its
/// landing in one step (the landing check snaps any overshoot anyway).
const BOX_PROP_FALL_MAX_VEL: i32 = 384;
/// Slack for "rests on the floor / on the box below" support tests, in
/// room units. Boxes are ~900+ units tall, so this only absorbs rounding
/// and small authored gaps.
const BOX_PROP_SUPPORT_TOLERANCE: i32 = 64;
const BOX_PROP_BREAK_ATTACK_REACH: i32 = 768;
const BOX_PROP_BREAK_ATTACK_WIDTH: i32 = 320;
const BOX_PROP_FACE_NORMAL_SHIFT: u32 = 10;

/// One live break burst for a newly broken box prop.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoxPropBreakEvent {
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
    /// Inactive slot sentinel.
    pub const EMPTY: Self = Self {
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
pub struct BoxPropFallState {
    falling: bool,
    /// Downward offset applied to the box's geometry while falling (<= 0).
    fall_y: i32,
    /// Current downward speed in room units per vblank.
    vel: i32,
}

impl BoxPropFallState {
    /// Resting (not falling) state.
    pub const EMPTY: Self = Self {
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

/// Static derived per-box data used by render, break tests, and
/// collision (world-space faces, cull bounds, floor/ground, AABB).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoxPropRuntime {
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
    /// Unbuilt slot placeholder.
    pub const EMPTY: Self = Self {
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

/// Owned box-prop runtime state: the persistent broken bits, the
/// static derived per-box data, the dynamic fall state, and the live
/// break bursts (formerly four scene fields). The game keeps one
/// instance wherever it keeps scene state.
pub struct BoxProps<
    const MAX_BOX_PROP_STATE: usize,
    const BOX_PROP_BROKEN_WORDS: usize,
    const MAX_BOX_PROP_BREAK_EVENTS: usize,
> {
    /// Persistent runtime state for authored breakable box props.
    broken: [u32; BOX_PROP_BROKEN_WORDS],
    /// Door-open bits: a box a [`logic_kind::DOOR`] record links is
    /// hidden and passable while its door is open, and comes back
    /// solid + drawn when it closes -- unlike `broken`, this state is
    /// reversible and spawns no debris.
    ///
    /// [`logic_kind::DOOR`]: psx_level::logic_kind::DOOR
    door_open: [u32; BOX_PROP_BROKEN_WORDS],
    /// Static derived box-prop data used by render, break tests, and collision.
    runtime: [BoxPropRuntime; MAX_BOX_PROP_STATE],
    /// Dynamic fall state per box, parallel to `broken`. A box
    /// starts falling when its support is removed and breaks on landing.
    fall: [BoxPropFallState; MAX_BOX_PROP_STATE],
    /// Short-lived baked face-burst events for newly broken box props.
    break_events: [BoxPropBreakEvent; MAX_BOX_PROP_BREAK_EVENTS],
}

impl<
        const MAX_BOX_PROP_STATE: usize,
        const BOX_PROP_BROKEN_WORDS: usize,
        const MAX_BOX_PROP_BREAK_EVENTS: usize,
    > BoxProps<MAX_BOX_PROP_STATE, BOX_PROP_BROKEN_WORDS, MAX_BOX_PROP_BREAK_EVENTS>
{
    /// Empty state. NOT all-zero bytes: the runtime slots carry safe
    /// placeholder cull/debris extents and the break-event slots carry
    /// inactive-sentinel indices, so a game keeping this state in
    /// link-time-zero (`.bss`) storage must stamp it at boot via
    /// [`Self::init`] instead of storing this `const` directly.
    pub const EMPTY: Self = Self {
        broken: [0; BOX_PROP_BROKEN_WORDS],
        door_open: [0; BOX_PROP_BROKEN_WORDS],
        runtime: [BoxPropRuntime::EMPTY; MAX_BOX_PROP_STATE],
        fall: [BoxPropFallState::EMPTY; MAX_BOX_PROP_STATE],
        break_events: [BoxPropBreakEvent::EMPTY; MAX_BOX_PROP_BREAK_EVENTS],
    };

    /// Stamp the non-zero pieces of [`Self::EMPTY`] onto link-time-zero
    /// storage: the unbuilt-slot runtime placeholders (element by
    /// element, so no whole-struct temporary is built) plus the dynamic
    /// state via [`Self::reset_dynamic_state`]. Equivalent to `*self =
    /// Self::EMPTY` over zeroed storage.
    pub fn init(&mut self) {
        for slot in self.runtime.iter_mut() {
            *slot = BoxPropRuntime::EMPTY;
        }
        self.reset_dynamic_state();
    }

    /// Reset the persistent + transient dynamic state (broken bits,
    /// door-open bits, falls, break bursts) on gameplay (re)entry.
    pub fn reset_dynamic_state(&mut self) {
        self.broken = [0; BOX_PROP_BROKEN_WORDS];
        self.door_open = [0; BOX_PROP_BROKEN_WORDS];
        self.fall = [BoxPropFallState::EMPTY; MAX_BOX_PROP_STATE];
        self.break_events = [BoxPropBreakEvent::EMPTY; MAX_BOX_PROP_BREAK_EVENTS];
    }

    /// Set the door-open state for box `index` (the logic runtime's
    /// DOOR kind drives this through its `link`): open = hidden +
    /// passable, closed = drawn + solid.
    pub fn set_door_open(&mut self, index: usize, open: bool) {
        let Some((word, mask)) = box_prop_state_bit::<MAX_BOX_PROP_STATE>(index) else {
            return;
        };
        if open {
            self.door_open[word] |= mask;
        } else {
            self.door_open[word] &= !mask;
        }
    }

    /// True when box `index` is hidden by an open door.
    pub fn is_door_open(&self, index: usize) -> bool {
        let Some((word, mask)) = box_prop_state_bit::<MAX_BOX_PROP_STATE>(index) else {
            return false;
        };
        self.door_open[word] & mask != 0
    }

    /// Rebuild the static derived per-box data from the cooked records.
    pub fn rebuild(&mut self, props: &'static [LevelBoxPropRecord]) {
        self.runtime = [BoxPropRuntime::EMPTY; MAX_BOX_PROP_STATE];
        self.fall = [BoxPropFallState::EMPTY; MAX_BOX_PROP_STATE];
        for (index, prop) in props.iter().enumerate() {
            if index >= self.runtime.len() {
                break;
            }
            self.runtime[index] = geometry::build_box_prop_runtime(prop);
        }
    }

    fn is_box_prop_broken(&self, index: usize) -> bool {
        let Some((word, mask)) = box_prop_state_bit::<MAX_BOX_PROP_STATE>(index) else {
            return false;
        };
        self.broken[word] & mask != 0
    }

    fn mark_box_prop_broken(
        &mut self,
        props: &'static [LevelBoxPropRecord],
        current_room: RoomIndex,
        index: usize,
        impulse_x_q8: i16,
        impulse_z_q8: i16,
    ) -> bool {
        let Some((word, mask)) = box_prop_state_bit::<MAX_BOX_PROP_STATE>(index) else {
            return false;
        };
        if self.broken[word] & mask != 0 {
            return false;
        }
        self.broken[word] |= mask;
        self.spawn_box_prop_break_event(index, impulse_x_q8, impulse_z_q8);
        // Anything that was resting on this box (or on a box this one was
        // holding up) has lost its support: let it fall and break in turn.
        self.start_unsupported_box_falls(props, current_room);
        true
    }

    fn spawn_box_prop_break_event(&mut self, index: usize, impulse_x_q8: i16, impulse_z_q8: i16) {
        let prop_index = index.min(u16::MAX as usize) as u16;
        let ground_y = self
            .runtime
            .get(index)
            .map_or(i32::MIN, |runtime| runtime.ground_y);
        // Spawn shards from wherever the box actually is: a box that fell
        // and broke on impact carries its landed downward offset.
        let y_offset = self.fall.get(index).map_or(0, |fall| fall.fall_y);
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
        for (slot, event) in self.break_events.iter().enumerate() {
            if !event.is_active() {
                self.break_events[slot] = replacement;
                return;
            }
            if event.age >= oldest_age {
                oldest_age = event.age;
                target = slot;
            }
        }
        self.break_events[target] = replacement;
    }

    /// Age the live break bursts, retiring finished ones.
    pub fn advance_break_events(&mut self, delta_vblanks: u16) {
        let step = delta_vblanks.max(1).min(u8::MAX as u16) as u8;
        for event in &mut self.break_events {
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
    fn start_unsupported_box_falls(
        &mut self,
        props: &'static [LevelBoxPropRecord],
        current_room: RoomIndex,
    ) {
        let count = props.len().min(self.runtime.len());
        loop {
            let mut changed = false;
            for index in 0..count {
                // Only the active room's boxes are live; never start a fall
                // in an unloaded room from a break over here.
                if props[index].room != current_room {
                    continue;
                }
                if self.is_box_prop_broken(index) || self.fall[index].falling {
                    continue;
                }
                // A door-linked box is anchored architecture: it never
                // falls, whatever happened beneath it.
                if self.is_door_open(index) {
                    continue;
                }
                if self.box_prop_supported(props, index, count) {
                    continue;
                }
                self.fall[index].falling = true;
                self.fall[index].vel = 0;
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    /// True if box `index` rests on the room floor or on a still-standing
    /// (unbroken, not-falling) box directly beneath it.
    fn box_prop_supported(
        &self,
        props: &'static [LevelBoxPropRecord],
        index: usize,
        count: usize,
    ) -> bool {
        let runtime = self.runtime[index];
        // Grounded: its bottom sits at (or below) the baked room floor.
        if runtime.aabb_min.y <= runtime.ground_y.saturating_add(BOX_PROP_SUPPORT_TOLERANCE) {
            return true;
        }
        let room = props[index].room;
        // `other` indexes props, self.fall and the broken/door predicates,
        // so iterating one of them still leaves the rest indexed.
        #[allow(clippy::needless_range_loop)]
        for other in 0..count {
            if other == index
                || props[other].room != room
                || self.is_box_prop_broken(other)
                || self.is_door_open(other)
                || self.fall[other].falling
            {
                continue;
            }
            let support = self.runtime[other];
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
    pub fn advance_falls(
        &mut self,
        props: &'static [LevelBoxPropRecord],
        current_room: RoomIndex,
        delta_vblanks: u16,
    ) {
        let step = delta_vblanks.max(1) as i32;
        let count = props.len().min(self.runtime.len());
        for index in 0..count {
            if !self.fall[index].falling || self.is_box_prop_broken(index) {
                continue;
            }
            let runtime = self.runtime[index];
            let mut fall = self.fall[index];
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
                self.fall[index] = fall;
                self.mark_box_prop_broken(props, current_room, index, 0, 0);
            } else {
                self.fall[index] = fall;
            }
        }
    }

    /// Break the boxes the player's movement probe overlaps.
    pub fn break_for_movement(
        &mut self,
        props: &'static [LevelBoxPropRecord],
        current_room: RoomIndex,
        current: RoomPoint,
        yaw: Angle,
        trigger: u16,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) {
        let target =
            box_prop_movement_probe_target(current, yaw, input, config, trigger, delta_vblanks);
        for (index, prop) in props.iter().enumerate() {
            if prop.room != current_room
                || prop.flags & trigger == 0
                || self.is_box_prop_broken(index)
                || self.is_door_open(index)
            {
                continue;
            }
            let Some(box_runtime) = self.runtime.get(index) else {
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
                self.mark_box_prop_broken(props, current_room, index, impulse_x_q8, impulse_z_q8);
            }
        }
    }

    /// Break the boxes inside the player's attack volume.
    pub fn break_for_attack(
        &mut self,
        props: &'static [LevelBoxPropRecord],
        current_room: RoomIndex,
        origin: RoomPoint,
        yaw: Angle,
        config: CharacterMotorConfig,
    ) {
        for (index, prop) in props.iter().enumerate() {
            if prop.room != current_room
                || prop.flags & box_prop_flags::BREAK_ON_ATTACK == 0
                || self.is_box_prop_broken(index)
                || self.is_door_open(index)
            {
                continue;
            }
            let Some(box_runtime) = self.runtime.get(index) else {
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
                self.mark_box_prop_broken(props, current_room, index, impulse.0, impulse.1);
            }
        }
    }

    /// Collect the unbroken, door-closed collision-enabled boxes of
    /// `current_room` as AABB blockers. Returns the filled count.
    pub fn collect_collision_blockers(
        &self,
        props: &'static [LevelBoxPropRecord],
        current_room: RoomIndex,
        out: &mut [CharacterCollisionAabb],
    ) -> usize {
        let mut count = 0usize;
        for (index, prop) in props.iter().enumerate() {
            if prop.room != current_room
                || prop.flags & box_prop_flags::COLLISION_ENABLED == 0
                || self.is_box_prop_broken(index)
                || self.is_door_open(index)
                || count >= out.len()
            {
                continue;
            }
            let Some(box_runtime) = self.runtime.get(index) else {
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

fn box_prop_state_bit<const MAX_BOX_PROP_STATE: usize>(index: usize) -> Option<(usize, u32)> {
    if index >= MAX_BOX_PROP_STATE {
        return None;
    }
    Some((index / 32, 1u32 << (index % 32)))
}

fn box_prop_broken_in_words<const MAX_BOX_PROP_STATE: usize>(broken: &[u32], index: usize) -> bool {
    let Some((word, mask)) = box_prop_state_bit::<MAX_BOX_PROP_STATE>(index) else {
        return false;
    };
    broken[word] & mask != 0
}

/// The box-break trigger flag for the current movement input, if any.
pub fn box_prop_movement_break_trigger(
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
    let speed = base_speed.saturating_mul(delta_vblanks.clamp(1, 4) as i32);
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
    let denom = abs_i32(dx).saturating_add(abs_i32(dz));
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
    forward >= -prop_extent && forward <= reach && abs_i32(lateral) <= half_width
}
