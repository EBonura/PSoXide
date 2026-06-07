use super::*;

/// One cooked animation clip referenced by a [`ModelResource`].
///
/// `psxanim_path` resolves with the same precedence rules as
/// [`ResourceData::Texture::psxt_path`]: absolute → project-relative →
/// workspace cwd-relative. Stored relative to the project when the
/// editor registers a bundle, so projects move freely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAnimationClip {
    /// Display name surfaced in the inspector (clip dropdown,
    /// scrubber). Derived from the source filename when registered
    /// via a cooked bundle; user-editable.
    pub name: String,
    /// Path to the cooked `.psxanim` artifact.
    pub psxanim_path: String,
    /// Per-clip model placement controls used by editor preview and
    /// cooked runtime rendering.
    #[serde(default, skip_serializing_if = "AnimationClipCalibration::is_default")]
    pub calibration: AnimationClipCalibration,
}

/// Per-animation model placement controls.
///
/// These are deliberately stored on the clip, not on the character or
/// model renderer: different imported animations can have different
/// root conventions even when they target the same skeleton.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationClipCalibration {
    /// Render the clip in-place by cancelling root translation in
    /// model-local space. Controller code owns gameplay movement.
    #[serde(default = "default_true")]
    pub in_place: bool,
    /// Extra model-local pose translation in cooked pose units.
    #[serde(default)]
    pub offset: [i32; 3],
}

impl AnimationClipCalibration {
    pub const DEFAULT: Self = Self {
        in_place: true,
        offset: [0, 0, 0],
    };

    pub fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }
}

impl Default for AnimationClipCalibration {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Skeleton compatibility contract for skinned models and animation
/// clips.
///
/// The cooked `.psxanim` format only stores a joint count, so the
/// editor keeps the stronger authoring-side contract here: joint
/// count plus the cooked model parent table. Source importers can
/// extend this later with joint names and bind-pose hashes without
/// changing the runtime record layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkeletonResource {
    /// Number of joints in the skeleton.
    pub joint_count: u16,
    /// Parent index for each joint, or `None` for root joints.
    #[serde(default)]
    pub parents: Vec<Option<u16>>,
    /// Deterministic compatibility key. Current cooked assets use a
    /// parent-table signature; future importers should include joint
    /// names and bind pose in this value.
    #[serde(default)]
    pub signature: String,
    /// Human-readable note/source hint.
    #[serde(default)]
    pub note: String,
}

impl SkeletonResource {
    /// Build a skeleton descriptor from a cooked model.
    pub fn from_model(model: &psx_asset::Model<'_>) -> Self {
        let mut parents = Vec::with_capacity(model.joint_count() as usize);
        for index in 0..model.joint_count() {
            parents.push(model.joint(index).and_then(|joint| joint.parent()));
        }
        let signature = skeleton_signature(model.joint_count(), &parents);
        Self {
            joint_count: model.joint_count(),
            parents,
            signature,
            note: String::new(),
        }
    }

    /// True when an animation with `joint_count` can at least be
    /// safely sampled against this skeleton. This is the minimum
    /// cooked-format guarantee; exact skeleton signatures are checked
    /// when another skeleton resource is available.
    pub const fn accepts_joint_count(&self, joint_count: u16) -> bool {
        self.joint_count == joint_count
    }
}

pub(crate) fn skeleton_signature(joint_count: u16, parents: &[Option<u16>]) -> String {
    let mut out = format!("psx-parent-v1:{joint_count}:");
    for (index, parent) in parents.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match parent {
            Some(parent) => out.push_str(&parent.to_string()),
            None => out.push_str("root"),
        }
    }
    out
}

/// Semantic role for an animation clip. This is editor metadata:
/// runtime still receives concrete clip indices after cooking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationRole {
    /// No specific gameplay meaning yet.
    #[default]
    Generic,
    Idle,
    Walk,
    Run,
    Turn,
    Roll,
    Backstep,
    Attack,
    Hit,
    Death,
}

impl AnimationRole {
    pub const ALL: [Self; 10] = [
        Self::Generic,
        Self::Idle,
        Self::Walk,
        Self::Run,
        Self::Turn,
        Self::Roll,
        Self::Backstep,
        Self::Attack,
        Self::Hit,
        Self::Death,
    ];

    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Generic => "Generic",
            Self::Idle => "Idle",
            Self::Walk => "Walk",
            Self::Run => "Run",
            Self::Turn => "Turn",
            Self::Roll => "Roll",
            Self::Backstep => "Backstep",
            Self::Attack => "Attack",
            Self::Hit => "Hit",
            Self::Death => "Death",
        }
    }

    /// Guess a role from a clip/resource name.
    pub fn guess_from_name(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("idle") {
            Self::Idle
        } else if name.contains("run") {
            Self::Run
        } else if name.contains("backstep")
            || name.contains("back_step")
            || name.contains("back step")
            || name.contains("step_back")
            || name.contains("step back")
        {
            Self::Backstep
        } else if name.contains("roll") || name.contains("dodge") {
            Self::Roll
        } else if name.contains("walk") {
            Self::Walk
        } else if name.contains("turn") {
            Self::Turn
        } else if name.contains("attack") || name.contains("combo") || name.contains("melee") {
            Self::Attack
        } else if name.contains("hit") || name.contains("reaction") {
            Self::Hit
        } else if name.contains("death") || name.contains("dead") {
            Self::Death
        } else {
            Self::Generic
        }
    }
}

/// Gameplay action slots that can be driven by animation clips.
///
/// This is distinct from [`AnimationRole`]: a clip's role describes
/// what the source appears to be, while a character action says how
/// the game will use it. Authors may bind any compatible clip to any
/// action.
pub const CHARACTER_ANIMATION_ACTION_COUNT: usize = psx_level::CHARACTER_ANIMATION_ACTION_COUNT;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CharacterAnimationAction {
    #[default]
    Idle,
    Walk,
    Run,
    Turn,
    Roll,
    Backstep,
    LightAttack,
    HeavyAttack,
    ComboAttack,
    Block,
    HitReact,
    Death,
}

