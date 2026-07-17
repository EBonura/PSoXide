use super::*;

/// Anchor point used to resolve a UI rectangle against its parent
/// rectangle or the root canvas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiAnchor {
    /// Parent top-left corner.
    #[default]
    TopLeft,
    /// Parent top edge midpoint.
    Top,
    /// Parent top-right corner.
    TopRight,
    /// Parent left edge midpoint.
    Left,
    /// Parent center.
    Center,
    /// Parent right edge midpoint.
    Right,
    /// Parent bottom-left corner.
    BottomLeft,
    /// Parent bottom edge midpoint.
    Bottom,
    /// Parent bottom-right corner.
    BottomRight,
}

impl UiAnchor {
    /// Stable list used by editor controls.
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    /// Compact display label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Top Left",
            Self::Top => "Top",
            Self::TopRight => "Top Right",
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
            Self::BottomLeft => "Bottom Left",
            Self::Bottom => "Bottom",
            Self::BottomRight => "Bottom Right",
        }
    }

    /// Tiny grid label for dense editor controls.
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::TopLeft => "TL",
            Self::Top => "T",
            Self::TopRight => "TR",
            Self::Left => "L",
            Self::Center => "C",
            Self::Right => "R",
            Self::BottomLeft => "BL",
            Self::Bottom => "B",
            Self::BottomRight => "BR",
        }
    }

    /// Runtime flag value. Kept stable for cooked manifests.
    pub const fn runtime_bits(self) -> u16 {
        match self {
            Self::TopLeft => 0,
            Self::Top => 1,
            Self::TopRight => 2,
            Self::Left => 3,
            Self::Center => 4,
            Self::Right => 5,
            Self::BottomLeft => 6,
            Self::Bottom => 7,
            Self::BottomRight => 8,
        }
    }

    const fn factors(self) -> (i32, i32) {
        match self {
            Self::TopLeft => (0, 0),
            Self::Top => (1, 0),
            Self::TopRight => (2, 0),
            Self::Left => (0, 1),
            Self::Center => (1, 1),
            Self::Right => (2, 1),
            Self::BottomLeft => (0, 2),
            Self::Bottom => (1, 2),
            Self::BottomRight => (2, 2),
        }
    }
}

/// Text alignment for authored UI labels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiTextAlign {
    /// Align text to the left edge.
    #[default]
    Left,
    /// Center text within the label rectangle.
    Center,
    /// Align text to the right edge.
    Right,
}

impl UiTextAlign {
    /// Stable list used by editor controls.
    pub const ALL: [Self; 3] = [Self::Left, Self::Center, Self::Right];

    /// Compact display label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
        }
    }

    /// Runtime flag value. Kept stable for cooked manifests.
    pub const fn runtime_bits(self) -> u16 {
        match self {
            Self::Left => 0,
            Self::Center => 1,
            Self::Right => 2,
        }
    }
}

/// Direction for authored UI gradients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiGradientDirection {
    /// Interpolate from top to bottom.
    #[default]
    Vertical,
    /// Interpolate from left to right.
    Horizontal,
}

/// Runtime condition controlling whether an authored UI node is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiVisibilityCondition {
    /// Always draw the node.
    #[default]
    Always,
    /// Draw only while the current pad is not reporting DualShock analog mode.
    AnalogInactive,
}

impl UiVisibilityCondition {
    /// Stable list used by editor controls.
    pub const ALL: [Self; 2] = [Self::Always, Self::AnalogInactive];

    /// Compact display label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Always => "Always",
            Self::AnalogInactive => "Analog inactive",
        }
    }
}

impl UiGradientDirection {
    /// Stable list used by editor controls.
    pub const ALL: [Self; 2] = [Self::Vertical, Self::Horizontal];

    /// Compact display label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Vertical => "Vertical",
            Self::Horizontal => "Horizontal",
        }
    }
}

/// Optional second colour for a UI paint, paired with a direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiGradient {
    /// Target colour at the far edge of the gradient.
    #[serde(default = "default_ui_gradient_to")]
    pub to: [u8; 3],
    /// Interpolation direction.
    #[serde(default)]
    pub direction: UiGradientDirection,
}

impl UiGradient {
    /// Construct a gradient paint from an existing solid colour to `to`.
    pub const fn new(to: [u8; 3], direction: UiGradientDirection) -> Self {
        Self { to, direction }
    }
}

/// Default far colour used when an authored gradient omits it.
pub const fn default_ui_gradient_to() -> [u8; 3] {
    [255, 255, 255]
}

/// Screen-space rectangle in authored PSX framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiRect {
    /// Local X offset from the selected anchor.
    pub x: i16,
    /// Local Y offset from the selected anchor.
    pub y: i16,
    /// Width in canvas pixels.
    pub width: u16,
    /// Height in canvas pixels.
    pub height: u16,
    /// Parent/canvas anchor used to resolve `x` and `y`.
    #[serde(default)]
    pub anchor: UiAnchor,
    /// Clockwise visual rotation around this rectangle's centre, in degrees.
    #[serde(default)]
    pub rotation_degrees: i16,
    /// Mirror the rectangle's local X axis before rotation.
    #[serde(default)]
    pub flip_x: bool,
    /// Mirror the rectangle's local Y axis before rotation.
    #[serde(default)]
    pub flip_y: bool,
}

impl UiRect {
    /// Build a screen-space UI rectangle.
    pub const fn new(x: i16, y: i16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
            anchor: UiAnchor::TopLeft,
            rotation_degrees: 0,
            flip_x: false,
            flip_y: false,
        }
    }

    /// Return a copy using a different anchor.
    pub const fn with_anchor(mut self, anchor: UiAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Return a copy using visual rotation around its centre.
    pub const fn with_rotation(mut self, rotation_degrees: i16) -> Self {
        self.rotation_degrees = rotation_degrees;
        self
    }

    /// Return a copy using visual mirror flags.
    pub const fn with_flips(mut self, flip_x: bool, flip_y: bool) -> Self {
        self.flip_x = flip_x;
        self.flip_y = flip_y;
        self
    }
}

impl Default for UiRect {
    fn default() -> Self {
        Self::new(0, 0, 32, 12)
    }
}

/// Runtime value a UI element can bind to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiValueBinding {
    /// Literal fixed-point Q12 value.
    ConstantQ12(i32),
    /// Live project option value.
    Option(OptionId),
    /// Player health value.
    PlayerHealth,
    /// Player health maximum.
    PlayerHealthMax,
    /// Player stamina value.
    PlayerStamina,
    /// Player stamina maximum.
    PlayerStaminaMax,
    /// Live world-load progress (Q12, 0..=4096) while the engine
    /// streams the next state's world. Zero outside loading screens.
    LoadingProgress,
}

