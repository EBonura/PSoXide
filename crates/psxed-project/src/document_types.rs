use super::*;

/// Saved editor 3D camera mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EditorCameraMode {
    /// Target/radius orbit camera.
    #[default]
    Orbit,
    /// Explicit-position fly camera.
    Free,
}

pub(crate) fn default_editor_camera_orbit_yaw_q12() -> u16 {
    256
}

pub(crate) fn default_editor_camera_orbit_pitch_q12() -> u16 {
    256
}

pub(crate) fn default_editor_camera_orbit_radius() -> i32 {
    6144
}

pub(crate) fn default_editor_camera_orbit_target() -> [i32; 3] {
    [0, 512, 0]
}

pub(crate) fn default_editor_camera_free_yaw_q12() -> u16 {
    default_editor_camera_orbit_yaw_q12()
}

pub(crate) fn default_editor_camera_free_pitch_q12() -> u16 {
    default_editor_camera_orbit_pitch_q12()
}

/// Editor-only 3D viewport camera state persisted with a project.
///
/// This is intentionally authoring metadata: cook/playtest paths
/// should not use it for runtime camera behavior.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EditorCameraState {
    #[serde(default)]
    pub mode: EditorCameraMode,
    #[serde(default = "default_editor_camera_orbit_yaw_q12")]
    pub orbit_yaw_q12: u16,
    #[serde(default = "default_editor_camera_orbit_pitch_q12")]
    pub orbit_pitch_q12: u16,
    #[serde(default = "default_editor_camera_orbit_radius")]
    pub orbit_radius: i32,
    #[serde(default = "default_editor_camera_orbit_target")]
    pub orbit_target: [i32; 3],
    #[serde(default = "default_editor_camera_free_yaw_q12")]
    pub free_yaw_q12: u16,
    #[serde(default = "default_editor_camera_free_pitch_q12")]
    pub free_pitch_q12: u16,
    #[serde(default)]
    pub free_position: [i32; 3],
    #[serde(default)]
    pub free_initialized: bool,
    /// Scroll-wheel dolly speed multiplier for the 3D viewport.
    #[serde(default = "default_editor_camera_zoom_speed")]
    pub zoom_speed: f32,
}

pub(crate) fn default_editor_camera_zoom_speed() -> f32 {
    1.0
}

impl Default for EditorCameraState {
    fn default() -> Self {
        Self {
            mode: EditorCameraMode::Orbit,
            orbit_yaw_q12: default_editor_camera_orbit_yaw_q12(),
            orbit_pitch_q12: default_editor_camera_orbit_pitch_q12(),
            orbit_radius: default_editor_camera_orbit_radius(),
            orbit_target: default_editor_camera_orbit_target(),
            free_yaw_q12: default_editor_camera_free_yaw_q12(),
            free_pitch_q12: default_editor_camera_free_pitch_q12(),
            free_position: [0, 0, 0],
            free_initialized: false,
            zoom_speed: default_editor_camera_zoom_speed(),
        }
    }
}

impl EditorCameraState {
    pub fn normalize(&mut self) {
        self.orbit_pitch_q12 = clamp_q12_pitch(self.orbit_pitch_q12);
        self.free_pitch_q12 = clamp_q12_pitch(self.free_pitch_q12);
        self.orbit_radius = self.orbit_radius.clamp(512, 262_144);
        self.zoom_speed = if self.zoom_speed.is_finite() {
            self.zoom_speed.clamp(0.2, 3.0)
        } else {
            default_editor_camera_zoom_speed()
        };
    }
}

pub(crate) fn clamp_q12_pitch(value: u16) -> u16 {
    let raw = (value & 0x0fff) as i32;
    let signed = if raw >= 2048 { raw - 4096 } else { raw };
    signed.clamp(-960, 960).rem_euclid(4096) as u16
}

/// Editor-only visibility preferences persisted with a project.
///
/// These fields affect authoring and debug overlays only; cooked
/// runtime output must not depend on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorVisibilityState {
    #[serde(default = "default_true")]
    pub show_grid: bool,
    /// Project the active world grid over visible BSP brush faces while
    /// leaving the ordinary background grid independently controllable.
    #[serde(default = "default_true")]
    pub show_brush_surface_grid: bool,
    #[serde(default = "default_true")]
    pub show_lights: bool,
    #[serde(default = "default_true")]
    pub preview_bounds: bool,
    #[serde(default = "default_true")]
    pub show_play_debug_overlays: bool,
    /// Wireframe outlines for UNSELECTED brushes. Off by default: selected
    /// brushes always outline, the full cage is an opt-in alignment view.
    #[serde(default)]
    pub show_brush_wireframes: bool,
}

impl Default for EditorVisibilityState {
    fn default() -> Self {
        Self {
            show_grid: true,
            show_brush_surface_grid: true,
            show_lights: true,
            preview_bounds: true,
            show_play_debug_overlays: true,
            show_brush_wireframes: false,
        }
    }
}

/// Last editor workspace shown for this project.
///
/// This is editor-only UI state; cooked runtime output must not depend on it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorWorkspaceView {
    /// Room/level workspace.
    #[default]
    Room,
    /// 2D UI workspace.
    Ui,
    /// Model/animation preview workspace.
    Animation,
    /// Reusable material authoring workspace.
    Material,
}

/// Editor-only workspace preferences persisted with a project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorWorkspaceState {
    /// Last active top-level editor workspace.
    #[serde(default)]
    pub active: EditorWorkspaceView,
}

/// Which orthographic projection the 2D viewport shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorOrthographicView {
    /// XZ plane, looking down -Y.
    #[default]
    Top,
    /// XY plane, looking down +Z.
    Front,
    /// ZY plane, looking down -X.
    Side,
}

/// Editor-only viewport layout persisted with a project: whether the
/// 2D view is active, which projection it shows, where it looks, how
/// zoomed it is, and the shared grid snap step. Authoring metadata
/// only; cooked runtime output must not depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EditorViewportState {
    /// True when the central viewport shows the orthographic view.
    #[serde(default)]
    pub view_2d: bool,
    /// Active orthographic projection.
    #[serde(default)]
    pub orthographic_view: EditorOrthographicView,
    /// Shared world-space focus of the orthographic views.
    #[serde(default)]
    pub orthographic_focus: [f32; 3],
    /// Orthographic zoom, pixels per world sector-unit. The editor
    /// clamps this into its interactive zoom range on load.
    #[serde(default = "default_editor_viewport_zoom")]
    pub viewport_zoom: f32,
    /// Grid snap step in world units, shared by every drag/create op.
    #[serde(default = "default_editor_snap_units")]
    pub snap_units: u16,
}

pub(crate) fn default_editor_viewport_zoom() -> f32 {
    96.0
}

pub(crate) fn default_editor_snap_units() -> u16 {
    16
}

impl Default for EditorViewportState {
    fn default() -> Self {
        Self {
            view_2d: false,
            orthographic_view: EditorOrthographicView::default(),
            orthographic_focus: [0.0; 3],
            viewport_zoom: default_editor_viewport_zoom(),
            snap_units: default_editor_snap_units(),
        }
    }
}

/// Runtime depth sorting policy for cooked cached room geometry.
///
/// This affects embedded play and generated runtime manifests. The editor
/// preview remains the reference view, but the PS1 path needs explicit
/// tradeoffs between stable ordering and per-triangle work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuntimeDepthSortMode {
    /// Use the legacy fixed cell depth key for every cached surface.
    FixedCell,
    /// Use per-triangle depth for sloped/high-span horizontal surfaces.
    Hybrid,
    /// Like hybrid, but also sorts high-depth-span walls per triangle.
    #[default]
    HybridWalls,
    /// Use per-triangle projected depth for every cached surface.
    PerTriangle,
}

impl RuntimeDepthSortMode {
    pub const ALL: [Self; 4] = [
        Self::Hybrid,
        Self::HybridWalls,
        Self::PerTriangle,
        Self::FixedCell,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::FixedCell => "Fixed cell",
            Self::Hybrid => "Hybrid",
            Self::HybridWalls => "Hybrid + walls",
            Self::PerTriangle => "Per triangle",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::FixedCell => "Fast legacy ordering. Can show overlap errors on ramps.",
            Self::Hybrid => "Uses per-triangle depth only where sloped floors need it.",
            Self::HybridWalls => {
                "Also sorts high-depth-span walls per triangle for ramp/wall conflicts."
            }
            Self::PerTriangle => "Most precise cached-room ordering. Costs more sort work.",
        }
    }

    pub const fn manifest_value(self) -> u8 {
        match self {
            Self::FixedCell => 0,
            Self::Hybrid => 1,
            Self::HybridWalls => 2,
            Self::PerTriangle => 3,
        }
    }
}

/// Default projected edge threshold for runtime room subdivision.
///
/// `0` keeps the fixed adaptive depth-band schedule without additional
/// projected-edge refinement. Lower positive values split more aggressively.
pub const DEFAULT_RUNTIME_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;

pub(crate) const fn default_runtime_texture_split_max_edge() -> u16 {
    DEFAULT_RUNTIME_TEXTURE_SPLIT_MAX_EDGE
}

/// Scope for runtime room triangle subdivision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuntimeTextureSplitMode {
    /// Apply depth-band subdivision and optional edge refinement everywhere.
    #[default]
    All,
    /// Apply the edge threshold only to surfaces using per-triangle depth.
    DepthSorted,
    /// Apply the edge threshold only to sloped/high-depth-span surfaces.
    Risky,
}

