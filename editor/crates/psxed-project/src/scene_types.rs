use super::*;
use std::collections::HashMap;

/// Largest point-light radius representable by the cooked runtime record.
/// Authoring stores radius in sectors, while editors present this world-unit
/// limit after applying the active World's sector size.
pub const POINT_LIGHT_RADIUS_MAX_WORLD_UNITS: f32 = u16::MAX as f32;

/// Node type used by the editor scene tree.
///
/// Hierarchy convention for level authoring:
/// `World (scene root) -> Room (sector grid) -> portal/entity nodes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// Plain organisational node.
    Node,
    /// Named authoring group. Groups are editor-only hierarchy objects: they
    /// can contain scene nodes, nested groups, and world-space BSP brushes.
    /// Cooking deliberately flattens them away.
    Group,
    /// Spatial transform node.
    Node3D,
    /// Composed world object. The node owns transform/identity;
    /// behaviour is expressed by component-node children such as
    /// [`ModelRenderer`](Self::ModelRenderer),
    /// [`Animator`](Self::Animator), and
    /// [`Collider`](Self::Collider).
    Entity,
    /// World-root node for one authored world. Owns global settings
    /// inherited by descendant room grids.
    World {
        /// Shared sector size in engine units, snapped to
        /// [`WORLD_SECTOR_SIZE_QUANTUM`].
        #[serde(default = "default_world_sector_size")]
        sector_size: i32,
        /// Background sky drawn before room geometry.
        #[serde(default)]
        sky: SkySettings,
        /// Distant scenery ring drawn between sky and room geometry.
        #[serde(default)]
        far_vista: FarVistaSettings,
        /// Third-person camera defaults inherited by descendant rooms.
        #[serde(default)]
        camera: WorldCameraSettings,
        /// Runtime culling controls inherited by descendant rooms.
        #[serde(default)]
        culling: WorldCullingSettings,
        /// Cook-time streaming controls inherited by descendant rooms.
        #[serde(default)]
        streaming: WorldStreamingSettings,
        /// Runtime physics controls inherited by descendant rooms.
        #[serde(default)]
        physics: WorldPhysicsSettings,
        /// Optional message shown once per game launch when this scene starts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        world_message: Option<WorldMessage>,
    },
    /// One authored level section: a sector grid plus its child
    /// entities and portal links.
    ///
    /// Named Section, not Room, because "room" already means two other
    /// things: the runtime [`crate::portal_rooms::PortalRoom`] the cook
    /// derives by splitting this grid at authored portals (the streaming,
    /// PVS and residency unit, and what the size caps apply to), and the
    /// chamber a player perceives. A Section is a named, placeable,
    /// hideable authoring layer that can be saved as a prefab. One Section
    /// usually becomes several runtime rooms.
    ///
    /// The alias chain keeps every project that was saved as `Map` or
    /// `Room` loading unchanged.
    #[serde(rename = "Section", alias = "Room", alias = "Map")]
    Section {
        /// Authored grid-world payload.
        grid: WorldGrid,
    },
    /// A horizontal, cell-painted water body owned by one Room floor.
    ///
    /// The node's [`SceneNode::floor`] selects the stacked floor and `cells`
    /// use that floor grid's persistent world-cell coordinates. `material`
    /// renders only the exposed top surface; gameplay comes from `settings`,
    /// never from the material.
    WaterVolume {
        /// Material used by the generated water surface.
        #[serde(default)]
        material: Option<ResourceId>,
        /// Persistent world-cell footprint.
        #[serde(default)]
        cells: Vec<WaterVolumeCell>,
        /// Shallow/lethal gameplay configuration.
        #[serde(default)]
        settings: WaterVolumeSettings,
    },
    /// Static or dynamic mesh / model instance.
    ///
    /// `mesh` references either a legacy [`ResourceData::Mesh`] or a
    /// cooked [`ResourceData::Model`]. When it points at a Model,
    /// `animation_clip` selects which clip plays -- an explicit
    /// `Some(idx)` overrides the model's `default_clip`; `None`
    /// inherits the model default. Instances of legacy meshes
    /// ignore this field.
    MeshInstance {
        /// Mesh / model resource.
        mesh: Option<ResourceId>,
        /// Material override (legacy mesh path; ignored for Model
        /// resources, which embed material data in the `.psxmdl`).
        material: Option<ResourceId>,
        /// Per-instance animation clip override.
        #[serde(default)]
        animation_clip: Option<u16>,
    },
    /// Flat material-backed image plane. The node transform marks
    /// the bottom-center anchor; yaw controls the static facing
    /// direction unless cylindrical billboarding is enabled.
    ImageProp {
        /// Material used by the quad.
        #[serde(default)]
        material: Option<ResourceId>,
        /// Authored width in engine/editor units.
        #[serde(default = "default_image_prop_size")]
        width: u16,
        /// Authored height in engine/editor units.
        #[serde(default = "default_image_prop_size")]
        height: u16,
        /// Rotate around Y every frame so the card faces the camera
        /// while staying upright.
        #[serde(default)]
        cylindrical_billboard: bool,
        /// Toggle the authored AABB collision box around the prop.
        /// Disabled by default so legacy props (and freshly placed
        /// ones) keep the "decorative-only" semantics they had
        /// before collision was opt-in.
        #[serde(default)]
        collision_enabled: bool,
        /// Full size (width / height / depth) of the AABB collision
        /// box in engine units, centered on the visible plane.
        /// Ignored when [`collision_enabled`](Self::ImageProp) is
        /// `false`, but kept around so toggling it back on restores
        /// the user's last size instead of snapping to a default.
        #[serde(default = "default_image_prop_collision_size")]
        collision_size: [u16; 3],
    },
    /// Material-backed editable hexahedron. The transform is a
    /// bottom-center anchor, `vertices` are local engine units from
    /// that anchor, and each face can bind its own material.
    BoxProp {
        /// Per-face material slots in [`BOX_PROP_FACE_NAMES`] order.
        #[serde(default = "default_box_prop_materials")]
        materials: [Option<ResourceId>; BOX_PROP_FACE_COUNT],
        /// Per-face texture transforms in [`BOX_PROP_FACE_NAMES`] order.
        #[serde(default = "default_box_prop_uvs")]
        uvs: [GridUvTransform; BOX_PROP_FACE_COUNT],
        /// Editable local vertices, bottom ring then top ring.
        #[serde(default = "default_box_prop_vertices")]
        vertices: [[i16; 3]; BOX_PROP_VERTEX_COUNT],
        /// Whether this prop blocks the character motor.
        #[serde(default = "default_true")]
        collision_enabled: bool,
        /// Authored break trigger bits from [`psx_level::box_prop_flags`].
        #[serde(default)]
        break_flags: u16,
        /// Optional direction-driven low-poly erosion. Disabled by default so
        /// existing projects retain the exact legacy six-face box.
        #[serde(default)]
        erosion: BoxPropErosion,
    },
    /// Low-poly radial prop for columns, pillars, pipes, and authored debris.
    ///
    /// This deliberately remains separate from [`BoxProp`](Self::BoxProp):
    /// the transform is a bottom-center anchor and `geometry` describes a
    /// compact radial profile expanded by the shared preview/cook generator.
    CylinderProp {
        /// Side, top, bottom, and fracture material slots.
        #[serde(default = "default_cylinder_prop_materials")]
        materials: [Option<ResourceId>; CYLINDER_PROP_MATERIAL_COUNT],
        /// Per-slot texture transforms.
        #[serde(default = "default_cylinder_prop_uvs")]
        uvs: [GridUvTransform; CYLINDER_PROP_MATERIAL_COUNT],
        /// Compact procedural shape recipe.
        #[serde(default)]
        geometry: CylinderPropGeometry,
        /// Whether this prop blocks the character motor.
        #[serde(default = "default_true")]
        collision_enabled: bool,
    },
    /// Tile-snapped procedural arch or half-arch.
    ///
    /// The transform is a bottom-centre anchor. Horizontal dimensions are
    /// inherited from the enclosing room's sector size, while vertical
    /// dimensions are stored as 64-unit quanta in `geometry`. Preview and cook
    /// expand the same compact curve recipe into an extruded low-poly band.
    ArchProp {
        /// Fascia, soffit, extrados, and exposed end-cap material slots.
        #[serde(default = "default_arch_prop_materials")]
        materials: [Option<ResourceId>; ARCH_PROP_MATERIAL_COUNT],
        /// Per-slot texture transforms.
        #[serde(default = "default_arch_prop_uvs")]
        uvs: [GridUvTransform; ARCH_PROP_MATERIAL_COUNT],
        /// Compact tile-native arch recipe.
        #[serde(default)]
        geometry: ArchPropGeometry,
        /// Whether generated arch segments block the character motor.
        #[serde(default)]
        collision_enabled: bool,
    },
    /// Render a cooked [`ResourceData::Model`] from the transform
    /// on the nearest entity ancestor. This is the component form of
    /// the legacy [`MeshInstance`](Self::MeshInstance) node.
    ModelRenderer {
        /// Model resource.
        model: Option<ResourceId>,
        /// Material override. `None` renders the model's own cooked
        /// atlas unchanged. A Material without a texture keeps that
        /// atlas and changes blend/tint/sidedness; a textured
        /// Material also replaces the atlas image.
        #[serde(default)]
        material: Option<ResourceId>,
        /// Render-only offset from the owning Entity root to the
        /// model origin, in entity-local engine units. This does
        /// not affect collision, camera, or movement.
        #[serde(default)]
        visual_offset: [i16; 3],
        /// Render-only uniform scale in Q8 fixed point (`256 =
        /// 1.0`). Use this for per-instance calibration; use the
        /// Model resource import scale for global asset fixes.
        #[serde(default = "default_model_renderer_visual_scale_q8")]
        visual_scale_q8: u16,
    },
    /// Animation component for a model-rendering entity. `clip`
    /// overrides the model default when set; `None` inherits the
    /// model's runtime default.
    Animator {
        /// Per-instance clip override.
        #[serde(default)]
        clip: Option<u16>,
        /// Gameplay action to model-local animation clip mapping.
        /// This is the authoritative authoring location for
        /// player/NPC action animation.
        #[serde(default)]
        action_clips: Vec<CharacterActionClip>,
        /// Whether this animation should run automatically in the
        /// editor/playtest runtime.
        #[serde(default = "default_true")]
        autoplay: bool,
        /// Frame to hold when `autoplay` is off, so a model can be
        /// placed frozen on a chosen pose (e.g. a corpse on a death
        /// frame). Ignored while `autoplay` is on.
        #[serde(default)]
        pose_frame: u16,
    },
    /// Collision component. The first runtime pass only cooks room
    /// grid collision, but keeping authored collider data as a node
    /// makes entity/interactable/NPC architecture explicit now.
    Collider {
        /// Collision shape in engine/editor units.
        #[serde(default)]
        shape: ColliderShape,
        /// Solid colliders block movement; non-solid colliders are
        /// trigger volumes.
        #[serde(default = "default_true")]
        solid: bool,
    },
    /// Character/controller component. It binds an entity to a reusable
    /// [`ResourceData::Character`] profile. When `player` is true this is
    /// the component-tree replacement for a legacy player
    /// [`SpawnPoint`](Self::SpawnPoint); non-player controllers cook as
    /// idle model instances until dedicated NPC runtime records exist.
    CharacterController {
        /// Character profile resource.
        #[serde(default)]
        character: Option<ResourceId>,
        /// Per-placement override of the Character's movement, stamina, evade
        /// and capsule tuning.
        ///
        /// `None` means "whatever the Character says", which is what makes the
        /// Character resource a live type rather than a template that is copied
        /// once and then drifts. Placement leaves this `None`; it is
        /// materialised only when someone actually edits a placed controller.
        /// Projects written before this was optional carry a bare settings
        /// struct and deserialize into `Some`, so nothing needs migrating.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_controller_settings",
            deserialize_with = "deserialize_controller_settings"
        )]
        settings: Option<CharacterControllerSettings>,
        /// Whether this controller drives the player.
        #[serde(default)]
        player: bool,
    },
    /// Gameplay camera component for a player-controlled entity.
    /// The parent Entity supplies the start position/yaw; these settings
    /// define the third-person follow rig used by Play.
    Camera {
        /// Third-person follow camera settings.
        #[serde(default)]
        settings: WorldCameraSettings,
    },
    /// Equipment component. The parent Entity supplies the animated
    /// character model; this component names the Weapon and which
    /// socket/grip pair should be composed.
    Equipment {
        /// Weapon resource.
        #[serde(default)]
        weapon: Option<ResourceId>,
        /// Character/model socket to follow.
        #[serde(default = "default_character_socket")]
        character_socket: String,
        /// Weapon-local grip/pivot to align to the character socket.
        #[serde(default = "default_weapon_grip")]
        weapon_grip: String,
    },
    /// Physics body component for movable entities.
    PhysicsBody {
        /// Per-entity physics tuning.
        #[serde(default)]
        settings: PhysicsBodySettings,
    },
    /// Gameplay interaction component. Attach this to an Entity
    /// alongside render/collision components to make the placed object
    /// readable, synchronizable, or otherwise activatable at runtime.
    Interactable {
        /// What happens when the player presses the interaction button.
        #[serde(default)]
        kind: InteractableKind,
        /// Short prompt shown while the player is inside the radius.
        #[serde(default = "default_interactable_prompt")]
        prompt: String,
        /// Interaction radius in engine/editor units, measured in XZ
        /// from the parent Entity origin.
        #[serde(default = "default_interactable_radius")]
        radius: u16,
        /// Disabled interactables remain authored but are not emitted as
        /// active runtime records.
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// Authored readable beacon component. The parent Entity supplies the
    /// world transform; the runtime draws the procedural marker and handles
    /// interaction, paging, persistence, and an optional one-time reward.
    PointOfInterest {
        /// Body-only message pages. New POIs start with valid placeholder
        /// copy so they can be played immediately, while authors can replace
        /// it with the final message in the inspector.
        #[serde(default = "default_point_of_interest_pages")]
        pages: Vec<String>,
        /// Short prompt shown while the player is inside the radius.
        #[serde(default = "default_point_of_interest_prompt")]
        prompt: String,
        /// Interaction radius in engine/editor units, measured in XZ from the
        /// parent Entity origin.
        #[serde(default = "default_point_of_interest_radius")]
        radius: u16,
        /// Authored visual scale of the procedural marker grounded at the
        /// parent Entity origin. The runtime converts this to a compact glyph.
        #[serde(default = "default_point_of_interest_marker_height")]
        marker_height: u16,
        /// Whether the message can be opened again after being read.
        #[serde(default = "default_true")]
        repeatable: bool,
        /// Stable save-state key. An empty id lets the cooker derive one from
        /// the authored node identity.
        #[serde(default)]
        persistence_id: String,
        /// Optional one-time module grant, tracked independently from message
        /// repeatability.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reward: Option<PointOfInterestReward>,
        /// Disabled points remain authored but are not active at runtime.
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// Static point light.
    PointLight {
        /// RGB light colour.
        #[serde(default = "default_light_color")]
        color: [u8; 3],
        /// Light intensity multiplier.
        intensity: f32,
        /// Approximate editor/runtime radius in sectors.
        radius: f32,
    },
    /// Cheap point-projected world particle emitter.
    ParticleEmitter {
        /// Fixed-budget emitter tuning.
        #[serde(default)]
        settings: ParticleEmitterSettings,
    },
    /// Placed logic-graph node (the phase-3 event graph): a trigger
    /// volume, relay, multisource gate, or door. The node NAME is the
    /// record's targetname (interned at cook); `target`, `killtarget`
    /// and `master` name other nodes the same way interactables and
    /// enemies are named. Cooks to a `psx_level::LevelLogicRecord`.
    Logic {
        /// Kind-specific behavior + payload.
        #[serde(default)]
        kind: LogicNodeKind,
        /// Node name this record fires when it triggers ("" = none).
        #[serde(default)]
        target: String,
        /// Node name this record removes when it triggers ("" = none).
        #[serde(default)]
        killtarget: String,
        /// Multisource node name gating this record ("" = ungated).
        #[serde(default)]
        master: String,
        /// 60 Hz ticks between triggering and firing `target`.
        #[serde(default)]
        delay_ticks: u16,
        /// 60 Hz ticks before re-arming after a fire; negative means
        /// fire once then retire (hl's `wait -1`).
        #[serde(default)]
        wait_ticks: i16,
        /// Disabled nodes stay authored but cook flag-disabled.
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// Spawn marker.
    SpawnPoint {
        /// Whether this is the player spawn.
        player: bool,
        /// Character profile resource that drives this spawn. For the
        /// player spawn this picks the player's model + role
        /// clips + controller params. `None` lets the cook step
        /// auto-pick a Character when exactly one exists, or
        /// fail with a clear error otherwise. Non-player spawns
        /// currently ignore this field.
        #[serde(default)]
        character: Option<ResourceId>,
    },
    /// Manual streaming/visibility graph edge: the cooker snaps the marker
    /// to a grid edge and treats that edge as a room-to-room portal.
    Portal {
        /// Target room node by id, or `None` when not wired.
        target_room: Option<NodeId>,
        /// Entry-portal label on the target room.
        target_entry: String,
        /// Identifier this portal marker is known by in its source room.
        entry_name: String,
        /// Optional exact 3D portal plane imported from a Tomb
        /// Raider-style level file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        geometry: Option<PortalGeometry>,
    },
}

impl NodeKind {
    /// Default scene-root World node.
    pub fn default_world() -> Self {
        Self::World {
            sector_size: DEFAULT_WORLD_SECTOR_SIZE,
            sky: SkySettings::default(),
            far_vista: FarVistaSettings::default(),
            camera: WorldCameraSettings::default(),
            culling: WorldCullingSettings::default(),
            streaming: WorldStreamingSettings::default(),
            physics: WorldPhysicsSettings::default(),
            world_message: None,
        }
    }

    /// User-facing label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Node => "Node",
            Self::Group => "Group",
            Self::Node3D => "Node3D",
            Self::Entity => "Entity",
            Self::World { .. } => "World",
            Self::Section { .. } => "Section",
            Self::WaterVolume { .. } => "Water Volume",
            Self::MeshInstance { .. } => "Mesh Instance",
            Self::ImageProp { .. } => "Image Prop",
            Self::BoxProp { .. } => "Box Prop",
            Self::CylinderProp { .. } => "Cylinder Prop",
            Self::ArchProp { .. } => "Arch Prop",
            Self::ModelRenderer { .. } => "Model Renderer",
            Self::Animator { .. } => "Animator",
            Self::Collider { .. } => "Collider",
            Self::CharacterController { .. } => "Character Controller",
            Self::Camera { .. } => "Camera",
            Self::Equipment { .. } => "Equipment",
            Self::PhysicsBody { .. } => "Physics Body",
            Self::Interactable { .. } => "Interactable",
            Self::PointOfInterest { .. } => "Point of Interest",
            Self::Logic { .. } => "Logic",
            Self::PointLight { .. } => "Point Light",
            Self::ParticleEmitter { .. } => "Particle Emitter",
            Self::SpawnPoint { .. } => "Spawn Point",
            Self::Portal { .. } => "Portal",
        }
    }

    /// True for behaviour/component nodes that are intended to be
    /// children of an [`Entity`](Self::Entity) host rather than
    /// independent placed objects.
    pub const fn is_component(&self) -> bool {
        matches!(
            self,
            Self::ModelRenderer { .. }
                | Self::Animator { .. }
                | Self::Collider { .. }
                | Self::CharacterController { .. }
                | Self::Camera { .. }
                | Self::Equipment { .. }
                | Self::PhysicsBody { .. }
                | Self::Interactable { .. }
                | Self::PointOfInterest { .. }
        )
    }
}

