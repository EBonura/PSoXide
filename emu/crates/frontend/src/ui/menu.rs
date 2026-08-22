//! Menu overlay -- the launcher / pause shell drawn over the framebuffer.
//!
//! Horizontal animated category icons with a vertical item list beneath
//! the active category. Drawn via `egui::Painter` on a middle layer so
//! it overlays the framebuffer/central area but sits below the HUD.
//!
//! Navigation: arrows + Enter + Escape (gamepad will land when the
//! input subsystem does). Escape also toggles the overlay open/closed.
//!
//! Categories: Games / Projects / Editor / Settings / System
//! (Projects + Create are editor/native-only). The debug sidebar is
//! toggled from the toolbar, not the menu.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Align2, FontId, Pos2, Rect, Vec2};

use crate::icons;
use crate::theme;

const CATEGORY_SPACING: f32 = 100.0;
const ICON_SIZE_ACTIVE: f32 = 32.0;
const ICON_SIZE_INACTIVE: f32 = 20.0;
const ITEM_HEIGHT: f32 = 40.0;
/// Hard ceiling on item-row width (huge libraries / long paths elide past it).
const ITEM_MAX_WIDTH: f32 = 820.0;
/// Floor so short categories (System, Settings) don't shrink to a sliver.
const ITEM_MIN_WIDTH: f32 = 260.0;
/// How much a row's right-aligned value (path / region tag) may contribute to
/// the auto-sizing, and how wide it may draw before eliding.
const VALUE_WIDTH_CAP: f32 = 240.0;
const ITEM_GAP: f32 = 2.0;
const ROW_ACTION_WIDTH: f32 = 40.0;
const ANIM_SPEED: f32 = 10.0;
/// Open/close dissolve speed (exponential ease). Higher = snappier.
const FADE_SPEED: f32 = 16.0;

/// A menu action the Menu emits when the user confirms an item. The
/// app layer interprets these -- Menu stays stateless about the
/// emulator.
///
/// Note: dropped `Copy` in favour of `Clone` to carry the
/// game-ID payload on `LaunchGame`. The dispatch cost is one
/// `String::clone` per selection -- negligible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Toggle between continuous-run and paused.
    ToggleRun,
    /// Advance the CPU by one retired instruction.
    StepOne,
    /// Reseat the CPU at its reset vector.
    Reset,
    /// Start a new poll-exact port-1 recording, or stop and persist the
    /// active one. Native saves the tape under the current game's config
    /// tree; the web build reboots the game first so the tape counts from
    /// poll 0 of a cold boot, and stopping downloads it as a CSV.
    ToggleInputRecording,
    /// Load a recorded input tape (CSV / `.pxtape`) and replay it against a
    /// fresh boot of the current game. Native opens a file dialog; the web
    /// build opens the browser upload picker.
    LoadInputReplay,
    /// Toggle warm SYSTEM.CNF disc fast boot. When disabled, discs
    /// boot through the full BIOS logo path.
    ToggleFastBoot,
    /// Save the running game. Native builds create a new history slot (see
    /// [`SaveStateRow`]/[`MenuState::sync_save_states`]); the browser replaces
    /// its one persistent per-game quick-save. Either becomes the F7 target.
    SaveState,
    /// Load the running game's state from the numbered save slot. The
    /// `bool` is "resume paused" -- leave the emulator frozen on the
    /// restored frame instead of immediately continuing.
    LoadState(u8, bool),
    /// Open the save-states panel (thumbnail list, pin-to-top,
    /// load-with-confirmation) -- driven by the always-visible toolbar
    /// icon as well as the System category's "Save states" row.
    OpenSaveStates,
    /// Open the controls panel (clickable PS1 controller drawing,
    /// press-a-key rebinding, reset to defaults) -- driven by the
    /// always-visible toolbar icon as well as the System category's
    /// "Controls" row.
    OpenControls,
    /// Restore every port-1 binding to the built-in defaults.
    ResetControls,
    /// Pin the given slot as the save history's "top" -- the target
    /// [`MenuAction::LoadState`] via F7/quick-load resolves to --
    /// without touching slot numbering or any other save's position.
    PinAsTop(u8),
    /// Launch a game by its menu launch token. Retail games use the
    /// stable library ID; authored project builds use a path-qualified
    /// token so projects sharing the same PSX volume ID remain distinct.
    LaunchGame(String),
    /// Open the CD burn submenu for a launchable project disc.
    #[cfg(not(target_arch = "wasm32"))]
    OpenBurnMenu(String),
    /// Re-walk the configured library root and refresh
    /// `library.ron`. Surfaced as a "Refresh library" item in
    /// the Games category so users can trigger a rescan without leaving the
    /// Menu.
    RescanLibrary,
    /// Enter or leave the host-side editor workspace.
    #[cfg(feature = "editor")]
    ToggleEditorWorkspace,
    /// Pick and persist the BIOS image path.
    ChooseBiosPath,
    /// Pick and persist the games library root.
    ChooseGamesPath,
    /// Cycle the menu backdrop opacity through a few presets.
    CycleMenuOpacity,
    /// Web: reconnect a previously-saved BIOS + games folder.
    #[cfg(target_arch = "wasm32")]
    Reconnect,
    /// Open the About card (from the Settings menu).
    ShowAbout,
    /// A non-actionable row, e.g. the "not available in the web build"
    /// placeholder on a desktop-only category. Selecting it does nothing.
    Noop,
    /// Quit the application.
    Quit,
}

/// Every rebindable port-1 input the controls panel exposes -- the
/// full PS1 pad (d-pad, face, shoulders, Start/Select, stick clicks,
/// the DualShock Analog toggle) plus the keyboard-emulated analog
/// stick directions. The app layer maps each target onto its
/// `psoxide_settings` binding field; the Menu module stays decoupled
/// from the settings crate's types, same as [`LibraryItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PadBindTarget {
    Up,
    Down,
    Left,
    Right,
    Cross,
    Circle,
    Square,
    Triangle,
    L1,
    L2,
    R1,
    R2,
    Start,
    Select,
    L3,
    R3,
    Analog,
    LStickUp,
    LStickDown,
    LStickLeft,
    LStickRight,
    RStickUp,
    RStickDown,
    RStickLeft,
    RStickRight,
}

impl PadBindTarget {
    /// Every target, in the order the panel's fallback list renders.
    pub const ALL: [PadBindTarget; 25] = [
        PadBindTarget::Up,
        PadBindTarget::Down,
        PadBindTarget::Left,
        PadBindTarget::Right,
        PadBindTarget::Cross,
        PadBindTarget::Circle,
        PadBindTarget::Square,
        PadBindTarget::Triangle,
        PadBindTarget::L1,
        PadBindTarget::L2,
        PadBindTarget::R1,
        PadBindTarget::R2,
        PadBindTarget::Start,
        PadBindTarget::Select,
        PadBindTarget::L3,
        PadBindTarget::R3,
        PadBindTarget::Analog,
        PadBindTarget::LStickUp,
        PadBindTarget::LStickDown,
        PadBindTarget::LStickLeft,
        PadBindTarget::LStickRight,
        PadBindTarget::RStickUp,
        PadBindTarget::RStickDown,
        PadBindTarget::RStickLeft,
        PadBindTarget::RStickRight,
    ];

    /// Short display name drawn on/next to the hotspot.
    pub fn label(self) -> &'static str {
        match self {
            PadBindTarget::Up => "D-Pad Up",
            PadBindTarget::Down => "D-Pad Down",
            PadBindTarget::Left => "D-Pad Left",
            PadBindTarget::Right => "D-Pad Right",
            PadBindTarget::Cross => "Cross",
            PadBindTarget::Circle => "Circle",
            PadBindTarget::Square => "Square",
            PadBindTarget::Triangle => "Triangle",
            PadBindTarget::L1 => "L1",
            PadBindTarget::L2 => "L2",
            PadBindTarget::R1 => "R1",
            PadBindTarget::R2 => "R2",
            PadBindTarget::Start => "Start",
            PadBindTarget::Select => "Select",
            PadBindTarget::L3 => "L3",
            PadBindTarget::R3 => "R3",
            PadBindTarget::Analog => "Analog",
            PadBindTarget::LStickUp => "L-Stick Up",
            PadBindTarget::LStickDown => "L-Stick Down",
            PadBindTarget::LStickLeft => "L-Stick Left",
            PadBindTarget::LStickRight => "L-Stick Right",
            PadBindTarget::RStickUp => "R-Stick Up",
            PadBindTarget::RStickDown => "R-Stick Down",
            PadBindTarget::RStickLeft => "R-Stick Left",
            PadBindTarget::RStickRight => "R-Stick Right",
        }
    }
}

/// Per-frame input snapshot the shell assembles from keyboard events.
#[derive(Default, Debug, Clone, Copy)]
pub struct MenuInput {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub confirm: bool,
    pub back: bool,
    pub toggle_open: bool,
}

/// One row inside a category. Labels + values are `String` so we
/// can populate them from library entries at runtime (titles,
/// region tags, sizes). Static strings like "Run" / "Pause" also
/// fit the same shape at a small allocation cost -- the whole
/// category tree is rebuilt at most a few times a session.
struct MenuItem {
    label: String,
    action: MenuAction,
    burn_action: Option<MenuAction>,
    /// Optional right-aligned subtitle -- used for region tags
    /// ("NTSC-U"), file sizes, and keyboard shortcut hints.
    value: Option<String>,
}

/// One Menu column. `icon_name` is a short tag used by tests /
/// diagnostics so we can identify a category without comparing
/// Unicode codepoints.
struct Category {
    name: &'static str,
    icon: char,
    items: Vec<MenuItem>,
}

impl Category {
    /// True when this category is a desktop-only stand-in: a single
    /// non-actionable placeholder row. Drawn greyed in the icon row.
    fn disabled(&self) -> bool {
        matches!(self.items.as_slice(), [item] if item.action == MenuAction::Noop)
    }
}

pub struct MenuState {
    pub open: bool,
    category_index: usize,
    item_index: usize,
    anim_x: f32,
    /// Open/close dissolve factor: 0 (hidden) .. 1 (fully shown).
    appear: f32,
    /// Menu backdrop opacity (percent), synced from settings; drives the
    /// `draw` backdrop alpha.
    backdrop_pct: u8,
    /// Per-frame animated scroll position for the item list, in
    /// "rows of (ITEM_HEIGHT + ITEM_GAP)". A value of `N` means
    /// item `N` is drawn at the top of the visible strip.
    /// Eased toward the integer target computed from `item_index`
    /// each frame by the same `ANIM_SPEED` knob that drives the
    /// category slide, so navigating a long list produces a smooth
    /// scroll rather than a snap.
    scroll_y: f32,
    /// Seconds the current selection has been highlighted -- drives the
    /// marquee scroll of an overflowing selected label.
    marquee_t: f32,
    /// The (category, item) the marquee is tracking; resets `marquee_t`
    /// when the selection moves.
    marquee_key: (usize, usize),
    /// Whether the top-right About card is showing. Mouse-driven; cleared
    /// whenever the menu itself closes.
    about_open: bool,
    /// Whether the save-states panel (thumbnail list, pin-to-top,
    /// load-with-confirmation) is showing. Unlike `about_open`, this
    /// is reachable from the always-visible toolbar icon independent
    /// of whether the main Menu overlay (`open`) is up, so it is not
    /// tied to `open` the way the About card is.
    save_states_open: bool,
    /// Live snapshot of this game's saves, newest first, set by
    /// [`MenuState::sync_save_states`]. The System category's row
    /// count and the save-states panel both read from here rather
    /// than each keeping their own copy.
    save_rows: Vec<SaveStateRow>,
    /// A "Load" click in the save-states panel doesn't load
    /// immediately -- it stages the target slot here so the panel can
    /// show a confirm-with-"resume paused" dialog first.
    pending_load_confirm: Option<PendingLoadConfirm>,
    /// Lazily-loaded, path-keyed cache of save-thumbnail textures.
    /// Never invalidated: a slot's `.png` never changes after it's
    /// written (saves are a history, not overwritten slots), so a
    /// path is a stable cache key for the process's lifetime.
    save_thumb_cache: HashMap<PathBuf, egui::TextureHandle>,
    /// Whether the controls panel (PS1 controller drawing + rebinds)
    /// is showing. Like `save_states_open`, reachable from the
    /// always-visible toolbar icon independent of the Menu overlay.
    controls_open: bool,
    /// The target currently waiting for a key press, if the user
    /// clicked a hotspot. The shell's keyboard handler consumes the
    /// next physical key into this instead of routing it anywhere
    /// else (Escape cancels).
    controls_capture: Option<PadBindTarget>,
    /// Current binding label per target, synced from the app layer via
    /// [`MenuState::sync_controls`] whenever a binding changes.
    controls_labels: HashMap<PadBindTarget, String>,
    /// Targets whose key is physically held right now, synced by the
    /// shell each frame while the panel is open. Drives the panel's
    /// green held-highlights (a live rollover/ghosting tester).
    controls_live_held: Vec<PadBindTarget>,
    pending_pointer_action: Option<MenuAction>,
    categories: Vec<Category>,
}

