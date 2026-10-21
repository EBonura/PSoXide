//! The binary form of the records one level owns.
//!
//! A level's placed things (rooms' atmosphere, lights, props, entities, logic,
//! interactables, the player's spawn) are not part of the executable: the cook
//! writes them as one chunk of `WORLD.PAK` and the runtime reads and decodes
//! that chunk at boot. Project-wide data (characters, models, UI, game flow)
//! stays in the generated manifest.
//!
//! The format is plain little-endian fields in declaration order, no padding:
//! `u8`/`u16`/`i16`/`u32`/`i32`, `bool` as one byte, arrays and tuples as
//! their elements, `Option` as a tag byte then the value, a `&str` as a `u32`
//! length then UTF-8 bytes (decoded in place, no copy), and a slice as a `u32`
//! count then the elements (decoded into a leaked allocation). A table is a
//! `u32` count then its records. Structs list every field (the macro
//! destructures without `..`, so a field added to a record and not to the list
//! below does not compile).
//!
//! The chunk begins with `PXLR` and a version, so a chunk and a runtime that
//! disagree are refused instead of misread.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::mem::MaybeUninit;

use crate::*;

/// What a chunk begins with.
pub const MAGIC: [u8; 4] = *b"PXLR";
/// Version of the layout below. Bump it whenever a record or the table list
/// changes.
pub const VERSION: u16 = 2;

/// A cursor over the chunk's bytes. The bytes live for the whole run, so a
/// string decoded from them can be `&'static`.
pub struct Reader {
    bytes: &'static [u8],
    pos: usize,
}

impl Reader {
    /// A reader at the start of `bytes`.
    pub const fn new(bytes: &'static [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Bytes not yet read.
    pub const fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, count: usize) -> Option<&'static [u8]> {
        let end = self.pos.checked_add(count)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
}

/// A value with a binary form.
pub trait Wire: Sized {
    /// Append the value's bytes.
    fn put(&self, out: &mut Vec<u8>);
    /// Read a value, or `None` when the bytes run out or are not a value.
    fn get(r: &mut Reader) -> Option<Self>;
}

macro_rules! wire_int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn get(r: &mut Reader) -> Option<Self> {
                let bytes = r.take(core::mem::size_of::<$t>())?;
                Some(<$t>::from_le_bytes(bytes.try_into().ok()?))
            }
        }
    )*};
}
wire_int!(u8, u16, i16, u32, i32);

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    fn get(r: &mut Reader) -> Option<Self> {
        Some(u8::get(r)? != 0)
    }
}

impl<T: Wire, const N: usize> Wire for [T; N] {
    fn put(&self, out: &mut Vec<u8>) {
        for item in self {
            item.put(out);
        }
    }
    fn get(r: &mut Reader) -> Option<Self> {
        let mut out: [MaybeUninit<T>; N] = [const { MaybeUninit::uninit() }; N];
        for slot in 0..N {
            match T::get(r) {
                Some(value) => {
                    out[slot].write(value);
                }
                None => {
                    // Drop what was built; the records are plain data, so this
                    // only matters to keep the function honest.
                    for built in &mut out[..slot] {
                        // SAFETY: the first `slot` entries were written.
                        unsafe { built.assume_init_drop() };
                    }
                    return None;
                }
            }
        }
        // SAFETY: every entry was written above.
        Some(out.map(|value| unsafe { value.assume_init() }))
    }
}

macro_rules! wire_tuple {
    ($(($($name:ident),+)),*) => {$(
        impl<$($name: Wire),+> Wire for ($($name,)+) {
            #[allow(non_snake_case)]
            fn put(&self, out: &mut Vec<u8>) {
                let ($($name,)+) = self;
                $($name.put(out);)+
            }
            fn get(r: &mut Reader) -> Option<Self> {
                Some(($($name::get(r)?,)+))
            }
        }
    )*};
}
wire_tuple!((A, B), (A, B, C));

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                value.put(out);
            }
        }
    }
    fn get(r: &mut Reader) -> Option<Self> {
        match u8::get(r)? {
            0 => Some(None),
            _ => Some(Some(T::get(r)?)),
        }
    }
}