impl RuntimeTextureSplitMode {
    pub const ALL: [Self; 3] = [Self::All, Self::DepthSorted, Self::Risky];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All surfaces",
            Self::DepthSorted => "Depth sorted",
            Self::Risky => "Risky only",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::All => "adaptive depth-band subdivision applies to every cached room surface.",
            Self::DepthSorted => {
                "Only surfaces using per-triangle depth receive depth-band subdivision."
            }
            Self::Risky => {
                "Only sloped or high-depth-span surfaces receive depth-band subdivision."
            }
        }
    }

    pub const fn manifest_value(self) -> u8 {
        match self {
            Self::All => 0,
            Self::DepthSorted => 1,
            Self::Risky => 2,
        }
    }
}

/// Runtime draw ordering for active room chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RuntimeRoomDrawOrderMode {
    /// Sort active visible rooms by their camera-space center depth.
    #[default]
    Distance,
    /// Draw rooms in portal traversal order.
    Portal,
    /// Draw active slots in runtime slot order.
    Slot,
}

impl RuntimeRoomDrawOrderMode {
    pub const ALL: [Self; 3] = [Self::Distance, Self::Portal, Self::Slot];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Distance => "Distance",
            Self::Portal => "Portal order",
            Self::Slot => "Slot order",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Distance => "Current behavior. Sort active rooms by camera-space center depth.",
            Self::Portal => {
                "Draw rooms in portal traversal order, closer to adaptive-style visibility."
            }
            Self::Slot => "Stable runtime slot order for debugging streaming/order interactions.",
        }
    }

    pub const fn manifest_value(self) -> u8 {
        match self {
            Self::Distance => 0,
            Self::Portal => 1,
            Self::Slot => 2,
        }
    }
}

/// One editor project document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDocument {
    /// Display name.
    pub name: String,
    /// Editor-only viewport camera state.
    #[serde(default)]
    pub editor_camera: EditorCameraState,
    /// Editor-only overlay visibility preferences.
    #[serde(default)]
    pub editor_visibility: EditorVisibilityState,
    /// Editor-only workspace preferences.
    #[serde(default)]
    pub editor_workspace: EditorWorkspaceState,
    /// Editor-only 2D/orthographic viewport layout.
    #[serde(default)]
    pub editor_viewport: EditorViewportState,
    /// BSP compiler quality used by Build, Play, and Rebuild. Persisting this
    /// in the project keeps GUI and CLI cooks on one deterministic policy.
    #[serde(default)]
    pub bsp_cook_mode: crate::brush_world::BrushWorldCookMode,
    /// Worst-case joint rotation error, in whole degrees, that the cook may
    /// introduce by resampling animation clips to a lower rate. `0` disables
    /// resampling and cooks every clip at its authored rate.
    ///
    /// One budget covers every clip because it is self-selecting: a fast clip
    /// blows the budget at the first step down and keeps its rate, while a slow
    /// one gives up most of its frames for an error nobody can see. Authoring a
    /// stride per clip would be the same decision made worse, by hand.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub animation_error_budget_degrees: u8,
    /// Leading and trailing frames of a one-shot clip whose motion sits under
    /// this percentage of the clip's own peak are dead time, and the cook drops
    /// them. `0` disables trimming.
    ///
    /// This is not only a RAM saving. The cooked character record plays an
    /// action's full frame range, so a still head is half a second of nothing
    /// between the button and the swing. Looping clips are never trimmed:
    /// their quiet stretch is part of the cycle.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub animation_trim_still_percent: u8,
    /// Cooked playtest cached-room depth sorting mode.
    #[serde(default)]
    pub runtime_depth_sort_mode: RuntimeDepthSortMode,
    /// Runtime room triangle subdivision scope.
    #[serde(default)]
    pub runtime_texture_split_mode: RuntimeTextureSplitMode,
    /// Runtime active-room draw ordering policy.
    #[serde(default)]
    pub runtime_room_draw_order_mode: RuntimeRoomDrawOrderMode,
    /// Optional projected-edge refinement layered over depth-band subdivision.
    #[serde(default = "default_runtime_texture_split_max_edge")]
    pub runtime_texture_split_max_edge: u16,
    /// Open scenes. The first scene is the active scene for now.
    pub scenes: Vec<Scene>,
    /// Authored screen-space UI scenes. The first scene is the HUD for now.
    #[serde(default = "default_ui_scenes")]
    pub ui_scenes: Vec<UiScene>,
    /// Authored runtime screen states. Each state can combine a world layer
    /// with an optional UI scene overlay.
    #[serde(default)]
    pub scene_states: Vec<ProjectSceneState>,
    /// Project-level tunable options sliders and `SetOption` button
    /// actions bind to. Defaults to empty for legacy projects.
    #[serde(default)]
    pub options: Vec<OptionDef>,
    /// Where the cooked game boots: straight into gameplay, or into one of
    /// the authored UI scenes (a title/menu screen). Drives the cooked
    /// `GameFlow.entry`. Defaults to [`BootTarget::Gameplay`] so existing
    /// projects boot exactly as before.
    #[serde(default)]
    pub boot: BootTarget,
    /// Default full-screen transition used by ordinary state changes such as
    /// START, Back, `GotoScene`, and `StartGameplay`. Explicit transition
    /// actions override this value. `None` preserves legacy instant handoffs.
    #[serde(default)]
    pub screen_transition: crate::UiTransition,
    /// Project resources.
    pub resources: Vec<Resource>,
    next_resource_id: u64,
}

/// Where a cooked project starts running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BootTarget {
    /// Boot straight into the gameplay/level simulation (skip any menus).
    #[default]
    Gameplay,
    /// Boot into a composed screen state.
    SceneState(SceneStateId),
    /// Boot into the authored UI scene with this stable id (a title/menu).
    UiScene(UiSceneId),
}

/// Serde predicate: leave a default budget out of the file so projects that do
/// not resample stay byte-identical to what they were before the knob existed.
fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

impl ProjectDocument {
    /// Create an empty project with one scene.
    pub fn new(name: impl Into<String>) -> Self {
        let ui_scenes = default_ui_scenes();
        let scene_states = default_scene_states_for_ui_scenes(&ui_scenes);
        Self {
            name: name.into(),
            editor_camera: EditorCameraState::default(),
            editor_visibility: EditorVisibilityState::default(),
            editor_workspace: EditorWorkspaceState::default(),
            editor_viewport: EditorViewportState::default(),
            animation_error_budget_degrees: 0,
            animation_trim_still_percent: 0,
            bsp_cook_mode: crate::brush_world::BrushWorldCookMode::default(),
            runtime_depth_sort_mode: RuntimeDepthSortMode::default(),
            runtime_texture_split_mode: RuntimeTextureSplitMode::default(),
            runtime_room_draw_order_mode: RuntimeRoomDrawOrderMode::default(),
            runtime_texture_split_max_edge: DEFAULT_RUNTIME_TEXTURE_SPLIT_MAX_EDGE,
            scenes: vec![Scene::new("Main")],
            ui_scenes,
            scene_states,
            options: Vec::new(),
            boot: BootTarget::default(),
            screen_transition: crate::UiTransition::default(),
            resources: Vec::new(),
            next_resource_id: 1,
        }
    }

    /// Deserialize the default project shipped at
    /// `editor/projects/default/project.ron`. The on-disk RON file
    /// is the single source of truth -- the editor reads the exact
    /// same bytes a `cargo run` would, so changes to the default
    /// project are git-trackable and don't require a rebuild.
    ///
    /// Panics only if the embedded RON drifts out of sync with the
    /// `ProjectDocument` schema; the `embedded_default_project_ron_deserializes`
    /// test guards the build-time invariant.
    pub fn starter() -> Self {
        Self::from_ron_str(DEFAULT_PROJECT_RON)
            .expect("editor/projects/default/project.ron is malformed")
    }

    /// Active scene.
    pub fn active_scene(&self) -> &Scene {
        &self.scenes[0]
    }

    /// Active scene, mutable.
    pub fn active_scene_mut(&mut self) -> &mut Scene {
        &mut self.scenes[0]
    }

    /// Active UI scene.
    pub fn active_ui_scene(&self) -> Option<&UiScene> {
        self.ui_scenes.first()
    }

    /// Active UI scene, mutable.
    pub fn active_ui_scene_mut(&mut self) -> Option<&mut UiScene> {
        self.ui_scenes.first_mut()
    }

    /// UI scene by stable id.
    pub fn ui_scene(&self, id: UiSceneId) -> Option<&UiScene> {
        self.ui_scenes.iter().find(|scene| scene.id == id)
    }

    /// UI scene by stable id, mutable.
    pub fn ui_scene_mut(&mut self, id: UiSceneId) -> Option<&mut UiScene> {
        self.ui_scenes.iter_mut().find(|scene| scene.id == id)
    }

    /// UI scene by list position.
    pub fn ui_scene_at(&self, index: usize) -> Option<&UiScene> {
        self.ui_scenes.get(index)
    }

    /// UI scene by list position, mutable.
    pub fn ui_scene_at_mut(&mut self, index: usize) -> Option<&mut UiScene> {
        self.ui_scenes.get_mut(index)
    }

    /// Assign a stable id to every UI scene that still carries
    /// [`UiSceneId::UNASSIGNED`], and de-duplicate any colliding ids
    /// from hand-authored data. Ids already in use are preserved so
    /// they stay stable across renames and reorders.
    fn assign_ui_scene_ids(&mut self) {
        let mut next = UiSceneId::FIRST.raw();
        for scene in &self.ui_scenes {
            if scene.id != UiSceneId::UNASSIGNED {
                next = next.max(scene.id.raw().saturating_add(1));
            }
        }
        let mut seen: HashSet<UiSceneId> = HashSet::new();
        for scene in &mut self.ui_scenes {
            if scene.id == UiSceneId::UNASSIGNED || !seen.insert(scene.id) {
                scene.id = UiSceneId(next);
                seen.insert(scene.id);
                next = next.saturating_add(1);
            }
        }
    }