/// Per-scene body-only launch message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldMessage {
    /// Pages shown in authored order. The world presentation supports three
    /// lines per page; line wrapping is a runtime concern.
    #[serde(default = "default_message_pages")]
    pub pages: Vec<String>,
}

impl Default for WorldMessage {
    fn default() -> Self {
        Self {
            pages: default_message_pages(),
        }
    }
}

/// Optional one-time reward attached to a point of interest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointOfInterestReward {
    /// Legacy Boost Module resource reference. Existing projects continue to
    /// load; new rewards define their unique item inline below.
    #[serde(default)]
    pub module: Option<ResourceId>,
    /// Legacy quantity. Unique modules always grant once.
    #[serde(default = "default_point_of_interest_reward_quantity")]
    pub quantity: u8,
    /// Unique item name presented in the inventory and acquisition panel.
    #[serde(default)]
    pub item_name: String,
    /// Short inventory description.
    #[serde(default)]
    pub description: String,
    /// Signed percentage modifiers. Multiple entries may target the same stat
    /// and are added together by the cooker.
    #[serde(default)]
    pub modifiers: Vec<crate::BoostStatModifier>,
}

impl Default for PointOfInterestReward {
    fn default() -> Self {
        Self {
            module: None,
            quantity: 1,
            item_name: "NEW MODULE".to_string(),
            description: "Recovered boost module.".to_string(),
            modifiers: vec![crate::BoostStatModifier::default()],
        }
    }
}