impl Wire for &'static str {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u32).put(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader) -> Option<Self> {
        let len = u32::get(r)? as usize;
        core::str::from_utf8(r.take(len)?).ok()
    }
}

impl<T: Wire + 'static> Wire for &'static [T] {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u32).put(out);
        for item in self.iter() {
            item.put(out);
        }
    }
    fn get(r: &mut Reader) -> Option<Self> {
        decode_slice(r)
    }
}

/// A table: a count, then the records, decoded into an allocation that lives
/// for the rest of the run.
pub fn decode_slice<T: Wire>(r: &mut Reader) -> Option<&'static [T]> {
    let count = u32::get(r)? as usize;
    // Every element takes at least one byte; a count the bytes cannot hold is
    // damage, not a reason to allocate.
    if count > r.remaining() {
        return None;
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(T::get(r)?);
    }
    Some(Box::leak(items.into_boxed_slice()))
}

/// Append a table: its count, then the records.
pub fn put_slice<T: Wire>(items: &[T], out: &mut Vec<u8>) {
    (items.len() as u32).put(out);
    for item in items {
        item.put(out);
    }
}

macro_rules! wire_newtype {
    ($($t:ident($inner:ty)),*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                self.0.put(out);
            }
            fn get(r: &mut Reader) -> Option<Self> {
                Some($t(<$inner>::get(r)?))
            }
        }
    )*};
}
wire_newtype!(
    AssetId(u16),
    RoomIndex(u16),
    ModelIndex(u16),
    OptionalModelClipIndex(u16),
    CharacterIndex(u16),
    CombatCapsuleIndex(u16),
    ResourceSlot(u16),
    StableId(u32)
);

macro_rules! wire_struct {
    ($($t:ident { $($f:ident),* $(,)? })*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                let $t { $($f,)* } = self;
                $($f.put(out);)*
            }
            fn get(r: &mut Reader) -> Option<Self> {
                Some($t { $($f: Wire::get(r)?,)* })
            }
        }
    )*};
}

impl Wire for EntityKind {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(match self {
            EntityKind::HookPoint => 0,
            EntityKind::Marker => 1,
            EntityKind::StaticMesh => 2,
        });
    }
    fn get(r: &mut Reader) -> Option<Self> {
        match u8::get(r)? {
            0 => Some(EntityKind::HookPoint),
            1 => Some(EntityKind::Marker),
            2 => Some(EntityKind::StaticMesh),
            _ => None,
        }
    }
}

impl Wire for InteractableKind {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(match self {
            InteractableKind::Message => 0,
            InteractableKind::Checkpoint => 1,
            InteractableKind::PointOfInterest => 2,
        });
    }
    fn get(r: &mut Reader) -> Option<Self> {
        match u8::get(r)? {
            0 => Some(InteractableKind::Message),
            1 => Some(InteractableKind::Checkpoint),
            2 => Some(InteractableKind::PointOfInterest),
            _ => None,
        }
    }
}

