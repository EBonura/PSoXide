//! egui editor workspace for PSoXide.
//!
//! The frontend owns the window/Menu. This crate owns the editor panels and
//! the in-memory authoring document they manipulate.

mod geometry;
mod gizmo;
mod history;
mod icons;
mod material_lab;
mod model_animation_viewer;
pub mod model_import_preview;
mod play_mode;
mod selection;
pub use selection::*;
mod starter_catalogue;
mod style;
mod ui_preview;
mod viewport2d;
use starter_catalogue::*;
mod inspector_transform_node;
use inspector_transform_node::*;
mod inspector_assets;
use inspector_assets::*;
mod inspector_character_ui;
use inspector_character_ui::*;
mod play_overlay;
use play_overlay::*;
mod scene_tree;
use scene_tree::*;
mod resource_browser;
use resource_browser::*;
mod searchable_picker;
use searchable_picker::*;
mod editor_helpers;
use editor_helpers::*;
mod animation_catalogue;
use animation_catalogue::*;
mod sector_inspector;
use sector_inspector::*;
mod workspace;
pub use geometry::face_corner_world;
use geometry::*;
use ui_preview::*;
use viewport2d::*;

pub use play_mode::{
    EditorCameraPreviewPresentation, EditorPlaytestMetrics, EditorPlaytestRequest,
    EditorPlaytestStatus, EditorPlaytestTapeMode, EditorPlaytestTapeStatus, EditorViewport3dMode,
    EditorViewport3dPresentation, EditorViewportOverlayLine,
};