/// A save staged for loading, pending the user confirming (and
/// optionally toggling) "resume paused" in the save-states panel.
#[derive(Debug, Clone)]
struct PendingLoadConfirm {
    slot: u8,
    label: String,
    thumbnail_path: Option<PathBuf>,
    resume_paused: bool,
}

impl Default for MenuState {
    fn default() -> Self {
        Self::new()
    }
}

/// An entry passed into the Menu from the library layer -- minimal
/// subset of [`psoxide_settings::LibraryEntry`] the Menu needs to
/// render an item (title + id for dispatch + region/size as the
/// right-aligned value). Kept separate so the Menu module stays
/// decoupled from the settings crate's types (and from the GUI
/// from the tests' perspective).
#[derive(Debug, Clone)]
pub struct LibraryItem {
    /// Stable game ID (16-hex-char fingerprint). Payload of
    /// [`MenuAction::LaunchGame`] when the user confirms.
    pub id: String,
    /// Main label -- typically the PVD volume identifier or the
    /// file stem.
    pub title: String,
    /// Right-aligned subtitle, e.g. "NTSC-U · 602 MiB".
    pub subtitle: String,
    /// Whether the launcher should show the CD burn affordance.
    #[cfg(not(target_arch = "wasm32"))]
    pub burnable: bool,
}

/// One existing save state, as far as the Menu needs to know to draw
/// a "Load state" row -- the fully-formatted display label (e.g. "2m
/// ago -- tick 45,000,667") plus the slot number to dispatch on
/// selection. Built by `AppState` from `psoxide_settings::savestate`
/// data; kept as a plain string here for the same reason as
/// [`LibraryItem`] -- the Menu module doesn't depend on the settings
/// crate's types.
#[derive(Debug, Clone)]
pub struct SaveStateRow {
    /// Slot number to pass to [`MenuAction::LoadState`].
    pub slot: u8,
    /// Pre-formatted row label.
    pub label: String,
    /// Path to this slot's screenshot thumbnail, if a readable one
    /// exists on disk (older saves, or ones whose capture failed,
    /// have none -- the panel draws a placeholder for those).
    pub thumbnail_path: Option<PathBuf>,
    /// Whether this is the save history's pinned "top" -- the one
    /// F7/quick-load currently resolves to. Exactly one row is `true`
    /// whenever `save_rows` is non-empty.
    pub is_top: bool,
}

impl MenuState {
    pub fn new() -> Self {
        Self::with_running(false)
    }

    pub fn with_running(running: bool) -> Self {
        // Boot categories with the library sections empty -- they
        // get filled by `set_library` once AppState loads the
        // cached entries. A fresh install sees placeholder "No
        // games found -- run Refresh library" rows.
        let categories = vec![
            build_games_category(&[]),
            // Projects are editor-authored and filesystem-backed; the web build
            // has neither, so the category is dropped there.
            #[cfg(not(target_arch = "wasm32"))]
            build_projects_category(&[]),
            // Projects is dropped outright on web rather than shown greyed:
            // it is filesystem-backed and can never work there, so a permanent
            // "not available" row is a dead entry the user has to skip past on
            // every visit.
            // The Editor category is the entry point into the host editor
            // workspace; it is absent in emulator-only builds.
            #[cfg(feature = "editor")]
            build_create_category(false),
            #[cfg(target_arch = "wasm32")]
            disabled_category("Editor", icons::FOLDER),
            build_settings_category(),
            build_system_category(running, 0),
            // There is no "quit" in a browser tab, so the web build omits it.
            #[cfg(not(target_arch = "wasm32"))]
            Category {
                name: "Quit",
                icon: icons::POWER,
                items: vec![MenuItem {
                    label: "Quit PSoXide".to_string(),
                    action: MenuAction::Quit,
                    burn_action: None,
                    value: Some("Esc ×2".to_string()),
                }],
            },
        ];

        Self {
            open: true,
            category_index: 0,
            item_index: 0,
            anim_x: 0.0,
            appear: 0.0,
            backdrop_pct: 90,
            scroll_y: 0.0,
            marquee_t: 0.0,
            marquee_key: (0, 0),
            about_open: false,
            save_states_open: false,
            save_rows: Vec::new(),
            pending_load_confirm: None,
            save_thumb_cache: HashMap::new(),
            controls_open: false,
            controls_capture: None,
            controls_labels: HashMap::new(),
            controls_live_held: Vec::new(),
            pending_pointer_action: None,
            categories,
        }
    }

    /// Rebuild the Games and Projects categories from a library
    /// snapshot. Call after load, after a rescan, and whenever the
    /// library changes. Existing selection is preserved when
    /// possible (same category + in-range item) and clamped to the
    /// new bounds otherwise.
    pub fn set_library(&mut self, games: &[LibraryItem], projects: &[LibraryItem]) {
        // Snapshot the current selection's category NAME so we can
        // re-resolve after rebuilding (indices may change).
        let current_cat_name = self
            .categories
            .get(self.category_index)
            .map(|c| c.name)
            .unwrap_or("");

        if let Some(games_cat) = self.categories.iter_mut().find(|c| c.name == "Games") {
            *games_cat = build_games_category(games);
        }
        // The web build has no Projects category (see `with_running`).
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(projects_cat) = self.categories.iter_mut().find(|c| c.name == "Projects") {
            *projects_cat = build_projects_category(projects);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = projects;

        // Try to preserve the user's category if it still exists.
        if let Some(idx) = self
            .categories
            .iter()
            .position(|c| c.name == current_cat_name)
        {
            self.category_index = idx;
        } else {
            self.category_index = 0;
        }
        // Clamp item index to the new category bounds.
        let item_count = self.categories[self.category_index].items.len();
        if self.item_index >= item_count {
            self.item_index = item_count.saturating_sub(1);
        }
    }

    /// Rebuild categories with a fresh "Run"/"Pause" label. Called
    /// when `AppState.running` flips.
    pub fn sync_run_label(&mut self, running: bool) {
        if let Some(system) = self.categories.iter_mut().find(|c| c.name == "System") {
            if let Some(item) = system.items.first_mut() {
                item.label = if running {
                    "Pause".into()
                } else {
                    "Run".into()
                };
            }
        }
    }

    /// Update the System category's disc fast-boot value after the
    /// persisted setting changes.
    pub fn sync_fast_boot_label(&mut self, enabled: bool) {
        if let Some(system) = self.categories.iter_mut().find(|c| c.name == "System") {
            if let Some(item) = system
                .items
                .iter_mut()
                .find(|item| item.action == MenuAction::ToggleFastBoot)
            {
                item.value = Some(if enabled { "On" } else { "Off" }.into());
            }
        }
    }

    /// Keep the System row in sync with the F8 recording latch.
    pub fn sync_input_recording_label(&mut self, recording: bool) {
        if let Some(system) = self.categories.iter_mut().find(|c| c.name == "System") {
            if let Some(item) = system
                .items
                .iter_mut()
                .find(|item| item.action == MenuAction::ToggleInputRecording)
            {
                item.label = if recording {
                    if cfg!(target_arch = "wasm32") {
                        "Stop recording (download CSV)"
                    } else {
                        "Stop input recording"
                    }
                } else if cfg!(target_arch = "wasm32") {
                    "Record input from boot"
                } else {
                    "Record input"
                }
                .into();
            }
        }
    }

    /// Rebuild the System category's save-state rows from a live save
    /// listing. Call after a save/load completes and whenever the
    /// running game changes (a different game has different saves).
    /// Rebuilds the whole category (like [`build_system_category`]
    /// does at startup) rather than patching rows in place, since the
    /// row *count* changes as saves are added -- `running` is passed
    /// through unchanged so this doesn't clobber the Run/Pause label.
    pub fn sync_save_states(&mut self, running: bool, rows: &[SaveStateRow]) {
        self.save_rows = rows.to_vec();
        if let Some(idx) = self.categories.iter().position(|c| c.name == "System") {
            self.categories[idx] = build_system_category(running, rows.len());
        }
    }

    /// Open the save-states panel. Driven by the always-visible
    /// toolbar icon and the System category's "Save states" row.
    pub fn open_save_states(&mut self) {
        self.save_states_open = true;
    }

    /// Open the controls panel. Driven by the always-visible toolbar
    /// icon and the System category's "Controls" row.
    pub fn open_controls(&mut self) {
        self.controls_open = true;
    }

    /// Replace the per-target binding labels the controls panel shows.
    /// The app layer calls this at startup and after every rebind /
    /// reset, so the panel always reflects what the settings actually
    /// persist.
    pub fn sync_controls(&mut self, labels: impl IntoIterator<Item = (PadBindTarget, String)>) {
        self.controls_labels = labels.into_iter().collect();
    }

    /// The target currently waiting for a key press, if any. The
    /// shell's keyboard handler checks this every key event: while a
    /// capture is armed, keys feed the rebind instead of the game.
    pub fn controls_capture(&self) -> Option<PadBindTarget> {
        self.controls_capture
    }

    /// Disarm the pending capture (key consumed or Escape pressed).
    pub fn clear_controls_capture(&mut self) {
        self.controls_capture = None;
    }

    /// Whether the controls panel is currently showing -- the shell
    /// checks this to route Escape to "close panel" instead of the
    /// menu toggle, and to skip the live-held sync when it's not up.
    pub fn controls_panel_open(&self) -> bool {
        self.controls_open
    }

    /// Close the controls panel, disarming any pending capture.
    pub fn close_controls(&mut self) {
        self.controls_open = false;
        self.controls_capture = None;
    }

    /// Replace the live held-target set the panel highlights. Synced
    /// by the shell each frame while the panel is open.
    pub fn set_controls_live_held(&mut self, held: Vec<PadBindTarget>) {
        self.controls_live_held = held;
    }

    /// Store the menu-backdrop opacity (percent) and reflect it in the
    /// Settings item's displayed value. Driven by `video.menu_opacity_pct`.
    pub fn set_menu_opacity(&mut self, pct: u8) {
        self.backdrop_pct = pct.min(100);
        if let Some(settings) = self.categories.iter_mut().find(|c| c.name == "Settings") {
            if let Some(item) = settings
                .items
                .iter_mut()
                .find(|item| item.action == MenuAction::CycleMenuOpacity)
            {
                item.value = Some(format!("{}%", self.backdrop_pct));
            }
        }
    }

    /// Update the Editor category label for the current workspace.
    #[cfg(feature = "editor")]
    pub fn sync_editor_label(&mut self, editor_open: bool) {
        if let Some(create) = self.categories.iter_mut().find(|c| c.name == "Editor") {
            if let Some(item) = create
                .items
                .iter_mut()
                .find(|item| item.action == MenuAction::ToggleEditorWorkspace)
            {
                item.label = if editor_open {
                    "Close editor workspace".into()
                } else {
                    "Open editor workspace".into()
                };
                item.value = Some(if editor_open { "Active" } else { "Studio" }.into());
            }
        }
    }

    /// Update the Settings category path summaries.
    pub fn sync_settings_paths(&mut self, bios: impl Into<String>, games: impl Into<String>) {
        let bios = bios.into();
        let games = games.into();
        if let Some(settings) = self.categories.iter_mut().find(|c| c.name == "Settings") {
            for item in &mut settings.items {
                match item.action {
                    MenuAction::ChooseBiosPath => item.value = Some(bios.clone()),
                    MenuAction::ChooseGamesPath => item.value = Some(games.clone()),
                    _ => {}
                }
            }
        }
    }

    /// Move selection to the category named `name`, if it exists.
    pub fn select_category(&mut self, name: &str) {
        if let Some(idx) = self.categories.iter().position(|c| c.name == name) {
            self.category_index = idx;
            self.item_index = 0;
            self.scroll_y = 0.0;
        }
    }

    /// Open the About card. Driven by the Settings "About" row.
    pub fn show_about(&mut self) {
        self.about_open = true;
    }

    /// Feed one frame of input. Returns `Some(action)` when a confirm
    /// selects an item.
    pub fn update(&mut self, input: &MenuInput) -> Option<MenuAction> {
        if let Some(action) = self.pending_pointer_action.take() {
            return Some(action);
        }
        if input.toggle_open {
            self.open = !self.open;
        }
        if !self.open {
            return None;
        }

        // The About card is modal: swallow menu navigation while it is up, and
        // let confirm or back dismiss it (mouse users get Close / click-outside).
        if self.about_open {
            if input.confirm || input.back {
                self.about_open = false;
            }
            return None;
        }

        let num_cats = self.categories.len();
        if num_cats > 0 {
            if input.left {
                // Wrap to the last category from the first, mirroring the
                // up/down item wrap below.
                self.category_index = if self.category_index == 0 {
                    num_cats - 1
                } else {
                    self.category_index - 1
                };
                self.item_index = 0;
                // Snap the scroll so the new category's list shows from the
                // top -- avoids an awkward animation from mid-list.
                self.scroll_y = 0.0;
            }
            if input.right {
                self.category_index = (self.category_index + 1) % num_cats;
                self.item_index = 0;
                self.scroll_y = 0.0;
            }
        }

        let num_items = self.categories[self.category_index].items.len();
        if num_items > 0 {
            if input.up {
                self.item_index = if self.item_index == 0 {
                    num_items - 1
                } else {
                    self.item_index - 1
                };
            }
            if input.down {
                self.item_index = (self.item_index + 1) % num_items;
            }
        }

        if input.confirm && num_items > 0 {
            return Some(
                self.categories[self.category_index].items[self.item_index]
                    .action
                    .clone(),
            );
        }

        if input.back {
            self.open = false;
        }
        None
    }

    /// Take a click action queued while painting this frame.
    ///
    /// The shell drains this immediately after [`Self::draw`] so browser file
    /// pickers still run as part of the click-triggered redraw. Leaving it for
    /// the next animation frame can lose the browser's transient user gesture.
    pub fn take_pending_pointer_action(&mut self) -> Option<MenuAction> {
        self.pending_pointer_action.take()
    }

    /// Public reader for the currently-selected item's action --
    /// tests use it to assert the menu is populated correctly
    /// without driving input events.
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn selected_action(&self) -> Option<&MenuAction> {
        self.categories
            .get(self.category_index)
            .and_then(|c| c.items.get(self.item_index))
            .map(|i| &i.action)
    }

    /// Current category name -- also exposed for test assertions.
    #[cfg(test)]
    pub fn current_category(&self) -> Option<&'static str> {
        self.categories.get(self.category_index).map(|c| c.name)
    }