wire_struct! {
    LevelCycloramaQuadRecord { direction_q12, rgb, flags }
    LevelCloudLayerRecord {
        texture_asset, color_rgb, density, altitude, extent, tile_count, scroll_speed, noise_seed, flags
    }
    LevelSkyRecord {
        top_rgb, horizon_rgb, bottom_rgb, horizon_percent, horizon_thickness_percent,
        skybox_columns, skybox_rows, flags, texture_asset, cyclorama_quads, cloud_layer
    }
    LevelCameraProfile { distance, height, target_height, shoulder_offset, fov_y_degrees }
    LevelCameraRecord {
        distance, height, target_height, lock_rise_percent, min_floor_clearance, orbit_speed_level,
        accelerated_orbit, recenter_preserves_pitch, fov_y_degrees, blend_profiles,
        lock_target_framing, lock_profile, position_lag_shift, position_vertical_lag_shift,
        focus_lag_shift, focus_vertical_lag_shift, distance_lag_shift
    }
    LevelRoomRecord {
        name, sector_size, draw_distance, gravity_per_tick_q8, fog_rgb, fog_near, fog_far,
        atmosphere_rgb, atmosphere_density, atmosphere_fall_speed_q4, atmosphere_wind_speed_q4,
        sky, camera, flags
    }
    PointLightRecord { room, x, y, z, radius, intensity_q8, color, flags }
    LevelWorldObjectRecord {
        room, kind, flags, source_index, destructible, bounds_min, bounds_max
    }
    LevelImagePropRecord {
        room, texture_asset, x, y, z, pitch, yaw, roll, width, height, tint_rgb, baked_vertex_rgb,
        collision_min, collision_max, flags
    }
    LevelBoxPropRecord {
        room, texture_assets, blend_modes, uvs, x, y, z, ground_y, pitch, yaw, roll, vertices,
        collision_min, collision_max, surface_first, surface_count, tint_rgb, baked_vertex_rgb, flags
    }
    LevelBoxPropSurfaceRecord {
        vertices, center, normal, uv_q8, baked_vertex_rgb, source_face, flags
    }
    LevelCylinderPropRecord {
        room, texture_assets, blend_modes, uvs, surface_first, surface_count, center, cull_radius,
        bounds_min, bounds_max, flags
    }
    LevelCylinderPropSurfaceRecord {
        vertices, center, normal, uv_q8, baked_vertex_rgb, material_slot, vertex_count
    }
    LevelArchPropRecord {
        room, texture_assets, blend_modes, uvs, surface_first, surface_count, collision_first,
        collision_count, center, cull_radius, flags
    }
    LevelArchPropSurfaceRecord { vertices, center, normal, uv_q8, baked_vertex_rgb, material_slot }
    LevelArchPropCollisionRecord { min, max }
    LevelDestructibleRecord { max_health, persistent_flag, damage_affinity, flags }
    ParticleEmitterRecord {
        room, x, y, z, max_particles, spawn_rate_q8, lifetime_frames, start_size, end_size,
        start_color, end_color, blend_mode, base_velocity_q4, random_velocity_q4, acceleration_q4,
        spawn_radius, flags
    }
    LevelMaterialUvMotion { enabled, speed_u_q8, speed_v_q8, phase_u, phase_v }
    LevelModelSecondaryLayer { texture_asset, blend_mode, tint_rgb, motion, flags }
    LevelModelMaterialOverride { texture_asset, blend_mode, tint_rgb, motion, secondary_layer, flags }
    LevelModelInstanceRecord {
        room, model, clip, pose_frame, x, y, z, yaw, visual_yaw, pitch, roll, visual_offset,
        visual_scale_q8, material_override, flags
    }
    InteractableRecord {
        room, kind, x, y, z, yaw, radius, marker_height, prompt, message, logic, checkpoint_id,
        read_flag, reward_flag, reward_resource, reward_quantity, flags
    }
    InteractableMessageRecord { title, body, page_first, page_count }
    LevelVitalityCircleRecord {
        room, x, y, z, radius, axis, refill_per_second, drain_per_second
    }
    EntityRecord { room, kind, x, y, z, yaw, resource_slot, flags }
    PlayerSpawnRecord { room, x, y, z, yaw, flags }
    PlayerControllerRecord { spawn, character, flags }
    LevelLogicRecord {
        room, kind, spawnflags, targetname, target, killtarget, master, delay_ticks, wait_ticks,
        arg0, arg1, link, message, x, y, z, min, max, flags
    }
    CharacterActionFrameRange { start, end }
    LevelGameEntityRecord {
        room, kind, targetname, model_instance, idle_clip, alert_clip, turn_clip, walk_clip,
        walk_backward_clip, strafe_left_clip, strafe_right_clip, run_clip, attack_clip,
        attack_speed_q8, attack_frame_range, heavy_attack_clip, heavy_attack_speed_q8,
        heavy_attack_frame_range, ranged_attack_clip, ranged_attack_speed_q8,
        ranged_attack_frame_range, stagger_clip, stagger_speed_q8, stagger_frame_range,
        stagger_ticks, death_clip, combat_capsule_first, combat_capsule_count,
        ranged_attack_action, x, y, z, yaw, radius, height, walk_speed, run_speed, patrol_x,
        patrol_y, patrol_z, patrol_wait_ticks, aggro_radius, reaction_ticks, preferred_distance,
        spacing_tolerance, spacing_speed_percent, decision_interval_ticks, circle_chance,
        attack_priority, attack_cooldown_ticks, group_attack_delay_ticks, windup_ticks,
        attack_active_ticks, heavy_attack_active_ticks, ranged_attack_active_ticks, recovery_ticks,
        attack_min_range, attack_max_range, poise, touch_damage, max_health, max_health_secondary,
        soul_value, flags
    }
}