    /// Assign a stable id to every screen state and de-duplicate any
    /// colliding ids from hand-authored data.
    fn assign_scene_state_ids(&mut self) {
        assign_scene_state_ids_for_slice(&mut self.scene_states);
    }

    /// Normalize authored screen states after load or UI-scene edits.
    fn normalize_scene_states(&mut self) {
        if self.scene_states.is_empty() {
            self.scene_states = default_scene_states_for_ui_scenes(&self.ui_scenes);
        }
        self.assign_scene_state_ids();
        let valid_ui_scenes: HashSet<UiSceneId> =
            self.ui_scenes.iter().map(|scene| scene.id).collect();
        let mut has_gameplay = false;
        for state in &mut self.scene_states {
            if state.name.trim().is_empty() {
                state.name = format!("State {}", state.id.raw());
            }
            if state
                .ui_scene
                .is_some_and(|scene| !valid_ui_scenes.contains(&scene))
            {
                state.ui_scene = None;
            }
            if state.world == SceneWorldLayer::Gameplay {
                has_gameplay = true;
            }
            if state.ui_scene.is_none() && state.world == SceneWorldLayer::None {
                state.ui_input = false;
            }
        }
        self.scene_states
            .retain(|state| state.world != SceneWorldLayer::None || state.ui_scene.is_some());
        for scene in &self.ui_scenes {
            if !self
                .scene_states
                .iter()
                .any(|state| state.ui_scene == Some(scene.id))
            {
                self.scene_states
                    .push(ProjectSceneState::ui_only(scene.name.clone(), scene.id));
            }
        }
        if !has_gameplay {
            self.scene_states
                .push(ProjectSceneState::gameplay("Gameplay", None));
        }
        self.assign_scene_state_ids();
        let valid_state_ids: HashSet<SceneStateId> =
            self.scene_states.iter().map(|state| state.id).collect();
        for state in &mut self.scene_states {
            if state
                .start_state
                .is_some_and(|target| target == state.id || !valid_state_ids.contains(&target))
            {
                state.start_state = None;
            }
        }
        match self.boot {
            BootTarget::SceneState(id) => {
                if !self.scene_states.iter().any(|state| state.id == id) {
                    self.boot = BootTarget::Gameplay;
                }
            }
            BootTarget::UiScene(id) => {
                if !valid_ui_scenes.contains(&id) {
                    self.boot = BootTarget::Gameplay;
                }
            }
            BootTarget::Gameplay => {}
        }
    }

    /// Append a fresh UI scene named `name`, seeded with an empty root
    /// Canvas at the PSX 320x240 authoring resolution, and return its
    /// freshly assigned stable id. The id is handed out by the shared
    /// [`Self::assign_ui_scene_ids`] path so it never collides with an
    /// existing scene and stays stable across renames and reorders.
    pub fn add_ui_scene(&mut self, name: impl Into<String>) -> UiSceneId {
        self.ui_scenes
            .push(UiScene::empty_canvas(name, UiSceneId::UNASSIGNED));
        self.assign_ui_scene_ids();
        self.normalize_scene_states();
        self.ui_scenes
            .last()
            .map(|scene| scene.id)
            .unwrap_or(UiSceneId::UNASSIGNED)
    }

    /// Screen state by stable id.
    pub fn scene_state(&self, id: SceneStateId) -> Option<&ProjectSceneState> {
        self.scene_states.iter().find(|state| state.id == id)
    }

    /// Screen state by stable id, mutable.
    pub fn scene_state_mut(&mut self, id: SceneStateId) -> Option<&mut ProjectSceneState> {
        self.scene_states.iter_mut().find(|state| state.id == id)
    }

    /// Screen state by list position.
    pub fn scene_state_at(&self, index: usize) -> Option<&ProjectSceneState> {
        self.scene_states.get(index)
    }

    /// Screen state by list position, mutable.
    pub fn scene_state_at_mut(&mut self, index: usize) -> Option<&mut ProjectSceneState> {
        self.scene_states.get_mut(index)
    }

    /// Append a fresh screen state and return its assigned id.
    pub fn add_scene_state(&mut self, name: impl Into<String>) -> SceneStateId {
        self.scene_states.push(ProjectSceneState {
            id: SceneStateId::UNASSIGNED,
            name: name.into(),
            world: SceneWorldLayer::None,
            ui_scene: self.ui_scenes.first().map(|scene| scene.id),
            ui_input: true,
            pause_world: false,
            start_state: None,
        });
        self.assign_scene_state_ids();
        self.scene_states
            .last()
            .map(|state| state.id)
            .unwrap_or(SceneStateId::UNASSIGNED)
    }

    /// Remove a screen state at `index`. The project always keeps at
    /// least one gameplay-capable state after normalization.
    pub fn remove_scene_state(&mut self, index: usize) -> bool {
        if index >= self.scene_states.len() {
            return false;
        }
        let removed = self.scene_states.remove(index);
        for state in &mut self.scene_states {
            if state.start_state == Some(removed.id) {
                state.start_state = None;
            }
        }
        if self.boot == BootTarget::SceneState(removed.id) {
            self.boot = BootTarget::Gameplay;
        }
        self.normalize_scene_states();
        true
    }

    /// Deep-copy the UI scene at `index`, insert the copy directly after
    /// the source, give it a fresh stable id, and name it "{source} Copy".
    /// Returns the new scene's id, or `None` when `index` is out of range.
    pub fn duplicate_ui_scene(&mut self, index: usize) -> Option<UiSceneId> {
        let source = self.ui_scenes.get(index)?;
        let mut copy = source.clone();
        copy.id = UiSceneId::UNASSIGNED;
        copy.name = format!("{} Copy", source.name);
        self.ui_scenes.insert(index + 1, copy);
        self.assign_ui_scene_ids();
        self.normalize_scene_states();
        self.ui_scenes.get(index + 1).map(|scene| scene.id)
    }

    /// Remove the UI scene at `index`. `ui_scenes` is never left empty:
    /// removing the final scene re-seeds the default HUD in its place.
    /// Returns `true` when a scene at `index` existed and was removed.
    pub fn remove_ui_scene(&mut self, index: usize) -> bool {
        if index >= self.ui_scenes.len() {
            return false;
        }
        self.ui_scenes.remove(index);
        if self.ui_scenes.is_empty() {
            self.ui_scenes = default_ui_scenes();
            self.assign_ui_scene_ids();
        }
        self.normalize_scene_states();
        true
    }

    /// Look up a project option by stable id.
    pub fn option(&self, id: OptionId) -> Option<&OptionDef> {
        self.options.iter().find(|option| option.id == id)
    }

    /// Append a fresh [`OptionDef`] named `name` with a default
    /// [`OptionKind`], assign it a stable id that does not collide with
    /// any existing option, and return that id.
    pub fn add_option(&mut self, name: impl Into<String>) -> OptionId {
        let id = self.next_option_id();
        self.options.push(OptionDef {
            id,
            name: name.into(),
            kind: OptionKind::default(),
        });
        id
    }

    /// Remove the option at `index`. Returns `true` when an option
    /// existed there. Sliders bound to the removed id keep the id and
    /// simply resolve to nothing until rebound.
    pub fn remove_option(&mut self, index: usize) -> bool {
        if index >= self.options.len() {
            return false;
        }
        self.options.remove(index);
        true
    }

    /// First option id not already in use. Stable across reorders so a
    /// freshly added option never shadows an existing slider binding.
    fn next_option_id(&self) -> OptionId {
        let mut next = 1u32;
        for option in &self.options {
            next = next.max(option.id.raw().saturating_add(1));
        }
        OptionId(next)
    }

    /// Add a resource and return its id.
    pub fn add_resource(&mut self, name: impl Into<String>, data: ResourceData) -> ResourceId {
        let id = ResourceId(self.next_resource_id);
        self.next_resource_id = self.next_resource_id.saturating_add(1);
        self.resources.push(Resource {
            id,
            name: name.into(),
            data,
        });
        id
    }

    /// Get a resource.
    pub fn resource(&self, id: ResourceId) -> Option<&Resource> {
        self.resources.iter().find(|resource| resource.id == id)
    }

    /// Get a mutable resource.
    pub fn resource_mut(&mut self, id: ResourceId) -> Option<&mut Resource> {
        self.resources.iter_mut().find(|resource| resource.id == id)
    }

    /// Return a resource display name.
    pub fn resource_name(&self, id: ResourceId) -> Option<&str> {
        self.resource(id).map(|resource| resource.name.as_str())
    }

    /// Resolve every animation a model can play. Legacy model-local
    /// clips are listed first so existing clip indices remain stable;
    /// target-specific cooked clips are preferred over generic
    /// skeleton-shared clips, de-duplicated by path.
    pub fn resolved_model_animation_clips(
        &self,
        model_id: ResourceId,
    ) -> Vec<ResolvedModelAnimationClip> {
        let Some(model) = self
            .resource(model_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => Some(model),
                _ => None,
            })
        else {
            return Vec::new();
        };

