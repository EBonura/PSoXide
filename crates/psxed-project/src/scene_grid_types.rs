use super::*;

/// World layer attached to an authored screen state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneWorldLayer {
    /// No 3D/gameplay layer; the state is UI-only.
    #[default]
    None,
    /// Run the gameplay/level world layer.
    Gameplay,
}

impl SceneWorldLayer {
    /// Human-readable label for editor controls.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Gameplay => "Gameplay",
        }
    }
}

/// One authored screen/game state. This is the scene-arranger object:
/// it references a world layer and an optional UI scene overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSceneState {
    /// Stable state id. Assigned during normalization for legacy data.
    #[serde(default)]
    pub id: SceneStateId,
    /// Display name.
    pub name: String,
    /// Optional 3D/gameplay layer.
    #[serde(default)]
    pub world: SceneWorldLayer,
    /// Optional UI scene drawn over the world.
    #[serde(default)]
    pub ui_scene: Option<UiSceneId>,
    /// Whether UI navigation/buttons capture input while this state is active.
    #[serde(default = "default_true")]
    pub ui_input: bool,
    /// Whether the world layer is paused while the UI overlay is active.
    #[serde(default)]
    pub pause_world: bool,
    /// Optional composed state entered by a fresh controller START press.
    /// `None` leaves START to the active UI scene's legacy confirm shortcut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_state: Option<SceneStateId>,
}

impl ProjectSceneState {
    /// UI-only state backed by one authored UI scene.
    pub fn ui_only(name: impl Into<String>, ui_scene: UiSceneId) -> Self {
        Self {
            id: SceneStateId::UNASSIGNED,
            name: name.into(),
            world: SceneWorldLayer::None,
            ui_scene: Some(ui_scene),
            ui_input: true,
            pause_world: false,
            start_state: None,
        }
    }

    /// Gameplay/world state with an optional UI overlay.
    pub fn gameplay(name: impl Into<String>, ui_scene: Option<UiSceneId>) -> Self {
        Self {
            id: SceneStateId::UNASSIGNED,
            name: name.into(),
            world: SceneWorldLayer::Gameplay,
            ui_scene,
            ui_input: false,
            pause_world: false,
            start_state: None,
        }
    }
}

pub(crate) fn default_scene_states_for_ui_scenes(ui_scenes: &[UiScene]) -> Vec<ProjectSceneState> {
    let mut states: Vec<ProjectSceneState> = ui_scenes
        .iter()
        .map(|scene| ProjectSceneState::ui_only(scene.name.clone(), scene.id))
        .collect();
    states.push(ProjectSceneState::gameplay("Gameplay", None));
    assign_scene_state_ids_for_slice(&mut states);
    states
}

pub(crate) fn assign_scene_state_ids_for_slice(states: &mut [ProjectSceneState]) {
    let mut next = SceneStateId::FIRST.raw();
    for state in states.iter() {
        if state.id != SceneStateId::UNASSIGNED {
            next = next.max(state.id.raw().saturating_add(1));
        }
    }
    let mut seen: HashSet<SceneStateId> = HashSet::new();
    for state in states {
        if state.id == SceneStateId::UNASSIGNED || !seen.insert(state.id) {
            state.id = SceneStateId(next);
            seen.insert(state.id);
            next = next.saturating_add(1);
        }
    }
}

/// Explicit material assignment for one floor/ceiling triangle.
/// Missing override means "inherit the parent face material"; this
/// enum represents the two explicit states a triangle can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridTriangleMaterialOverride {
    /// The triangle is intentionally unassigned even if the parent
    /// face has a material.
    Unassigned,
    /// The triangle uses this material instead of the parent face.
    Resource(ResourceId),
}

impl GridTriangleMaterialOverride {
    pub const fn from_material(material: Option<ResourceId>) -> Self {
        match material {
            Some(id) => Self::Resource(id),
            None => Self::Unassigned,
        }
    }

    pub const fn material(self) -> Option<ResourceId> {
        match self {
            Self::Unassigned => None,
            Self::Resource(id) => Some(id),
        }
    }
}

/// Basic 3D transform used by authored nodes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform3 {
    /// World/local translation in editor units.
    pub translation: [f32; 3],
    /// Euler rotation in degrees, matching common editor UI language.
    pub rotation_degrees: [f32; 3],
    /// Per-axis scale.
    pub scale: [f32; 3],
}

impl Default for Transform3 {
    fn default() -> Self {
        Self {
            translation: [0.0, 0.0, 0.0],
            rotation_degrees: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
        }
    }
}

/// PS1 semi-transparency mode exposed at editor level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PsxBlendMode {
    /// Opaque textured or flat surface.
    #[default]
    Opaque,
    /// `(background + foreground) / 2`.
    Average,
    /// `background + foreground`, clamped per channel.
    Add,
    /// `background - foreground`, clamped per channel.
    Subtract,
    /// `background + foreground / 4`, clamped per channel.
    AddQuarter,
}

impl PsxBlendMode {
    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Opaque => "Opaque",
            Self::Average => "Average",
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::AddQuarter => "Add Quarter",
        }
    }
}

/// Which side of an authored face should render.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialFaceSidedness {
    /// Render the face's authored/front winding only.
    #[default]
    Front,
    /// Render only the opposite side.
    Back,
    /// Render both sides.
    Both,
}

/// Deterministic host-baked noise used by a model material's second pass.
///
/// The editor and cooker turn this into a seamless 128x128 4bpp PSXT. The PS1
/// never evaluates noise at runtime; it only samples the cooked texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProceduralNoiseTexture {
    /// Stable noise seed.
    pub seed: u32,
    /// Approximate size of the broadest features, in texels.
    pub feature_size: u8,
    /// Number of summed detail layers (1..=5).
    pub octaves: u8,
    /// Contrast around the midpoint. 128 is neutral.
    pub contrast: u8,
}

impl Default for ProceduralNoiseTexture {
    fn default() -> Self {
        Self {
            seed: 1,
            feature_size: 24,
            octaves: 3,
            contrast: 176,
        }
    }
}

/// UV-domain transform applied while baking a generated texture layer.
///
/// This remains host-side authoring data: the transformed noise is folded
/// into the final 4bpp PSXT, so it adds no per-frame PlayStation work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedTextureUv {
    /// Horizontal sample scale in Q8 (`256 = 1.0x`).
    pub scale_u_q8: u16,
    /// Vertical sample scale in Q8 (`256 = 1.0x`).
    pub scale_v_q8: u16,
    /// Horizontal sample offset in generated texels.
    pub offset_u: i16,
    /// Vertical sample offset in generated texels.
    pub offset_v: i16,
    /// Clockwise quarter turns (`0..=3`).
    pub rotation_quarters: u8,
}

impl Default for GeneratedTextureUv {
    fn default() -> Self {
        Self {
            scale_u_q8: 256,
            scale_v_q8: 256,
            offset_u: 0,
            offset_v: 0,
            rotation_quarters: 0,
        }
    }
}

/// Host-baked base-colour plus value-noise texture recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedMaterialTexture {
    /// Square output size in texels (8, 16, 32, 64, or 128).
    pub size: u16,
    /// Dark/low end of the generated 16-colour palette.
    pub base_color: [u8; 3],
    /// Whether value noise is folded into the generated base texture.
    /// Defaults on so existing projects retain their appearance.
    #[serde(default = "default_true")]
    pub noise_enabled: bool,
    /// Bright/high end of the generated 16-colour palette.
    pub noise_color: [u8; 3],
    /// Deterministic value-noise controls.
    pub noise: ProceduralNoiseTexture,
    /// Noise-domain scale, offset, and rotation.
    pub noise_uv: GeneratedTextureUv,
}

impl Default for GeneratedMaterialTexture {
    fn default() -> Self {
        Self {
            size: 64,
            base_color: [72, 84, 96],
            noise_enabled: true,
            noise_color: [188, 208, 224],
            noise: ProceduralNoiseTexture::default(),
            noise_uv: GeneratedTextureUv::default(),
        }
    }
}