impl UiValueBinding {
    /// Human-readable label for editor UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ConstantQ12(_) => "Constant",
            Self::Option(_) => "Option",
            Self::PlayerHealth => "Player Health",
            Self::PlayerHealthMax => "Player Health Max",
            Self::PlayerStamina => "Player Stamina",
            Self::PlayerStaminaMax => "Player Stamina Max",
            Self::LoadingProgress => "Loading Progress",
        }
    }
}

/// Authored full-screen transition effect kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiTransitionKind {
    /// No transition; switch immediately.
    #[default]
    None,
    /// Darken the frame toward the transition colour.
    Fade,
    /// Deterministic random block cover.
    BlockDissolve,
    /// Horizon-Glide-style digital break.
    GlitchBreak,
}

impl UiTransitionKind {
    /// Human-readable editor label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Fade => "Fade",
            Self::BlockDissolve => "Block Dissolve",
            Self::GlitchBreak => "Glitch Break",
        }
    }
}

/// Authored full-screen transition settings for button-driven flow changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiTransition {
    /// Effect variant.
    #[serde(default)]
    pub kind: UiTransitionKind,
    /// Duration in visual frames.
    #[serde(default = "default_ui_transition_frames")]
    pub frames: u16,
    /// Overlay colour. Usually black.
    #[serde(default)]
    pub color: [u8; 3],
    /// Deterministic noise seed.
    #[serde(default = "default_ui_transition_seed")]
    pub seed: u16,
}

impl Default for UiTransition {
    fn default() -> Self {
        Self {
            kind: UiTransitionKind::None,
            frames: default_ui_transition_frames(),
            color: [0, 0, 0],
            seed: default_ui_transition_seed(),
        }
    }
}

impl UiTransition {
    /// Default glitch transition for title/menu handoffs.
    pub const fn glitch_break() -> Self {
        Self {
            kind: UiTransitionKind::GlitchBreak,
            frames: 45,
            color: [0, 0, 0],
            seed: 0x4b1d,
        }
    }
}

pub(crate) fn default_ui_transition_frames() -> u16 {
    45
}

pub(crate) fn default_ui_transition_seed() -> u16 {
    0x4b1d
}

/// Action a [`UiNodeKind::Button`] fires when activated. Runtime
/// dispatch is a later step; this carries the authored intent so the
/// cook can lower it to a [`psx_level::LevelUiAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UiAction {
    /// Switch to a composed screen state by stable id.
    GotoState(SceneStateId),
    /// Switch to a composed screen state through a full-screen transition.
    TransitionToState {
        /// Target screen state.
        state: SceneStateId,
        /// Transition effect.
        transition: UiTransition,
    },
    /// Switch to another authored UI scene by stable id.
    GotoScene(UiSceneId),
    /// Switch to another authored UI scene through a full-screen transition.
    TransitionToScene {
        /// Target UI scene.
        scene: UiSceneId,
        /// Transition effect.
        transition: UiTransition,
    },
    /// Enter the gameplay/level simulation.
    StartGameplay,
    /// Enter gameplay through a full-screen transition.
    StartGameplayTransition {
        /// Transition effect.
        transition: UiTransition,
    },
    /// Return to the previous menu/scene.
    #[default]
    Back,
    /// Adjust a project option by a signed delta.
    SetOption {
        /// Target option id.
        option: OptionId,
        /// Signed step applied to the option value.
        delta: i32,
    },
    /// Game-specific action dispatched by opaque id.
    Game(u16),
}

impl UiAction {
    /// Editor-facing label for the action variant.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::GotoState(_) => "Go To State",
            Self::TransitionToState { .. } => "Transition To State",
            Self::GotoScene(_) => "Go To Scene",
            Self::TransitionToScene { .. } => "Transition To Scene",
            Self::StartGameplay => "Start Gameplay",
            Self::StartGameplayTransition { .. } => "Transition To Gameplay",
            Self::Back => "Back",
            Self::SetOption { .. } => "Set Option",
            Self::Game(_) => "Game Action",
        }
    }
}

/// Pitch multiplier that plays a UI SFX cue at its source pitch.
pub const UI_SFX_PITCH_UNITY_Q12: u16 = 4096;
/// Playback-speed multiplier that bakes a music cue at its source tempo.
pub const UI_MUSIC_PLAYBACK_SPEED_UNITY_Q12: u16 = 4096;

pub(crate) fn default_ui_sfx_volume() -> u8 {
    80
}

pub(crate) fn default_ui_sfx_pitch_q12() -> u16 {
    UI_SFX_PITCH_UNITY_Q12
}

pub(crate) fn default_ui_music_playback_speed_q12() -> u16 {
    UI_MUSIC_PLAYBACK_SPEED_UNITY_Q12
}

/// One editor-authored SFX choice for a UI event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSfxCue {
    /// Project-relative source WAV. The playtest/export cook converts it to
    /// PS1 SPU ADPCM automatically.
    #[serde(default)]
    pub wav_path: String,
    /// Voice volume as a percentage of full scale.
    #[serde(default = "default_ui_sfx_volume")]
    pub volume: u8,
    /// Q12 pitch multiplier (`4096` = source pitch).
    #[serde(default = "default_ui_sfx_pitch_q12")]
    pub pitch_q12: u16,
}

impl Default for UiSfxCue {
    fn default() -> Self {
        Self {
            wav_path: String::new(),
            volume: default_ui_sfx_volume(),
            pitch_q12: default_ui_sfx_pitch_q12(),
        }
    }
}

/// Per-event SFX pools shared by interactive UI nodes. Button nodes use
/// `focus` + `activate`; sliders use `focus` + `nudge` + `limit`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSfxBindings {
    /// Played when focus moves onto the control.
    #[serde(default)]
    pub focus: Vec<UiSfxCue>,
    /// Played when a button is activated.
    #[serde(default)]
    pub activate: Vec<UiSfxCue>,
    /// Played when a slider changes value.
    #[serde(default)]
    pub nudge: Vec<UiSfxCue>,
    /// Played when a slider is nudged against its clamped limit.
    #[serde(default)]
    pub limit: Vec<UiSfxCue>,
}

pub(crate) fn normalize_ui_sfx_bindings(sfx: &mut UiSfxBindings) {
    for cue in sfx
        .focus
        .iter_mut()
        .chain(sfx.activate.iter_mut())
        .chain(sfx.nudge.iter_mut())
        .chain(sfx.limit.iter_mut())
    {
        cue.volume = cue.volume.min(100);
        cue.pitch_q12 = cue.pitch_q12.clamp(1, 0x3FFF);
    }
}

/// How a project [`OptionDef`] is interpreted and edited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionKind {
    /// Bounded integer with a fixed step.
    IntRange {
        /// Minimum value.
        min: i32,
        /// Maximum value.
        max: i32,
        /// Increment applied per step.
        step: i32,
        /// Default value.
        default: i32,
    },
    /// Choice from a fixed list of named variants.
    Enum {
        /// Ordered variant labels.
        variants: Vec<String>,
        /// Default variant index.
        default: usize,
    },
    /// On/off toggle.
    Bool {
        /// Default value.
        default: bool,
    },
}