    /// Draw the Menu overlay on a middle-layer painter. `dt` drives the
    /// slide animation.
    pub fn draw(&mut self, ctx: &egui::Context, dt: f32, warning: Option<&str>) {
        // The save-states panel is reachable straight from the
        // always-visible toolbar icon, independent of whether the
        // full Menu overlay is open -- so it's drawn (and its own
        // egui::Window handles its layering) before the `appear`-gated
        // early-return below, unlike the About card which only makes
        // sense as a child of the open Menu.
        if self.save_states_open {
            save_states_panel(
                ctx,
                &mut self.save_states_open,
                &self.save_rows,
                &mut self.pending_load_confirm,
                &mut self.save_thumb_cache,
                &mut self.pending_pointer_action,
            );
        }
        // The controls panel is likewise toolbar-reachable and lives
        // outside the appear-gate so rebinding works mid-game. Closing
        // the panel disarms any pending key capture with it.
        if self.controls_open {
            controls_panel(
                ctx,
                &mut self.controls_open,
                &self.controls_labels,
                &self.controls_live_held,
                &mut self.controls_capture,
                &mut self.pending_pointer_action,
            );
            if !self.controls_open {
                self.controls_capture = None;
            }
        }
        // The About card belongs to the open menu; drop it the moment the menu
        // is dismissed so it can't linger through the close dissolve.
        if !self.open {
            self.about_open = false;
        }
        // Quick dissolve: ease `appear` toward 1 when open / 0 when closed, and
        // keep drawing (faded) until it reaches 0. Every colour below is run
        // through `fade`, so the whole overlay cross-fades in and out.
        let target = if self.open { 1.0 } else { 0.0 };
        // Cap the per-frame step so a single long frame (e.g. the hitch when a
        // game or demo boots) can't collapse the fade into a hard cut --
        // it stays a dissolve across the following frames.
        let k = (FADE_SPEED * dt).min(0.5);
        self.appear += (target - self.appear) * k;
        if (self.appear - target).abs() < 0.01 {
            self.appear = target;
        }
        if self.appear <= 0.0 {
            return;
        }
        if self.appear != target {
            ctx.request_repaint();
        }
        let appear = self.appear;
        let fade = |c: egui::Color32| c.gamma_multiply(appear);

        let screen = ctx.screen_rect();
        let sw = screen.width();
        let sh = screen.height();

        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("menu"),
        ));
        let pointer_release = ctx.input(|input| {
            input
                .pointer
                .any_released()
                .then(|| input.pointer.latest_pos())
                .flatten()
        });
        let pointer_hover = ctx.input(|input| input.pointer.hover_pos());
        let interactive_release = if self.about_open {
            None
        } else {
            pointer_release
        };

        let backdrop_alpha = (self.backdrop_pct as u16 * 255 / 100) as u8;
        painter.rect_filled(
            screen,
            0.0,
            fade(egui::Color32::from_rgba_premultiplied(
                0,
                0,
                0,
                backdrop_alpha,
            )),
        );
        if let Some(warning) = warning {
            let banner_h = 34.0;
            let rect = Rect::from_min_size(screen.min, Vec2::new(sw, banner_h));
            // A calm info bar, not an alarm: this is setup guidance, not an error.
            painter.rect_filled(rect, 0.0, fade(egui::Color32::from_rgb(28, 42, 50)));
            painter.text(
                Pos2::new(sw / 2.0, banner_h / 2.0),
                Align2::CENTER_CENTER,
                warning,
                FontId::proportional(14.0),
                fade(theme::MENU_ACCENT),
            );
        }

        // Animate horizontal slide.
        let target_x = self.category_index as f32;
        self.anim_x += (target_x - self.anim_x) * ANIM_SPEED * dt;
        if (self.anim_x - target_x).abs() < 0.001 {
            self.anim_x = target_x;
        }

        let center_x = sw / 2.0;
        let center_y = sh * 0.38;

        // Category row. These used to be keyboard/gamepad only even though
        // they look like clickable tabs, which made Settings unreachable by
        // pointer in the browser build.
        let mut clicked_category = None;
        for (i, cat) in self.categories.iter().enumerate() {
            let offset = i as f32 - self.anim_x;
            let x = center_x + offset * CATEGORY_SPACING;
            let is_active = i == self.category_index;
            let size = if is_active {
                ICON_SIZE_ACTIVE
            } else {
                ICON_SIZE_INACTIVE
            };
            let disabled = cat.disabled();
            let color = if disabled {
                // Desktop-only category: always greyed, even when selected.
                fade(theme::MENU_TEXT_DIM.gamma_multiply(0.45))
            } else {
                fade(if is_active {
                    theme::MENU_ACCENT
                } else {
                    theme::MENU_TEXT_DIM
                })
            };

            if x < -50.0 || x > sw + 50.0 {
                continue;
            }
            let hit_rect =
                Rect::from_center_size(Pos2::new(x, center_y + 8.0), Vec2::new(64.0, 72.0));
            let hovered = !self.about_open
                && pointer_hover.is_some_and(|position| hit_rect.contains(position));
            if hovered {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            painter.text(
                Pos2::new(x, center_y),
                Align2::CENTER_CENTER,
                cat.icon.to_string(),
                icons::font(size),
                if hovered && !disabled {
                    fade(theme::MENU_ACCENT)
                } else {
                    color
                },
            );
            if is_active {
                painter.text(
                    Pos2::new(x, center_y + size / 2.0 + 20.0),
                    Align2::CENTER_TOP,
                    cat.name,
                    FontId::proportional(16.0),
                    if disabled {
                        fade(theme::MENU_TEXT_DIM)
                    } else {
                        fade(theme::MENU_TEXT_BRIGHT)
                    },
                );
            }
            if interactive_release.is_some_and(|position| hit_rect.contains(position)) {
                clicked_category = Some(i);
            }
        }
        if let Some(index) = clicked_category {
            self.category_index = index;
            self.item_index = 0;
            self.scroll_y = 0.0;
        }

        // Item list.
        // Reset the selected-row marquee when the selection changes, then
        // advance it (it only actually scrolls labels that overflow).
        let sel_key = (self.category_index, self.item_index);
        if sel_key != self.marquee_key {
            self.marquee_key = sel_key;
            self.marquee_t = 0.0;
        }
        self.marquee_t += dt;
        let marquee_t = self.marquee_t;

        let cat = &self.categories[self.category_index];
        let items_start_y = center_y + ICON_SIZE_ACTIVE + 44.0;
        let label_font = FontId::proportional(15.0);
        let value_font = FontId::proportional(13.0);

        // Auto-size the row box to THIS category's content: fit the widest
        // label fully, plus its action icons, plus a capped value column -- so
        // each category is only as wide as it needs (no global stretch).
        // Values past the cap elide; the window width is the hard ceiling.
        let max_avail = (sw - 2.0 * 40.0).min(ITEM_MAX_WIDTH);
        let measure = |text: &str, font: FontId| {
            line_galley(ctx, text, font, theme::MENU_TEXT_DIM, None)
                .size()
                .x
        };
        let needed = cat.items.iter().fold(0.0_f32, |acc, item| {
            let label_w = measure(&item.label, label_font.clone());
            let value_w = item
                .value
                .as_deref()
                .filter(|v| !v.is_empty())
                .map_or(0.0, |v| measure(v, value_font.clone()).min(VALUE_WIDTH_CAP));
            let has_play =
                matches!(item.action, MenuAction::LaunchGame(_)) && item.burn_action.is_some();
            let action_w = (usize::from(item.burn_action.is_some()) + usize::from(has_play)) as f32
                * ROW_ACTION_WIDTH;
            let value_gap = if value_w > 0.0 { 16.0 } else { 0.0 };
            // 14px left pad + 12px right pad = 26.
            acc.max(26.0 + label_w + value_gap + value_w + action_w)
        });
        let item_width = needed.clamp(ITEM_MIN_WIDTH, max_avail);
        let items_x = center_x - item_width / 2.0;
        let row_stride = ITEM_HEIGHT + ITEM_GAP;

        // How many full rows fit between `items_start_y` and the
        // bottom edge of the screen (with a small bottom margin so
        // the list doesn't butt against the edge).
        //
        // `max(1)` so a degenerate window height (tiny resize during
        // launch) still produces at least one visible row and avoids
        // a divide-by-zero in the visible-count math below.
        let bottom_margin = 16.0;
        let available_h = (sh - items_start_y - bottom_margin).max(row_stride);
        let visible_rows = (available_h / row_stride).floor().max(1.0) as usize;

        // Compute a TARGET scroll position that keeps the selected
        // item visible with a lead-in margin: once you hit row
        // `edge_margin` from the top or bottom, further navigation
        // scrolls the whole list instead of just moving the cursor.
        //
        // For very short lists (num_items ≤ visible_rows) the target
        // is 0 -- nothing to scroll.
        let num_items = cat.items.len();
        let edge_margin: usize = if visible_rows >= 5 { 2 } else { 1 };
        let target_scroll = if num_items <= visible_rows {
            0.0_f32
        } else {
            let max_scroll = (num_items - visible_rows) as f32;
            let sel = self.item_index as f32;
            let top_lead = edge_margin as f32;
            let bottom_lead = (visible_rows - 1 - edge_margin) as f32;
            // Ideal scroll keeps the selected row between
            // [scroll + top_lead, scroll + bottom_lead] inclusive.
            let t = if sel < self.scroll_y + top_lead {
                sel - top_lead
            } else if sel > self.scroll_y + bottom_lead {
                sel - bottom_lead
            } else {
                self.scroll_y
            };
            t.clamp(0.0, max_scroll)
        };

        // Ease `scroll_y` toward the target using the same
        // `ANIM_SPEED * dt` blend that drives the horizontal
        // category slide -- so navigation feels uniform between
        // axes. Snap when we're within a pixel of the target.
        self.scroll_y += (target_scroll - self.scroll_y) * ANIM_SPEED * dt;
        if (self.scroll_y - target_scroll).abs() * row_stride < 0.5 {
            self.scroll_y = target_scroll;
        }

        for (i, item) in cat.items.iter().enumerate() {
            let y = items_start_y + (i as f32 - self.scroll_y) * row_stride;
            let row_bottom = y + ITEM_HEIGHT;
            // Cull items entirely above the list region or below the
            // bottom margin. One row of overhang on each side so the
            // scroll animation doesn't "pop" items in/out at the
            // moment they fully arrive.
            if row_bottom < items_start_y - row_stride || y > sh - bottom_margin {
                continue;
            }
            let rect =
                Rect::from_min_size(Pos2::new(items_x, y), Vec2::new(item_width, ITEM_HEIGHT));
            let action_count = usize::from(item.burn_action.is_some())
                + usize::from(
                    matches!(item.action, MenuAction::LaunchGame(_)) && item.burn_action.is_some(),
                );
            let main_rect = row_main_rect(rect, action_count);
            let row_hovered = !self.about_open
                && pointer_hover.is_some_and(|position| main_rect.contains(position));
            let is_selected = i == self.item_index;
            let highlighted = is_selected || row_hovered;
            if row_hovered {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if interactive_release.is_some_and(|position| main_rect.contains(position)) {
                self.item_index = i;
                self.pending_pointer_action = Some(item.action.clone());
            }

            let bg = fade(if highlighted {
                theme::MENU_ITEM_SEL
            } else {
                theme::MENU_ITEM_BG
            });
            painter.rect_filled(rect, 0.0, bg);

            if is_selected {
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(items_x, y), Vec2::new(3.0, ITEM_HEIGHT)),
                    0.0,
                    fade(theme::MENU_ACCENT),
                );
            }

            // Text lives between `content_left` and the action icons. The value
            // (right) is capped + elided; the label (left) takes the rest --
            // elided when idle, marquee-scrolled when selected and overflowing.
            let launch_action = matches!(item.action, MenuAction::LaunchGame(_))
                .then_some(&item.action)
                .filter(|_| item.burn_action.is_some());
            let content_left = items_x + 14.0;
            let avail_right = items_x + item_width - 12.0 - action_count as f32 * ROW_ACTION_WIDTH;

            let value_galley = item.value.as_deref().filter(|v| !v.is_empty()).map(|val| {
                let vcolor = fade(if highlighted {
                    theme::MENU_TEXT_VALUE
                } else {
                    theme::MENU_TEXT_DIM
                });
                let vmax = VALUE_WIDTH_CAP.min((avail_right - content_left).max(0.0));
                line_galley(ctx, val, value_font.clone(), vcolor, Some(vmax))
            });
            let value_w = value_galley.as_ref().map_or(0.0, |g| g.size().x);
            let value_left = avail_right - value_w;

            let label_color = fade(if highlighted {
                theme::MENU_TEXT_BRIGHT
            } else {
                theme::MENU_TEXT_DIM
            });
            let label_gap = if value_w > 0.0 { 16.0 } else { 0.0 };
            let label_budget = (value_left - label_gap - content_left).max(8.0);
            let full = line_galley(ctx, &item.label, label_font.clone(), label_color, None);
            if is_selected && full.size().x > label_budget {
                let overflow = full.size().x - label_budget;
                let off = marquee_offset(marquee_t, overflow);
                let clip = Rect::from_min_size(
                    Pos2::new(content_left, y),
                    Vec2::new(label_budget, ITEM_HEIGHT),
                );
                let ty = y + (ITEM_HEIGHT - full.size().y) / 2.0;
                painter.with_clip_rect(clip).galley(
                    Pos2::new(content_left - off, ty),
                    full,
                    label_color,
                );
            } else {
                let g = line_galley(
                    ctx,
                    &item.label,
                    label_font.clone(),
                    label_color,
                    Some(label_budget),
                );
                let ty = y + (ITEM_HEIGHT - g.size().y) / 2.0;
                painter.galley(Pos2::new(content_left, ty), g, label_color);
            }

            if let Some(g) = value_galley {
                let ty = y + (ITEM_HEIGHT - g.size().y) / 2.0;
                painter.galley(Pos2::new(value_left, ty), g, fade(theme::MENU_TEXT_DIM));
            }

            let mut action_index = 0;
            if let Some(action) = item.burn_action.as_ref() {
                draw_row_icon_action(
                    ctx,
                    &painter,
                    row_action_rect(items_x, item_width, y, action_index),
                    pointer_hover,
                    interactive_release,
                    icons::DISC,
                    "Burn disc",
                    is_selected,
                    action,
                    &mut self.pending_pointer_action,
                    value_font.clone(),
                    appear,
                );
                action_index += 1;
            }
            if let Some(action) = launch_action {
                draw_row_icon_action(
                    ctx,
                    &painter,
                    row_action_rect(items_x, item_width, y, action_index),
                    pointer_hover,
                    interactive_release,
                    icons::PLAY,
                    "Play",
                    is_selected,
                    action,
                    &mut self.pending_pointer_action,
                    value_font.clone(),
                    appear,
                );
            }
        }

        // Scroll indicators: small triangles at the top/bottom edges
        // of the item strip when there's content outside the visible
        // window. Gives the user an affordance that "there's more
        // here" without waiting for them to hit the edge.
        let indicator_color = fade(theme::MENU_TEXT_DIM);
        let has_above = self.scroll_y > 0.1;
        let has_below = (self.scroll_y + visible_rows as f32) < num_items as f32 - 0.1;
        if has_above {
            painter.text(
                Pos2::new(center_x, items_start_y - 6.0),
                Align2::CENTER_BOTTOM,
                "▲",
                FontId::proportional(10.0),
                indicator_color,
            );
        }
        if has_below {
            painter.text(
                Pos2::new(center_x, sh - bottom_margin + 4.0),
                Align2::CENTER_TOP,
                "▼",
                FontId::proportional(10.0),
                indicator_color,
            );
        }

        // Project framing + WIP/legal notice, shown on every menu screen in
        // both builds.
        painter.text(
            Pos2::new(sw / 2.0, sh - 62.0),
            Align2::CENTER_TOP,
            "The emulator is early, game compatibility is still low.",
            FontId::proportional(11.0),
            fade(theme::MENU_TEXT_DIM),
        );
        painter.text(
            Pos2::new(sw / 2.0, sh - 46.0),
            Align2::CENTER_TOP,
            "PSoXide is an independent, open-source PS1 developer environment. Use only a BIOS and games you legally own.",
            FontId::proportional(11.0),
            fade(theme::MENU_TEXT_DIM),
        );

        // Bottom hint bar.
        painter.text(
            Pos2::new(sw / 2.0, sh - 30.0),
            Align2::CENTER_TOP,
            "Click/Enter: Select   Esc: Close   Arrows: Navigate",
            FontId::proportional(12.0),
            fade(theme::MENU_HINT),
        );

        // About card overlay, painted on top when opened from Settings.
        if self.about_open {
            about_panel(ctx, &mut self.about_open);
        }
    }
}

