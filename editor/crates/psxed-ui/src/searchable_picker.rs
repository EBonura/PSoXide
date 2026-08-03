//! Shared searchable dropdowns for resource-heavy editor choices.
//!
//! Keep the filtering and popup behavior here so Material, Model, Character,
//! audio, scene, and future asset pickers do not each grow a different search
//! field and matching rule.

use std::hash::Hash;

use egui::{RichText, TextWrapMode};

use crate::style::STUDIO_TEXT_WEAK;

const DEFAULT_POPUP_HEIGHT: f32 = 360.0;
const DEFAULT_POPUP_MIN_WIDTH: f32 = 300.0;

#[derive(Clone, Copy)]
pub(crate) struct SearchablePickerConfig<'a> {
    pub(crate) none_label: Option<&'a str>,
    pub(crate) search_hint: &'a str,
    pub(crate) width: Option<f32>,
    pub(crate) popup_min_width: f32,
    pub(crate) popup_height: f32,
}

impl<'a> SearchablePickerConfig<'a> {
    pub(crate) const fn optional(none_label: &'a str) -> Self {
        Self {
            none_label: Some(none_label),
            search_hint: "Search options…",
            width: None,
            popup_min_width: DEFAULT_POPUP_MIN_WIDTH,
            popup_height: DEFAULT_POPUP_HEIGHT,
        }
    }

    pub(crate) const fn required() -> Self {
        Self {
            none_label: None,
            search_hint: "Search options…",
            width: None,
            popup_min_width: DEFAULT_POPUP_MIN_WIDTH,
            popup_height: DEFAULT_POPUP_HEIGHT,
        }
    }

    pub(crate) const fn with_width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub(crate) const fn with_popup_min_width(mut self, width: f32) -> Self {
        self.popup_min_width = width;
        self
    }

    pub(crate) const fn with_search_hint(mut self, hint: &'a str) -> Self {
        self.search_hint = hint;
        self
    }
}

/// Draw a compact ComboBox with an immediately focused search field.
///
/// `options` intentionally uses the common `(value, display name)` shape used
/// throughout the editor. Search is case-insensitive and every whitespace-
/// separated term must occur, so `brick dark` finds `Dark Brick Material`.
pub(crate) fn searchable_picker<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id_salt: impl Hash,
    selected: &mut Option<T>,
    selected_text: &str,
    options: &[(T, String)],
    config: SearchablePickerConfig<'_>,
) -> bool {
    let picker_id = ui.make_persistent_id(id_salt);
    let filter_id = picker_id.with("search-filter");
    let mut changed = false;
    let mut combo = egui::ComboBox::from_id_salt(picker_id)
        .selected_text(selected_text)
        .height(config.popup_height)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .wrap_mode(TextWrapMode::Truncate);
    if let Some(width) = config.width {
        combo = combo.width(width);
    }

    combo.show_ui(ui, |ui| {
        ui.set_min_width(config.popup_min_width);
        let mut filter = ui
            .memory_mut(|memory| memory.data.get_persisted::<String>(filter_id))
            .unwrap_or_default();
        let search = ui.add(
            egui::TextEdit::singleline(&mut filter)
                .hint_text(config.search_hint)
                .desired_width(f32::INFINITY),
        );
        search.request_focus();
        if search.changed() {
            ui.memory_mut(|memory| memory.data.insert_persisted(filter_id, filter.clone()));
        }
        let accept_first_match = !filter.trim().is_empty()
            && search.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter));

        let matching = options
            .iter()
            .filter(|(_, label)| option_matches_filter(label, &filter))
            .count();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{matching} of {} options", options.len()))
                    .small()
                    .color(STUDIO_TEXT_WEAK),
            );
            if !filter.is_empty() && ui.small_button("Clear").clicked() {
                filter.clear();
                ui.memory_mut(|memory| memory.data.insert_persisted(filter_id, String::new()));
            }
        });
        ui.separator();

        if let Some(none_label) = config.none_label {
            if ui
                .selectable_label(selected.is_none(), none_label)
                .clicked()
                && selected.is_some()
            {
                *selected = None;
                changed = true;
                clear_picker_filter(ui, filter_id);
                ui.close_menu();
            }
        }

        let mut any_match = false;
        for (match_index, (value, label)) in options
            .iter()
            .filter(|(_, label)| option_matches_filter(label, &filter))
            .enumerate()
        {
            any_match = true;
            let clicked = ui
                .selectable_label(*selected == Some(*value), label)
                .on_hover_text(label)
                .clicked();
            if clicked || (accept_first_match && match_index == 0) {
                if *selected != Some(*value) {
                    *selected = Some(*value);
                    changed = true;
                }
                clear_picker_filter(ui, filter_id);
                ui.close_menu();
            }
        }
        if !any_match {
            ui.label(RichText::new("No matching options").color(STUDIO_TEXT_WEAK));
        }
    });
    changed
}