pub fn default_message_pages() -> Vec<String> {
    vec![String::new()]
}

pub fn default_point_of_interest_pages() -> Vec<String> {
    vec!["ARCHIVE SIGNAL DETECTED.".to_string()]
}

pub fn default_point_of_interest_prompt() -> String {
    "READ".to_string()
}

pub const fn default_point_of_interest_radius() -> u16 {
    576
}

pub const fn default_point_of_interest_marker_height() -> u16 {
    192
}

pub const fn default_point_of_interest_reward_quantity() -> u8 {
    1
}

/// Authored interaction payload for an [`NodeKind::Interactable`]
/// component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractableKind {
    /// Show a diegetic text message.
    Message {
        /// Message title/header.
        #[serde(default = "default_interactable_message_title")]
        title: String,
        /// Message body.
        #[serde(default)]
        body: String,
    },
    /// Update the in-memory checkpoint/sync point and optionally show
    /// a confirmation message.
    Checkpoint {
        /// Stable authored id for future save/flag systems.
        #[serde(default)]
        checkpoint_id: String,
        /// Confirmation title/header.
        #[serde(default = "default_interactable_checkpoint_title")]
        title: String,
        /// Confirmation body.
        #[serde(default = "default_interactable_checkpoint_body")]
        body: String,
    },
}