impl CharacterAnimationAction {
    pub const ALL: [Self; CHARACTER_ANIMATION_ACTION_COUNT] = [
        Self::Idle,
        Self::Walk,
        Self::Run,
        Self::Turn,
        Self::Roll,
        Self::Backstep,
        Self::LightAttack,
        Self::HeavyAttack,
        Self::ComboAttack,
        Self::Block,
        Self::HitReact,
        Self::Death,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Walk => "Walk",
            Self::Run => "Run",
            Self::Turn => "Turn",
            Self::Roll => "Roll",
            Self::Backstep => "Backstep",
            Self::LightAttack => "Light Attack",
            Self::HeavyAttack => "Heavy Attack",
            Self::ComboAttack => "Combo Attack",
            Self::Block => "Block",
            Self::HitReact => "Hit React",
            Self::Death => "Death",
        }
    }

    pub const fn to_index(self) -> usize {
        match self {
            Self::Idle => 0,
            Self::Walk => 1,
            Self::Run => 2,
            Self::Turn => 3,
            Self::Roll => 4,
            Self::Backstep => 5,
            Self::LightAttack => 6,
            Self::HeavyAttack => 7,
            Self::ComboAttack => 8,
            Self::Block => 9,
            Self::HitReact => 10,
            Self::Death => 11,
        }
    }

    pub const fn role_hint(self) -> Option<AnimationRole> {
        match self {
            Self::Idle => Some(AnimationRole::Idle),
            Self::Walk => Some(AnimationRole::Walk),
            Self::Run => Some(AnimationRole::Run),
            Self::Turn => Some(AnimationRole::Turn),
            Self::Roll => Some(AnimationRole::Roll),
            Self::Backstep => Some(AnimationRole::Backstep),
            Self::LightAttack | Self::HeavyAttack | Self::ComboAttack | Self::Block => {
                Some(AnimationRole::Attack)
            }
            Self::HitReact => Some(AnimationRole::Hit),
            Self::Death => Some(AnimationRole::Death),
        }
    }

    pub const fn required_for_player(self) -> bool {
        matches!(self, Self::Idle | Self::Walk)
    }

    pub const fn loops_by_default(self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Walk | Self::Run | Self::Turn | Self::Block
        )
    }

    pub fn guess_from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.contains("idle") {
            Some(Self::Idle)
        } else if name.contains("run") {
            Some(Self::Run)
        } else if name.contains("backstep")
            || name.contains("back_step")
            || name.contains("back step")
            || name.contains("step_back")
            || name.contains("step back")
        {
            Some(Self::Backstep)
        } else if name.contains("roll") || name.contains("dodge") {
            Some(Self::Roll)
        } else if name.contains("walk") {
            Some(Self::Walk)
        } else if name.contains("turn") {
            Some(Self::Turn)
        } else if name.contains("death") || name.contains("dead") {
            Some(Self::Death)
        } else if name.contains("hit") || name.contains("reaction") {
            Some(Self::HitReact)
        } else if name.contains("block") || name.contains("guard") {
            Some(Self::Block)
        } else if name.contains("combo") {
            Some(Self::ComboAttack)
        } else if name.contains("heavy") || name.contains("strong") {
            Some(Self::HeavyAttack)
        } else if name.contains("light") || name.contains("attack") || name.contains("melee") {
            Some(Self::LightAttack)
        } else {
            None
        }
    }
}

/// Resource-based action binding used by Animation Sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationActionBinding {
    pub action: CharacterAnimationAction,
    pub clip: ResourceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CharacterActionOptions>,
}

/// Model-local fallback action binding used directly on Characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterActionClip {
    pub action: CharacterAnimationAction,
    pub clip: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<CharacterActionOptions>,
}

/// Q8 fixed-point unit (`1.0x`) for per-action playback speed.
pub const ACTION_SPEED_UNSCALED_Q8: u16 = 256;
/// Authoring lower clamp for per-action playback speed (`0.25x`).
pub const ACTION_SPEED_MIN_Q8: u16 = 64;
/// Authoring upper clamp for per-action playback speed (`4.0x`).
pub const ACTION_SPEED_MAX_Q8: u16 = 1024;

/// Default per-action playback speed: `1.0x` (unscaled).
pub(crate) const fn default_action_speed_q8() -> u16 {
    ACTION_SPEED_UNSCALED_Q8
}

/// Per-action playback controls.
///
/// This deliberately belongs to the action binding, not the clip
/// resource: the same cooked animation can be used as a looping
/// locomotion fallback in one place and a one-shot action in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterActionOptions {
    #[serde(default)]
    pub looping: bool,
    #[serde(default = "default_true")]
    pub in_place: bool,
    /// Playback speed multiplier in Q8 fixed point (`256 = 1.0x`).
    /// Scales how fast this action's cooked clip advances at runtime:
    /// `< 256` plays slower, `> 256` plays faster. Authoring clamps to
    /// [`ACTION_SPEED_MIN_Q8`]..=[`ACTION_SPEED_MAX_Q8`].
    #[serde(default = "default_action_speed_q8")]
    pub speed_q8: u16,
}

impl CharacterActionOptions {
    pub const fn for_action(action: CharacterAnimationAction) -> Self {
        Self {
            looping: action.loops_by_default(),
            in_place: true,
            speed_q8: ACTION_SPEED_UNSCALED_Q8,
        }
    }
}

/// Where an authoring-time animation candidate came from. The source
/// kind is editor metadata only; runtime receives already-cooked
/// `.psxanim` clips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationSourceProvider {
    #[default]
    Unknown,
    Meshy,
    Mixamo,
    Synty,
    Other,
}

impl AnimationSourceProvider {
    pub const ALL: [Self; 5] = [
        Self::Unknown,
        Self::Meshy,
        Self::Mixamo,
        Self::Synty,
        Self::Other,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Meshy => "Meshy",
            Self::Mixamo => "Mixamo",
            Self::Synty => "Synty",
            Self::Other => "Other",
        }
    }

    pub fn guess_from_path(path: &str) -> Self {
        let lowered = path.to_ascii_lowercase();
        if lowered.contains("meshy") {
            Self::Meshy
        } else if lowered.contains("mixamo") || lowered.contains("standalone_fbx") {
            Self::Mixamo
        } else if lowered.contains("synty")
            || lowered.contains("sword_combat")
            || lowered.contains("sourcefiles/animations/polygon")
            || lowered.contains("sourcefiles/animations/sidekick")
        {
            Self::Synty
        } else {
            Self::Unknown
        }
    }
}

/// Authoring-time animation library entry. A source may be a raw FBX /
/// GLB clip, or a legacy cooked clip that has not yet been traced back
/// to its raw source. It is never consumed directly by the runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationSourceResource {
    /// Source file path. Prefer raw `.fbx` / `.glb` assets; legacy
    /// catalogued projects may point at an existing `.psxanim`.
    pub source_path: String,
    /// Clip/take name inside the source file.
    #[serde(default)]
    pub clip_name: String,
    /// Source provider hint used by the future retargeting pipeline.
    #[serde(default)]
    pub provider: AnimationSourceProvider,
    /// Optional source skeleton metadata when the importer knows it.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    /// Optional target model when this source is known to be authored
    /// specifically for one Meshy character/export.
    #[serde(default)]
    pub target_model: Option<ResourceId>,
    /// Semantic role used for filtering and assignment.
    #[serde(default)]
    pub role: AnimationRole,
    /// Whether this source is expected to loop when used.
    #[serde(default = "default_true")]
    pub looping: bool,
    /// Searchable editor tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl AnimationSourceResource {
    pub fn from_path(path: impl Into<String>, clip_name: impl Into<String>) -> Self {
        let source_path = path.into();
        let clip_name = clip_name.into();
        let role = AnimationRole::guess_from_name(if clip_name.is_empty() {
            &source_path
        } else {
            &clip_name
        });
        Self {
            provider: AnimationSourceProvider::guess_from_path(&source_path),
            source_path,
            clip_name,
            skeleton: None,
            target_model: None,
            role,
            looping: !matches!(
                role,
                AnimationRole::Roll
                    | AnimationRole::Backstep
                    | AnimationRole::Attack
                    | AnimationRole::Hit
                    | AnimationRole::Death
            ),
            tags: if matches!(role, AnimationRole::Generic) {
                Vec::new()
            } else {
                vec![role.label().to_ascii_lowercase()]
            },
        }
    }
}

