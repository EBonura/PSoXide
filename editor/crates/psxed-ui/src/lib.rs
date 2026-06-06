//! egui editor workspace for PSoXide.
//!
//! The frontend owns the window/Menu. This crate owns the editor panels and
//! the in-memory authoring document they manipulate.

mod geometry;
mod gizmo;
mod history;
mod icons;
mod model_animation_viewer;
mod model_import_preview;
mod play_mode;
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
    EditorPlaytestMetrics, EditorPlaytestRequest, EditorPlaytestStatus, EditorPlaytestTapeMode,
    EditorPlaytestTapeStatus, EditorViewport3dMode, EditorViewport3dPresentation,
    EditorViewportOverlayLine,
};

use crate::gizmo::*;
use crate::history::UndoStack;
use crate::model_animation_viewer::ModelAnimationViewerState;
use crate::style::*;

use std::collections::{HashMap, HashSet, VecDeque};
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
#[cfg(test)]
use psxed_project::streaming::SceneResourceUse;
use psxed_project::world_cook::{self, WorldGridCookError, WorldGridFaceKind};
use psxed_project::{
    default_model_collision_radius_for_height, default_ui_font_scale, default_ui_letter_spacing,
    snap_height, ui_font_scale_f32_to_q8, ui_font_scale_q8_to_f32, BootTarget,
    CharacterControllerSettings, ColliderShape, EditorCameraMode, EditorCameraState,
    EditorVisibilityState, EditorWorkspaceState, EditorWorkspaceView, FarVistaSettings,
    GridCellBounds, GridDirection, GridHorizontalFace, GridSector, GridSplit,
    GridTriangleMaterialOverride, GridUvRotation, GridUvTransform, GridVerticalFace,
    InteractableKind, MaterialFaceSidedness, MaterialResource, NodeId, NodeKind, NodeRow, OptionId,
    OptionKind, ParticleEmitterSettings, PhysicsBodySettings, ProjectDocument, PsxBlendMode,
    Resource, ResourceData, ResourceId, RuntimeDepthSortMode, RuntimeRoomDrawOrderMode,
    RuntimeTextureSplitMode, Scene, SceneNode, SceneStateId, SceneWorldLayer, SkyMode, SkySettings,
    UiAction, UiAnchor, UiFontChoice, UiGradient, UiGradientDirection, UiImageEffect, UiNode,
    UiNodeId, UiNodeKind, UiNodeRow, UiRect, UiScene, UiSceneId, UiSfxBindings, UiSfxCue,
    UiTextAlign, UiValueBinding, WorldCameraSettings, WorldCullingSettings, WorldGrid,
    WorldPhysicsSettings, WorldStreamingSettings, DEFAULT_WALL_HEIGHT_SECTORS,
    DEFAULT_WORLD_SECTOR_SIZE, HEIGHT_QUANTUM, MAX_PHYSICS_WEIGHT_Q8, MAX_UI_FONT_SCALE,
    MAX_UI_LETTER_SPACING, MAX_WORLD_CAMERA_DISTANCE, MAX_WORLD_CAMERA_HEIGHT,
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
const MATERIAL_TEXTURE_PICKER_POPUP_WIDTH: f32 = 320.0;
const MATERIAL_TEXTURE_PICKER_POPUP_HEIGHT: f32 = 320.0;
const MATERIAL_TEXTURE_PICKER_ROW_HEIGHT: f32 = 44.0;
const MATERIAL_TEXTURE_PICKER_THUMB_SIZE: f32 = 34.0;
const MATERIAL_TEXTURE_PICKER_BUTTON_WIDTH: f32 = 190.0;
const VIEWPORT_PREVIEW_ASPECT: f32 = 320.0 / 240.0;
const SHORTCUT_GROUP_FLASH_SECONDS: f32 = 0.85;
const ACTION_BAR_COMPACT_HEIGHT: f32 = 50.0;
const ACTION_BAR_EXPANDED_HEIGHT: f32 = 74.0;
const ACTION_BAR_WRAP_STATUS_CHARS: usize = 96;
const PLAY_FRAME_HISTORY_CAP: usize = 150;
const PLAY_DEBUG_TERMINAL_LINE_CAP: usize = 1_000;
const PLAY_FRAME_TARGET_FPS: f32 = 30.0;
const PLAY_NTSC_VBLANK_MS: f32 = 1000.0 / 60.0;
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
];
const STARTER_ANIMATION_SET_NAMES: &[&str] = &[
    "Obsidian Wraith Enemy Set",
    "Crimson Cross Knight Player Set",
    "Hooded Wretch Enemy Set",
    "Crowned Wraith Enemy Set",
];
const STARTER_CHARACTER_PROFILE_NAMES: &[&str] = &[
    "Crimson Cross Knight Player",
    "Obsidian Wraith Enemy",
    "Hooded Wretch Enemy",
    "Crowned Wraith Enemy",
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
    /// Floating duplicate placement created by Cmd+D. While active,
    /// `project` contains the preview copy, but `base_project`
    /// lets Escape cancel without dirtying the document and lets
    /// click commit one clean undo step.
    floating_geometry: Option<FloatingGeometryPlacement>,
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
    scene_filter: String,
    file_filter: String,
    left_dock_scene_fraction: f32,
    resource_search: String,
    resource_filter: ResourceFilter,
    material_texture_search: String,
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
    snap_to_grid: bool,
    snap_units: u16,
    show_grid: bool,
    show_portals: bool,
    show_lights: bool,
    preview_fog: bool,
    preview_backface_wireframe: bool,
    preview_bounds: bool,
    show_play_debug_overlays: bool,
    show_play_debug_map: bool,
    play_frame_times_ms: VecDeque<f32>,
    play_frame_last_sample_serial: Option<u32>,
    play_debug_terminal_lines: VecDeque<String>,
    shortcut_group_flash: Option<(ShortcutGroup, Instant)>,
    view_2d: bool,
    active_workspace: WorkspaceView,
    left_dock_open: bool,
    inspector_open: bool,
    resources_open: bool,
    content_browser_view: ContentBrowserView,
    viewport_pan: Vec2,
    viewport_zoom: f32,
    last_viewport_size: Vec2,
    /// 3D viewport camera rig (orbit + free-fly params, mode, and all the
    /// camera math). Orbit preserves the original target/radius camera; Free
    /// stores an explicit world position with the same yaw/pitch convention.
    camera_rig: CameraRig,
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
    texture_import_dialog: TextureImportDialog,
    model_import_dialog: ModelImportDialog,
    import_retired_textures: Vec<(u8, egui::TextureHandle)>,
    dirty: bool,
    status: String,
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

#[derive(Clone)]
struct TexturePickerOption {
    id: ResourceId,
    name: String,
    thumb: Option<(egui::TextureId, PsxtStats)>,
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

enum ProjectFileRowAction {
    Select(ResourceClick),
    ToggleFolder(String),
}

struct ModelImportDialog {
    open: bool,
    source_path: String,
    animation_paths: Vec<String>,
    output_name: String,
    texture_width: i32,
    texture_height: i32,
    animation_fps: i32,
    world_height: i32,
    collision_radius: i32,
    normalize_root_translation: bool,
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
            animation_fps: 15,
            world_height: 1024,
            collision_radius: default_model_collision_radius_for_height(1024) as i32,
            normalize_root_translation: true,
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

type SectorSelection = (NodeId, u16, u16);

/// What the next paint click would target. Carries world-cell
/// coordinates (which can be negative -- outside the current grid)
/// so the renderer can preview cells the next click would auto-
/// create. Stays populated for any paint tool, mirroring the
/// dispatch so what you preview is what you'll paint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintTargetPreview {
    /// Floor / ceiling / erase / place -- outlines the cell.
    Cell {
        world_cell_x: i32,
        world_cell_z: i32,
        kind: PaintCellPreviewKind,
    },
    /// PaintWall -- outlines the wall that would be added on the
    /// targeted edge. `stack` is the next-free wall slot index for
    /// that edge, used by the renderer to position the ghost above
    /// any existing walls.
    Wall {
        world_cell_x: i32,
        world_cell_z: i32,
        dir: GridDirection,
        stack: u8,
    },
    /// Portal placement -- highlights the cardinal edge that will
    /// become an open seam. `valid` is false when either side of the
    /// edge is missing authored geometry, so the click will be
    /// rejected instead of creating a marker the cooker ignores.
    PortalEdge {
        world_cell_x: i32,
        world_cell_z: i32,
        dir: GridDirection,
        valid: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintCellPreviewKind {
    Ground,
    Floor,
    Ceiling,
}

/// One pickable surface on the active Room's grid. Floors and
/// ceilings are addressed by sector; walls add a cardinal direction
/// plus a stack index (a single edge can hold multiple stacked walls
/// -- windows / arches -- and each is independently selectable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceKind {
    Floor,
    Ceiling,
    Wall { dir: GridDirection, stack: u8 },
}

/// Horizontal surface type for triangle editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalSurfaceKind {
    Floor,
    Ceiling,
}

impl HorizontalSurfaceKind {
    const fn face_kind(self) -> FaceKind {
        match self {
            Self::Floor => FaceKind::Floor,
            Self::Ceiling => FaceKind::Ceiling,
        }
    }
}

/// Which half of a split floor/ceiling face is being addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HorizontalTriangleIndex {
    A,
    B,
}

impl HorizontalTriangleIndex {
    const fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }

    const fn idx(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}

/// One triangle half of a floor or ceiling face. The corner list
/// snapshots the split layout at pick time so downstream edit code
/// can move/outline the exact triangle the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HorizontalTriangleRef {
    pub room: NodeId,
    pub sx: u16,
    pub sz: u16,
    pub surface: HorizontalSurfaceKind,
    pub index: HorizontalTriangleIndex,
    pub corners: [Corner; 3],
}