        // Animations must match the skeleton and, when the bake records a
        // target, the model bind pose and quantization bounds as well.
        let Some(skeleton) = model.skeleton else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen_paths = HashSet::new();
        for target_specific in [true, false] {
            for resource in &self.resources {
                let ResourceData::AnimationClip(clip) = &resource.data else {
                    continue;
                };
                if clip.skeleton != Some(skeleton)
                    || (clip.target_model == Some(model_id)) != target_specific
                {
                    continue;
                }
                if !target_specific && clip.target_model.is_some() {
                    continue;
                }
                if seen_paths.insert(clip.psxanim_path.clone()) {
                    out.push(ResolvedModelAnimationClip {
                        name: resource.name.clone(),
                        psxanim_path: clip.psxanim_path.clone(),
                        animation_resource: Some(resource.id),
                        model_clip_index: None,
                        calibration: clip.calibration,
                    });
                }
            }
        }
        out
    }

    /// Resolve the model-local runtime index for a standalone
    /// animation resource after [`Self::resolved_model_animation_clips`]
    /// has appended compatible library clips.
    pub fn resolved_model_animation_index(
        &self,
        model_id: ResourceId,
        animation_id: ResourceId,
    ) -> Option<u16> {
        let resolved = self.resolved_model_animation_clips(model_id);
        if let Some(index) = resolved
            .iter()
            .position(|clip| clip.animation_resource == Some(animation_id))
        {
            return u16::try_from(index).ok();
        }

        let animation_path =
            self.resource(animation_id)
                .and_then(|resource| match &resource.data {
                    ResourceData::AnimationClip(clip)
                        if clip
                            .target_model
                            .is_none_or(|target_model| target_model == model_id) =>
                    {
                        Some(clip.psxanim_path.as_str())
                    }
                    _ => None,
                })?;
        resolved
            .iter()
            .position(|clip| clip.psxanim_path == animation_path)
            .and_then(|index| u16::try_from(index).ok())
    }

    /// Count project references to `id` from scenes and from other
    /// resources. Backing-file paths are counted separately by the
    /// delete plan because they are owned by the resource itself.
    pub fn resource_reference_count(&self, id: ResourceId) -> usize {
        let mut count = 0;
        for resource in &self.resources {
            count += resource_data_reference_count(&resource.data, id);
        }
        for scene in &self.scenes {
            for node in scene.nodes() {
                count += node_kind_reference_count(&node.kind, id);
            }
        }
        for scene in &self.ui_scenes {
            for node in scene.nodes() {
                count += match &node.kind {
                    UiNodeKind::Image { texture, .. } | UiNodeKind::Bar { texture, .. } => {
                        option_resource_reference_count(*texture, id)
                    }
                    _ => 0,
                };
            }
        }
        count
    }

    /// Runtime-ready files whose bytes an open editor project may preview or
    /// package. This intentionally excludes raw model and animation source
    /// files: those can be very large and are consumed only by explicit
    /// import/bake actions, so an idle editor must not poll them.
    pub fn watched_runtime_resource_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut push = |stored: &str| {
            let stored = stored.trim();
            if stored.is_empty() {
                return;
            }
            let path = model_import::resolve_path(stored, Some(project_root));
            if !paths.contains(&path) {
                paths.push(path);
            }
        };
        for resource in &self.resources {
            match &resource.data {
                ResourceData::Texture { psxt_path } => push(psxt_path),
                ResourceData::Material(material) => {
                    for path in material.version_texture_paths() {
                        push(path);
                    }
                }
                ResourceData::Model(model) => {
                    push(&model.model_path);
                    if let Some(path) = model.texture_path.as_deref() {
                        push(path);
                    }
                }
                ResourceData::AnimationClip(clip) => push(&clip.psxanim_path),
                ResourceData::Scene { source_path }
                | ResourceData::Script { source_path }
                | ResourceData::Audio { source_path } => push(source_path),
                ResourceData::Skeleton(_)
                | ResourceData::AnimationSource(_)
                | ResourceData::AnimationSet(_)
                | ResourceData::Mesh { .. }
                | ResourceData::Character(_)
                | ResourceData::Weapon(_)
                | ResourceData::Projectile(_)
                | ResourceData::BoostModule(_) => {}
            }
        }
        paths.sort();
        paths
    }

    /// Remove a resource from the project and clear references to it.
    pub fn delete_resource(&mut self, id: ResourceId) -> Option<ResourceDeleteReport> {
        let index = self
            .resources
            .iter()
            .position(|resource| resource.id == id)?;
        let removed = self.resources.remove(index);
        let cleared_references = self.clear_resource_references(id);
        Some(ResourceDeleteReport {
            removed,
            cleared_references,
            deleted_files: Vec::new(),
            skipped_files: Vec::new(),
        })
    }

    /// Remove a resource, delete its project-owned backing files, and
    /// clear references to it.
    ///
    /// Files are removed before project data is mutated. Only files
    /// that currently exist under `project_root` are deleted; missing
    /// or external paths are skipped and reported.
    pub fn delete_resource_with_files(
        &mut self,
        id: ResourceId,
        project_root: &Path,
    ) -> Result<ResourceDeleteReport, ResourceDeleteError> {
        let Some(index) = self.resources.iter().position(|resource| resource.id == id) else {
            return Err(ResourceDeleteError::MissingResource(id));
        };
        let mut plan = plan_resource_file_deletes(&self.resources[index], project_root);
        // Keep files shared by any active or saved version of another
        // material. Filtering per path avoids preserving unrelated files just
        // because one version happens to share an image.
        plan.files.retain(|op| {
            !self.resources.iter().any(|other| {
                other.id != id
                    && matches!(&other.data, ResourceData::Material(material)
                    if material.version_texture_paths().iter().any(|stored| {
                        model_import::resolve_path(stored, Some(project_root)) == op.abs
                    }))
            })
        });
        execute_resource_delete_plan(&plan, project_root)?;

        let mut report = self
            .delete_resource(id)
            .ok_or(ResourceDeleteError::MissingResource(id))?;
        report.deleted_files = plan
            .files
            .iter()
            .map(|op| ResourceFileDelete {
                path: op.stored.clone(),
            })
            .collect();
        report.skipped_files = plan.skipped;
        Ok(report)
    }

    /// Rename a resource and any project-owned backing files whose
    /// names are derived from the resource name.
    ///
    /// File moves are preflighted before project data is mutated:
    /// destinations must not already exist and duplicate destinations
    /// are refused. Only files that already exist under `project_root`
    /// are moved; missing paths and external absolute paths are
    /// preserved and reported as skipped.
    pub fn rename_resource_with_files(
        &mut self,
        id: ResourceId,
        new_name: &str,
        project_root: &Path,
    ) -> Result<ResourceRenameReport, ResourceRenameError> {
        let final_name = new_name.trim();
        if final_name.is_empty() {
            return Err(ResourceRenameError::EmptyName);
        }

        let Some(index) = self.resources.iter().position(|resource| resource.id == id) else {
            return Err(ResourceRenameError::MissingResource(id));
        };

        let resource = self.resources[index].clone();
        let safe_stem = resource_file_stem(final_name, resource_default_stem(&resource.data));
        let mut plan = ResourceRenamePlan::default();
        let mut data = resource.data.clone();

        match &mut data {
            ResourceData::Texture { psxt_path } => {
                plan_path_rename(psxt_path, &safe_stem, "psxt", project_root, &mut plan);
            }
            // Materials own their image file; rename it alongside the
            // resource unless another material shares the same path
            // (file-level sharing), in which case the file stays put.
            ResourceData::Material(material) => {
                if let Some(psxt_path) = &mut material.psxt_path {
                    let active_path = model_import::resolve_path(psxt_path, Some(project_root));
                    let shared = self
                        .resources
                        .iter()
                        .filter_map(|candidate| match &candidate.data {
                            ResourceData::Material(material) => Some(material),
                            _ => None,
                        })
                        .flat_map(MaterialResource::version_texture_paths)
                        .filter(|candidate| {
                            model_import::resolve_path(candidate, Some(project_root)) == active_path
                        })
                        .take(2)
                        .count()
                        > 1;
                    if !shared {
                        plan_path_rename(psxt_path, &safe_stem, "psxt", project_root, &mut plan);
                    }
                }
            }
            ResourceData::Model(model) => {
                plan_model_resource_rename(model, &safe_stem, project_root, &mut plan);
            }
            ResourceData::AnimationSource(source) => {
                let fallback_ext = resource_default_extension(&resource.data);
                plan_path_rename(
                    &mut source.source_path,
                    &safe_stem,
                    fallback_ext,
                    project_root,
                    &mut plan,
                );
            }
            ResourceData::AnimationClip(clip) => {
                plan_path_rename(
                    &mut clip.psxanim_path,
                    &safe_stem,
                    "psxanim",
                    project_root,
                    &mut plan,
                );
            }
            ResourceData::Mesh { source_path }
            | ResourceData::Scene { source_path }
            | ResourceData::Script { source_path }
            | ResourceData::Audio { source_path } => {
                let fallback_ext = resource_default_extension(&resource.data);
                plan_path_rename(
                    source_path,
                    &safe_stem,
                    fallback_ext,
                    project_root,
                    &mut plan,
                );
            }
            ResourceData::Skeleton(_)
            | ResourceData::AnimationSet(_)
            | ResourceData::Character(_)
            | ResourceData::Weapon(_)
            | ResourceData::Projectile(_)
            | ResourceData::BoostModule(_) => {}
        }

        execute_resource_rename_plan(&plan)?;

        self.resources[index].name = final_name.to_string();
        self.resources[index].data = data;

        Ok(ResourceRenameReport {
            renamed_files: plan
                .ops
                .iter()
                .map(|op| ResourceFileRename {
                    from: op.from_stored.clone(),
                    to: op.to_stored.clone(),
                })
                .collect(),
            skipped_files: plan.skipped,
        })
    }

    fn clear_resource_references(&mut self, id: ResourceId) -> usize {
        let mut count = 0;
        for resource in &mut self.resources {
            count += clear_resource_data_references(&mut resource.data, id);
        }
        for scene in &mut self.scenes {
            for node in &mut scene.nodes {
                count += clear_node_kind_references(&mut node.kind, id);
            }
        }
        for scene in &mut self.ui_scenes {
            for node in scene.nodes_mut() {
                count += match &mut node.kind {
                    UiNodeKind::Image { texture, .. } | UiNodeKind::Bar { texture, .. } => {
                        clear_option_resource(texture, id)
                    }
                    _ => 0,
                };
            }
        }
        count
    }

    /// World-surface material resources as `(id, name)` pairs for inspector
    /// combo boxes. Project-local 2D UI textures live under `assets/ui/` and
    /// stay available to UI texture pickers without polluting room paint
    /// menus or prefab material remapping.
    pub fn material_options(&self) -> Vec<(ResourceId, String)> {
        self.resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(material)
                    if !resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX)
                        && !material.psxt_path.as_deref().is_some_and(|path| {
                            path.starts_with("assets/ui/") || path.starts_with("assets\\ui\\")
                        }) =>
                {
                    Some((resource.id, resource.name.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Serialize this project to human-readable RON.
    pub fn to_ron_string(&self) -> Result<String, ProjectIoError> {
        let config = PrettyConfig::new()
            .depth_limit(4)
            .separate_tuple_members(true)
            .enumerate_arrays(true);
        ron::ser::to_string_pretty(self, config).map_err(ProjectIoError::Serialize)
    }

    /// Deserialize a project from RON.
    pub fn from_ron_str(source: &str) -> Result<Self, ProjectIoError> {
        let mut project: Self = match ron::from_str(source) {
            Ok(project) => project,
            Err(first_error) => {
                let migrated = migrate_legacy_project_ron(source);
                if migrated == source {
                    return Err(ProjectIoError::Parse(first_error));
                }
                ron::from_str(&migrated).map_err(ProjectIoError::Parse)?
            }
        };
        project.normalize_loaded();
        Ok(project)
    }

    /// Save this project to a RON file, creating parent directories.
    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<(), ProjectIoError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut normalized = self.clone();
        normalized.normalize_loaded();
        std::fs::write(path, normalized.to_ron_string()?)?;
        Ok(())
    }

    /// Load a project from a RON file.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ProjectIoError> {
        let source = std::fs::read_to_string(path)?;
        Self::from_ron_str(&source)
    }

    /// Fold legacy Texture resources into the materials that reference
    /// them. Materials own their `.psxt` path now; the separate Texture
    /// resource kind only exists in pre-merge project files.
    ///
    /// Texture ids referenced directly (UI image nodes, far-vista
    /// rings/panels, particle emitter masks) are converted to materials
    /// in place under the SAME resource id, so those references stay
    /// valid without rewriting. Folded and orphaned Texture resources
    /// are dropped. One-way: saves write only merged materials. See
    /// docs/material-texture-merge-plan.md.
    pub fn migrate_legacy_texture_resources(&mut self) {
        use std::collections::{HashMap, HashSet};
        let texture_paths: HashMap<ResourceId, String> = self
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Texture { psxt_path } => Some((resource.id, psxt_path.clone())),
                _ => None,
            })
            .collect();

        // Fold wrapped textures into their materials. Run even with no
        // Texture resources so a stray legacy reference can't linger.
        for resource in &mut self.resources {
            if let ResourceData::Material(material) = &mut resource.data {
                if let Some(texture_id) = material.legacy_texture.take() {
                    if material.psxt_path.is_none() {
                        material.psxt_path = texture_paths.get(&texture_id).cloned();
                    }
                }
            }
        }
        if texture_paths.is_empty() {
            return;
        }

        // Texture ids referenced directly stay alive as materials with
        // the same id; raw-image consumers read just the image part.
        let mut directly_referenced: HashSet<ResourceId> = HashSet::new();
        let note = |id: Option<ResourceId>, set: &mut HashSet<ResourceId>| {
            if let Some(id) = id {
                if texture_paths.contains_key(&id) {
                    set.insert(id);
                }
            }
        };
        for scene in &self.scenes {
            for node in &scene.nodes {
                match &node.kind {
                    NodeKind::World { far_vista, .. } => {
                        note(far_vista.texture, &mut directly_referenced);
                        for panel in far_vista.texture_panels {
                            note(panel, &mut directly_referenced);
                        }
                    }
                    NodeKind::ParticleEmitter { settings } => {
                        note(settings.texture, &mut directly_referenced);
                    }
                    _ => {}
                }
            }
        }
        for scene in &self.ui_scenes {
            for node in scene.nodes() {
                match &node.kind {
                    UiNodeKind::Image { texture, .. } | UiNodeKind::Bar { texture, .. } => {
                        note(*texture, &mut directly_referenced);
                    }
                    _ => {}
                }
            }
        }
        for resource in &mut self.resources {
            if directly_referenced.contains(&resource.id) {
                if let ResourceData::Texture { psxt_path } = &resource.data {
                    resource.data =
                        ResourceData::Material(MaterialResource::opaque(Some(psxt_path.clone())));
                }
            }
        }
        self.resources
            .retain(|resource| !matches!(resource.data, ResourceData::Texture { .. }));
    }

    /// Fold the former material-owned sky projection choices into the World
    /// node. The material keeps only the aperture role; one World now owns the
    /// projection, source texture and visibility policy.
    fn migrate_legacy_material_skies(&mut self) {
        let legacy_skies: Vec<(ResourceId, SkyMode)> = self
            .resources
            .iter()
            .filter_map(|resource| {
                let ResourceData::Material(material) = &resource.data else {
                    return None;
                };
                if material.directional_sky {
                    Some((resource.id, SkyMode::Cube))
                } else if material.layered_sky {
                    Some((resource.id, SkyMode::QuakeLayered))
                } else {
                    None
                }
            })
            .collect();

        for scene in &mut self.scenes {
            let selected = legacy_skies
                .iter()
                .copied()
                .filter(|(material, _)| {
                    scene.brushes.iter().any(|brush| {
                        brush
                            .faces
                            .iter()
                            .any(|face| face.material == Some(*material))
                    })
                })
                // The old renderer gave the cube pass priority if both were
                // visible, so preserve that deterministic result.
                .max_by_key(|(_, mode)| usize::from(*mode == SkyMode::Cube));
            let Some((texture, mode)) = selected else {
                continue;
            };
            let root = scene.root;
            let Some(world) = scene.node_mut(root) else {
                continue;
            };
            let NodeKind::World { sky, .. } = &mut world.kind else {
                continue;
            };
            sky.mode = mode;
            sky.visibility = SkyVisibility::ThroughSkySurfaces;
            sky.texture = Some(texture);
        }

        for resource in &mut self.resources {
            let ResourceData::Material(material) = &mut resource.data else {
                continue;
            };
            material.sky_aperture |= material.layered_sky || material.directional_sky;
            material.layered_sky = false;
            material.directional_sky = false;
        }
    }

    /// Normalize legacy or hand-authored project data after load.
    pub fn normalize_loaded(&mut self) {
        self.migrate_legacy_texture_resources();
        self.migrate_legacy_material_skies();
        for resource in &mut self.resources {
            if let ResourceData::Material(material) = &mut resource.data {
                material.normalize_versions();
            }
        }
        self.editor_camera.normalize();
        if self.ui_scenes.is_empty() {
            self.ui_scenes = default_ui_scenes();
        }
        for scene in &mut self.ui_scenes {
            scene.normalize();
        }
        self.assign_ui_scene_ids();
        self.normalize_scene_states();
        for scene in &mut self.scenes {
            scene.normalize_world_root();
            scene.normalize_brush_groups();
            // CharacterController owns the runtime body dimensions. Older
            // projects also created a sibling capsule Collider, producing two
            // conflicting capsule editors even though the generic Collider is
            // not used by the character motor. Remove only the default-named,
            // solid legacy shape; colliders on ordinary entities and custom
            // character hit/interaction volumes remain intact.
            let redundant_character_capsules: Vec<NodeId> = scene
                .nodes()
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::Entity))
                .filter(|entity| {
                    entity.children.iter().any(|child| {
                        scene.node(*child).is_some_and(|node| {
                            matches!(node.kind, NodeKind::CharacterController { .. })
                        })
                    })
                })
                .flat_map(|entity| entity.children.iter().copied())
                .filter(|child| {
                    scene.node(*child).is_some_and(|node| {
                        node.name == "Collider"
                            && node.children.is_empty()
                            && matches!(
                                node.kind,
                                NodeKind::Collider {
                                    shape: ColliderShape::Capsule { .. },
                                    solid: true,
                                }
                            )
                    })
                })
                .collect();
            for collider in redundant_character_capsules {
                scene.remove_node(collider);
            }
            for node in &mut scene.nodes {
                match &mut node.kind {
                    NodeKind::World {
                        sector_size,
                        sky,
                        far_vista,
                        camera,
                        culling,
                        streaming,
                        physics,
                        world_message,
                    } => {
                        *sector_size = snap_world_sector_size(*sector_size);
                        sky.horizon_percent = sky.horizon_percent.clamp(5, 95);
                        sky.horizon_thickness_percent = sky.horizon_thickness_percent.clamp(0, 80);
                        sky.horizon_glow_percent = sky.horizon_glow_percent.clamp(0, 100);
                        sky.horizon_glow_yaw_degrees =
                            sky.horizon_glow_yaw_degrees.clamp(-180, 180);
                        sky.sun_yaw_degrees = sky.sun_yaw_degrees.clamp(-180, 180);
                        sky.sun_pitch_degrees = sky.sun_pitch_degrees.clamp(-30, 75);
                        sky.sun_size_percent = sky.sun_size_percent.clamp(1, 100);
                        sky.sun_glow_percent = sky.sun_glow_percent.clamp(0, 100);
                        sky.sun_glow_size_percent = sky.sun_glow_size_percent.clamp(0, 100);
                        sky.mountain_height_percent = sky.mountain_height_percent.clamp(0, 100);
                        sky.mountain_gap_percent = sky.mountain_gap_percent.clamp(0, 100);
                        sky.mountain_roughness_percent =
                            sky.mountain_roughness_percent.clamp(0, 100);
                        sky.mountain_layer_count = sky.mountain_layer_count.clamp(1, 3);
                        sky.cloud_layer.tile_count = sky.cloud_layer.tile_count.clamp(1, 16);
                        sky.skybox_columns = sky
                            .skybox_columns
                            .clamp(SKYBOX_COLUMNS_MIN, SKYBOX_COLUMNS_MAX);
                        sky.skybox_rows = sky.skybox_rows.clamp(SKYBOX_ROWS_MIN, SKYBOX_ROWS_MAX);
                        far_vista.radius = far_vista.radius.clamp(1_024, 65_535);
                        far_vista.height = far_vista.height.clamp(128, 32_768);
                        far_vista.vertical_offset =
                            far_vista.vertical_offset.clamp(-32_768, 32_768);
                        far_vista.segments = far_vista.segments.clamp(3, 16);
                        *camera = camera.normalized();
                        *culling = culling.normalized();
                        *streaming = streaming.normalized();
                        *physics = physics.normalized();
                        if let Some(message) = world_message {
                            normalize_message_pages(&mut message.pages);
                        }
                    }
                    NodeKind::PointOfInterest {
                        pages,
                        radius,
                        marker_height,
                        reward,
                        ..
                    } => {
                        normalize_message_pages(pages);
                        *radius = (*radius).max(1);
                        *marker_height = (*marker_height).max(1);
                        if let Some(reward) = reward {
                            reward.quantity = reward.quantity.max(1);
                        }
                    }
                    NodeKind::PhysicsBody { settings } => {
                        *settings = settings.normalized();
                    }
                    _ => {}
                }
            }
            let worlds: Vec<(NodeId, i32)> = scene
                .nodes()
                .iter()
                .filter_map(|node| match &node.kind {
                    NodeKind::World { sector_size, .. } => Some((node.id, *sector_size)),
                    _ => None,
                })
                .collect();
            for (world_id, sector_size) in worlds {
                apply_world_sector_size_to_descendants(
                    scene,
                    world_id,
                    sector_size,
                    sector_size,
                    false,
                );
            }
            let orphan_rooms: Vec<NodeId> = scene
                .nodes()
                .iter()
                .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
                .filter(|node| scene.world_sector_size_for_node(node.id).is_none())
                .map(|node| node.id)
                .collect();
            for room_id in orphan_rooms {
                if let Some(node) = scene.node_mut(room_id) {
                    if let NodeKind::Section { grid } = &mut node.kind {
                        grid.rescale_sector_size(grid.sector_size);
                    }
                }
            }
        }
    }

    /// Sector size inherited by `node_id` from its nearest World
    /// ancestor, or the default when no World exists.
    pub fn world_sector_size_for_node(&self, node_id: NodeId) -> i32 {
        self.active_scene()
            .world_sector_size_for_node(node_id)
            .unwrap_or(DEFAULT_WORLD_SECTOR_SIZE)
    }

    /// Update a World node's sector size, snapping to 128-unit
    /// increments and rescaling descendant rooms/components.
    pub fn set_world_sector_size(&mut self, world_id: NodeId, requested: i32) -> Option<i32> {
        let scene = self.active_scene_mut();
        let new_size = snap_world_sector_size(requested);
        let old_size = {
            let world = scene.node_mut(world_id)?;
            let NodeKind::World { sector_size, .. } = &mut world.kind else {
                return None;
            };
            let old_size = snap_world_sector_size(*sector_size);
            *sector_size = new_size;
            old_size
        };
        apply_world_sector_size_to_descendants(
            scene,
            world_id,
            new_size,
            old_size,
            old_size != new_size,
        );
        Some(new_size)
    }
}