fn row_action_rect(items_x: f32, item_width: f32, y: f32, index_from_right: usize) -> Rect {
    let right = items_x + item_width - index_from_right as f32 * ROW_ACTION_WIDTH;
    Rect::from_min_size(
        Pos2::new(right - ROW_ACTION_WIDTH, y),
        Vec2::new(ROW_ACTION_WIDTH, ITEM_HEIGHT),
    )
}

/// Clickable body of a row, excluding any dedicated right-side action icons.
fn row_main_rect(rect: Rect, action_count: usize) -> Rect {
    Rect::from_min_max(
        rect.min,
        Pos2::new(
            (rect.right() - action_count as f32 * ROW_ACTION_WIDTH).max(rect.left()),
            rect.bottom(),
        ),
    )
}

/// Lay out one line of text. `max_width = Some(w)` elides to `w` with an
/// ellipsis; `None` lays the full line out (for marquee scrolling).
fn line_galley(
    ctx: &egui::Context,
    text: &str,
    font_id: FontId,
    color: egui::Color32,
    max_width: Option<f32>,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::text::TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.unwrap_or(f32::INFINITY),
        max_rows: 1,
        overflow_character: max_width.map(|_| '…'),
        ..Default::default()
    };
    ctx.fonts(|f| f.layout_job(job))
}