/// Authoring controls for a room reflection-probe material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionProbeMaterial {
    /// Read packed facet normals from model UV words for a reflection-only map.
    /// Requires a model cooked with the facet-normal UV layout.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub facet_normals: bool,
    /// Project the material's own texture in screen space for a cheap,
    /// camera-reactive crystal/reflection treatment on models.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub enabled: bool,
    /// Reflection strength (`0 = none`, `255 = full probe colour`).
    pub strength: u8,
    /// Surface roughness used by the probe baker (`0 = mirror`).
    pub roughness: u8,
}

impl Default for ReflectionProbeMaterial {
    fn default() -> Self {
        Self {
            facet_normals: false,
            enabled: false,
            strength: 255,
            roughness: 8,
        }
    }
}

/// High-level Material Lab source preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialTextureMode {
    /// Imported 4bpp PSXT, model atlas fallback, or flat tint.
    #[default]
    SimpleImage,
    /// Active room's baked reflection probe with reflection-vector UVs.
    ReflectiveProbe,
    /// Host-baked 4bpp base colour plus procedural noise.
    Generated,
    /// Host-baked mask transition between two Material images.
    Transition,
}

impl MaterialTextureMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SimpleImage => "Simple Image",
            Self::ReflectiveProbe => "Reflective Probe",
            Self::Generated => "Generated Colour + Noise",
            Self::Transition => "Transition Material",
        }
    }
}

/// Coverage-mask family used by a host-baked transition material.
///
/// Every shape is thresholded rather than alpha blended. This keeps the
/// resulting pixel art crisp and leaves the cooker to quantise one final
/// image into a normal PS1 4bpp texture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionMaskShape {
    /// A moving edge across the texture.
    #[default]
    Straight,
    /// A diagonal moving edge.
    Diagonal,
    /// Coverage growing outward from one corner.
    Corner,
    /// Coverage growing outward from the texture centre.
    Island,
    /// Source B fills the tile interior and reaches only the edges connected
    /// to adjacent tiles painted with the same material. Used by Material
    /// Paint to remove seams inside a multi-tile painted region.
    Connected,
}

/// Reserved resource-name prefix for transition variants synthesized by the
/// material Paint tool. They remain normal Material resources for cooking and
/// undo, but author-facing material pickers hide them by default.
pub const AUTO_PAINT_BLEND_PREFIX: &str = "@paint-blend:";

impl TransitionMaskShape {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Straight => "Straight edge",
            Self::Diagonal => "Diagonal edge",
            Self::Corner => "Corner",
            Self::Island => "Island / patch",
            Self::Connected => "Connected paint",
        }
    }
}

/// Authoring-only recipe for combining two Material images.
///
/// The editor and cooker resolve both sources, apply their tints, choose
/// pixels through the selected coverage mask, and jointly quantise the result
/// into one 4bpp PSXT. The PlayStation never evaluates this recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionMaterialTexture {
    /// Material visible outside / beyond the coverage mask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_a: Option<ResourceId>,
    /// Material revealed as coverage increases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_b: Option<ResourceId>,
    /// Square output size in texels (8, 16, 32, 64, or 128).
    pub size: u16,
    /// Amount of source B coverage (`0 = all A`, `255 = all B`).
    pub coverage: u8,
    /// Spatial family used to form the boundary.
    pub shape: TransitionMaskShape,
    /// Clockwise quarter turns (`0..=3`).
    pub rotation_quarters: u8,
    /// Mirror the mask horizontally after rotation.
    pub flip_x: bool,
    /// Mirror the mask vertically after rotation.
    pub flip_y: bool,
    /// Deterministic displacement of the boundary in texels (`0 = clean`).
    pub edge_breakup: u8,
    /// Stable boundary-noise seed.
    pub seed: u32,
    /// Edge bits reached by Source B for [`TransitionMaskShape::Connected`].
    /// North, east, south, and west use bits 0 through 3 respectively.
    #[serde(default, skip_serializing_if = "u8_is_zero")]
    pub connected_edges: u8,
}

impl Default for TransitionMaterialTexture {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TransitionMaterialTexture {
    pub const DEFAULT: Self = Self {
        source_a: None,
        source_b: None,
        size: 64,
        coverage: 128,
        shape: TransitionMaskShape::Straight,
        rotation_quarters: 0,
        flip_x: false,
        flip_y: false,
        edge_breakup: 20,
        seed: 1,
        connected_edges: 0,
    };
}

const fn u8_is_zero(value: &u8) -> bool {
    *value == 0
}

/// Image source for a model material's optional second texture pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelSecondaryTexture {
    /// A separately authored 4bpp PSXT.
    Texture(String),
    /// A deterministic seamless 128x128 4bpp value-noise texture generated on
    /// the host.
    ProceduralNoise(ProceduralNoiseTexture),
}

/// Deterministic UV motion for a material texture pass.
///
/// Speeds are signed Q8 texels per second. Keeping the recipe integral makes
/// editor preview and PS1 playback agree without regenerating or uploading a
/// texture every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialUvMotion {
    /// Whether the authored motion is active. Disabled motion preserves its
    /// speed and phase so it can be toggled while iterating in Material Lab.
    pub enabled: bool,
    /// Horizontal speed in signed Q8 texels per second.
    pub speed_u_q8: i16,
    /// Vertical speed in signed Q8 texels per second.
    pub speed_v_q8: i16,
    /// Initial horizontal texel offset, wrapping at 256.
    pub phase_u: u8,
    /// Initial vertical texel offset, wrapping at 256.
    pub phase_v: u8,
}

impl Default for MaterialUvMotion {
    fn default() -> Self {
        Self {
            enabled: false,
            speed_u_q8: 8 * 256,
            speed_v_q8: 0,
            phase_u: 0,
            phase_v: 0,
        }
    }
}

impl MaterialUvMotion {
    /// Resolve this recipe to a wrapped PS1 UV offset at `tick`.
    pub fn offset_at_tick(self, tick: u32, ticks_per_second: u16) -> [u8; 2] {
        if !self.enabled || ticks_per_second == 0 {
            return [self.phase_u, self.phase_v];
        }
        let resolve = |speed_q8: i16, phase: u8| {
            let travelled_q8 =
                i64::from(speed_q8).saturating_mul(i64::from(tick)) / i64::from(ticks_per_second);
            // Truncate sub-texel motion toward zero so positive and negative
            // speeds wait the same amount of time before the first UV step.
            let texels = travelled_q8 / 256 + i64::from(phase);
            texels.rem_euclid(256) as u8
        };
        [
            resolve(self.speed_u_q8, self.phase_u),
            resolve(self.speed_v_q8, self.phase_v),
        ]
    }
}

/// Runtime animation applied to a room material's single texture pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialAnimationMode {
    /// No per-frame material work.
    #[default]
    Static,
    /// Move the material's UVs through its resident texture.
    UvScroll,
    /// Select cells from a grid packed into the resident texture.
    Flipbook,
    /// Modulate the material's baked light between two authored levels.
    LightPulse,
}

impl MaterialAnimationMode {
    /// User-facing Material Lab label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Static => "Static",
            Self::UvScroll => "UV Scroll",
            Self::Flipbook => "Cycle Materials",
            Self::LightPulse => "Light Pulse",
        }
    }
}

/// Smooth baked-light modulation for a room material.
///
/// Values use the PS1 texture-modulation convention: 128 is neutral, values
/// below darken, and values above brighten. The runtime evaluates a sine wave
/// directly from the gameplay tick, so no per-material mutable state is
/// required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialLightPulse {
    /// Darkest Q7 light multiplier (`128 = 1.0x`).
    pub minimum_q7: u8,
    /// Brightest Q7 light multiplier (`128 = 1.0x`).
    pub maximum_q7: u8,
    /// Complete dark-to-bright-to-dark cycle in gameplay ticks.
    pub ticks_per_cycle: u8,
    /// Initial position within the cycle, in ticks.
    pub phase: u8,
}

impl Default for MaterialLightPulse {
    fn default() -> Self {
        Self {
            minimum_q7: 80,
            maximum_q7: 208,
            ticks_per_cycle: 72,
            phase: 0,
        }
    }
}

impl MaterialLightPulse {
    /// Clamp the recipe to a valid non-empty cycle and ordered light range.
    pub const fn normalized(self) -> Self {
        let ticks_per_cycle = if self.ticks_per_cycle == 0 {
            1
        } else {
            self.ticks_per_cycle
        };
        let minimum_q7 = if self.minimum_q7 > self.maximum_q7 {
            self.maximum_q7
        } else {
            self.minimum_q7
        };
        Self {
            minimum_q7,
            maximum_q7: self.maximum_q7,
            ticks_per_cycle,
            phase: self.phase % ticks_per_cycle,
        }
    }
}