fn normalize_message_pages(pages: &mut Vec<String>) {
    if pages.is_empty() {
        pages.push(String::new());
    }
}

pub(crate) fn resource_data_reference_count(data: &ResourceData, id: ResourceId) -> usize {
    match data {
        ResourceData::Material(material) => material.version_resource_reference_count(id),
        ResourceData::Model(model) => option_resource_reference_count(model.skeleton, id),
        ResourceData::AnimationSource(source) => {
            option_resource_reference_count(source.skeleton, id)
                + option_resource_reference_count(source.target_model, id)
        }
        ResourceData::AnimationClip(clip) => {
            option_resource_reference_count(clip.skeleton, id)
                + option_resource_reference_count(clip.target_model, id)
                + option_resource_reference_count(clip.source, id)
        }
        ResourceData::AnimationSet(set) => {
            option_resource_reference_count(set.skeleton, id)
                + option_resource_reference_count(set.idle_clip, id)
                + option_resource_reference_count(set.walk_clip, id)
                + option_resource_reference_count(set.run_clip, id)
                + option_resource_reference_count(set.turn_clip, id)
                + option_resource_reference_count(set.roll_clip, id)
                + option_resource_reference_count(set.backstep_clip, id)
                + set
                    .action_clips
                    .iter()
                    .filter(|binding| binding.clip == id)
                    .count()
                + set
                    .weapon_appearance_tracks
                    .iter()
                    .filter(|track| track.weapon == id)
                    .count()
                + set.clips.iter().filter(|clip_id| **clip_id == id).count()
        }
        ResourceData::Character(character) => {
            option_resource_reference_count(character.model, id)
                + option_resource_reference_count(character.material, id)
                + option_resource_reference_count(character.animation_set, id)
                + character
                    .default_equipment
                    .iter()
                    .chain(character.loadouts.iter().flat_map(|loadout| &loadout.equipment))
                    .map(|equipment| option_resource_reference_count(equipment.weapon, id))
                    .sum::<usize>()
                + character
                    .combat_capsules
                    .iter()
                    .filter(|volume| {
                        matches!(
                            volume.role,
                            crate::CombatCapsuleRole::ProjectileEmitter {
                                projectile: Some(projectile),
                                ..
                            } if projectile == id
                        )
                    })
                    .count()
        }
        ResourceData::Weapon(weapon) => option_resource_reference_count(weapon.model, id),
        ResourceData::Texture { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::Projectile(_)
        | ResourceData::BoostModule(_) => 0,
    }
}

pub(crate) fn clear_resource_data_references(data: &mut ResourceData, id: ResourceId) -> usize {
    match data {
        ResourceData::Material(material) => material.clear_version_resource_references(id),
        ResourceData::Model(model) => clear_option_resource(&mut model.skeleton, id),
        ResourceData::AnimationSource(source) => {
            clear_option_resource(&mut source.skeleton, id)
                + clear_option_resource(&mut source.target_model, id)
        }
        ResourceData::AnimationClip(clip) => {
            let cleared_target = clear_option_resource(&mut clip.target_model, id);
            if cleared_target > 0 {
                // A model-specific bake must not silently become a generic
                // skeleton clip when its target model is removed.
                clip.skeleton = None;
            }
            clear_option_resource(&mut clip.skeleton, id)
                + cleared_target
                + clear_option_resource(&mut clip.source, id)
        }
        ResourceData::AnimationSet(set) => {
            let mut cleared = clear_option_resource(&mut set.skeleton, id)
                + clear_option_resource(&mut set.idle_clip, id)
                + clear_option_resource(&mut set.walk_clip, id)
                + clear_option_resource(&mut set.run_clip, id)
                + clear_option_resource(&mut set.turn_clip, id)
                + clear_option_resource(&mut set.roll_clip, id)
                + clear_option_resource(&mut set.backstep_clip, id);
            let before_actions = set.action_clips.len();
            set.action_clips.retain(|binding| binding.clip != id);
            cleared += before_actions - set.action_clips.len();
            let before_weapon_tracks = set.weapon_appearance_tracks.len();
            set.weapon_appearance_tracks
                .retain(|track| track.weapon != id);
            cleared += before_weapon_tracks - set.weapon_appearance_tracks.len();
            let before = set.clips.len();
            set.clips.retain(|clip_id| *clip_id != id);
            cleared += before - set.clips.len();
            cleared
        }
        ResourceData::Character(character) => {
            let mut cleared = clear_option_resource(&mut character.model, id)
                + clear_option_resource(&mut character.material, id)
                + clear_option_resource(&mut character.animation_set, id);
            for equipment in character
                .default_equipment
                .iter_mut()
                .chain(character.loadouts.iter_mut().flat_map(|loadout| &mut loadout.equipment))
            {
                cleared += clear_option_resource(&mut equipment.weapon, id);
            }
            for volume in &mut character.combat_capsules {
                if let crate::CombatCapsuleRole::ProjectileEmitter { projectile, .. } =
                    &mut volume.role
                {
                    cleared += clear_option_resource(projectile, id);
                }
            }
            cleared
        }
        ResourceData::Weapon(weapon) => clear_option_resource(&mut weapon.model, id),
        ResourceData::Texture { .. }
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::Projectile(_)
        | ResourceData::BoostModule(_) => 0,
    }
}

pub(crate) fn node_kind_reference_count(kind: &NodeKind, id: ResourceId) -> usize {
    match kind {
        NodeKind::Section { grid } => grid_resource_reference_count(grid, id),
        NodeKind::WaterVolume { material, .. } => option_resource_reference_count(*material, id),
        NodeKind::MeshInstance { mesh, material, .. } => {
            option_resource_reference_count(*mesh, id)
                + option_resource_reference_count(*material, id)
        }
        NodeKind::ImageProp { material, .. } => option_resource_reference_count(*material, id),
        NodeKind::BoxProp { materials, .. } => materials
            .iter()
            .filter(|material| **material == Some(id))
            .count(),
        NodeKind::CylinderProp { materials, .. } => materials
            .iter()
            .filter(|material| **material == Some(id))
            .count(),
        NodeKind::ArchProp { materials, .. } => materials
            .iter()
            .filter(|material| **material == Some(id))
            .count(),
        NodeKind::ModelRenderer {
            model, material, ..
        } => {
            option_resource_reference_count(*model, id)
                + option_resource_reference_count(*material, id)
        }
        NodeKind::CharacterController { character, .. } => {
            option_resource_reference_count(*character, id)
        }
        NodeKind::Equipment { weapon, .. } => option_resource_reference_count(*weapon, id),
        NodeKind::ParticleEmitter { settings } => {
            option_resource_reference_count(settings.texture, id)
        }
        NodeKind::SpawnPoint { character, .. } => option_resource_reference_count(*character, id),
        NodeKind::World { far_vista, .. } => far_vista_resource_reference_count(far_vista, id),
        NodeKind::PointOfInterest { reward, .. } => reward.as_ref().map_or(0, |reward| {
            option_resource_reference_count(reward.module, id)
        }),
        NodeKind::Node
        | NodeKind::Group
        | NodeKind::Node3D
        | NodeKind::Entity
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::Camera { .. }
        | NodeKind::PhysicsBody { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::Logic { .. }
        | NodeKind::Destructible { .. }
        | NodeKind::PointLight { .. }
        | NodeKind::Portal { .. } => 0,
    }
}

pub(crate) fn far_vista_resource_reference_count(
    far_vista: &FarVistaSettings,
    id: ResourceId,
) -> usize {
    option_resource_reference_count(far_vista.texture, id)
        + far_vista
            .texture_panels
            .iter()
            .filter(|panel| **panel == Some(id))
            .count()
}

pub(crate) fn clear_far_vista_resource_references(
    far_vista: &mut FarVistaSettings,
    id: ResourceId,
) -> usize {
    let mut cleared = clear_option_resource(&mut far_vista.texture, id);
    for panel in &mut far_vista.texture_panels {
        cleared += clear_option_resource(panel, id);
    }
    cleared
}

pub(crate) fn clear_node_kind_references(kind: &mut NodeKind, id: ResourceId) -> usize {
    match kind {
        NodeKind::Section { grid } => clear_grid_resource_references(grid, id),
        NodeKind::WaterVolume { material, .. } => clear_option_resource(material, id),
        NodeKind::MeshInstance { mesh, material, .. } => {
            clear_option_resource(mesh, id) + clear_option_resource(material, id)
        }
        NodeKind::ImageProp { material, .. } => clear_option_resource(material, id),
        NodeKind::BoxProp { materials, .. } => {
            let mut cleared = 0;
            for material in materials {
                cleared += clear_option_resource(material, id);
            }
            cleared
        }
        NodeKind::CylinderProp { materials, .. } => {
            let mut cleared = 0;
            for material in materials {
                cleared += clear_option_resource(material, id);
            }
            cleared
        }
        NodeKind::ArchProp { materials, .. } => {
            let mut cleared = 0;
            for material in materials {
                cleared += clear_option_resource(material, id);
            }
            cleared
        }
        NodeKind::ModelRenderer {
            model, material, ..
        } => clear_option_resource(model, id) + clear_option_resource(material, id),
        NodeKind::CharacterController { character, .. } => clear_option_resource(character, id),
        NodeKind::Equipment { weapon, .. } => clear_option_resource(weapon, id),
        NodeKind::ParticleEmitter { settings } => clear_option_resource(&mut settings.texture, id),
        NodeKind::SpawnPoint { character, .. } => clear_option_resource(character, id),
        NodeKind::World { far_vista, .. } => clear_far_vista_resource_references(far_vista, id),
        NodeKind::PointOfInterest { reward, .. } => {
            if reward
                .as_ref()
                .is_some_and(|reward| reward.module == Some(id))
            {
                *reward = None;
                1
            } else {
                0
            }
        }
        NodeKind::Node
        | NodeKind::Group
        | NodeKind::Node3D
        | NodeKind::Entity
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::Camera { .. }
        | NodeKind::PhysicsBody { .. }
        | NodeKind::Interactable { .. }
        | NodeKind::Logic { .. }
        | NodeKind::Destructible { .. }
        | NodeKind::PointLight { .. }
        | NodeKind::Portal { .. } => 0,
    }
}

pub(crate) fn grid_resource_reference_count(grid: &WorldGrid, id: ResourceId) -> usize {
    let mut count = 0;
    for sector in grid.sectors.iter().flatten() {
        if let Some(face) = &sector.floor {
            count += option_resource_reference_count(face.material, id);
        }
        if let Some(face) = &sector.ceiling {
            count += option_resource_reference_count(face.material, id);
        }
        for direction in GridDirection::ALL {
            for wall in sector.walls.get(direction) {
                count += option_resource_reference_count(wall.material, id);
            }
        }
    }
    for floor in &grid.floors_above {
        count += grid_resource_reference_count(floor, id);
    }
    count
}

pub(crate) fn clear_grid_resource_references(grid: &mut WorldGrid, id: ResourceId) -> usize {
    let mut count = 0;
    for sector in grid.sectors.iter_mut().flatten() {
        if let Some(face) = &mut sector.floor {
            count += clear_option_resource(&mut face.material, id);
        }
        if let Some(face) = &mut sector.ceiling {
            count += clear_option_resource(&mut face.material, id);
        }
        for direction in GridDirection::ALL {
            for wall in sector.walls.get_mut(direction) {
                count += clear_option_resource(&mut wall.material, id);
            }
        }
    }
    for floor in &mut grid.floors_above {
        count += clear_grid_resource_references(floor, id);
    }
    count
}

pub(crate) fn option_resource_reference_count(value: Option<ResourceId>, id: ResourceId) -> usize {
    usize::from(value == Some(id))
}

pub(crate) fn clear_option_resource(value: &mut Option<ResourceId>, id: ResourceId) -> usize {
    if *value == Some(id) {
        *value = None;
        1
    } else {
        0
    }
}

pub(crate) fn migrate_legacy_project_ron(source: &str) -> String {
    source
        .replace(
            "kind: World,",
            &format!("kind: World(sector_size: {}),", DEFAULT_WORLD_SECTOR_SIZE),
        )
        .replace("kind: Actor,", "kind: Entity,")
}

pub(crate) fn apply_world_sector_size_to_descendants(
    scene: &mut Scene,
    world_id: NodeId,
    sector_size: i32,
    old_sector_size: i32,
    rescale: bool,
) {
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|node| scene.is_descendant_of(node.id, world_id))
        .map(|node| node.id)
        .collect();
    for id in ids {
        let Some(node) = scene.node_mut(id) else {
            continue;
        };
        match &mut node.kind {
            NodeKind::Section { grid } => {
                if rescale {
                    grid.rescale_sector_size(sector_size);
                } else {
                    grid.normalize_stacked_sector_size(sector_size);
                }
            }
            NodeKind::Collider { shape, .. } if rescale => {
                rescale_collider_shape(shape, old_sector_size, sector_size);
            }
            _ => {}
        }
    }
}

pub(crate) fn rescale_collider_shape(shape: &mut ColliderShape, from: i32, to: i32) {
    match shape {
        ColliderShape::Box { half_extents } => {
            for axis in half_extents {
                *axis = scale_u16_ratio(*axis, from, to);
            }
        }
        ColliderShape::Sphere { radius } => {
            *radius = scale_u16_ratio(*radius, from, to);
        }
        ColliderShape::Capsule { radius, height } => {
            *radius = scale_u16_ratio(*radius, from, to);
            *height = scale_u16_ratio(*height, from, to);
        }
    }
}

#[derive(Default)]
pub(crate) struct ResourceRenamePlan {
    ops: Vec<ResourcePathRename>,
    skipped: Vec<String>,
}

#[derive(Default)]
pub(crate) struct ResourceDeletePlan {
    files: Vec<ResourcePathDelete>,
    skipped: Vec<String>,
}

pub(crate) struct ResourcePathRename {
    from_abs: PathBuf,
    to_abs: PathBuf,
    from_stored: String,
    to_stored: String,
}

pub(crate) struct ResourcePathDelete {
    abs: PathBuf,
    stored: String,
}

pub(crate) fn plan_resource_file_deletes(
    resource: &Resource,
    project_root: &Path,
) -> ResourceDeletePlan {
    let mut plan = ResourceDeletePlan::default();
    match &resource.data {
        ResourceData::Texture { psxt_path } => {
            plan_path_delete(psxt_path, project_root, &mut plan);
        }
        ResourceData::Material(material) => {
            for psxt_path in material.version_texture_paths() {
                plan_path_delete(psxt_path, project_root, &mut plan);
            }
        }
        ResourceData::Model(model) => {
            plan_path_delete(&model.model_path, project_root, &mut plan);
            if let Some(texture_path) = &model.texture_path {
                plan_path_delete(texture_path, project_root, &mut plan);
            }
        }
        ResourceData::AnimationClip(clip) => {
            plan_path_delete(&clip.psxanim_path, project_root, &mut plan);
        }
        ResourceData::AnimationSource(source) => {
            plan_path_delete(&source.source_path, project_root, &mut plan);
        }
        ResourceData::Mesh { source_path }
        | ResourceData::Scene { source_path }
        | ResourceData::Script { source_path }
        | ResourceData::Audio { source_path } => {
            plan_path_delete(source_path, project_root, &mut plan);
        }
        ResourceData::Skeleton(_)
        | ResourceData::AnimationSet(_)
        | ResourceData::Character(_)
        | ResourceData::Weapon(_)
        | ResourceData::Projectile(_)
        | ResourceData::BoostModule(_) => {}
    }
    plan
}

pub(crate) fn plan_path_delete(stored: &str, project_root: &Path, plan: &mut ResourceDeletePlan) {
    let trimmed = stored.trim();
    if trimmed.is_empty() {
        return;
    }

    let abs = model_import::resolve_path(trimmed, Some(project_root));
    if !abs.is_file() {
        plan.skipped.push(trimmed.to_string());
        return;
    }
    if !path_is_project_owned(&abs, project_root) {
        plan.skipped.push(trimmed.to_string());
        return;
    }
    if plan.files.iter().any(|op| op.abs == abs) {
        return;
    }
    plan.files.push(ResourcePathDelete {
        stored: relativise_resource_path(&abs, project_root),
        abs,
    });
}

pub(crate) fn execute_resource_delete_plan(
    plan: &ResourceDeletePlan,
    project_root: &Path,
) -> Result<(), ResourceDeleteError> {
    for op in &plan.files {
        std::fs::remove_file(&op.abs).map_err(|error| ResourceDeleteError::Io {
            path: op.abs.clone(),
            detail: error.to_string(),
        })?;
    }
    for op in &plan.files {
        remove_empty_project_parents(op.abs.parent(), project_root);
    }
    Ok(())
}

pub(crate) fn remove_empty_project_parents(mut dir: Option<&Path>, project_root: &Path) {
    while let Some(current) = dir {
        if current == project_root {
            break;
        }
        if std::fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}

pub(crate) fn plan_path_rename(
    stored: &mut String,
    safe_stem: &str,
    fallback_ext: &str,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) {
    let original = stored.clone();
    let Some(op) = build_path_rename(&original, safe_stem, fallback_ext, project_root, plan) else {
        return;
    };
    *stored = op.to_stored.clone();
    plan.ops.push(op);
}

pub(crate) fn plan_model_resource_rename(
    model: &mut ModelResource,
    safe_stem: &str,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) {
    let model_path = model.model_path.clone();
    let model_abs = model_import::resolve_path(&model_path, Some(project_root));
    let model_dir = model_abs.parent().map(Path::to_path_buf);
    let target_dir = model_dir
        .as_deref()
        .map(|dir| model_bundle_target_dir(dir, safe_stem, project_root));

    if let Some(op) = build_path_rename_in_dir(
        &model_path,
        safe_stem,
        "psxmdl",
        target_dir.as_deref(),
        project_root,
        plan,
    ) {
        model.model_path = op.to_stored.clone();
        plan.ops.push(op);
    }

    if let Some(texture_path) = &mut model.texture_path {
        let original = texture_path.clone();
        if let Some(op) = build_path_rename_in_dir(
            &original,
            safe_stem,
            "psxt",
            target_dir.as_deref(),
            project_root,
            plan,
        ) {
            *texture_path = op.to_stored.clone();
            plan.ops.push(op);
        }
    }
}

pub(crate) fn build_path_rename(
    stored: &str,
    safe_stem: &str,
    fallback_ext: &str,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) -> Option<ResourcePathRename> {
    build_path_rename_in_dir(stored, safe_stem, fallback_ext, None, project_root, plan)
}

pub(crate) fn build_path_rename_in_dir(
    stored: &str,
    safe_stem: &str,
    fallback_ext: &str,
    target_dir: Option<&Path>,
    project_root: &Path,
    plan: &mut ResourceRenamePlan,
) -> Option<ResourcePathRename> {
    let trimmed = stored.trim();
    if trimmed.is_empty() {
        return None;
    }

    let from_abs = model_import::resolve_path(trimmed, Some(project_root));
    if !from_abs.is_file() {
        plan.skipped.push(trimmed.to_string());
        return None;
    }
    if !path_is_project_owned(&from_abs, project_root) {
        plan.skipped.push(trimmed.to_string());
        return None;
    }

    let ext = from_abs
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .unwrap_or(fallback_ext);
    let target_name = format!("{safe_stem}.{ext}");
    let to_abs = target_dir
        .map(|dir| dir.join(&target_name))
        .unwrap_or_else(|| from_abs.with_file_name(target_name));

    if from_abs == to_abs {
        return None;
    }

    Some(ResourcePathRename {
        from_abs,
        to_stored: relativise_resource_path(&to_abs, project_root),
        to_abs,
        from_stored: trimmed.to_string(),
    })
}

pub(crate) fn execute_resource_rename_plan(
    plan: &ResourceRenamePlan,
) -> Result<(), ResourceRenameError> {
    let mut targets = HashSet::new();
    for op in &plan.ops {
        if !targets.insert(op.to_abs.clone()) {
            return Err(ResourceRenameError::DuplicateTarget(op.to_abs.clone()));
        }
        if op.to_abs.exists() {
            return Err(ResourceRenameError::TargetExists(op.to_abs.clone()));
        }
    }

    let mut moved: Vec<&ResourcePathRename> = Vec::new();
    let mut created_dirs = Vec::new();
    for op in &plan.ops {
        if let Some(parent) = op.to_abs.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|error| ResourceRenameError::Io {
                    from: op.from_abs.clone(),
                    to: op.to_abs.clone(),
                    detail: error.to_string(),
                })?;
                created_dirs.push(parent.to_path_buf());
            }
        }
        if let Err(error) = std::fs::rename(&op.from_abs, &op.to_abs) {
            for done in moved.iter().rev() {
                let _ = std::fs::rename(&done.to_abs, &done.from_abs);
            }
            for dir in created_dirs.iter().rev() {
                let _ = std::fs::remove_dir(dir);
            }
            return Err(ResourceRenameError::Io {
                from: op.from_abs.clone(),
                to: op.to_abs.clone(),
                detail: error.to_string(),
            });
        }
        moved.push(op);
    }

    for op in &plan.ops {
        if let (Some(from_parent), Some(to_parent)) = (op.from_abs.parent(), op.to_abs.parent()) {
            if from_parent != to_parent {
                let _ = std::fs::remove_dir(from_parent);
            }
        }
    }

    Ok(())
}