impl Default for OptionKind {
    fn default() -> Self {
        Self::IntRange {
            min: 0,
            max: 10,
            step: 1,
            default: 5,
        }
    }
}

impl OptionKind {
    /// Editor-facing label for the option kind.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::IntRange { .. } => "Int Range",
            Self::Enum { .. } => "Enum",
            Self::Bool { .. } => "Bool",
        }
    }
}

/// One project-level tunable option. Sliders and `SetOption` button
/// actions reference an option by its stable [`OptionId`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionDef {
    /// Stable id, preserved across renames and reorders.
    pub id: OptionId,
    /// Display name.
    pub name: String,
    /// Value domain and default.
    pub kind: OptionKind,
}

/// Built-in bitmap font a text UI node draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UiFontChoice {
    /// 8x8 basic ASCII font.
    #[default]
    Basic,
    /// 8x16 basic ASCII font.
    Basic8x16,
    /// Kenney Blocks display font.
    KenneyBlocks,
    /// Kenney Future display font.
    KenneyFuture,
    /// Kenney Future Narrow display font.
    KenneyFutureNarrow,
    /// Kenney High display font.
    KenneyHigh,
    /// Kenney High Square display font.
    KenneyHighSquare,
    /// Kenney Mini UI font.
    KenneyMini,
    /// Kenney Mini Square UI font.
    KenneyMiniSquare,
    /// Kenney Mini Square Mono UI font.
    KenneyMiniSquareMono,
    /// Kenney Pixel UI font.
    KenneyPixel,
    /// Kenney Pixel Square UI font.
    KenneyPixelSquare,
    /// Kenney Rocket display font.
    KenneyRocket,
    /// Kenney Rocket Square display font.
    KenneyRocketSquare,
    /// Press Start 2P display font.
    PressStart2P,
    /// Silkscreen UI font.
    Silkscreen,
    /// Pixelify Sans display font.
    PixelifySans,
    /// Orbitron display font.
    Orbitron,
    /// Audiowide display font.
    Audiowide,
    /// Michroma display font.
    Michroma,
    /// Electrolize UI font.
    Electrolize,
    /// Oxanium UI font.
    Oxanium,
    /// Rajdhani UI font.
    Rajdhani,
    /// Chakra Petch UI font.
    ChakraPetch,
    /// Tektur display font.
    Tektur,
    /// Tomorrow UI font.
    Tomorrow,
    /// Zen Dots display font.
    ZenDots,
    /// Turret Road display font.
    TurretRoad,
    /// Tiny5 display font.
    Tiny5,
    /// Jersey 10 display font.
    Jersey10,
    /// Space Mono display font.
    SpaceMono,
    /// Bruno Ace display font.
    BrunoAce,
    /// Aldrich display font.
    Aldrich,
    /// Syncopate display font.
    Syncopate,
    /// Share Tech Mono UI font.
    ShareTechMono,
    /// Jura UI font.
    Jura,
}

impl UiFontChoice {
    /// All editor-selectable built-in UI fonts.
    pub const ALL: [Self; 36] = [
        Self::Basic,
        Self::Basic8x16,
        Self::KenneyBlocks,
        Self::KenneyFuture,
        Self::KenneyFutureNarrow,
        Self::KenneyHigh,
        Self::KenneyHighSquare,
        Self::KenneyMini,
        Self::KenneyMiniSquare,
        Self::KenneyMiniSquareMono,
        Self::KenneyPixel,
        Self::KenneyPixelSquare,
        Self::KenneyRocket,
        Self::KenneyRocketSquare,
        Self::PressStart2P,
        Self::Silkscreen,
        Self::PixelifySans,
        Self::Orbitron,
        Self::Audiowide,
        Self::Michroma,
        Self::Electrolize,
        Self::Oxanium,
        Self::Rajdhani,
        Self::ChakraPetch,
        Self::Tektur,
        Self::Tomorrow,
        Self::ZenDots,
        Self::TurretRoad,
        Self::Tiny5,
        Self::Jersey10,
        Self::SpaceMono,
        Self::BrunoAce,
        Self::Aldrich,
        Self::Syncopate,
        Self::ShareTechMono,
        Self::Jura,
    ];