/// Two-material authoring cycle plus its compact runtime timing/layout.
///
/// Authors select `source_a` and `source_b`. The host cooker resolves those
/// materials and creates the grid-packed 4bpp runtime texture; the PS1 sees
/// only the numeric layout and timing fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialFlipbook {
    /// Number of frame columns in the texture.
    pub columns: u8,
    /// Number of frame rows in the texture.
    pub rows: u8,
    /// Number of active cells, in row-major order.
    pub frame_count: u8,
    /// Simulation ticks each frame remains selected.
    pub ticks_per_frame: u8,
    /// Initial frame index.
    pub phase: u8,
    /// First authoring material. Its resolved image becomes runtime frame A.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_a: Option<ResourceId>,
    /// Second authoring material. Its resolved image becomes runtime frame B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_b: Option<ResourceId>,
}

impl Default for MaterialFlipbook {
    fn default() -> Self {
        Self {
            columns: 2,
            rows: 2,
            frame_count: 4,
            ticks_per_frame: 6,
            phase: 0,
            source_a: None,
            source_b: None,
        }
    }
}

impl MaterialFlipbook {
    /// Clamp authored values to a non-empty atlas grid.
    pub const fn normalized(self) -> Self {
        let two_material_sources = self.source_a.is_some() || self.source_b.is_some();
        let columns = if two_material_sources {
            2
        } else if self.columns == 0 {
            1
        } else {
            self.columns
        };
        let rows = if two_material_sources || self.rows == 0 {
            1
        } else {
            self.rows
        };
        let capacity = columns.saturating_mul(rows);
        let frame_count = if two_material_sources {
            2
        } else if self.frame_count == 0 {
            1
        } else if self.frame_count > capacity {
            capacity
        } else {
            self.frame_count
        };
        Self {
            columns,
            rows,
            frame_count,
            ticks_per_frame: if self.ticks_per_frame == 0 {
                1
            } else {
                self.ticks_per_frame
            },
            phase: self.phase % frame_count,
            source_a: self.source_a,
            source_b: self.source_b,
        }
    }
}

/// Preserved room-material animation recipes. Only the selected mode is
/// evaluated at runtime, so switching modes in Material Lab does not discard
/// the other mode's controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MaterialAnimation {
    /// Active animation algorithm.
    pub mode: MaterialAnimationMode,
    /// Preserved UV-scroll controls.
    pub uv_scroll: MaterialUvMotion,
    /// Preserved grid-flipbook controls.
    pub flipbook: MaterialFlipbook,
    /// Preserved baked-light pulse controls.
    #[serde(default)]
    pub light_pulse: MaterialLightPulse,
}

impl Default for ModelSecondaryTexture {
    fn default() -> Self {
        Self::ProceduralNoise(ProceduralNoiseTexture::default())
    }
}

/// Optional second model-material pass. It deliberately mirrors the first
/// layer's image/generated/probe controls so Material Lab exposes two
/// predictable PS1 passes instead of a special-purpose "noise overlay".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSecondaryLayer {
    /// Whether this authored pass is currently rendered. Disabling a layer
    /// deliberately preserves every control below so it can be re-enabled
    /// without rebuilding the material recipe.
    #[serde(
        default = "model_secondary_layer_enabled_default",
        skip_serializing_if = "bool_is_true"
    )]
    pub enabled: bool,
    /// Active texture source for this pass.
    #[serde(default)]
    pub texture_mode: MaterialTextureMode,
    /// Optional separately-authored 4bpp texture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psxt_path: Option<String>,
    /// Preserved host-baked colour/noise recipe.
    #[serde(default)]
    pub generated: GeneratedMaterialTexture,
    /// Preserved transition recipe. Transition sources are currently intended
    /// for the primary material pass, but retaining this keeps source-mode
    /// switching lossless and forward compatible.
    #[serde(default)]
    pub transition: TransitionMaterialTexture,
    /// Preserved room-probe controls.
    #[serde(default)]
    pub reflection: ReflectionProbeMaterial,
    /// Optical equation used by the second pass.
    pub blend_mode: PsxBlendMode,
    /// Independent modulation tint; 0x80 is neutral.
    pub tint: [u8; 3],
    /// Optional runtime UV scroll. Defaults off for old project files.
    #[serde(default)]
    pub motion: MaterialUvMotion,
    /// Pre-two-layer source, migrated by [`Self::normalize_legacy_source`].
    #[serde(rename = "texture", default, skip_serializing)]
    pub legacy_texture: Option<ModelSecondaryTexture>,
}

impl Default for ModelSecondaryLayer {
    fn default() -> Self {
        Self {
            enabled: true,
            texture_mode: MaterialTextureMode::Generated,
            psxt_path: None,
            generated: GeneratedMaterialTexture {
                size: MODEL_SECONDARY_GENERATED_SIZE,
                base_color: [0, 0, 0],
                noise_enabled: true,
                noise_color: [255, 255, 255],
                noise: ProceduralNoiseTexture::default(),
                noise_uv: GeneratedTextureUv::default(),
            },
            transition: TransitionMaterialTexture::DEFAULT,
            reflection: ReflectionProbeMaterial::default(),
            blend_mode: PsxBlendMode::AddQuarter,
            tint: [0x70, 0x78, 0x80],
            motion: MaterialUvMotion::default(),
            legacy_texture: None,
        }
    }
}

impl ModelSecondaryLayer {
    /// Convert the old texture-or-noise overlay into the symmetric source
    /// controls. This runs once after loading and is then omitted on save.
    pub fn normalize_legacy_source(&mut self) {
        let Some(source) = self.legacy_texture.take() else {
            return;
        };
        match source {
            ModelSecondaryTexture::Texture(path) => {
                self.texture_mode = MaterialTextureMode::SimpleImage;
                self.psxt_path = (!path.trim().is_empty()).then_some(path);
            }
            ModelSecondaryTexture::ProceduralNoise(noise) => {
                self.texture_mode = MaterialTextureMode::Generated;
                self.generated = GeneratedMaterialTexture {
                    size: MODEL_SECONDARY_GENERATED_SIZE,
                    base_color: [0, 0, 0],
                    noise_enabled: true,
                    noise_color: [255, 255, 255],
                    noise,
                    noise_uv: GeneratedTextureUv::default(),
                };
            }
        }
    }

    /// New overlay authored from the editor: independently scrolling by
    /// default, while legacy saved overlays without a motion field stay still.
    pub fn moving_default() -> Self {
        Self {
            motion: MaterialUvMotion {
                enabled: true,
                speed_u_q8: 2 * 256,
                speed_v_q8: 256,
                phase_u: 0,
                phase_v: 0,
            },
            ..Self::default()
        }
    }
}

const MODEL_SECONDARY_GENERATED_SIZE: u16 = 128;

impl MaterialFaceSidedness {
    /// User-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Front => "Front",
            Self::Back => "Back",
            Self::Both => "Both",
        }
    }

    /// The side a BSP brush face actually draws in game. The PXBSP runtime
    /// draws brush faces front only even for `Both`: a brush is closed, so
    /// the back of a two-sided face is another face of the same brush with
    /// its own bake, and drawing both tied a dark and a lit face in the
    /// ordering table (`psx-bsp` `pxbsp_policy_face_draws`). `Both` still
    /// applies to model material overrides.
    pub const fn brush_face_drawn(self) -> Self {
        match self {
            Self::Back => Self::Back,
            Self::Front | Self::Both => Self::Front,
        }
    }

    /// Convert the old checkbox value into the new enum.
    pub const fn from_double_sided(double_sided: bool) -> Self {
        if double_sided {
            Self::Both
        } else {
            Self::Front
        }
    }
}

