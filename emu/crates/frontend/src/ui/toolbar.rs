//! Top-of-window toolbar.
//!
//! Two halves on a single strip:
//!
//! - **Left**: live emulator status. Colored dot + RUNNING/PAUSED
//!   label, followed by PS1 emulation cadence, paced visual cadence
//!   when available, host redraw rate, MIPS, frame-time, and
//!   audio backlog.
//! - **Right**: icon buttons. Play/pause (context-sensitive),
//!   reset, advance-one-frame. Clicking fires the same state
//!   transition as the equivalent Menu menu item.
//!
//! Layout shape:
//!
//! ```text
//!  ┌────────────────────────────────────────────────────────────────┐
//!  │ ● RUNNING    EMU 60.0   VIS 20.0   HOST 120.0        ▶  ⟲  ⇥ │
//!  └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The left and right halves are laid out into separate clipped
//! rectangles. The metrics are allowed to become less verbose as the
//! window narrows, but the icon controls never paint over text.

use egui::{Align, Button, Color32, Context, Label, Layout, Rect, RichText, TopBottomPanel};

use crate::app::{self, AppState, ScaleMode};
use crate::icons;

/// Icon font size in the button row.
const ICON_SIZE: f32 = 16.0;
/// Minimum clickable size for each icon button. egui will grow the
/// button to fit its content but won't shrink below this.
const BUTTON_SIZE: egui::Vec2 = egui::vec2(30.0, 26.0);
/// Exact height of the toolbar panel.
const BAR_HEIGHT: f32 = 34.0;
/// Horizontal padding inside the toolbar strip.
const TOOLBAR_MARGIN_X: f32 = 8.0;
/// Gap between the metrics lane and the controls lane.
const TOOLBAR_CLUSTER_GAP: f32 = 8.0;
/// Right-side lane: transport, volume slider, BIOS toggle, debug tools. The
/// controls render right-anchored and are clipped to this width, so it must
/// comfortably exceed their natural content width -- otherwise the leftmost
/// button (the native/high-res scale toggle) loses its left edge to the clip.
const CONTROLS_WIDTH: f32 = 580.0;
/// Keep enough room for the status dot + RUNNING/PAUSED label.
const METRICS_MIN_WIDTH: f32 = 116.0;
/// Slider width used in the toolbar.
const AUDIO_SLIDER_WIDTH: f32 = 72.0;

/// Text size for the left-hand metrics cluster. Slightly smaller than
/// the default UI font so the three values fit without wrapping on a
/// narrow window.
const METRIC_TEXT_SIZE: f32 = 12.0;

/// Running / paused status colours. Green when the CPU is stepping,
/// dim-text when paused. Kept out of `theme.rs` for now -- it's the
/// only place these get used.
const STATUS_RUNNING: Color32 = Color32::from_rgb(80, 200, 120);
const STATUS_PAUSED: Color32 = Color32::from_rgb(153, 153, 166);
const METRIC_TEXT: Color32 = Color32::from_rgb(204, 204, 217);
const METRIC_LABEL: Color32 = Color32::from_rgb(102, 102, 115);

/// Paint the top toolbar. Called once per frame before the central
/// panel so the framebuffer clips underneath it.
///
/// The bar slides up when hidden: the panel height animates to 0 while
/// the row stays anchored to the panel's bottom edge, so the whole
/// cluster translates up and clips off under the window top. A small
/// floating tab in the top-right corner brings it back.
pub fn draw(ctx: &Context, state: &mut AppState) {
    // 0.0 = fully shown, 1.0 = fully hidden.
    let t = ctx.animate_bool_with_time(
        egui::Id::new("toolbar_slide"),
        state.toolbar_hidden,
        0.22,
    );
    let height = (BAR_HEIGHT * (1.0 - t)).max(0.0);

    if height >= 0.5 {
        TopBottomPanel::top("toolbar")
            .resizable(false)
            .exact_height(height)
            .show(ctx, |ui| {
                let panel_rect = ui.max_rect();
                // Anchor the row to the panel bottom at full height; the
                // shrinking panel clips the overflow above -> slide-up.
                let row_top = panel_rect.bottom() - BAR_HEIGHT;
                let row_left = panel_rect.left() + TOOLBAR_MARGIN_X;
                let row_right = (panel_rect.right() - TOOLBAR_MARGIN_X).max(row_left);
                let row_rect = Rect::from_min_max(
                    egui::pos2(row_left, row_top),
                    egui::pos2(row_right, panel_rect.bottom()),
                );

                let controls_width = CONTROLS_WIDTH
                    .min((row_rect.width() - METRICS_MIN_WIDTH - TOOLBAR_CLUSTER_GAP).max(0.0));
                let controls_left = (row_rect.right() - controls_width).max(row_rect.left());
                let metrics_right = (controls_left - TOOLBAR_CLUSTER_GAP).max(row_rect.left());

                let metrics_rect = Rect::from_min_max(
                    egui::pos2(row_rect.left(), row_rect.top()),
                    egui::pos2(metrics_right, row_rect.bottom()),
                );
                let controls_rect = Rect::from_min_max(
                    egui::pos2(controls_left, row_rect.top()),
                    egui::pos2(row_rect.right(), row_rect.bottom()),
                );

                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(metrics_rect)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        ui.set_clip_rect(metrics_rect.intersect(panel_rect));
                        ui.set_width(metrics_rect.width());
                        ui.set_height(metrics_rect.height());
                        draw_metrics(ui, state, metrics_rect.width());
                    },
                );

                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(controls_rect)
                        .layout(Layout::right_to_left(Align::Center)),
                    |ui| {
                        ui.set_clip_rect(controls_rect.intersect(panel_rect));
                        ui.set_width(controls_rect.width());
                        ui.set_height(controls_rect.height());
                        draw_toolbar_controls(ui, state);
                    },
                );
            });
    }

    // Fade the restore tab in only once the bar has mostly cleared, so it
    // never overlaps the sliding toolbar.
    if t > 0.5 {
        draw_restore_tab(ctx, state, ((t - 0.5) / 0.5).clamp(0.0, 1.0));
    }
}