/// How a cooked `.psxanim` was produced. This is editor metadata used
/// to avoid treating raw source-compatible clips as if they were
/// universally safe for every model on the same parent table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationClipBakeKind {
    /// Legacy or hand-authored resource. Kept playable for existing
    /// projects, but new imports should prefer a more specific value.
    #[default]
    LegacyShared,
    /// Cooked directly from animation data authored with the target
    /// model/export.
    ModelNative,
    /// Cooked from a source clip after retargeting to a target model.
    Retargeted,
}

impl AnimationClipBakeKind {
    pub const ALL: [Self; 3] = [Self::LegacyShared, Self::ModelNative, Self::Retargeted];

    pub const fn label(self) -> &'static str {
        match self {
            Self::LegacyShared => "Legacy/shared",
            Self::ModelNative => "Model native",
            Self::Retargeted => "Retargeted",
        }
    }
}

/// Standalone cooked animation clip. This is the runtime-ready result:
/// either model-native, retargeted to one target model, or legacy
/// skeleton-shared data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationClipResource {
    /// Path to the cooked `.psxanim` artifact.
    pub psxanim_path: String,
    /// Skeleton this clip targets.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    /// Optional authoring source this cooked clip was baked from.
    #[serde(default)]
    pub source: Option<ResourceId>,
    /// Optional model this cooked clip was baked for. When present,
    /// `resolved_model_animation_clips` only exposes the clip to that
    /// exact model.
    #[serde(default)]
    pub target_model: Option<ResourceId>,
    /// Bake provenance. Runtime ignores this; editor tooling uses it
    /// to distinguish native Meshy clips from future retargeted Mixamo
    /// clips.
    #[serde(default)]
    pub bake: AnimationClipBakeKind,
    /// Semantic role used by auto-assignment and animation sets.
    #[serde(default)]
    pub role: AnimationRole,
    /// Whether gameplay should loop this clip by default.
    #[serde(default = "default_true")]
    pub looping: bool,
    /// Searchable editor tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Per-clip model placement controls used by editor preview and
    /// cooked runtime rendering.
    #[serde(default, skip_serializing_if = "AnimationClipCalibration::is_default")]
    pub calibration: AnimationClipCalibration,
}

impl AnimationClipResource {
    /// Mirror this resource into the legacy model-local clip shape.
    pub fn as_model_clip(&self, name: impl Into<String>) -> ModelAnimationClip {
        ModelAnimationClip {
            name: name.into(),
            psxanim_path: self.psxanim_path.clone(),
            calibration: self.calibration,
        }
    }
}

/// Reusable role mapping for one skeleton. Characters combine a
/// visual model with an Animation Set rather than raw clip indices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationSetResource {
    /// Skeleton every assigned clip must target.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    #[serde(default)]
    pub idle_clip: Option<ResourceId>,
    #[serde(default)]
    pub walk_clip: Option<ResourceId>,
    #[serde(default)]
    pub run_clip: Option<ResourceId>,
    #[serde(default)]
    pub turn_clip: Option<ResourceId>,
    #[serde(default)]
    pub roll_clip: Option<ResourceId>,
    #[serde(default)]
    pub backstep_clip: Option<ResourceId>,
    /// Preferred action mapping. These bindings are used first
    /// when cooking and let authors assign any compatible clip to
    /// any gameplay action.
    #[serde(default)]
    pub action_clips: Vec<AnimationActionBinding>,
    /// Extra clips included with the set, such as attacks, hit
    /// reactions, death clips, emotes, and experiments.
    #[serde(default)]
    pub clips: Vec<ResourceId>,
}

impl AnimationSetResource {
    pub const fn defaults() -> Self {
        Self {
            skeleton: None,
            idle_clip: None,
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            roll_clip: None,
            backstep_clip: None,
            action_clips: Vec::new(),
            clips: Vec::new(),
        }
    }

    pub fn action_clip(&self, action: CharacterAnimationAction) -> Option<ResourceId> {
        self.action_clips
            .iter()
            .find_map(|binding| (binding.action == action).then_some(binding.clip))
            .or_else(|| action.role_hint().and_then(|role| self.role_clip(role)))
    }

    pub fn action_binding(
        &self,
        action: CharacterAnimationAction,
    ) -> Option<&AnimationActionBinding> {
        self.action_clips
            .iter()
            .find(|binding| binding.action == action)
    }

    pub fn set_action_clip(&mut self, action: CharacterAnimationAction, clip: Option<ResourceId>) {
        if let Some(role) = action.role_hint() {
            if let Some(slot) = self.role_clip_mut(role) {
                *slot = None;
            }
        }
        match clip {
            Some(clip) => {
                if let Some(binding) = self
                    .action_clips
                    .iter_mut()
                    .find(|binding| binding.action == action)
                {
                    binding.clip = clip;
                } else {
                    self.action_clips.push(AnimationActionBinding {
                        action,
                        clip,
                        options: None,
                    });
                }
            }
            None => self.action_clips.retain(|binding| binding.action != action),
        }
    }

    pub fn role_clip(&self, role: AnimationRole) -> Option<ResourceId> {
        match role {
            AnimationRole::Idle => self.idle_clip,
            AnimationRole::Walk => self.walk_clip,
            AnimationRole::Run => self.run_clip,
            AnimationRole::Turn => self.turn_clip,
            AnimationRole::Roll => self.roll_clip,
            AnimationRole::Backstep => self.backstep_clip,
            AnimationRole::Generic
            | AnimationRole::Attack
            | AnimationRole::Hit
            | AnimationRole::Death => None,
        }
    }

    pub fn role_clip_mut(&mut self, role: AnimationRole) -> Option<&mut Option<ResourceId>> {
        match role {
            AnimationRole::Idle => Some(&mut self.idle_clip),
            AnimationRole::Walk => Some(&mut self.walk_clip),
            AnimationRole::Run => Some(&mut self.run_clip),
            AnimationRole::Turn => Some(&mut self.turn_clip),
            AnimationRole::Roll => Some(&mut self.roll_clip),
            AnimationRole::Backstep => Some(&mut self.backstep_clip),
            AnimationRole::Generic
            | AnimationRole::Attack
            | AnimationRole::Hit
            | AnimationRole::Death => None,
        }
    }
}

impl Default for AnimationSetResource {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Named model attachment point, usually bound to a skeleton
/// joint. Runtime composition is:
/// `entity transform × joint pose × socket local transform`.
///
/// Offsets are integer model/engine units and rotations are Q12
/// turn units (`4096 = 360°`) so project data can be cooked
/// directly for the PS1 without preserving floats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentSocket {
    /// User-facing socket name (`right_hand_grip`, `back_slot`, …).
    pub name: String,
    /// Joint index in the cooked `.psxmdl` skeleton.
    pub joint: u16,
    /// Local translation relative to the joint pose.
    #[serde(default)]
    pub translation: [i32; 3],
    /// Local Euler rotation in Q12 turns: X / Y / Z, 4096 per turn.
    #[serde(default)]
    pub rotation_q12: [i16; 3],
}