/// Authoring material. The cooker maps this to runtime texture/material state.
///
/// A material owns its image: `psxt_path` points at the cooked `.psxt`
/// this material draws with (`None` renders flat tint). Materials and
/// textures used to be separate resources with the material holding a
/// `texture: ResourceId` reference; that split was folded here because
/// the relationship was 1:1 in practice. Legacy projects migrate at
/// load via [`crate::ProjectDocument::migrate_legacy_texture_resources`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialResource {
    /// Material Lab's active texture-source preset. Old project files omit
    /// this field and therefore remain Simple Image materials.
    #[serde(default)]
    pub texture_mode: MaterialTextureMode,
    /// Cooked `.psxt` image this material draws with, or `None` for a
    /// flat tinted material. Resolved first as-is (absolute paths),
    /// then relative to the project file's directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psxt_path: Option<String>,
    /// PS1 blend mode.
    pub blend_mode: PsxBlendMode,
    /// Texture modulation tint. `0x80` is neutral for PS1 textured polys.
    pub tint: [u8; 3],
    /// Preserved generator recipe, even while another source mode is active.
    #[serde(default)]
    pub generated: GeneratedMaterialTexture,
    /// Preserved two-source transition recipe, even while another source mode
    /// is active.
    #[serde(default)]
    pub transition: TransitionMaterialTexture,
    /// Preserved reflection controls, even while another source mode is active.
    #[serde(default)]
    pub reflection: ReflectionProbeMaterial,
    /// One-pass runtime animation for room tiles using this material.
    #[serde(default)]
    pub animation: MaterialAnimation,
    /// Optional independently blended second texture pass for model renderers.
    /// Room geometry continues to use the base material only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_layer: Option<ModelSecondaryLayer>,
    /// Which side(s) of faces using this material should render.
    #[serde(default)]
    pub face_sidedness: MaterialFaceSidedness,
    /// Brush faces using this material reveal the World node's scene-level
    /// sky instead of drawing their authored polygons.
    #[serde(default)]
    pub sky_aperture: bool,
    /// Legacy material-level Quake sky choice. Folded into `sky_aperture` and
    /// the World sky settings at load time, then never written back.
    #[serde(default, skip_serializing)]
    pub layered_sky: bool,
    /// Legacy material-level cube sky choice. Folded into `sky_aperture` and
    /// the World sky settings at load time, then never written back.
    #[serde(default, skip_serializing)]
    pub directional_sky: bool,
    /// Stable identity of the recipe currently exposed to preview and cooking.
    /// Legacy project files become version 1 (`Original`).
    #[serde(
        default = "default_material_version_id",
        skip_serializing_if = "material_version_id_is_original"
    )]
    pub active_version_id: MaterialVersionId,
    /// Author-facing label for the active recipe.
    #[serde(
        default = "default_material_version_name",
        skip_serializing_if = "material_version_name_is_original"
    )]
    pub active_version_name: String,
    /// Complete non-active recipes for this same logical material. Faces,
    /// props, models, and transition sources continue to reference the owning
    /// [`ResourceId`], so activating one of these changes every use at once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<MaterialVersion>,
    /// Legacy pre-merge texture resource reference. Parsed from old
    /// projects, folded into [`psxt_path`](Self::psxt_path) by the
    /// load-time migration, never written back.
    #[serde(rename = "texture", default, skip_serializing)]
    pub legacy_texture: Option<ResourceId>,
    /// Legacy project field. New code reads/writes
    /// [`face_sidedness`](Self::face_sidedness); this remains so older
    /// `.ron` projects migrate without losing their two-sided setting.
    #[serde(default)]
    pub double_sided: bool,
}

/// Stable identity for one named recipe inside a [`MaterialResource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MaterialVersionId(u64);

impl MaterialVersionId {
    /// Identity assigned to every legacy material's initial recipe.
    pub const ORIGINAL: Self = Self(1);

    /// Return the stored integer for UI ordering and diagnostics.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl Default for MaterialVersionId {
    fn default() -> Self {
        Self::ORIGINAL
    }
}

/// One named, inactive version of a material recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialVersion {
    /// Stable version identity, unique within the owning material.
    pub id: MaterialVersionId,
    /// Author-facing version label.
    pub name: String,
    /// Complete material recipe restored when this version is activated.
    pub recipe: MaterialVersionRecipe,
}

/// Complete versionable portion of a [`MaterialResource`].
///
/// Legacy migration fields deliberately stay on the owning material. They are
/// input compatibility state, not authoring choices that should vary by
/// version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialVersionRecipe {
    pub texture_mode: MaterialTextureMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psxt_path: Option<String>,
    pub blend_mode: PsxBlendMode,
    pub tint: [u8; 3],
    #[serde(default)]
    pub generated: GeneratedMaterialTexture,
    #[serde(default)]
    pub transition: TransitionMaterialTexture,
    #[serde(default)]
    pub reflection: ReflectionProbeMaterial,
    #[serde(default)]
    pub animation: MaterialAnimation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_layer: Option<ModelSecondaryLayer>,
    #[serde(default)]
    pub face_sidedness: MaterialFaceSidedness,
    /// Whether brush faces using this recipe reveal the World sky.
    #[serde(default)]
    pub sky_aperture: bool,
    /// Parse-only compatibility with old version recipes.
    #[serde(default, skip_serializing)]
    pub layered_sky: bool,
    /// Parse-only compatibility with old version recipes.
    #[serde(default, skip_serializing)]
    pub directional_sky: bool,
}

fn default_material_version_id() -> MaterialVersionId {
    MaterialVersionId::ORIGINAL
}

fn material_version_id_is_original(id: &MaterialVersionId) -> bool {
    *id == MaterialVersionId::ORIGINAL
}

fn default_material_version_name() -> String {
    "Original".to_string()
}

fn material_version_name_is_original(name: &String) -> bool {
    name == "Original"
}

/// Default authored width/height for image props, in engine/editor units.
pub const DEFAULT_IMAGE_PROP_SIZE: u16 = DEFAULT_WORLD_SECTOR_SIZE as u16;

pub(crate) const fn default_image_prop_size() -> u16 {
    DEFAULT_IMAGE_PROP_SIZE
}

/// Default authored collision-box full-size (width / height / depth)
/// for an image prop. Sized to match `DEFAULT_IMAGE_PROP_SIZE` so a
/// fresh prop with collision toggled on has a sensible cube around
/// its visible plane.
pub(crate) const fn default_image_prop_collision_size() -> [u16; 3] {
    [
        DEFAULT_IMAGE_PROP_SIZE,
        DEFAULT_IMAGE_PROP_SIZE,
        DEFAULT_IMAGE_PROP_SIZE,
    ]
}

/// Face slots on an authored boxed prop.
pub const BOX_PROP_FACE_COUNT: usize = 6;
/// Editable vertex count on an authored boxed prop.
pub const BOX_PROP_VERTEX_COUNT: usize = 8;
/// Default authored cube size for boxed props, in engine/editor units.
pub const DEFAULT_BOX_PROP_SIZE: u16 = DEFAULT_WORLD_SECTOR_SIZE as u16;

/// User-facing face order for boxed prop material slots.
pub const BOX_PROP_FACE_NAMES: [&str; BOX_PROP_FACE_COUNT] =
    ["Front", "Right", "Back", "Left", "Top", "Bottom"];

/// Vertex perimeter for each Box Prop face in [`BOX_PROP_FACE_NAMES`] order.
///
/// The order on every face is UV-compatible: `[top-left, top-right,
/// bottom-right, bottom-left]` for vertical faces, with equivalent perimeter
/// ordering on the top and bottom.
pub const BOX_PROP_FACE_VERTEX_INDICES: [[usize; 4]; BOX_PROP_FACE_COUNT] = [
    [4, 5, 1, 0],
    [5, 6, 2, 1],
    [6, 7, 3, 2],
    [7, 4, 0, 3],
    [7, 6, 5, 4],
    [0, 1, 2, 3],
];

pub(crate) const fn default_box_prop_materials() -> [Option<ResourceId>; BOX_PROP_FACE_COUNT] {
    [None; BOX_PROP_FACE_COUNT]
}

pub(crate) const fn default_box_prop_uvs() -> [UvTransform; BOX_PROP_FACE_COUNT] {
    [UvTransform::IDENTITY; BOX_PROP_FACE_COUNT]
}

pub(crate) const fn default_box_prop_vertices() -> [[i16; 3]; BOX_PROP_VERTEX_COUNT] {
    box_prop_vertices_for_size(DEFAULT_BOX_PROP_SIZE)
}