use crate::gizmo::*;
use crate::history::UndoStack;
use crate::material_lab::MaterialLabState;
use crate::model_animation_viewer::ModelAnimationViewerState;
use crate::style::*;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use egui::{
    Align2, Color32, ColorImage, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use psxed_project::portal_rooms::{
    extract_portal_room_grid, plan_portal_rooms, portal_edge_for_node, portal_seam_edges_for_edge,
    portal_seam_edges_for_node, PortalEdge, PortalRoomConfig,
};
use psxed_project::room_connections::{
    connection_for_portal, derive_room_connections, RoomConnection, RoomConnectionStatus,
};
use psxed_project::spatial::{euler_degrees_to_matrix, rotate_euler_degrees, RotationSpace};
#[cfg(test)]
use psxed_project::streaming::SceneResourceUse;
use psxed_project::world_cook::{self, WorldGridCookError, WorldGridFaceKind};
use psxed_project::{
    default_model_collision_radius_for_height, default_ui_font_scale, default_ui_letter_spacing,
    snap_height, ui_font_scale_f32_to_q8, ui_font_scale_q8_to_f32, BootTarget,
    CharacterControllerSettings, ColliderShape, EditorCameraMode, EditorCameraState,
    EditorVisibilityState, EditorWorkspaceState, EditorWorkspaceView, FarVistaSettings,
    GeneratedMaterialTexture, GridCellBounds, GridDirection, GridHorizontalFace, GridSector,
    GridSplit, GridTriangleMaterialOverride, GridUvRotation, GridUvTransform, GridVerticalFace,
    InteractableKind, MaterialAnimationMode, MaterialFaceSidedness, MaterialResource,
    MaterialTextureMode, NodeId, NodeKind, NodeRow, OptionId, OptionKind, ParticleEmitterSettings,
    PhysicsBodySettings, ProjectDocument, PsxBlendMode, ReflectionProbeMaterial, Resource,
    ResourceData, ResourceId, RuntimeDepthSortMode, RuntimeRoomDrawOrderMode,
    RuntimeTextureSplitMode, Scene, SceneNode, SceneStateId, SceneWorldLayer, SkyMode, SkySettings,
    TransitionMaskShape, TransitionMaterialTexture, UiAction, UiAnchor, UiFontChoice, UiGradient,
    UiGradientDirection, UiImageEffect, UiNode, UiNodeId, UiNodeKind, UiNodeRow, UiRect, UiScene,
    UiSceneId, UiSfxBindings, UiSfxCue, UiTextAlign, UiValueBinding, WaterVolumeCell,
    WaterVolumeSettings, WorldCameraSettings, WorldCullingSettings, WorldGrid,
    WorldPhysicsSettings, WorldStreamingSettings, AUTO_PAINT_BLEND_PREFIX,
    DEFAULT_WALL_HEIGHT_SECTORS, DEFAULT_WORLD_SECTOR_SIZE, HEIGHT_QUANTUM, MAX_PHYSICS_WEIGHT_Q8,
    MAX_UI_FONT_SCALE, MAX_UI_LETTER_SPACING, MAX_WORLD_CAMERA_DISTANCE, MAX_WORLD_CAMERA_HEIGHT,
    MAX_WORLD_CAMERA_MIN_FLOOR_CLEARANCE, MAX_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS,
    MAX_WORLD_DRAW_DISTANCE, MAX_WORLD_GRAVITY_PER_TICK, MAX_WORLD_SECTOR_SIZE,
    MAX_WORLD_STREAMING_RESIDENT_CHUNKS, MAX_WORLD_STREAMING_VISIBLE_CHUNKS,
    MAX_WORLD_VISIBILITY_RADIUS, MIN_PHYSICS_WEIGHT_Q8, MIN_UI_FONT_SCALE, MIN_UI_LETTER_SPACING,
    MIN_WORLD_CAMERA_DISTANCE, MIN_WORLD_CHUNK_ACTIVATION_RADIUS_SECTORS, MIN_WORLD_DRAW_DISTANCE,
    MIN_WORLD_GRAVITY_PER_TICK, MIN_WORLD_SECTOR_SIZE, MIN_WORLD_STREAMING_RESIDENT_CHUNKS,
    MIN_WORLD_STREAMING_VISIBLE_CHUNKS, MIN_WORLD_VISIBILITY_RADIUS, MODEL_SCALE_ONE_Q8,
    PHYSICS_WEIGHT_ONE_Q8, SKYBOX_COLUMNS_MAX, SKYBOX_COLUMNS_MIN, SKYBOX_ROWS_MAX,
    SKYBOX_ROWS_MIN, SKY_MOUNTAIN_HEIGHT_PERCENT_MAX, WORLD_SECTOR_SIZE_QUANTUM,
};

const RESIZABLE_DOCK_MIN_WIDTH: f32 = 48.0;
const CONTENT_BROWSER_MIN_HEIGHT: f32 = 48.0;
const CENTRAL_WORKSPACE_MIN_WIDTH: f32 = 360.0;
const CENTRAL_WORKSPACE_MIN_HEIGHT: f32 = 220.0;
const LEFT_DOCK_MIN_SPLIT_PANEL_HEIGHT: f32 = 116.0;
const LEFT_DOCK_SPLITTER_HEIGHT: f32 = 8.0;
const LEFT_DOCK_DEFAULT_SCENE_FRACTION: f32 = 0.58;
const LEFT_DOCK_LABEL_CHARS: usize = 34;
const EDITOR_OUTLINE_STROKE_WIDTH: f32 = 1.25;
const EDITOR_SELECTED_OUTLINE_STROKE_WIDTH: f32 = 3.0;
const EDITOR_OUTLINE_ACCENT: Color32 = Color32::from_rgb(165, 238, 255);
const EDITOR_OUTLINE_GOLD: Color32 = Color32::from_rgb(255, 238, 150);
const PORTAL_PINK: Color32 = Color32::from_rgb(255, 72, 214);
const GIZMO_AXIS_PICK_RADIUS: f32 = 10.0;
const GIZMO_ROTATION_PICK_RADIUS: f32 = 12.0;
/// Screen-space forgiveness for selecting BSP brushes and projected brush
/// bounds. Keeping this in pixels makes tiny, distant brushes usable without
/// tying the tolerance to authored world scale.
const BRUSH_SCREEN_PICK_RADIUS: f32 = 8.0;
const PROJECT_WATCH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchedFileMetadata {
    len: u64,
    modified_nanos: Option<u128>,
    created_nanos: Option<u128>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchedProjectSignature {
    metadata: Option<WatchedFileMetadata>,
    hash: Option<u64>,
}

#[derive(Debug, Clone)]
struct ProjectWatchState {
    project: WatchedProjectSignature,
    resources: BTreeMap<PathBuf, Option<WatchedFileMetadata>>,
    last_poll: Instant,
    dirty_conflict: bool,
}

impl ProjectWatchState {
    fn capture(project_dir: &Path, project: &ProjectDocument) -> Self {
        Self {
            project: watched_project_signature(&project_dir.join("project.ron")),
            resources: watched_resource_signatures(project_dir, project),
            last_poll: Instant::now(),
            dirty_conflict: false,
        }
    }
}

fn watched_file_metadata(path: &Path) -> Option<WatchedFileMetadata> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    let created_nanos = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    Some(WatchedFileMetadata {
        len: metadata.len(),
        modified_nanos,
        created_nanos,
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

fn watched_file_hash(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in &bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(hash)
}

fn watched_project_signature(path: &Path) -> WatchedProjectSignature {
    WatchedProjectSignature {
        metadata: watched_file_metadata(path),
        hash: watched_file_hash(path),
    }
}

fn watched_resource_signatures(
    project_dir: &Path,
    project: &ProjectDocument,
) -> BTreeMap<PathBuf, Option<WatchedFileMetadata>> {
    project
        .watched_runtime_resource_paths(project_dir)
        .into_iter()
        .map(|path| {
            let signature = watched_file_metadata(&path);
            (path, signature)
        })
        .collect()
}

fn q12_turns_to_degrees(q12: i32) -> f32 {
    q12.rem_euclid(4096) as f32 * (360.0 / 4096.0)
}

fn q12_turns_to_i16(q12: i32) -> i16 {
    q12.rem_euclid(4096) as i16
}

/// Screen-space forgiveness, in pixels, for grabbing a translate move
/// plane. The axes already pick within [`GIZMO_AXIS_PICK_RADIUS`]; the
/// planes used strict polygon containment with no tolerance, so a click
/// one pixel outside the projected quad (or anywhere on it when a
/// grazing camera angle squashes it to a sliver) fell through to the
/// tile behind. Matching the axes' radius makes plane grabs reliable.
const GIZMO_PLANE_PICK_RADIUS: f32 = 8.0;
const UI_RESIZE_HANDLE_SIZE: f32 = 8.0;
const UI_RESIZE_HANDLE_HIT_SIZE: f32 = 14.0;
const UI_NODE_HIT_MIN_SIZE: f32 = 10.0;
const UI_CENTER_SNAP_TOLERANCE: i32 = 3;
const UI_CENTER_GUIDE_COLOR: Color32 = Color32::from_rgb(80, 170, 255);
const UI_NODE_MIN_SIZE: i32 = 1;
const UI_NODE_COORD_MIN: i32 = -4096;
const UI_NODE_COORD_MAX: i32 = 4096;
const UI_NODE_SIZE_MAX: i32 = 4096;
const MAX_IMAGE_PROP_SIZE: u16 = 4096;
const BOX_PROP_FACE_VERTEX_INDICES: [[usize; 4]; psxed_project::BOX_PROP_FACE_COUNT] = [
    [4, 5, 1, 0],
    [5, 6, 2, 1],
    [6, 7, 3, 2],
    [7, 4, 0, 3],
    [7, 6, 5, 4],
    [0, 1, 2, 3],
];
const BOX_PROP_EDGE_VERTEX_INDICES: [[usize; 2]; 12] = [
    [0, 1],
    [1, 2],
    [2, 3],
    [3, 0],
    [4, 5],
    [5, 6],
    [6, 7],
    [7, 4],
    [0, 4],
    [1, 5],
    [2, 6],
    [3, 7],
];
const PLACEMENT_DUPLICATE_EPSILON: f32 = 0.001;
const EGUI_TEXTURE_RETIRE_FRAMES: u8 = 2;
const RESOURCE_CARD_WIDTH: f32 = 120.0;
const RESOURCE_CARD_HEIGHT: f32 = 128.0;
const VIEWPORT_PREVIEW_ASPECT: f32 = 320.0 / 240.0;
const SHORTCUT_GROUP_FLASH_SECONDS: f32 = 0.85;
const ACTION_BAR_COMPACT_HEIGHT: f32 = 50.0;
const ACTION_BAR_EXPANDED_HEIGHT: f32 = 74.0;
const ACTION_BAR_WRAP_STATUS_CHARS: usize = 96;
const PLAY_FRAME_HISTORY_CAP: usize = 150;
const PLAY_DEBUG_TERMINAL_LINE_CAP: usize = 1_000;
const PLAY_FRAME_TARGET_FPS: f32 = 30.0;
const PSOXIDE_APP_ICON_PNG: &[u8] =
    include_bytes!("../../../../assets/branding/psoxide-app-icon.png");
const PLAY_PORTAL_DEBUG_SCREEN_CX: i32 = 160;
const PLAY_PORTAL_DEBUG_SCREEN_CY: i32 = 120;
const PLAY_PORTAL_DEBUG_FOCAL: i32 = 320;
const PLAY_PORTAL_DEBUG_NEAR_Z: i32 = 64;
const PLAY_PORTAL_DEBUG_FAR_Z: i32 = 16_384;
const PLAY_PORTAL_DEBUG_MAX_DEPTH: u8 = 8;
const PLAY_PORTAL_DEBUG_MIN_WIDTH_Q12: i32 = 4;
const STARTER_CHARACTER_ASSET_DIRS: &[&str] = &[
    "assets/models/obsidian_wraith",
    "assets/models/crimson_cross_knight",
    "assets/models/hooded_wretch",
    "assets/models/crowned_wraith",
    "assets/animations/standalone_fbx",
    "assets/models/ci_player",
    "assets/models/rust_mantis",
    "assets/animations/ci_player_complete",
    "assets/animations/rust_mantis_starter",
    "assets/models/aletha_delivered",
    "assets/animations/aletha_delivered",
    "assets/models/sword1_light",
    "assets/models/sword1_heavy",
];

fn action_bar_height_for_status(status: &str) -> f32 {
    if status.contains('\n') || status.chars().count() > ACTION_BAR_WRAP_STATUS_CHARS {
        ACTION_BAR_EXPANDED_HEIGHT
    } else {
        ACTION_BAR_COMPACT_HEIGHT
    }
}
const STARTER_CHARACTER_MODEL_NAMES: &[&str] = &[
    "Obsidian Wraith",
    "Crimson Cross Knight",
    "Hooded Wretch",
    "Crowned Wraith",
    "Aletha",
    "Aletha Delivered",
    "Rust Mantis",
    "Sword1 Light",
    "Sword1 Heavy",
];
const STARTER_CHARACTER_MATERIAL_NAMES: &[&str] = &["Aletha Crystal"];
/// Verified combat loadout weapons synced beside the character profiles.
const STARTER_WEAPON_NAMES: &[&str] = &["Sword1 Light", "Sword1 Heavy"];
const STARTER_ANIMATION_SET_NAMES: &[&str] = &[
    "Obsidian Wraith Enemy Set",
    "Crimson Cross Knight Player Set",
    // The set the shipped Crimson Cross Knight Player profile actually
    // references; without it the synced profile's animation_set dangled in
    // fresh projects.
    "Crimson Cross Knight / Meshy Gold Standard",
    "Hooded Wretch Enemy Set",
    "Crowned Wraith Enemy Set",
    "Aletha Complete Animation Set",
    "Aletha Delivered Animation Set",
    "Rust Mantis Starter Animation Set",
];
const STARTER_CHARACTER_PROFILE_NAMES: &[&str] = &[
    "Crimson Cross Knight Player",
    "Obsidian Wraith Enemy",
    "Hooded Wretch Enemy",
    "Crowned Wraith Enemy",
    "Aletha",
    "Rust Mantis Enemy",
];
const LEGACY_WRAITH_HERO_PROFILE_NAME: &str = "Wraith Hero";
const LEGACY_OBSIDIAN_WARDEN_ASSET_DIR: &str = "assets/models/obsidian_warden";
const LEGACY_OBSIDIAN_WARDEN_RESOURCE_NAMES: &[&str] = &[
    "Obsidian Warden",
    "Obsidian Warden Enemy Set",
    "Obsidian Warden Enemy",
];

/// Discrete action a scene-tree row can produce in one frame.
///
/// The panel iterates rows borrowing `&self.project` immutably; rows
/// describe what they want to happen via this enum, and the panel
/// drains the queue after iteration so all the mutating helpers
/// (`push_undo`, `add_node`, `move_node`, …) can take `&mut self`
/// without fighting the iteration borrow.
// One transient action per frame, never stored in bulk, so the variant
// spread costs nothing worth a Box indirection.
#[allow(clippy::large_enum_variant)]
enum TreeAction {
    Select {
        id: NodeId,
        modifiers: egui::Modifiers,
    },
    BeginRename(NodeId),
    CommitRename(NodeId, String),
    CancelRename,
    Delete(NodeId),
    Duplicate(NodeId),
    AddChild {
        parent: NodeId,
        kind: NodeKind,
        name: &'static str,
    },
    ToggleExpanded(NodeId),
    ToggleVisibility(NodeId),
    /// Move `source` so it becomes a child of `target_parent` at
    /// `position` in that parent's child list. Caller has already
    /// proven the move is non-cyclic; `Scene::move_node` re-checks.
    Reparent {
        source: NodeId,
        target_parent: NodeId,
        position: usize,
    },
}

enum UiTreeAction {
    Select(UiNodeId),
    Copy(UiNodeId),
    PasteInto(UiNodeId),
    Delete(UiNodeId),
    ToggleVisibility(UiNodeId),
    AddChild {
        parent: UiNodeId,
        kind: UiNodeKind,
        name: &'static str,
    },
    Reparent {
        source: UiNodeId,
        target_parent: UiNodeId,
        position: usize,
    },
}

struct ActionBarStatus<'a> {
    icon: char,
    badge: &'static str,
    message: &'a str,
    accent: Color32,
    border: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShortcutGroup {
    Workspace,
    Tool,
    Transform,
    Selection,
    Surface,
    Vertex,
    Visibility,
    Camera,
    Viewport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentBrowserView {
    Resources,
    Debug,
}

/// An inspector edit that already owns one undo snapshot. Pointer drags and
/// focused keyboard edits keep this alive across frames so a single gesture is
/// a single Ctrl/Cmd+Z step.
#[derive(Debug, Clone, Copy)]
struct InspectorUndoTransaction {
    focused_widget: Option<egui::Id>,
}

#[derive(Debug, Clone, Copy, Default)]
struct InspectorUndoInput {
    pointer_down: bool,
    wants_keyboard: bool,
    focused_widget: Option<egui::Id>,
}

impl InspectorUndoInput {
    fn from_context(ctx: &egui::Context) -> Self {
        Self {
            pointer_down: ctx.input(|input| input.pointer.any_down()),
            wants_keyboard: ctx.wants_keyboard_input(),
            focused_widget: ctx.memory(|memory| memory.focused()),
        }
    }
}

/// Hover-only pointer movement cannot change an inspector value. Avoid cloning
/// the full project on those frames; clicks, drags, wheel input, keyboard/IME,
/// focus events, and accessibility input still pass through this guard.
fn inspector_has_edit_input(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        input.pointer.any_down()
            || input
                .events
                .iter()
                .any(|event| !matches!(event, egui::Event::PointerMoved(_)))
    })
}

/// Embedded editor workspace state.
pub struct EditorWorkspace {
    project: ProjectDocument,
    project_dir: PathBuf,
    saved_project_name: String,
    project_name_editing: bool,
    project_name_focus_pending: bool,
    /// Active centered modal dialog (New / Delete Project), if any.
    modal: Modal,
    /// Everything currently selected or hovered across the editor
    /// (scene tree, content browser, viewports, UI canvas).
    selection: SelectionState,
    /// The single in-flight pointer interaction stroke: drags and
    /// screen-space marquees across the 2D/3D viewports and the UI
    /// canvas. At most one is ever active, so this is one enum
    /// rather than a bag of mutually-exclusive `Option`s. See
    /// [`Interaction`].
    interaction: Interaction,
    /// Selection mode the Select tool picks at: a whole face,
    /// one of its edges, or one of its corners.
    selection_mode: SelectionMode,
    /// Transform gizmo mode for selected scene nodes in the 3D
    /// viewport. Move keeps the existing axis handles; Rotate edits
    /// yaw; Scale edits size data for node kinds that support it.
    transform_gizmo_mode: TransformGizmoMode,
    /// Reference frame for the node transform gizmo: world axes or
    /// the active node's own rotated axes.
    gizmo_space: GizmoSpace,
    /// Transform mode for pointer drags in the 2D UI canvas.
    ui_transform_mode: UiTransformMode,
    /// Whether floor/ceiling face picks address the authored quad
    /// or one triangle half of the current split.
    horizontal_edit_mode: HorizontalEditMode,
    /// Whether vertex edits move every coincident face-corner, or
    /// only the selected face's own corner data.
    vertex_connectivity: VertexConnectivity,
    /// Red authoring-error overlays populated when cook/playtest
    /// validation can map a failure back to concrete grid faces.
    validation_issue_primitives: Vec<Selection>,
    /// Room-level validation failures that don't have a finer face
    /// target, such as budget or dimension errors.
    validation_issue_rooms: HashSet<NodeId>,
    /// Every hard error from the last failed cook, each with the authoring
    /// object it blames. The auto-focus only ever jumps to the first
    /// focusable error; this list lets the author pick any row.
    last_cook_errors: Vec<psxed_project::playtest::PlaytestValidationError>,
    /// Floating duplicate placement created by Cmd+D. While active,
    /// `project` contains the preview copy, but `base_project`
    /// lets Escape cancel without dirtying the document and lets
    /// click commit one clean undo step.
    floating_geometry: Option<FloatingGeometryPlacement>,
    /// Name typed into the Prefabs menu for the next "Save Selection".
    prefab_name: String,
    /// Shared editor-library prefabs, cached outside project data. Refreshed
    /// after saves and on explicit request so panel paint never hits disk.
    prefab_library: Vec<PrefabLibraryEntry>,
    /// What the next paint click would target. Cell variant fires
    /// for floor / ceiling / erase / place; Wall variant fires for
    /// PaintWall. World-cell coords let the preview track cells
    /// outside the current grid bounds -- the renderer outlines
    /// them as ghosts at the world position the auto-grow would
    /// place them.
    paint_target_preview: Option<PaintTargetPreview>,
    /// Last paint stamp committed during the current drag. Edge-
    /// aware so dragging across different edges of the same cell
    /// stamps each one (PaintWall sweeping a cell can hit N then
    /// E without the dedupe blocking the second click), but
    /// dwelling on the same edge doesn't re-stack walls. Reset
    /// to `None` whenever a new primary click starts.
    last_paint_stamp: Option<PaintStamp>,
    /// Wall brush targeting mode. Cardinal keeps the old nearest-edge
    /// behavior; diagonal modes stamp a wall across the cell.
    wall_paint_shape: WallPaintShape,
    /// `Some((id, buffer))` while a scene-tree row is in rename mode.
    /// Buffer holds the in-flight string the user is typing; commit /
    /// cancel finalises against the actual node name.
    renaming: Option<(NodeId, String)>,
    /// One-shot flag set when entering rename mode so the next frame
    /// requests focus + selects the text inside the rename TextEdit.
    pending_rename_focus: bool,
    /// List position of the UI scene the editor is currently authoring.
    /// All UI-scene read/write call sites resolve through this index via
    /// [`Self::current_ui_scene`] / [`Self::current_ui_scene_mut`] so the
    /// editor edits the selected scene rather than always `ui_scenes[0]`.
    /// Clamped against `ui_scenes.len()` after deletions and undo/redo.
    active_ui_scene_index: usize,
    /// List position of the screen state currently selected in the scene
    /// arranger embedded in the UI workspace.
    active_scene_state_index: usize,
    /// Active floor being authored in the Room workspace. Floor 0 is the
    /// base grid; floor `i` reads/writes `grid.floors_above[i - 1]` via
    /// [`WorldGrid::floor`] / [`WorldGrid::floor_mut`]. Clamped per-room
    /// against `grid.floor_count()` at every access. View state, like
    /// `active_ui_scene_index`; not serialized with the project.
    active_floor: usize,
    /// When on, the UI canvas runs an in-editor navigation preview: arrow
    /// keys move focus through the scene's focusable controls via the shared
    /// `psx_level::next_focus` (the same resolver the runtime uses), Enter
    /// activates a focused button's GotoScene, and the focused control is
    /// highlighted. Toggled in the UI workspace; off by default.
    ui_nav_preview: bool,
    /// When on, moving a UI node snaps its centre to the authored canvas
    /// centre within a small tolerance and shows blue centre guide lines.
    ui_center_snap: bool,
    /// Device-pixel horizontal "screen offset" for the UI workspace's
    /// TV-centring simulation -- the in-editor mirror of the runtime's
    /// GP1(06h) screen offset. The preview slides the previewed picture this
    /// many pixels within the fixed screen/bezel so an authored offset can be
    /// eyeballed while authoring. It never changes authored node positions;
    /// `0` = centred.
    screen_offset_sim_px: i16,
    /// Focused control id during the in-editor nav preview, if any.
    ui_nav_focus: Option<UiNodeId>,
    /// `Some((index, buffer))` while a scene strip row is in rename mode.
    /// Mirrors the scene-tree rename pattern: commit on Enter / blur,
    /// cancel on Escape.
    ui_scene_renaming: Option<(usize, String)>,
    /// One-shot focus request for the in-flight scene strip rename field.
    ui_scene_rename_focus_pending: bool,
    /// Scene strip index waiting for a second explicit Delete click, the
    /// two-step delete-confirm pattern used by the resource browser.
    ui_scene_delete_confirm: Option<usize>,
    collapsed_scene_nodes: HashSet<NodeId>,
    collapsed_file_folders: HashSet<String>,
    hidden_scene_nodes: HashSet<NodeId>,
    hidden_ui_nodes: HashSet<(UiSceneId, UiNodeId)>,
    ui_node_clipboard: Option<UiNodeClipboard>,
    history: UndoStack,
    inspector_undo_transaction: Option<InspectorUndoTransaction>,
    scene_filter: String,
    file_filter: String,
    left_dock_scene_fraction: f32,
    resource_search: String,
    resource_filter: ResourceFilter,
    /// `Some((id, buffer))` while the resource inspector's name
    /// field is editing. Committed resource renames may move backing
    /// files, so they happen on focus loss / Enter rather than on
    /// every keystroke.
    resource_renaming: Option<(ResourceId, String)>,
    /// Resource ids waiting for a second explicit delete click.
    /// Deletion removes project entries, deletes project-owned
    /// backing files, and clears references.
    resource_delete_confirm: Option<Vec<ResourceId>>,
    active_tool: ViewTool,
    /// Kind of node the Place tool drops on a click. Surfaces in
    /// the toolbar as a small picker visible only when
    /// `active_tool == Place`. Player Spawn is unique per world;
    /// the others are additive markers.
    place_kind: PlaceKind,
    /// Cardinal edge used by the dedicated Portal placement tool.
    /// The marker is written at the edge midpoint so the cooker can
    /// snap it back to the authored seam.
    portal_place_direction: GridDirection,
    /// Resource chosen in the Place toolbar. Kept separate from the
    /// resource browser selection so repeated placement doesn't lose
    /// the chosen Model/Profile/Image after each click.
    place_resource: Option<ResourceId>,
    /// Material the next Floor / Wall / Ceiling paint will use when
    /// pinned. `None` means Auto: use the selected Material resource
    /// if one is active, otherwise fall back to a tool-specific
    /// material name hint.
    brush_material: Option<ResourceId>,
    /// When true, the material Paint tool bakes a transition onto the one
    /// clicked face. Neighboring faces are context only and never mutated.
    material_paint_blend: bool,
    /// One-shot eyedropper state for Material Paint. The next existing face
    /// click samples its logical material into the shared brush/resource
    /// selection, then automatically returns to painting.
    material_paint_sampling: bool,
    /// `true` once the in-flight Material Paint gesture has painted at least
    /// one BSP brush face. Cleared at every primary press alongside
    /// `last_paint_stamp`, so a drag across a whole wall costs one undo step.
    brush_face_paint_stroke: bool,
    /// Index into the active scene's brushes, when one is selected.
    selected_brush: Option<usize>,
    /// Multi-brush selection (shift-click). Always contains
    /// `selected_brush` when it is non-empty; empty means "just the
    /// primary" so single-selection flows stay untouched.
    selected_brushes: Vec<usize>,
    /// Selected face index within the selected brush.
    selected_brush_face: Option<usize>,
    /// Selected sub-elements of the primary brush (faces by index,
    /// edges/vertices by quantized key, see `workspace::brush_elements`).
    /// Last entry is the primary element. Cleared whenever the primary
    /// brush changes; stale keys drop in `reconcile_brush_selection`.
    selected_brush_elements: Vec<BrushElement>,
    /// Visible, mode-driven BSP transform grammar. Move is the default so a
    /// plain drag performs the most common operation without a modifier.
    brush_edit_mode: BrushEditMode,
    /// In-flight element gizmo rotate/scale gesture.
    brush_element_transform: Option<BrushElementTransformDrag>,
    /// In-flight brush-create drag (Brush tool primary held).
    brush_drag: Option<BrushDrag>,
    /// In-flight brush face-extrude drag (Brush tool primary held on a
    /// brush face).
    brush_extrude: Option<BrushExtrude>,
    /// First ground point of a pending two-point brush clip
    /// (Brush tool, modifier-click).
    /// Placed clip points (max 3): position plus the surface normal it
    /// was placed on, which synthesizes the third point for two-point
    /// cuts so sloped surfaces cut perpendicular to themselves.
    brush_clip_points: Vec<BrushClipPoint>,
    /// Which side(s) the next brush clip keeps.
    brush_clip_keep: BrushClipKeep,
    /// Keep face textures anchored to the brush when it moves.
    brush_texture_lock: bool,
    /// In-flight whole-brush move drag (Brush tool, shift-press).
    brush_move: Option<BrushMove>,
    /// In-flight face UV scale/rotation interaction. See [`UvEditTransaction`].
    brush_uv_edit: Option<UvEditTransaction>,
    /// In-flight vertex/edge drag (Brush tool with Vertex or Edge
    /// selection mode in an orthographic view).
    brush_vertex_drag: Option<BrushVertexDrag>,
    /// Percentage of the painted material kept by generated Paint blends.
    /// Stored as a human-facing percentage and converted to the transition
    /// recipe's byte threshold when a stroke is baked.
    material_paint_blend_coverage_percent: u8,
    /// Organic variation applied to the exposed edge of generated blends.
    /// The transition baker clamps this to its seam-safe 0..=96 range.
    material_paint_blend_edge_detail: u8,
    /// Active Water subtool: select an existing volume, add cells to the
    /// selected volume, or erase cells from any volume on the active floor.
    water_tool_mode: WaterToolMode,
    snap_to_grid: bool,
    snap_units: u16,
    show_grid: bool,
    show_portals: bool,
    show_lights: bool,
    /// Wireframe outlines for unselected brushes (View menu toggle).
    show_brush_wireframes: bool,
    preview_fog: bool,
    preview_backface_wireframe: bool,
    preview_bounds: bool,
    show_play_debug_overlays: bool,
    show_play_debug_map: bool,
    play_debug_map_view: PlayDebugMapView,
    play_frame_times_ms: VecDeque<f32>,
    play_frame_last_sample_serial: Option<u32>,
    play_debug_terminal_lines: VecDeque<String>,
    shortcut_group_flash: Option<(ShortcutGroup, Instant)>,
    view_2d: bool,
    /// Active axis-aligned 2D view. Top/Front/Side share this one
    /// world-space focus so changing planes does not lose the authored
    /// location, grid scale, or selection context.
    orthographic_view: OrthographicView,
    orthographic_focus: [f32; 3],
    active_workspace: WorkspaceView,
    left_dock_open: bool,
    inspector_open: bool,
    resources_open: bool,
    content_browser_view: ContentBrowserView,
    viewport_zoom: f32,
    last_viewport_size: Vec2,
    #[cfg(test)]
    last_orthographic_viewport_rect: Rect,
    #[cfg(test)]
    last_orthographic_response: Option<(egui::Id, bool, bool, bool)>,
    /// 3D viewport camera rig (orbit + free-fly params, mode, and all the
    /// camera math). Orbit preserves the original target/radius camera; Free
    /// stores an explicit world position with the same yaw/pitch convention.
    camera_rig: CameraRig,
    /// Editor-only controller playback. The ProjectDocument retains the
    /// authored Entity transform while the renderer consumes a derived pose.
    character_motion_preview: Option<CharacterMotionPreviewState>,
    /// Decoded `.psxt` thumbnails for the resources panel. Built
    /// lazily once per Texture resource (or whenever its `psxt_path`
    /// changes); the egui texture handle stays alive across frames
    /// so the painter can blit it into resource cards without
    /// re-decoding. Keyed on the *Texture* resource id; Materials
    /// follow `material.texture` to the same key.
    texture_thumbs: HashMap<ResourceId, ThumbnailEntry>,
    psoxide_logo_texture: Option<egui::TextureHandle>,
    /// Cached egui textures of the on-device bitmap UI fonts, rasterized once
    /// so the UI-scene preview shows the real glyphs the runtime draws instead
    /// of an egui host face.
    ui_font_textures: Vec<Option<egui::TextureHandle>>,
    /// Persistent texture used by the Model resource inspector's
    /// animated preview. Keeping the handle alive across frames
    /// avoids submitting a texture id that egui has already freed.
    model_resource_preview_texture: Option<egui::TextureHandle>,
    animation_viewer: ModelAnimationViewerState,
    animation_viewer_preview_texture: Option<egui::TextureHandle>,
    material_lab: MaterialLabState,
    material_lab_preview_texture: Option<egui::TextureHandle>,
    texture_import_dialog: TextureImportDialog,
    model_import_dialog: ModelImportDialog,
    import_retired_textures: Vec<(u8, egui::TextureHandle)>,
    dirty: bool,
    /// Polling watcher for the project document and project-owned resource
    /// files. A dirty conflict is latched until explicit Reload succeeds, so
    /// an ordinary Save can never overwrite an external project.ron edit.
    project_watch: ProjectWatchState,
    status: String,
    /// Most recent host-side PSX envelope. Before a successful cook this is
    /// the authored estimate; after it, the exact emitted-package report.
    last_playtest_budget: Option<psxed_project::playtest::PlaytestBudgetReport>,
    /// One-shot request emitted by the editor UI for the frontend
    /// to handle. The frontend owns emulator state and build child
    /// processes, so the editor never launches playtest directly.
    pending_playtest_request: Option<EditorPlaytestRequest>,
}

/// One cached `.psxt` thumbnail plus the metadata the inspector
/// reads off the same parse. `signature` is the path the handle was
/// built from -- when the user retypes the path on a Texture
/// resource, the signature mismatches and the cache rebuilds.
struct ThumbnailEntry {
    signature: String,
    handle: egui::TextureHandle,
    image: ColorImage,
    stats: PsxtStats,
}

#[derive(Clone)]
struct TexturePreviewSnapshot {
    texture_id: egui::TextureId,
    image: ColorImage,
    stats: PsxtStats,
}

#[derive(Clone, Copy)]
struct PickedPsxtTexel {
    x: u16,
    y: u16,
    color: Color32,
}

struct TextureImportDialog {
    open: bool,
    source_path: String,
    output_name: String,
    width: i32,
    height: i32,
    depth_bits: u8,
    centre_crop: bool,
    transparent_index_zero: bool,
    resampler: TextureImportResamplerChoice,
    tint: [u8; 3],
    status: Option<TextureImportStatus>,
    preview: Option<TextureImportPreview>,
}

const TEXTURE_IMPORT_RESOLUTION_PRESETS: [i32; 6] = [8, 16, 32, 64, 128, 256];

impl Default for TextureImportDialog {
    fn default() -> Self {
        Self {
            open: false,
            source_path: String::new(),
            output_name: String::new(),
            width: 32,
            height: 32,
            depth_bits: 4,
            centre_crop: true,
            transparent_index_zero: false,
            resampler: TextureImportResamplerChoice::Lanczos3,
            tint: [255, 255, 255],
            status: None,
            preview: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureImportResamplerChoice {
    Nearest,
    Triangle,
    Lanczos3,
}

impl TextureImportResamplerChoice {
    const fn label(self) -> &'static str {
        match self {
            Self::Nearest => "Nearest",
            Self::Triangle => "Triangle",
            Self::Lanczos3 => "Lanczos3",
        }
    }

    const fn to_import(self) -> psxed_project::texture_import::Resampler {
        match self {
            Self::Nearest => psxed_project::texture_import::Resampler::Nearest,
            Self::Triangle => psxed_project::texture_import::Resampler::Triangle,
            Self::Lanczos3 => psxed_project::texture_import::Resampler::Lanczos3,
        }
    }
}

fn texture_import_resolution_label(width: i32, height: i32) -> String {
    let width = width.clamp(1, 256);
    let height = height.clamp(1, 256);
    if width == height && TEXTURE_IMPORT_RESOLUTION_PRESETS.contains(&width) {
        format!("{width} x {height}")
    } else {
        format!("Custom {width} x {height}")
    }
}

enum TextureImportStatus {
    Info(String),
    Error(String),
}

struct TextureImportPreview {
    handle: egui::TextureHandle,
    stats: PsxtStats,
}

#[derive(Clone, PartialEq, Eq)]
struct TextureImportPreviewKey {
    source_path: String,
    width: i32,
    height: i32,
    depth_bits: u8,
    centre_crop: bool,
    transparent_index_zero: bool,
    resampler: TextureImportResamplerChoice,
    tint: [u8; 3],
}

#[derive(Clone, Copy)]
struct ResourceClick {
    id: ResourceId,
    modifiers: egui::Modifiers,
}

#[derive(Debug, Clone)]
struct PrefabDragPayload {
    path: PathBuf,
}

enum ProjectFileRowAction {
    Select(ResourceClick),
    SelectPrefab(PathBuf),
    ToggleFolder(String),
}

struct ModelImportDialog {
    open: bool,
    source_path: String,
    animation_paths: Vec<String>,
    output_name: String,
    texture_width: i32,
    texture_height: i32,
    texture_depth_bits: u8,
    animation_fps: i32,
    world_height: i32,
    collision_radius: i32,
    visual_scale_q8: i32,
    default_visual_yaw_q12: i32,
    normalize_root_translation: bool,
    force_single_bind: bool,
    collapse_detail_bones: bool,
    selected_clip: usize,
    preview_yaw_q12: i32,
    preview_pitch_q12: i32,
    preview_radius: i32,
    show_animation_root: bool,
    preview_in_place: bool,
    status: Option<ModelImportStatus>,
    preview: Option<ModelImportPreview>,
}

impl Default for ModelImportDialog {
    fn default() -> Self {
        Self {
            open: false,
            source_path: String::new(),
            animation_paths: Vec::new(),
            output_name: String::new(),
            texture_width: 128,
            texture_height: 128,
            texture_depth_bits: 4,
            animation_fps: 15,
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024) as i32,
            visual_scale_q8: MODEL_SCALE_ONE_Q8 as i32,
            default_visual_yaw_q12: 0,
            normalize_root_translation: true,
            force_single_bind: true,
            collapse_detail_bones: true,
            selected_clip: 0,
            preview_yaw_q12: 340,
            preview_pitch_q12: 350,
            preview_radius: 1536,
            show_animation_root: true,
            preview_in_place: true,
            status: None,
            preview: None,
        }
    }
}

enum ModelImportStatus {
    Info(String),
    Error(String),
}

struct ModelImportPreview {
    model_bytes: Vec<u8>,
    report: psxed_project::model_import::RigidModelReport,
    atlas: Option<(egui::TextureHandle, PsxtStats)>,
    atlas_image: Option<ColorImage>,
    animated_texture: Option<egui::TextureHandle>,
    world_height: i32,
    clips: Vec<ModelImportClipPreview>,
}

struct ModelImportClipPreview {
    name: String,
    frames: usize,
    bytes: Vec<u8>,
    byte_len: usize,
    root_motion: Option<RootMotionStats>,
}

#[derive(Copy, Clone)]
struct RootMotionStats {
    min: [i32; 3],
    max: [i32; 3],
    mean: [i32; 3],
    first: [i32; 3],
    last: [i32; 3],
    delta: [i32; 3],
}

/// Decoded metadata for one `.psxt` blob. Cheap to compute
/// (header parse + lengths); shown in the resource inspector so
/// authors can spot mismatches against an authored target depth /
/// dimensions without leaving the editor.
#[derive(Debug, Clone, Copy)]
struct PsxtStats {
    width: u16,
    height: u16,
    /// 4, 8, or 15 -- mirrors `psxed_format::texture::Depth`'s
    /// numeric form.
    depth_bits: u8,
    /// 16 for 4bpp, 256 for 8bpp, 0 for 15bpp.
    clut_entries: u16,
    index_zero_transparent: bool,
    pixel_bytes: u32,
    clut_bytes: u32,
    file_bytes: u32,
}

/// Counts shown in the embedded Play readiness status line.
/// Built once per cook from the validated package; stays in
/// the editor crate (not psxed-project) because the status
/// string is editor-facing UI text.
struct PackageSummary {
    rooms: usize,
    assets: usize,
    textures: usize,
    materials: usize,
    models: usize,
    characters: usize,
    lights: usize,
    entities: usize,
    /// Display name of the player's resolved Character, or
    /// `None` when no player controller was emitted.
    player_character: Option<String>,
}

/// Per-stamp paint dedupe key. Two paint events with equal stamps
/// are considered redundant during a single drag -- typically the
/// second is dropped. The edge component lets PaintWall stamp
/// multiple edges of the same cell without dwelling on one
/// re-firing the same wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaintStamp {
    room: NodeId,
    sx: u16,
    sz: u16,
    tool: ViewTool,
    triangle: Option<HorizontalTriangleIndex>,
    edge: Option<GridDirection>,
    stack: Option<u8>,
}

/// Three-mode selection switch -- Blender-style. `Face` keeps
/// the existing whole-face semantics; `Edge` and `Vertex` pick
/// finer primitives via local-UV math on the picked face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    #[default]
    Face,
    Edge,
    Vertex,
}

impl SelectionMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Face => "Face",
            Self::Edge => "Edge",
            Self::Vertex => "Vertex",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Face => icons::SQUARE,
            Self::Edge => icons::SCAN,
            Self::Vertex => icons::CIRCLE_DOT,
        }
    }
}