    /// Editor-facing label for this font.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Basic => "Basic 8x8",
            Self::Basic8x16 => "Basic 8x16",
            Self::KenneyBlocks => "Kenney Blocks",
            Self::KenneyFuture => "Kenney Future",
            Self::KenneyFutureNarrow => "Kenney Future Narrow",
            Self::KenneyHigh => "Kenney High",
            Self::KenneyHighSquare => "Kenney High Square",
            Self::KenneyMini => "Kenney Mini",
            Self::KenneyMiniSquare => "Kenney Mini Square",
            Self::KenneyMiniSquareMono => "Kenney Mini Square Mono",
            Self::KenneyPixel => "Kenney Pixel",
            Self::KenneyPixelSquare => "Kenney Pixel Square",
            Self::KenneyRocket => "Kenney Rocket",
            Self::KenneyRocketSquare => "Kenney Rocket Square",
            Self::PressStart2P => "Press Start 2P",
            Self::Silkscreen => "Silkscreen",
            Self::PixelifySans => "Pixelify Sans",
            Self::Orbitron => "Orbitron",
            Self::Audiowide => "Audiowide",
            Self::Michroma => "Michroma",
            Self::Electrolize => "Electrolize",
            Self::Oxanium => "Oxanium",
            Self::Rajdhani => "Rajdhani",
            Self::ChakraPetch => "Chakra Petch",
            Self::Tektur => "Tektur",
            Self::Tomorrow => "Tomorrow",
            Self::ZenDots => "Zen Dots",
            Self::TurretRoad => "Turret Road",
            Self::Tiny5 => "Tiny5",
            Self::Jersey10 => "Jersey 10",
            Self::SpaceMono => "Space Mono",
            Self::BrunoAce => "Bruno Ace",
            Self::Aldrich => "Aldrich",
            Self::Syncopate => "Syncopate",
            Self::ShareTechMono => "Share Tech Mono",
            Self::Jura => "Jura",
        }
    }

    /// Stable editor-preview texture slug.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Basic => "basic-8x8",
            Self::Basic8x16 => "basic-8x16",
            Self::KenneyBlocks => "kenney-blocks",
            Self::KenneyFuture => "kenney-future",
            Self::KenneyFutureNarrow => "kenney-future-narrow",
            Self::KenneyHigh => "kenney-high",
            Self::KenneyHighSquare => "kenney-high-square",
            Self::KenneyMini => "kenney-mini",
            Self::KenneyMiniSquare => "kenney-mini-square",
            Self::KenneyMiniSquareMono => "kenney-mini-square-mono",
            Self::KenneyPixel => "kenney-pixel",
            Self::KenneyPixelSquare => "kenney-pixel-square",
            Self::KenneyRocket => "kenney-rocket",
            Self::KenneyRocketSquare => "kenney-rocket-square",
            Self::PressStart2P => "press-start-2p",
            Self::Silkscreen => "silkscreen",
            Self::PixelifySans => "pixelify-sans",
            Self::Orbitron => "orbitron",
            Self::Audiowide => "audiowide",
            Self::Michroma => "michroma",
            Self::Electrolize => "electrolize",
            Self::Oxanium => "oxanium",
            Self::Rajdhani => "rajdhani",
            Self::ChakraPetch => "chakra-petch",
            Self::Tektur => "tektur",
            Self::Tomorrow => "tomorrow",
            Self::ZenDots => "zen-dots",
            Self::TurretRoad => "turret-road",
            Self::Tiny5 => "tiny5",
            Self::Jersey10 => "jersey-10",
            Self::SpaceMono => "space-mono",
            Self::BrunoAce => "bruno-ace",
            Self::Aldrich => "aldrich",
            Self::Syncopate => "syncopate",
            Self::ShareTechMono => "share-tech-mono",
            Self::Jura => "jura",
        }
    }

    /// Runtime font-table selector written into cooked UI nodes.
    pub const fn runtime_index(self) -> u8 {
        match self {
            Self::Basic => 0,
            Self::Basic8x16 => 1,
            Self::KenneyBlocks => 2,
            Self::KenneyFuture => 3,
            Self::KenneyFutureNarrow => 4,
            Self::KenneyHigh => 5,
            Self::KenneyHighSquare => 6,
            Self::KenneyMini => 7,
            Self::KenneyMiniSquare => 8,
            Self::KenneyMiniSquareMono => 9,
            Self::KenneyPixel => 10,
            Self::KenneyPixelSquare => 11,
            Self::KenneyRocket => 12,
            Self::KenneyRocketSquare => 13,
            Self::PressStart2P => 14,
            Self::Silkscreen => 15,
            Self::PixelifySans => 16,
            Self::Orbitron => 17,
            Self::Audiowide => 18,
            Self::Michroma => 19,
            Self::Electrolize => 20,
            Self::Oxanium => 21,
            Self::Rajdhani => 22,
            Self::ChakraPetch => 23,
            Self::Tektur => 24,
            Self::Tomorrow => 25,
            Self::ZenDots => 26,
            Self::TurretRoad => 27,
            Self::Tiny5 => 28,
            Self::Jersey10 => 29,
            Self::SpaceMono => 30,
            Self::BrunoAce => 31,
            Self::Aldrich => 32,
            Self::Syncopate => 33,
            Self::ShareTechMono => 34,
            Self::Jura => 35,
        }
    }
}

/// Q8 fixed-point value for 1.0x authored UI font scale.
pub const UI_FONT_SCALE_ONE_Q8: u16 = 256;
/// Smallest authored UI font scale, Q8 fixed point (0.5x).
pub const MIN_UI_FONT_SCALE: u16 = UI_FONT_SCALE_ONE_Q8 / 2;
/// Largest authored UI font scale, Q8 fixed point (8.0x).
pub const MAX_UI_FONT_SCALE: u16 = UI_FONT_SCALE_ONE_Q8 * 8;
/// Tightest authored UI letter spacing, in PSX framebuffer pixels.
pub const MIN_UI_LETTER_SPACING: i8 = -16;
/// Widest authored UI letter spacing, in PSX framebuffer pixels.
pub const MAX_UI_LETTER_SPACING: i8 = 64;

/// Default authored UI font scale.
pub const fn default_ui_font_scale() -> u16 {
    UI_FONT_SCALE_ONE_Q8
}

/// Clamp an authored UI font scale in Q8 fixed-point units.
pub const fn clamp_ui_font_scale(scale_q8: u16) -> u16 {
    if scale_q8 < MIN_UI_FONT_SCALE {
        MIN_UI_FONT_SCALE
    } else if scale_q8 > MAX_UI_FONT_SCALE {
        MAX_UI_FONT_SCALE
    } else {
        scale_q8
    }
}

/// Convert an authored Q8 UI font scale to an editor-facing multiplier.
pub fn ui_font_scale_q8_to_f32(scale_q8: u16) -> f32 {
    f32::from(clamp_ui_font_scale(scale_q8)) / f32::from(UI_FONT_SCALE_ONE_Q8)
}

/// Convert an editor-facing multiplier into authored Q8 UI font scale.
pub fn ui_font_scale_f32_to_q8(scale: f32) -> u16 {
    if !scale.is_finite() {
        return default_ui_font_scale();
    }
    let scaled = (scale * f32::from(UI_FONT_SCALE_ONE_Q8)).round();
    clamp_ui_font_scale(scaled.clamp(0.0, f32::from(u16::MAX)) as u16)
}

/// Default authored UI letter spacing, in PSX framebuffer pixels.
pub const fn default_ui_letter_spacing() -> i8 {
    0
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum UiFontScaleWire {
    Int(u16),
    Float(f32),
}

pub(crate) fn deserialize_ui_font_scale<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let scale = match UiFontScaleWire::deserialize(deserializer)? {
        UiFontScaleWire::Float(value) => ui_font_scale_f32_to_q8(value),
        UiFontScaleWire::Int(value) if value <= 4 => ui_font_scale_f32_to_q8(f32::from(value)),
        UiFontScaleWire::Int(value) => clamp_ui_font_scale(value),
    };
    Ok(scale)
}

pub(crate) fn serialize_ui_font_scale<S>(scale_q8: &u16, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_f32(ui_font_scale_q8_to_f32(*scale_q8))
}

/// Animated vertex-colour effect for a screen-space image node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiImageEffect {
    /// Static image tint.
    #[default]
    None,
    /// Broad highlight moving left-to-right.
    Shimmer,
    /// Faster, stronger left-to-right shimmer.
    FastShimmer,
    /// Highlight moving diagonally across the image.
    DiagonalSweep,
    /// Whole-image brightness pulse.
    SoftPulse,
    /// Gentle vertical bob (loading-screen mascot idiom).
    Bob,
}

impl UiImageEffect {
    /// Stable list used by editor controls.
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Shimmer,
        Self::FastShimmer,
        Self::DiagonalSweep,
        Self::SoftPulse,
        Self::Bob,
    ];

    /// Compact display label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Shimmer => "Shimmer",
            Self::FastShimmer => "Fast Shimmer",
            Self::DiagonalSweep => "Diagonal Sweep",
            Self::SoftPulse => "Soft Pulse",
            Self::Bob => "Bob",
        }
    }
}