/// Ping-pong marquee offset (px) for an overflowing label: pause, scroll to
/// the end, pause, scroll back. `t` is seconds since the row was selected.
fn marquee_offset(t: f32, overflow: f32) -> f32 {
    const SPEED: f32 = 45.0; // px/s
    const PAUSE: f32 = 1.3; // s held at each end
    let travel = (overflow / SPEED).max(0.001);
    let period = 2.0 * (PAUSE + travel);
    let mut p = t % period;
    if p < PAUSE {
        0.0
    } else if p < PAUSE + travel {
        (p - PAUSE) / travel * overflow
    } else if p < 2.0 * PAUSE + travel {
        overflow
    } else {
        p -= 2.0 * PAUSE + travel;
        overflow - (p / travel) * overflow
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row_icon_action(
    ctx: &egui::Context,
    painter: &egui::Painter,
    rect: Rect,
    pointer_hover: Option<Pos2>,
    pointer_release: Option<Pos2>,
    icon: char,
    tooltip: &str,
    selected: bool,
    action: &MenuAction,
    pending_pointer_action: &mut Option<MenuAction>,
    tooltip_font: FontId,
    alpha: f32,
) {
    let hovered = pointer_hover.is_some_and(|pos| rect.contains(pos));
    if hovered {
        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        let hover_rect = rect.shrink2(Vec2::new(5.0, 5.0));
        painter.rect_filled(
            hover_rect,
            4.0,
            egui::Color32::from_rgba_premultiplied(0, 191, 230, 42).gamma_multiply(alpha),
        );
        painter.rect_stroke(
            hover_rect,
            4.0,
            egui::Stroke::new(1.0, theme::MENU_ACCENT.gamma_multiply(alpha)),
            egui::StrokeKind::Inside,
        );
    }

    let icon_color = (if hovered || selected {
        theme::MENU_ACCENT
    } else {
        theme::MENU_TEXT_DIM
    })
    .gamma_multiply(alpha);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon.to_string(),
        icons::font(15.0),
        icon_color,
    );

    if hovered {
        let width = (tooltip.len() as f32 * 7.0 + 18.0).max(44.0);
        let tooltip_rect = Rect::from_min_size(
            Pos2::new(rect.right() - width - 6.0, rect.top() - 26.0),
            Vec2::new(width, 22.0),
        );
        painter.rect_filled(tooltip_rect, 3.0, theme::MENU_ITEM_BG.gamma_multiply(alpha));
        painter.text(
            tooltip_rect.center(),
            Align2::CENTER_CENTER,
            tooltip,
            tooltip_font,
            theme::MENU_TEXT_BRIGHT.gamma_multiply(alpha),
        );
    }

    if pointer_release.is_some_and(|pos| rect.contains(pos)) {
        *pending_pointer_action = Some(action.clone());
    }
}

/// The save-states panel: a "Save state" action, the pinned "top"
/// entry (what F7/quick-load targets) shown separately above a
/// divider, and a scrollable, newest-first list of every save for the
/// running game. Built from plain egui widgets (`egui::Window` +
/// `ScrollArea`), same reasoning as [`about_panel`] -- this needs
/// real image widgets and a checkbox, which the hand-painted category
/// list beneath it has no machinery for.
///
/// Button clicks feed `pending_pointer_action` -- the same channel
/// `draw_row_icon_action` uses for pointer-driven category rows --
/// rather than returning a value directly, so this integrates with
/// [`MenuState::update`]'s existing "take the pending action next
/// frame" dispatch without a second code path.
fn save_states_panel(
    ctx: &egui::Context,
    open: &mut bool,
    rows: &[SaveStateRow],
    pending_load_confirm: &mut Option<PendingLoadConfirm>,
    thumb_cache: &mut HashMap<PathBuf, egui::TextureHandle>,
    pending_pointer_action: &mut Option<MenuAction>,
) {
    const THUMB_SIZE: Vec2 = Vec2::new(84.0, 63.0);

    let mut still_open = *open;
    egui::Window::new("Save States")
        .open(&mut still_open)
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            let save_label = if cfg!(target_arch = "wasm32") {
                "Save browser quick-state"
            } else {
                "Save state"
            };
            let save_help = if cfg!(target_arch = "wasm32") {
                "Replace this game's persistent browser quick-save (F5)"
            } else {
                "Push a new save (F5) and pin it as the quick-load target"
            };
            if ui
                .add(egui::Button::new(
                    egui::RichText::new(format!("{}  {save_label}", icons::SAVE)).size(14.0),
                ))
                .on_hover_text(save_help)
                .clicked()
            {
                *pending_pointer_action = Some(MenuAction::SaveState);
            }
            ui.add_space(8.0);

            if rows.is_empty() {
                ui.label(
                    egui::RichText::new("No saves yet")
                        .color(theme::MENU_TEXT_DIM)
                        .italics(),
                );
                return;
            }

            if let Some(top) = rows.iter().find(|r| r.is_top) {
                egui::Frame::group(ui.style())
                    .stroke(egui::Stroke::new(1.0, theme::MENU_ACCENT))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            draw_thumb(
                                ui,
                                ctx,
                                thumb_cache,
                                top.thumbnail_path.as_deref(),
                                THUMB_SIZE,
                            );
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("Top -- loads on F7")
                                        .strong()
                                        .color(theme::MENU_ACCENT)
                                        .size(12.0),
                                );
                                ui.label(egui::RichText::new(&top.label).size(13.0));
                                if ui.small_button("Load...").clicked() {
                                    *pending_load_confirm = Some(PendingLoadConfirm {
                                        slot: top.slot,
                                        label: top.label.clone(),
                                        thumbnail_path: top.thumbnail_path.clone(),
                                        resume_paused: true,
                                    });
                                }
                            });
                        });
                    });
            }

            ui.add_space(6.0);
            ui.separator();
            ui.label(
                egui::RichText::new("All saves")
                    .color(theme::MENU_TEXT_DIM)
                    .size(12.0),
            );
            ui.add_space(4.0);

            egui::ScrollArea::vertical()
                .max_height(340.0)
                .show(ui, |ui| {
                    for row in rows {
                        ui.horizontal(|ui| {
                            draw_thumb(
                                ui,
                                ctx,
                                thumb_cache,
                                row.thumbnail_path.as_deref(),
                                THUMB_SIZE,
                            );
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&row.label).size(13.0));
                                ui.horizontal(|ui| {
                                    if ui.small_button("Load...").clicked() {
                                        *pending_load_confirm = Some(PendingLoadConfirm {
                                            slot: row.slot,
                                            label: row.label.clone(),
                                            thumbnail_path: row.thumbnail_path.clone(),
                                            resume_paused: true,
                                        });
                                    }
                                    if row.is_top {
                                        ui.label(
                                            egui::RichText::new("pinned")
                                                .color(theme::MENU_TEXT_DIM)
                                                .size(11.0),
                                        );
                                    } else if ui
                                        .small_button("Pin as top")
                                        .on_hover_text(
                                            "Make this the save F7/quick-load targets, \
                                             without moving it in this list",
                                        )
                                        .clicked()
                                    {
                                        *pending_pointer_action =
                                            Some(MenuAction::PinAsTop(row.slot));
                                    }
                                });
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
        });
    *open = still_open;

    // Load-confirmation sub-modal: staged by a "Load..." click above,
    // not dispatched until the user confirms (and has had a chance to
    // flip "resume paused").
    if let Some(confirm) = pending_load_confirm.clone() {
        let mut keep_confirming = true;
        let mut resume_paused = confirm.resume_paused;
        egui::Window::new("Load this save?")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    draw_thumb(
                        ui,
                        ctx,
                        thumb_cache,
                        confirm.thumbnail_path.as_deref(),
                        THUMB_SIZE,
                    );
                    ui.label(&confirm.label);
                });
                ui.add_space(6.0);
                ui.checkbox(&mut resume_paused, "Resume paused");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Load").clicked() {
                        *pending_pointer_action =
                            Some(MenuAction::LoadState(confirm.slot, resume_paused));
                        keep_confirming = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep_confirming = false;
                    }
                });
            });
        if keep_confirming {
            if resume_paused != confirm.resume_paused {
                *pending_load_confirm = Some(PendingLoadConfirm {
                    resume_paused,
                    ..confirm
                });
            }
        } else {
            *pending_load_confirm = None;
        }
    }
}