impl Default for InteractableKind {
    fn default() -> Self {
        Self::Message {
            title: default_interactable_message_title(),
            body: String::new(),
        }
    }
}

pub(crate) fn default_interactable_prompt() -> String {
    "READ ECHO".to_string()
}

pub(crate) const fn default_interactable_radius() -> u16 {
    96
}

pub(crate) fn default_interactable_message_title() -> String {
    "ECHO REMNANT".to_string()
}

pub(crate) fn default_interactable_checkpoint_title() -> String {
    "SYNC RELAY".to_string()
}

pub(crate) fn default_interactable_checkpoint_body() -> String {
    "Relay synchronized.".to_string()
}

/// Kind payload for a placed [`NodeKind::Logic`] node. Mirrors the
/// runtime's `psx_level::logic_kind` selectors that are placed (not
/// paired from interactables): trigger volumes, relays, multisource
/// AND gates, and doors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogicNodeKind {
    /// AABB volume that fires `target` when the player enters it.
    /// The node transform is the floor-anchored center; `size` is the
    /// full extent in engine units (XZ centered, Y up from the floor).
    TriggerVolume {
        /// Full volume extent, engine units.
        #[serde(default = "default_logic_trigger_size")]
        size: [u16; 3],
    },
    /// Fires `target` after `delay_ticks` when triggered (the
    /// fan-out/delay building block).
    Relay,
    /// AND gate: satisfied while `required` inputs are on; records
    /// naming this node as `master` are gated by it.
    Multisource {
        /// Inputs required to satisfy the gate.
        #[serde(default = "default_logic_multisource_required")]
        required: u16,
    },
    /// Toggles the named Box Prop between closed (drawn + solid) and
    /// open (hidden + passable) in frozen grid projects. A brush-bound
    /// door instead translates its compiled submodel to `open_offset`.
    Door {
        /// Name of the Box Prop node this door drives. Must resolve
        /// to exactly one placed Box Prop at cook time.
        #[serde(default)]
        box_prop: String,
        /// Whether the door starts open.
        #[serde(default)]
        start_open: bool,
        /// World-space translation from closed to open, in engine units.
        #[serde(default = "default_brush_door_open_offset")]
        open_offset: [i16; 3],
        /// Fixed 60 Hz simulation ticks between endpoints.
        #[serde(default = "default_brush_door_travel_ticks")]
        travel_ticks: u16,
    },
}

