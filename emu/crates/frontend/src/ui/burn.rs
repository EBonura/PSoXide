//! CD burn settings window.

use egui::{Align, ComboBox, RichText};

use crate::app::AppState;

const SPEEDS: &[&str] = &["Default", "4x", "8x", "16x", "24x"];

/// Draw the burn submenu opened from an example/project row.
pub fn draw(ctx: &egui::Context, state: &mut AppState) {
    if !state.burn.open {
        return;
    }

    let mut open = state.burn.open;
    let mut refresh = false;
    let mut close = false;
    let mut burn_requested = false;

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
                    if ui.button("Scan").clicked() {
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
            }

            ComboBox::from_label("Speed")
                .selected_text(state.burn.speed.as_str())
                .show_ui(ui, |ui| {
                    for speed in SPEEDS {
                        ui.selectable_value(&mut state.burn.speed, (*speed).to_string(), *speed);
                    }
                });

            ui.checkbox(&mut state.burn.simulate, "Simulate first");
            ui.checkbox(&mut state.burn.eject, "Eject after burn");

            ui.separator();
            ui.label(&state.burn.status);

            ui.horizontal(|ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
                let selected_can_burn = state
                    .burn
                    .burners
                    .get(state.burn.selected_burner)
                    .map_or(false, |burner| burner.can_burn());
                let can_burn =
                    state.burn.target.is_some() && selected_can_burn && !state.burn.is_burning();
                if ui
                    .add_enabled(can_burn, egui::Button::new("Burn"))
                    .clicked()
                {
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
