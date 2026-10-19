//! The records the loaded level owns: rooms' atmosphere and camera, lights,
//! props, interactables, logic, enemies, the player's spawn.
//!
//! They are not part of the executable. The cook writes them as chunk 1 of
//! `WORLD.PAK`; `GridRuntime::load_manifest` reads and decodes the chunk at
//! boot and [`install`]s the tables here, once, before anything reads them.
//! The names are the ones the generated manifest used to define, so the code
//! that reads a level's records reads them the way it always did; the runtime's
//! own arrays are sized by the pool caps in `psx_level::wire::caps`, not by
//! what a level happens to hold.

use core::cell::UnsafeCell;
use core::ops::Deref;

use psx_level::wire::LevelRecords;
use psx_level::{
    AssetId, EntityRecord, InteractableMessageRecord, InteractableRecord,
    LevelArchPropCollisionRecord, LevelArchPropRecord, LevelArchPropSurfaceRecord,
    LevelBoxPropRecord, LevelBoxPropSurfaceRecord, LevelCylinderPropRecord,
    LevelCylinderPropSurfaceRecord, LevelDestructibleRecord, LevelGameEntityRecord,
    LevelImagePropRecord, LevelLogicRecord, LevelModelInstanceRecord, LevelRoomRecord,
    LevelVitalityCircleRecord, LevelWorldObjectRecord, ParticleEmitterRecord,
    PlayerControllerRecord, PlayerSpawnRecord, PointLightRecord, StableId,
};

/// A table of records installed once at boot. Reads as the `&[T]` it holds.
pub struct LevelTable<T: 'static>(UnsafeCell<&'static [T]>);

// SAFETY: the guest is single threaded and a table is written once, at boot,
// before any reader runs. (Host tests install behind a `Once`.)
unsafe impl<T> Sync for LevelTable<T> {}

impl<T: 'static> LevelTable<T> {
    const fn new() -> Self {
        Self(UnsafeCell::new(&[]))
    }

    fn set(&self, table: &'static [T]) {
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() = table };
    }

    /// The table, for the code that wants the `&'static [T]` itself.
    pub fn as_slice(&self) -> &'static [T] {
        #[cfg(test)]
        fixture::ensure();
        // SAFETY: see the `Sync` impl.
        unsafe { *self.0.get() }
    }
}

impl<T: 'static> Deref for LevelTable<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<'a, T: 'static> IntoIterator for &'a LevelTable<T> {
    type Item = &'static T;
    type IntoIter = core::slice::Iter<'static, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// A single installed value.
pub struct LevelValue<T: Copy>(UnsafeCell<T>);

// SAFETY: as for `LevelTable`.
unsafe impl<T: Copy> Sync for LevelValue<T> {}

impl<T: Copy> LevelValue<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    fn set(&self, value: T) {
        // SAFETY: see the `Sync` impl of `LevelTable`.
        unsafe { *self.0.get() = value };
    }

    /// The value.
    pub fn get(&self) -> T {
        #[cfg(test)]
        fixture::ensure();
        // SAFETY: as above.
        unsafe { *self.0.get() }
    }
}

macro_rules! level_statics {
    ($(table $name:ident: $field:ident: $ty:ty;)* $(value $vname:ident: $vfield:ident: $vty:ty = $init:expr;)*) => {
        $(
            #[doc = concat!("The level's `", stringify!($field), "` table.")]
            pub static $name: LevelTable<$ty> = LevelTable::new();
        )*
        $(
            #[doc = concat!("The level's `", stringify!($vfield), "`.")]
            pub static $vname: LevelValue<$vty> = LevelValue::new($init);
        )*

        /// Install a decoded level's records. Call once, at boot, before any
        /// reader.
        pub fn install(records: LevelRecords<'static>) {
            $($name.set(records.$field);)*
            $($vname.set(records.$vfield);)*
        }
    };
}

level_statics! {
    table ROOMS: rooms: LevelRoomRecord;
    table ROOM_REFLECTION_PROBES: room_reflection_probes: Option<AssetId>;
    table LIGHTS: lights: PointLightRecord;
    table PARTICLE_EMITTERS: particle_emitters: ParticleEmitterRecord;
    table INTERACTABLE_MESSAGES: interactable_messages: InteractableMessageRecord;
    table INTERACTABLE_MESSAGE_PAGES: interactable_message_pages: &'static str;
    table INTERACTABLE_MESSAGE_PAGES_IT: interactable_message_pages_it: &'static str;
    table INTERACTABLES: interactables: InteractableRecord;
    table VITALITY_CIRCLES: vitality_circles: LevelVitalityCircleRecord;
    table LOGIC: logic: LevelLogicRecord;
    table LOGIC_IDS: logic_ids: StableId;
    table GAME_ENTITIES: game_entities: LevelGameEntityRecord;
    table GAME_ENTITY_IDS: game_entity_ids: StableId;
    table ENTITIES: entities: EntityRecord;
    table DESTRUCTIBLES: destructibles: LevelDestructibleRecord;
    table WORLD_OBJECTS: world_objects: LevelWorldObjectRecord;
    table IMAGE_PROPS: image_props: LevelImagePropRecord;
    table BOX_PROPS: box_props: LevelBoxPropRecord;
    table BOX_PROP_SURFACES: box_prop_surfaces: LevelBoxPropSurfaceRecord;
    table CYLINDER_PROPS: cylinder_props: LevelCylinderPropRecord;
    table CYLINDER_PROP_SURFACES: cylinder_prop_surfaces: LevelCylinderPropSurfaceRecord;
    table ARCH_PROPS: arch_props: LevelArchPropRecord;
    table ARCH_PROP_SURFACES: arch_prop_surfaces: LevelArchPropSurfaceRecord;
    table ARCH_PROP_COLLISIONS: arch_prop_collisions: LevelArchPropCollisionRecord;
    table MODEL_INSTANCES: model_instances: LevelModelInstanceRecord;
    value PLAYER_SPAWN: player_spawn: PlayerSpawnRecord = PlayerSpawnRecord {
        room: psx_level::RoomIndex(0),
        x: 0,
        y: 0,
        z: 0,
        yaw: 0,
        flags: 0,
    };
    value PLAYER_CONTROLLER: player_controller: Option<PlayerControllerRecord> = None;
    value WORLD_MESSAGE: world_message: Option<InteractableMessageRecord> = None;
    value WORLD_AMBIENT_RGB: world_ambient_rgb: [u8; 3] = [0; 3];
}

/// The host tests read a fixture level instead of a disc: the first read of
/// any table installs it.
#[cfg(test)]
mod fixture {
    #![allow(missing_docs, dead_code, unused_imports)]
    use psx_level::wire::LevelRecords;
    use psx_level::*;

    include!("../test-fixtures/level_records.rs");

    static INSTALLED: std::sync::Once = std::sync::Once::new();

    pub(super) fn ensure() {
        INSTALLED.call_once(|| {
            super::install(LevelRecords {
                rooms: ROOMS,
                room_reflection_probes: &[],
                lights: &[],
                particle_emitters: &[],
                interactable_messages: &[],
                interactable_message_pages: INTERACTABLE_MESSAGE_PAGES,
                interactable_message_pages_it: INTERACTABLE_MESSAGE_PAGES_IT,
                interactables: &[],
                vitality_circles: &[],
                logic: &[],
                logic_ids: &[],
                game_entities: GAME_ENTITIES,
                game_entity_ids: GAME_ENTITY_IDS,
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
                player_spawn: PLAYER_SPAWN,
                player_controller: None,
                world_message: WORLD_MESSAGE,
                world_ambient_rgb: [0; 3],
            });
        });
    }
}