impl Default for LogicNodeKind {
    fn default() -> Self {
        Self::TriggerVolume {
            size: default_logic_trigger_size(),
        }
    }
}

pub(crate) const fn default_logic_trigger_size() -> [u16; 3] {
    [768, 1024, 768]
}

pub(crate) const fn default_logic_multisource_required() -> u16 {
    1
}

pub const fn default_brush_door_open_offset() -> [i16; 3] {
    [0, 128, 0]
}

pub const fn default_brush_door_travel_ticks() -> u16 {
    60
}

/// Authored collision shape for component-node entities.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ColliderShape {
    /// Axis-aligned box, stored as half-extents.
    Box {
        /// Half extents in engine/editor units.
        half_extents: [u16; 3],
    },
    /// Sphere collider.
    Sphere {
        /// Radius in engine/editor units.
        radius: u16,
    },
    /// Upright capsule.
    Capsule {
        /// Radius in engine/editor units.
        radius: u16,
        /// Height in engine/editor units.
        height: u16,
    },
}

impl Default for ColliderShape {
    fn default() -> Self {
        Self::Box {
            half_extents: [256, 256, 256],
        }
    }
}

pub(crate) const fn default_true() -> bool {
    true
}

/// Explicit adaptive-style portal rectangle.
///
/// Authored seam portals still use the marker transform and snap to
/// sector edges. Imported TR levels already carry the exact 3D
/// rectangle that connects two rooms, so keep that information on
/// the portal node instead of trying to rediscover it from a 2D grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalGeometry {
    /// Portal normal in editor/world coordinates.
    pub normal: [i32; 3],
    /// Portal corners in editor/world coordinates.
    pub vertices: [[i32; 3]; 4],
}

