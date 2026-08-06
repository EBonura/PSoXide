//! UI panel orchestration.
//!
//! Individual panels live in submodules; `draw_layout` composes them in
//! the order that makes the visual layering work: docked panels first
//! (bottom/side get clipped to remaining space), then the central area,
//! then free-floating overlays (Menu, HUD) on top.

pub mod burn;
pub mod debug_sidebar;
pub mod framebuffer;
pub mod hud;
pub mod memory;
pub mod menu;
pub mod profiler;
pub mod registers;
pub mod splash;
pub mod toolbar;
pub mod vram;

use crate::app::AppState;

/// Paint every panel for this frame, in layering order.
pub fn draw_layout(
    ctx: &egui::Context,
    state: &mut AppState,
    vram_tex: egui::TextureId,
    display_tex: egui::TextureId,
    #[cfg(feature = "editor")] editor_viewport: psxed_ui::EditorViewport3dPresentation,
    display_uv: egui::Rect,
    dt: f32,
) {
    state.hud.update(dt, state.cpu.tick());
    state.tick_status(dt);
    let recording_input = state.input_recording_status().0;
    state.menu.sync_input_recording_label(recording_input);

    // One-shot boot splash on a foreground layer (drawn before the workspace
    // branch so it overlays both the emulator and editor at launch).
    splash::draw(ctx);

    // When the editor workspace owns the central UI it takes over the whole
    // frame; the emulator panels below never run. Compiled out without the
    // editor feature (the workspace can never be the editor then).
    #[cfg(feature = "editor")]
    if state.workspace.is_editor() {
        let playtest_status = state.editor_playtest_status();
        state.editor.draw(ctx, editor_viewport, playtest_status);
        state.sync_embedded_playtest_with_editor_project();
        let menu_warning = state.menu_setup_warning();
        state.menu.draw(ctx, dt, menu_warning);
        burn::draw(ctx, state);
        draw_recording_indicator(ctx, state);
        draw_freecam_indicator(ctx, state);
        draw_status_toast(ctx, state);
        return;
    }

    // Top-bar controls go first so the central panel (framebuffer)
    // clips to what's left under them. The unified debug sidebar
    // docks next so the framebuffer gives it room.
    toolbar::draw(ctx, state);

    // Always called: the sidebar animates itself open/closed from the
    // `debug_sidebar` flag and early-returns when fully closed.
    debug_sidebar::draw(ctx, state, vram_tex);

    // Zero-margin frame: the game screen gets every pixel between the
    // toolbar and the sidebar; the 4:3 letterbox bars are plain black,
    // CRT-bezel style, with no inset or border chrome.
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
        .show(ctx, |ui| {
            framebuffer::draw(
                ui,
                display_tex,
                display_uv,
                &mut state.framebuffer_present_size_px,
            );
        });

    let menu_warning = state.menu_setup_warning();
    state.menu.draw(ctx, dt, menu_warning);
    burn::draw(ctx, state);
    draw_recording_indicator(ctx, state);
    draw_freecam_indicator(ctx, state);
    draw_status_toast(ctx, state);
}