/// Focus-ring animation selector. Mirrors
/// `psx_level::LevelUiFocusEffect`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiFocusEffect {
    /// Static outline (the classic ring).
    #[default]
    Solid,
    /// Outline colour breathes between the two style colours.
    Pulse,
    /// A bright head with a gradient tail orbits the outline.
    Tracer,
    /// Four corner brackets that breathe and pulse.
    Corners,
}

impl UiFocusEffect {
    /// Stable list used by editor controls.
    pub const ALL: [Self; 4] = [Self::Solid, Self::Pulse, Self::Tracer, Self::Corners];

    /// Compact display label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Pulse => "Pulse",
            Self::Tracer => "Tracer",
            Self::Corners => "Corners",
        }
    }
}

/// Authored focus-ring style for one UI scene. Mirrors
/// `psx_level::LevelUiFocusStyle`; the cook copies it through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiFocusStyle {
    /// Which animation the focused control's ring runs.
    #[serde(default)]
    pub effect: UiFocusEffect,
    /// Primary colour (solid ring / bright end / tracer head).
    pub color_a: [u8; 3],
    /// Secondary colour (dim end / tracer tail and base ring).
    pub color_b: [u8; 3],
    /// Full animation cycle in vblank-paced frames. 0 freezes at the
    /// brightest phase.
    pub period: u16,
    /// Line thickness in pixels (1..=4).
    pub thickness: u8,
    /// Gap between the control rect and the ring, in pixels.
    pub margin: u8,
    /// Corner bracket arm length in pixels (Corners only).
    pub corner_len: u8,
}

impl Default for UiFocusStyle {
    fn default() -> Self {
        Self {
            effect: UiFocusEffect::Solid,
            color_a: [248, 224, 96],
            color_b: [96, 88, 40],
            period: 96,
            thickness: 1,
            margin: 1,
            corner_len: 8,
        }
    }
}

/// Authored 2D UI node type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiNodeKind {
    /// Root fixed-resolution PSX canvas.
    Canvas {
        /// Canvas width in pixels.
        width: u16,
        /// Canvas height in pixels.
        height: u16,
    },
    /// Organizational transform group.
    Group {
        /// Group bounds in canvas pixels.
        rect: UiRect,
    },
    /// Solid screen-space rectangle.
    Rect {
        /// Rectangle bounds in canvas pixels.
        rect: UiRect,
        /// Fill colour.
        color: [u8; 3],
        /// Optional fill gradient ending colour/direction.
        #[serde(default)]
        gradient: Option<UiGradient>,
    },
    /// Text label drawn with the runtime font atlas.
    Label {
        /// Label bounds in canvas pixels.
        rect: UiRect,
        /// Authored text.
        text: String,
        /// Optional runtime lookup tag for game-controlled text.
        #[serde(default)]
        tag: String,
        /// Text alignment inside `rect`.
        #[serde(default)]
        align: UiTextAlign,
        /// Wrap words inside the label rectangle.
        #[serde(default)]
        wrap: bool,
        /// Runtime bitmap font.
        #[serde(default)]
        font: UiFontChoice,
        /// Q8 text scale written into the runtime UI node.
        #[serde(
            default = "default_ui_font_scale",
            deserialize_with = "deserialize_ui_font_scale",
            serialize_with = "serialize_ui_font_scale"
        )]
        font_scale: u16,
        /// Extra signed screen pixels inserted between adjacent glyphs.
        #[serde(default = "default_ui_letter_spacing")]
        letter_spacing: i8,
        /// Text tint.
        color: [u8; 3],
        /// Optional text gradient ending colour/direction.
        #[serde(default)]
        gradient: Option<UiGradient>,
    },
    /// Screen-space textured image.
    Image {
        /// Image bounds in canvas pixels.
        rect: UiRect,
        /// Optional Texture resource.
        #[serde(default)]
        texture: Option<ResourceId>,
        /// Texture tint.
        #[serde(default = "default_ui_image_tint")]
        tint: [u8; 3],
        /// Optional animated vertex-colour preset.
        #[serde(default)]
        effect: UiImageEffect,
    },
    /// Horizontal status bar backed by a runtime value binding.
    Bar {
        /// Bar bounds in canvas pixels.
        rect: UiRect,
        /// Current value binding.
        value: UiValueBinding,
        /// Maximum value binding.
        max: UiValueBinding,
        /// Filled portion colour.
        fill: [u8; 3],
        /// Optional fill gradient ending colour/direction.
        #[serde(default)]
        fill_gradient: Option<UiGradient>,
        /// Empty/background colour.
        background: [u8; 3],
        /// Optional background gradient ending colour/direction.
        #[serde(default)]
        background_gradient: Option<UiGradient>,
    },
    /// Interactive button: a filled rectangle with a centered label
    /// that fires `action` when activated. Runtime activation is a
    /// later step.
    Button {
        /// Button bounds in canvas pixels.
        rect: UiRect,
        /// Centered label text.
        #[serde(default)]
        label: String,
        /// Label alignment inside `rect`.
        #[serde(default)]
        align: UiTextAlign,
        /// Runtime bitmap font.
        #[serde(default)]
        font: UiFontChoice,
        /// Q8 text scale written into the runtime UI node.
        #[serde(
            default = "default_ui_font_scale",
            deserialize_with = "deserialize_ui_font_scale",
            serialize_with = "serialize_ui_font_scale"
        )]
        font_scale: u16,
        /// Extra signed screen pixels inserted between adjacent glyphs.
        #[serde(default = "default_ui_letter_spacing")]
        letter_spacing: i8,
        /// Background fill colour (ignored when `transparent`).
        #[serde(default = "default_ui_button_color")]
        color: [u8; 3],
        /// Optional background gradient ending colour/direction.
        #[serde(default)]
        background_gradient: Option<UiGradient>,
        /// Label text colour.
        #[serde(default = "default_ui_button_text_color")]
        text_color: [u8; 3],
        /// Optional label gradient ending colour/direction.
        #[serde(default)]
        text_gradient: Option<UiGradient>,
        /// Transparent background: skip the fill and draw only the label.
        #[serde(default)]
        transparent: bool,
        /// Action fired on activation.
        #[serde(default)]
        action: UiAction,
        /// Per-event SFX cue pools.
        #[serde(default)]
        sfx: UiSfxBindings,
    },
    /// Interactive slider bound to a project option, drawn as a track,
    /// a proportional fill, and a knob. Runtime value editing is a
    /// later step.
    Slider {
        /// Slider bounds in canvas pixels.
        rect: UiRect,
        /// Bound project option id.
        #[serde(default)]
        option: OptionId,
        /// Track (background) colour.
        #[serde(default = "default_ui_slider_track")]
        track: [u8; 3],
        /// Optional track gradient ending colour/direction.
        #[serde(default)]
        track_gradient: Option<UiGradient>,
        /// Fill colour up to the current value.
        #[serde(default = "default_ui_slider_fill")]
        fill: [u8; 3],
        /// Optional fill gradient ending colour/direction.
        #[serde(default)]
        fill_gradient: Option<UiGradient>,
        /// Knob colour.
        #[serde(default = "default_ui_slider_knob")]
        knob: [u8; 3],
        /// Optional knob gradient ending colour/direction.
        #[serde(default)]
        knob_gradient: Option<UiGradient>,
        /// Per-event SFX cue pools.
        #[serde(default)]
        sfx: UiSfxBindings,
    },
    /// Non-visual CD-DA music cue for the containing UI scene.
    Music {
        /// Project-relative source WAV. The playtest/export disc builder
        /// converts it to a sector-aligned CD-DA track automatically.
        #[serde(default)]
        wav_path: String,
        /// CD-DA playback volume as a percentage of the hardware maximum.
        #[serde(default = "default_ui_music_volume")]
        volume: u8,
        /// Optional project option whose live value overrides [`Self::Music`]'s
        /// fixed `volume`, so a menu slider can change CD-DA loudness while the
        /// scene is open.
        #[serde(default)]
        volume_option: Option<OptionId>,
        /// Baked playback-speed multiplier in Q12 (`4096` = 1.0x). This is a
        /// preprocessing knob: the generated CD-DA track is resampled, while
        /// runtime CD playback remains standard speed.
        #[serde(default = "default_ui_music_playback_speed_q12")]
        playback_speed_q12: u16,
        /// Restart the track when the CD-ROM reports playback has ended.
        #[serde(default)]
        loop_track: bool,
    },
}