/// One selectable sub-element of a brush. Faces are stable authored
/// indices; edges and vertices are quantized solved positions (canonical
/// endpoint order for edges), re-resolved each frame against
/// `workspace::brush_elements` because solved corners have no persistent
/// identity in the data model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BrushElement {
    Face(usize),
    Edge([i64; 3], [i64; 3]),
    Vertex([i64; 3]),
}

/// Direct BSP transform grammar. This is deliberately separate from the
/// legacy grid primitive [`SelectionMode`]: brushes need an explicit
/// whole-object Move state as well as Face/Edge/Vertex reshape states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum BrushEditMode {
    #[default]
    Move,
    Face,
    Edge,
    Vertex,
    /// TrenchBroom-style cut: click 2-3 points to define the plane,
    /// Enter applies, Tab cycles the kept side, Esc clears.
    Clip,
}

impl BrushEditMode {
    const ALL: [Self; 5] = [
        Self::Move,
        Self::Face,
        Self::Edge,
        Self::Vertex,
        Self::Clip,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Face => "Face",
            Self::Edge => "Edge",
            Self::Vertex => "Vertex",
            Self::Clip => "Clip",
        }
    }

    const fn gesture_hint(self) -> &'static str {
        match self {
            Self::Move => "Drag the brush to move it",
            Self::Face => "Click a face to select it; drag its normal handle to extrude",
            Self::Edge => "Drag an edge handle to reshape",
            Self::Vertex => "Drag a vertex handle to reshape",
            Self::Clip => "Click 2-3 points; Enter cuts, X flips the kept side, Esc clears",
        }
    }

    const fn toolbar_hint(self) -> &'static str {
        match self {
            Self::Move => "Drag brush",
            Self::Face => "Select faces; drag normal to extrude",
            Self::Edge => "Drag edge",
            Self::Vertex => "Drag vertex",
            Self::Clip => "Click points, Enter cuts",
        }
    }

    const fn selection_mode(self) -> Option<SelectionMode> {
        match self {
            Self::Move => None,
            Self::Face => Some(SelectionMode::Face),
            Self::Edge => Some(SelectionMode::Edge),
            Self::Vertex => Some(SelectionMode::Vertex),
            Self::Clip => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransformGizmoMode {
    Move,
    Rotate,
    Scale,
}

impl TransformGizmoMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Move => icons::MOVE,
            Self::Rotate => icons::ROTATE_3D,
            Self::Scale => icons::SCALE_3D,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum UiTransformMode {
    #[default]
    Move,
    Rotate,
}

impl UiTransformMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Move => "Move",
            Self::Rotate => "Rotate",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Move => icons::MOVE,
            Self::Rotate => icons::ROTATE_3D,
        }
    }
}

/// Floor/ceiling edit granularity for Select mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HorizontalEditMode {
    #[default]
    Quad,
    Triangle,
}

impl HorizontalEditMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Quad => "Quad",
            Self::Triangle => "Tri",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Quad => icons::SQUARE,
            Self::Triangle => icons::PLAY,
        }
    }
}

/// Vertex propagation behavior for primitive height edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum VertexConnectivity {
    /// Move every face-corner currently sharing the same physical
    /// vertex. Existing project behavior.
    #[default]
    Welded,
    /// Move only the selected face/edge/vertex corner records.
    Detached,
}

impl VertexConnectivity {
    const fn label(self) -> &'static str {
        match self {
            Self::Welded => "Welded",
            Self::Detached => "Detached",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Welded => icons::BLEND,
            Self::Detached => icons::COPY,
        }
    }
}

/// In-flight drag-translate stroke. Captured at drag-start
/// over a primitive in Select mode and applied every frame the
/// pointer moves until release.
#[derive(Debug, Clone)]
struct PrimitiveDrag {
    /// Primitives being dragged. Usually one entry, or the current
    /// multi-selection when the drag starts on an already-selected
    /// primitive.
    targets: Vec<Selection>,
    /// Physical vertices to translate. Each entry carries the owning
    /// room plus every coincident face-corner so one drag can span
    /// multiple selected faces/edges/vertices.
    vertices: Vec<DragVertex>,
    /// Total mouse-Y travel since drag-start, in screen pixels.
    /// Sign-flipped at apply time (screen +Y is down, world +Y
    /// is up). Stored as `f32` because egui hands per-frame
    /// deltas that way.
    accumulated_pixel_dy: f32,
    /// Whether `push_undo` has fired for this stroke. Lazy:
    /// only fires the first time `accumulated_pixel_dy` causes
    /// a non-zero quantum delta, so a press-without-drag (a
    /// pure click) leaves the undo stack alone.
    snapshot_pushed: bool,
}

#[derive(Debug, Clone)]
struct DragVertex {
    room: NodeId,
    vertex: PhysicalVertex,
    /// Pre-drag Y. Apply step is `snap(pre_y + total_delta_world)`
    /// so every frame is derived from the original geometry.
    pre_drag_y: i32,
}

#[derive(Debug, Clone)]
struct PrimitiveGridDrag {
    base_project: ProjectDocument,
    base_dirty: bool,
    room: NodeId,
    targets: Vec<Selection>,
    source_origin: [i32; 2],
    start_cell: [i32; 2],
    current_delta: [i32; 2],
    cells: Vec<GeometryClipboardCell>,
}

#[derive(Debug, Clone, Copy)]
enum Viewport3dPointerTarget {
    PrimitiveGizmo(PrimitiveGizmoAxis),
    NodeGizmo(NodeGizmoHandle),
    Entity(EntityBoundHit),
    Brush {
        brush: usize,
        face: usize,
    },
    Surface {
        face: FaceRef,
        hit: [f32; 3],
        selection: Selection,
    },
}

impl Viewport3dPointerTarget {
    fn primitive_axis(self) -> Option<PrimitiveGizmoAxis> {
        match self {
            Self::PrimitiveGizmo(axis) => Some(axis),
            _ => None,
        }
    }

    fn node_handle(self) -> Option<NodeGizmoHandle> {
        match self {
            Self::NodeGizmo(handle) => Some(handle),
            _ => None,
        }
    }

    fn entity_hit(self) -> Option<EntityBoundHit> {
        match self {
            Self::Entity(hit) => Some(hit),
            _ => None,
        }
    }

    fn face_hit(self) -> Option<(FaceRef, [f32; 3])> {
        match self {
            Self::Surface { face, hit, .. } => Some((face, hit)),
            _ => None,
        }
    }

    fn primitive_selection(self) -> Option<Selection> {
        match self {
            Self::Surface { selection, .. } => Some(selection),
            _ => None,
        }
    }
}

fn lerp_u8(a: u8, b: u8, amount: u16) -> u8 {
    let inv = 255u16.saturating_sub(amount);
    let value = (a as u16)
        .saturating_mul(inv)
        .saturating_add((b as u16).saturating_mul(amount))
        / 255;
    value.min(255) as u8
}

fn rotate_vector_by_matrix(matrix: &[[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

#[derive(Debug, Clone, Copy)]
struct NodeGizmoScreenPlane {
    plane: NodeGizmoPlane,
    corners: [Pos2; 4],
}

#[derive(Debug, Clone, Copy)]
struct BoxPropFaceScreenHandle {
    face: u8,
    center: Pos2,
    end: Pos2,
}

#[derive(Debug, Clone)]
struct NodeRotationGizmoScreenRing {
    axis: PrimitiveGizmoAxis,
    center: Pos2,
    points: Vec<Pos2>,
}

#[derive(Debug, Clone)]
struct PrimitiveGizmoDrag {
    axis: PrimitiveGizmoAxis,
    start_pointer: Pos2,
    screen_axis: Vec2,
    targets: Vec<Selection>,
    y_vertices: Vec<DragVertex>,
    grid: Option<PrimitiveGizmoGridDrag>,
    current_steps: i32,
    snapshot_pushed: bool,
}

#[derive(Debug, Clone)]
struct PrimitiveGizmoGridDrag {
    base_project: ProjectDocument,
    base_dirty: bool,
    room: NodeId,
    targets: Vec<Selection>,
    source_origin: [i32; 2],
    current_delta: [i32; 2],
    cells: Vec<GeometryClipboardCell>,
}

#[derive(Debug, Clone)]
struct NodeGizmoDrag {
    mode: TransformGizmoMode,
    handle: NodeGizmoHandle,
    start_pointer: Pos2,
    /// Projected drag direction for Move/Scale axis handles. Unused by
    /// Rotate, which tracks angular motion in `rotate` instead.
    screen_axis: Vec2,
    start_plane_hit: Option<[f32; 3]>,
    current_plane_delta_world: [f32; 3],
    /// World-space direction (gizmo-basis column) a Move axis handle
    /// translates along; identity-basis columns in Global space.
    move_axis_world: [f32; 3],
    /// Angular tracking state, present only for Rotate drags.
    rotate: Option<NodeGizmoRotateDrag>,
    targets: Vec<NodeGizmoTarget>,
    current_steps: i32,
    snapshot_pushed: bool,
    /// Shift held: world-unit nodes skip the brush-grid snap and move at
    /// single-unit precision. Refreshed every update from the live modifiers.
    free: bool,
}

/// Angular state for a rotation-ring drag. Steps come from the angle
/// the pointer sweeps around the projected pivot, not from linear
/// pointer motion, so grabbing the ring anywhere and circling it works
/// from any camera angle.
#[derive(Debug, Clone, Copy)]
struct NodeGizmoRotateDrag {
    /// Projected ring centre (the gizmo pivot) in viewport points.
    center: Pos2,
    /// `+1`/`-1`: maps screen-space pointer sweep direction onto the
    /// sign of the world rotation, from the projected ring's winding.
    winding: f32,
    /// Pointer polar angle (radians) at the last update.
    last_angle: f32,
    /// Total swept pointer angle (radians, signed, unwrapped across
    /// revolutions).
    accumulated: f32,
    /// Frame the rotation composes in, captured at drag start.
    space: RotationSpace,
}

#[derive(Debug, Clone)]
struct NodeGizmoTarget {
    node: NodeId,
    start_translation: [f32; 3],
    start_rotation_degrees: [f32; 3],
    start_image_prop_size: Option<[u16; 2]>,
    start_box_prop_vertices: Option<[[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT]>,
    start_cylinder_prop_geometry: Option<psxed_project::CylinderPropGeometry>,
    start_arch_prop_geometry: Option<psxed_project::ArchPropGeometry>,
    sector_size: i32,
}

#[derive(Debug, Clone)]
struct GeometryClipboard {
    mode: GeometryClipboardMode,
    source_room: NodeId,
    source_origin: [i32; 2],
    next_paste_origin: [i32; 2],
    width: i32,
    height: i32,
    cells: Vec<GeometryClipboardCell>,
    /// Floors stacked above `cells`, base floor first above the active one.
    /// Always empty for Duplicate, which works one floor at a time; only a
    /// multi-floor prefab fills it.
    extra_floors: Vec<psxed_project::PrefabFloor>,
    /// Lights the piece carries. Empty for Duplicate.
    lights: Vec<psxed_project::PrefabLight>,
}

#[derive(Debug, Clone)]
struct UiNodeClipboard {
    root: UiNodeId,
    root_name: String,
    nodes: Vec<UiNode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeometryClipboardMode {
    ReplaceCells,
    MergePrimitives,
}

/// The clipboard cell and the prefab cell are the same thing: an offset plus
/// an optional sector. Sharing the type is what lets a prefab be written and
/// read with no conversion layer.
type GeometryClipboardCell = psxed_project::PrefabCell;

#[derive(Debug, Clone)]
struct FloatingGeometryPlacement {
    base_project: ProjectDocument,
    base_dirty: bool,
    mode: GeometryClipboardMode,
    room: NodeId,
    origin: [i32; 2],
    width: i32,
    height: i32,
    rotation_quarters: u8,
    flip_x: bool,
    flip_z: bool,
    /// First grid cell observed under the pointer after duplication begins.
    /// Capturing it without moving prevents the command's own frame from
    /// teleporting the adjacent preview to a stale mouse position.
    pointer_anchor_origin: Option<[i32; 2]>,
    /// Preview origin paired with `pointer_anchor_origin`. Pointer motion is
    /// applied as a delta from this nearby starting placement, never as an
    /// absolute snap to wherever the mouse happened to be when Duplicate ran.
    pointer_anchor_placement_origin: [i32; 2],
    /// The geometry authored by the latest preview pass. Keeping this on the
    /// placement makes the duplicate's selection durable across the click
    /// frame that commits it instead of relying on transient UI selection.
    selected_cells: Vec<(u16, u16)>,
    selected_primitives: Vec<Selection>,
    cells: Vec<GeometryClipboardCell>,
    /// Upper floors of a multi-floor prefab. Empty for Duplicate.
    extra_floors: Vec<psxed_project::PrefabFloor>,
    /// Lights the piece carries, materialised as child nodes on commit.
    lights: Vec<psxed_project::PrefabLight>,
    /// Walls the latest preview pass dropped for landing on an edge a
    /// neighbour already claimed. Reported once on commit rather than every
    /// preview frame, which would fight the rotate / flip status messages.
    seam_walls_stripped: usize,
    /// World units added to every authored height in the placement. Heights
    /// are absolute, so this is the only way to land a ground-level piece on
    /// a terrace without re-authoring every face.
    elevation_offset: i32,
}

#[derive(Debug, Clone)]
struct ViewportBoxSelect {
    start: Pos2,
    current: Pos2,
    room: Option<NodeId>,
    additive: bool,
    base_sectors: HashSet<SectorSelection>,
    /// BSP-first scenes marquee brushes instead of the compatibility grid.
    /// The base selection makes additive dragging stable while the marquee
    /// grows and shrinks across frames.
    brushes: bool,
    base_brushes: Vec<usize>,
    base_primary_brush: Option<usize>,
}

impl ViewportBoxSelect {
    fn rect(&self) -> Rect {
        Rect::from_two_pos(self.start, self.current)
    }
}

#[derive(Debug, Clone)]
struct Viewport3dBoxSelect {
    start: Pos2,
    current: Pos2,
    room: Option<NodeId>,
    additive: bool,
    base_primitives: Vec<Selection>,
    brushes: bool,
    base_brushes: Vec<usize>,
    base_primary_brush: Option<usize>,
}

impl Viewport3dBoxSelect {
    fn rect(&self) -> Rect {
        Rect::from_two_pos(self.start, self.current)
    }
}

/// Active node-drag stroke. Set on press over an entity
/// bound, updated each frame the pointer moves with primary
/// held, cleared on release. The drag is constrained to the
/// horizontal plane the node sits on so X/Z editing is the
/// only motion -- Y stays editable via the inspector.
#[derive(Debug, Clone)]
struct NodeDrag {
    /// The node being dragged.
    node: NodeId,
    /// Editor-space translation when the drag started. The
    /// per-frame update writes `start + delta` so floating
    /// rounding errors don't accumulate.
    start_translation: [f32; 3],
    /// World-space hit point on the node's drag plane at
    /// drag start. Subsequent ray hits on the same plane
    /// yield a delta to add to `start_translation`.
    start_world_hit: [f32; 3],
    /// Plane Y in world units -- locked to the node's current
    /// world Y at drag start.
    drag_plane_y: f32,
    /// `true` once `push_undo` has fired for this stroke.
    /// Pure clicks (press without movement) leave the undo
    /// stack untouched.
    snapshot_pushed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl UiResizeHandle {
    const fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::Top => "top",
            Self::TopRight => "top-right",
            Self::Right => "right",
            Self::BottomRight => "bottom-right",
            Self::Bottom => "bottom",
            Self::BottomLeft => "bottom-left",
            Self::Left => "left",
        }
    }

    const fn cursor(self) -> egui::CursorIcon {
        match self {
            Self::TopLeft | Self::BottomRight => egui::CursorIcon::ResizeNwSe,
            Self::TopRight | Self::BottomLeft => egui::CursorIcon::ResizeNeSw,
            Self::Top | Self::Bottom => egui::CursorIcon::ResizeVertical,
            Self::Right | Self::Left => egui::CursorIcon::ResizeHorizontal,
        }
    }

    const fn moves_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft | Self::Left)
    }

    const fn moves_right(self) -> bool {
        matches!(self, Self::TopRight | Self::Right | Self::BottomRight)
    }

    const fn moves_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::Top | Self::TopRight)
    }

    const fn moves_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::Bottom | Self::BottomRight)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiCanvasDragMode {
    Move,
    Rotate,
    Resize(UiResizeHandle),
}