pub fn apply_menu_action(state: &mut AppState, action: menu::MenuAction) -> MenuOutcome {
    use menu::MenuAction::*;
    match action {
        ToggleRun => {
            state.running = !state.running;
            state.menu.sync_run_label(state.running);
            // Auto-close the overlay so Run is observable immediately.
            if state.running {
                state.menu.open = false;
            }
            MenuOutcome::None
        }
        StepOne => {
            if let Some(bus) = state.bus.as_mut() {
                if let Ok(record) = state.cpu.step_traced(bus) {
                    crate::app::push_history(&mut state.exec_history, record);
                }
            }
            MenuOutcome::None
        }
        Reset => {
            // Reboot the CPU but keep the run state -- reset shouldn't drop
            // you into a paused state.
            state.cpu = emulator_core::Cpu::new();
            state.exec_history.clear();
            state.gpr_snapshot = None;
            if let Some(bus) = state.bus.as_mut() {
                bus.gpu.vram.clear();
                state.gpu_resync_generation = state.gpu_resync_generation.wrapping_add(1);
            }
            MenuOutcome::None
        }
        ToggleInputRecording => {
            state.toggle_input_recording();
            MenuOutcome::None
        }
        #[cfg(target_arch = "wasm32")]
        LoadInputReplay => {
            state.pick_web_replay();
            MenuOutcome::None
        }
        ToggleFastBoot => {
            state.toggle_fast_boot_disc();
            MenuOutcome::None
        }
        SaveState => {
            state.save_state();
            MenuOutcome::None
        }
        LoadState(slot, start_paused) => {
            state.load_state(slot, start_paused);
            MenuOutcome::None
        }
        OpenSaveStates => {
            state.menu.open_save_states();
            MenuOutcome::None
        }
        OpenControls => {
            state.menu.open_controls();
            MenuOutcome::None
        }
        ResetControls => {
            state.reset_controls();
            // The shell caches pad state keyed by the just-replaced
            // bindings; it must throw that cache away or bits set
            // under the old mapping can never be released.
            MenuOutcome::ClearHostKeyboardInput
        }
        PinAsTop(slot) => {
            state.pin_save_state_as_top(slot);
            MenuOutcome::None
        }
        LaunchGame(id) => {
            // Game-launch rebuilds Bus + Cpu from scratch. Close
            // the Menu on success so the user sees the freshly-
            // booted BIOS / EXE, exactly like a real PSX shell.
            match state.launch_by_id(&id) {
                Ok(()) => {
                    state.menu.open = false;
                }
                Err(e) => {
                    eprintln!("[frontend] launch failed: {e}");
                    if e.contains("BIOS path is not configured") {
                        state.select_settings_category();
                    }
                    state.status_message_set(format!("Launch failed: {e}"));
                }
            }
            MenuOutcome::None
        }
        OpenBurnMenu(id) => {
            if let Err(e) = state.open_burn_menu_by_id(&id) {
                eprintln!("[frontend] burn menu failed: {e}");
                state.status_message_set(format!("Burn menu failed: {e}"));
            }
            MenuOutcome::None
        }
        RescanLibrary => {
            match state.rescan_library() {
                Ok(n) => {
                    state.status_message_set(format!(
                        "Scan complete: {} entries ({n} new/changed)",
                        state.library.entries.len()
                    ));
                }
                Err(e) => {
                    eprintln!("[frontend] rescan failed: {e}");
                    state.status_message_set(format!("Rescan failed: {e}"));
                }
            }
            MenuOutcome::None
        }
        BuildExamples => {
            state.start_examples_build();
            MenuOutcome::None
        }
        #[cfg(feature = "editor")]
        ToggleEditorWorkspace => {
            state.toggle_editor_workspace();
            state.menu.open = false;
            MenuOutcome::None
        }
        ChooseBiosPath => {
            state.choose_bios_path();
            MenuOutcome::None
        }
        ChooseGamesPath => {
            state.choose_games_path();
            MenuOutcome::None
        }
        CycleMenuOpacity => {
            state.cycle_menu_opacity();
            MenuOutcome::None
        }
        #[cfg(target_arch = "wasm32")]
        Reconnect => {
            state.reconnect_web_files();
            MenuOutcome::None
        }
        ShowAbout => {
            state.menu.show_about();
            MenuOutcome::None
        }
        Noop => MenuOutcome::None,
        Quit => MenuOutcome::Quit,
    }
}

/// What the shell needs to do after an Menu action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    None,
    /// The action invalidated the bindings the shell's cached keyboard
    /// pad state was built under (Reset Controls). The shell must drop
    /// that cache and rebuild the current frame's merged input from
    /// the gamepad alone.
    ClearHostKeyboardInput,
    Quit,
}

fn draw_status_toast(ctx: &egui::Context, state: &AppState) {
    let Some((msg, ttl)) = state.status_message.as_ref() else {
        return;
    };
    let alpha = (*ttl / 0.35).clamp(0.0, 1.0);
    let bg = egui::Color32::from_rgba_premultiplied(16, 18, 22, (230.0 * alpha) as u8);
    let stroke = egui::Stroke::new(
        1.0,
        egui::Color32::from_rgba_premultiplied(0, 191, 230, (180.0 * alpha) as u8),
    );
    let text = egui::Color32::from_rgba_premultiplied(235, 238, 242, (255.0 * alpha) as u8);

    // Sit below whichever persistent indicators are up, so a toast never
    // lands on top of the freecam or recording badge.
    let mut toast_y = 48.0;
    if state.input_recording_status().0 {
        toast_y += 46.0;
    }
    if state.freelook.enabled {
        toast_y += 46.0;
    }
    egui::Area::new("status-toast".into())
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, toast_y))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(bg)
                .stroke(stroke)
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(msg).color(text).size(13.0)).wrap(),
                    );
                });
        });
}

/// Persistent freecam badge.
///
/// Freecam mutes the guest pad entirely, so without a standing indicator a
/// forgotten freecam looks exactly like a hung game: buttons do nothing and
/// nothing on screen says why. The toolbar EYE button already shows the state,
/// but it is not always visible and it does not explain the muted controller.
fn draw_freecam_indicator(ctx: &egui::Context, state: &AppState) {
    if !state.freelook.enabled {
        return;
    }
    let y = if state.input_recording_status().0 {
        94.0
    } else {
        48.0
    };
    egui::Area::new("freecam-indicator".into())
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, y))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_premultiplied(12, 22, 30, 235))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0, 191, 230)))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "FREECAM  game input paused · tap L3+R3 to exit · hold to reset",
                        )
                        .color(egui::Color32::WHITE)
                        .strong(),
                    );
                });
        });
}

fn draw_recording_indicator(ctx: &egui::Context, state: &AppState) {
    let (recording, frames) = state.input_recording_status();
    if !recording {
        return;
    }
    egui::Area::new("input-recording-indicator".into())
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 48.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_premultiplied(30, 12, 14, 235))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(235, 62, 74)))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(egui::Color32::from_rgb(245, 54, 68), "●");
                        ui.label(
                            egui::RichText::new(format!("REC  {frames} frames · F8 to stop"))
                                .color(egui::Color32::WHITE)
                                .strong(),
                        );
                    });
                });
        });
}