/// Floating tab shown when the toolbar is hidden. Anchored to the
/// top-right so it sits over the framebuffer corner; clicking pulls the
/// bar back down. `opacity` fades it in with the slide.
fn draw_restore_tab(ctx: &Context, state: &mut AppState, opacity: f32) {
    egui::Area::new(egui::Id::new("toolbar_restore"))
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-6.0, 6.0))
        .show(ctx, |ui| {
            ui.set_opacity(opacity);
            egui::Frame::new()
                .fill(ui.visuals().panel_fill)
                .corner_radius(6.0)
                .inner_margin(egui::Margin::symmetric(2, 0))
                .show(ui, |ui| {
                    let btn = icon_button(icons::CARET_DOWN);
                    if ui.add(btn).on_hover_text("Show toolbar").clicked() {
                        state.toolbar_hidden = false;
                    }
                });
        });
}

fn draw_toolbar_controls(ui: &mut egui::Ui, state: &mut AppState) {
    // Right-to-left layout: first added sits furthest right.
    let hide_btn = icon_button(icons::CARET_UP);
    if ui.add(hide_btn).on_hover_text("Hide toolbar").clicked() {
        state.toolbar_hidden = true;
    }
    ui.add_space(TOOLBAR_CLUSTER_GAP);
    draw_buttons(ui, state);
    ui.add_space(TOOLBAR_CLUSTER_GAP);
    draw_audio_controls(ui, state);
    ui.add_space(TOOLBAR_CLUSTER_GAP);
    draw_boot_toggle(ui, state);
    ui.add_space(TOOLBAR_CLUSTER_GAP);
    draw_debug_toggles(ui, state);
}

fn toolbar_label(text: impl Into<String>, color: Color32) -> Label {
    Label::new(
        RichText::new(text.into())
            .color(color)
            .size(METRIC_TEXT_SIZE),
    )
    .truncate()
}

fn toolbar_value(text: impl Into<String>) -> Label {
    Label::new(
        RichText::new(text.into())
            .color(METRIC_TEXT)
            .monospace()
            .size(METRIC_TEXT_SIZE),
    )
    .truncate()
}