/// Active UI-canvas edit stroke. Coordinates are in authored
/// canvas pixels, so preview scaling does not affect the stored
/// rect values.
#[derive(Debug, Clone)]
struct UiCanvasDrag {
    node: UiNodeId,
    mode: UiCanvasDragMode,
    start_pointer_canvas: [f32; 2],
    start_rect: UiRect,
    start_absolute_rect: UiRect,
    snapshot_pushed: bool,
    snap_center_x: bool,
    snap_center_y: bool,
}

/// The single pointer-driven interaction stroke in flight.
///
/// A stroke is bound to one pointer-button press, so at most one can ever be
/// active. Modelling that as one enum (rather than one `Option<…>` field per
/// stroke kind) makes the illegal "two strokes at once" states unrepresentable
/// and makes every begin-site self-clearing: assigning a new stroke replaces
/// whatever was active, so a stroke can no longer leak when its release event
/// is missed.
///
/// Floating-duplicate placement (`floating_geometry`) is deliberately *not*
/// here: it is a persistent placement *mode* that survives pointer-up and
/// mutates the document with rollback, not a transient stroke.
#[derive(Debug, Clone, Default)]
enum Interaction {
    /// No stroke in flight.
    #[default]
    Idle,
    /// Per-vertex height drag of selected world geometry (3D viewport).
    PrimitiveHeight(PrimitiveDrag),
    /// Grid-snapped X/Z move of selected primitive faces.
    PrimitiveGrid(PrimitiveGridDrag),
    /// Axis-gizmo drag of selected world geometry.
    PrimitiveGizmo(PrimitiveGizmoDrag),
    /// Axis-gizmo drag of selected scene nodes.
    NodeGizmo(NodeGizmoDrag),
    /// Entity-bound plane drag of a scene node (3D viewport).
    Node(NodeDrag),
    /// Screen-space marquee selection in the 3D viewport.
    BoxSelect3d(Viewport3dBoxSelect),
    /// Screen-space marquee selection in the 2D viewport.
    BoxSelect2d(ViewportBoxSelect),
    /// Move/resize stroke in the UI authoring canvas.
    UiCanvas(UiCanvasDrag),
}

/// Generates `&`/`&mut`/owning accessors for one [`Interaction`] variant, so
/// call sites read like the old per-field `Option` API (`.primitive_drag()`,
/// `.primitive_drag_mut()`, `.take_primitive_drag()`) while the storage stays a
/// single enum. The `take_*` form resets to [`Interaction::Idle`] only when the
/// active stroke matches, so it is a safe variant-scoped cancel.
macro_rules! interaction_accessors {
    ($($variant:ident => $get:ident, $get_mut:ident, $take:ident : $ty:ty);* $(;)?) => {
        impl Interaction {
            $(
                fn $get(&self) -> Option<&$ty> {
                    match self {
                        Interaction::$variant(value) => Some(value),
                        _ => None,
                    }
                }

                fn $get_mut(&mut self) -> Option<&mut $ty> {
                    match self {
                        Interaction::$variant(value) => Some(value),
                        _ => None,
                    }
                }

                fn $take(&mut self) -> Option<$ty> {
                    match std::mem::take(self) {
                        Interaction::$variant(value) => Some(value),
                        other => {
                            *self = other;
                            None
                        }
                    }
                }
            )*
        }
    };
}

interaction_accessors! {
    PrimitiveHeight => primitive_drag, primitive_drag_mut, take_primitive_drag : PrimitiveDrag;
    PrimitiveGrid => primitive_grid_drag, primitive_grid_drag_mut, take_primitive_grid_drag : PrimitiveGridDrag;
    PrimitiveGizmo => primitive_gizmo_drag, primitive_gizmo_drag_mut, take_primitive_gizmo_drag : PrimitiveGizmoDrag;
    NodeGizmo => node_gizmo_drag, node_gizmo_drag_mut, take_node_gizmo_drag : NodeGizmoDrag;
    Node => node_drag, node_drag_mut, take_node_drag : NodeDrag;
    BoxSelect3d => box_select_3d, box_select_3d_mut, take_box_select_3d : Viewport3dBoxSelect;
    BoxSelect2d => box_select_2d, box_select_2d_mut, take_box_select_2d : ViewportBoxSelect;
    UiCanvas => ui_canvas_drag, ui_canvas_drag_mut, take_ui_canvas_drag : UiCanvasDrag;
}

/// The active centered modal dialog overlaying the editor.
///
/// New Project and Delete Project are mutually-exclusive, menu-triggered
/// overlays. Modelling them as one enum makes "both dialogs open at once"
/// unrepresentable and binds each dialog's transient input/error to its open
/// state, so neither can linger once the dialog closes. Opening one implicitly
/// dismisses the other.
///
/// Inline confirmations (resource deletion, rendered inside the inspector) and
/// inline renames are not centered overlays, so they live with their panel and
/// selection state rather than here.
#[derive(Debug, Clone, Default)]
enum Modal {
    /// No dialog open.
    #[default]
    None,
    /// File → New Project. Carries the live-edited name and the last
    /// submit error, if any.
    NewProject {
        name: String,
        cook_mode: psxed_project::brush_world::BrushWorldCookMode,
        error: Option<String>,
    },
    /// Delete the current project. Carries the last delete error, if any.
    DeleteProject { error: Option<String> },
}

/// What the editor currently has selected or is hovering, across the scene
/// tree, content browser, 2D/3D viewports and UI canvas.
///
/// Grouping this state out of `EditorWorkspace` shrinks the god-object and lets
/// selection logic borrow `&mut self.selection` disjointly from the document
/// and renderer.
#[derive(Debug, Clone)]
struct SelectionState {
    /// Primary selected scene node; drives the inspector.
    selected_node: NodeId,
    /// Primary selected node in the UI-authoring scene.
    selected_ui_node: UiNodeId,
    /// Multi-selection of scene nodes.
    selected_nodes: HashSet<NodeId>,
    /// Anchor for Shift-click scene-node range selection.
    node_selection_anchor: Option<NodeId>,
    /// Primary selected resource (content browser / inspector).
    selected_resource: Option<ResourceId>,
    /// Shared prefab selected in the filesystem or content browser. This is
    /// editor state only and is never serialized into `project.ron`.
    selected_prefab: Option<PathBuf>,
    /// Multi-selection of resources.
    selected_resources: HashSet<ResourceId>,
    /// Anchor for Shift-click resource range selection.
    resource_selection_anchor: Option<ResourceId>,
    /// Highlighted sector cell within the active Room. Tracked so the
    /// inspector can show per-cell properties without inflating the
    /// scene-tree node count with a node per sector.
    selected_sector: Option<(u16, u16)>,
    /// Multi-cell Room tile selection. Fully qualified with Room id so
    /// selections survive scene-tree focus changes and can span the
    /// active Room without pretending each tile is a scene node.
    selected_sectors: HashSet<SectorSelection>,
    /// Anchor used by Shift-click tile range selection.
    sector_selection_anchor: Option<SectorSelection>,
    /// Primitive under the pointer while the Select tool is active. Updated
    /// every frame the panel is hovered and the tool is Select; cleared when
    /// the pointer leaves or another tool takes over. The renderer outlines
    /// this lightly so the user sees what the next click will pick.
    hovered_primitive: Option<Selection>,
    /// Brush sub-element handle under the cursor this frame (either
    /// view). Drives the pre-click hover highlight in the overlays.
    hovered_brush_handle: Option<BrushElement>,
    /// Primitive the user clicked with the Select tool last. Persists across
    /// frames until the user clicks a different one or switches tools. The
    /// renderer outlines it more boldly than `hovered_primitive`; the
    /// inspector reads it to surface per-primitive properties.
    selected_primitive: Option<Selection>,
    /// Multi primitive selection for Select mode. `selected_primitive` remains
    /// the active/inspected item; this list is the editable set used by
    /// overlay, delete, and drag.
    selected_primitives: Vec<Selection>,
    /// Hovered entity bound under the cursor (Select tool only). Drives the
    /// entity-bounds highlight overlay and the click→select fast path.
    hovered_entity_node: Option<NodeId>,
}

impl SelectionState {
    /// Drop the primitive (face/edge/vertex) selection.
    fn clear_primitives(&mut self) {
        self.selected_primitive = None;
        self.selected_primitives.clear();
    }

    /// Apply a scene-node click with Shift (range) / toggle (Ctrl/Cmd)
    /// modifiers against `visible_order`. Pure selection-state mutation: it
    /// also clears any resource and primitive selection, but leaves status
    /// text and sector state to the caller. egui-free so it is unit-testable.
    fn apply_node_modifiers(&mut self, id: NodeId, shift: bool, toggle: bool, order: &[NodeId]) {
        // Toggling with nothing multi-selected promotes the current primary
        // into the set first, so the click adds a second rather than replacing.
        if toggle && self.selected_nodes.is_empty() && self.selected_node != NodeId::ROOT {
            self.selected_nodes.insert(self.selected_node);
        }
        let fallback = self.selected_node;
        self.selected_node = apply_range_modifiers(
            &mut self.selected_nodes,
            &mut self.node_selection_anchor,
            id,
            shift,
            toggle,
            order,
            fallback,
        )
        .unwrap_or(NodeId::ROOT);
        self.selected_resource = None;
        self.selected_prefab = None;
        self.selected_resources.clear();
        self.resource_selection_anchor = None;
        self.clear_primitives();
    }

    /// Resource counterpart to [`Self::apply_node_modifiers`]. Clears any node
    /// and primitive selection; the caller owns status text, sector state and
    /// the resource-delete confirmation.
    fn apply_resource_modifiers(
        &mut self,
        id: ResourceId,
        shift: bool,
        toggle: bool,
        order: &[ResourceId],
    ) {
        if toggle && self.selected_resources.is_empty() {
            if let Some(current) = self.selected_resource {
                self.selected_resources.insert(current);
            }
        }
        self.selected_resource = apply_range_modifiers(
            &mut self.selected_resources,
            &mut self.resource_selection_anchor,
            id,
            shift,
            toggle,
            order,
            id,
        );
        self.selected_prefab = None;
        self.selected_node = NodeId::ROOT;
        self.selected_nodes.clear();
        self.node_selection_anchor = None;
        self.clear_primitives();
    }
}

/// Resolved physical vertex: every face-corner that currently
/// sits at `world` and therefore moves together when the
/// vertex's height is dragged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalVertex {
    /// Integer world position. Every member is at exactly this
    /// `(X, Y, Z)`.
    pub world: [i32; 3],
    /// Face-corners that share the position. Always non-empty
    /// (contains at least the seed).
    pub members: Vec<FaceCornerRef>,
}

/// Camera style used by the editor's 3D viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportCameraMode {
    /// Original target/radius orbit camera.
    Orbit,
    /// Explicit-position fly camera.
    Free,
}

impl ViewportCameraMode {
    const fn icon(self) -> char {
        match self {
            Self::Orbit => icons::ROTATE_3D,
            Self::Free => icons::MOVE,
        }
    }
}

/// Snapshot of the editor's 3D viewport camera, handed to the
/// frontend each frame so it can drive the editor-owned `HwRenderer`
/// from the same state the editor's viewport input updates.
#[derive(Debug, Clone, Copy)]
pub struct ViewportCameraState {
    /// Active camera style.
    pub mode: ViewportCameraMode,
    /// Yaw, 4096 per full revolution.
    pub yaw_q12: u16,
    /// Pitch, 4096 per full revolution. For Orbit, positive raises
    /// the camera above the target; for Free, positive looks up.
    pub pitch_q12: u16,
    /// Distance from the camera to the orbit target, world units.
    pub radius: i32,
    /// Orbit target in editor preview world units.
    pub target: [i32; 3],
    /// Free-camera position in editor preview world units.
    pub position: [i32; 3],
}

/// Extra editor-preview render requested by the selected Camera inspector.
#[derive(Debug, Clone, Copy)]
pub struct EditorCameraPreviewRequest {
    /// Camera state to render from.
    pub camera: ViewportCameraState,
    /// Room window to render for the preview.
    pub active_room: Option<NodeId>,
    /// Floor to render as the preview's active floor.
    pub active_floor: usize,
}

/// Transient character transform consumed by the native editor renderer.
/// It never mutates or serializes the owning Entity's authored transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorCharacterMotionPreview {
    /// Entity host whose rendered model should use this transform.
    pub entity: NodeId,
    /// Absolute editor-preview world origin in engine units.
    pub origin: [i32; 3],
    /// Absolute preview yaw in Q12 turn units.
    pub yaw_q12: u16,
    /// Effective model-local animation clip selected for this action.
    pub clip: u16,
}

#[derive(Debug, Clone, Copy)]
struct CharacterMotionPreviewState {
    entity: NodeId,
    action: psxed_project::CharacterAnimationAction,
    clip: u16,
    started_at: Instant,
}

/// 3D viewport camera rig: orbit and free-fly parameters plus the active mode.
///
/// Owns all camera math (rotate / pan / dolly / fly / orbit↔free sync) so it is
/// unit-testable without a workspace or egui. Persisting the state back into the
/// project document is the caller's responsibility, kept out of here so the rig
/// stays a pure value type.
#[derive(Debug, Clone)]
struct CameraRig {
    /// Active camera style.
    mode: ViewportCameraMode,
    /// Orbit yaw, 4096 per revolution.
    yaw: u16,
    /// Orbit pitch, clamped near the poles.
    pitch: u16,
    /// Orbit distance from `target`, world units.
    radius: i32,
    /// Orbit target, editor preview world units.
    target: [i32; 3],
    /// Free-camera yaw.
    free_yaw: u16,
    /// Free-camera pitch.
    free_pitch: u16,
    /// Free-camera position, editor preview world units.
    free_position: [i32; 3],
    /// Whether the free camera has been seeded from the orbit camera yet.
    free_initialized: bool,
    /// Scroll-wheel dolly speed multiplier (persisted per project).
    zoom_speed: f32,
}