/// The controls panel: a painter-drawn PS1 controller whose parts are
/// clickable rebind hotspots -- each showing its current key right on
/// the drawing -- plus a grouped clickable list of every target below
/// it, a live capture banner, and a reset-to-defaults button.
///
/// Controls light up green while their key is physically held, which
/// doubles as an in-app rollover/ghosting tester: hold three keys and
/// see exactly which ones the keyboard actually delivered.
///
/// The silhouette is drawn from the PS1 pad's real construction: two
/// circular pods carrying the d-pad and face cluster, a narrower
/// bridge between them, and capsule grips flaring down-outward.
/// Everything routes through one scale constant so the whole drawing
/// (and its text) can be resized in one place.
///
/// Rebinds don't go through [`MenuAction`]: clicking a hotspot arms
/// `capture`, and the shell's keyboard handler consumes the next
/// physical key into the binding (Escape cancels). Reset does dispatch
/// via `pending_pointer_action`, like every other pointer-driven
/// action.
fn controls_panel(
    ctx: &egui::Context,
    open: &mut bool,
    labels: &HashMap<PadBindTarget, String>,
    held: &[PadBindTarget],
    capture: &mut Option<PadBindTarget>,
    pending_pointer_action: &mut Option<MenuAction>,
) {
    use egui::{Color32, CornerRadius, Sense, Stroke};

    /// One knob for the whole drawing: positions, sizes, and font
    /// sizes below are in a 470x300 design space multiplied by this.
    const S: f32 = 1.35;
    const CANVAS: Vec2 = Vec2::new(470.0 * S, 300.0 * S);

    let bind_of =
        |t: PadBindTarget| -> String { labels.get(&t).cloned().unwrap_or_else(|| "-".to_string()) };
    // Space on the drawing is tight: well-known names compact to
    // glyphs/abbreviations, anything still too long elides. The list
    // and hover text always carry the full name.
    let short_bind = |t: PadBindTarget| -> String {
        let full = bind_of(t);
        // ASCII stand-ins for the arrows: the menu font has no
        // U+2190..2193 glyphs (they render as .notdef boxes).
        let compact = match full.as_str() {
            "ArrowUp" => "^".to_string(),
            "ArrowDown" => "v".to_string(),
            "ArrowLeft" => "<".to_string(),
            "ArrowRight" => ">".to_string(),
            "Backspace" => "Bksp".to_string(),
            "Space" => "Spc".to_string(),
            other => other.replace("Numpad", "Num"),
        };
        if compact.chars().count() > 7 {
            let head: String = compact.chars().take(6).collect();
            format!("{head}\u{2026}")
        } else {
            compact
        }
    };

    // Keep frames coming while the panel is up: the capture pulse
    // animates, and the held-key highlights must track key releases
    // even when the paused game isn't producing new frames itself.
    if capture.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    }

    let mut still_open = *open;
    egui::Window::new("Controls")
        .open(&mut still_open)
        .collapsible(false)
        .resizable(false)
        // Foreground: the Menu overlay paints on a mid-layer painter
        // above ordinary windows, so without this the category list
        // draws straight over the panel when both are up.
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            // Capture banner / hint line above the drawing.
            match *capture {
                Some(target) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  Press a key for {} ... (Esc cancels)",
                            icons::GAMEPAD_2,
                            target.label()
                        ))
                        .color(theme::MENU_ACCENT)
                        .strong()
                        .size(14.0),
                    );
                }
                None => {
                    ui.label(
                        egui::RichText::new(
                            "Click a control, then press the key to bind it. Changes apply \
                             immediately and are saved. Held keys light up green.",
                        )
                        .color(theme::MENU_TEXT_DIM)
                        .size(13.0),
                    );
                }
            }
            ui.add_space(4.0);

            let (resp, painter) = ui.allocate_painter(CANVAS, Sense::hover());
            let origin = resp.rect.min;
            let at = |x: f32, y: f32| Pos2::new(origin.x + x * S, origin.y + y * S);
            let sz = |w: f32, h: f32| Vec2::new(w * S, h * S);
            let sc = |v: f32| v * S;
            // Pulse for the armed hotspot: 2 Hz sine on the ui clock.
            let pulse = ((ui.input(|i| i.time) * std::f64::consts::TAU).sin() * 0.5 + 0.5) as f32;

            let body_fill = Color32::from_rgb(58, 62, 71);
            let body_shade = Color32::from_rgb(48, 52, 60);
            let btn_fill = Color32::from_rgb(30, 33, 39);
            let held_fill = Color32::from_rgba_unmultiplied(70, 190, 110, 70);
            let held_ring = Stroke::new(2.0, Color32::from_rgb(80, 210, 120));
            let bind_text = Color32::from_rgb(150, 200, 235);

            // One clickable region. Paints a held/hover/armed ring,
            // shows the current bind as hover text, and arms the
            // capture on click.
            let mut hotspot = |ui: &mut egui::Ui, rect: Rect, target: PadBindTarget| {
                let id = resp.id.with(target.label());
                let r = ui.interact(rect, id, Sense::click());
                let armed = *capture == Some(target);
                if held.contains(&target) {
                    ui.painter().rect_filled(rect, 4.0, held_fill);
                    ui.painter().rect_stroke(
                        rect.expand(1.0),
                        4.0,
                        held_ring,
                        egui::StrokeKind::Outside,
                    );
                }
                if armed {
                    let ring = Color32::from_rgb(
                        60 + (170.0 * pulse) as u8,
                        180,
                        230 - (80.0 * pulse) as u8,
                    );
                    ui.painter().rect_stroke(
                        rect.expand(2.0),
                        4.0,
                        Stroke::new(2.0, ring),
                        egui::StrokeKind::Outside,
                    );
                } else if r.hovered() {
                    ui.painter().rect_stroke(
                        rect.expand(2.0),
                        4.0,
                        Stroke::new(1.5, theme::MENU_ACCENT),
                        egui::StrokeKind::Outside,
                    );
                }
                let r = r.on_hover_text(format!(
                    "{} - current: {} (click to rebind)",
                    target.label(),
                    bind_of(target)
                ));
                if r.clicked() {
                    *capture = Some(target);
                }
            };

            // Capsule polygon spanning `a`..`b` with radius `r` --
            // used for the grips. Convex by construction.
            let capsule = |a: Pos2, b: Pos2, r: f32| -> Vec<Pos2> {
                let theta = (b - a).angle();
                let mut pts = Vec::with_capacity(26);
                for i in 0..=12 {
                    let phi = theta - std::f32::consts::FRAC_PI_2
                        + std::f32::consts::PI * (i as f32 / 12.0);
                    pts.push(b + r * Vec2::angled(phi));
                }
                for i in 0..=12 {
                    let phi = theta
                        + std::f32::consts::FRAC_PI_2
                        + std::f32::consts::PI * (i as f32 / 12.0);
                    pts.push(a + r * Vec2::angled(phi));
                }
                pts
            };

            // --- Body: grips first (they sit behind), then the two
            // circular pods, then the narrower bridge that joins them.
            painter.add(egui::Shape::convex_polygon(
                capsule(at(96.0, 160.0), at(62.0, 272.0), sc(31.0)),
                body_shade,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                capsule(at(374.0, 160.0), at(408.0, 272.0), sc(31.0)),
                body_shade,
                Stroke::NONE,
            ));
            painter.circle_filled(at(105.0, 128.0), sc(66.0), body_fill);
            painter.circle_filled(at(365.0, 128.0), sc(66.0), body_fill);
            painter.rect_filled(
                Rect::from_min_max(at(105.0, 84.0), at(365.0, 172.0)),
                CornerRadius::same(10),
                body_fill,
            );

            // Shoulder buttons floating above the pods' top edge;
            // bind keys drawn right in the button.
            let shoulder = |painter: &egui::Painter, rect: Rect, name: &str, key: &str| {
                painter.rect_filled(rect, CornerRadius::same(6), btn_fill);
                painter.text(
                    Pos2::new(rect.center().x, rect.center().y - sc(5.0)),
                    Align2::CENTER_CENTER,
                    name,
                    FontId::proportional(sc(9.0)),
                    Color32::from_rgb(200, 205, 215),
                );
                painter.text(
                    Pos2::new(rect.center().x, rect.center().y + sc(6.0)),
                    Align2::CENTER_CENTER,
                    key,
                    FontId::proportional(sc(7.5)),
                    bind_text,
                );
            };
            let l2 = Rect::from_min_max(at(56.0, 8.0), at(120.0, 32.0));
            let l1 = Rect::from_min_max(at(56.0, 38.0), at(120.0, 62.0));
            let r2 = Rect::from_min_max(at(350.0, 8.0), at(414.0, 32.0));
            let r1 = Rect::from_min_max(at(350.0, 38.0), at(414.0, 62.0));
            shoulder(&painter, l2, "L2", &short_bind(PadBindTarget::L2));
            shoulder(&painter, l1, "L1", &short_bind(PadBindTarget::L1));
            shoulder(&painter, r2, "R2", &short_bind(PadBindTarget::R2));
            shoulder(&painter, r1, "R1", &short_bind(PadBindTarget::R1));
            hotspot(ui, l2, PadBindTarget::L2);
            hotspot(ui, l1, PadBindTarget::L1);
            hotspot(ui, r2, PadBindTarget::R2);
            hotspot(ui, r1, PadBindTarget::R1);

            // D-pad on the left pod: cross of two bars, each arm
            // carrying its bind key.
            let dpad_c = at(105.0, 128.0);
            painter.rect_filled(
                Rect::from_center_size(dpad_c, sz(30.0, 92.0)),
                CornerRadius::same(5),
                btn_fill,
            );
            painter.rect_filled(
                Rect::from_center_size(dpad_c, sz(92.0, 30.0)),
                CornerRadius::same(5),
                btn_fill,
            );
            let dpad_arm = |off: Vec2, target: PadBindTarget| {
                let rect = Rect::from_center_size(dpad_c + off * S, sz(30.0, 30.0));
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    short_bind(target),
                    FontId::proportional(sc(7.0)),
                    bind_text,
                );
                rect
            };
            let up_r = dpad_arm(Vec2::new(0.0, -31.0), PadBindTarget::Up);
            let down_r = dpad_arm(Vec2::new(0.0, 31.0), PadBindTarget::Down);
            let left_r = dpad_arm(Vec2::new(-31.0, 0.0), PadBindTarget::Left);
            let right_r = dpad_arm(Vec2::new(31.0, 0.0), PadBindTarget::Right);
            hotspot(ui, up_r, PadBindTarget::Up);
            hotspot(ui, down_r, PadBindTarget::Down);
            hotspot(ui, left_r, PadBindTarget::Left);
            hotspot(ui, right_r, PadBindTarget::Right);

            // Face buttons on the right pod: PS1 symbol colours, bind
            // key under each.
            let face_c = at(365.0, 128.0);
            let face = |painter: &egui::Painter, c: Pos2, sym: PadBindTarget| {
                painter.circle_filled(c, sc(16.0), btn_fill);
                let s = sc(6.5);
                match sym {
                    PadBindTarget::Triangle => {
                        let col = Color32::from_rgb(64, 190, 130);
                        let pts = [
                            Pos2::new(c.x, c.y - s),
                            Pos2::new(c.x - s, c.y + s * 0.8),
                            Pos2::new(c.x + s, c.y + s * 0.8),
                        ];
                        painter.line_segment([pts[0], pts[1]], Stroke::new(2.0, col));
                        painter.line_segment([pts[1], pts[2]], Stroke::new(2.0, col));
                        painter.line_segment([pts[2], pts[0]], Stroke::new(2.0, col));
                    }
                    PadBindTarget::Circle => {
                        painter.circle_stroke(
                            c,
                            s,
                            Stroke::new(2.0, Color32::from_rgb(235, 90, 90)),
                        );
                    }
                    PadBindTarget::Cross => {
                        let col = Color32::from_rgb(120, 150, 235);
                        painter.line_segment(
                            [Pos2::new(c.x - s, c.y - s), Pos2::new(c.x + s, c.y + s)],
                            Stroke::new(2.0, col),
                        );
                        painter.line_segment(
                            [Pos2::new(c.x - s, c.y + s), Pos2::new(c.x + s, c.y - s)],
                            Stroke::new(2.0, col),
                        );
                    }
                    PadBindTarget::Square => {
                        painter.rect_stroke(
                            Rect::from_center_size(c, Vec2::splat(s * 1.7)),
                            CornerRadius::ZERO,
                            Stroke::new(2.0, Color32::from_rgb(230, 130, 200)),
                            egui::StrokeKind::Middle,
                        );
                    }
                    _ => {}
                }
                painter.text(
                    c + Vec2::new(0.0, sc(25.0)),
                    Align2::CENTER_CENTER,
                    short_bind(sym),
                    FontId::proportional(sc(8.0)),
                    bind_text,
                );
            };
            let tri_c = face_c - Vec2::new(0.0, sc(36.0));
            let cross_c = face_c + Vec2::new(0.0, sc(36.0));
            let sq_c = face_c - Vec2::new(sc(36.0), 0.0);
            let cir_c = face_c + Vec2::new(sc(36.0), 0.0);
            face(&painter, tri_c, PadBindTarget::Triangle);
            face(&painter, cross_c, PadBindTarget::Cross);
            face(&painter, sq_c, PadBindTarget::Square);
            face(&painter, cir_c, PadBindTarget::Circle);
            let face_hit = Vec2::splat(sc(32.0));
            hotspot(
                ui,
                Rect::from_center_size(tri_c, face_hit),
                PadBindTarget::Triangle,
            );
            hotspot(
                ui,
                Rect::from_center_size(cross_c, face_hit),
                PadBindTarget::Cross,
            );
            hotspot(
                ui,
                Rect::from_center_size(sq_c, face_hit),
                PadBindTarget::Square,
            );
            hotspot(
                ui,
                Rect::from_center_size(cir_c, face_hit),
                PadBindTarget::Circle,
            );

            // Select / Start / Analog cluster on the bridge, each with
            // its bind key beneath.
            let mid_btn = |painter: &egui::Painter, rect: Rect, name: &str, key: &str| {
                painter.rect_filled(rect, CornerRadius::same(3), btn_fill);
                painter.text(
                    Pos2::new(rect.center().x, rect.top() - sc(6.0)),
                    Align2::CENTER_CENTER,
                    name,
                    FontId::proportional(sc(7.0)),
                    theme::MENU_TEXT_DIM,
                );
                painter.text(
                    Pos2::new(rect.center().x, rect.bottom() + sc(7.0)),
                    Align2::CENTER_CENTER,
                    key,
                    FontId::proportional(sc(7.5)),
                    bind_text,
                );
            };
            let select_r = Rect::from_min_max(at(194.0, 100.0), at(228.0, 113.0));
            let start_r = Rect::from_min_max(at(242.0, 100.0), at(276.0, 113.0));
            mid_btn(
                &painter,
                select_r,
                "SELECT",
                &short_bind(PadBindTarget::Select),
            );
            mid_btn(
                &painter,
                start_r,
                "START",
                &short_bind(PadBindTarget::Start),
            );
            hotspot(ui, select_r, PadBindTarget::Select);
            hotspot(ui, start_r, PadBindTarget::Start);

            let analog_r = Rect::from_min_max(at(217.0, 136.0), at(253.0, 149.0));
            painter.rect_filled(analog_r, CornerRadius::same(3), btn_fill);
            painter.text(
                analog_r.center(),
                Align2::CENTER_CENTER,
                "ANALOG",
                FontId::proportional(sc(6.0)),
                Color32::from_rgb(200, 80, 80),
            );
            painter.text(
                Pos2::new(analog_r.center().x, analog_r.bottom() + sc(7.0)),
                Align2::CENTER_CENTER,
                short_bind(PadBindTarget::Analog),
                FontId::proportional(sc(7.5)),
                bind_text,
            );
            hotspot(ui, analog_r, PadBindTarget::Analog);

            // Analog sticks between the grips, DualShock-style: the
            // circle is the stick click (L3/R3, bind key inside), the
            // four chips around it are the keyboard-emulated stick
            // directions.
            let stick = |ui: &mut egui::Ui,
                         painter: &egui::Painter,
                         hotspot: &mut dyn FnMut(&mut egui::Ui, Rect, PadBindTarget),
                         c: Pos2,
                         click: PadBindTarget,
                         dirs: [PadBindTarget; 4]| {
                painter.circle_filled(c, sc(22.0), btn_fill);
                painter.circle_filled(c, sc(14.0), body_shade);
                painter.text(
                    c - Vec2::new(0.0, sc(5.0)),
                    Align2::CENTER_CENTER,
                    match click {
                        PadBindTarget::L3 => "L3",
                        _ => "R3",
                    },
                    FontId::proportional(sc(8.0)),
                    Color32::from_rgb(200, 205, 215),
                );
                painter.text(
                    c + Vec2::new(0.0, sc(5.0)),
                    Align2::CENTER_CENTER,
                    short_bind(click),
                    FontId::proportional(sc(6.5)),
                    bind_text,
                );
                hotspot(ui, Rect::from_center_size(c, Vec2::splat(sc(30.0))), click);
                let chip = sc(15.0);
                let offs = sc(34.0);
                let dirs_off = [
                    Vec2::new(0.0, -offs),
                    Vec2::new(0.0, offs),
                    Vec2::new(-offs, 0.0),
                    Vec2::new(offs, 0.0),
                ];
                let glyphs = ["^", "v", "<", ">"];
                for ((target, off), glyph) in dirs.iter().zip(dirs_off).zip(glyphs) {
                    let r = Rect::from_center_size(c + off, Vec2::splat(chip));
                    painter.rect_filled(r, CornerRadius::same(3), btn_fill);
                    painter.text(
                        r.center(),
                        Align2::CENTER_CENTER,
                        glyph,
                        FontId::proportional(sc(9.0)),
                        theme::MENU_TEXT_DIM,
                    );
                    hotspot(ui, r, *target);
                }
            };
            stick(
                ui,
                &painter,
                &mut hotspot,
                at(184.0, 218.0),
                PadBindTarget::L3,
                [
                    PadBindTarget::LStickUp,
                    PadBindTarget::LStickDown,
                    PadBindTarget::LStickLeft,
                    PadBindTarget::LStickRight,
                ],
            );
            stick(
                ui,
                &painter,
                &mut hotspot,
                at(286.0, 218.0),
                PadBindTarget::R3,
                [
                    PadBindTarget::RStickUp,
                    PadBindTarget::RStickDown,
                    PadBindTarget::RStickLeft,
                    PadBindTarget::RStickRight,
                ],
            );

            ui.add_space(2.0);
            ui.separator();

            // Grouped clickable list -- same capture flow as the
            // drawing, guaranteed to cover every target, with full
            // (unelided) key names.
            const GROUPS: [(&str, &[PadBindTarget]); 6] = [
                (
                    "D-Pad",
                    &[
                        PadBindTarget::Up,
                        PadBindTarget::Down,
                        PadBindTarget::Left,
                        PadBindTarget::Right,
                    ],
                ),
                (
                    "Face buttons",
                    &[
                        PadBindTarget::Cross,
                        PadBindTarget::Circle,
                        PadBindTarget::Square,
                        PadBindTarget::Triangle,
                    ],
                ),
                (
                    "Shoulders",
                    &[
                        PadBindTarget::L1,
                        PadBindTarget::L2,
                        PadBindTarget::R1,
                        PadBindTarget::R2,
                    ],
                ),
                (
                    "Start / Select",
                    &[PadBindTarget::Start, PadBindTarget::Select],
                ),
                (
                    "Left stick",
                    &[
                        PadBindTarget::L3,
                        PadBindTarget::LStickUp,
                        PadBindTarget::LStickDown,
                        PadBindTarget::LStickLeft,
                        PadBindTarget::LStickRight,
                    ],
                ),
                (
                    "Right stick + DualShock",
                    &[
                        PadBindTarget::R3,
                        PadBindTarget::RStickUp,
                        PadBindTarget::RStickDown,
                        PadBindTarget::RStickLeft,
                        PadBindTarget::RStickRight,
                        PadBindTarget::Analog,
                    ],
                ),
            ];
            egui::ScrollArea::vertical()
                .max_height(170.0)
                .show(ui, |ui| {
                    for (group, targets) in GROUPS {
                        ui.add_space(3.0);
                        ui.label(
                            egui::RichText::new(group)
                                .color(theme::MENU_ACCENT)
                                .size(12.0)
                                .strong(),
                        );
                        egui::Grid::new(group)
                            .num_columns(2)
                            .striped(true)
                            .min_col_width(190.0)
                            .show(ui, |ui| {
                                for &target in targets {
                                    ui.label(egui::RichText::new(target.label()).size(13.0));
                                    let armed = *capture == Some(target);
                                    let text = if armed {
                                        "press a key...".to_string()
                                    } else {
                                        bind_of(target)
                                    };
                                    let btn = egui::Button::new(
                                        egui::RichText::new(text).size(13.0).color(if armed {
                                            theme::MENU_ACCENT
                                        } else {
                                            Color32::from_rgb(220, 224, 232)
                                        }),
                                    )
                                    .min_size(Vec2::new(150.0, 20.0));
                                    if ui.add(btn).clicked() {
                                        *capture = Some(target);
                                    }
                                    ui.end_row();
                                }
                            });
                    }
                });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{}  Reset to defaults", icons::ROTATE_CCW))
                    .clicked()
                {
                    *pending_pointer_action = Some(MenuAction::ResetControls);
                    *capture = None;
                }
                ui.label(
                    egui::RichText::new("Binding a key already in use unbinds its old control.")
                        .color(theme::MENU_TEXT_DIM)
                        .size(11.0),
                );
            });
        });
    *open = still_open;
}
/// Paint a save's thumbnail at `size`, loading and caching the
/// texture on first use. Draws a plain placeholder box when `path` is
/// `None` (no capture for this save) or fails to decode.
fn draw_thumb(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    cache: &mut HashMap<PathBuf, egui::TextureHandle>,
    path: Option<&std::path::Path>,
    size: Vec2,
) {
    let placeholder = |ui: &mut egui::Ui| {
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 4.0, egui::Color32::from_gray(28));
    };
    let Some(path) = path else {
        placeholder(ui);
        return;
    };
    if !cache.contains_key(path) {
        let loaded = std::fs::read(path)
            .ok()
            .and_then(|bytes| image::load_from_memory(&bytes).ok())
            .map(|img| img.to_rgba8());
        if let Some(rgba) = loaded {
            let (w, h) = rgba.dimensions();
            let color_image =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
            let tex = ctx.load_texture(
                format!("savethumb-{}", path.display()),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            cache.insert(path.to_path_buf(), tex);
        }
    }
    match cache.get(path) {
        Some(tex) => {
            ui.add(egui::Image::new((tex.id(), size)));
        }
        None => placeholder(ui),
    }
}