/// Build the default bottom-anchored cube vertices for a boxed prop.
pub const fn box_prop_vertices_for_size(size: u16) -> [[i16; 3]; BOX_PROP_VERTEX_COUNT] {
    let half = (size / 2) as i16;
    let height = size as i16;
    [
        [-half, 0, -half],
        [half, 0, -half],
        [half, 0, half],
        [-half, 0, half],
        [-half, height, -half],
        [half, height, -half],
        [half, height, half],
        [-half, height, half],
    ]
}

impl MaterialResource {
    /// Build an opaque neutral material.
    pub fn opaque(psxt_path: Option<String>) -> Self {
        Self {
            texture_mode: MaterialTextureMode::SimpleImage,
            psxt_path,
            blend_mode: PsxBlendMode::Opaque,
            tint: [0x80, 0x80, 0x80],
            generated: GeneratedMaterialTexture {
                size: 64,
                base_color: [72, 84, 96],
                noise_enabled: true,
                noise_color: [188, 208, 224],
                noise: ProceduralNoiseTexture {
                    seed: 1,
                    feature_size: 24,
                    octaves: 3,
                    contrast: 176,
                },
                noise_uv: GeneratedTextureUv {
                    scale_u_q8: 256,
                    scale_v_q8: 256,
                    offset_u: 0,
                    offset_v: 0,
                    rotation_quarters: 0,
                },
            },
            transition: TransitionMaterialTexture::DEFAULT,
            reflection: ReflectionProbeMaterial {
                facet_normals: false,
                enabled: false,
                strength: 255,
                roughness: 8,
            },
            animation: MaterialAnimation {
                mode: MaterialAnimationMode::Static,
                uv_scroll: MaterialUvMotion {
                    enabled: false,
                    speed_u_q8: 8 * 256,
                    speed_v_q8: 0,
                    phase_u: 0,
                    phase_v: 0,
                },
                flipbook: MaterialFlipbook {
                    columns: 2,
                    rows: 2,
                    frame_count: 4,
                    ticks_per_frame: 6,
                    phase: 0,
                    source_a: None,
                    source_b: None,
                },
                light_pulse: MaterialLightPulse::default(),
            },
            secondary_layer: None,
            face_sidedness: MaterialFaceSidedness::Front,
            sky_aperture: false,
            layered_sky: false,
            directional_sky: false,
            active_version_id: MaterialVersionId::ORIGINAL,
            active_version_name: default_material_version_name(),
            versions: Vec::new(),
            legacy_texture: None,
            double_sided: false,
        }
    }

    /// Build a translucent neutral material.
    pub fn translucent(psxt_path: Option<String>, blend_mode: PsxBlendMode) -> Self {
        Self {
            texture_mode: MaterialTextureMode::SimpleImage,
            psxt_path,
            blend_mode,
            tint: [0x80, 0x80, 0x80],
            generated: GeneratedMaterialTexture {
                size: 64,
                base_color: [72, 84, 96],
                noise_enabled: true,
                noise_color: [188, 208, 224],
                noise: ProceduralNoiseTexture {
                    seed: 1,
                    feature_size: 24,
                    octaves: 3,
                    contrast: 176,
                },
                noise_uv: GeneratedTextureUv {
                    scale_u_q8: 256,
                    scale_v_q8: 256,
                    offset_u: 0,
                    offset_v: 0,
                    rotation_quarters: 0,
                },
            },
            transition: TransitionMaterialTexture::DEFAULT,
            reflection: ReflectionProbeMaterial {
                facet_normals: false,
                enabled: false,
                strength: 255,
                roughness: 8,
            },
            animation: MaterialAnimation {
                mode: MaterialAnimationMode::Static,
                uv_scroll: MaterialUvMotion {
                    enabled: false,
                    speed_u_q8: 8 * 256,
                    speed_v_q8: 0,
                    phase_u: 0,
                    phase_v: 0,
                },
                flipbook: MaterialFlipbook {
                    columns: 2,
                    rows: 2,
                    frame_count: 4,
                    ticks_per_frame: 6,
                    phase: 0,
                    source_a: None,
                    source_b: None,
                },
                light_pulse: MaterialLightPulse::default(),
            },
            secondary_layer: None,
            face_sidedness: MaterialFaceSidedness::Front,
            sky_aperture: false,
            layered_sky: false,
            directional_sky: false,
            active_version_id: MaterialVersionId::ORIGINAL,
            active_version_name: default_material_version_name(),
            versions: Vec::new(),
            legacy_texture: None,
            double_sided: false,
        }
    }

    /// Resolved sidedness. Missing `face_sidedness` defaults to `Front`, while
    /// legacy `double_sided = true` still upgrades that value to two-sided.
    pub const fn sidedness(&self) -> MaterialFaceSidedness {
        if self.double_sided && matches!(self.face_sidedness, MaterialFaceSidedness::Front) {
            MaterialFaceSidedness::Both
        } else {
            self.face_sidedness
        }
    }

    /// Keep the legacy field aligned after editing `face_sidedness`.
    pub fn sync_legacy_sidedness(&mut self) {
        self.double_sided = matches!(self.face_sidedness, MaterialFaceSidedness::Both);
    }

    /// Return the second pass only when it should participate in preview,
    /// cooking and rendering. The stored `Option` is retained while disabled
    /// so the editor never destroys the author's layer settings.
    pub fn enabled_secondary_layer(&self) -> Option<&ModelSecondaryLayer> {
        self.secondary_layer.as_ref().filter(|layer| layer.enabled)
    }

    /// Enable or disable the second pass without discarding its recipe.
    /// Enabling a never-authored layer creates the one sensible initial preset.
    pub fn set_secondary_layer_enabled(&mut self, enabled: bool) {
        match (enabled, self.secondary_layer.as_mut()) {
            (true, Some(layer)) => layer.enabled = true,
            (true, None) => self.secondary_layer = Some(ModelSecondaryLayer::moving_default()),
            (false, Some(layer)) => layer.enabled = false,
            (false, None) => {}
        }
    }

    /// Named versions in stable id order, including the active recipe.
    pub fn version_options(&self) -> Vec<(MaterialVersionId, String)> {
        let mut options = Vec::with_capacity(self.versions.len() + 1);
        options.push((self.active_version_id, self.active_version_name.clone()));
        options.extend(
            self.versions
                .iter()
                .map(|version| (version.id, version.name.clone())),
        );
        options.sort_by_key(|(id, _)| id.raw());
        options
    }

    /// Total number of recipes stored under this logical material.
    pub fn version_count(&self) -> usize {
        self.versions.len() + 1
    }

    /// Duplicate the active recipe into a newly named active version.
    /// The previous active version is retained as an inactive snapshot.
    pub fn create_version(&mut self, name: impl Into<String>) -> MaterialVersionId {
        let current = self.capture_active_version();
        self.versions.push(current);
        let id = self.next_version_id();
        self.active_version_id = id;
        self.active_version_name = normalized_material_version_name(name.into(), id);
        id
    }

    /// Activate a saved recipe without changing the owning resource id.
    pub fn activate_version(&mut self, id: MaterialVersionId) -> bool {
        if id == self.active_version_id {
            return false;
        }
        let Some(index) = self.versions.iter().position(|version| version.id == id) else {
            return false;
        };
        let current = self.capture_active_version();
        let target = std::mem::replace(&mut self.versions[index], current);
        self.apply_version(target);
        true
    }

    /// Rename one version. Empty and duplicate labels are rejected.
    pub fn rename_version(&mut self, id: MaterialVersionId, name: impl Into<String>) -> bool {
        let name = name.into();
        let name = name.trim();
        if name.is_empty()
            || self
                .version_options()
                .iter()
                .any(|(candidate_id, candidate)| {
                    *candidate_id != id && candidate.eq_ignore_ascii_case(name)
                })
        {
            return false;
        }
        if id == self.active_version_id {
            self.active_version_name = name.to_string();
            return true;
        }
        let Some(version) = self.versions.iter_mut().find(|version| version.id == id) else {
            return false;
        };
        version.name = name.to_string();
        true
    }