macro_rules! level_tables {
    ($(table $tf:ident: $tt:ty;)* $(one $of:ident: $ot:ty;)*) => {
        /// Every record one level owns.
        #[derive(Debug, Clone, Copy)]
        pub struct LevelRecords<'a> {
            $(#[doc = concat!("`", stringify!($tt), "` records.")] pub $tf: &'a [$tt],)*
            $(#[doc = concat!("A `", stringify!($ot), "`.")] pub $of: $ot,)*
        }

        /// The names of the tables, in chunk order.
        pub const TABLE_NAMES: &[&str] = &[$(stringify!($tf)),*];

        impl LevelRecords<'_> {
            /// How many records each table holds, by [`TABLE_NAMES`] order.
            pub fn table_lens(&self) -> Vec<usize> {
                alloc::vec![$(self.$tf.len()),*]
            }

            /// The chunk bytes of these records.
            pub fn encode(&self) -> Vec<u8> {
                let mut out = Vec::new();
                out.extend_from_slice(&MAGIC);
                VERSION.put(&mut out);
                $(put_slice(self.$tf, &mut out);)*
                $(self.$of.put(&mut out);)*
                out
            }
        }

        impl LevelRecords<'static> {
            /// Decode a chunk. `None` for a chunk of another version, a
            /// truncated one, or one with bytes left over.
            pub fn decode(bytes: &'static [u8]) -> Option<Self> {
                let mut r = Reader::new(bytes);
                if r.take(4)? != MAGIC || u16::get(&mut r)? != VERSION {
                    return None;
                }
                let records = Self {
                    $($tf: decode_slice(&mut r)?,)*
                    $($of: Wire::get(&mut r)?,)*
                };
                (r.remaining() == 0).then_some(records)
            }
        }
    };
}

level_tables! {
    table rooms: LevelRoomRecord;
    table room_reflection_probes: Option<AssetId>;
    table lights: PointLightRecord;
    table particle_emitters: ParticleEmitterRecord;
    table interactable_messages: InteractableMessageRecord;
    table interactable_message_pages: &'static str;
    table interactable_message_pages_it: &'static str;
    table interactables: InteractableRecord;
    table vitality_circles: LevelVitalityCircleRecord;
    table logic: LevelLogicRecord;
    table logic_ids: StableId;
    table game_entities: LevelGameEntityRecord;
    table game_entity_ids: StableId;
    table entities: EntityRecord;
    table destructibles: LevelDestructibleRecord;
    table world_objects: LevelWorldObjectRecord;
    table image_props: LevelImagePropRecord;
    table box_props: LevelBoxPropRecord;
    table box_prop_surfaces: LevelBoxPropSurfaceRecord;
    table cylinder_props: LevelCylinderPropRecord;
    table cylinder_prop_surfaces: LevelCylinderPropSurfaceRecord;
    table arch_props: LevelArchPropRecord;
    table arch_prop_surfaces: LevelArchPropSurfaceRecord;
    table arch_prop_collisions: LevelArchPropCollisionRecord;
    table model_instances: LevelModelInstanceRecord;
    one player_spawn: PlayerSpawnRecord;
    one player_controller: Option<PlayerControllerRecord>;
    one world_message: Option<InteractableMessageRecord>;
    one world_ambient_rgb: [u8; 3];
}