impl AttachmentSocket {
    /// Common right-hand default for humanoid rigs.
    pub fn right_hand_grip() -> Self {
        Self {
            name: default_character_socket(),
            joint: 0,
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        }
    }
}

/// Pivot on a weapon model that should land on a character socket.
/// A sword normally uses `grip`; a shield might use `handle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponGrip {
    /// User-facing grip/pivot name.
    pub name: String,
    /// Local translation inside the weapon model.
    #[serde(default)]
    pub translation: [i32; 3],
    /// Local Euler rotation in Q12 turns: X / Y / Z, 4096 per turn.
    #[serde(default)]
    pub rotation_q12: [i16; 3],
}

impl Default for WeaponGrip {
    fn default() -> Self {
        Self {
            name: default_weapon_grip(),
            translation: [0, 0, 0],
            rotation_q12: [0, 0, 0],
        }
    }
}

/// Weapon hit volume, stored relative to the weapon grip/pivot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaponHitShape {
    /// Oriented box hit volume. `half_extents` are local axes.
    Box {
        /// Local center relative to the weapon grip.
        center: [i32; 3],
        /// Half extents in engine/model units.
        half_extents: [u16; 3],
    },
    /// Capsule hit volume, useful for blades, clubs, and spears.
    Capsule {
        /// Local capsule start relative to the weapon grip.
        start: [i32; 3],
        /// Local capsule end relative to the weapon grip.
        end: [i32; 3],
        /// Capsule radius in engine/model units.
        radius: u16,
    },
}

impl Default for WeaponHitShape {
    fn default() -> Self {
        Self::Capsule {
            start: [0, 0, 0],
            end: [0, 512, 0],
            radius: 48,
        }
    }
}

/// One named active hitbox window for a weapon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponHitbox {
    /// User-facing hitbox name.
    pub name: String,
    /// Local hit volume.
    #[serde(default)]
    pub shape: WeaponHitShape,
    /// First animation frame where the hitbox is active.
    #[serde(default)]
    pub active_start_frame: u16,
    /// Last animation frame where the hitbox is active.
    #[serde(default)]
    pub active_end_frame: u16,
}

impl Default for WeaponHitbox {
    fn default() -> Self {
        Self {
            name: "Main Hit".to_string(),
            shape: WeaponHitShape::default(),
            active_start_frame: 0,
            active_end_frame: 0,
        }
    }
}

/// Gameplay weapon resource: model reference, grip/pivot, and
/// authored attack hit volumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeaponResource {
    /// Visual model used for the weapon. `None` is allowed during
    /// authoring so hitboxes can be blocked in before art lands.
    #[serde(default)]
    pub model: Option<ResourceId>,
    /// Which character socket this weapon expects by default.
    #[serde(default = "default_character_socket")]
    pub default_character_socket: String,
    /// Weapon-local grip/pivot that aligns to the character socket.
    #[serde(default)]
    pub grip: WeaponGrip,
    /// Hit volumes authored relative to [`Self::grip`].
    #[serde(default)]
    pub hitboxes: Vec<WeaponHitbox>,
}

impl WeaponResource {
    /// Minimal editable weapon.
    pub fn defaults() -> Self {
        Self {
            model: None,
            default_character_socket: default_character_socket(),
            grip: WeaponGrip::default(),
            hitboxes: vec![WeaponHitbox::default()],
        }
    }
}

impl Default for WeaponResource {
    fn default() -> Self {
        Self::defaults()
    }
}

pub(crate) fn default_character_socket() -> String {
    "right_hand_grip".to_string()
}

pub(crate) fn default_weapon_grip() -> String {
    "grip".to_string()
}

/// Cooked PSX model bundle: a `.psxmdl` plus optional atlas
/// `.psxt` plus zero or more `.psxanim` clips.
///
/// All paths follow the project-relative resolution rule shared
/// with `Texture` resources. `clips` is ordered deterministically
/// (by file name at registration time); `default_clip` /
/// `preview_clip` index into that list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResource {
    /// Path to the cooked `.psxmdl` artifact.
    pub model_path: String,
    /// Original GLB/glTF/FBX source used to cook this model, when
    /// known. The editor uses this to bake additional animation
    /// sources against the same skeleton.
    #[serde(default)]
    pub source_path: Option<String>,
    /// Optional atlas. Required for textured rendering at runtime;
    /// omitting is allowed for placeholder / debug bundles.
    #[serde(default)]
    pub texture_path: Option<String>,
    /// Skeleton this model was cooked against. Models can still
    /// carry legacy local clips for compatibility, but shared
    /// animation matching should use this resource id.
    #[serde(default)]
    pub skeleton: Option<ResourceId>,
    /// Cooked animation clips, sorted by file name. Empty for
    /// static models (rendered in bind pose).
    #[serde(default)]
    pub clips: Vec<ModelAnimationClip>,
    /// Index into `clips` used at runtime when no per-instance
    /// override is set. `None` means "first clip if any, else
    /// bind pose".
    #[serde(default)]
    pub default_clip: Option<u16>,
    /// Index into `clips` shown in the editor inspector preview.
    /// Falls back to `default_clip` when unset.
    #[serde(default)]
    pub preview_clip: Option<u16>,
    /// Suggested world-space height in engine units (mirrors the
    /// value the cooker stamped into the `.psxmdl` header). Used
    /// by the inspector for sanity checks and by the editor
    /// preview to size selection gizmos.
    #[serde(default = "default_model_world_height")]
    pub world_height: u16,
    /// Authored coarse collision radius in engine units. The
    /// runtime treats model actors as vertical cylinders for
    /// PS1-scale movement/collision.
    #[serde(default = "default_model_collision_radius")]
    pub collision_radius: u16,
    /// Authored bake-time scale in Q8 fixed point (`256 = 1.0`).
    /// Stored as integers so project data mirrors the PS1/runtime
    /// constraint; any application to mesh data must happen during
    /// cook/import, not as runtime floats.
    #[serde(default = "default_model_scale_q8")]
    pub scale_q8: [u16; 3],
    /// Named sockets used by equipment, VFX, and hitbox authoring.
    #[serde(default)]
    pub attachments: Vec<AttachmentSocket>,
}

pub(crate) const fn default_model_world_height() -> u16 {
    1024
}

pub(crate) const fn default_model_collision_radius() -> u16 {
    default_model_collision_radius_for_height(default_model_world_height())
}

pub const fn default_model_collision_radius_for_height(world_height: u16) -> u16 {
    let scaled = (world_height as u32 * 3) / 16;
    if scaled < 80 {
        80
    } else if scaled > 384 {
        384
    } else {
        scaled as u16
    }
}

impl ModelResource {
    /// Human-readable scale factor for one axis.
    pub fn scale_axis(&self, axis: usize) -> f32 {
        self.scale_q8
            .get(axis)
            .copied()
            .unwrap_or(MODEL_SCALE_ONE_Q8) as f32
            / MODEL_SCALE_ONE_Q8 as f32
    }

    /// Index of the clip the editor inspector should preview --
    /// `preview_clip` if set, else `default_clip`, else `None`.
    pub fn effective_preview_clip(&self) -> Option<u16> {
        self.preview_clip.or(self.default_clip)
    }

