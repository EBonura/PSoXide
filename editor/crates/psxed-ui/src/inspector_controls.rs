use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GridUvTransformEdit {
    pub(crate) offset: [bool; 2],
    pub(crate) span: [bool; 2],
    pub(crate) rotation: bool,
    pub(crate) flip_u: bool,
    pub(crate) flip_v: bool,
}

impl GridUvTransformEdit {
    pub(crate) const fn changed(self) -> bool {
        self.offset[0]
            || self.offset[1]
            || self.span[0]
            || self.span[1]
            || self.rotation
            || self.flip_u
            || self.flip_v
    }

    pub(crate) fn include_value_changes(
        &mut self,
        before: GridUvTransform,
        after: GridUvTransform,
    ) {
        for axis in 0..2 {
            self.offset[axis] |= before.offset[axis] != after.offset[axis];
            self.span[axis] |= before.span[axis] != after.span[axis];
        }
        self.rotation |= before.rotation != after.rotation;
        self.flip_u |= before.flip_u != after.flip_u;
        self.flip_v |= before.flip_v != after.flip_v;
    }

    pub(crate) fn apply(self, uv: &mut GridUvTransform, authored: GridUvTransform) {
        for axis in 0..2 {
            if self.offset[axis] {
                uv.offset[axis] = authored.offset[axis];
            }
            if self.span[axis] {
                uv.span[axis] = authored.span[axis];
            }
        }
        if self.rotation {
            uv.rotation = authored.rotation;
        }
        if self.flip_u {
            uv.flip_u = authored.flip_u;
        }
        if self.flip_v {
            uv.flip_v = authored.flip_v;
        }
    }

    const fn all() -> Self {
        Self {
            offset: [true; 2],
            span: [true; 2],
            rotation: true,
            flip_u: true,
            flip_v: true,
        }
    }
}

pub(crate) fn uv_transform_controls(
    uv: &mut GridUvTransform,
    ui: &mut egui::Ui,
) -> GridUvTransformEdit {
    let mut edit = GridUvTransformEdit::default();
    ui.horizontal(|ui| {
        ui.label("Offset");
        ui.label("U");
        edit.offset[0] |= ui
            .add(egui::DragValue::new(&mut uv.offset[0]).speed(1.0))
            .changed();
        ui.label("V");
        edit.offset[1] |= ui
            .add(egui::DragValue::new(&mut uv.offset[1]).speed(1.0))
            .changed();
    });
    ui.horizontal(|ui| {
        ui.label("Span");
        ui.label("U");
        edit.span[0] |= ui
            .add(
                egui::DragValue::new(&mut uv.span[0])
                    .speed(1.0)
                    .range(0..=255),
            )
            .on_hover_text("0 uses the material's native U span.")
            .changed();
        ui.label("V");
        edit.span[1] |= ui
            .add(
                egui::DragValue::new(&mut uv.span[1])
                    .speed(1.0)
                    .range(0..=255),
            )
            .on_hover_text("0 uses the material's native V span.")
            .changed();
    });
    ui.horizontal(|ui| {
        ui.label("Rotate");
        for (rotation, label) in [
            (GridUvRotation::Deg0, "0"),
            (GridUvRotation::Deg45, "45"),
            (GridUvRotation::Deg90, "90"),
            (GridUvRotation::Deg135, "135"),
            (GridUvRotation::Deg180, "180"),
            (GridUvRotation::Deg225, "225"),
            (GridUvRotation::Deg270, "270"),
            (GridUvRotation::Deg315, "315"),
        ] {
            edit.rotation |= ui
                .selectable_value(&mut uv.rotation, rotation, label)
                .clicked();
        }
    });
    ui.horizontal(|ui| {
        edit.flip_u |= ui.checkbox(&mut uv.flip_u, "Flip U").changed();
        edit.flip_v |= ui.checkbox(&mut uv.flip_v, "Flip V").changed();
        if ui
            .small_button("Reset")
            .on_hover_text("Reset selected faces' UV offset, span, rotation, and flips.")
            .clicked()
        {
            *uv = GridUvTransform::IDENTITY;
            edit = GridUvTransformEdit::all();
        }
    });
    edit
}

pub(crate) fn height_row(label: &str, heights: &mut [i32; 4], ui: &mut egui::Ui) -> bool {
    let mut changed = false;
    let mut sloped =
        !(heights[0] == heights[1] && heights[1] == heights[2] && heights[2] == heights[3]);
    ui.horizontal(|ui| {
        ui.label(label);
        if ui
            .toggle_value(&mut sloped, "Slope")
            .on_hover_text("Edit each corner height independently.")
            .changed()
            && !sloped
        {
            heights[1] = heights[0];
            heights[2] = heights[0];
            heights[3] = heights[0];
            changed = true;
        }
    });
    if sloped {
        egui::Grid::new(format!("{label}-corners"))
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for &idx in &[0usize, 1, 3, 2] {
                    if ui
                        .add(egui::DragValue::new(&mut heights[idx]).speed(HEIGHT_QUANTUM as f32))
                        .changed()
                    {
                        heights[idx] = snap_height(heights[idx]);
                        changed = true;
                    }
                    if idx == 1 {
                        ui.end_row();
                    }
                }
                ui.end_row();
            });
    } else {
        ui.horizontal(|ui| {
            ui.label("    ");
            let mut h = heights[0];
            if ui
                .add(egui::DragValue::new(&mut h).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                *heights = [snap_height(h); 4];
                changed = true;
            }
        });
    }
    changed
}

pub(crate) fn split_row(label: &str, split: &mut GridSplit, ui: &mut egui::Ui) -> bool {
    let before = *split;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.selectable_value(split, GridSplit::NorthWestSouthEast, "NW-SE");
        ui.selectable_value(split, GridSplit::NorthEastSouthWest, "NE-SW");
    });
    *split != before
}

pub(crate) fn material_picker(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut Option<ResourceId>,
    options: &[(ResourceId, String)],
    jump_to: &mut Option<ResourceId>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = current
            .and_then(|id| {
                options
                    .iter()
                    .find(|(rid, _)| *rid == id)
                    .map(|(_, name)| name.as_str())
            })
            .unwrap_or("(none)");
        changed |= searchable_picker(
            ui,
            ui.id().with(("material-picker", label)),
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)")
                .with_popup_min_width(360.0)
                .with_search_hint("Search materials…"),
        );
        if let Some(id) = *current {
            if ui
                .small_button("→")
                .on_hover_text("Open this material in the inspector")
                .clicked()
            {
                *jump_to = Some(id);
            }
        }
    });
    changed
}

pub(crate) fn material_option_label(
    material: Option<ResourceId>,
    options: &[(ResourceId, String)],
) -> String {
    material
        .and_then(|id| {
            options
                .iter()
                .find(|(rid, _)| *rid == id)
                .map(|(_, name)| name.clone())
        })
        .unwrap_or_else(|| "(none)".to_string())
}