impl UiNodeKind {
    /// Editor-facing node kind label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Canvas { .. } => "Canvas",
            Self::Group { .. } => "Group",
            Self::Rect { .. } => "Rect",
            Self::Label { .. } => "Label",
            Self::Image { .. } => "Image",
            Self::Bar { .. } => "Bar",
            Self::Button { .. } => "Button",
            Self::Slider { .. } => "Slider",
            Self::Music { .. } => "Music",
        }
    }

    /// Mutable bounds for node kinds that occupy a screen-space rectangle.
    pub fn rect_mut(&mut self) -> Option<&mut UiRect> {
        match self {
            Self::Canvas { .. } => None,
            Self::Group { rect }
            | Self::Rect { rect, .. }
            | Self::Label { rect, .. }
            | Self::Image { rect, .. }
            | Self::Bar { rect, .. }
            | Self::Button { rect, .. }
            | Self::Slider { rect, .. } => Some(rect),
            Self::Music { .. } => None,
        }
    }

    /// Bounds for node kinds that occupy a screen-space rectangle.
    pub fn rect(&self) -> Option<UiRect> {
        match self {
            Self::Canvas { .. } => None,
            Self::Group { rect }
            | Self::Rect { rect, .. }
            | Self::Label { rect, .. }
            | Self::Image { rect, .. }
            | Self::Bar { rect, .. }
            | Self::Button { rect, .. }
            | Self::Slider { rect, .. } => Some(*rect),
            Self::Music { .. } => None,
        }
    }
}

pub(crate) fn default_ui_image_tint() -> [u8; 3] {
    [128, 128, 128]
}

pub(crate) fn default_ui_button_color() -> [u8; 3] {
    [52, 60, 80]
}

pub(crate) fn default_ui_button_text_color() -> [u8; 3] {
    [236, 240, 248]
}

pub(crate) fn default_ui_slider_track() -> [u8; 3] {
    [30, 34, 44]
}

pub(crate) fn default_ui_slider_fill() -> [u8; 3] {
    [80, 132, 180]
}

pub(crate) fn default_ui_slider_knob() -> [u8; 3] {
    [210, 218, 232]
}

pub(crate) fn default_ui_music_volume() -> u8 {
    25
}

/// One node in an authored UI scene tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiNode {
    /// Stable node id.
    pub id: UiNodeId,
    /// Parent id. `None` only for the scene root.
    pub parent: Option<UiNodeId>,
    /// Child ids in display/draw order.
    pub children: Vec<UiNodeId>,
    /// Display name.
    pub name: String,
    /// Node payload.
    pub kind: UiNodeKind,
    /// Runtime visibility condition.
    #[serde(default)]
    pub visible_when: UiVisibilityCondition,
}

impl UiNode {
    /// Build a UI node.
    pub fn new(
        id: UiNodeId,
        parent: Option<UiNodeId>,
        name: impl Into<String>,
        kind: UiNodeKind,
    ) -> Self {
        Self {
            id,
            parent,
            children: Vec::new(),
            name: name.into(),
            kind,
            visible_when: UiVisibilityCondition::Always,
        }
    }
}

/// One visible row in the UI scene hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiNodeRow {
    /// Node id.
    pub id: UiNodeId,
    /// Parent node id. `None` only for the root canvas.
    pub parent: Option<UiNodeId>,
    /// Index inside the parent's child list.
    pub sibling_index: usize,
    /// Nesting depth.
    pub depth: usize,
    /// Display name.
    pub name: String,
    /// Node kind label.
    pub kind: &'static str,
    /// Optional authored lookup tag, currently used by labels.
    pub tag: Option<String>,
    /// Number of direct children.
    pub child_count: usize,
}

/// One authored 2D UI scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiScene {
    /// Stable scene id. Defaults to [`UiSceneId::UNASSIGNED`] for
    /// legacy projects and is filled in by
    /// [`ProjectDocument::normalize_loaded`].
    #[serde(default)]
    pub id: UiSceneId,
    /// Display name.
    pub name: String,
    /// Root node id.
    pub root: UiNodeId,
    /// Optional node that receives focus when this scene becomes the
    /// active game state. `None` falls back to the root canvas.
    #[serde(default)]
    pub default_focus: Option<UiNodeId>,
    /// Focus-ring style for this scene's focused control.
    #[serde(default)]
    pub focus_style: UiFocusStyle,
    next_node_id: u64,
    nodes: Vec<UiNode>,
}

impl UiScene {
    /// Create the default HUD scene authored at PSX 320x240 resolution.
    pub fn default_hud() -> Self {
        let mut root = UiNode::new(
            UiNodeId::ROOT,
            None,
            "HUD",
            UiNodeKind::Canvas {
                width: 320,
                height: 240,
            },
        );
        let health = UiNode::new(
            UiNodeId(2),
            Some(UiNodeId::ROOT),
            "Health Bar",
            UiNodeKind::Bar {
                rect: UiRect::new(18, 16, 120, 8),
                value: UiValueBinding::PlayerHealth,
                max: UiValueBinding::PlayerHealthMax,
                fill: [94, 16, 24],
                fill_gradient: None,
                background: [30, 26, 28],
                background_gradient: None,
            },
        );
        let stamina = UiNode::new(
            UiNodeId(3),
            Some(UiNodeId::ROOT),
            "Stamina Bar",
            UiNodeKind::Bar {
                rect: UiRect::new(18, 29, 96, 5),
                value: UiValueBinding::PlayerStamina,
                max: UiValueBinding::PlayerStaminaMax,
                fill: [44, 98, 48],
                fill_gradient: None,
                background: [30, 26, 28],
                background_gradient: None,
            },
        );
        root.children = vec![health.id, stamina.id];
        Self {
            id: UiSceneId::FIRST,
            name: "HUD".to_string(),
            root: UiNodeId::ROOT,
            default_focus: None,
            focus_style: UiFocusStyle::default(),
            next_node_id: 4,
            nodes: vec![root, health, stamina],
        }
    }