    /// Index of the clip a runtime instance with no override
    /// should play -- `default_clip` if set, else clip 0 if any
    /// clip exists, else `None`.
    pub fn effective_runtime_clip(&self) -> Option<u16> {
        self.default_clip
            .or_else(|| (!self.clips.is_empty()).then_some(0))
    }
}

/// Gameplay metadata layered on top of a Model. The Model owns
/// the `.psxmdl` / `.psxt` / `.psxanim` artifacts; the Character
/// names which clips fill the idle / walk / run / turn roles.
///
/// Authoring may leave the model unset (the resource still
/// validates to support partial setup); a Character assigned to
/// the player spawn must resolve to a Model with valid idle and
/// walk clips at cook time.
///
/// Engine units throughout -- same convention used by the rest
/// of the runtime (`sector_size = 1024`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterResource {
    /// Backing model. `None` is allowed during authoring;
    /// validated at cook time when assigned to the player.
    #[serde(default)]
    pub model: Option<ResourceId>,
    /// Preferred reusable animation set. When present, cook/preview
    /// resolve role clips from this set and fall back to the legacy
    /// per-model clip indices only for unset roles.
    #[serde(default)]
    pub animation_set: Option<ResourceId>,
    /// Index into the model's clip list -- played when the
    /// character has no movement input. Required for the player.
    #[serde(default)]
    pub idle_clip: Option<u16>,
    /// Index into the model's clip list -- played while walking.
    /// Required for the player.
    #[serde(default)]
    pub walk_clip: Option<u16>,
    /// Index into the model's clip list -- optional run clip.
    #[serde(default)]
    pub run_clip: Option<u16>,
    /// Index into the model's clip list -- optional turn clip.
    #[serde(default)]
    pub turn_clip: Option<u16>,
    /// Index into the model's clip list -- optional roll clip.
    #[serde(default)]
    pub roll_clip: Option<u16>,
    /// Index into the model's clip list -- optional backstep clip.
    #[serde(default)]
    pub backstep_clip: Option<u16>,
    /// Model-local fallback action bindings. Animation Set
    /// bindings are preferred when present.
    #[serde(default)]
    pub action_clips: Vec<CharacterActionClip>,
    /// Capsule radius (engine units). Used by collision +
    /// editor preview gizmo.
    pub radius: u16,
    /// Capsule height (engine units).
    pub height: u16,
    /// Forward walk speed in engine units per frame at 60 Hz.
    pub walk_speed: i32,
    /// Forward run speed in engine units per frame at 60 Hz.
    pub run_speed: i32,
    /// Yaw rate the controller applies when turning.
    pub turn_speed_degrees_per_second: u16,
    /// Maximum stamina. Uses the runtime's Q12-style stamina units.
    #[serde(default = "default_character_stamina_max_q12")]
    pub stamina_max_q12: i32,
    /// Minimum stamina required to start sprinting.
    #[serde(default = "default_character_sprint_min_q12")]
    pub sprint_min_q12: i32,
    /// Stamina drained per 60 Hz sprint frame.
    #[serde(default = "default_character_sprint_drain_q12")]
    pub sprint_drain_q12: i32,
    /// Stamina recovered per grounded non-sprint frame.
    #[serde(default = "default_character_stamina_recover_q12")]
    pub stamina_recover_q12: i32,
    /// Stamina spent to start a roll.
    #[serde(default = "default_character_roll_cost_q12")]
    pub roll_cost_q12: i32,
    /// Roll travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_roll_speed")]
    pub roll_speed: i32,
    /// Frames where the roll keeps moving.
    #[serde(default = "default_character_roll_active_frames")]
    pub roll_active_frames: u8,
    /// Recovery frames after roll movement ends.
    #[serde(default = "default_character_roll_recovery_frames")]
    pub roll_recovery_frames: u8,
    /// Invulnerable frames from roll start.
    #[serde(default = "default_character_roll_invulnerable_frames")]
    pub roll_invulnerable_frames: u8,
    /// Stamina spent to start a backstep.
    #[serde(default = "default_character_backstep_cost_q12")]
    pub backstep_cost_q12: i32,
    /// Backstep travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_backstep_speed")]
    pub backstep_speed: i32,
    /// Frames where the backstep keeps moving.
    #[serde(default = "default_character_backstep_active_frames")]
    pub backstep_active_frames: u8,
    /// Recovery frames after backstep movement ends.
    #[serde(default = "default_character_backstep_recovery_frames")]
    pub backstep_recovery_frames: u8,
    /// Invulnerable frames from backstep start.
    #[serde(default = "default_character_backstep_invulnerable_frames")]
    pub backstep_invulnerable_frames: u8,
    /// Distance the third-person camera trails the character.
    pub camera_distance: i32,
    /// Camera vertical offset above the character origin.
    pub camera_height: i32,
    /// Vertical offset of the camera's look-at target above
    /// the character origin (typically around the upper torso
    /// for comfortable third-person framing).
    pub camera_target_height: i32,
}

impl CharacterResource {
    /// Sensible defaults for a humanoid third-person character.
    /// Sized for the starter project's 1024-unit sector grid.
    pub const fn defaults() -> Self {
        Self {
            model: None,
            animation_set: None,
            idle_clip: None,
            walk_clip: None,
            run_clip: None,
            turn_clip: None,
            roll_clip: None,
            backstep_clip: None,
            action_clips: Vec::new(),
            radius: default_character_radius(),
            height: default_character_height(),
            walk_speed: default_character_walk_speed(),
            run_speed: default_character_run_speed(),
            turn_speed_degrees_per_second: default_character_turn_speed_degrees_per_second(),
            stamina_max_q12: default_character_stamina_max_q12(),
            sprint_min_q12: default_character_sprint_min_q12(),
            sprint_drain_q12: default_character_sprint_drain_q12(),
            stamina_recover_q12: default_character_stamina_recover_q12(),
            roll_cost_q12: default_character_roll_cost_q12(),
            roll_speed: default_character_roll_speed(),
            roll_active_frames: default_character_roll_active_frames(),
            roll_recovery_frames: default_character_roll_recovery_frames(),
            roll_invulnerable_frames: default_character_roll_invulnerable_frames(),
            backstep_cost_q12: default_character_backstep_cost_q12(),
            backstep_speed: default_character_backstep_speed(),
            backstep_active_frames: default_character_backstep_active_frames(),
            backstep_recovery_frames: default_character_backstep_recovery_frames(),
            backstep_invulnerable_frames: default_character_backstep_invulnerable_frames(),
            camera_distance: 6144,
            camera_height: 1280,
            camera_target_height: 640,
        }
    }

    pub fn action_clip(&self, action: CharacterAnimationAction) -> Option<u16> {
        self.action_clips
            .iter()
            .find_map(|binding| (binding.action == action).then_some(binding.clip))
            .or_else(|| match action {
                CharacterAnimationAction::Idle => self.idle_clip,
                CharacterAnimationAction::Walk => self.walk_clip,
                CharacterAnimationAction::Run => self.run_clip,
                CharacterAnimationAction::Turn => self.turn_clip,
                CharacterAnimationAction::Roll => self.roll_clip,
                CharacterAnimationAction::Backstep => self.backstep_clip,
                CharacterAnimationAction::LightAttack
                | CharacterAnimationAction::HeavyAttack
                | CharacterAnimationAction::ComboAttack
                | CharacterAnimationAction::Block
                | CharacterAnimationAction::HitReact
                | CharacterAnimationAction::Death => None,
            })
    }