/// Write an override as the bare struct older projects already used, so the
/// on-disk shape never changed and a file stays readable by both. `None` is
/// skipped entirely, which is what marks a placement as following its type.
fn serialize_controller_settings<S: serde::Serializer>(
    value: &Option<CharacterControllerSettings>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::Serialize as _;
    value
        .as_ref()
        .expect("skip_serializing_if keeps None out of here")
        .serialize(serializer)
}

/// Accept both shapes of `CharacterController::settings`: the bare struct that
/// older projects wrote, and an absent field on newly placed controllers.
fn deserialize_controller_settings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CharacterControllerSettings>, D::Error> {
    use serde::Deserialize as _;
    CharacterControllerSettings::deserialize(deserializer).map(Some)
}

/// A scene-tree node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneNode {
    /// Stable node id.
    pub id: NodeId,
    /// Display name.
    pub name: String,
    /// Node type.
    pub kind: NodeKind,
    /// Local transform.
    pub transform: Transform3,
    /// Which floor of the enclosing Room this node belongs to (0 =
    /// ground). Stacked floors share the same XZ cells, so a node's
    /// floor cannot be inferred from its Y (the authored standing height
    /// is a placement default identical across projects). Recorded
    /// explicitly at placement and consumed by the cook to bind the node
    /// to the right runtime room. Default `0` keeps every existing
    /// project (and all non-Room-child nodes) on the ground.
    #[serde(default)]
    pub floor: usize,
    /// Parent id, absent only for the scene root.
    pub parent: Option<NodeId>,
    /// Ordered child ids.
    pub children: Vec<NodeId>,
}

impl SceneNode {
    fn new(id: NodeId, parent: Option<NodeId>, name: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            transform: Transform3::default(),
            floor: 0,
            parent,
            children: Vec::new(),
        }
    }
}

/// Owned row used by hierarchy UI without borrowing the scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRow {
    /// Node id.
    pub id: NodeId,
    /// Parent node id, or `None` for the scene root.
    pub parent: Option<NodeId>,
    /// Tree depth from root.
    pub depth: usize,
    /// Index of this node inside its parent's `children` list. Used
    /// by the editor's drag-drop machinery so a "drop above this row"
    /// gesture maps cleanly to `move_node(.., parent, sibling_index)`.
    pub sibling_index: usize,
    /// Display name.
    pub name: String,
    /// Node kind label.
    pub kind: &'static str,
    /// Number of direct children.
    pub child_count: usize,
    /// Number of BSP brushes directly owned by this authoring group.
    pub brush_count: usize,
}

/// One editor scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Display name.
    pub name: String,
    /// World root node id.
    pub root: NodeId,
    next_node_id: u64,
    pub(crate) nodes: Vec<SceneNode>,
    /// World-space convex brushes (docs/brush-editor-integration.md).
    /// A parallel collection for now; folds into the node tree when
    /// brushes need hierarchy/visibility.
    #[serde(default)]
    pub brushes: Vec<crate::brush::Brush>,
}