    /// Build a fresh scene with `name`, `id`, and a single empty root
    /// Canvas at the PSX 320x240 authoring resolution. Used by the
    /// editor's "new scene" path; pass [`UiSceneId::UNASSIGNED`] to let
    /// [`ProjectDocument::assign_ui_scene_ids`] hand out the stable id.
    pub fn empty_canvas(name: impl Into<String>, id: UiSceneId) -> Self {
        let root = UiNode::new(
            UiNodeId::ROOT,
            None,
            "Canvas",
            UiNodeKind::Canvas {
                width: 320,
                height: 240,
            },
        );
        Self {
            id,
            name: name.into(),
            root: UiNodeId::ROOT,
            default_focus: None,
            focus_style: UiFocusStyle::default(),
            next_node_id: UiNodeId::ROOT.raw().saturating_add(1),
            nodes: vec![root],
        }
    }

    /// All nodes in storage order.
    pub fn nodes(&self) -> &[UiNode] {
        &self.nodes
    }

    /// Get a node.
    pub fn node(&self, id: UiNodeId) -> Option<&UiNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Get a mutable node.
    pub fn node_mut(&mut self, id: UiNodeId) -> Option<&mut UiNode> {
        self.nodes.iter_mut().find(|node| node.id == id)
    }

    /// Add a node under `parent`. Invalid parents fall back to the root.
    pub fn add_node(
        &mut self,
        parent: UiNodeId,
        name: impl Into<String>,
        kind: UiNodeKind,
    ) -> UiNodeId {
        let parent = if self.node(parent).is_some() {
            parent
        } else {
            self.root
        };
        let id = UiNodeId(self.next_node_id);
        self.next_node_id = self.next_node_id.saturating_add(1);
        self.nodes.push(UiNode::new(id, Some(parent), name, kind));
        if let Some(parent_node) = self.node_mut(parent) {
            parent_node.children.push(id);
        }
        id
    }

    /// Remove a non-root node and all of its descendants.
    pub fn remove_node(&mut self, id: UiNodeId) -> bool {
        if id == self.root {
            return false;
        }
        if self.node(id).is_none() {
            return false;
        }

        let mut remove_ids = HashSet::new();
        let mut stack = vec![id];
        while let Some(next) = stack.pop() {
            if !remove_ids.insert(next) {
                continue;
            }
            if let Some(node) = self.node(next) {
                stack.extend(node.children.iter().copied());
            }
        }

        self.nodes.retain(|node| !remove_ids.contains(&node.id));
        for node in &mut self.nodes {
            node.children.retain(|child| !remove_ids.contains(child));
        }
        true
    }

    /// `true` when `ancestor` appears anywhere on the parent chain of
    /// `id`. Includes `id` itself, matching [`Scene::is_descendant_of`].
    pub fn is_descendant_of(&self, id: UiNodeId, ancestor: UiNodeId) -> bool {
        if id == ancestor {
            return true;
        }
        let mut current = self.node(id).and_then(|node| node.parent);
        let mut guard = 0usize;
        while let Some(parent) = current {
            if guard >= self.nodes.len() {
                break;
            }
            if parent == ancestor {
                return true;
            }
            current = self.node(parent).and_then(|node| node.parent);
            guard += 1;
        }
        false
    }