fn metric_gap(ui: &mut egui::Ui) {
    ui.add_space(10.0);
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

fn maybe_metric(
    ui: &mut egui::Ui,
    available_width: f32,
    threshold: f32,
    label: &str,
    value: String,
) {
    if available_width >= threshold {
        metric_gap(ui);
        metric(ui, label, value);
    }
}

/// Volume control: mute button plus gain slider.
fn draw_audio_controls(ui: &mut egui::Ui, state: &mut AppState) {
    let effective = state.effective_audio_volume();
    let icon = if effective <= 0.0 {
        icons::VOLUME_X
    } else if effective < 0.6 {
        icons::VOLUME_1
    } else {
        icons::VOLUME_2
    };
    let btn = toggle_button(icon, !state.audio_muted && state.audio_volume > 0.0);
    if ui.add(btn).on_hover_text("Mute / unmute audio").clicked() {
        state.audio_muted = !state.audio_muted;
        state.status_message_set(if state.audio_muted {
            "Audio muted"
        } else {
            "Audio unmuted"
        });
    }

    let before = state.audio_volume;
    let slider = egui::Slider::new(&mut state.audio_volume, 0.0..=1.5)
        .show_value(false)
        .clamping(egui::SliderClamping::Always);
    let response = ui
        .add_sized([AUDIO_SLIDER_WIDTH, 18.0], slider)
        .on_hover_text("Volume");
    if response.changed() {
        state.audio_volume = state.audio_volume.clamp(0.0, 1.5);
        if state.audio_volume > 0.0 && before <= 0.0 {
            state.audio_muted = false;
        }
    }
}

/// Disc boot-mode toggle. Active means warm fast boot is used on the
/// next disc launch; inactive means the full BIOS logo path.
fn draw_boot_toggle(ui: &mut egui::Ui, state: &mut AppState) {
    let enabled = state.settings.emulator.fast_boot_disc;
    let tooltip = if enabled {
        "Disc fast boot enabled - skips BIOS logo"
    } else {
        "Disc fast boot disabled - full BIOS logo boot"
    };
    let btn = toggle_button(icons::DISC, enabled);
    if ui.add(btn).on_hover_text(tooltip).clicked() {
        state.toggle_fast_boot_disc();
    }
}

/// Debug-tool toggle cluster.
fn draw_debug_toggles(ui: &mut egui::Ui, state: &mut AppState) {
    debug_toggle(
        ui,
        icons::BUG,
        "Toggle debug sidebar",
        &mut state.panels.debug_sidebar,
    );
    // Freelook camera toggle. Resets the pose on disable so re-enabling
    // starts centred on the game's own camera.
    let fl_btn = toggle_button(icons::EYE, state.freelook.enabled);
    if ui
        .add(fl_btn)
        .on_hover_text("Freelook camera - move T/F/G/H, look I/J/K/L, up/down U/O (Shift = fast)")
        .clicked()
    {
        state.freelook.enabled = !state.freelook.enabled;
        if !state.freelook.enabled {
            state.freelook = emulator_core::FreelookState::default();
        }
        state.status_message_set(if state.freelook.enabled {
            "Freelook ON - T/F/G/H move, I/J/K/L look, U/O up/down"
        } else {
            "Freelook off"
        });
    }
    // Wireframe mode lives on the GPU, not a frontend panel -- we
    // dereference through Bus to flip it.
    let wf_active = state
        .bus
        .as_ref()
        .map(|b| b.gpu.wireframe_enabled)
        .unwrap_or(false);
    let wf_btn = toggle_button(icons::GRID, wf_active);
    if ui
        .add(wf_btn)
        .on_hover_text("Wireframe mode - edges only")
        .clicked()
    {
        if let Some(bus) = state.bus.as_mut() {
            bus.gpu.wireframe_enabled = !bus.gpu.wireframe_enabled;
        }
    }
    // Resolution switch: shared HW renderer at native scale ↔
    // high-res rasterization fitted to the framebuffer panel.
    let high_res_active = state.scale_mode == ScaleMode::Window;
    let scale_icon = if high_res_active {
        icons::MINIMIZE
    } else {
        icons::MAXIMIZE
    };
    let scale_btn = toggle_button(scale_icon, high_res_active);
    if ui
        .add(scale_btn)
        .on_hover_text("Toggle native scale vs. high-res scale")
        .clicked()
    {
        state.scale_mode = match state.scale_mode {
            ScaleMode::Window => ScaleMode::Native,
            ScaleMode::Native => ScaleMode::Window,
        };
    }
    // Sample-time texture filter: cycle None -> xBR.
    let tex_active = state.texture_filter != app::TextureFilter::None;
    let tex_btn = toggle_button(icons::FILTER, tex_active);
    if ui
        .add(tex_btn)
        .on_hover_text(format!("Texture filter: {}", state.texture_filter.label()))
        .clicked()
    {
        state.texture_filter = state.texture_filter.next();
        state.status_message_set(format!("Texture filter: {}", state.texture_filter.label()));
    }
}

/// One flag-backed toggle button. Tooltip + tint reflect the flag;
/// clicking mutates it in place.
fn debug_toggle(ui: &mut egui::Ui, icon: char, tooltip: &str, flag: &mut bool) {
    let btn = toggle_button(icon, *flag);
    if ui.add(btn).on_hover_text(tooltip).clicked() {
        *flag = !*flag;
    }
}

/// Build a Button at the shared icon size, tinted to indicate active
/// vs. inactive state. Keeps the toggle cluster visually coherent.
fn toggle_button(icon: char, active: bool) -> Button<'static> {
    let color = if active { STATUS_RUNNING } else { METRIC_LABEL };
    // Active toggles use the solid (fill) weight, inactive the outline.
    let icon_font = if active {
        icons::font_fill(ICON_SIZE)
    } else {
        icons::font(ICON_SIZE)
    };
    let label = RichText::new(icon.to_string()).font(icon_font).color(color);
    // `Extend` stops egui truncating/clipping the glyph to its advance width;
    // some Lucide icons (e.g. DISC) have ink that overruns the advance and
    // would otherwise render with the right edge sliced off in the tight button.
    Button::new(label)
        .min_size(BUTTON_SIZE)
        .wrap_mode(egui::TextWrapMode::Extend)
}