    /// Remove one version. The final remaining recipe cannot be deleted.
    /// Deleting the active version first activates the oldest saved version.
    pub fn delete_version(&mut self, id: MaterialVersionId) -> bool {
        if self.version_count() <= 1 {
            return false;
        }
        if id == self.active_version_id {
            let Some(replacement) = self
                .versions
                .iter()
                .min_by_key(|version| version.id.raw())
                .map(|version| version.id)
            else {
                return false;
            };
            if !self.activate_version(replacement) {
                return false;
            }
        }
        let Some(index) = self.versions.iter().position(|version| version.id == id) else {
            return false;
        };
        self.versions.remove(index);
        true
    }

    /// Repair hand-authored or legacy version metadata after deserialization.
    pub fn normalize_versions(&mut self) {
        self.sky_aperture |= self.layered_sky || self.directional_sky;
        if self.active_version_id.raw() == 0 {
            self.active_version_id = MaterialVersionId::ORIGINAL;
        }
        self.active_version_name = normalized_material_version_name(
            std::mem::take(&mut self.active_version_name),
            self.active_version_id,
        );
        if let Some(layer) = self.secondary_layer.as_mut() {
            layer.normalize_legacy_source();
        }

        let mut seen = HashSet::new();
        seen.insert(self.active_version_id);
        let mut next = self
            .versions
            .iter()
            .map(|version| version.id.raw())
            .chain(std::iter::once(self.active_version_id.raw()))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(2);
        for version in &mut self.versions {
            version.recipe.sky_aperture |=
                version.recipe.layered_sky || version.recipe.directional_sky;
            if version.id.raw() == 0 || !seen.insert(version.id) {
                while seen.contains(&MaterialVersionId(next)) {
                    next = next.saturating_add(1);
                }
                version.id = MaterialVersionId(next);
                seen.insert(version.id);
                next = next.saturating_add(1);
            }
            version.name =
                normalized_material_version_name(std::mem::take(&mut version.name), version.id);
            if let Some(layer) = version.recipe.secondary_layer.as_mut() {
                layer.normalize_legacy_source();
            }
        }
    }

    pub(crate) fn version_resource_reference_count(&self, id: ResourceId) -> usize {
        MaterialVersionRecipe::from(self).resource_reference_count(id)
            + self
                .versions
                .iter()
                .map(|version| version.recipe.resource_reference_count(id))
                .sum::<usize>()
    }

    pub(crate) fn clear_version_resource_references(&mut self, id: ResourceId) -> usize {
        let mut active = MaterialVersionRecipe::from(&*self);
        let mut cleared = active.clear_resource_references(id);
        active.apply_to(self);
        cleared += self
            .versions
            .iter_mut()
            .map(|version| version.recipe.clear_resource_references(id))
            .sum::<usize>();
        cleared
    }

    /// Every imported texture path preserved by every version and layer.
    pub(crate) fn version_texture_paths(&self) -> Vec<&str> {
        let mut paths = Vec::with_capacity((self.versions.len() + 1) * 2);
        if let Some(path) = self.psxt_path.as_deref() {
            paths.push(path);
        }
        if let Some(path) = self
            .secondary_layer
            .as_ref()
            .and_then(|layer| layer.psxt_path.as_deref())
        {
            paths.push(path);
        }
        for version in &self.versions {
            paths.extend(version.recipe.texture_paths());
        }
        paths
    }

    fn capture_active_version(&self) -> MaterialVersion {
        MaterialVersion {
            id: self.active_version_id,
            name: self.active_version_name.clone(),
            recipe: MaterialVersionRecipe::from(self),
        }
    }

    fn apply_version(&mut self, version: MaterialVersion) {
        self.active_version_id = version.id;
        self.active_version_name = version.name;
        version.recipe.apply_to(self);
    }

    fn next_version_id(&self) -> MaterialVersionId {
        let next = self
            .versions
            .iter()
            .map(|version| version.id.raw())
            .chain(std::iter::once(self.active_version_id.raw()))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(2);
        MaterialVersionId(next)
    }
}

impl From<&MaterialResource> for MaterialVersionRecipe {
    fn from(material: &MaterialResource) -> Self {
        Self {
            texture_mode: material.texture_mode,
            psxt_path: material.psxt_path.clone(),
            blend_mode: material.blend_mode,
            tint: material.tint,
            generated: material.generated,
            transition: material.transition,
            reflection: material.reflection,
            animation: material.animation,
            secondary_layer: material.secondary_layer.clone(),
            face_sidedness: material.sidedness(),
            sky_aperture: material.sky_aperture,
            layered_sky: material.layered_sky,
            directional_sky: material.directional_sky,
        }
    }
}

impl MaterialVersionRecipe {
    fn apply_to(self, material: &mut MaterialResource) {
        material.texture_mode = self.texture_mode;
        material.psxt_path = self.psxt_path;
        material.blend_mode = self.blend_mode;
        material.tint = self.tint;
        material.generated = self.generated;
        material.transition = self.transition;
        material.reflection = self.reflection;
        material.animation = self.animation;
        material.secondary_layer = self.secondary_layer;
        material.face_sidedness = self.face_sidedness;
        material.sky_aperture = self.sky_aperture || self.layered_sky || self.directional_sky;
        material.layered_sky = self.layered_sky;
        material.directional_sky = self.directional_sky;
        material.legacy_texture = None;
        material.sync_legacy_sidedness();
    }

    fn resource_reference_count(&self, id: ResourceId) -> usize {
        usize::from(self.transition.source_a == Some(id))
            + usize::from(self.transition.source_b == Some(id))
            + usize::from(self.animation.flipbook.source_a == Some(id))
            + usize::from(self.animation.flipbook.source_b == Some(id))
            + self.secondary_layer.as_ref().map_or(0, |layer| {
                usize::from(layer.transition.source_a == Some(id))
                    + usize::from(layer.transition.source_b == Some(id))
            })
    }

    fn clear_resource_references(&mut self, id: ResourceId) -> usize {
        let clear = |value: &mut Option<ResourceId>| {
            if *value == Some(id) {
                *value = None;
                1
            } else {
                0
            }
        };
        let mut cleared = clear(&mut self.transition.source_a)
            + clear(&mut self.transition.source_b)
            + clear(&mut self.animation.flipbook.source_a)
            + clear(&mut self.animation.flipbook.source_b);
        if let Some(layer) = self.secondary_layer.as_mut() {
            cleared +=
                clear(&mut layer.transition.source_a) + clear(&mut layer.transition.source_b);
        }
        cleared
    }

    fn texture_paths(&self) -> Vec<&str> {
        let mut paths = Vec::with_capacity(2);
        if let Some(path) = self.psxt_path.as_deref() {
            paths.push(path);
        }
        if let Some(path) = self
            .secondary_layer
            .as_ref()
            .and_then(|layer| layer.psxt_path.as_deref())
        {
            paths.push(path);
        }
        paths
    }
}

fn normalized_material_version_name(name: String, id: MaterialVersionId) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        if id == MaterialVersionId::ORIGINAL {
            default_material_version_name()
        } else {
            format!("Version {}", id.raw())
        }
    } else {
        trimmed.to_string()
    }
}

const fn model_secondary_layer_enabled_default() -> bool {
    true
}

fn bool_is_true(value: &bool) -> bool {
    *value
}

fn bool_is_false(value: &bool) -> bool {
    !*value
}

/// Texture rotation preset for authored faces.
///
/// PS1 textured polygons carry per-corner 8-bit UVs, not a texture
/// matrix, so these rotations are represented by rewriting the UVs
/// sent with each face.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UvRotation {
    /// No texture rotation.
    #[default]
    Deg0,
    /// Rotate texture coordinates 45 degrees clockwise on the face.
    Deg45,
    /// Rotate texture coordinates 90 degrees clockwise on the face.
    Deg90,
    /// Rotate texture coordinates 135 degrees clockwise on the face.
    Deg135,
    /// Rotate texture coordinates 180 degrees.
    Deg180,
    /// Rotate texture coordinates 225 degrees clockwise on the face.
    Deg225,
    /// Rotate texture coordinates 270 degrees clockwise on the face.
    Deg270,
    /// Rotate texture coordinates 315 degrees clockwise on the face.
    Deg315,
}