    pub fn action_binding(&self, action: CharacterAnimationAction) -> Option<&CharacterActionClip> {
        self.action_clips
            .iter()
            .find(|binding| binding.action == action)
    }

    pub fn set_action_clip(&mut self, action: CharacterAnimationAction, clip: Option<u16>) {
        match action {
            CharacterAnimationAction::Idle => self.idle_clip = None,
            CharacterAnimationAction::Walk => self.walk_clip = None,
            CharacterAnimationAction::Run => self.run_clip = None,
            CharacterAnimationAction::Turn => self.turn_clip = None,
            CharacterAnimationAction::Roll => self.roll_clip = None,
            CharacterAnimationAction::Backstep => self.backstep_clip = None,
            CharacterAnimationAction::LightAttack
            | CharacterAnimationAction::HeavyAttack
            | CharacterAnimationAction::ComboAttack
            | CharacterAnimationAction::Block
            | CharacterAnimationAction::HitReact
            | CharacterAnimationAction::Death => {}
        }
        match clip {
            Some(clip) => {
                if let Some(binding) = self
                    .action_clips
                    .iter_mut()
                    .find(|binding| binding.action == action)
                {
                    binding.clip = clip;
                } else {
                    self.action_clips.push(CharacterActionClip {
                        action,
                        clip,
                        options: None,
                    });
                }
            }
            None => self.action_clips.retain(|binding| binding.action != action),
        }
    }
}

pub(crate) const fn default_character_stamina_max_q12() -> i32 {
    4096
}

pub(crate) const fn default_character_radius() -> u16 {
    192
}

pub(crate) const fn default_character_height() -> u16 {
    1024
}

pub(crate) const fn default_character_walk_speed() -> i32 {
    48
}

pub(crate) const fn default_character_run_speed() -> i32 {
    96
}

pub(crate) const fn default_character_turn_speed_degrees_per_second() -> u16 {
    180
}

pub(crate) const fn default_character_sprint_min_q12() -> i32 {
    384
}

pub(crate) const fn default_character_sprint_drain_q12() -> i32 {
    10
}

pub(crate) const fn default_character_stamina_recover_q12() -> i32 {
    36
}

pub(crate) const fn default_character_roll_cost_q12() -> i32 {
    768
}

pub(crate) const fn default_character_roll_speed() -> i32 {
    96
}

pub(crate) const fn default_character_roll_active_frames() -> u8 {
    14
}

pub(crate) const fn default_character_roll_recovery_frames() -> u8 {
    12
}

pub(crate) const fn default_character_roll_invulnerable_frames() -> u8 {
    10
}

pub(crate) const fn default_character_backstep_cost_q12() -> i32 {
    512
}

pub(crate) const fn default_character_backstep_speed() -> i32 {
    72
}

pub(crate) const fn default_character_backstep_active_frames() -> u8 {
    8
}

pub(crate) const fn default_character_backstep_recovery_frames() -> u8 {
    10
}

pub(crate) const fn default_character_backstep_invulnerable_frames() -> u8 {
    6
}

impl Default for CharacterResource {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Tunable movement/collision settings authored on a
/// [`NodeKind::CharacterController`] component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterControllerSettings {
    /// Capsule radius (engine units).
    #[serde(default = "default_character_radius")]
    pub radius: u16,
    /// Capsule height (engine units).
    #[serde(default = "default_character_height")]
    pub height: u16,
    /// Forward walk speed in engine units per frame at 60 Hz.
    #[serde(default = "default_character_walk_speed")]
    pub walk_speed: i32,
    /// Forward run speed in engine units per frame at 60 Hz.
    #[serde(default = "default_character_run_speed")]
    pub run_speed: i32,
    /// Yaw rate the controller applies when turning.
    #[serde(default = "default_character_turn_speed_degrees_per_second")]
    pub turn_speed_degrees_per_second: u16,
    /// Maximum stamina. Uses the runtime's Q12-style stamina units.
    #[serde(default = "default_character_stamina_max_q12")]
    pub stamina_max_q12: i32,
    /// Minimum stamina required to start sprinting.
    #[serde(default = "default_character_sprint_min_q12")]
    pub sprint_min_q12: i32,
    /// Stamina drained per 60 Hz sprint frame.
    #[serde(default = "default_character_sprint_drain_q12")]
    pub sprint_drain_q12: i32,
    /// Stamina recovered per grounded non-sprint frame.
    #[serde(default = "default_character_stamina_recover_q12")]
    pub stamina_recover_q12: i32,
    /// Stamina spent to start a roll.
    #[serde(default = "default_character_roll_cost_q12")]
    pub roll_cost_q12: i32,
    /// Roll travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_roll_speed")]
    pub roll_speed: i32,
    /// Frames where the roll keeps moving.
    #[serde(default = "default_character_roll_active_frames")]
    pub roll_active_frames: u8,
    /// Recovery frames after roll movement ends.
    #[serde(default = "default_character_roll_recovery_frames")]
    pub roll_recovery_frames: u8,
    /// Invulnerable frames from roll start.
    #[serde(default = "default_character_roll_invulnerable_frames")]
    pub roll_invulnerable_frames: u8,
    /// Stamina spent to start a backstep.
    #[serde(default = "default_character_backstep_cost_q12")]
    pub backstep_cost_q12: i32,
    /// Backstep travel speed in engine units per 60 Hz frame.
    #[serde(default = "default_character_backstep_speed")]
    pub backstep_speed: i32,
    /// Frames where the backstep keeps moving.
    #[serde(default = "default_character_backstep_active_frames")]
    pub backstep_active_frames: u8,
    /// Recovery frames after backstep movement ends.
    #[serde(default = "default_character_backstep_recovery_frames")]
    pub backstep_recovery_frames: u8,
    /// Invulnerable frames from backstep start.
    #[serde(default = "default_character_backstep_invulnerable_frames")]
    pub backstep_invulnerable_frames: u8,
}

impl CharacterControllerSettings {
    pub const fn defaults() -> Self {
        Self {
            radius: default_character_radius(),
            height: default_character_height(),
            walk_speed: default_character_walk_speed(),
            run_speed: default_character_run_speed(),
            turn_speed_degrees_per_second: default_character_turn_speed_degrees_per_second(),
            stamina_max_q12: default_character_stamina_max_q12(),
            sprint_min_q12: default_character_sprint_min_q12(),
            sprint_drain_q12: default_character_sprint_drain_q12(),
            stamina_recover_q12: default_character_stamina_recover_q12(),
            roll_cost_q12: default_character_roll_cost_q12(),
            roll_speed: default_character_roll_speed(),
            roll_active_frames: default_character_roll_active_frames(),
            roll_recovery_frames: default_character_roll_recovery_frames(),
            roll_invulnerable_frames: default_character_roll_invulnerable_frames(),
            backstep_cost_q12: default_character_backstep_cost_q12(),
            backstep_speed: default_character_backstep_speed(),
            backstep_active_frames: default_character_backstep_active_frames(),
            backstep_recovery_frames: default_character_backstep_recovery_frames(),
            backstep_invulnerable_frames: default_character_backstep_invulnerable_frames(),
        }
    }