impl Scene {
    /// Create a scene with one root `World`.
    pub fn new(name: impl Into<String>) -> Self {
        let root = SceneNode::new(NodeId::ROOT, None, "World", NodeKind::default_world());
        Self {
            name: name.into(),
            root: NodeId::ROOT,
            next_node_id: NodeId::ROOT.raw() + 1,
            nodes: vec![root],
            brushes: Vec::new(),
        }
    }

    /// Normalize legacy `Root -> World -> Room` scenes into the
    /// adaptive-style `World(root) -> Room` hierarchy.
    pub fn normalize_world_root(&mut self) {
        let root_id = self.root;
        if self.node(root_id).is_none() {
            self.nodes.insert(
                0,
                SceneNode::new(root_id, None, "World", NodeKind::default_world()),
            );
        }

        let child_world = self.node(root_id).and_then(|root| {
            root.children.iter().copied().find(|id| {
                self.node(*id)
                    .is_some_and(|node| matches!(&node.kind, NodeKind::World { .. }))
            })
        });

        if self
            .node(root_id)
            .is_some_and(|root| matches!(&root.kind, NodeKind::World { .. }))
        {
            if let Some(root) = self.node_mut(root_id) {
                root.parent = None;
                if root.name == "Root" || root.name.is_empty() {
                    root.name = "World".to_string();
                }
            }
            return;
        }

        if let Some(world_id) = child_world {
            let Some(world_node) = self.node(world_id).cloned() else {
                return;
            };
            let mut merged_children = self
                .node(root_id)
                .map(|root| root.children.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|id| *id != world_id)
                .collect::<Vec<_>>();
            for child in world_node.children {
                if child != root_id && !merged_children.contains(&child) {
                    merged_children.push(child);
                }
            }
            for node in &mut self.nodes {
                if node.parent == Some(world_id) {
                    node.parent = Some(root_id);
                }
                node.children.retain(|child| *child != world_id);
            }
            if let Some(root) = self.node_mut(root_id) {
                root.name = if world_node.name.is_empty() || world_node.name == "Root" {
                    "World".to_string()
                } else {
                    world_node.name
                };
                root.kind = world_node.kind;
                root.parent = None;
                root.children = merged_children;
            }
            self.nodes.retain(|node| node.id != world_id);
        } else if let Some(root) = self.node_mut(root_id) {
            root.name = "World".to_string();
            root.kind = NodeKind::default_world();
            root.parent = None;
        }
    }

    /// All nodes in storage order.
    pub fn nodes(&self) -> &[SceneNode] {
        &self.nodes
    }

    /// Get a node.
    pub fn node(&self, id: NodeId) -> Option<&SceneNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Get a mutable node.
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut SceneNode> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    /// Add a node under `parent`. Invalid parents fall back to the root.
    pub fn add_node(&mut self, parent: NodeId, name: impl Into<String>, kind: NodeKind) -> NodeId {
        let parent = if self.node(parent).is_some() {
            parent
        } else {
            self.root
        };
        let id = NodeId(self.next_node_id);
        self.next_node_id = self.next_node_id.saturating_add(1);
        self.nodes
            .push(SceneNode::new(id, Some(parent), name, kind));
        if let Some(parent_node) = self.node_mut(parent) {
            parent_node.children.push(id);
        }
        id
    }

    /// Remove a non-root node and its descendants.
    pub fn remove_node(&mut self, id: NodeId) -> bool {
        if id == self.root || self.node(id).is_none() {
            return false;
        }

        let mut doomed = Vec::new();
        self.collect_descendants(id, &mut doomed);
        doomed.push(id);

        for node in &mut self.nodes {
            node.children.retain(|child| !doomed.contains(child));
        }
        self.nodes.retain(|node| !doomed.contains(&node.id));
        // Brushes owned by a deleted group subtree are part of that subtree.
        // Ungrouping reparents them explicitly before removing the now-empty
        // group, so it does not pass through this destructive path.
        self.brushes
            .retain(|brush| brush.group.is_none_or(|group| !doomed.contains(&group)));
        true
    }

