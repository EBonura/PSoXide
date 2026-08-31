use super::*;

#[derive(Clone, Copy)]
enum BrushOverlapAction {
    Focus { brush: usize, face: usize },
    Delete { brush: usize },
}

impl EditorWorkspace {
    pub(crate) fn run_brush_overlap_audit(&mut self) {
        let overlaps = psxed_project::brush_overlap::find_brush_face_overlaps(
            &self.project.active_scene().brushes,
        );
        let pair_count = overlaps
            .iter()
            .map(|overlap| (overlap.brush_a, overlap.brush_b))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        self.status = if overlaps.is_empty() {
            "Brush overlap check: no same-facing coplanar overlaps".to_string()
        } else {
            format!(
                "Brush overlap check: {} face overlap{} across {} brush pair{}",
                overlaps.len(),
                if overlaps.len() == 1 { "" } else { "s" },
                pair_count,
                if pair_count == 1 { "" } else { "s" }
            )
        };
        self.brush_overlap_report = Some(overlaps);
    }

    pub(crate) fn focus_brush_overlap_face(&mut self, brush: usize, face: usize) -> bool {
        let Some(authored) = self.project.active_scene().brushes.get(brush) else {
            return false;
        };
        if face >= authored.faces.len() {
            return false;
        }
        self.active_workspace = WorkspaceView::Room;
        self.active_tool = ViewTool::Brush;
        self.brush_edit_mode = BrushEditMode::Face;
        self.replace_brush_selection(brush, Some(face));
        self.clear_node_selection_state();
        self.clear_resource_selection_state();
        self.clear_sector_selection();
        self.clear_primitive_selection_state();
        self.frame_viewport();
        self.status = format!("Overlap: selected brush {}, face {}", brush + 1, face + 1);
        true
    }

    pub(crate) fn delete_brush_from_overlap_report(&mut self, brush: usize) {
        if brush >= self.project.active_scene().brushes.len() {
            self.run_brush_overlap_audit();
            return;
        }
        self.replace_brush_selection(brush, None);
        self.delete_selected_brushes();
        self.run_brush_overlap_audit();
        self.status = format!(
            "Deleted brush {}; overlap report refreshed (Undo restores it)",
            brush + 1
        );
    }

    pub(crate) fn draw_brush_overlap_dialog(&mut self, ctx: &egui::Context) {
        let Some(overlaps) = self.brush_overlap_report.clone() else {
            return;
        };
        let pair_count = overlaps
            .iter()
            .map(|overlap| (overlap.brush_a, overlap.brush_b))
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let mut close = false;
        let mut action = None;

        egui::Window::new("Overlapping Brush Faces")
            .collapsible(false)
            .resizable(true)
            .default_width(760.0)
            .default_height(420.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Same-facing coplanar faces with shared surface area. Ordinary opposite-facing brush seams are ignored.",
                    )
                    .color(STUDIO_TEXT_WEAK),
                );
                ui.label(
                    RichText::new("Focus either face to inspect it. Delete removes only that brush and is undoable.")
                        .color(STUDIO_TEXT_WEAK)
                        .small(),
                );
                ui.add_space(6.0);

                if overlaps.is_empty() {
                    ui.label(RichText::new("No overlapping brush faces found.").strong());
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{} overlapping face{} across {} brush pair{}",
                            overlaps.len(),
                            if overlaps.len() == 1 { "" } else { "s" },
                            pair_count,
                            if pair_count == 1 { "" } else { "s" }
                        ))
                        .strong(),
                    );
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("brush-overlap-results")
                                .striped(true)
                                .spacing([10.0, 6.0])
                                .show(ui, |ui| {
                                    ui.strong("Overlap");
                                    ui.strong("Area");
                                    ui.strong("Inspect");
                                    ui.strong("Remove");
                                    ui.end_row();
                                    for overlap in &overlaps {
                                        if ui
                                            .selectable_label(
                                                false,
                                                format!(
                                                    "Brush {} face {}  ↔  Brush {} face {}",
                                                    overlap.brush_a + 1,
                                                    overlap.face_a + 1,
                                                    overlap.brush_b + 1,
                                                    overlap.face_b + 1
                                                ),
                                            )
                                            .on_hover_text("Focus the second face")
                                            .clicked()
                                        {
                                            action = Some(BrushOverlapAction::Focus {
                                                brush: overlap.brush_b,
                                                face: overlap.face_b,
                                            });
                                        }
                                        ui.monospace(format!("{:.1} u²", overlap.area));
                                        ui.horizontal(|ui| {
                                            if ui.small_button("A").on_hover_text("Focus brush A face").clicked() {
                                                action = Some(BrushOverlapAction::Focus {
                                                    brush: overlap.brush_a,
                                                    face: overlap.face_a,
                                                });
                                            }
                                            if ui.small_button("B").on_hover_text("Focus brush B face").clicked() {
                                                action = Some(BrushOverlapAction::Focus {
                                                    brush: overlap.brush_b,
                                                    face: overlap.face_b,
                                                });
                                            }
                                        });
                                        ui.horizontal(|ui| {
                                            if ui.small_button("Delete A").clicked() {
                                                action = Some(BrushOverlapAction::Delete {
                                                    brush: overlap.brush_a,
                                                });
                                            }
                                            if ui.small_button("Delete B").clicked() {
                                                action = Some(BrushOverlapAction::Delete {
                                                    brush: overlap.brush_b,
                                                });
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                        });
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Refresh").clicked() {
                        self.run_brush_overlap_audit();
                    }
                    if ui.button("Close").clicked()
                        || ui.input(|input| input.key_pressed(egui::Key::Escape))
                    {
                        close = true;
                    }
                });
            });

        match action {
            Some(BrushOverlapAction::Focus { brush, face }) => {
                let _ = self.focus_brush_overlap_face(brush, face);
            }
            Some(BrushOverlapAction::Delete { brush }) => {
                self.delete_brush_from_overlap_report(brush);
            }
            None => {}
        }
        if close {
            self.brush_overlap_report = None;
        }
    }
}