    pub fn from_character(character: &CharacterResource) -> Self {
        Self {
            radius: character.radius,
            height: character.height,
            walk_speed: character.walk_speed,
            run_speed: character.run_speed,
            turn_speed_degrees_per_second: character.turn_speed_degrees_per_second,
            stamina_max_q12: character.stamina_max_q12,
            sprint_min_q12: character.sprint_min_q12,
            sprint_drain_q12: character.sprint_drain_q12,
            stamina_recover_q12: character.stamina_recover_q12,
            roll_cost_q12: character.roll_cost_q12,
            roll_speed: character.roll_speed,
            roll_active_frames: character.roll_active_frames,
            roll_recovery_frames: character.roll_recovery_frames,
            roll_invulnerable_frames: character.roll_invulnerable_frames,
            backstep_cost_q12: character.backstep_cost_q12,
            backstep_speed: character.backstep_speed,
            backstep_active_frames: character.backstep_active_frames,
            backstep_recovery_frames: character.backstep_recovery_frames,
            backstep_invulnerable_frames: character.backstep_invulnerable_frames,
        }
    }
}

impl Default for CharacterControllerSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

pub const DEFAULT_PARTICLE_EMITTER_MAX_PARTICLES: u16 = 32;
pub const DEFAULT_PARTICLE_EMITTER_SPAWN_RATE_Q8: u16 = 8 * 256;
pub const DEFAULT_PARTICLE_EMITTER_LIFETIME_FRAMES: u8 = 60;
pub const DEFAULT_PARTICLE_EMITTER_START_SIZE: u16 = 128;
pub const DEFAULT_PARTICLE_EMITTER_END_SIZE: u16 = 512;

pub(crate) const fn default_particle_emitter_enabled() -> bool {
    true
}

pub(crate) const fn default_particle_emitter_max_particles() -> u16 {
    DEFAULT_PARTICLE_EMITTER_MAX_PARTICLES
}

pub(crate) const fn default_particle_emitter_spawn_rate_q8() -> u16 {
    DEFAULT_PARTICLE_EMITTER_SPAWN_RATE_Q8
}

pub(crate) const fn default_particle_emitter_lifetime_frames() -> u8 {
    DEFAULT_PARTICLE_EMITTER_LIFETIME_FRAMES
}

pub(crate) const fn default_particle_emitter_start_size() -> u16 {
    DEFAULT_PARTICLE_EMITTER_START_SIZE
}

pub(crate) const fn default_particle_emitter_end_size() -> u16 {
    DEFAULT_PARTICLE_EMITTER_END_SIZE
}

pub(crate) const fn default_particle_emitter_start_color() -> [u8; 3] {
    [255, 255, 255]
}

pub(crate) const fn default_particle_emitter_end_color() -> [u8; 3] {
    [96, 96, 96]
}

pub(crate) const fn default_particle_emitter_blend_mode() -> PsxBlendMode {
    PsxBlendMode::Average
}

pub(crate) const fn default_particle_emitter_base_velocity_q4() -> [i16; 3] {
    [0, 24, 0]
}

pub(crate) const fn default_particle_emitter_random_velocity_q4() -> [u16; 3] {
    [16, 8, 16]
}

pub(crate) const fn default_particle_emitter_acceleration_q4() -> [i16; 3] {
    [0, 0, 0]
}

pub(crate) const fn default_particle_emitter_spawn_radius() -> u16 {
    0
}

/// Authoring settings for a cheap world-space particle emitter.
///
/// The intended runtime path is point-projected sprites: each live
/// particle owns a 3D position, projects one centre point, then draws a
/// screen-aligned textured sprite. Textures are authored or generated at
/// build time; this node never implies runtime texture generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticleEmitterSettings {
    /// Whether the emitter should run in playtest/runtime.
    #[serde(default = "default_particle_emitter_enabled")]
    pub enabled: bool,
    /// Optional 16x16 greyscale/white mask texture used for the sprite.
    #[serde(default)]
    pub texture: Option<ResourceId>,
    /// Hard live-particle cap owned by this emitter.
    #[serde(default = "default_particle_emitter_max_particles")]
    pub max_particles: u16,
    /// Spawn rate in particles/second, Q8 fixed point.
    #[serde(default = "default_particle_emitter_spawn_rate_q8")]
    pub spawn_rate_q8: u16,
    /// Particle lifetime in 60 Hz simulation frames.
    #[serde(default = "default_particle_emitter_lifetime_frames")]
    pub lifetime_frames: u8,
    /// Particle size at birth, in engine units before projection.
    #[serde(default = "default_particle_emitter_start_size")]
    pub start_size: u16,
    /// Particle size at death, in engine units before projection.
    #[serde(default = "default_particle_emitter_end_size")]
    pub end_size: u16,
    /// Tint at birth. Multiplies the greyscale texture.
    #[serde(default = "default_particle_emitter_start_color")]
    pub start_color: [u8; 3],
    /// Tint at death. Runtime fades between start/end tint.
    #[serde(default = "default_particle_emitter_end_color")]
    pub end_color: [u8; 3],
    /// PS1 semi-transparency mode used by the sprite packet.
    #[serde(default = "default_particle_emitter_blend_mode")]
    pub blend_mode: PsxBlendMode,
    /// Base velocity in Q4.4 engine units per 60 Hz frame.
    #[serde(default = "default_particle_emitter_base_velocity_q4")]
    pub base_velocity_q4: [i16; 3],
    /// Random velocity spread in Q4.4 engine units per 60 Hz frame.
    ///
    /// Runtime samples each component in `[-spread, +spread]`.
    #[serde(default = "default_particle_emitter_random_velocity_q4")]
    pub random_velocity_q4: [u16; 3],
    /// Constant acceleration in Q4.4 engine units per 60 Hz frame.
    #[serde(default = "default_particle_emitter_acceleration_q4")]
    pub acceleration_q4: [i16; 3],
    /// Random spawn offset radius around the emitter origin, in engine units.
    #[serde(default = "default_particle_emitter_spawn_radius")]
    pub spawn_radius: u16,
}

impl ParticleEmitterSettings {
    pub const fn defaults() -> Self {
        Self {
            enabled: default_particle_emitter_enabled(),
            texture: None,
            max_particles: default_particle_emitter_max_particles(),
            spawn_rate_q8: default_particle_emitter_spawn_rate_q8(),
            lifetime_frames: default_particle_emitter_lifetime_frames(),
            start_size: default_particle_emitter_start_size(),
            end_size: default_particle_emitter_end_size(),
            start_color: default_particle_emitter_start_color(),
            end_color: default_particle_emitter_end_color(),
            blend_mode: default_particle_emitter_blend_mode(),
            base_velocity_q4: default_particle_emitter_base_velocity_q4(),
            random_velocity_q4: default_particle_emitter_random_velocity_q4(),
            acceleration_q4: default_particle_emitter_acceleration_q4(),
            spawn_radius: default_particle_emitter_spawn_radius(),
        }
    }
}

impl Default for ParticleEmitterSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