/// The About card: brand mark, build info, and a few real links. Built from
/// egui widgets (not the painter) so the links are first-class clickable
/// `ui.link`s -- much less code than hand-rolled hit-testing. Opened from the
/// Settings "About" row; closes on its Close button, confirm/back, or a click
/// outside it.
fn about_panel(ctx: &egui::Context, open: &mut bool) {
    let logo_tex = crate::ui::splash::logo_texture(ctx);
    let [tw, th] = logo_tex.size();
    let aspect = tw as f32 / th.max(1) as f32;
    let link = |ui: &mut egui::Ui, text: &str, url: &str| {
        if ui
            .link(
                egui::RichText::new(text)
                    .color(theme::MENU_ACCENT)
                    .size(14.0),
            )
            .clicked()
        {
            open_external_url(url);
        }
    };

    let area = egui::Area::new(egui::Id::new("about-card"))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgb(18, 20, 26))
                .stroke(egui::Stroke::new(1.0, theme::MENU_ACCENT))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(30, 26))
                .show(ui, |ui| {
                    ui.set_width(340.0);
                    ui.vertical_centered(|ui| {
                        let w = 210.0;
                        ui.image(egui::load::SizedTexture::new(
                            logo_tex.id(),
                            egui::vec2(w, w / aspect),
                        ));
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION")))
                                .color(theme::MENU_TEXT_BRIGHT),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Independent, open-source PS1 developer environment",
                            )
                            .color(theme::MENU_TEXT_DIM)
                            .size(13.0),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Emulator is early, game compatibility is still low",
                            )
                            .color(theme::MENU_TEXT_DIM)
                            .size(13.0),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("License: GPL-2.0-or-later")
                                .color(theme::MENU_TEXT_DIM)
                                .size(13.0),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Use only a BIOS and games you legally own.")
                                .color(theme::MENU_TEXT_DIM)
                                .size(12.0),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(
                                "PlayStation and PS1 are trademarks of Sony Interactive \
                                 Entertainment. PSoXide is unaffiliated.",
                            )
                            .color(theme::MENU_HINT)
                            .size(11.0),
                        );
                        ui.add_space(4.0);
                        link(
                            ui,
                            "How to dump your own BIOS and discs",
                            // The old Sony_PlayStation page is gone. This one is
                            // the wiki's dumping guide, and it opens by saying
                            // to dump what you own -- which is the point of the
                            // line above it. Emulator_files, the other live
                            // candidate, is a BIOS download index instead.
                            "https://emulation.gametechwiki.com/index.php/Ripping_games",
                        );
                        ui.add_space(16.0);
                        link(
                            ui,
                            "Source code on GitHub",
                            "https://github.com/EBonura/PSoXide",
                        );
                        ui.add_space(4.0);
                        link(
                            ui,
                            "Bonnie Studios on itch.io",
                            "https://bonnie-studios.itch.io/",
                        );
                        ui.add_space(18.0);
                        if ui.button("Close").clicked() {
                            *open = false;
                        }
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Cross / Enter to close")
                                .color(theme::MENU_HINT)
                                .size(11.0),
                        );
                    });
                });
        });

    // A click anywhere outside the card closes it.
    if area.response.clicked_elsewhere() {
        *open = false;
    }
}

/// Open a URL in the user's default browser (native) or a new tab (web).
#[cfg(not(target_arch = "wasm32"))]
fn open_external_url(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(opener).arg(url).spawn() {
        eprintln!("[frontend] open url failed: {e}");
    }
}

#[cfg(target_arch = "wasm32")]
fn open_external_url(url: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.open_with_url_and_target(url, "_blank");
    }
}

/// Emulator settings entry point.
fn build_settings_category() -> Category {
    Category {
        name: "Settings",
        icon: icons::HARD_DRIVE,
        items: vec![
            MenuItem {
                // Native picks a file path; the web build uploads the bytes.
                label: if cfg!(target_arch = "wasm32") {
                    "Load BIOS file"
                } else {
                    "Choose BIOS path"
                }
                .into(),
                action: MenuAction::ChooseBiosPath,
                burn_action: None,
                value: Some("Missing".into()),
            },
            MenuItem {
                label: if cfg!(target_arch = "wasm32") {
                    "Load games folder"
                } else {
                    "Choose games path"
                }
                .into(),
                action: MenuAction::ChooseGamesPath,
                burn_action: None,
                value: Some("Missing".into()),
            },
            MenuItem {
                label: "Menu opacity".into(),
                action: MenuAction::CycleMenuOpacity,
                burn_action: None,
                value: Some("90%".into()),
            },
            // Web only: reload the BIOS + games folder remembered from a
            // previous visit (Chrome/Edge; no-op where unsupported).
            #[cfg(target_arch = "wasm32")]
            MenuItem {
                label: "Reconnect saved files".into(),
                action: MenuAction::Reconnect,
                burn_action: None,
                value: None,
            },
            MenuItem {
                label: "About".into(),
                action: MenuAction::ShowAbout,
                burn_action: None,
                value: None,
            },
        ],
    }
}

/// Construct the Games category from a library snapshot. Empty
/// libraries get a helpful placeholder item so the user
/// understands the category isn't broken, just unpopulated.
fn build_games_category(games: &[LibraryItem]) -> Category {
    let mut items = Vec::with_capacity(games.len() + 2);
    // Web: a quick "load your own game file" entry at the top of Games (the
    // browser has no scanned library folder). Reuses the Settings action.
    #[cfg(target_arch = "wasm32")]
    items.push(MenuItem {
        label: "Load games folder".into(),
        action: MenuAction::ChooseGamesPath,
        burn_action: None,
        value: None,
    });
    if games.is_empty() {
        items.push(MenuItem {
            label: "No games found yet".into(),
            action: MenuAction::RescanLibrary,
            burn_action: None,
            value: Some("Refresh".into()),
        });
    } else {
        for g in games {
            items.push(MenuItem {
                label: g.title.clone(),
                action: MenuAction::LaunchGame(g.id.clone()),
                burn_action: None,
                value: if g.subtitle.is_empty() {
                    None
                } else {
                    Some(g.subtitle.clone())
                },
            });
        }
        // Always offer a rescan at the end of the Games list so the
        // primary entries stay grouped together.
        items.push(MenuItem {
            label: "Refresh library".into(),
            action: MenuAction::RescanLibrary,
            burn_action: None,
            value: Some("↻".into()),
        });
    }
    Category {
        name: "Games",
        icon: icons::DISC,
        items,
    }
}

