//! Menu overlay -- the launcher / pause shell drawn over the framebuffer.
//!
//! Horizontal animated category icons with a vertical item list beneath
//! the active category. Drawn via `egui::Painter` on a middle layer so
//! it overlays the framebuffer/central area but sits below the HUD.
//!
//! Navigation: arrows + Enter + Escape (gamepad will land when the
//! input subsystem does). Escape also toggles the overlay open/closed.
//!
//! Categories: Games / Examples / Projects / Settings / Create / System
//! (Projects + Create are editor/native-only). The debug sidebar is
//! toggled from the toolbar, not the menu.

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
    /// Toggle warm SYSTEM.CNF disc fast boot. When disabled, discs
    /// boot through the full BIOS logo path.
    ToggleFastBoot,
    /// Launch a game by its menu launch token. Retail games use the
    /// stable library ID; authored project builds use a path-qualified
    /// token so projects sharing the same PSX volume ID remain distinct.
    LaunchGame(String),
    /// Open the CD burn submenu for a launchable example/project disc.
    OpenBurnMenu(String),
    /// Re-walk the configured library root and refresh
    /// `library.ron`. Surfaced as a "Refresh library" item in
    /// the Games / Examples categories so users can trigger a
    /// rescan without leaving the Menu.
    RescanLibrary,
    /// Build all public SDK/engine examples, then rescan the
    /// library once the background make job completes.
    BuildExamples,
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
    /// Quit the application.
    Quit,
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
    pending_pointer_action: Option<MenuAction>,
    categories: Vec<Category>,
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
    pub burnable: bool,
    /// Whether confirming the row should launch a built artifact.
    pub launchable: bool,
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
            build_examples_category(&[]),
            // Projects are editor-authored and filesystem-backed; the web build
            // has neither, so the category is dropped there.
            #[cfg(not(target_arch = "wasm32"))]
            build_projects_category(&[]),
            build_settings_category(),
            // The Create category is the entry point into the host editor
            // workspace; it is absent in emulator-only builds.
            #[cfg(feature = "editor")]
            build_create_category(false),
            build_system_category(running),
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
            pending_pointer_action: None,
            categories,
        }
    }

    /// Rebuild the Games + Examples + Projects categories from a library
    /// snapshot. Call after load, after a rescan, and whenever the
    /// library changes. Existing selection is preserved when
    /// possible (same category + in-range item) and clamped to the
    /// new bounds otherwise.
    pub fn set_library(
        &mut self,
        games: &[LibraryItem],
        examples: &[LibraryItem],
        projects: &[LibraryItem],
    ) {
        // Snapshot the current selection's category NAME so we can
        // re-resolve after rebuilding (indices may change).
        let current_cat_name = self
            .categories
            .get(self.category_index)
            .map(|c| c.name)
            .unwrap_or("");

        if let Some(games_cat) = self.categories.first_mut() {
            *games_cat = build_games_category(games);
        }
        if let Some(examples_cat) = self.categories.get_mut(1) {
            *examples_cat = build_examples_category(examples);
        }
        // The web build has no Projects category (see `with_running`), so the
        // index-2 slot is Settings there; only update Projects off-web.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(projects_cat) = self.categories.get_mut(2) {
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

    /// Update the Create category label for the current workspace.
    #[cfg(feature = "editor")]
    pub fn sync_editor_label(&mut self, editor_open: bool) {
        if let Some(create) = self.categories.iter_mut().find(|c| c.name == "Create") {
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
        // game/example/demo boots) can't collapse the fade into a hard cut --
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

        let backdrop_alpha = (self.backdrop_pct as u16 * 255 / 100) as u8;
        painter.rect_filled(
            screen,
            0.0,
            fade(egui::Color32::from_rgba_premultiplied(0, 0, 0, backdrop_alpha)),
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

        // Category row.
        for (i, cat) in self.categories.iter().enumerate() {
            let offset = i as f32 - self.anim_x;
            let x = center_x + offset * CATEGORY_SPACING;
            let is_active = i == self.category_index;
            let size = if is_active {
                ICON_SIZE_ACTIVE
            } else {
                ICON_SIZE_INACTIVE
            };
            let color = fade(if is_active {
                theme::MENU_ACCENT
            } else {
                theme::MENU_TEXT_DIM
            });

            if x < -50.0 || x > sw + 50.0 {
                continue;
            }
            painter.text(
                Pos2::new(x, center_y),
                Align2::CENTER_CENTER,
                cat.icon.to_string(),
                icons::font(size),
                color,
            );
            if is_active {
                painter.text(
                    Pos2::new(x, center_y + size / 2.0 + 20.0),
                    Align2::CENTER_TOP,
                    cat.name,
                    FontId::proportional(16.0),
                    fade(theme::MENU_TEXT_BRIGHT),
                );
            }
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
            line_galley(ctx, text, font, theme::MENU_TEXT_DIM, None).size().x
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
        let pointer_release = ctx.input(|input| {
            input
                .pointer
                .any_released()
                .then(|| input.pointer.latest_pos())
                .flatten()
        });
        let pointer_hover = ctx.input(|input| input.pointer.hover_pos());

        // Top-right brand mark. Clicking it toggles the About card. Drawn here so
        // it can reuse the pointer release/hover already gathered above. It is run
        // through `fade`, so it joins the open/close dissolve.
        let logo_tex = crate::ui::splash::logo_texture(ctx);
        let [logo_tw, logo_th] = logo_tex.size();
        let logo_aspect = logo_tw as f32 / logo_th.max(1) as f32;
        let logo_h = 22.0;
        let logo_w = logo_h * logo_aspect;
        // Drop below the setup banner when one is showing, else hug the top.
        let logo_top = if warning.is_some() { 42.0 } else { 14.0 };
        let logo_rect = Rect::from_min_size(
            Pos2::new(sw - 16.0 - logo_w, logo_top),
            Vec2::new(logo_w, logo_h),
        );
        let logo_hovered = pointer_hover.is_some_and(|p| logo_rect.contains(p));
        if logo_hovered {
            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        painter.image(
            logo_tex.id(),
            logo_rect,
            Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
            fade(egui::Color32::from_white_alpha(
                if logo_hovered || self.about_open {
                    255
                } else {
                    190
                },
            )),
        );
        let logo_clicked = pointer_release.is_some_and(|p| logo_rect.contains(p));
        if logo_clicked {
            self.about_open = !self.about_open;
        }
        // While the About card is up, swallow row-icon clicks behind it.
        let row_release = if self.about_open { None } else { pointer_release };

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
            let is_selected = i == self.item_index;

            let bg = fade(if is_selected {
                theme::MENU_ITEM_SEL
            } else {
                theme::MENU_ITEM_BG
            });
            let rect =
                Rect::from_min_size(Pos2::new(items_x, y), Vec2::new(item_width, ITEM_HEIGHT));
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
            let action_count =
                usize::from(item.burn_action.is_some()) + usize::from(launch_action.is_some());
            let content_left = items_x + 14.0;
            let avail_right = items_x + item_width - 12.0 - action_count as f32 * ROW_ACTION_WIDTH;

            let value_galley = item.value.as_deref().filter(|v| !v.is_empty()).map(|val| {
                let vcolor = fade(if is_selected {
                    theme::MENU_TEXT_VALUE
                } else {
                    theme::MENU_TEXT_DIM
                });
                let vmax = VALUE_WIDTH_CAP.min((avail_right - content_left).max(0.0));
                line_galley(ctx, val, value_font.clone(), vcolor, Some(vmax))
            });
            let value_w = value_galley.as_ref().map_or(0.0, |g| g.size().x);
            let value_left = avail_right - value_w;

            let label_color = fade(if is_selected {
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
                painter
                    .with_clip_rect(clip)
                    .galley(Pos2::new(content_left - off, ty), full, label_color);
            } else {
                let g =
                    line_galley(ctx, &item.label, label_font.clone(), label_color, Some(label_budget));
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
                    row_release,
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
                    row_release,
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

        // Independence + legal-ownership notice, shown on every menu screen
        // in both builds. Deliberately names no third-party marks.
        painter.text(
            Pos2::new(sw / 2.0, sh - 46.0),
            Align2::CENTER_TOP,
            "PSoXide is an independent, open-source emulator. Use only a BIOS and games you legally own.",
            FontId::proportional(11.0),
            fade(theme::MENU_TEXT_DIM),
        );

        // Bottom hint bar.
        painter.text(
            Pos2::new(sw / 2.0, sh - 30.0),
            Align2::CENTER_TOP,
            "Enter: Select   Esc: Close   Arrows: Navigate",
            FontId::proportional(12.0),
            fade(theme::MENU_HINT),
        );

        // About card overlay (mouse-driven), painted on top when toggled.
        if self.about_open {
            about_panel(ctx, &mut self.about_open, logo_clicked);
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

/// The About card: brand mark, build info, and a few real links. Built from
/// egui widgets (not the painter) so the links are first-class clickable
/// `ui.link`s -- much less code than hand-rolled hit-testing. Mouse-driven;
/// closes on its Close button, a second logo click, or a click outside it.
fn about_panel(ctx: &egui::Context, open: &mut bool, logo_clicked: bool) {
    let logo_tex = crate::ui::splash::logo_texture(ctx);
    let [tw, th] = logo_tex.size();
    let aspect = tw as f32 / th.max(1) as f32;
    let link = |ui: &mut egui::Ui, text: &str, url: &str| {
        if ui
            .link(egui::RichText::new(text).color(theme::MENU_ACCENT).size(14.0))
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
                    ui.set_width(300.0);
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
                            egui::RichText::new("Independent, open-source emulator")
                                .color(theme::MENU_TEXT_DIM)
                                .size(13.0),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("License: GPL-2.0-or-later")
                                .color(theme::MENU_TEXT_DIM)
                                .size(13.0),
                        );
                        ui.label(
                            egui::RichText::new("Built on PCSX-Redux")
                                .color(theme::MENU_TEXT_DIM)
                                .size(13.0),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("Use only a BIOS and games you legally own.")
                                .color(theme::MENU_TEXT_DIM)
                                .size(12.0),
                        );
                        ui.add_space(16.0);
                        link(ui, "Source code on GitHub", "https://github.com/EBonura/PSoXide");
                        ui.add_space(4.0);
                        link(ui, "Play in your browser", "https://ebonura.github.io/PSoXide/");
                        ui.add_space(4.0);
                        link(ui, "Created by EBonura", "https://github.com/EBonura");
                        ui.add_space(18.0);
                        if ui.button("Close").clicked() {
                            *open = false;
                        }
                    });
                });
        });

    // A click anywhere outside the card closes it -- except the logo click that
    // toggled it this very frame (which would otherwise re-close immediately).
    if !logo_clicked && area.response.clicked_elsewhere() {
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

/// Construct the Examples category. Built examples launch from CUE/BIN
/// discs; source placeholders are supplied by the app layer after it
/// scans `sdk/examples` and `engine/examples`.
fn build_examples_category(examples: &[LibraryItem]) -> Category {
    let mut items = Vec::with_capacity(examples.len() + 2);
    if examples.is_empty() {
        items.push(MenuItem {
            label: "Build public examples".into(),
            action: MenuAction::BuildExamples,
            burn_action: None,
            value: Some("make examples".into()),
        });
        items.push(MenuItem {
            label: "Refresh library".into(),
            action: MenuAction::RescanLibrary,
            burn_action: None,
            value: Some("↻".into()),
        });
    } else {
        for e in examples {
            items.push(MenuItem {
                label: e.title.clone(),
                action: if e.launchable {
                    MenuAction::LaunchGame(e.id.clone())
                } else {
                    MenuAction::BuildExamples
                },
                burn_action: (e.launchable && e.burnable)
                    .then(|| MenuAction::OpenBurnMenu(e.id.clone())),
                value: if e.subtitle.is_empty() {
                    None
                } else {
                    Some(e.subtitle.clone())
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
        name: "Examples",
        icon: icons::FOLDER,
        items,
    }
}

/// Construct the Projects category. These are project-baked CUE/BIN
/// discs discovered under `editor/projects`, separated from SDK
/// examples so authored games have their own launch surface.
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
        name: "Create",
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

/// The System category holds emulator-wide actions: run/pause,
/// step, reset. The Games column stays focused on launchable entries,
/// while System carries runtime controls.
fn build_system_category(running: bool) -> Category {
    let run_label = if running { "Pause" } else { "Run" };
    Category {
        name: "System",
        icon: icons::CPU,
        items: vec![
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
                label: "Fast boot discs".into(),
                action: MenuAction::ToggleFastBoot,
                burn_action: None,
                value: Some("On".into()),
            },
        ],
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
            burnable: false,
            launchable: true,
        }
    }

    #[test]
    fn fresh_state_has_expected_categories() {
        let s = MenuState::new();
        assert_eq!(s.categories.len(), 7);
        assert_eq!(s.categories[0].name, "Games");
        assert_eq!(s.categories[1].name, "Examples");
        assert_eq!(s.categories[2].name, "Projects");
        assert_eq!(s.categories[3].name, "Settings");
        assert_eq!(s.categories[4].name, "Create");
        assert_eq!(s.categories[5].name, "System");
        assert_eq!(s.categories[6].name, "Quit");
    }

    #[test]
    fn empty_library_shows_placeholder_that_triggers_rescan() {
        let s = MenuState::new();
        let first = s.categories[0].items.first().unwrap();
        assert_eq!(first.action, MenuAction::RescanLibrary);
        let first_example = s.categories[1].items.first().unwrap();
        assert_eq!(first_example.action, MenuAction::BuildExamples);
    }

    #[test]
    fn set_library_populates_games_and_examples() {
        let mut s = MenuState::new();
        s.set_library(
            &[dummy_item("g1", "Crash", "NTSC-U · 600 MiB")],
            &[dummy_item("e1", "hello-tri", "EXE")],
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
        assert_eq!(s.categories[1].items[0].label, "hello-tri");
        assert_eq!(s.categories[2].items[0].label, "Stone Room");
    }

    #[test]
    fn burn_action_is_only_shown_for_burnable_examples_and_projects() {
        let mut s = MenuState::new();
        let game = LibraryItem {
            burnable: true,
            ..dummy_item("g1", "Retail Disc", "NTSC-U")
        };
        let example = LibraryItem {
            burnable: true,
            ..dummy_item("e1", "hello-cdda", "CUE")
        };
        let source_example = LibraryItem {
            launchable: false,
            burnable: true,
            ..dummy_item("e2", "hello-tri", "not built")
        };
        let project = LibraryItem {
            burnable: true,
            ..dummy_item("p1", "Demo 10", "Project")
        };

        s.set_library(&[game], &[example, source_example], &[project]);

        assert_eq!(s.categories[0].items[0].burn_action, None);
        assert_eq!(
            s.categories[1].items[0].burn_action,
            Some(MenuAction::OpenBurnMenu("e1".to_string()))
        );
        assert_eq!(s.categories[1].items[1].burn_action, None);
        assert_eq!(
            s.categories[2].items[0].burn_action,
            Some(MenuAction::OpenBurnMenu("p1".to_string()))
        );
    }

    #[test]
    fn set_library_preserves_category_across_rebuild() {
        let mut s = MenuState::new();
        // Move to "System" category before rebuilding.
        s.category_index = 5;
        s.set_library(&[], &[], &[]);
        assert_eq!(s.current_category(), Some("System"));
    }

    #[test]
    fn sync_run_label_flips_system_run_item() {
        let mut s = MenuState::new();
        assert_eq!(s.categories[5].items[0].label, "Run");
        s.sync_run_label(true);
        assert_eq!(s.categories[5].items[0].label, "Pause");
        s.sync_run_label(false);
        assert_eq!(s.categories[5].items[0].label, "Run");
    }

    #[test]
    fn sync_fast_boot_label_flips_system_value() {
        let mut s = MenuState::new();
        let fast_boot = s.categories[5]
            .items
            .iter()
            .find(|item| item.action == MenuAction::ToggleFastBoot)
            .unwrap();
        assert_eq!(fast_boot.value.as_deref(), Some("On"));

        s.sync_fast_boot_label(false);
        let fast_boot = s.categories[5]
            .items
            .iter()
            .find(|item| item.action == MenuAction::ToggleFastBoot)
            .unwrap();
        assert_eq!(fast_boot.value.as_deref(), Some("Off"));

        s.sync_fast_boot_label(true);
        let fast_boot = s.categories[5]
            .items
            .iter()
            .find(|item| item.action == MenuAction::ToggleFastBoot)
            .unwrap();
        assert_eq!(fast_boot.value.as_deref(), Some("On"));
    }

    #[test]
    fn left_right_wraps_around_categories() {
        let mut s = MenuState::new();
        s.set_library(&[dummy_item("a", "A", "")], &[], &[]);
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
    fn sync_settings_paths_updates_menu_values() {
        let mut s = MenuState::new();
        s.sync_settings_paths("SCPH1001.BIN", "discs");
        assert_eq!(s.categories[2].items[0].value.as_deref(), Some("Refresh"));
        assert_eq!(
            s.categories[3].items[0].value.as_deref(),
            Some("SCPH1001.BIN")
        );
        assert_eq!(s.categories[3].items[1].value.as_deref(), Some("discs"));
    }
}