impl CameraRig {
    /// Per-frame snapshot handed to the renderer.
    fn camera(&self) -> ViewportCameraState {
        match self.mode {
            ViewportCameraMode::Orbit => ViewportCameraState {
                mode: ViewportCameraMode::Orbit,
                yaw_q12: self.yaw,
                pitch_q12: self.pitch,
                radius: self.radius,
                target: self.target,
                position: self.free_position,
            },
            ViewportCameraMode::Free => ViewportCameraState {
                mode: ViewportCameraMode::Free,
                yaw_q12: self.free_yaw,
                pitch_q12: self.free_pitch,
                radius: self.radius,
                target: self.target,
                position: self.free_position,
            },
        }
    }

    /// Drag-rotate the active camera. `delta` is pointer pixels.
    fn rotate(&mut self, delta: Vec2) {
        // 4 q12 units per pixel preserves the pre-q12-fix feel:
        // roughly 0.35 degrees of camera rotation per dragged pixel.
        const CAMERA_DRAG_STEP: f32 = 4.0;
        let yaw_delta = (delta.x * CAMERA_DRAG_STEP) as i16 as u16;
        let pitch_delta = (delta.y * CAMERA_DRAG_STEP) as i32;
        match self.mode {
            ViewportCameraMode::Orbit => {
                self.yaw = self.yaw.wrapping_add(yaw_delta);
                self.pitch = add_q12_signed_clamped(self.pitch, pitch_delta, -960, 960);
            }
            ViewportCameraMode::Free => {
                self.free_yaw = self.free_yaw.wrapping_sub(yaw_delta);
                self.free_pitch = add_q12_signed_clamped(self.free_pitch, -pitch_delta, -960, 960);
                self.free_initialized = true;
            }
        }
    }

    /// Screen-space pan: dolly the orbit target, or slide the free camera.
    fn pan(&mut self, delta: Vec2, panel_size: Vec2) {
        let world_delta = viewport_3d_pan_delta(self.camera(), panel_size, delta);
        match self.mode {
            ViewportCameraMode::Orbit => {
                for (axis, amount) in world_delta.into_iter().enumerate() {
                    self.target[axis] = round_to_i32(self.target[axis] as f32 + amount);
                }
            }
            ViewportCameraMode::Free => self.move_free_world(world_delta),
        }
    }

    fn zoom_speed(&self) -> f32 {
        self.zoom_speed
    }

    fn set_zoom_speed(&mut self, speed: f32) {
        self.zoom_speed = speed.clamp(0.2, 3.0);
    }

    /// Mouse-wheel: dolly the orbit radius, or fly the free camera forward.
    fn scroll(&mut self, scroll: f32) {
        let speed = self.zoom_speed.clamp(0.2, 3.0);
        match self.mode {
            ViewportCameraMode::Orbit => {
                // Scroll = dolly, proportional to the actual scroll
                // magnitude so trackpad momentum glides instead of
                // stepping in fixed 8% notches: ~8% per 50px at speed
                // 1.0, clamped so one event can't teleport the camera
                // or pass it through the target.
                let notches = (scroll / 50.0).clamp(-4.0, 4.0);
                let factor = 1.08f32.powf(-notches * speed);
                self.radius = ((self.radius as f32) * factor).clamp(512.0, 262_144.0) as i32;
            }
            ViewportCameraMode::Free => {
                let amount = (scroll * 8.0 * speed).clamp(-4096.0, 4096.0);
                self.move_free_local(amount, 0.0, 0.0);
            }
        }
    }

    /// Move the free camera along its own basis (forward/right/up).
    fn move_free_local(&mut self, forward: f32, right: f32, vertical_y: f32) {
        let basis = self.camera().basis();
        let delta = [
            basis.forward[0] * forward + basis.right[0] * right,
            basis.forward[1] * forward + basis.right[1] * right + vertical_y,
            basis.forward[2] * forward + basis.right[2] * right,
        ];
        self.move_free_world(delta);
    }

    /// Move the free camera by a world-space delta.
    fn move_free_world(&mut self, delta: [f32; 3]) {
        for (axis, amount) in delta.into_iter().enumerate() {
            self.free_position[axis] = round_to_i32(self.free_position[axis] as f32 + amount);
        }
        self.free_initialized = true;
    }

    /// Switch camera mode, seeding the free camera from the orbit camera on the
    /// first switch. Returns whether the mode actually changed.
    fn set_mode(&mut self, mode: ViewportCameraMode) -> bool {
        if self.mode == mode {
            return false;
        }
        if mode == ViewportCameraMode::Free && !self.free_initialized {
            self.sync_free_to_orbit();
        }
        self.mode = mode;
        true
    }

    /// Copy the orbit camera's framing into the free camera.
    fn sync_free_to_orbit(&mut self) {
        self.free_yaw = self.yaw;
        self.free_pitch = self.pitch;
        self.free_position =
            orbit_camera_position_i32(self.yaw, self.pitch, self.radius, self.target);
        self.free_initialized = true;
    }
}

/// Floating-point camera basis used by editor picking.
#[derive(Debug, Clone, Copy)]
pub struct ViewportCameraBasis {
    /// Camera position in editor preview world units.
    pub position: [f32; 3],
    /// Forward unit vector.
    pub forward: [f32; 3],
    /// Right unit vector.
    pub right: [f32; 3],
    /// Up unit vector.
    pub up: [f32; 3],
}

impl ViewportCameraState {
    /// Orbit target as floating-point preview-world coordinates.
    pub fn target_f32(self) -> [f32; 3] {
        [
            self.target[0] as f32,
            self.target[1] as f32,
            self.target[2] as f32,
        ]
    }

    /// Camera position as floating-point preview-world coordinates.
    pub fn position_f32(self) -> [f32; 3] {
        match self.mode {
            ViewportCameraMode::Orbit => {
                orbit_camera_position_f32(self.yaw_q12, self.pitch_q12, self.radius, self.target)
            }
            ViewportCameraMode::Free => [
                self.position[0] as f32,
                self.position[1] as f32,
                self.position[2] as f32,
            ],
        }
    }

    /// Integer camera position for fixed-point preview render paths.
    pub fn position_i32(self) -> [i32; 3] {
        match self.mode {
            ViewportCameraMode::Orbit => {
                orbit_camera_position_i32(self.yaw_q12, self.pitch_q12, self.radius, self.target)
            }
            ViewportCameraMode::Free => self.position,
        }
    }

    /// Anchor subtracted from emitted room vertices before GTE
    /// projection. Orbit uses the target; Free uses the camera
    /// position so large authored rooms remain camera-local.
    pub fn anchor_i32(self) -> [i32; 3] {
        match self.mode {
            ViewportCameraMode::Orbit => self.target,
            ViewportCameraMode::Free => self.position,
        }
    }

    /// Camera basis in preview-world coordinates.
    pub fn basis(self) -> ViewportCameraBasis {
        let position = self.position_f32();
        let forward = match self.mode {
            ViewportCameraMode::Orbit => {
                let target_world = self.target_f32();
                normalize3([
                    target_world[0] - position[0],
                    target_world[1] - position[1],
                    target_world[2] - position[2],
                ])
            }
            ViewportCameraMode::Free => camera_forward_from_angles(self.yaw_q12, self.pitch_q12),
        };
        let right = normalize3(cross3(forward, [0.0, 1.0, 0.0]));
        let up = cross3(right, forward);
        ViewportCameraBasis {
            position,
            forward,
            right,
            up,
        }
    }

    /// Inverse of [`Self::ray_for_normalized_panel_point`]: project a
    /// world position to normalized panel coordinates (`[-1, 1]`, centre
    /// origin). `None` when at or behind the camera plane.
    pub fn normalized_panel_point_for_world(self, world: [f32; 3]) -> Option<(f32, f32)> {
        let basis = self.basis();
        let v = [
            world[0] - basis.position[0],
            world[1] - basis.position[1],
            world[2] - basis.position[2],
        ];
        let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        let depth = dot(v, basis.forward);
        if depth <= 1.0 {
            return None;
        }
        let half_fov_x: f32 = 0.5;
        let half_fov_y: f32 = 0.5 * 240.0 / 320.0;
        Some((
            dot(v, basis.right) / depth / half_fov_x,
            -dot(v, basis.up) / depth / half_fov_y,
        ))
    }

    /// Build a world-space ray from normalized panel coordinates.
    ///
    /// `nx` and `ny` are in `[-1, 1]`, where `0, 0` is the panel
    /// centre. Constants match the editor preview's 320x240,
    /// projection-plane-320 camera.
    pub fn ray_for_normalized_panel_point(self, nx: f32, ny: f32) -> ([f32; 3], [f32; 3]) {
        let basis = self.basis();
        let half_fov_x: f32 = 0.5;
        let half_fov_y: f32 = 0.5 * 240.0 / 320.0;
        let dir = normalize3([
            basis.forward[0]
                + basis.right[0] * (nx * half_fov_x)
                + basis.up[0] * (-ny * half_fov_y),
            basis.forward[1]
                + basis.right[1] * (nx * half_fov_x)
                + basis.up[1] * (-ny * half_fov_y),
            basis.forward[2]
                + basis.right[2] * (nx * half_fov_x)
                + basis.up[2] * (-ny * half_fov_y),
        ]);
        (basis.position, dir)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewTool {
    /// Click to select; press-and-drag on a selected primitive
    /// (face / edge / vertex) translates it vertically in the
    /// 3D viewport. No separate "Move" tool -- the same gesture
    /// handles both select and move.
    Select,
    /// Paint a floor onto the sector under the cursor (Room context).
    PaintFloor,
    /// Paint a wall on the directed edge under the cursor.
    PaintWall,
    /// Paint a ceiling on the sector under the cursor.
    PaintCeiling,
    /// Repaint exactly one existing floor, ceiling, or wall face.
    PaintMaterial,
    /// Paint cells into a first-class Water Volume on the active Room floor.
    Water,
    /// Clear the painted surface under the cursor.
    Erase,
    /// Drop a child entity node into the sector under the cursor.
    /// The kind of node placed is controlled by `place_kind`.
    Place,
    /// Drag a world-space convex brush on the active Top/Front/Side plane;
    /// click selects the nearest visible brush face under the cursor.
    Brush,
}

/// In-flight brush-create drag: press anchor and current corner on one
/// orthographic plane, snapped in world units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrushDrag {
    pub(crate) anchor: [i32; 3],
    pub(crate) current: [i32; 3],
    pub(crate) view: OrthographicView,
}

/// One placed clip point: grid-snapped position plus the unit normal of
/// the surface it was placed on (view depth axis in 2D).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BrushClipPoint {
    pub(crate) point: [i32; 3],
    pub(crate) normal: [f64; 3],
}

/// Which side(s) a two-point brush clip keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrushClipKeep {
    Both,
    Back,
    Front,
}

impl BrushClipKeep {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Back => "back",
            Self::Front => "front",
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Both => Self::Back,
            Self::Back => Self::Front,
            Self::Front => Self::Both,
        }
    }
}

/// In-flight whole-brush move drag (shift held on press). The same 3-axis
/// delta storage serves 3D ground moves and Top/Front/Side plane moves.
/// When the pressed brush belongs to a multi-selection, `others` carries
/// the rest of the selection's (index, base) pairs so the whole group
/// moves and commits as one gesture.
#[derive(Debug, Clone)]
pub(crate) struct BrushMove {
    pub(crate) index: usize,
    pub(crate) base: psxed_project::brush::Brush,
    pub(crate) others: Vec<(usize, psxed_project::brush::Brush)>,
    pub(crate) press_ground: [f32; 3],
    pub(crate) applied: [i32; 3],
}

/// One face UV scale/rotation interaction, held for as long as the pointer
/// drag or the focused keyboard edit lasts.
///
/// Re-anchoring solves an offset, and that offset is stored in an `i16`. A
/// DragValue emits one small step per frame, so re-solving each frame
/// against the PREVIOUS frame's already-rounded mapping banks up to half a
/// texel of rounding every step: dragging Q8 scale 256 to 512 at an anchor
/// 136 texels out ends about sixty texels away from the phase it started
/// at. The interaction therefore captures where it started ONCE and every
/// later frame solves against that same fixed target, so the error is one
/// rounding, not one per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct UvEditTransaction {
    pub(crate) brush: usize,
    pub(crate) face: usize,
    /// Face-local anchor in the raw texel space `FaceUv::apply` consumes.
    pub(crate) anchor: [f64; 2],
    /// The mapping this interaction started from.
    pub(crate) origin: psxed_project::brush::FaceUv,
    /// `origin.apply(anchor)`: the phase every frame has to reproduce.
    pub(crate) target: [f64; 2],
}

/// In-flight brush face-extrude drag. Orthographic handles slide on one
/// visible world axis; 3D handles slide along the face's exact normal.
/// The scene brush previews live and the base is restored before the single
/// undo-recorded commit on release.
#[derive(Debug, Clone)]
pub(crate) struct BrushExtrude {
    pub(crate) index: usize,
    pub(crate) face: usize,
    pub(crate) base: psxed_project::brush::Brush,
    pub(crate) axis: usize,
    pub(crate) dir: i32,
    /// Pointer y at press (vertical faces drag by pixels).
    pub(crate) press_y: f32,
    /// Unsnapped ground point at press (horizontal drags measure here).
    pub(crate) press_ground: [f32; 3],
    /// Exact outward unit normal for a 3D face drag. `None` is the existing
    /// orthographic axis grammar.
    pub(crate) normal_3d: Option<[f64; 3]>,
    /// Screen direction corresponding to positive movement along normal_3d.
    pub(crate) screen_direction: Vec2,
    pub(crate) units_per_pixel: f32,
    /// Last applied integer translation, world units.
    pub(crate) applied: [i32; 3],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BrushDragPlane3d {
    pub(crate) anchor: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) press_world: [f32; 3],
}

/// In-flight brush vertex/edge drag: the grabbed solved vertices (a
/// projected corner's whole depth column, or an edge's two columns) move
/// together in the active orthographic plane. The scene brush previews
/// live from `base`; one undo records on commit.
#[derive(Debug, Clone)]
pub(crate) struct BrushVertexDrag {
    pub(crate) index: usize,
    pub(crate) base: psxed_project::brush::Brush,
    /// Solved base-brush vertices being dragged, world f64.
    pub(crate) targets: Vec<[f64; 3]>,
    /// Unsnapped plane point at press.
    pub(crate) press_ground: [f32; 3],
    /// Camera-facing drag plane for 3D handles. Orthographic gestures leave
    /// this empty and continue to use press_ground.
    pub(crate) plane_3d: Option<BrushDragPlane3d>,
    /// Last applied snapped delta, world units.
    pub(crate) applied: [i32; 3],
    /// Per-axis constraint from the element gizmo: a masked-off axis
    /// never receives delta. Free handle drags leave all three true.
    pub(crate) axis_mask: [bool; 3],
}

/// In-flight rotate/scale gesture from the element gizmo: the selected
/// element corner set transforms about its centroid around/along the
/// grabbed world axis. Previews rebuild from `base` each frame.
#[derive(Debug, Clone)]
pub(crate) struct BrushElementTransformDrag {
    pub(crate) index: usize,
    pub(crate) base: psxed_project::brush::Brush,
    pub(crate) targets: Vec<[f64; 3]>,
    pub(crate) center: [f64; 3],
    pub(crate) axis: usize,
    /// true = rotate (5 degree steps, Shift 1), false = scale (5% steps).
    pub(crate) rotate: bool,
    pub(crate) start_pointer: Pos2,
    /// Last applied snapped amount: degrees for rotate, percent-delta
    /// steps for scale. Zero means the base brush is still in place.
    pub(crate) applied: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaterToolMode {
    Add,
    Erase,
    Select,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallPaintShape {
    Cardinal,
    NorthWestSouthEast,
    NorthEastSouthWest,
}

impl WallPaintShape {
    const fn label(self) -> &'static str {
        match self {
            Self::Cardinal => "Cardinal",
            Self::NorthWestSouthEast => "NW-SE",
            Self::NorthEastSouthWest => "NE-SW",
        }
    }

    fn direction(self, dx: f32, dz: f32) -> GridDirection {
        match self {
            Self::Cardinal => edge_from_world_offset(dx, dz),
            Self::NorthWestSouthEast => GridDirection::NorthWestSouthEast,
            Self::NorthEastSouthWest => GridDirection::NorthEastSouthWest,
        }
    }
}

/// Variants of the `Place` tool. Maps directly onto the
/// `NodeKind` produced by a Place click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceKind {
    /// `SpawnPoint { player: true }` -- the editor enforces
    /// player-source uniqueness by refusing a second player source
    /// at place time.
    PlayerSpawn,
    /// `SpawnPoint { player: false }`. Multiple OK.
    SpawnMarker,
    /// Prop entity: `Entity + ModelRenderer + Animator` referencing a
    /// [`ResourceData::Model`]. Place flow picks the selected Model
    /// resource if set, auto-picks the only Model resource if exactly
    /// one exists, or refuses with an actionable error otherwise.
    ModelInstance,
    /// Character entity: `Entity + ModelRenderer + Animator +
    /// CharacterController` referencing a
    /// [`ResourceData::Character`] profile.
    Character,
    /// Flat material-backed image prop.
    ImageProp,
    /// Editable material-backed box prop.
    BoxProp,
    /// Low-poly procedural radial prop.
    CylinderProp,
    /// Tile-snapped procedural arch or half-arch.
    ArchProp,
    /// `PointLight` with default color / intensity / radius.
    PointLightMarker,
    /// Fixed-budget point-projected sprite particle emitter.
    ParticleEmitter,
    /// `Portal` marker. Runtime cooking snaps it to the nearest
    /// cardinal sector edge and treats that edge as an open seam.
    Portal,
    /// Placed `Logic` graph node (trigger volume by default; switch
    /// the kind to relay/multisource/door in the inspector).
    Logic,
}