impl HorizontalTriangleRef {
    pub const fn parent_face(self) -> FaceRef {
        FaceRef {
            room: self.room,
            sx: self.sx,
            sz: self.sz,
            kind: self.surface.face_kind(),
        }
    }

    pub const fn face_corner(self, corner: Corner) -> FaceCornerRef {
        match self.surface {
            HorizontalSurfaceKind::Floor => FaceCornerRef::Floor {
                sx: self.sx,
                sz: self.sz,
                corner,
            },
            HorizontalSurfaceKind::Ceiling => FaceCornerRef::Ceiling {
                sx: self.sx,
                sz: self.sz,
                corner,
            },
        }
    }
}

/// A face inside the active Room, fully qualified by Room id +
/// sector + face kind. Used by the Select tool's hover / selected
/// state and the per-face inspector that follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceRef {
    pub room: NodeId,
    pub sx: u16,
    pub sz: u16,
    pub kind: FaceKind,
}

// Corner / WallCorner live in `psxed-project` so faces can carry
// `dropped_corner` data with serde support. Re-exported here so
// existing imports (`use psxed_ui::Corner`) keep working.
pub use psxed_project::{Corner, WallCorner};

/// Which of the four edges of a wall quad. Order matches the
/// perimeter walk used by the picker:
/// `Bottom = BL-BR`, `Right = BR-TR`, `Top = TR-TL`,
/// `Left = TL-BL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallEdge {
    Bottom,
    Right,
    Top,
    Left,
}

