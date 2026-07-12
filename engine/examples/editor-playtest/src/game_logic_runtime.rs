//! Phase-3 gameplay-layer glue (docs/game-runtime-plan.md): the
//! example-side movement backend and effect dispatch the runtime
//! crate's `GameEntities`/`LogicRuntime` leave to the owning game.
//!
//! - [`SceneEntityMover`] backs entity Patrol/Aggro steps with the
//!   engine motor's `commit_body_step` over the entity room's ACTIVE
//!   collision (grid floors + step rules, closed box props, other
//!   bodies), so enemies move under exactly the player's rules. A
//!   room whose collision is not resident refuses movement (the
//!   entity keeps thinking; it walks again when the room streams in).
//! - Effect dispatch drains `LogicRuntime::take_fired` marks into the
//!   example's presentation: DOOR records toggle their linked box
//!   prop (draw + collision), MESSAGE records open the interactable
//!   message overlay, CHECKPOINT records update the in-memory
//!   checkpoint -- the same UI paths the legacy interactable flow
//!   used, now driven off LOGIC records (the cook pairs them 1:1).

use super::*;

/// Movement backend for game entities: the entity room's grid
/// collision from the active window, closed box props, the other
/// entities' bodies (pre-tick snapshot), and the player's body.
pub(super) struct SceneEntityMover<'a> {
    pub(super) window: &'a RuntimeRoomWindow,
    pub(super) box_props: &'a RuntimeBoxProps,
    pub(super) models: &'a [Option<RuntimeModelAsset>; MAX_RUNTIME_MODELS],
    /// Pre-tick entity positions (entities move one at a time inside
    /// the tick; the snapshot keeps blocker order deterministic).
    pub(super) entity_positions: [[i32; 3]; MAX_GAME_ENTITIES],
    pub(super) player: RoomPoint,
    pub(super) player_room: RoomIndex,
    pub(super) player_radius: i32,
    pub(super) player_height: i32,
}

impl SceneEntityMover<'_> {
    /// The active-window slot for `room`, if its collision is
    /// resident.
    fn active_room(&self, room: RoomIndex) -> Option<&ActiveRuntimeRoom> {
        self.window
            .rooms
            .iter()
            .flatten()
            .find(|active| active.index == room)
    }
}

impl psx_game_runtime::entities::GameEntityMover for SceneEntityMover<'_> {
    fn step(
        &mut self,
        entity: usize,
        room: RoomIndex,
        position: [i32; 3],
        dx: i32,
        dz: i32,
        radius: i32,
        height: i32,
    ) -> [i32; 3] {
        // Entity coordinates are their OWN room's local space, so the
        // collision room enters with zero offsets (the offsets on the
        // window slots translate rooms into the CURRENT room's space
        // for the player; entities never leave their room this
        // slice).
        let Some(active) = self.active_room(room) else {
            return position;
        };
        let collision_rooms = [CharacterCollisionRoom::from_collision(
            active.collision_room,
            0,
            0,
        )];

        // Body blockers: other entities' cooked instances at their
        // LIVE positions (self excluded), plus the player's capsule.
        let own_instance = GAME_ENTITIES
            .get(entity)
            .map(|record| record.model_instance)
            .unwrap_or(psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE);
        let mut cylinders = [CharacterCollisionCylinder::EMPTY; MAX_MODEL_INSTANCES];
        let mut cylinder_count = 0usize;
        for (index, inst) in MODEL_INSTANCES.iter().enumerate() {
            if inst.room != room || cylinder_count >= cylinders.len() {
                continue;
            }
            let inst_index = index.min(u16::MAX as usize) as u16;
            if inst_index == own_instance {
                continue;
            }
            let Some(model) = self.models.get(inst.model.to_usize()).copied().flatten() else {
                continue;
            };
            let body_radius = i32::from(model.collision_radius).max(1);
            let body_height = (model.world_height as i32).max(1);
            let center = match game_entity_for_instance(inst_index) {
                Some(other) => {
                    let live = self.entity_positions[other.min(MAX_GAME_ENTITIES - 1)];
                    RoomPoint::new(live[0], live[1], live[2])
                }
                None => RoomPoint::new(inst.x, inst.y, inst.z),
            };
            cylinders[cylinder_count] =
                CharacterCollisionCylinder::new(center, body_radius, body_height);
            cylinder_count += 1;
        }
        if self.player_room == room && self.player_radius > 0 && cylinder_count < cylinders.len() {
            cylinders[cylinder_count] = CharacterCollisionCylinder::new(
                self.player,
                self.player_radius,
                self.player_height.max(1),
            );
            cylinder_count += 1;
        }

        let mut aabbs = [CharacterCollisionAabb::EMPTY; MAX_BOX_PROP_BLOCKERS];
        let aabb_count = self
            .box_props
            .collect_collision_blockers(BOX_PROPS, room, &mut aabbs);

        let collision = CharacterCollision::rooms_with_aabbs(
            &collision_rooms,
            &cylinders[..cylinder_count],
            &aabbs[..aabb_count],
        );
        let step = psx_engine::character_motor::commit_body_step(
            collision,
            RoomPoint::new(position[0], position[1], position[2]),
            dx,
            dz,
            radius,
            height,
        );
        [step.position.x, step.position.y, step.position.z]
    }
}