impl PlaceKind {
    const ALL: [Self; 12] = [
        Self::PlayerSpawn,
        Self::SpawnMarker,
        Self::ModelInstance,
        Self::Character,
        Self::ImageProp,
        Self::BoxProp,
        Self::CylinderProp,
        Self::ArchProp,
        Self::PointLightMarker,
        Self::ParticleEmitter,
        Self::Portal,
        Self::Logic,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::PlayerSpawn => "Player Spawn",
            Self::SpawnMarker => "Spawn",
            Self::ModelInstance => "Prop",
            Self::Character => "Character",
            Self::ImageProp => "Image Prop",
            Self::BoxProp => "Box Prop",
            Self::CylinderProp => "Cylinder Prop",
            Self::ArchProp => "Arch Prop",
            Self::PointLightMarker => "Point Light",
            Self::ParticleEmitter => "Particle Emitter",
            Self::Portal => "Portal",
            Self::Logic => "Logic",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::PlayerSpawn | Self::SpawnMarker => icons::MAP_PIN,
            Self::ModelInstance => icons::BOX,
            Self::Character => icons::CIRCLE_DOT,
            Self::ImageProp => icons::PALETTE,
            Self::BoxProp => icons::BOX,
            Self::CylinderProp => icons::CIRCLE_DOT,
            Self::ArchProp => icons::WAYPOINT,
            Self::PointLightMarker => icons::SUN,
            Self::ParticleEmitter => icons::FOCUS,
            Self::Portal => icons::WAYPOINT,
            Self::Logic => icons::BLEND,
        }
    }
}