/// One face-corner. `Selection::Vertex(_)` resolves through
/// [`physical_vertex`] to a `Vec<FaceCornerRef>` listing every
/// face-corner currently sharing the same world position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaceCornerRef {
    Floor {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    FloorTriangle {
        sx: u16,
        sz: u16,
        triangle: HorizontalTriangleIndex,
        corner: Corner,
    },
    Ceiling {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    CeilingTriangle {
        sx: u16,
        sz: u16,
        triangle: HorizontalTriangleIndex,
        corner: Corner,
    },
    Wall {
        sx: u16,
        sz: u16,
        dir: GridDirection,
        stack: u8,
        corner: WallCorner,
    },
}

/// Vertex in a `Selection`. Carries the *seed* corner -- the one
/// the user actually clicked. Resolve to a `PhysicalVertex` to
/// get every coincident face-corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VertexRef {
    pub room: NodeId,
    pub anchor: VertexAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAnchor {
    Floor {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    Ceiling {
        sx: u16,
        sz: u16,
        corner: Corner,
    },
    Wall {
        sx: u16,
        sz: u16,
        dir: GridDirection,
        stack: u8,
        corner: WallCorner,
    },
}

impl VertexAnchor {
    pub const fn as_face_corner(self) -> FaceCornerRef {
        match self {
            Self::Floor { sx, sz, corner } => FaceCornerRef::Floor { sx, sz, corner },
            Self::Ceiling { sx, sz, corner } => FaceCornerRef::Ceiling { sx, sz, corner },
            Self::Wall {
                sx,
                sz,
                dir,
                stack,
                corner,
            } => FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner,
            },
        }
    }
}

/// Edge in a `Selection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeRef {
    pub room: NodeId,
    pub anchor: EdgeAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeAnchor {
    Floor {
        sx: u16,
        sz: u16,
        dir: GridDirection,
    },
    Ceiling {
        sx: u16,
        sz: u16,
        dir: GridDirection,
    },
    Wall {
        sx: u16,
        sz: u16,
        dir: GridDirection,
        stack: u8,
        edge: WallEdge,
    },
}

/// Tagged selection used by the editor's Select tool. Replaces
/// the previous `selected_face: Option<FaceRef>` so all three
/// modes share one piece of state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Face(FaceRef),
    Triangle(HorizontalTriangleRef),
    Edge(EdgeRef),
    Vertex(VertexRef),
}

impl Selection {
    /// The room this selection belongs to.
    pub const fn room(&self) -> NodeId {
        match self {
            Self::Face(f) => f.room,
            Self::Triangle(t) => t.room,
            Self::Edge(e) => e.room,
            Self::Vertex(v) => v.room,
        }
    }