/// Non-destructive texture-coordinate transform for one grid face.
///
/// `offset` is in PS1 texels and is applied after flip/rotation. It
/// wraps in the 8-bit UV coordinate space, which matches packet-level
/// PS1 UVs; runtime room materials use texture-window state so this
/// can repeat a compact material tile without rebaking the texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UvTransform {
    /// Signed `[u, v]` texel offset.
    #[serde(default)]
    pub offset: [i16; 2],
    /// Optional `[u, v]` UV span in texels. Zero means "use the
    /// source quad's native span" for that axis.
    #[serde(default, skip_serializing_if = "is_default_uv_span")]
    pub span: [u16; 2],
    /// Texture rotation preset.
    #[serde(default)]
    pub rotation: UvRotation,
    /// Mirror horizontally before rotation.
    #[serde(default)]
    pub flip_u: bool,
    /// Mirror vertically before rotation.
    #[serde(default)]
    pub flip_v: bool,
}

impl UvTransform {
    /// Identity transform.
    pub const IDENTITY: Self = Self {
        offset: [0, 0],
        span: [0, 0],
        rotation: UvRotation::Deg0,
        flip_u: false,
        flip_v: false,
    };

    /// `true` when this transform leaves UVs unchanged.
    pub const fn is_identity(&self) -> bool {
        self.offset[0] == 0
            && self.offset[1] == 0
            && self.span[0] == 0
            && self.span[1] == 0
            && matches!(self.rotation, UvRotation::Deg0)
            && !self.flip_u
            && !self.flip_v
    }

    /// Apply the transform to a quad's corner UVs.
    ///
    /// The input order can be any perimeter order (`[NW, NE, SE, SW]`
    /// for floors or `[BL, BR, TR, TL]` for walls); the transform is
    /// computed inside the UV rectangle spanned by those four points.
    pub fn apply_to_quad(self, uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
        if self.is_identity() {
            return uvs;
        }
        let bounds = uv_bounds(uvs);
        [
            self.apply_one(uvs[0], bounds),
            self.apply_one(uvs[1], bounds),
            self.apply_one(uvs[2], bounds),
            self.apply_one(uvs[3], bounds),
        ]
    }

    fn apply_one(self, uv: (u8, u8), bounds: UvBounds) -> (u8, u8) {
        let width = bounds.max_u - bounds.min_u;
        let height = bounds.max_v - bounds.min_v;
        if width == 0 || height == 0 {
            return (
                wrap_uv(uv.0 as i32 + self.offset[0] as i32),
                wrap_uv(uv.1 as i32 + self.offset[1] as i32),
            );
        }

        let mut u = uv.0 as i32 - bounds.min_u;
        let mut v = uv.1 as i32 - bounds.min_v;
        if self.flip_u {
            u = width - u;
        }
        if self.flip_v {
            v = height - v;
        }

        let (u, v) = match self.rotation {
            UvRotation::Deg0 => (u, v),
            UvRotation::Deg45 => rotate_uv_diagonal_fit(u, v, width, height, 1),
            UvRotation::Deg90 => (
                width - scale_rounded(v, width, height),
                scale_rounded(u, height, width),
            ),
            UvRotation::Deg135 => rotate_uv_diagonal_fit(u, v, width, height, 3),
            UvRotation::Deg180 => (width - u, height - v),
            UvRotation::Deg225 => rotate_uv_diagonal_fit(u, v, width, height, 5),
            UvRotation::Deg270 => (
                scale_rounded(v, width, height),
                height - scale_rounded(u, height, width),
            ),
            UvRotation::Deg315 => rotate_uv_diagonal_fit(u, v, width, height, 7),
        };
        let span_u = self.effective_span_axis(0, width);
        let span_v = self.effective_span_axis(1, height);
        let u = scale_rounded(u, span_u, width);
        let v = scale_rounded(v, span_v, height);

        (
            wrap_uv(bounds.min_u + u + self.offset[0] as i32),
            wrap_uv(bounds.min_v + v + self.offset[1] as i32),
        )
    }

    fn effective_span_axis(self, axis: usize, fallback: i32) -> i32 {
        let span = self.span[axis];
        if span == 0 {
            fallback
        } else {
            i32::from(span.min(255))
        }
    }
}

impl Default for UvTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy)]
pub(crate) struct UvBounds {
    min_u: i32,
    max_u: i32,
    min_v: i32,
    max_v: i32,
}

pub(crate) fn uv_bounds(uvs: [(u8, u8); 4]) -> UvBounds {
    let mut min_u = uvs[0].0 as i32;
    let mut max_u = min_u;
    let mut min_v = uvs[0].1 as i32;
    let mut max_v = min_v;
    for (u, v) in uvs {
        let u = u as i32;
        let v = v as i32;
        min_u = min_u.min(u);
        max_u = max_u.max(u);
        min_v = min_v.min(v);
        max_v = max_v.max(v);
    }
    UvBounds {
        min_u,
        max_u,
        min_v,
        max_v,
    }
}

pub(crate) fn scale_rounded(value: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        0
    } else {
        (value.saturating_mul(numerator) + denominator / 2) / denominator
    }
}

pub(crate) fn signed_div_round(value: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        0
    } else if value >= 0 {
        (value + denominator / 2) / denominator
    } else {
        (value - denominator / 2) / denominator
    }
}

pub(crate) fn rotate_uv_diagonal_fit(
    u: i32,
    v: i32,
    width: i32,
    height: i32,
    clockwise_steps: u8,
) -> (i32, i32) {
    const Q: i32 = 4096;
    const HALF_Q: i32 = Q / 2;

    let du = signed_div_round((u.saturating_mul(2) - width).saturating_mul(Q), width);
    let dv = signed_div_round((v.saturating_mul(2) - height).saturating_mul(Q), height);
    let (cos_q, sin_q) = match clockwise_steps & 7 {
        1 => (HALF_Q, HALF_Q),
        3 => (-HALF_Q, HALF_Q),
        5 => (-HALF_Q, -HALF_Q),
        7 => (HALF_Q, -HALF_Q),
        _ => (Q, 0),
    };
    let rotated_u = signed_div_round(
        cos_q
            .saturating_mul(du)
            .saturating_sub(sin_q.saturating_mul(dv)),
        Q,
    );
    let rotated_v = signed_div_round(
        sin_q
            .saturating_mul(du)
            .saturating_add(cos_q.saturating_mul(dv)),
        Q,
    );

    (
        signed_div_round((rotated_u + Q).saturating_mul(width), Q * 2),
        signed_div_round((rotated_v + Q).saturating_mul(height), Q * 2),
    )
}

pub(crate) fn wrap_uv(value: i32) -> u8 {
    value.rem_euclid(256) as u8
}

pub(crate) const fn is_default_uv_span(span: &[u16; 2]) -> bool {
    span[0] == 0 && span[1] == 0
}

