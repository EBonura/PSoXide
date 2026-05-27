//! CD burn settings window.

use std::time::Duration;

use egui::{Align, ComboBox, RichText};

use crate::app::AppState;

/// Draw the burn submenu opened from an example/project row.
pub fn draw(ctx: &egui::Context, state: &mut AppState) {
    if !state.burn.open {
        return;
    }

    let mut open = state.burn.open;
    let mut refresh = false;
    let mut close = false;
    let mut burn_requested = false;
    let is_burning = state.burn.is_burning();

    egui::Window::new("Burn Disc")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| {
            if let Some(target) = state.burn.target.as_ref() {
                ui.label(RichText::new(&target.title).strong());
                ui.label(target.path.display().to_string());
            }

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Burner");
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!is_burning, egui::Button::new("Scan"))
                        .clicked()
                    {
                        refresh = true;
                    }
                });
            });

            if state.burn.burners.is_empty() {
                ui.label("No CD burner detected");
            } else {
                let selected = state
                    .burn
                    .burners
                    .get(state.burn.selected_burner)
                    .map(|burner| burner.label())
                    .unwrap_or_else(|| "Select burner".to_string());
                let previous_burner = state.burn.selected_burner;
                ui.add_enabled_ui(!is_burning, |ui| {
                    ComboBox::from_id_salt("burner-select")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (index, burner) in state.burn.burners.iter().enumerate() {
                                ui.selectable_value(
                                    &mut state.burn.selected_burner,
                                    index,
                                    burner.label(),
                                );
                            }
                        });
                });
                if state.burn.selected_burner != previous_burner {
                    state.burn.align_speed_to_selected_burner();
                }
            }

            ui.add_enabled_ui(!is_burning, |ui| {
                let speed_choices = state.burn.speed_choices();
                ComboBox::from_label("Speed")
                    .selected_text(state.burn.speed.as_str())
                    .show_ui(ui, |ui| {
                        for speed in speed_choices {
                            ui.selectable_value(&mut state.burn.speed, speed.clone(), speed);
                        }
                    });

                ui.checkbox(&mut state.burn.simulate, "Simulate first");
                if state.burn.simulate {
                    state.burn.confirm_real_burn = false;
                } else {
                    ui.checkbox(&mut state.burn.confirm_real_burn, "Confirm real burn");
                }
                ui.checkbox(&mut state.burn.eject, "Eject after burn");
            });

            ui.separator();
            ui.label(&state.burn.status);
            if let Some(label) = state.burn.running_label() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(label);
                });
                ctx.request_repaint_after(Duration::from_millis(100));
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!is_burning, egui::Button::new("Close"))
                    .clicked()
                {
                    close = true;
                }
                let selected_can_burn = state
                    .burn
                    .burners
                    .get(state.burn.selected_burner)
                    .map_or(false, |burner| burner.can_burn());
                let confirmed = state.burn.simulate || state.burn.confirm_real_burn;
                let can_burn = state.burn.target.is_some()
                    && selected_can_burn
                    && confirmed
                    && !state.burn.is_burning();
                let label = if state.burn.simulate {
                    "Run Simulation"
                } else {
                    "Burn CD-R"
                };
                if ui.add_enabled(can_burn, egui::Button::new(label)).clicked() {
                    burn_requested = true;
                }
            });
        });

    state.burn.open = open && !close;

    if refresh {
        match state.burn.scan_now() {
            Ok(Some(notice)) => state.status_message_set(notice),
            Ok(None) => state.status_message_set(state.burn.status.clone()),
            Err(error) => state.status_message_set(format!("Burner scan failed: {error}")),
        }
    }

    if burn_requested {
        match state.burn.start_burn() {
            Ok(message) => state.status_message_set(message),
            Err(error) => state.status_message_set(format!("Burn failed to start: {error}")),
        }
    }
}