    /// Convenience: when the selection is a face, hand it to
    /// callers that still want the old `FaceRef` shape (e.g.
    /// the per-face inspector).
    pub const fn as_face(&self) -> Option<FaceRef> {
        match self {
            Self::Face(f) => Some(*f),
            Self::Triangle(t) => Some(t.parent_face()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaterialTarget {
    Face(FaceRef),
    Triangle(HorizontalTriangleRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoxPropMaterialAssignment {
    material: ResourceId,
    targets: usize,
    updated: usize,
}

fn world_cook_error_primitives(
    room: NodeId,
    error: &WorldGridCookError,
    array_origin: [u16; 2],
) -> Vec<Selection> {
    let face = |x: u16, z: u16, kind: WorldGridFaceKind| {
        world_cook_face_selection(
            room,
            x.saturating_add(array_origin[0]),
            z.saturating_add(array_origin[1]),
            kind,
        )
    };

    match *error {
        WorldGridCookError::UnassignedMaterial { x, z, face: kind } => vec![face(x, z, kind)],
        WorldGridCookError::InvalidWallHeights {
            x, z, direction, ..
        }
        | WorldGridCookError::UnsupportedDiagonalWall { x, z, direction }
        | WorldGridCookError::WallStackExceeded {
            x, z, direction, ..
        } => vec![face(x, z, WorldGridFaceKind::Wall(direction))],
        WorldGridCookError::DuplicatePhysicalWall {
            x,
            z,
            direction,
            other_x,
            other_z,
            other_direction,
        } => vec![
            face(x, z, WorldGridFaceKind::Wall(direction)),
            face(other_x, other_z, WorldGridFaceKind::Wall(other_direction)),
        ],
        WorldGridCookError::HeightNotQuantized {
            x, z, face: kind, ..
        }
        | WorldGridCookError::TriangleFaceNotSupported { x, z, face: kind } => {
            vec![face(x, z, kind)]
        }
        _ => Vec::new(),
    }
}

fn world_cook_face_selection(room: NodeId, sx: u16, sz: u16, kind: WorldGridFaceKind) -> Selection {
    let kind = match kind {
        WorldGridFaceKind::Floor => FaceKind::Floor,
        WorldGridFaceKind::Ceiling => FaceKind::Ceiling,
        WorldGridFaceKind::Wall(dir) => FaceKind::Wall { dir, stack: 0 },
    };
    Selection::Face(FaceRef { room, sx, sz, kind })
}

/// Kind label for an [`EntityBounds`]. Drives picking
/// priorities and per-kind gizmo rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityBoundKind {
    /// Model-backed `MeshInstance` with parsed model bounds.
    Model,
    /// Legacy / unbound `MeshInstance` -- fallback box.
    MeshFallback,
    /// Flat `ImageProp`.
    ImageProp,
    /// Editable boxed prop.
    BoxProp,
    /// `SpawnPoint` (player or non-player).
    SpawnPoint,
    /// `PointLight`. Marker box only -- radius ring is drawn
    /// separately so a wide-radius light doesn't intercept
    /// every click in the room.
    PointLight,
    /// `ParticleEmitter`.
    ParticleEmitter,
    /// `Portal`.
    Portal,
}

/// World-space AABB for one selectable scene entity.
/// Coordinates use [`psxed_project::spatial::node_preview_bounds_center`]
/// for entities under a Room, so bounds line up with the same
/// origin-aware preview world used by rendered models, markers, and
/// lights.
#[derive(Debug, Clone, Copy)]
pub struct EntityBounds {
    /// Owning scene-tree node id.
    pub node: NodeId,
    /// Enclosing Room id, if any. Used to filter picking to
    /// the active room.
    pub room: Option<NodeId>,
    /// Bound class for visual styling + picking priority.
    pub kind: EntityBoundKind,
    /// World-space AABB centre.
    pub center: [f32; 3],
    /// World-space half-extents along X / Y / Z. Always
    /// positive.
    pub half_extents: [f32; 3],
    /// Authored Y rotation in degrees. Stored on the bound
    /// so the renderer can draw a facing arrow without
    /// re-walking the scene tree.
    pub yaw_degrees: f32,
}

/// Result of a successful entity-bound pick.
#[derive(Debug, Clone, Copy)]
pub struct EntityBoundHit {
    /// Hit node.
    pub node: NodeId,
    /// Distance from the ray origin to the first hit slab,
    /// in world units. Used to compare against grid hits and
    /// other entity hits.
    pub distance: f32,
    /// World-space hit point along the ray.
    pub point: [f32; 3],
    /// Bounds that produced the hit.
    pub bounds: EntityBounds,
}

/// Slab-intersection ray-vs-AABB. Returns the smallest
/// non-negative `t` for which `origin + t * dir` lands on
/// the box surface (or inside it).
///
/// * `dir` is *not* required to be unit length; the returned
///   `t` is in the same units as `dir`. When the editor uses
///   normalized rays (`camera_ray_for_pointer`), `t` lands in
///   world units.
/// * Box must have positive `half_extents`. Zero-extent boxes
///   never hit.
/// * Rays starting *inside* the box return `t = 0` so callers
///   can still pick something they're standing on.
pub fn ray_intersects_aabb(
    origin: [f32; 3],
    dir: [f32; 3],
    center: [f32; 3],
    half_extents: [f32; 3],
) -> Option<f32> {
    let mut t_min = f32::NEG_INFINITY;
    let mut t_max = f32::INFINITY;
    for axis in 0..3 {
        let half = half_extents[axis];
        if half <= 0.0 {
            return None;
        }
        let lo = center[axis] - half;
        let hi = center[axis] + half;
        let o = origin[axis];
        let d = dir[axis];
        if d.abs() < 1e-6 {
            // Ray parallel to this axis -- only hits if origin
            // is between the slabs.
            if o < lo || o > hi {
                return None;
            }
        } else {
            let inv = 1.0 / d;
            let t1 = (lo - o) * inv;
            let t2 = (hi - o) * inv;
            let (t_near, t_far) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
            if t_near > t_min {
                t_min = t_near;
            }
            if t_far < t_max {
                t_max = t_far;
            }
            if t_min > t_max {
                return None;
            }
        }
    }
    if t_max < 0.0 {
        return None;
    }
    Some(if t_min < 0.0 { 0.0 } else { t_min })
}

/// Intersect a ray with the horizontal plane `y = plane_y`.
/// Used by the entity-drag path to project mouse-move into
/// world-space on the same plane the entity lives on.
/// Returns `None` for parallel rays or hits behind the camera.
pub fn ray_intersects_horizontal_plane(
    origin: [f32; 3],
    dir: [f32; 3],
    plane_y: f32,
) -> Option<[f32; 3]> {
    if dir[1].abs() < 1e-6 {
        return None;
    }
    let t = (plane_y - origin[1]) / dir[1];
    if t < 0.0 {
        return None;
    }
    Some([origin[0] + dir[0] * t, plane_y, origin[2] + dir[2] * t])
}

fn ray_intersects_axis_aligned_plane(
    origin: [f32; 3],
    dir: [f32; 3],
    normal_axis: PrimitiveGizmoAxis,
    plane_coord: f32,
) -> Option<[f32; 3]> {
    let axis = normal_axis.index();
    if dir[axis].abs() < 1e-6 {
        return None;
    }
    let t = (plane_coord - origin[axis]) / dir[axis];
    if t < 0.0 {
        return None;
    }
    let mut hit = [
        origin[0] + dir[0] * t,
        origin[1] + dir[1] * t,
        origin[2] + dir[2] * t,
    ];
    hit[axis] = plane_coord;
    Some(hit)
}

#[cfg(test)]
mod entity_bounds_tests {
    use super::ray_intersects_aabb as ray_aabb;
    use super::ray_intersects_horizontal_plane as ray_plane;

    #[test]
    fn ray_aabb_hits_centred_box() {
        // Ray along +Z toward origin AABB at distance 10.
        let t = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        assert!(t.is_some());
        // Hit should land on the near slab at t = 9.
        assert!((t.unwrap() - 9.0).abs() < 1e-3);
    }

    #[test]
    fn ray_aabb_misses_offset_box() {
        // Box offset to +X by 100 -- a +Z ray at origin misses.
        let t = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [100.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        assert!(t.is_none());
    }

    #[test]
    fn ray_aabb_origin_inside_box_returns_zero() {
        let t = ray_aabb(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [10.0, 10.0, 10.0],
        );
        assert_eq!(t, Some(0.0));
    }

    #[test]
    fn ray_aabb_zero_extent_never_hits() {
        let t = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 1.0],
        );
        assert!(t.is_none());
    }

    #[test]
    fn ray_aabb_ray_parallel_to_slab() {
        // Ray on the X axis at Y=10, box at Y=0. Parallel +X
        // ray never enters the Y slab so it must miss.
        let t = ray_aabb(
            [0.0, 10.0, 0.0],
            [1.0, 0.0, 0.0],
            [50.0, 0.0, 0.0],
            [5.0, 5.0, 5.0],
        );
        assert!(t.is_none());
    }

    #[test]
    fn ray_aabb_nearest_of_two_boxes() {
        // Two co-axial boxes; near box at z=10, far box at
        // z=50. Nearest t corresponds to the near box.
        let near = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
        );
        let far = ray_aabb(
            [0.0, 0.0, -10.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, 50.0],
            [1.0, 1.0, 1.0],
        );
        assert!(near.unwrap() < far.unwrap());
    }

    #[test]
    fn ray_plane_hits_horizontal_plane_below() {
        // Camera 100 above origin looking down → +Z forward,
        // -Y up. Hit floor plane y=0 at t=100.
        let p = ray_plane([0.0, 100.0, 0.0], [0.0, -1.0, 0.0], 0.0);
        assert!(p.is_some());
        let p = p.unwrap();
        assert!((p[1] - 0.0).abs() < 1e-3);
    }

    #[test]
    fn ray_plane_misses_when_parallel() {
        let p = ray_plane([0.0, 100.0, 0.0], [1.0, 0.0, 0.0], 0.0);
        assert!(p.is_none());
    }

    #[test]
    fn ray_plane_misses_when_behind_camera() {
        // Ray points away from the plane.
        let p = ray_plane([0.0, 100.0, 0.0], [0.0, 1.0, 0.0], 0.0);
        assert!(p.is_none());
    }
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

#[derive(Debug, Clone, Copy)]
struct NodeGizmoScreenPlane {
    plane: NodeGizmoPlane,
    corners: [Pos2; 4],
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
    screen_axis: Vec2,
    start_plane_hit: Option<[f32; 3]>,
    current_plane_delta_world: [f32; 3],
    targets: Vec<NodeGizmoTarget>,
    current_steps: i32,
    snapshot_pushed: bool,
}

#[derive(Debug, Clone)]
struct NodeGizmoTarget {
    node: NodeId,
    start_translation: [f32; 3],
    start_rotation_degrees: [f32; 3],
    start_image_prop_size: Option<[u16; 2]>,
    start_box_prop_vertices: Option<[[i16; 3]; psxed_project::BOX_PROP_VERTEX_COUNT]>,
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

#[derive(Debug, Clone)]
struct GeometryClipboardCell {
    offset: [i32; 2],
    sector: Option<GridSector>,
}

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
    cells: Vec<GeometryClipboardCell>,
}

#[derive(Debug, Clone)]
struct ViewportBoxSelect {
    start: Pos2,
    current: Pos2,
    room: Option<NodeId>,
    additive: bool,
    base_sectors: HashSet<SectorSelection>,
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
    /// Cached enclosing room id so per-frame updates don't
    /// re-walk the scene tree.
    room: Option<NodeId>,
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
    NewProject { name: String, error: Option<String> },
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

    /// Mouse-wheel: dolly the orbit radius, or fly the free camera forward.
    fn scroll(&mut self, scroll: f32) {
        match self.mode {
            ViewportCameraMode::Orbit => {
                // Scroll = dolly. +/-8% of current radius per wheel notch,
                // clamped so the camera can't pass through the target or
                // escape the world entirely.
                let factor = if scroll > 0.0 { 0.92 } else { 1.08 };
                self.radius = ((self.radius as f32) * factor).clamp(512.0, 262_144.0) as i32;
            }
            ViewportCameraMode::Free => {
                let amount = (scroll * 8.0).clamp(-4096.0, 4096.0);
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
    /// Clear the painted surface under the cursor.
    Erase,
    /// Drop a child entity node into the sector under the cursor.
    /// The kind of node placed is controlled by `place_kind`.
    Place,
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
    /// `PointLight` with default color / intensity / radius.
    PointLightMarker,
    /// Fixed-budget point-projected sprite particle emitter.
    ParticleEmitter,
    /// `Portal` marker. Runtime cooking snaps it to the nearest
    /// cardinal sector edge and treats that edge as an open seam.
    Portal,
}

impl PlaceKind {
    const ALL: [Self; 9] = [
        Self::PlayerSpawn,
        Self::SpawnMarker,
        Self::ModelInstance,
        Self::Character,
        Self::ImageProp,
        Self::BoxProp,
        Self::PointLightMarker,
        Self::ParticleEmitter,
        Self::Portal,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::PlayerSpawn => "Player Spawn",
            Self::SpawnMarker => "Spawn",
            Self::ModelInstance => "Prop",
            Self::Character => "Character",
            Self::ImageProp => "Image Prop",
            Self::BoxProp => "Box Prop",
            Self::PointLightMarker => "Point Light",
            Self::ParticleEmitter => "Particle Emitter",
            Self::Portal => "Portal",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::PlayerSpawn | Self::SpawnMarker => icons::MAP_PIN,
            Self::ModelInstance => icons::BOX,
            Self::Character => icons::CIRCLE_DOT,
            Self::ImageProp => icons::PALETTE,
            Self::BoxProp => icons::BOX,
            Self::PointLightMarker => icons::SUN,
            Self::ParticleEmitter => icons::FOCUS,
            Self::Portal => icons::WAYPOINT,
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
            Self::Erase => "Erase",
            Self::Place => "Place",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Select => icons::POINTER,
            Self::PaintFloor => icons::GRID,
            Self::PaintWall => icons::BRICK_WALL,
            Self::PaintCeiling => icons::LAYERS,
            Self::Erase => icons::TRASH,
            Self::Place => icons::PLUS,
        }
    }

    /// `true` when the tool only makes sense once a Room is the
    /// active context -- viewport clicks should be suppressed
    /// otherwise so we don't paint into thin air.
    const fn requires_room_context(self) -> bool {
        matches!(
            self,
            Self::PaintFloor | Self::PaintWall | Self::PaintCeiling | Self::Erase | Self::Place
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceFilter {
    All,
    Texture,
    Material,
    ImagePropSource,
    Model,
    Animation,
    Character,
    Weapon,
    Mesh,
    Room,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceView {
    Room,
    Ui,
    Animation,
}

impl WorkspaceView {
    const fn label(self) -> &'static str {
        match self {
            Self::Room => "Room",
            Self::Ui => "UI",
            Self::Animation => "Animation Viewer",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::Room => icons::GRID,
            Self::Ui => icons::SQUARE,
            Self::Animation => icons::PLAY,
        }
    }

    const fn from_project(value: EditorWorkspaceView) -> Self {
        match value {
            EditorWorkspaceView::Room => Self::Room,
            EditorWorkspaceView::Ui => Self::Ui,
            EditorWorkspaceView::Animation => Self::Animation,
        }
    }

    const fn to_project(self) -> EditorWorkspaceView {
        match self {
            Self::Room => EditorWorkspaceView::Room,
            Self::Ui => EditorWorkspaceView::Ui,
            Self::Animation => EditorWorkspaceView::Animation,
        }
    }
}

impl ResourceFilter {
    const fn label(self) -> &'static str {
        match self {
            Self::All => "All Resources",
            Self::Texture => "Texture",
            Self::Material => "Material",
            Self::ImagePropSource => "Image Source",
            Self::Model => "Model",
            Self::Animation => "Animation",
            Self::Character => "Character Profiles",
            Self::Weapon => "Weapon",
            Self::Mesh => "Mesh",
            Self::Room => "Room",
            Self::Other => "Other",
        }
    }

    const fn icon(self) -> char {
        match self {
            Self::All => icons::LAYERS,
            Self::Texture => icons::PALETTE,
            Self::Material => icons::BLEND,
            Self::ImagePropSource => icons::PALETTE,
            Self::Model => icons::BOX,
            Self::Animation => icons::PLAY,
            Self::Character => icons::MAP_PIN,
            Self::Weapon => icons::WAYPOINT,
            Self::Mesh => icons::BOX,
            Self::Room => icons::GRID,
            Self::Other => icons::FILE,
        }
    }

    fn matches(self, data: &ResourceData) -> bool {
        match self {
            Self::All => true,
            Self::Texture => matches!(data, ResourceData::Texture { .. }),
            Self::Material => matches!(data, ResourceData::Material(_)),
            Self::ImagePropSource => {
                matches!(
                    data,
                    ResourceData::Texture { .. } | ResourceData::Material(_)
                )
            }
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
    /// Open the project at `dir`. Errors when `dir/project.ron` is
    /// missing or malformed -- the frontend wraps the error and falls
    /// back to the default project so a real load failure surfaces
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
        workspace.apply_project_editor_camera();
        workspace.apply_project_editor_visibility();
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
                selected_resources: HashSet::new(),
                resource_selection_anchor: None,
                selected_sector: None,
                selected_sectors: HashSet::new(),
                sector_selection_anchor: None,
                hovered_primitive: None,
                selected_primitive: None,
                selected_primitives: Vec::new(),
                hovered_entity_node: None,
            },
            interaction: Interaction::Idle,
            selection_mode: SelectionMode::default(),
            transform_gizmo_mode: TransformGizmoMode::Move,
            ui_transform_mode: UiTransformMode::Move,
            horizontal_edit_mode: HorizontalEditMode::default(),
            vertex_connectivity: VertexConnectivity::default(),
            validation_issue_primitives: Vec::new(),
            validation_issue_rooms: HashSet::new(),
            floating_geometry: None,
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
            scene_filter: String::new(),
            file_filter: String::new(),
            left_dock_scene_fraction: LEFT_DOCK_DEFAULT_SCENE_FRACTION,
            resource_search: String::new(),
            resource_filter: ResourceFilter::All,
            material_texture_search: String::new(),
            resource_renaming: None,
            resource_delete_confirm: None,
            active_tool: ViewTool::Select,
            place_kind: PlaceKind::PlayerSpawn,
            portal_place_direction: GridDirection::North,
            place_resource: None,
            brush_material: None,
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
            play_frame_times_ms: VecDeque::with_capacity(PLAY_FRAME_HISTORY_CAP),
            play_frame_last_sample_serial: None,
            play_debug_terminal_lines: VecDeque::with_capacity(PLAY_DEBUG_TERMINAL_LINE_CAP),
            shortcut_group_flash: None,
            // The Room workspace still uses the bit-faithful 3D preview by
            // default, but the top-level workspace itself is restored from
            // the project so UI/Animation authoring reopens where it left off.
            view_2d: false,
            active_workspace: WorkspaceView::from_project(editor_workspace.active),
            left_dock_open: true,
            inspector_open: true,
            resources_open: true,
            content_browser_view: ContentBrowserView::Resources,
            viewport_pan: Vec2::ZERO,
            viewport_zoom: DEFAULT_VIEWPORT_ZOOM,
            last_viewport_size: Vec2::new(1280.0, 720.0),
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
            },
            texture_thumbs: HashMap::new(),
            psoxide_logo_texture: None,
            ui_font_textures: Vec::new(),
            model_resource_preview_texture: None,
            animation_viewer: ModelAnimationViewerState::default(),
            animation_viewer_preview_texture: None,
            texture_import_dialog: TextureImportDialog::default(),
            model_import_dialog: ModelImportDialog::default(),
            import_retired_textures: Vec::new(),
            dirty: false,
            status: "Editor ready".to_string(),
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
    }

    fn current_editor_workspace_state(&self) -> EditorWorkspaceState {
        EditorWorkspaceState {
            active: self.active_workspace.to_project(),
        }
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
        self.persist_editor_camera_state();
        self.persist_editor_visibility_state();
        self.persist_editor_workspace_state();
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
        if paths_equivalent(&self.project_dir, &psxed_project::default_project_dir()) {
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
                self.status = sync_status
                    .unwrap_or_else(|| format!("Reloaded {}", short_path(&self.project_dir)));
                self.select_first_room();
                self.apply_project_editor_camera();
                self.apply_project_editor_visibility();
                self.apply_project_editor_workspace();
            }
            Err(error) => {
                self.status = format!("Reload failed: {error}");
            }
        }
    }

    /// Create `editor/projects/<derived-name>/` by recursive-copy of
    /// the default project, then switch this workspace to it.
    ///
    /// Validates `name`: non-empty and target directory must not
    /// already exist. On success the workspace points at the new project; on
    /// failure the workspace is unchanged.
    pub fn create_and_open_project(&mut self, name: &str) -> Result<(), String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("Project name cannot be empty".to_string());
        }
        let target = psxed_project::projects_dir().join(psxed_project::project_file_stem(trimmed));
        if target.exists() {
            return Err(format!("{} already exists", short_path(&target)));
        }
        copy_dir_recursive(&psxed_project::default_project_dir(), &target)
            .map_err(|error| format!("copy default project: {error}"))?;
        let mut opened = Self::open_directory(&target)?;
        opened.project.name = trimmed.to_string();
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

    fn current_project_is_default(&self) -> bool {
        paths_equivalent(&self.project_dir, &psxed_project::default_project_dir())
    }

    fn delete_project_fallback_dir(delete_dir: &Path) -> Result<PathBuf, String> {
        let default_dir = psxed_project::default_project_dir();
        if !paths_equivalent(delete_dir, &default_dir) && default_dir.join("project.ron").is_file()
        {
            return Ok(default_dir);
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
        if self.current_project_is_default() {
            return Err("The default project cannot be deleted".to_string());
        }
        let projects_root = psxed_project::projects_dir();
        let parent = delete_dir
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", delete_dir.display()))?;
        if !paths_equivalent(parent, &projects_root) {
            return Err("Only projects in editor/projects can be deleted".to_string());
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
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| node.id)
        {
            self.replace_node_selection(room_id);
            self.frame_3d_on_room(room_id);
        }
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

    /// Repoint the 3D viewport at `center` without changing the
    /// user's current distance from the focus. This is the behavior
    /// expected from the `.` shortcut while inspecting nearby props.
    fn focus_3d_on_point_preserving_distance(&mut self, center: [f32; 3]) {
        let target = [
            round_to_i32(center[0]),
            round_to_i32(center[1]),
            round_to_i32(center[2]),
        ];
        match self.camera_rig.mode {
            ViewportCameraMode::Orbit => {
                self.camera_rig.target = target;
            }
            ViewportCameraMode::Free => {
                self.camera_rig.target = target;
                self.camera_rig.radius =
                    distance_i32(self.camera_rig.free_position, target).clamp(512, 262_144);
                if let Some((yaw, pitch)) =
                    camera_angles_to_look_at(self.camera_rig.free_position, target)
                {
                    self.camera_rig.free_yaw = yaw;
                    self.camera_rig.free_pitch = pitch;
                }
                self.camera_rig.free_initialized = true;
            }
        }
    }

    fn room_bounds_3d(&self, room_id: NodeId) -> Option<([f32; 3], [f32; 3])> {
        let scene = self.project.active_scene();
        let room = scene.node(room_id)?;
        let NodeKind::Room { grid } = &room.kind else {
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
        let NodeKind::Room { grid } = &room.kind else {
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
            .filter(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| (node.id, node.name.clone()))
            .collect();
        if rooms.is_empty() {
            return Err("No Room nodes in the active scene".to_string());
        }

        let mut total_bytes = 0usize;
        let mut written = 0usize;
        for (room_id, room_name) in &rooms {
            let cook_result = {
                let scene = self.project.active_scene();
                let Some(node) = scene.node(*room_id) else {
                    continue;
                };
                let NodeKind::Room { grid } = &node.kind else {
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
    #[allow(clippy::too_many_arguments)]
    pub fn cook_playtest_to_disk(&mut self) -> Result<String, String> {
        let dir = psxed_project::playtest::default_generated_dir();
        let mut project = self.project.clone();
        project.normalize_loaded();
        self.clear_validation_issues();
        // Re-run build_package up front to grab the asset/material
        // counts for the status string. cook_to_dir does this
        // internally too; the duplicate cost is negligible
        // compared to the IO it saves a step later.
        let (package, _report) =
            psxed_project::playtest::build_package(&project, &self.project_dir);
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

        let report = psxed_project::playtest::cook_to_dir(&project, &self.project_dir, &dir)
            .map_err(|e| format!("write playtest output: {e}"))?;
        if !report.is_ok() {
            self.record_first_playtest_world_cook_issue(&project);
            return Err(format!(
                "playtest validation failed: {}",
                report.errors.join("; ")
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
                    " — {} room{}, {} model{}, {} character{}{}, {} light{}, {} asset{}, {} texture{}, {} material{}, {} entit{}",
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
        Ok(format!(
            "Playtest cooked → {}{}{}",
            dir.display(),
            counts,
            warning_suffix,
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