    /// Move `id` under `new_parent` at `position` in the child list.
    ///
    /// The canvas root cannot be moved and cycles are rejected. This is
    /// deliberately parallel to [`Scene::move_node`] so 2D and 3D tree
    /// authoring share the same parent/child rules.
    pub fn move_node(&mut self, id: UiNodeId, new_parent: UiNodeId, position: usize) -> bool {
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
            node.children.retain(|child| *child != id);
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

    /// Return a deep copy of `root` and all of its descendants in
    /// hierarchy order. The copied nodes keep their source ids so a
    /// destination scene can remap parent/child links consistently.
    pub fn subtree_nodes(&self, root: UiNodeId) -> Option<Vec<UiNode>> {
        self.node(root)?;
        let mut nodes = Vec::new();
        let mut visited = HashSet::new();
        self.push_subtree_node(root, &mut nodes, &mut visited);
        Some(nodes)
    }

    fn push_subtree_node(
        &self,
        id: UiNodeId,
        nodes: &mut Vec<UiNode>,
        visited: &mut HashSet<UiNodeId>,
    ) {
        if !visited.insert(id) {
            return;
        }
        if let Some(node) = self.node(id) {
            nodes.push(node.clone());
            for &child in &node.children {
                self.push_subtree_node(child, nodes, visited);
            }
        }
    }

    /// Paste a copied UI subtree under `parent`, remapping every node id
    /// to fresh ids in this scene while preserving the copied hierarchy.
    /// Returns the new root node id.
    pub fn paste_subtree(
        &mut self,
        parent: UiNodeId,
        subtree: &[UiNode],
        subtree_root: UiNodeId,
    ) -> Option<UiNodeId> {
        if subtree.is_empty() || !subtree.iter().any(|node| node.id == subtree_root) {
            return None;
        }
        let parent = if self.node(parent).is_some() {
            parent
        } else {
            self.root
        };

        let mut remap = BTreeMap::new();
        for source in subtree {
            let id = UiNodeId(self.next_node_id);
            self.next_node_id = self.next_node_id.saturating_add(1);
            remap.insert(source.id, id);
        }
        let new_root = *remap.get(&subtree_root)?;

        let mut pasted = Vec::with_capacity(subtree.len());
        for source in subtree {
            let id = *remap.get(&source.id)?;
            let parent_id = if source.id == subtree_root {
                Some(parent)
            } else {
                source
                    .parent
                    .and_then(|source_parent| remap.get(&source_parent).copied())
                    .or(Some(new_root))
            };
            let mut node = source.clone();
            node.id = id;
            node.parent = parent_id;
            node.children = source
                .children
                .iter()
                .filter_map(|child| remap.get(child).copied())
                .collect();
            pasted.push(node);
        }

        self.nodes.extend(pasted);
        if let Some(parent_node) = self.node_mut(parent) {
            parent_node.children.push(new_root);
        }
        Some(new_root)
    }

    /// Node ids in tree draw order.
    pub fn hierarchy_node_ids(&self) -> Vec<UiNodeId> {
        let mut ids = Vec::new();
        let mut visited = HashSet::new();
        self.push_hierarchy_node_id(self.root, &mut ids, &mut visited);
        for node in &self.nodes {
            self.push_hierarchy_node_id(node.id, &mut ids, &mut visited);
        }
        ids
    }

    fn push_hierarchy_node_id(
        &self,
        id: UiNodeId,
        ids: &mut Vec<UiNodeId>,
        visited: &mut HashSet<UiNodeId>,
    ) {
        if !visited.insert(id) {
            return;
        }
        if let Some(node) = self.node(id) {
            ids.push(id);
            for &child in &node.children {
                self.push_hierarchy_node_id(child, ids, visited);
            }
        }
    }

    /// Bounds resolved into canvas space. Child `x/y` values are
    /// local offsets from their selected anchor on the parent rect.
    pub fn absolute_rect(&self, id: UiNodeId) -> Option<UiRect> {
        self.absolute_rect_inner(id, 0)
    }

    fn absolute_rect_inner(&self, id: UiNodeId, depth: usize) -> Option<UiRect> {
        if depth > self.nodes.len() {
            return None;
        }
        let node = self.node(id)?;
        match &node.kind {
            UiNodeKind::Canvas { width, height } => Some(UiRect::new(0, 0, *width, *height)),
            _ => {
                let local = node.kind.rect()?;
                let parent = node
                    .parent
                    .and_then(|parent_id| self.absolute_rect_inner(parent_id, depth + 1))
                    .unwrap_or_else(|| UiRect::new(0, 0, 0, 0));
                let (anchor_x, anchor_y) = local.anchor.factors();
                let x = parent.x as i32 + (parent.width as i32 * anchor_x) / 2 + local.x as i32;
                let y = parent.y as i32 + (parent.height as i32 * anchor_y) / 2 + local.y as i32;
                let mut rect = local;
                rect.x = clamp_ui_rect_coord(x);
                rect.y = clamp_ui_rect_coord(y);
                Some(rect)
            }
        }
    }

    /// Normalize loaded UI data.
    pub fn normalize(&mut self) {
        if self.node(self.root).is_none() {
            self.root = UiNodeId::ROOT;
            self.nodes.insert(
                0,
                UiNode::new(
                    self.root,
                    None,
                    "HUD",
                    UiNodeKind::Canvas {
                        width: 320,
                        height: 240,
                    },
                ),
            );
        }
        if let Some(root) = self.node_mut(self.root) {
            root.parent = None;
            if root.name.trim().is_empty() {
                root.name = "HUD".to_string();
            }
            if !matches!(root.kind, UiNodeKind::Canvas { .. }) {
                root.kind = UiNodeKind::Canvas {
                    width: 320,
                    height: 240,
                };
            }
        }
        let mut max_id = self.root.raw();
        let valid_ids: HashSet<UiNodeId> = self.nodes.iter().map(|node| node.id).collect();
        for node in &mut self.nodes {
            max_id = max_id.max(node.id.raw());
            if let UiNodeKind::Music { volume, .. } = &mut node.kind {
                *volume = (*volume).min(100);
            }
            match &mut node.kind {
                UiNodeKind::Button { sfx, .. } | UiNodeKind::Slider { sfx, .. } => {
                    normalize_ui_sfx_bindings(sfx);
                }
                _ => {}
            }
            node.children
                .retain(|child| *child != node.id && valid_ids.contains(child));
            if node.id != self.root
                && !node
                    .parent
                    .is_some_and(|id| id != node.id && valid_ids.contains(&id))
            {
                node.parent = Some(self.root);
            }
        }
        let parent_by_id = self
            .nodes
            .iter()
            .map(|node| (node.id, node.parent))
            .collect::<BTreeMap<_, _>>();
        for node in &mut self.nodes {
            if node.id == self.root {
                continue;
            }
            let mut seen = HashSet::new();
            let mut current = node.parent;
            let mut valid_parent_chain = true;
            while let Some(parent) = current {
                if parent == node.id || !seen.insert(parent) {
                    valid_parent_chain = false;
                    break;
                }
                current = parent_by_id.get(&parent).copied().flatten();
            }
            if !valid_parent_chain {
                node.parent = Some(self.root);
            }
        }
        self.rebuild_child_links();
        self.next_node_id = self.next_node_id.max(max_id.saturating_add(1));
        if self
            .default_focus
            .is_some_and(|focus| self.node(focus).is_none())
        {
            self.default_focus = None;
        }
    }

    fn rebuild_child_links(&mut self) {
        let parent_by_id = self
            .nodes
            .iter()
            .map(|node| (node.id, node.parent))
            .collect::<BTreeMap<_, _>>();
        let mut children_by_parent = self
            .nodes
            .iter()
            .map(|node| (node.id, Vec::new()))
            .collect::<BTreeMap<_, Vec<UiNodeId>>>();
        let mut assigned = HashSet::new();

        for node in &self.nodes {
            for &child in &node.children {
                if child == self.root || assigned.contains(&child) {
                    continue;
                }
                if parent_by_id.get(&child).copied().flatten() == Some(node.id) {
                    if let Some(children) = children_by_parent.get_mut(&node.id) {
                        children.push(child);
                        assigned.insert(child);
                    }
                }
            }
        }

        for node in &self.nodes {
            if node.id == self.root || assigned.contains(&node.id) {
                continue;
            }
            let parent = node.parent.unwrap_or(self.root);
            if let Some(children) = children_by_parent.get_mut(&parent) {
                children.push(node.id);
                assigned.insert(node.id);
            }
        }

        for node in &mut self.nodes {
            node.children = children_by_parent.remove(&node.id).unwrap_or_default();
        }
    }

    /// Flatten the hierarchy into display rows.
    pub fn hierarchy_rows(&self) -> Vec<UiNodeRow> {
        let mut rows = Vec::new();
        self.push_hierarchy_row(self.root, 0, None, 0, &mut rows);
        rows
    }

    fn push_hierarchy_row(
        &self,
        id: UiNodeId,
        depth: usize,
        parent: Option<UiNodeId>,
        sibling_index: usize,
        rows: &mut Vec<UiNodeRow>,
    ) {
        let Some(node) = self.node(id) else {
            return;
        };
        rows.push(UiNodeRow {
            id,
            parent,
            sibling_index,
            depth,
            name: node.name.clone(),
            kind: node.kind.label(),
            tag: match &node.kind {
                UiNodeKind::Label { tag, .. } if !tag.trim().is_empty() => Some(tag.clone()),
                _ => None,
            },
            child_count: node.children.len(),
        });
        for (index, &child) in node.children.iter().enumerate() {
            self.push_hierarchy_row(child, depth.saturating_add(1), Some(id), index, rows);
        }
    }
}

pub(crate) fn clamp_ui_rect_coord(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

pub(crate) fn default_ui_scenes() -> Vec<UiScene> {
    vec![UiScene::default_hud()]
}