/// The game entity owning cooked model instance `instance`, if any.
pub(super) fn game_entity_for_instance(instance: u16) -> Option<usize> {
    if instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE {
        return None;
    }
    GAME_ENTITIES
        .iter()
        .position(|record| record.model_instance == instance)
}

impl Playtest {
    /// Build the per-frame pose-override list: every live (visible)
    /// entity with a cooked visual renders at its runtime position and
    /// facing. Returns the filled count.
    pub(super) fn game_entity_pose_overrides(
        &self,
        out: &mut [ModelInstancePoseOverride; MAX_GAME_ENTITIES],
    ) -> usize {
        let mut count = 0usize;
        for (index, record) in GAME_ENTITIES.iter().enumerate() {
            if record.model_instance == psx_level::GAME_ENTITY_MODEL_INSTANCE_NONE
                || count >= out.len()
            {
                continue;
            }
            let position = self.game_entities.position(index);
            out[count] = ModelInstancePoseOverride {
                instance: record.model_instance,
                x: position[0],
                y: position[1],
                z: position[2],
                yaw: self.game_entities.yaw(index),
            };
            count += 1;
        }
        count
    }

    /// Drain the logic fire marks into example-side effects: doors
    /// toggle their linked box prop, messages open the interactable
    /// overlay, checkpoints update the in-memory checkpoint (with the
    /// same confirmation overlay the legacy interactable path showed).
    pub(super) fn dispatch_logic_effects(&mut self) {
        if !self.logic.any_fired() {
            return;
        }
        for (index, record) in LOGIC.iter().enumerate() {
            if !self.logic.take_fired(index) {
                continue;
            }
            match record.kind {
                psx_level::logic_kind::DOOR => {
                    if record.link != psx_level::LOGIC_LINK_NONE {
                        self.box_props
                            .set_door_open(usize::from(record.link), self.logic.door_open(index));
                    }
                }
                psx_level::logic_kind::MESSAGE => {
                    self.open_logic_message(index, record);
                }
                psx_level::logic_kind::CHECKPOINT => {
                    self.checkpoint = Some(RuntimeCheckpoint {
                        room: self.room_index,
                        position: self.motor.position(),
                        yaw: self.motor.yaw(),
                        checkpoint_id: interactable_for_logic(index)
                            .map(|interactable| interactable.checkpoint_id)
                            .unwrap_or(""),
                    });
                    self.open_logic_message(index, record);
                }
                // Graph plumbing (trigger volumes, relays,
                // multisource gates) has no presentation effect.
                _ => {}
            }
        }
    }

    /// Message overlay for a fired MESSAGE/CHECKPOINT record, through
    /// the same text table + fallbacks the legacy interactable path
    /// used (the cook shares the message index between both records).
    fn open_logic_message(&mut self, index: usize, record: &psx_level::LevelLogicRecord) {
        let (title, body) = match interactable_for_logic(index) {
            Some(interactable) => interactable_message_text(interactable),
            None => match INTERACTABLE_MESSAGES.get(record.message as usize) {
                Some(message) => (message.title, message.body),
                None => return,
            },
        };
        self.message_overlay = Some(RuntimeMessageOverlay { title, body });
    }

    /// Push every DOOR record's current open state onto its linked
    /// box prop (gameplay init and future checkpoint respawns:
    /// START_ON doors begin open without a fire event).
    pub(super) fn sync_door_box_props(&mut self) {
        for (index, record) in LOGIC.iter().enumerate() {
            if record.kind == psx_level::logic_kind::DOOR
                && record.link != psx_level::LOGIC_LINK_NONE
            {
                self.box_props
                    .set_door_open(usize::from(record.link), self.logic.door_open(index));
            }
        }
    }
}

/// The interactable paired with logic record `logic_index`, if any.
pub(super) fn interactable_for_logic(logic_index: usize) -> Option<&'static InteractableRecord> {
    let logic_index = u16::try_from(logic_index).ok()?;
    INTERACTABLES
        .iter()
        .find(|interactable| interactable.logic == logic_index)
}