pub(crate) fn model_bundle_target_dir(
    model_dir: &Path,
    safe_stem: &str,
    project_root: &Path,
) -> PathBuf {
    let Ok(relative) = model_dir.strip_prefix(project_root) else {
        return model_dir.to_path_buf();
    };
    let mut components = relative.components();
    let is_imported_bundle = matches!(
        (
            components.next().and_then(|c| c.as_os_str().to_str()),
            components.next().and_then(|c| c.as_os_str().to_str()),
            components.next(),
            components.next()
        ),
        (Some("assets"), Some("models"), Some(_), None)
    );
    if is_imported_bundle {
        project_root.join("assets").join("models").join(safe_stem)
    } else {
        model_dir.to_path_buf()
    }
}

pub(crate) fn path_is_project_owned(path: &Path, project_root: &Path) -> bool {
    path.strip_prefix(project_root).is_ok()
}

pub(crate) fn relativise_resource_path(path: &Path, project_root: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn resource_file_stem(name: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

pub(crate) const fn resource_default_stem(data: &ResourceData) -> &'static str {
    match data {
        ResourceData::Texture { .. } => "texture",
        ResourceData::Material(_) => "material",
        ResourceData::Model(_) => "model",
        ResourceData::Skeleton(_) => "skeleton",
        ResourceData::AnimationSource(_) => "animation_source",
        ResourceData::AnimationClip(_) => "animation",
        ResourceData::AnimationSet(_) => "animation_set",
        ResourceData::Weapon(_) => "weapon",
        ResourceData::Mesh { .. } => "mesh",
        ResourceData::Scene { .. } => "room",
        ResourceData::Script { .. } => "script",
        ResourceData::Audio { .. } => "audio",
        ResourceData::Character(_) => "character",
        ResourceData::Projectile(_) => "projectile",
        ResourceData::BoostModule(_) => "boost_module",
    }
}

pub(crate) const fn resource_default_extension(data: &ResourceData) -> &'static str {
    match data {
        ResourceData::Texture { .. } => "psxt",
        ResourceData::Material(_) => "mat",
        ResourceData::Model(_) => "psxmdl",
        ResourceData::Skeleton(_) => "skeleton",
        ResourceData::AnimationSource(_) => "animsrc",
        ResourceData::AnimationClip(_) => "psxanim",
        ResourceData::AnimationSet(_) => "animset",
        ResourceData::Weapon(_) => "weapon",
        ResourceData::Mesh { .. } => "psxmesh",
        ResourceData::Scene { .. } => "room",
        ResourceData::Script { .. } => "script",
        ResourceData::Audio { .. } => "vag",
        ResourceData::Character(_) => "char",
        ResourceData::Projectile(_) => "projectile",
        ResourceData::BoostModule(_) => "module",
    }
}

impl Default for ProjectDocument {
    fn default() -> Self {
        Self::starter()
    }
}