/// Resource payloads available to editor scenes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceData {
    /// Cooked PSXT artifact reference.
    ///
    /// The editor and the runtime both consume the same `.psxt` blob
    /// -- the runtime via `include_bytes!` at compile time, the editor
    /// via `std::fs::read` at refresh time and `psx_asset::Texture::from_bytes`
    /// to extract pixel + CLUT bytes. PNG/JPG/BMP → PSXT cooking lives
    /// in `texture_import` and the `psxed tex` CLI; runtime paths still
    /// consume only cooked blobs.
    Texture {
        /// Path to the cooked `.psxt` artifact. Resolved at refresh
        /// time first as-is (absolute paths), then relative to the
        /// project file's directory, then relative to the workspace
        /// cwd. The starter project ships paths relative to the repo
        /// root so `cargo run -p frontend` from `/repos/psoxide` finds
        /// the canonical `assets/textures/*.psxt`.
        psxt_path: String,
    },
    /// Editor material.
    Material(MaterialResource),
    /// Cooked animated PSX model -- `.psxmdl` + optional `.psxt`
    /// atlas + animation clips. Instantiated in scenes by placing an
    /// [`NodeKind::Entity`] with a [`NodeKind::ModelRenderer`]
    /// component referencing this resource id.
    Model(ModelResource),
    /// Skeleton compatibility contract shared by models and
    /// standalone animation clips.
    Skeleton(SkeletonResource),
    /// Authoring-time animation library entry. Source clips are
    /// previewed / retargeted / baked by editor tooling; runtime uses
    /// [`ResourceData::AnimationClip`] only.
    AnimationSource(AnimationSourceResource),
    /// Standalone cooked animation clip bound to a skeleton.
    AnimationClip(AnimationClipResource),
    /// Reusable role mapping for characters on one skeleton.
    AnimationSet(AnimationSetResource),
    /// Legacy / generic source mesh path. Kept for backward
    /// compatibility; new authoring should use [`ResourceData::Model`].
    Mesh {
        /// Project-relative source path.
        source_path: String,
    },
    /// Nested room/prefab reference.
    Scene {
        /// Project-relative room/prefab path.
        source_path: String,
    },
    /// Script resource.
    Script {
        /// Project-relative script path.
        source_path: String,
    },
    /// Audio resource.
    Audio {
        /// Project-relative audio path.
        source_path: String,
    },
    /// Optional gameplay preset with model, animation, capsule, and
    /// camera defaults. Component-authored entities can override the
    /// visual model through Model Renderer, action clips through
    /// Animator, and movement tuning through Character Controller.
    Character(CharacterResource),
    /// Equipment/weapon authoring resource. A Weapon references a
    /// Model for visuals and owns grip + hitbox data for combat.
    Weapon(WeaponResource),
}

impl ResourceData {
    /// User-facing type label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Texture { .. } => "Texture",
            Self::Material(_) => "Material",
            Self::Model(_) => "Model",
            Self::Skeleton(_) => "Skeleton",
            Self::AnimationSource(_) => "Animation Source",
            Self::AnimationClip(_) => "Animation Clip",
            Self::AnimationSet(_) => "Clip Role Map",
            Self::Mesh { .. } => "Mesh",
            Self::Scene { .. } => "Room",
            Self::Script { .. } => "Script",
            Self::Audio { .. } => "Audio",
            Self::Character(_) => "Character Profile",
            Self::Weapon(_) => "Weapon",
        }
    }
}

/// One named project resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    /// Stable resource id.
    pub id: ResourceId,
    /// Display name.
    pub name: String,
    /// Payload.
    pub data: ResourceData,
}

/// One animation clip that a model can play after resolving both
/// legacy model-local clips and standalone animation resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelAnimationClip {
    /// Display name for dropdowns/runtime manifests.
    pub name: String,
    /// Cooked `.psxanim` path.
    pub psxanim_path: String,
    /// Standalone animation resource id when this row came from the
    /// animation library. `None` means it came from
    /// `ModelResource::clips`.
    pub animation_resource: Option<ResourceId>,
    /// Model-local clip index when this row came from
    /// `ModelResource::clips`.
    pub model_clip_index: Option<usize>,
    /// Per-clip placement calibration.
    pub calibration: AnimationClipCalibration,
}

/// One backing-file move performed by a resource rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFileRename {
    /// Previous stored project path.
    pub from: String,
    /// New stored project path.
    pub to: String,
}

/// One backing file deleted with a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceFileDelete {
    /// Stored project-relative path that was deleted.
    pub path: String,
}

/// Summary returned after renaming a resource and any backing files
/// that are safe for the project to own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceRenameReport {
    /// Files that were physically moved and whose project paths were
    /// updated.
    pub renamed_files: Vec<ResourceFileRename>,
    /// Path fields that were left alone because they were empty,
    /// missing on disk, outside the project root, or otherwise not
    /// safe to move automatically.
    pub skipped_files: Vec<String>,
}

/// Summary returned after removing a resource from the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDeleteReport {
    /// Resource removed from the project's resource table.
    pub removed: Resource,
    /// Number of project references cleared because they pointed at
    /// the removed resource.
    pub cleared_references: usize,
    /// Project-owned backing files physically removed from disk.
    pub deleted_files: Vec<ResourceFileDelete>,
    /// Path fields left alone because they were empty, missing,
    /// outside the project root, or otherwise not safe to delete.
    pub skipped_files: Vec<String>,
}

/// Failure modes for [`ProjectDocument::rename_resource_with_files`].
#[derive(Debug)]
pub enum ResourceRenameError {
    /// No resource with the requested id exists.
    MissingResource(ResourceId),
    /// Empty or whitespace-only names are refused.
    EmptyName,
    /// Two planned file moves would write the same destination.
    DuplicateTarget(PathBuf),
    /// A planned destination already exists.
    TargetExists(PathBuf),
    /// Filesystem operation failed.
    Io {
        /// Source path.
        from: PathBuf,
        /// Destination path.
        to: PathBuf,
        /// Error detail.
        detail: String,
    },
}

impl std::fmt::Display for ResourceRenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingResource(id) => write!(f, "resource #{} does not exist", id.raw()),
            Self::EmptyName => write!(f, "resource name cannot be empty"),
            Self::DuplicateTarget(path) => {
                write!(f, "multiple files would rename to {}", path.display())
            }
            Self::TargetExists(path) => write!(f, "target already exists: {}", path.display()),
            Self::Io { from, to, detail } => write!(
                f,
                "failed to rename {} to {}: {detail}",
                from.display(),
                to.display()
            ),
        }
    }
}

impl std::error::Error for ResourceRenameError {}

/// Failure modes for [`ProjectDocument::delete_resource_with_files`].
#[derive(Debug)]
pub enum ResourceDeleteError {
    /// No resource with the requested id exists.
    MissingResource(ResourceId),
    /// Filesystem operation failed.
    Io {
        /// File path that could not be removed.
        path: PathBuf,
        /// Error detail.
        detail: String,
    },
}

impl std::fmt::Display for ResourceDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingResource(id) => write!(f, "resource #{} does not exist", id.raw()),
            Self::Io { path, detail } => {
                write!(f, "failed to delete {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for ResourceDeleteError {}