/// World-unit step every authored vertex height must align to.
///
/// The X / Z grid is locked by construction -- corners are always
/// computed from the cell's array index and `sector_size`. Y is
/// the only free axis, and we constrain it to multiples of this
/// step so the editor can't author noise heights that the runtime
/// quantises away anyway.
///
/// 64 is `sector_size / 16` at the default 1024 -- fine enough for
/// authored slopes, coarse enough that PS1 i16 vertex jitter never
/// fights the snap.
pub const HEIGHT_QUANTUM: i32 = 64;
/// World grid size quantum. The editor stores one sector size per
/// World node and snaps it to this step so room/cook math stays
/// integer and PSX-friendly.
pub const WORLD_SECTOR_SIZE_QUANTUM: i32 = 128;
/// Default sector size used by starter/legacy projects.
pub const DEFAULT_WORLD_SECTOR_SIZE: i32 = 1024;
/// Default third-person camera distance inherited by rooms.
pub const DEFAULT_WORLD_CAMERA_DISTANCE: i32 = 2000;
/// Default camera origin height above the player origin.
pub const DEFAULT_WORLD_CAMERA_HEIGHT: i32 = 1000;
/// Default look-at height above the player origin.
pub const DEFAULT_WORLD_CAMERA_TARGET_HEIGHT: i32 = 850;
/// Default additional lock-on camera elevation as a percentage of Height.
pub const DEFAULT_WORLD_CAMERA_LOCK_RISE_PERCENT: u8 = 25;
/// Default minimum camera origin height above the sampled floor.
pub const DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE: i32 = HEIGHT_QUANTUM;
/// Default manual camera orbit speed level. Higher values turn faster.
pub const DEFAULT_WORLD_CAMERA_ORBIT_SPEED_LEVEL: u8 = 5;
/// Default camera position follow lag shift. Lower values move faster.
pub const DEFAULT_WORLD_CAMERA_POSITION_LAG_SHIFT: u8 = 2;
/// Default camera focus follow lag shift. Lower values move faster.
pub const DEFAULT_WORLD_CAMERA_FOCUS_LAG_SHIFT: u8 = 2;
/// Default camera boom-distance recovery lag shift. Lower values move faster.
pub const DEFAULT_WORLD_CAMERA_DISTANCE_LAG_SHIFT: u8 = 3;
/// Minimum authored third-person camera distance.
// Degenerate-distance floor only. The cook normalizes AFTER the 16x
// engine-unit divide, so this must be harmless at BOTH scales; 384
// silently clamped every engine-scale (Quake-unit) camera up to a
// far view.
pub const MIN_WORLD_CAMERA_DISTANCE: i32 = 16;
/// Maximum authored third-person camera distance.
pub const MAX_WORLD_CAMERA_DISTANCE: i32 = 16_384;
/// Maximum authored camera vertical offset.
pub const MAX_WORLD_CAMERA_HEIGHT: i32 = 16_384;
/// Maximum authored lock-on camera elevation percentage.
pub const MAX_WORLD_CAMERA_LOCK_RISE_PERCENT: u8 = 100;
/// Maximum authored minimum floor clearance for the third-person camera.
pub const MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE: i32 = 4_096;
/// Minimum authored manual camera orbit speed level.
pub const MIN_WORLD_CAMERA_ORBIT_SPEED_LEVEL: u8 = 1;
/// Maximum authored manual camera orbit speed level.
pub const MAX_WORLD_CAMERA_ORBIT_SPEED_LEVEL: u8 = 7;
/// Maximum authored camera follow lag shift.
pub const MAX_WORLD_CAMERA_LAG_SHIFT: u8 = 6;
/// Default wall span when no ceiling is authored above the edge.
pub const DEFAULT_WALL_HEIGHT_SECTORS: i32 = 2;
/// Minimum authored sector size.
pub const MIN_WORLD_SECTOR_SIZE: i32 = WORLD_SECTOR_SIZE_QUANTUM;
/// Maximum authored sector size. This is an authoring sanity cap,
/// not a PSX wire-format limit.
pub const MAX_WORLD_SECTOR_SIZE: i32 = 8192;
/// Fixed-point one for authored model resource scale.
pub const MODEL_SCALE_ONE_Q8: u16 = 256;

/// Snap a vertex height to the nearest [`HEIGHT_QUANTUM`] multiple.
///
/// Round-half-away-from-zero so the snap is symmetric for
/// negative heights -- `snap_height(-31)` returns `0`,
/// `snap_height(-32)` returns `-64`. Plain integer math; no
/// float intermediaries.
pub fn snap_height(y: i32) -> i32 {
    let q = HEIGHT_QUANTUM;
    let half = q / 2;
    if y >= 0 {
        ((y + half) / q) * q
    } else {
        -(((-y + half) / q) * q)
    }
}

/// Snap a requested World sector size to a positive 128-unit grid.
pub fn snap_world_sector_size(size: i32) -> i32 {
    let clamped = size.clamp(MIN_WORLD_SECTOR_SIZE, MAX_WORLD_SECTOR_SIZE);
    ((clamped + WORLD_SECTOR_SIZE_QUANTUM / 2) / WORLD_SECTOR_SIZE_QUANTUM)
        * WORLD_SECTOR_SIZE_QUANTUM
}

pub(crate) fn default_world_sector_size() -> i32 {
    DEFAULT_WORLD_SECTOR_SIZE
}

pub(crate) const fn default_world_camera_distance() -> i32 {
    DEFAULT_WORLD_CAMERA_DISTANCE
}

pub(crate) const fn default_world_camera_height() -> i32 {
    DEFAULT_WORLD_CAMERA_HEIGHT
}

pub(crate) const fn default_world_camera_target_height() -> i32 {
    DEFAULT_WORLD_CAMERA_TARGET_HEIGHT
}

pub(crate) const fn default_world_camera_lock_rise_percent() -> u8 {
    DEFAULT_WORLD_CAMERA_LOCK_RISE_PERCENT
}

pub(crate) const fn default_world_camera_min_floor_clearance() -> i32 {
    DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE
}

pub(crate) const fn default_world_camera_orbit_speed_level() -> u8 {
    DEFAULT_WORLD_CAMERA_ORBIT_SPEED_LEVEL
}

pub(crate) const fn default_world_camera_position_lag_shift() -> u8 {
    DEFAULT_WORLD_CAMERA_POSITION_LAG_SHIFT
}

pub(crate) const fn default_world_camera_focus_lag_shift() -> u8 {
    DEFAULT_WORLD_CAMERA_FOCUS_LAG_SHIFT
}

pub(crate) const fn default_world_camera_distance_lag_shift() -> u8 {
    DEFAULT_WORLD_CAMERA_DISTANCE_LAG_SHIFT
}

pub(crate) fn default_model_scale_q8() -> [u16; 3] {
    [MODEL_SCALE_ONE_Q8; 3]
}

pub(crate) fn default_model_renderer_visual_scale_q8() -> u16 {
    MODEL_SCALE_ONE_Q8
}

pub(crate) fn scale_i32_ratio(value: i32, from: i32, to: i32) -> i32 {
    if from <= 0 || from == to {
        return value;
    }
    (((value as i64) * (to as i64) + (from as i64 / 2)) / (from as i64)) as i32
}

pub(crate) fn scale_u16_ratio(value: u16, from: i32, to: i32) -> u16 {
    scale_i32_ratio(value as i32, from, to).clamp(0, u16::MAX as i32) as u16
}

#[cfg(test)]
mod material_uv_motion_tests {
    use super::{
        MaterialTextureMode, MaterialUvMotion, ModelSecondaryLayer, ModelSecondaryTexture,
        ProceduralNoiseTexture,
    };

    #[test]
    fn newly_authored_overlay_moves_independently_by_default() {
        let layer = ModelSecondaryLayer::moving_default();
        assert!(layer.motion.enabled);
        assert_eq!(layer.motion.speed_u_q8, 2 * 256);
        assert_eq!(layer.motion.speed_v_q8, 256);
    }

    #[test]
    fn motion_uses_signed_q8_speed_and_wraps() {
        let motion = MaterialUvMotion {
            enabled: true,
            speed_u_q8: 8 * 256,
            speed_v_q8: -4 * 256,
            phase_u: 250,
            phase_v: 2,
        };
        assert_eq!(motion.offset_at_tick(30, 60), [254, 0]);
        assert_eq!(motion.offset_at_tick(60, 60), [2, 254]);
        assert_eq!(motion.offset_at_tick(1, 60), [250, 2]);
    }

    #[test]
    fn disabled_motion_preserves_authored_phase() {
        let motion = MaterialUvMotion {
            enabled: false,
            phase_u: 19,
            phase_v: 37,
            ..MaterialUvMotion::default()
        };
        assert_eq!(motion.offset_at_tick(u32::MAX, 60), [19, 37]);
    }

    #[test]
    fn legacy_noise_overlay_migrates_to_generated_layer_two() {
        let noise = ProceduralNoiseTexture {
            seed: 91,
            ..ProceduralNoiseTexture::default()
        };
        let mut layer = ModelSecondaryLayer {
            legacy_texture: Some(ModelSecondaryTexture::ProceduralNoise(noise)),
            ..ModelSecondaryLayer::default()
        };
        layer.normalize_legacy_source();
        assert_eq!(layer.texture_mode, MaterialTextureMode::Generated);
        assert_eq!(layer.generated.noise, noise);
        assert!(layer.legacy_texture.is_none());
    }
}

#[cfg(test)]
mod sidedness_tests {
    use super::MaterialFaceSidedness;

    #[test]
    fn brush_faces_draw_front_for_both_like_the_pxbsp_runtime() {
        use MaterialFaceSidedness::*;
        assert_eq!(Front.brush_face_drawn(), Front);
        assert_eq!(Both.brush_face_drawn(), Front);
        assert_eq!(Back.brush_face_drawn(), Back);
    }
}