fn clear_picker_filter(ui: &mut egui::Ui, filter_id: egui::Id) {
    ui.memory_mut(|memory| {
        memory.data.insert_persisted(filter_id, String::new());
    });
}

pub(crate) fn option_matches_filter(label: &str, filter: &str) -> bool {
    let normalized_filter = filter.trim().to_ascii_lowercase();
    if normalized_filter.is_empty() {
        return true;
    }
    let normalized_label = label.to_ascii_lowercase();
    normalized_filter
        .split_whitespace()
        .all(|term| normalized_label.contains(term))
}

#[cfg(test)]
mod tests {
    use super::{option_matches_filter, searchable_picker, SearchablePickerConfig};

    #[test]
    fn picker_filter_is_case_insensitive_and_matches_all_terms() {
        assert!(option_matches_filter("BLOCK_1A Material", "block"));
        assert!(option_matches_filter("Dark Brick Material", "brick dark"));
        assert!(option_matches_filter(
            "Dark Brick Material",
            "  BRICK   mat "
        ));
        assert!(!option_matches_filter("Dark Brick Material", "brick light"));
    }

    #[test]
    fn picker_filter_empty_query_matches_everything() {
        assert!(option_matches_filter("Anything", ""));
        assert!(option_matches_filter("Anything", "   "));
    }

    #[test]
    fn picker_accepts_the_first_filtered_option_with_enter() {
        let ctx = egui::Context::default();
        let options = vec![
            (1_u8, "LIGHT_1D Material".to_string()),
            (2_u8, "BLOCK_1A Material".to_string()),
            (3_u8, "DIRT_1A Material".to_string()),
        ];
        let mut selected = Some(1_u8);

        let mut draw = |events: Vec<egui::Event>| {
            let mut button_id = egui::Id::NULL;
            let mut filter_id = egui::Id::NULL;
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 480.0),
                )),
                events,
                focused: true,
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .expect("root viewport")
                .focused = Some(true);
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let picker_id = ui.make_persistent_id("keyboard-picker-test");
                    button_id = ui.make_persistent_id(egui::Id::new(picker_id));
                    filter_id = picker_id.with("search-filter");
                    searchable_picker(
                        ui,
                        "keyboard-picker-test",
                        &mut selected,
                        "LIGHT_1D Material",
                        &options,
                        SearchablePickerConfig::required(),
                    );
                });
            });
            (button_id, filter_id)
        };

        let (button_id, filter_id) = draw(Vec::new());
        ctx.memory_mut(|memory| {
            memory.open_popup(button_id.with("popup"));
            memory.data.insert_persisted(filter_id, "block".to_string());
        });
        draw(Vec::new()); // Popup appears and its search field receives focus.
        draw(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: Some(egui::Key::Enter),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        // Ends the closure's mutable borrow of `selected` so the assertion
        // below can read it. Not a Drop impl, which is what clippy sees.
        #[allow(clippy::drop_non_drop)]
        drop(draw);

        assert_eq!(selected, Some(2));
    }
}