/// Plain icon button (no toggle tint) at the shared size. Carries the same
/// `Extend` guard as `toggle_button` so wide Lucide glyphs aren't sliced to
/// their advance width in the tight button.
fn icon_button(icon: char) -> Button<'static> {
    Button::new(icons::text(icon, ICON_SIZE))
        .min_size(BUTTON_SIZE)
        .wrap_mode(egui::TextWrapMode::Extend)
}

/// Left-hand cluster: status pill + responsive FPS / MIPS / dt metrics.
fn draw_metrics(ui: &mut egui::Ui, state: &AppState, available_width: f32) {
    ui.add_space(2.0);

    let (dot_color, status_label) = if state.running {
        (STATUS_RUNNING, "RUNNING")
    } else {
        (STATUS_PAUSED, "PAUSED")
    };
    status_dot(ui, dot_color);
    ui.add_space(4.0);
    ui.add(toolbar_label(status_label, METRIC_TEXT));

    let host_fps = state.hud.fps();
    let ms = state.hud.average_dt() * 1000.0;
    let mips = state.hud.ips() / 1_000_000.0;
    let audio = state.hud.audio_queue_len();
    let profile_avg = state
        .profiler
        .live_average()
        .or_else(|| state.profiler.average());
    let emu_hz = profile_avg
        .map(|sample| sample.emulated_vblank_hz())
        .unwrap_or(0.0);
    let (cadence_label, cadence_hz) = profile_avg
        .and_then(|sample| sample.guest_visual_frame_hz().map(|hz| ("VIS", hz)))
        .unwrap_or_else(|| {
            (
                "DRAW",
                profile_avg
                    .map(|sample| sample.psx_draw_hz())
                    .unwrap_or(0.0),
            )
        });

    maybe_metric(ui, available_width, 170.0, "EMU", format!("{emu_hz:4.1}"));
    maybe_metric(
        ui,
        available_width,
        260.0,
        cadence_label,
        format!("{cadence_hz:4.1}"),
    );
    maybe_metric(
        ui,
        available_width,
        360.0,
        "HOST",
        format!("{host_fps:4.1}"),
    );
    maybe_metric(ui, available_width, 460.0, "MIPS", format!("{mips:4.1}"));
    maybe_metric(ui, available_width, 560.0, "dt", format!("{ms:4.1} ms"));
    maybe_metric(ui, available_width, 660.0, "AUDIO", format!("{audio}"));
}

/// One "LABEL value" pair, formatted so the label is dim and the
/// value uses the primary text colour. Looks uniform across the row.
fn metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.add(toolbar_label(label, METRIC_LABEL));
    ui.add_space(4.0);
    ui.add(toolbar_value(value));
}

/// Transport cluster, in visual left-to-right order.
fn draw_buttons(ui: &mut egui::Ui, state: &mut AppState) {
    // Play / Pause -- icon + tooltip flip with state.
    let (icon, tooltip) = if state.running {
        (icons::PAUSE, "Pause")
    } else {
        (icons::PLAY, "Play")
    };
    let play_btn = icon_button(icon);
    if ui.add(play_btn).on_hover_text(tooltip).clicked() {
        state.running = !state.running;
        state.menu.sync_run_label(state.running);
    }

    // Reset -- rebuild the CPU, clear VRAM, keep the Bus (disc stays
    // inserted).
    let reset_btn = icon_button(icons::ROTATE_CCW);
    if ui.add(reset_btn).on_hover_text("Reset").clicked() {
        // Reboot the CPU but keep the run state -- reset shouldn't force a pause.
        state.cpu = emulator_core::Cpu::new();
        state.exec_history.clear();
        state.gpr_snapshot = None;
        if let Some(bus) = state.bus.as_mut() {
            bus.gpu.vram.clear();
            state.gpu_resync_generation = state.gpu_resync_generation.wrapping_add(1);
        }
    }

    // Advance one emulated frame. Pauses first so clicking while
    // running doesn't advance two frames (one from the click, one
    // from the shell's run loop).
    let step_btn = icon_button(icons::SKIP_FORWARD);
    if ui
        .add(step_btn)
        .on_hover_text("Advance one frame")
        .clicked()
    {
        state.running = false;
        state.menu.sync_run_label(false);
        app::step_one_frame(state);
    }
}