/// Runtime pool caps a level's records are checked against by the cook. The
/// runtime sizes its fixed arrays by these, not by what the level happens to
/// hold, so the executable is the same for every level; a level over a cap is
/// a cook error naming the cap.
pub mod caps {
    /// Placed enemies and training actors (see [`crate::MAX_GAME_ENTITY_RECORDS`]).
    pub const GAME_ENTITIES: usize = crate::MAX_GAME_ENTITY_RECORDS;
    /// Logic records (see [`crate::MAX_LOGIC_RECORDS`]).
    pub const LOGIC: usize = crate::MAX_LOGIC_RECORDS;
    /// Placed model instances (enemies' bodies and decoration).
    pub const MODEL_INSTANCES: usize = 64;
    /// Interactables (messages, checkpoints, points of interest).
    pub const INTERACTABLES: usize = 32;
    /// Authored box props. Each costs about 1.4 KB of runtime state.
    pub const BOX_PROPS: usize = 8;
    /// Rooms of the level.
    pub const ROOMS: usize = 65_534;
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    /// A tiny deterministic generator.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u8 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            let byte = (self.0 >> 24) as u8;
            // Small values are what flags, tags and kinds are made of.
            if self.0 & 3 == 0 {
                byte & 3
            } else {
                byte
            }
        }
    }

    fn leak(bytes: Vec<u8>) -> &'static [u8] {
        Box::leak(bytes.into_boxed_slice())
    }

    /// Decode random bytes as `T`; whatever decodes must encode to bytes that
    /// decode to the same value and read exactly what was written.
    fn round_trip<T: Wire + PartialEq + core::fmt::Debug>(name: &str, seed: u64) {
        let mut rng = Rng(seed);
        let mut accepted = 0;
        for _ in 0..4000 {
            let bytes = leak((0..1024).map(|_| rng.next()).collect());
            let mut r = Reader::new(bytes);
            let Some(value) = T::get(&mut r) else {
                continue;
            };
            let used = bytes.len() - r.remaining();
            let mut out = Vec::new();
            value.put(&mut out);
            assert_eq!(out.len(), used, "{name}: encoded length");
            let again = leak(out);
            let mut r2 = Reader::new(again);
            let decoded = T::get(&mut r2).unwrap_or_else(|| panic!("{name}: re-decode"));
            assert_eq!(r2.remaining(), 0, "{name}: bytes left");
            assert_eq!(decoded, value, "{name}");
            accepted += 1;
        }
        assert!(
            accepted > 20,
            "{name}: only {accepted} random inputs decoded"
        );
    }

    macro_rules! round_trips {
        ($($t:ty),* $(,)?) => {$(
            round_trip::<$t>(stringify!($t), 0x9e37_79b9_7f4a_7c15);
        )*};
    }

    #[test]
    fn every_record_survives_encode_and_decode() {
        round_trips!(
            PointLightRecord,
            LevelWorldObjectRecord,
            LevelImagePropRecord,
            LevelBoxPropRecord,
            LevelBoxPropSurfaceRecord,
            LevelCylinderPropRecord,
            LevelCylinderPropSurfaceRecord,
            LevelArchPropRecord,
            LevelArchPropSurfaceRecord,
            LevelArchPropCollisionRecord,
            LevelDestructibleRecord,
            ParticleEmitterRecord,
            LevelModelInstanceRecord,
            LevelVitalityCircleRecord,
            EntityRecord,
            PlayerSpawnRecord,
            PlayerControllerRecord,
            LevelLogicRecord,
            LevelGameEntityRecord,
            Option<AssetId>,
            StableId,
        );
    }

    #[test]
    fn a_chunk_round_trips_and_refuses_damage() {
        let empty_room = LevelRoomRecord {
            name: "Hall",
            sector_size: 1024,
            draw_distance: 4096,
            gravity_per_tick_q8: 32,
            fog_rgb: [1, 2, 3],
            fog_near: 0,
            fog_far: 4096,
            atmosphere_rgb: [0; 3],
            atmosphere_density: 0,
            atmosphere_fall_speed_q4: 0,
            atmosphere_wind_speed_q4: 0,
            sky: LevelSkyRecord {
                top_rgb: [1; 3],
                horizon_rgb: [2; 3],
                bottom_rgb: [3; 3],
                horizon_percent: 50,
                horizon_thickness_percent: 8,
                skybox_columns: 16,
                skybox_rows: 10,
                flags: 3,
                texture_asset: AssetId(4),
                cyclorama_quads: &[],
                cloud_layer: LevelCloudLayerRecord {
                    texture_asset: AssetId(0),
                    color_rgb: [9; 3],
                    density: 1,
                    altitude: 2,
                    extent: 3,
                    tile_count: 4,
                    scroll_speed: [5, -6],
                    noise_seed: 7,
                    flags: 0,
                },
            },
            camera: LevelCameraRecord {
                distance: 2000,
                height: 1000,
                target_height: 850,
                lock_rise_percent: 25,
                min_floor_clearance: 64,
                orbit_speed_level: 5,
                accelerated_orbit: false,
                recenter_preserves_pitch: true,
                fov_y_degrees: 0,
                blend_profiles: false,
                lock_target_framing: false,
                lock_profile: None,
                position_lag_shift: 2,
                position_vertical_lag_shift: 2,
                focus_lag_shift: 2,
                focus_vertical_lag_shift: 2,
                distance_lag_shift: 3,
            },
            flags: 0,
        };
        let rooms = [empty_room];
        let pages = ["one", "two"];
        let messages = [InteractableMessageRecord {
            title: "Notice",
            body: "Mind the step",
            page_first: 0,
            page_count: 2,
        }];
        let interactables = [InteractableRecord {
            room: RoomIndex(0),
            kind: InteractableKind::PointOfInterest,
            x: 1,
            y: -2,
            z: 3,
            yaw: 4,
            radius: 5,
            marker_height: 6,
            prompt: "Read",
            message: 0,
            logic: 7,
            checkpoint_id: "cp-1",
            read_flag: 8,
            reward_flag: 9,
            reward_resource: 10,
            reward_quantity: 11,
            flags: 1,
        }];
        let spawn = PlayerSpawnRecord {
            room: RoomIndex(0),
            x: 40,
            y: 4,
            z: 200,
            yaw: 0,
            flags: 1,
        };
        let records = LevelRecords {
            rooms: &rooms,
            room_reflection_probes: &[None],
            lights: &[],
            particle_emitters: &[],
            interactable_messages: &messages,
            interactable_message_pages: &pages,
            interactable_message_pages_it: &[""],
            interactables: &interactables,
            vitality_circles: &[],
            logic: &[],
            logic_ids: &[StableId(5)],
            game_entities: &[],
            game_entity_ids: &[],
            entities: &[],
            destructibles: &[],
            world_objects: &[],
            image_props: &[],
            box_props: &[],
            box_prop_surfaces: &[],
            cylinder_props: &[],
            cylinder_prop_surfaces: &[],
            arch_props: &[],
            arch_prop_surfaces: &[],
            arch_prop_collisions: &[],
            model_instances: &[],
            player_spawn: spawn,
            player_controller: None,
            world_message: None,
            world_ambient_rgb: [32; 3],
        };
        let bytes = records.encode();
        assert_eq!(&bytes[..4], b"PXLR");
        let decoded = LevelRecords::decode(leak(bytes.clone())).expect("decodes");
        assert_eq!(decoded.rooms, &rooms);
        assert_eq!(decoded.interactables, &interactables);
        assert_eq!(decoded.interactable_messages, &messages);
        assert_eq!(decoded.interactable_message_pages, &pages);
        assert_eq!(decoded.interactable_message_pages_it, &[""]);
        assert_eq!(decoded.logic_ids, &[StableId(5)]);
        assert_eq!(decoded.player_spawn, spawn);
        assert_eq!(decoded.world_ambient_rgb, [32; 3]);
        assert_eq!(decoded.encode(), bytes, "canonical");

        // Damage is refused, never misread.
        assert!(LevelRecords::decode(leak(bytes[..bytes.len() - 1].to_vec())).is_none());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(LevelRecords::decode(leak(longer)).is_none());
        let mut wrong_version = bytes.clone();
        wrong_version[4] = 9;
        assert!(LevelRecords::decode(leak(wrong_version)).is_none());
        let mut wrong_magic = bytes;
        wrong_magic[0] = b'X';
        assert!(LevelRecords::decode(leak(wrong_magic)).is_none());
        // A count the bytes cannot hold does not allocate.
        let mut huge = Vec::new();
        huge.extend_from_slice(&MAGIC);
        VERSION.put(&mut huge);
        u32::MAX.put(&mut huge);
        assert!(LevelRecords::decode(leak(huge)).is_none());
    }
}