/// Construct the Projects category. These are project-baked CUE/BIN
/// discs discovered under `editor/projects`, giving authored games their own
/// launch surface.
#[cfg(not(target_arch = "wasm32"))]
fn build_projects_category(projects: &[LibraryItem]) -> Category {
    let mut items = Vec::with_capacity(projects.len() + 1);
    if projects.is_empty() {
        items.push(MenuItem {
            label: "No project builds found".into(),
            action: MenuAction::RescanLibrary,
            burn_action: None,
            value: Some("Refresh".into()),
        });
    } else {
        for p in projects {
            items.push(MenuItem {
                label: p.title.clone(),
                action: MenuAction::LaunchGame(p.id.clone()),
                burn_action: p.burnable.then(|| MenuAction::OpenBurnMenu(p.id.clone())),
                value: if p.subtitle.is_empty() {
                    None
                } else {
                    Some(p.subtitle.clone())
                },
            });
        }
        items.push(MenuItem {
            label: "Refresh library".into(),
            action: MenuAction::RescanLibrary,
            burn_action: None,
            value: Some("↻".into()),
        });
    }
    Category {
        name: "Projects",
        icon: icons::LAYERS,
        items,
    }
}

/// Host-side creation tools.
#[cfg(feature = "editor")]
fn build_create_category(editor_open: bool) -> Category {
    Category {
        name: "Editor",
        icon: icons::FOLDER,
        items: vec![MenuItem {
            label: if editor_open {
                "Close editor workspace".into()
            } else {
                "Open editor workspace".into()
            },
            action: MenuAction::ToggleEditorWorkspace,
            burn_action: None,
            value: Some(if editor_open { "Active" } else { "Studio" }.into()),
        }],
    }
}

/// Web-only stand-in for a desktop-only category (Projects, Create): a greyed
/// icon plus a single non-actionable "not available" row, so the feature is
/// visible but clearly unavailable in the browser.
#[cfg(target_arch = "wasm32")]
fn disabled_category(name: &'static str, icon: char) -> Category {
    Category {
        name,
        icon,
        items: vec![MenuItem {
            label: "Not available in the web build".into(),
            action: MenuAction::Noop,
            burn_action: None,
            value: None,
        }],
    }
}

/// The System category holds emulator-wide actions: run/pause,
/// step, reset. The Games column stays focused on launchable entries,
/// while System carries runtime controls.
///
/// `save_count` is how many saves currently exist for whichever game
/// is running (0 if none, or no game running at all) -- just enough
/// for the row's label; the actual per-save data (thumbnails,
/// pin-to-top, load-with-confirmation) lives in the richer
/// save-states panel opened via [`MenuAction::OpenSaveStates`] --
/// see [`MenuState::sync_save_states`] / [`MenuState::open_save_states`].
fn build_system_category(running: bool, save_count: usize) -> Category {
    let run_label = if running { "Pause" } else { "Run" };
    let save_states_label = if save_count > 0 {
        format!("Save states ({save_count})")
    } else {
        "Save states".to_string()
    };
    let items = vec![
        MenuItem {
            label: run_label.into(),
            action: MenuAction::ToggleRun,
            burn_action: None,
            value: Some("Space".into()),
        },
        MenuItem {
            label: "Step one instruction".into(),
            action: MenuAction::StepOne,
            burn_action: None,
            value: None,
        },
        MenuItem {
            label: "Reset".into(),
            action: MenuAction::Reset,
            burn_action: None,
            value: None,
        },
        MenuItem {
            // Web recordings reboot the game first (cold-boot tapes).
            label: if cfg!(target_arch = "wasm32") {
                "Record input from boot"
            } else {
                "Record input"
            }
            .into(),
            action: MenuAction::ToggleInputRecording,
            burn_action: None,
            value: Some("F8".into()),
        },
        MenuItem {
            label: "Load input replay".into(),
            action: MenuAction::LoadInputReplay,
            burn_action: None,
            value: None,
        },
        MenuItem {
            label: "Fast boot discs".into(),
            action: MenuAction::ToggleFastBoot,
            burn_action: None,
            value: Some("On".into()),
        },
        MenuItem {
            label: save_states_label,
            action: MenuAction::OpenSaveStates,
            burn_action: None,
            value: Some("F5/F7".into()),
        },
        MenuItem {
            label: "Controls".into(),
            action: MenuAction::OpenControls,
            burn_action: None,
            value: None,
        },
    ];
    Category {
        name: "System",
        icon: icons::CPU,
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_item(id: &str, title: &str, sub: &str) -> LibraryItem {
        LibraryItem {
            id: id.into(),
            title: title.into(),
            subtitle: sub.into(),
            #[cfg(not(target_arch = "wasm32"))]
            burnable: false,
        }
    }

    fn category<'a>(menu: &'a MenuState, name: &str) -> &'a Category {
        menu.categories
            .iter()
            .find(|category| category.name == name)
            .unwrap_or_else(|| panic!("missing {name} category"))
    }

    fn draw_pointer_click(menu: &mut MenuState, position: Pos2) {
        let context = egui::Context::default();
        theme::apply(&context);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 720.0))),
            events: vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let _ = context.run(input, |context| menu.draw(context, 1.0, None));
    }

    #[test]
    fn fresh_state_has_expected_categories() {
        let s = MenuState::new();
        let names: Vec<_> = s.categories.iter().map(|category| category.name).collect();
        let mut expected = vec!["Games", "Projects"];
        #[cfg(feature = "editor")]
        expected.push("Editor");
        expected.extend(["Settings", "System", "Quit"]);
        assert_eq!(names, expected);
    }

    #[test]
    fn empty_library_shows_placeholder_that_triggers_rescan() {
        let s = MenuState::new();
        let first = s.categories[0].items.first().unwrap();
        assert_eq!(first.action, MenuAction::RescanLibrary);
        let first_project = category(&s, "Projects").items.first().unwrap();
        assert_eq!(first_project.action, MenuAction::RescanLibrary);
    }

    #[test]
    fn set_library_populates_games_and_projects() {
        let mut s = MenuState::new();
        s.set_library(
            &[dummy_item("g1", "Crash", "NTSC-U · 600 MiB")],
            &[dummy_item("p1", "Stone Room", "Project")],
        );
        assert_eq!(s.categories[0].items[0].label, "Crash");
        assert_eq!(
            s.categories[0].items[0].action,
            MenuAction::LaunchGame("g1".to_string())
        );
        // Refresh row is appended after the actual entries.
        assert_eq!(
            s.categories[0].items.last().unwrap().action,
            MenuAction::RescanLibrary
        );
        assert_eq!(category(&s, "Projects").items[0].label, "Stone Room");
    }

    #[test]
    fn burn_action_is_only_shown_for_burnable_projects() {
        let mut s = MenuState::new();
        let game = LibraryItem {
            burnable: true,
            ..dummy_item("g1", "Retail Disc", "NTSC-U")
        };
        let project = LibraryItem {
            burnable: true,
            ..dummy_item("p1", "Demo 10", "Project")
        };

        s.set_library(&[game], &[project]);

        assert_eq!(s.categories[0].items[0].burn_action, None);
        assert_eq!(
            category(&s, "Projects").items[0].burn_action,
            Some(MenuAction::OpenBurnMenu("p1".to_string()))
        );
    }

    #[test]
    fn set_library_preserves_category_across_rebuild() {
        let mut s = MenuState::new();
        // Move to "System" category before rebuilding.
        s.select_category("System");
        s.set_library(&[], &[]);
        assert_eq!(s.current_category(), Some("System"));
    }

    #[test]
    fn sync_run_label_flips_system_run_item() {
        let mut s = MenuState::new();
        assert_eq!(category(&s, "System").items[0].label, "Run");
        s.sync_run_label(true);
        assert_eq!(category(&s, "System").items[0].label, "Pause");
        s.sync_run_label(false);
        assert_eq!(category(&s, "System").items[0].label, "Run");
    }

    #[test]
    fn sync_fast_boot_label_flips_system_value() {
        let mut s = MenuState::new();
        let fast_boot = category(&s, "System")
            .items
            .iter()
            .find(|item| item.action == MenuAction::ToggleFastBoot)
            .unwrap();
        assert_eq!(fast_boot.value.as_deref(), Some("On"));

        s.sync_fast_boot_label(false);
        let fast_boot = category(&s, "System")
            .items
            .iter()
            .find(|item| item.action == MenuAction::ToggleFastBoot)
            .unwrap();
        assert_eq!(fast_boot.value.as_deref(), Some("Off"));

        s.sync_fast_boot_label(true);
        let fast_boot = category(&s, "System")
            .items
            .iter()
            .find(|item| item.action == MenuAction::ToggleFastBoot)
            .unwrap();
        assert_eq!(fast_boot.value.as_deref(), Some("On"));
    }

    #[test]
    fn left_right_wraps_around_categories() {
        let mut s = MenuState::new();
        s.set_library(&[dummy_item("a", "A", "")], &[]);
        let n = s.categories.len();
        assert!(n >= 2);
        assert_eq!(s.current_category(), Some("Games")); // first category

        let left = MenuInput {
            left: true,
            ..Default::default()
        };
        let right = MenuInput {
            right: true,
            ..Default::default()
        };

        // Left from the first category wraps to the last.
        s.update(&left);
        assert_eq!(s.current_category(), s.categories.last().map(|c| c.name));
        // Right from the last wraps back to the first.
        s.update(&right);
        assert_eq!(s.current_category(), Some("Games"));
        // A full lap of rights returns to the start.
        for _ in 0..n {
            s.update(&right);
        }
        assert_eq!(s.current_category(), Some("Games"));
    }

    #[test]
    fn vertical_navigation_wraps_within_category() {
        let mut s = MenuState::new();
        s.set_library(
            &[
                dummy_item("a", "A", ""),
                dummy_item("b", "B", ""),
                dummy_item("c", "C", ""),
            ],
            &[],
        );

        let up = MenuInput {
            up: true,
            ..Default::default()
        };
        s.update(&up);
        assert_eq!(s.selected_action(), Some(&MenuAction::RescanLibrary));

        let down = MenuInput {
            down: true,
            ..Default::default()
        };
        s.update(&down);
        assert_eq!(
            s.selected_action(),
            Some(&MenuAction::LaunchGame("a".to_string()))
        );
    }

    #[test]
    fn select_category_moves_to_settings() {
        let mut s = MenuState::new();
        s.select_category("Settings");
        assert_eq!(s.current_category(), Some("Settings"));
        assert_eq!(s.selected_action(), Some(&MenuAction::ChooseBiosPath));
    }

    #[test]
    fn pointer_click_on_row_queues_its_action() {
        let mut s = MenuState::new();
        s.select_category("Settings");
        let first_row_center = Pos2::new(480.0, 720.0 * 0.38 + ICON_SIZE_ACTIVE + 64.0);

        draw_pointer_click(&mut s, first_row_center);

        assert_eq!(
            s.take_pending_pointer_action(),
            Some(MenuAction::ChooseBiosPath)
        );
    }

    #[test]
    fn pointer_click_on_category_selects_it() {
        let mut s = MenuState::new();
        let settings_index = s
            .categories
            .iter()
            .position(|category| category.name == "Settings")
            .unwrap();
        let settings_icon = Pos2::new(
            480.0 + settings_index as f32 * CATEGORY_SPACING,
            720.0 * 0.38,
        );

        draw_pointer_click(&mut s, settings_icon);

        assert_eq!(s.current_category(), Some("Settings"));
    }

    #[test]
    fn sync_settings_paths_updates_menu_values() {
        let mut s = MenuState::new();
        s.sync_settings_paths("SCPH1001.BIN", "discs");
        assert_eq!(
            category(&s, "Projects").items[0].value.as_deref(),
            Some("Refresh")
        );
        assert_eq!(
            category(&s, "Settings").items[0].value.as_deref(),
            Some("SCPH1001.BIN")
        );
        assert_eq!(
            category(&s, "Settings").items[1].value.as_deref(),
            Some("discs")
        );
    }
}