impl ViewTool {
    const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::PaintFloor => "Floor",
            Self::PaintWall => "Wall",
            Self::PaintCeiling => "Ceiling",
            Self::PaintMaterial => "Paint",
            Self::Water => "Water",
            Self::Erase => "Erase",
            Self::Place => "Place",
            // "Draw": creates and clips brushes. Selecting/reshaping them
            // lives in Select, so the two tools stop reading as duplicates.
            Self::Brush => "Draw",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Select => icons::POINTER,
            Self::PaintFloor => icons::GRID,
            Self::PaintWall => icons::BRICK_WALL,
            Self::PaintCeiling => icons::LAYERS,
            Self::PaintMaterial => icons::PALETTE,
            Self::Water => icons::BLEND,
            Self::Erase => icons::TRASH,
            Self::Place => icons::PLUS,
            Self::Brush => icons::BOX,
        }
    }

    /// `true` when the tool only makes sense once a Room is the
    /// active context -- viewport clicks should be suppressed
    /// otherwise so we don't paint into thin air.
    const fn requires_room_context(self) -> bool {
        matches!(
            self,
            Self::PaintFloor
                | Self::PaintWall
                | Self::PaintCeiling
                | Self::PaintMaterial
                | Self::Water
                | Self::Erase
                | Self::Place
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceFilter {
    All,
    Material,
    ImagePropSource,
    Model,
    Animation,
    Character,
    Weapon,
    Mesh,
    Prefab,
    Room,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceView {
    Room,
    Ui,
    Animation,
    Material,
}

impl WorkspaceView {
    const fn label(self) -> &'static str {
        match self {
            Self::Room => "3D",
            Self::Ui => "2D",
            Self::Animation => "Animation",
            Self::Material => "Material",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Room => icons::GRID,
            Self::Ui => icons::SQUARE,
            Self::Animation => icons::PLAY,
            Self::Material => icons::PALETTE,
        }
    }

    const fn from_project(value: EditorWorkspaceView) -> Self {
        match value {
            EditorWorkspaceView::Room => Self::Room,
            EditorWorkspaceView::Ui => Self::Ui,
            EditorWorkspaceView::Animation => Self::Animation,
            EditorWorkspaceView::Material => Self::Material,
        }
    }

    const fn to_project(self) -> EditorWorkspaceView {
        match self {
            Self::Room => EditorWorkspaceView::Room,
            Self::Ui => EditorWorkspaceView::Ui,
            Self::Animation => EditorWorkspaceView::Animation,
            Self::Material => EditorWorkspaceView::Material,
        }
    }
}

impl ResourceFilter {
    const fn label(self) -> &'static str {
        match self {
            Self::All => "All Resources",
            Self::Material => "Material",
            Self::ImagePropSource => "Image Source",
            Self::Model => "Model",
            Self::Animation => "Animation",
            Self::Character => "Character Profiles",
            Self::Weapon => "Weapon",
            Self::Mesh => "Mesh",
            Self::Prefab => "Prefab",
            Self::Room => "Room",
            Self::Other => "Other",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::All => icons::LAYERS,
            Self::Material => icons::BLEND,
            Self::ImagePropSource => icons::PALETTE,
            Self::Model => icons::BOX,
            Self::Animation => icons::PLAY,
            Self::Character => icons::MAP_PIN,
            Self::Weapon => icons::WAYPOINT,
            Self::Mesh => icons::BOX,
            Self::Prefab => icons::LAYERS,
            Self::Room => icons::GRID,
            Self::Other => icons::FILE,
        }
    }

    fn matches(self, data: &ResourceData) -> bool {
        match self {
            Self::All => true,
            Self::Material => matches!(data, ResourceData::Material(_)),
            Self::ImagePropSource => matches!(data, ResourceData::Material(_)),
            Self::Model => matches!(data, ResourceData::Model(_)),
            Self::Animation => matches!(
                data,
                ResourceData::Skeleton(_)
                    | ResourceData::AnimationSource(_)
                    | ResourceData::AnimationClip(_)
                    | ResourceData::AnimationSet(_)
            ),
            Self::Character => matches!(data, ResourceData::Character(_)),
            Self::Weapon => matches!(data, ResourceData::Weapon(_)),
            Self::Mesh => matches!(data, ResourceData::Mesh { .. }),
            // Shared prefabs are virtual browser entries, not project data.
            Self::Prefab => false,
            Self::Room => matches!(data, ResourceData::Scene { .. }),
            Self::Other => matches!(
                data,
                ResourceData::Script { .. } | ResourceData::Audio { .. }
            ),
        }
    }
}

fn allocate_centered_preview_rect(
    ui: &mut egui::Ui,
    id_salt: &'static str,
    sense: Sense,
) -> (Rect, egui::Response) {
    let avail = ui.available_size();
    let container_size = Vec2::new(avail.x.max(1.0), avail.y.max(1.0));
    let (container, _) = ui.allocate_exact_size(container_size, Sense::hover());
    let rect = centered_aspect_rect(container, VIEWPORT_PREVIEW_ASPECT);
    let response = ui.interact(rect, ui.id().with(id_salt), sense);
    (rect, response)
}

fn centered_aspect_rect(container: Rect, target_aspect: f32) -> Rect {
    let size = container.size();
    if size.x <= 0.0 || size.y <= 0.0 || target_aspect <= 0.0 {
        return container;
    }
    let (w, h) = if size.x / size.y > target_aspect {
        (size.y * target_aspect, size.y)
    } else {
        (size.x, size.x / target_aspect)
    };
    Rect::from_center_size(container.center(), Vec2::new(w, h))
}

/// Horizontal egui-pixel shift for the UI workspace's screen-offset
/// simulation: `offset_px` device pixels scaled onto a preview canvas of egui
/// width `canvas_width` and logical width `logical_w` (e.g. 320). Mirrors how
/// the runtime's GP1(06h) offset slides the whole picture; here it only moves
/// the preview, never the authored layout.
fn screen_offset_preview_shift(offset_px: i16, canvas_width: f32, logical_w: u16) -> f32 {
    offset_px as f32 * canvas_width / logical_w.max(1) as f32
}

fn decode_embedded_png(bytes: &[u8]) -> Option<ColorImage> {
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}

impl EditorWorkspace {
    /// Select a Room Topology diagnostic view for deterministic/headless UI
    /// capture. Returns false for an unknown label.
    pub fn set_play_debug_map_view(&mut self, label: &str) -> bool {
        let Some(view) = PlayDebugMapView::parse(label) else {
            return false;
        };
        self.play_debug_map_view = view;
        self.show_play_debug_map = true;
        true
    }
    /// Current top-level workspace, exposed for native startup validation.
    pub const fn active_workspace_view(&self) -> EditorWorkspaceView {
        self.active_workspace.to_project()
    }

    /// Current editor status text, exposed to deterministic headless UI
    /// validation so scripted input can assert the action it triggered.
    pub fn status_text(&self) -> &str {
        &self.status
    }

    /// Model currently focused in Animation Studio.
    pub const fn animation_viewer_model(&self) -> Option<ResourceId> {
        self.animation_viewer.selected_model()
    }

    /// Whether Animation Studio is focused on the requested resource.
    /// Models and characters resolve to their visual model; clips and sources
    /// additionally verify the exact selected path so deterministic startup
    /// cannot silently fall back to another animation on the same rig.
    pub fn animation_viewer_resource_is_focused(&self, resource_id: ResourceId) -> bool {
        let Some(resource) = self.project.resource(resource_id) else {
            return false;
        };
        match &resource.data {
            ResourceData::Model(_) => self.animation_viewer.selected_model() == Some(resource_id),
            ResourceData::Character(character) => {
                self.animation_viewer.selected_model() == character.model
            }
            ResourceData::AnimationClip(clip) => {
                self.animation_viewer.selected_clip_path() == Some(clip.psxanim_path.as_str())
                    && clip
                        .target_model
                        .is_none_or(|model| self.animation_viewer.selected_model() == Some(model))
            }
            ResourceData::AnimationSource(source) => {
                let selected_path = self.animation_viewer.selected_clip_path();
                let source_is_selected = selected_path == Some(source.source_path.as_str())
                    || self.project.resources.iter().any(|candidate| {
                        let ResourceData::AnimationClip(clip) = &candidate.data else {
                            return false;
                        };
                        clip.source == Some(resource_id)
                            && selected_path == Some(clip.psxanim_path.as_str())
                    });
                source_is_selected
                    && source
                        .target_model
                        .is_none_or(|model| self.animation_viewer.selected_model() == Some(model))
            }
            ResourceData::AnimationSet(set) => {
                self.animation_viewer
                    .selected_model()
                    .and_then(|model| self.project.resource(model))
                    .and_then(|resource| match &resource.data {
                        ResourceData::Model(model) => model.skeleton,
                        _ => None,
                    })
                    == set.skeleton
            }
            _ => false,
        }
    }

    /// Select a top-level editor workspace without simulating UI input.
    ///
    /// The native frontend uses this for deterministic development startup;
    /// normal interactive workspace switching still follows the existing
    /// toolbar and shortcut paths.
    pub fn show_workspace(&mut self, workspace: EditorWorkspaceView) {
        self.active_workspace = WorkspaceView::from_project(workspace);
        self.view_2d = false;
        if self.active_workspace == WorkspaceView::Room {
            self.frame_bsp_camera_if_uninitialized();
        }
        self.status = format!("Workspace: {}", self.active_workspace.label());
    }

    /// Enter the Room workspace's Top orthographic viewport. This is kept
    /// separate from the legacy UI workspace even though both are labelled
    /// "2D" in older command-line surfaces.
    pub fn show_room_orthographic(&mut self) {
        self.active_workspace = WorkspaceView::Room;
        self.view_2d = true;
        self.orthographic_view = OrthographicView::Top;
        self.frame_bsp_viewport_if_uninitialized();
        self.status = "Viewport: Top".to_string();
    }

    /// Apply the same framing operation as the `.` viewport shortcut.
    /// Deterministic frontend capture routes use this before extracting their
    /// solid preview so the preview and editor overlays share one camera.
    pub fn frame_current_view(&mut self) {
        self.frame_viewport();
    }

    /// Open the project at `dir`. Errors when `dir/project.ron` is
    /// missing or malformed -- the frontend wraps the error and falls
    /// back to the bundled BSP starter so a real load failure surfaces
    /// in the status bar instead of silently spawning a fresh
    /// starter (which masked the path-resolution bug previously).
    pub fn open_directory(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let dir = dir.into();
        let (project, sync_status, dirty) = load_project_with_starter_catalogue(&dir)?;
        let mut workspace = Self::with_project(dir, project);
        workspace.dirty = dirty;
        workspace.status =
            sync_status.unwrap_or_else(|| format!("Loaded {}", short_path(&workspace.project_dir)));
        workspace.select_first_room();
        #[cfg(debug_assertions)]
        {
            workspace.focus_debug_scene_node_from_env();
            workspace.apply_debug_terrain_tool_from_env();
        }
        workspace.apply_project_editor_camera();
        workspace.frame_bsp_camera_if_uninitialized();
        workspace.apply_project_editor_visibility();
        workspace.apply_project_editor_viewport();
        if workspace.active_workspace == WorkspaceView::Room && workspace.view_2d {
            let status = workspace.status.clone();
            workspace.frame_bsp_viewport_if_uninitialized();
            workspace.status = status;
        }
        Ok(workspace)
    }

    /// Construct a workspace around an already-loaded project. Used
    /// by `open_directory` and `create_and_open_project`; not part
    /// of the public API.
    fn with_project(project_dir: PathBuf, project: ProjectDocument) -> Self {
        let saved_project_name = project.name.clone();
        let editor_camera = project.editor_camera;
        let editor_visibility = project.editor_visibility;
        let editor_workspace = project.editor_workspace;
        let camera_mode = match editor_camera.mode {
            EditorCameraMode::Orbit => ViewportCameraMode::Orbit,
            EditorCameraMode::Free => ViewportCameraMode::Free,
        };
        let free_initialized =
            editor_camera.free_initialized || editor_camera.mode == EditorCameraMode::Free;
        let free_position = if editor_camera.free_initialized {
            editor_camera.free_position
        } else {
            orbit_camera_position_i32(
                editor_camera.orbit_yaw_q12,
                editor_camera.orbit_pitch_q12,
                editor_camera.orbit_radius,
                editor_camera.orbit_target,
            )
        };
        let selected_ui_node = project
            .active_ui_scene()
            .map(|scene| scene.root)
            .unwrap_or(UiNodeId::ROOT);
        let prefab_library = load_prefab_library().unwrap_or_default();
        let project_watch = ProjectWatchState::capture(&project_dir, &project);
        Self {
            project,
            project_dir,
            saved_project_name,
            project_name_editing: false,
            project_name_focus_pending: false,
            modal: Modal::None,
            selection: SelectionState {
                selected_node: NodeId::ROOT,
                selected_ui_node,
                selected_nodes: HashSet::new(),
                node_selection_anchor: None,
                selected_resource: None,
                selected_prefab: None,
                selected_resources: HashSet::new(),
                resource_selection_anchor: None,
                selected_sector: None,
                selected_sectors: HashSet::new(),
                sector_selection_anchor: None,
                hovered_primitive: None,
                hovered_brush_handle: None,
                selected_primitive: None,
                selected_primitives: Vec::new(),
                hovered_entity_node: None,
            },
            interaction: Interaction::Idle,
            selection_mode: SelectionMode::default(),
            transform_gizmo_mode: TransformGizmoMode::Move,
            gizmo_space: GizmoSpace::Global,
            ui_transform_mode: UiTransformMode::Move,
            horizontal_edit_mode: HorizontalEditMode::default(),
            vertex_connectivity: VertexConnectivity::default(),
            validation_issue_primitives: Vec::new(),
            validation_issue_rooms: HashSet::new(),
            last_cook_errors: Vec::new(),
            floating_geometry: None,
            prefab_name: String::new(),
            prefab_library,
            paint_target_preview: None,
            last_paint_stamp: None,
            wall_paint_shape: WallPaintShape::Cardinal,
            renaming: None,
            pending_rename_focus: false,
            active_ui_scene_index: 0,
            active_scene_state_index: 0,
            active_floor: 0,
            ui_nav_preview: false,
            ui_center_snap: true,
            screen_offset_sim_px: 0,
            ui_nav_focus: None,
            ui_scene_renaming: None,
            ui_scene_rename_focus_pending: false,
            ui_scene_delete_confirm: None,
            collapsed_scene_nodes: HashSet::new(),
            collapsed_file_folders: HashSet::new(),
            hidden_scene_nodes: HashSet::new(),
            hidden_ui_nodes: HashSet::new(),
            ui_node_clipboard: None,
            history: UndoStack::default(),
            inspector_undo_transaction: None,
            scene_filter: String::new(),
            file_filter: String::new(),
            left_dock_scene_fraction: LEFT_DOCK_DEFAULT_SCENE_FRACTION,
            resource_search: String::new(),
            resource_filter: ResourceFilter::All,
            resource_renaming: None,
            resource_delete_confirm: None,
            active_tool: ViewTool::Select,
            place_kind: PlaceKind::PlayerSpawn,
            portal_place_direction: GridDirection::North,
            place_resource: None,
            brush_material: None,
            material_paint_blend: false,
            material_paint_sampling: false,
            brush_face_paint_stroke: false,
            selected_brush: None,
            selected_brushes: Vec::new(),
            selected_brush_face: None,
            selected_brush_elements: Vec::new(),
            brush_edit_mode: BrushEditMode::Move,
            brush_element_transform: None,
            brush_drag: None,
            brush_extrude: None,
            brush_clip_points: Vec::new(),
            brush_clip_keep: BrushClipKeep::Both,
            brush_texture_lock: true,
            brush_move: None,
            brush_uv_edit: None,
            brush_vertex_drag: None,
            material_paint_blend_coverage_percent: 50,
            material_paint_blend_edge_detail: 20,
            water_tool_mode: WaterToolMode::Add,
            snap_to_grid: true,
            snap_units: 16,
            show_grid: editor_visibility.show_grid,
            show_portals: editor_visibility.show_portals,
            show_lights: editor_visibility.show_lights,
            preview_fog: editor_visibility.preview_fog,
            preview_backface_wireframe: editor_visibility.preview_backface_wireframe,
            preview_bounds: editor_visibility.preview_bounds,
            show_play_debug_overlays: editor_visibility.show_play_debug_overlays,
            show_play_debug_map: editor_visibility.show_play_debug_map,
            show_brush_wireframes: editor_visibility.show_brush_wireframes,
            play_debug_map_view: PlayDebugMapView::default(),
            play_frame_times_ms: VecDeque::with_capacity(PLAY_FRAME_HISTORY_CAP),
            play_frame_last_sample_serial: None,
            play_debug_terminal_lines: VecDeque::with_capacity(PLAY_DEBUG_TERMINAL_LINE_CAP),
            shortcut_group_flash: None,
            // The Room workspace still uses the bit-faithful 3D preview by
            // default, but the top-level workspace itself is restored from
            // the project so UI/Animation authoring reopens where it left off.
            view_2d: false,
            orthographic_view: OrthographicView::Top,
            orthographic_focus: [0.0; 3],
            active_workspace: WorkspaceView::from_project(editor_workspace.active),
            left_dock_open: true,
            inspector_open: true,
            resources_open: true,
            content_browser_view: ContentBrowserView::Resources,
            viewport_zoom: DEFAULT_VIEWPORT_ZOOM,
            last_viewport_size: Vec2::new(1280.0, 720.0),
            #[cfg(test)]
            last_orthographic_viewport_rect: Rect::NOTHING,
            #[cfg(test)]
            last_orthographic_response: None,
            camera_rig: CameraRig {
                mode: camera_mode,
                yaw: editor_camera.orbit_yaw_q12,
                pitch: editor_camera.orbit_pitch_q12,
                radius: editor_camera.orbit_radius,
                target: editor_camera.orbit_target,
                free_yaw: editor_camera.free_yaw_q12,
                free_pitch: editor_camera.free_pitch_q12,
                free_position,
                free_initialized,
                zoom_speed: editor_camera.zoom_speed,
            },
            character_motion_preview: None,
            texture_thumbs: HashMap::new(),
            psoxide_logo_texture: None,
            ui_font_textures: Vec::new(),
            model_resource_preview_texture: None,
            animation_viewer: ModelAnimationViewerState::default(),
            animation_viewer_preview_texture: None,
            material_lab: MaterialLabState::default(),
            material_lab_preview_texture: None,
            texture_import_dialog: TextureImportDialog::default(),
            model_import_dialog: ModelImportDialog::default(),
            import_retired_textures: Vec::new(),
            dirty: false,
            project_watch,
            status: "Editor ready".to_string(),
            last_playtest_budget: None,
            pending_playtest_request: None,
        }
    }

    fn current_editor_camera_state(&self) -> EditorCameraState {
        EditorCameraState {
            mode: match self.camera_rig.mode {
                ViewportCameraMode::Orbit => EditorCameraMode::Orbit,
                ViewportCameraMode::Free => EditorCameraMode::Free,
            },
            orbit_yaw_q12: self.camera_rig.yaw,
            orbit_pitch_q12: self.camera_rig.pitch,
            orbit_radius: self.camera_rig.radius,
            orbit_target: self.camera_rig.target,
            free_yaw_q12: self.camera_rig.free_yaw,
            free_pitch_q12: self.camera_rig.free_pitch,
            free_position: if self.camera_rig.free_initialized {
                self.camera_rig.free_position
            } else {
                [0, 0, 0]
            },
            free_initialized: self.camera_rig.free_initialized,
            zoom_speed: self.camera_rig.zoom_speed,
        }
    }

    fn persist_editor_camera_state(&mut self) {
        let mut editor_camera = self.current_editor_camera_state();
        editor_camera.normalize();
        if self.project.editor_camera != editor_camera {
            self.project.editor_camera = editor_camera;
            self.dirty = true;
        }
    }

    fn apply_project_editor_camera(&mut self) {
        let mut editor_camera = self.project.editor_camera;
        editor_camera.normalize();
        self.camera_rig.mode = match editor_camera.mode {
            EditorCameraMode::Orbit => ViewportCameraMode::Orbit,
            EditorCameraMode::Free => ViewportCameraMode::Free,
        };
        self.camera_rig.yaw = editor_camera.orbit_yaw_q12;
        self.camera_rig.pitch = editor_camera.orbit_pitch_q12;
        self.camera_rig.radius = editor_camera.orbit_radius;
        self.camera_rig.target = editor_camera.orbit_target;
        self.camera_rig.free_yaw = editor_camera.free_yaw_q12;
        self.camera_rig.free_pitch = editor_camera.free_pitch_q12;
        self.camera_rig.free_position = if editor_camera.free_initialized {
            editor_camera.free_position
        } else {
            orbit_camera_position_i32(
                editor_camera.orbit_yaw_q12,
                editor_camera.orbit_pitch_q12,
                editor_camera.orbit_radius,
                editor_camera.orbit_target,
            )
        };
        self.camera_rig.free_initialized =
            editor_camera.free_initialized || editor_camera.mode == EditorCameraMode::Free;
        self.camera_rig.zoom_speed = editor_camera.zoom_speed;
    }

    fn current_editor_visibility_state(&self) -> EditorVisibilityState {
        EditorVisibilityState {
            show_grid: self.show_grid,
            show_portals: self.show_portals,
            show_lights: self.show_lights,
            preview_fog: self.preview_fog,
            preview_backface_wireframe: self.preview_backface_wireframe,
            preview_bounds: self.preview_bounds,
            show_play_debug_overlays: self.show_play_debug_overlays,
            show_play_debug_map: self.show_play_debug_map,
            show_brush_wireframes: self.show_brush_wireframes,
        }
    }

    fn persist_editor_visibility_state(&mut self) {
        let editor_visibility = self.current_editor_visibility_state();
        if self.project.editor_visibility != editor_visibility {
            self.project.editor_visibility = editor_visibility;
            self.dirty = true;
        }
    }

    fn apply_project_editor_visibility(&mut self) {
        let editor_visibility = self.project.editor_visibility;
        self.show_grid = editor_visibility.show_grid;
        self.show_portals = editor_visibility.show_portals;
        self.show_lights = editor_visibility.show_lights;
        self.preview_fog = editor_visibility.preview_fog;
        self.preview_backface_wireframe = editor_visibility.preview_backface_wireframe;
        self.preview_bounds = editor_visibility.preview_bounds;
        self.show_play_debug_overlays = editor_visibility.show_play_debug_overlays;
        self.show_play_debug_map = editor_visibility.show_play_debug_map;
        self.show_brush_wireframes = editor_visibility.show_brush_wireframes;
    }

    fn current_editor_workspace_state(&self) -> EditorWorkspaceState {
        EditorWorkspaceState {
            active: self.active_workspace.to_project(),
        }
    }

    fn current_editor_viewport_state(&self) -> psxed_project::EditorViewportState {
        psxed_project::EditorViewportState {
            view_2d: self.view_2d,
            orthographic_view: match self.orthographic_view {
                OrthographicView::Top => psxed_project::EditorOrthographicView::Top,
                OrthographicView::Front => psxed_project::EditorOrthographicView::Front,
                OrthographicView::Side => psxed_project::EditorOrthographicView::Side,
            },
            orthographic_focus: self.orthographic_focus,
            viewport_zoom: self.viewport_zoom,
            snap_units: self.snap_units,
        }
    }

    fn persist_editor_viewport_state(&mut self) {
        let editor_viewport = self.current_editor_viewport_state();
        if self.project.editor_viewport != editor_viewport {
            self.project.editor_viewport = editor_viewport;
            self.dirty = true;
        }
    }

    fn apply_project_editor_viewport(&mut self) {
        let editor_viewport = self.project.editor_viewport;
        self.view_2d = editor_viewport.view_2d;
        self.orthographic_view = match editor_viewport.orthographic_view {
            psxed_project::EditorOrthographicView::Top => OrthographicView::Top,
            psxed_project::EditorOrthographicView::Front => OrthographicView::Front,
            psxed_project::EditorOrthographicView::Side => OrthographicView::Side,
        };
        self.orthographic_focus =
            editor_viewport
                .orthographic_focus
                .map(|value| if value.is_finite() { value } else { 0.0 });
        self.viewport_zoom = if editor_viewport.viewport_zoom.is_finite() {
            editor_viewport
                .viewport_zoom
                .clamp(MIN_VIEWPORT_ZOOM, MAX_VIEWPORT_ZOOM)
        } else {
            DEFAULT_VIEWPORT_ZOOM
        };
        self.snap_units = editor_viewport.snap_units.max(1);
    }

    fn persist_editor_workspace_state(&mut self) {
        let editor_workspace = self.current_editor_workspace_state();
        if self.project.editor_workspace != editor_workspace {
            self.project.editor_workspace = editor_workspace;
            self.dirty = true;
        }
    }

    fn apply_project_editor_workspace(&mut self) {
        self.active_workspace = WorkspaceView::from_project(self.project.editor_workspace.active);
    }

    /// Current project document.
    pub fn project(&self) -> &ProjectDocument {
        &self.project
    }

    /// Change the project-authoritative BSP cook policy used by Build, Play,
    /// Rebuild, and the CLI cooker after the project is saved.
    pub fn set_bsp_cook_mode(&mut self, mode: psxed_project::brush_world::BrushWorldCookMode) {
        if self.project.bsp_cook_mode != mode {
            self.project.bsp_cook_mode = mode;
            self.last_playtest_budget = None;
            self.mark_dirty();
        }
    }

    /// Exact post-cook report, or the most recent pre-cook estimate when the
    /// cook failed before producing a package.
    pub fn playtest_budget_report(&self) -> Option<&psxed_project::playtest::PlaytestBudgetReport> {
        self.last_playtest_budget.as_ref()
    }

    /// Directory containing this project on disk.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// Directory project-relative resource paths resolve against --
    /// always the project's own directory now that every project
    /// owns one. No cwd fallback.
    pub fn project_root(&self) -> &Path {
        &self.project_dir
    }

    /// True when the project has unsaved edits.
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Save to `<project_dir>/project.ron`, retargeting the project
    /// directory when the user-facing project name changes.
    pub fn save(&mut self) -> Result<(), String> {
        // The periodic watcher keeps the UI current, but Save itself is the
        // safety boundary. Force one last signature check so an external edit
        // inside the polling interval can never be overwritten.
        self.poll_project_watch(true);
        if self.project_watch.dirty_conflict {
            return Err(
                "project.ron changed outside PSoXide while this project has unsaved edits; Reload to accept the disk version before saving"
                    .to_string(),
            );
        }
        self.persist_editor_camera_state();
        self.persist_editor_visibility_state();
        self.persist_editor_workspace_state();
        self.persist_editor_viewport_state();
        if self.floating_geometry.is_some() {
            return Err("Place or cancel the duplicate preview before saving".to_string());
        }
        let trimmed_name = self.project.name.trim().to_string();
        if trimmed_name.is_empty() {
            return Err("Project name cannot be empty".to_string());
        }
        if trimmed_name != self.project.name {
            self.project.name = trimmed_name;
        }
        self.retarget_project_dir_for_name()?;
        let path = self.project_dir.join("project.ron");
        self.project.normalize_loaded();
        self.project
            .save_to_path(&path)
            .map_err(|error| error.to_string())?;
        self.saved_project_name = self.project.name.clone();
        self.dirty = false;
        self.project_watch = ProjectWatchState::capture(&self.project_dir, &self.project);
        self.status = format!("Saved {}", short_path(&self.project_dir));
        Ok(())
    }

    fn retarget_project_dir_for_name(&mut self) -> Result<(), String> {
        if self.project.name == self.saved_project_name {
            return Ok(());
        }
        let parent = self
            .project_dir
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", self.project_dir.display()))?;
        let target = parent.join(psxed_project::project_file_stem(&self.project.name));
        if paths_equivalent(&self.project_dir, &target) {
            return Ok(());
        }
        if target.exists() {
            return Err(format!("{} already exists", short_path(&target)));
        }
        if bundled_project_is_protected(&self.project_dir) {
            copy_dir_recursive(&self.project_dir, &target)
                .map_err(|error| format!("copy project directory: {error}"))?;
        } else {
            std::fs::rename(&self.project_dir, &target)
                .map_err(|error| format!("rename project directory: {error}"))?;
        }
        self.project_dir = target;
        Ok(())
    }

    /// Save only when the project contains unsaved edits.
    pub fn save_if_dirty(&mut self) -> Result<bool, String> {
        self.persist_editor_camera_state();
        self.persist_editor_visibility_state();
        self.persist_editor_workspace_state();
        self.persist_editor_viewport_state();
        if !self.dirty {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    fn save_project_from_ui(&mut self) {
        if let Err(error) = self.save() {
            self.status = format!("Save failed: {error}");
        }
    }

    /// Re-read `<project_dir>/project.ron` from disk, discarding
    /// in-memory edits. Surfaces a load error in the status bar
    /// rather than failing -- the user can still keep editing the
    /// in-memory state.
    pub fn reload(&mut self) {
        if self.floating_geometry.is_some() {
            self.cancel_floating_geometry();
        }
        match load_project_with_starter_catalogue(&self.project_dir) {
            Ok((project, sync_status, dirty)) => {
                self.saved_project_name = project.name.clone();
                self.project = project;
                self.selection.selected_node = NodeId::ROOT;
                self.selection.selected_nodes.clear();
                self.selection.node_selection_anchor = None;
                self.selection.selected_resource = None;
                self.selection.selected_resources.clear();
                self.selection.resource_selection_anchor = None;
                self.place_resource = None;
                self.clear_sector_selection();
                self.ui_node_clipboard = None;
                self.resource_renaming = None;
                self.resource_delete_confirm = None;
                self.dirty = dirty;
                self.project_watch = ProjectWatchState::capture(&self.project_dir, &self.project);
                self.status = sync_status
                    .unwrap_or_else(|| format!("Reloaded {}", short_path(&self.project_dir)));
                self.select_first_room();
                self.apply_project_editor_camera();
                self.frame_bsp_camera_if_uninitialized();
                self.apply_project_editor_visibility();
                self.apply_project_editor_workspace();
                self.apply_project_editor_viewport();
                if self.active_workspace == WorkspaceView::Room && self.view_2d {
                    let status = self.status.clone();
                    self.frame_bsp_viewport_if_uninitialized();
                    self.status = status;
                }
            }
            Err(error) => {
                self.status = format!("Reload failed: {error}");
            }
        }
    }

    fn poll_project_watch(&mut self, force: bool) {
        if !force && self.project_watch.last_poll.elapsed() < PROJECT_WATCH_POLL_INTERVAL {
            return;
        }
        let project_path = self.project_dir.join("project.ron");
        // project.ron is the small authoritative document, so hash it on
        // every bounded poll. Metadata alone cannot detect an atomic replace
        // that preserves length and timestamp on a coarse filesystem. Runtime
        // resources remain metadata/identity-only below; raw model and
        // animation sources are still outside the watch set entirely.
        let project_signature = watched_project_signature(&project_path);
        let project_changed = project_signature.hash != self.project_watch.project.hash;
        self.project_watch.project = project_signature;
        let resource_signatures = watched_resource_signatures(&self.project_dir, &self.project);
        let resource_paths_changed = resource_signatures
            .keys()
            .ne(self.project_watch.resources.keys());
        let resources_changed =
            !resource_paths_changed && resource_signatures != self.project_watch.resources;
        self.project_watch.last_poll = Instant::now();

        if project_changed {
            self.project_watch.resources = resource_signatures;
            if self.dirty {
                self.project_watch.dirty_conflict = true;
                self.status = "External project.ron change detected; local edits preserved and Save blocked until Reload"
                    .to_string();
            } else {
                self.reload();
            }
            return;
        }

        if resource_paths_changed {
            // The in-memory project changed which files it references, for
            // example after an import or resource-path edit. Rebase without
            // claiming that an external process changed a file.
            self.project_watch.resources = resource_signatures;
        } else if resources_changed {
            self.project_watch.resources = resource_signatures;
            for entry in self.texture_thumbs.values_mut() {
                entry.signature.clear();
            }
            self.status = "Reloaded externally changed project resources".to_string();
        }
    }

    /// Whether an external project.ron edit is protected from an ordinary
    /// Save by the dirty-document conflict latch.
    pub const fn has_external_project_conflict(&self) -> bool {
        self.project_watch.dirty_conflict
    }

    /// Create `editor/projects/<derived-name>/` by recursive-copy of the
    /// buildable BSP-first template, then switch this workspace to it. The
    /// legacy default project remains a compatibility sample, not the source
    /// for newly authored levels.
    ///
    /// Validates `name`: non-empty and target directory must not
    /// already exist. On success the workspace points at the new project; on
    /// failure the workspace is unchanged.
    pub fn create_and_open_project(&mut self, name: &str) -> Result<(), String> {
        self.create_and_open_project_with_mode(
            name,
            psxed_project::brush_world::BrushWorldCookMode::Draft,
        )
    }

    /// New Project variant used by the dialog's explicit BSP quality choice.
    pub fn create_and_open_project_with_mode(
        &mut self,
        name: &str,
        cook_mode: psxed_project::brush_world::BrushWorldCookMode,
    ) -> Result<(), String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("Project name cannot be empty".to_string());
        }
        let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(trimmed));
        if target.exists() {
            return Err(format!("{} already exists", short_path(&target)));
        }
        copy_dir_recursive(&psxed_project::new_project_template_dir(), &target)
            .map_err(|error| format!("copy BSP starter project: {error}"))?;
        let mut opened = Self::open_directory(&target)?;
        opened.project.name = trimmed.to_string();
        opened.project.bsp_cook_mode = cook_mode;
        // Keep the template's deliberately authored courtyard 3D camera so a
        // new author can switch from the initial top view to a useful overview
        // of the roofless blockout without first recovering the camera.
        // New BSP maps open where blockout work begins: the top orthographic
        // viewport with the brush tool active. Existing projects retain their
        // saved workspace/camera state when opened normally.
        opened.active_workspace = WorkspaceView::Room;
        opened.view_2d = true;
        opened.orthographic_view = OrthographicView::Top;
        opened.active_tool = ViewTool::Brush;
        opened.frame_viewport();
        opened.mark_dirty();
        opened.save()?;
        opened.retire_egui_textures(self.drain_live_egui_textures());
        *self = opened;
        self.status = format!("Created {}", short_path(&self.project_dir));
        Ok(())
    }

    /// Switch to another project directory, preserving live egui
    /// texture handles long enough for the current frame to finish.
    fn switch_project(&mut self, dir: impl Into<PathBuf>) -> Result<(), String> {
        let target = dir.into();
        let mut opened = Self::open_directory(&target)?;
        opened.retire_egui_textures(self.drain_live_egui_textures());
        *self = opened;
        Ok(())
    }

    fn open_project_from_menu(&mut self, path: &Path) {
        if paths_equivalent(&self.project_dir, path) {
            self.status = format!("Already loaded {}", short_path(path));
            return;
        }
        if let Err(error) = self.switch_project(path.to_path_buf()) {
            self.status = format!("Open project failed: {error}");
        }
    }

    fn current_project_is_bundled(&self) -> bool {
        bundled_project_is_protected(&self.project_dir)
    }

    fn delete_project_fallback_dir(delete_dir: &Path) -> Result<PathBuf, String> {
        let bsp_starter = psxed_project::new_project_template_dir();
        if !paths_equivalent(delete_dir, &bsp_starter) && bsp_starter.join("project.ron").is_file()
        {
            return Ok(bsp_starter);
        }
        let projects =
            psxed_project::list_projects().map_err(|error| format!("list projects: {error}"))?;
        projects
            .into_iter()
            .find(|path| !paths_equivalent(path, delete_dir))
            .ok_or_else(|| "Cannot delete the last available project".to_string())
    }

    fn delete_current_project(&mut self) -> Result<(), String> {
        let delete_dir = self.project_dir.clone();
        if self.current_project_is_bundled() {
            return Err("Bundled starter projects cannot be deleted".to_string());
        }
        let projects_root = psxed_project::projects_dir();
        let parent = delete_dir
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", delete_dir.display()))?;
        if !paths_equivalent(parent, &projects_root) {
            // The guard is the resolved projects root, not a literal path, so
            // it follows `projects_dir()` into a shipped build's per-user data
            // directory. Report the root it actually enforced.
            return Err(format!(
                "Only projects in {} can be deleted",
                projects_root.display()
            ));
        }
        let fallback_dir = Self::delete_project_fallback_dir(&delete_dir)?;
        let mut opened = Self::open_directory(&fallback_dir)?;
        let deleted_name = self.project.name.clone();
        std::fs::remove_dir_all(&delete_dir)
            .map_err(|error| format!("delete {}: {error}", delete_dir.display()))?;
        opened.retire_egui_textures(self.drain_live_egui_textures());
        *self = opened;
        self.status = format!(
            "Deleted {deleted_name}; loaded {}",
            short_path(&self.project_dir)
        );
        Ok(())
    }

    fn draw_project_switch_menu(&mut self, ui: &mut egui::Ui) {
        match psxed_project::list_projects() {
            Ok(projects) if projects.is_empty() => {
                ui.weak("No projects found");
            }
            Ok(projects) => {
                for path in projects {
                    let current = paths_equivalent(&self.project_dir, &path);
                    let label = project_menu_label(&path);
                    if ui.selectable_label(current, label).clicked() {
                        self.open_project_from_menu(&path);
                        ui.close_menu();
                    }
                }
            }
            Err(error) => {
                ui.weak(format!("Could not list projects: {error}"));
            }
        }
    }

    /// Select the first Room in the active scene if one exists.
    /// Default state is `selected_node = ROOT`, which leaves the
    /// inspector empty and gates the paint tools -- selecting a
    /// concrete Room straight after construction or load makes the
    /// editor immediately useful for the common case (one Room per
    /// project).
    fn select_first_room(&mut self) {
        if let Some(room_id) = self
            .project
            .active_scene()
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Section { .. }))
            .map(|node| node.id)
        {
            self.replace_node_selection(room_id);
            self.frame_3d_on_room(room_id);
        }
    }

    /// Debug-only focus hook used by deterministic native-editor captures.
    /// Normal editor startup remains unchanged unless the environment variable
    /// is explicitly provided to a debug build.
    #[cfg(debug_assertions)]
    fn focus_debug_scene_node_from_env(&mut self) {
        let Ok(selector) = std::env::var("PSXED_DEBUG_FOCUS_NODE") else {
            return;
        };
        if !self.focus_scene_node_for_debug(&selector) {
            return;
        }
        let Ok(action) = std::env::var("PSXED_DEBUG_PREVIEW_ACTION") else {
            return;
        };
        let action = action.trim();
        if let Some(action) = psxed_project::CharacterAnimationAction::ALL
            .into_iter()
            .find(|candidate| candidate.label().eq_ignore_ascii_case(action))
        {
            self.preview_character_action(self.selection.selected_node, action);
        }
    }

    /// Optional material-paint tool state for deterministic native captures.
    /// Kept behind debug assertions so production startup never reads these
    /// development-only environment variables.
    #[cfg(debug_assertions)]
    fn apply_debug_terrain_tool_from_env(&mut self) {
        let Ok(tool) = std::env::var("PSXED_DEBUG_TOOL") else {
            return;
        };
        let tool = match tool.trim().to_ascii_lowercase().as_str() {
            "floor" => ViewTool::PaintFloor,
            "ceiling" => ViewTool::PaintCeiling,
            "wall" => ViewTool::PaintWall,
            "paint" | "material" => ViewTool::PaintMaterial,
            "water" => ViewTool::Water,
            _ => return,
        };
        if self.active_room_id().is_none() {
            return;
        }
        self.active_tool = tool;
        self.material_paint_blend = std::env::var("PSXED_DEBUG_PAINT_BLEND")
            .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"));
        self.material_paint_sampling = tool == ViewTool::PaintMaterial
            && std::env::var("PSXED_DEBUG_PAINT_SAMPLE")
                .is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"));
        if tool == ViewTool::Water {
            self.water_tool_mode = match std::env::var("PSXED_DEBUG_WATER_MODE")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "erase" => WaterToolMode::Erase,
                "select" => WaterToolMode::Select,
                _ => WaterToolMode::Add,
            };
        }
        self.status = if tool == ViewTool::Water {
            format!("Water: {:?}", self.water_tool_mode)
        } else if self.material_paint_sampling {
            "Eyedropper: click a surface to sample its material".to_string()
        } else if self.material_paint_blend {
            "Material Paint: Blend".to_string()
        } else {
            "Material Paint: Direct".to_string()
        };
    }

    #[cfg(debug_assertions)]
    fn focus_scene_node_for_debug(&mut self, selector: &str) -> bool {
        let selector = selector.trim();
        if selector.is_empty() {
            return false;
        }
        let numeric_id = selector.parse::<u64>().ok();
        let scene = self.project.active_scene();
        let target = scene
            .nodes()
            .iter()
            .find(|node| numeric_id.is_some_and(|id| node.id.raw() == id) || node.name == selector)
            .or_else(|| {
                scene
                    .nodes()
                    .iter()
                    .find(|node| node.name.eq_ignore_ascii_case(selector))
            })
            .map(|node| node.id);
        let Some(target) = target else {
            return false;
        };
        self.replace_node_selection(target);
        self.clear_resource_selection_state();
        self.clear_primitive_selection_state();
        self.clear_sector_selection();
        true
    }

    /// Position the orbit camera so `room_id`'s grid fills the
    /// viewport at startup. Pulls back to ~1.6× the room diagonal
    /// in world units, which lands a 3/4 view that shows all four
    /// walls plus the floor without the corners clipping.
    fn frame_3d_on_room(&mut self, room_id: NodeId) {
        let Some((center, half)) = self.room_bounds_3d(room_id) else {
            return;
        };
        // Default 3/4 view: yaw 8/64 turn (45° off the +Z axis),
        // pitch 4/64 (~22° looking down). Mirrors the showcase
        // demos' first-frame angle.
        self.camera_rig.yaw = 256;
        self.camera_rig.pitch = 256;
        self.fit_3d_bounds(center, half);
    }

    /// Move the 3D orbit target onto `center` and choose a radius
    /// that fits `half` without changing yaw/pitch. Used for startup
    /// and explicit whole-room framing.
    fn fit_3d_bounds(&mut self, center: [f32; 3], half: [f32; 3]) {
        self.camera_rig.target = [
            round_to_i32(center[0]),
            round_to_i32(center[1]),
            round_to_i32(center[2]),
        ];
        self.camera_rig.radius = frame_radius_for_3d_bounds(half);
        if self.camera_rig.mode == ViewportCameraMode::Free {
            self.camera_rig.sync_free_to_orbit();
        }
    }

    fn room_bounds_3d(&self, room_id: NodeId) -> Option<([f32; 3], [f32; 3])> {
        let scene = self.project.active_scene();
        let room = scene.node(room_id)?;
        let NodeKind::Section { grid } = &room.kind else {
            return None;
        };
        let footprint = grid.authored_footprint()?;
        let x0 = grid.cell_world_x(footprint.x) as f32;
        let x1 = grid.cell_world_x(footprint.end_x()) as f32;
        let z0 = grid.cell_world_z(footprint.z) as f32;
        let z1 = grid.cell_world_z(footprint.end_z()) as f32;
        let mut min_y: f32 = 0.0;
        let mut max_y = grid.sector_size as f32;
        for sx in footprint.x..footprint.end_x() {
            for sz in footprint.z..footprint.end_z() {
                let Some(sector) = grid.sector(sx, sz) else {
                    continue;
                };
                if !sector.has_geometry() {
                    continue;
                }
                if let Some(face) = &sector.floor {
                    for y in face.heights {
                        min_y = min_y.min(y as f32);
                        max_y = max_y.max(y as f32);
                    }
                }
                if let Some(face) = &sector.ceiling {
                    for y in face.heights {
                        min_y = min_y.min(y as f32);
                        max_y = max_y.max(y as f32);
                    }
                }
                for dir in GridDirection::ALL {
                    for wall in sector.walls.get(dir) {
                        for y in wall.heights {
                            min_y = min_y.min(y as f32);
                            max_y = max_y.max(y as f32);
                        }
                    }
                }
            }
        }
        Some((
            [(x0 + x1) * 0.5, (min_y + max_y) * 0.5, (z0 + z1) * 0.5],
            [
                (x1 - x0).abs() * 0.5,
                ((max_y - min_y).abs() * 0.5).max(64.0),
                (z1 - z0).abs() * 0.5,
            ],
        ))
    }

    fn sector_bounds_3d(&self, room_id: NodeId, sx: u16, sz: u16) -> Option<([f32; 3], [f32; 3])> {
        let scene = self.project.active_scene();
        let room = scene.node(room_id)?;
        let NodeKind::Section { grid } = &room.kind else {
            return None;
        };
        Self::sector_bounds_3d_for_grid(grid, sx, sz)
    }

    fn sector_bounds_3d_for_grid(
        grid: &WorldGrid,
        sx: u16,
        sz: u16,
    ) -> Option<([f32; 3], [f32; 3])> {
        if sx >= grid.width || sz >= grid.depth {
            return None;
        }
        let cell = grid.cell_bounds_world(sx, sz);
        let mut min_y = 0;
        let mut max_y = grid.sector_size;
        if let Some(sector) = grid.sector(sx, sz) {
            if let Some(face) = &sector.floor {
                for y in face.heights {
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
            if let Some(face) = &sector.ceiling {
                for y in face.heights {
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
            for dir in GridDirection::ALL {
                for wall in sector.walls.get(dir) {
                    for y in wall.heights {
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                    }
                }
            }
        }
        let min_y = min_y as f32;
        let max_y = max_y as f32;
        Some((
            [
                (cell.x0 + cell.x1) as f32 * 0.5,
                (min_y + max_y) * 0.5,
                (cell.z0 + cell.z1) as f32 * 0.5,
            ],
            [
                (cell.x1 - cell.x0).abs() as f32 * 0.5,
                ((max_y - min_y).abs() * 0.5).max(64.0),
                (cell.z1 - cell.z0).abs() as f32 * 0.5,
            ],
        ))
    }

    /// Cook every Room in the active scene to a per-Room `.psxw`
    /// blob under `<project_dir>/cooked/`.
    ///
    /// Returns a one-line summary on success. Fails when the project
    /// has not yet been saved (no anchor for `<project_dir>`), or
    /// when any room's grid cooker rejects its inputs (see
    /// `WorldGridCookError`).
    pub fn cook_world_to_disk(&mut self) -> Result<String, String> {
        self.project.normalize_loaded();
        self.clear_validation_issues();
        let cooked_dir = self.project_dir.join("cooked");
        std::fs::create_dir_all(&cooked_dir)
            .map_err(|error| format!("mkdir {}: {error}", cooked_dir.display()))?;

        let scene = self.project.active_scene();
        let rooms: Vec<(NodeId, String)> = scene
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
            .map(|node| (node.id, node.name.clone()))
            .collect();
        if rooms.is_empty() {
            return Err("No Section nodes in the active scene".to_string());
        }

        let mut total_bytes = 0usize;
        let mut written = 0usize;
        for (room_id, room_name) in &rooms {
            let cook_result = {
                let scene = self.project.active_scene();
                let Some(node) = scene.node(*room_id) else {
                    continue;
                };
                let NodeKind::Section { grid } = &node.kind else {
                    continue;
                };
                psxed_project::world_cook::encode_world_grid_psxw(&self.project, grid)
            };
            let bytes = match cook_result {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.record_world_cook_error(*room_id, &error, [0, 0]);
                    return Err(format!("cook \"{room_name}\": {error}"));
                }
            };
            let filename = sanitise_room_filename(room_name);
            let path = cooked_dir.join(format!("{filename}.psxw"));
            std::fs::write(&path, &bytes)
                .map_err(|error| format!("write {}: {error}", path.display()))?;
            total_bytes += bytes.len();
            written += 1;
        }

        Ok(format!(
            "Cooked {} room{} ({} KiB) into {}",
            written,
            if written == 1 { "" } else { "s" },
            total_bytes / 1024,
            cooked_dir.display(),
        ))
    }

    /// Cook the active project into the playtest example's
    /// `generated/` directory. Validates the scene tree, cooks
    /// every populated Room into `rooms/room_NNN.psxw`, and
    /// writes a fresh ignored cooked manifest. Returns a status
    /// string suitable for `self.status`. The "& Play" half is
    /// up to the caller -- the editor doesn't spawn child
    /// processes from this path; instead the status string
    /// hands back the exact command to run.
    pub fn cook_playtest_to_disk(&mut self) -> Result<String, String> {
        let dir = psxed_project::playtest::default_generated_dir();
        self.cook_playtest_to_dir(&dir)
    }

    /// Same cook as [`Self::cook_playtest_to_disk`], with an explicit output
    /// directory for deterministic tests and host integrations.
    pub fn cook_playtest_to_dir(&mut self, dir: &Path) -> Result<String, String> {
        let mut project = self.project.clone();
        project.normalize_loaded();
        self.clear_validation_issues();
        self.last_playtest_budget = Some(psxed_project::playtest::estimate_playtest_budgets(
            &project,
            &self.project_dir,
        ));
        // Build once: Cortex-sized projects have enough materials, animation,
        // world geometry, and portal topology that doing this again merely for
        // the status summary makes every Play launch needlessly expensive.
        let (package, report) = psxed_project::playtest::build_package(&project, &self.project_dir);
        let summary = package.as_ref().map(|p| PackageSummary {
            rooms: p.rooms.len(),
            assets: p.assets.len(),
            textures: p.texture_asset_count(),
            materials: p.materials.len(),
            models: p.models.len(),
            characters: p.characters.len(),
            lights: p.lights.len(),
            entities: p.entities.len(),
            player_character: p
                .player_controller
                .and_then(|pc| p.characters.get(pc.character as usize))
                .and_then(|c| project.resource(c.source_resource).map(|r| r.name.clone())),
        });
        if let Some(package) = package.as_ref() {
            self.last_playtest_budget = Some(psxed_project::playtest::cooked_playtest_budgets(
                &project, package,
            ));
        }

        psxed_project::playtest::write_cook_result(package.as_ref(), dir)
            .map_err(|e| format!("write playtest output: {e}"))?;
        if !report.is_ok() {
            self.last_cook_errors = report.errors.clone();
            if !report
                .focus_target()
                .is_some_and(|target| self.focus_playtest_validation_target(target))
            {
                self.record_first_playtest_world_cook_issue(&project);
            }
            return Err(format!(
                "playtest validation failed: {}",
                report.error_messages().join("; ")
            ));
        }
        let warning_suffix = if report.warnings.is_empty() {
            String::new()
        } else {
            format!(
                " ({} warning{})",
                report.warnings.len(),
                if report.warnings.len() == 1 { "" } else { "s" }
            )
        };
        let counts = summary
            .as_ref()
            .map(|s| {
                let player_blurb = match s.player_character.as_deref() {
                    Some(name) => format!(", player: {name}"),
                    None => ", no player".to_string(),
                };
                format!(
                    " - {} room{}, {} model{}, {} character{}{}, {} light{}, {} asset{}, {} texture{}, {} material{}, {} entit{}",
                    s.rooms,
                    if s.rooms == 1 { "" } else { "s" },
                    s.models,
                    if s.models == 1 { "" } else { "s" },
                    s.characters,
                    if s.characters == 1 { "" } else { "s" },
                    player_blurb,
                    s.lights,
                    if s.lights == 1 { "" } else { "s" },
                    s.assets,
                    if s.assets == 1 { "" } else { "s" },
                    s.textures,
                    if s.textures == 1 { "" } else { "s" },
                    s.materials,
                    if s.materials == 1 { "" } else { "s" },
                    s.entities,
                    if s.entities == 1 { "y" } else { "ies" },
                )
            })
            .unwrap_or_default();
        let budget = self
            .last_playtest_budget
            .as_ref()
            .map(|budget| format!("; {}", budget.concise_summary()))
            .unwrap_or_default();
        let budget_issue = self
            .last_playtest_budget
            .as_ref()
            .and_then(|budget| budget.first_actionable_issue())
            .cloned();
        if let Some(issue) = budget_issue {
            if let Some(target) = issue.target {
                let _ = self.focus_playtest_validation_target(target);
            }
        }
        Ok(format!(
            "Playtest cooked → {}{}{}{}",
            dir.display(),
            counts,
            warning_suffix,
            budget,
        ))
    }

    /// Drain the one-shot embedded play request emitted by this
    /// frame's UI. The frontend calls this after drawing the editor
    /// and performs the actual cook/build/load/stop work.
    pub fn take_playtest_request(&mut self) -> Option<EditorPlaytestRequest> {
        self.pending_playtest_request.take()
    }

    /// Let the frontend surface embedded play status in the editor's
    /// existing status strip.
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Append guest-runtime debug lines to the bottom debug terminal.
    pub fn append_play_debug_terminal_lines<I, S>(&mut self, lines: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for line in lines {
            if self.play_debug_terminal_lines.len() >= PLAY_DEBUG_TERMINAL_LINE_CAP {
                self.play_debug_terminal_lines.pop_front();
            }
            self.play_debug_terminal_lines.push_back(line.into());
        }
    }
}

#[cfg(test)]
mod tests;