    /// Repair stale or hand-authored brush group references after loading.
    /// Only real Group nodes may own brushes; legacy projects simply have
    /// `None` for every brush and therefore remain unchanged.
    pub fn normalize_brush_groups(&mut self) {
        let groups: std::collections::HashSet<NodeId> = self
            .nodes
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Group))
            .map(|node| node.id)
            .collect();
        for brush in &mut self.brushes {
            if brush.group.is_some_and(|group| !groups.contains(&group)) {
                brush.group = None;
            }
        }
    }

    /// Brush indices directly or recursively owned by `group`.
    pub fn brush_indices_in_group(&self, group: NodeId, recursive: bool) -> Vec<usize> {
        self.brushes
            .iter()
            .enumerate()
            .filter_map(|(index, brush)| {
                let owner = brush.group?;
                (owner == group || (recursive && self.is_descendant_of(owner, group)))
                    .then_some(index)
            })
            .collect()
    }

    /// `true` when `ancestor` appears anywhere on the parent chain of
    /// `id`. Includes `id` itself in the check, so callers using this
    /// for cycle detection don't need a separate equality test.
    pub fn is_descendant_of(&self, id: NodeId, ancestor: NodeId) -> bool {
        if id == ancestor {
            return true;
        }
        let mut current = self.node(id).and_then(|n| n.parent);
        let mut guard = 0usize;
        while let Some(p) = current {
            if guard >= self.nodes.len() {
                break;
            }
            if p == ancestor {
                return true;
            }
            current = self.node(p).and_then(|n| n.parent);
            guard += 1;
        }
        false
    }

    /// Move `id` under `new_parent` at `position` in its child list.
    ///
    /// Refuses (returns `false`) when:
    /// * `id` is the world root,
    /// * `id` or `new_parent` is missing,
    /// * `new_parent` is `id` or any of its descendants -- that would
    ///   form a cycle.
    ///
    /// `position` clamps to the destination's current child count.
    /// Reordering inside the same parent works because `id` is removed
    /// from the child list before `position` is clamped, so dropping
    /// at "the same slot" is a no-op without UI corner cases.
    pub fn move_node(&mut self, id: NodeId, new_parent: NodeId, position: usize) -> bool {
        if id == self.root {
            return false;
        }
        if self.node(id).is_none() || self.node(new_parent).is_none() {
            return false;
        }
        if self.is_descendant_of(new_parent, id) {
            return false;
        }

        for node in &mut self.nodes {
            node.children.retain(|c| *c != id);
        }
        if let Some(parent) = self.node_mut(new_parent) {
            let pos = position.min(parent.children.len());
            parent.children.insert(pos, id);
        }
        if let Some(node) = self.node_mut(id) {
            node.parent = Some(new_parent);
        }
        true
    }

    fn collect_descendants(&self, id: NodeId, out: &mut Vec<NodeId>) {
        if let Some(node) = self.node(id) {
            for child in &node.children {
                self.collect_descendants(*child, out);
                out.push(*child);
            }
        }
    }

    /// Sector size inherited by `id` from the nearest World ancestor.
    pub fn world_sector_size_for_node(&self, id: NodeId) -> Option<i32> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { sector_size, .. } = &node.kind {
                return Some(snap_world_sector_size(*sector_size));
            }
            current = node.parent;
        }
        None
    }

    /// Sky settings inherited by `id` from the nearest World ancestor.
    pub fn world_sky_for_node(&self, id: NodeId) -> Option<SkySettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { sky, .. } = &node.kind {
                return Some(*sky);
            }
            current = node.parent;
        }
        None
    }

    /// Far-vista settings inherited by `id` from the nearest World ancestor.
    pub fn world_far_vista_for_node(&self, id: NodeId) -> Option<FarVistaSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { far_vista, .. } = &node.kind {
                return Some(*far_vista);
            }
            current = node.parent;
        }
        None
    }

    /// Third-person camera settings inherited by `id` from the nearest World ancestor.
    pub fn world_camera_for_node(&self, id: NodeId) -> Option<WorldCameraSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { camera, .. } = &node.kind {
                return Some(camera.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Runtime culling settings inherited by `id` from the nearest World ancestor.
    pub fn world_culling_for_node(&self, id: NodeId) -> Option<WorldCullingSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { culling, .. } = &node.kind {
                return Some(culling.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Streaming chunk settings inherited by `id` from the nearest World ancestor.
    pub fn world_streaming_for_node(&self, id: NodeId) -> Option<WorldStreamingSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { streaming, .. } = &node.kind {
                return Some(streaming.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Physics settings inherited by `id` from the nearest World ancestor.
    pub fn world_physics_for_node(&self, id: NodeId) -> Option<WorldPhysicsSettings> {
        let mut current = Some(id);
        while let Some(node_id) = current {
            let node = self.node(node_id)?;
            if let NodeKind::World { physics, .. } = &node.kind {
                return Some(physics.normalized());
            }
            current = node.parent;
        }
        None
    }

    /// Rows in root-first depth-first order.
    pub fn hierarchy_rows(&self) -> Vec<NodeRow> {
        let mut rows = Vec::new();
        let mut brush_counts = HashMap::new();
        for group in self.brushes.iter().filter_map(|brush| brush.group) {
            *brush_counts.entry(group).or_insert(0) += 1;
        }
        self.push_hierarchy_row(self.root, 0, &brush_counts, &mut rows);
        rows
    }

    fn push_hierarchy_row(
        &self,
        id: NodeId,
        depth: usize,
        brush_counts: &HashMap<NodeId, usize>,
        rows: &mut Vec<NodeRow>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        rows.push(NodeRow {
            id,
            parent: node.parent,
            depth,
            sibling_index: node
                .parent
                .and_then(|parent_id| self.node(parent_id))
                .and_then(|parent| parent.children.iter().position(|child| *child == id))
                .unwrap_or(0),
            name: node.name.clone(),
            kind: node.kind.label(),
            child_count: node.children.len(),
            brush_count: brush_counts.get(&id).copied().unwrap_or(0),
        });
        for child in &node.children {
            self.push_hierarchy_row(*child, depth + 1, brush_counts, rows);
        }
    }
}
