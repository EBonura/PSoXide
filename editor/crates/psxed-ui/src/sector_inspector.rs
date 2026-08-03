use super::*;

#[cfg(test)]
#[derive(Default)]
pub(crate) struct RuntimeVramBudget {
    pub(crate) textures: usize,
    pub(crate) bytes: u64,
    pub(crate) room_textures: usize,
    pub(crate) room_bytes: u64,
    pub(crate) model_textures: usize,
    pub(crate) model_bytes: u64,
    pub(crate) missing: usize,
}

#[cfg(test)]
pub(crate) fn runtime_vram_budget(
    project: &ProjectDocument,
    project_root: &Path,
    resource_use: &SceneResourceUse,
) -> RuntimeVramBudget {
    let mut budget = RuntimeVramBudget::default();

    // Texture carriers are materials now; multiple materials can share
    // one image file, which occupies VRAM once.
    let mut counted_paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in &resource_use.textures {
        let Some(resource) = project.resource(*id) else {
            continue;
        };
        if let ResourceData::Material(material) = &resource.data {
            if let Some(psxt_path) = material.psxt_path.as_deref() {
                if counted_paths.insert(psxt_path) {
                    add_runtime_texture_vram(project_root, psxt_path, true, &mut budget);
                }
            }
        }
    }

    for id in &resource_use.models {
        let Some(resource) = project.resource(*id) else {
            continue;
        };
        let ResourceData::Model(model) = &resource.data else {
            continue;
        };
        if let Some(texture_path) = &model.texture_path {
            add_runtime_texture_vram(project_root, texture_path, false, &mut budget);
        }
    }

    budget
}

#[cfg(test)]
pub(crate) fn add_runtime_texture_vram(
    project_root: &Path,
    stored: &str,
    room_material: bool,
    budget: &mut RuntimeVramBudget,
) {
    if stored.trim().is_empty() {
        return;
    }
    let abs = psxed_project::model_import::resolve_path(stored, Some(project_root));
    let Ok(bytes) = std::fs::read(&abs) else {
        budget.missing += 1;
        return;
    };
    let Ok(texture) = psx_asset::Texture::from_bytes(&bytes) else {
        budget.missing += 1;
        return;
    };

    budget.textures += 1;
    let bytes = texture.pixel_bytes().len() as u64 + texture.clut_bytes().len() as u64;
    budget.bytes = budget.bytes.saturating_add(bytes);
    if room_material {
        budget.room_textures += 1;
        budget.room_bytes = budget.room_bytes.saturating_add(bytes);
    } else {
        budget.model_textures += 1;
        budget.model_bytes = budget.model_bytes.saturating_add(bytes);
    }
}

/// Per-cell inspector for one sector inside the active Room.
///
/// Renders a CollapsingHeader with floor/ceiling toggles, a single
/// flat height per face (corner authoring lands later), a material
/// dropdown for each face, and a row of toggles for the four
/// cardinal walls. Returns `true` if any field changed so the
/// workspace can mark the project dirty in one place.
pub(crate) fn draw_sector_inspector(
    ui: &mut egui::Ui,
    project: &mut ProjectDocument,
    room_id: NodeId,
    sx: u16,
    sz: u16,
    active_floor: usize,
    material_options: &[(ResourceId, String)],
    nav_target: &mut Option<ResourceId>,
) -> bool {
    let scene = project.active_scene_mut();
    let Some(room) = scene.node_mut(room_id) else {
        return false;
    };
    let NodeKind::Section { grid } = &mut room.kind else {
        return false;
    };
    let idx = active_floor.min(grid.floor_count().saturating_sub(1));
    let Some(grid) = grid.floor_mut(idx) else {
        return false;
    };
    if sx >= grid.width || sz >= grid.depth {
        return false;
    }
    let sector_size = grid.sector_size;
    let mut changed = false;

    egui::CollapsingHeader::new(icons::label(icons::GRID, "Sector"))
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Cell");
                ui.monospace(format!("{sx}, {sz}"));
            });

            let sector = grid.ensure_sector(sx, sz);
            let Some(sector) = sector else {
                ui.weak("Cell out of grid bounds");
                return;
            };

            // Floor row: enabled toggle + height + material picker.
            ui.horizontal(|ui| {
                let mut has_floor = sector.floor.is_some();
                if ui.checkbox(&mut has_floor, "Floor").changed() {
                    sector.floor = if has_floor {
                        Some(GridHorizontalFace::flat(0, None))
                    } else {
                        None
                    };
                    changed = true;
                }
            });
            if let Some(floor) = sector.floor.as_mut() {
                changed |= height_row("    Height", &mut floor.heights, ui);
                changed |= split_row("    Split", &mut floor.split, ui);
                changed |= material_picker(
                    ui,
                    "    Material",
                    &mut floor.material,
                    material_options,
                    nav_target,
                );
            }

            ui.separator();

            // Ceiling row.
            ui.horizontal(|ui| {
                let mut has_ceiling = sector.ceiling.is_some();
                if ui.checkbox(&mut has_ceiling, "Ceiling").changed() {
                    sector.ceiling = if has_ceiling {
                        Some(GridHorizontalFace::flat(sector_size, None))
                    } else {
                        None
                    };
                    changed = true;
                }
            });
            if let Some(ceiling) = sector.ceiling.as_mut() {
                changed |= height_row("    Height", &mut ceiling.heights, ui);
                changed |= split_row("    Split", &mut ceiling.split, ui);
                changed |= material_picker(
                    ui,
                    "    Material",
                    &mut ceiling.material,
                    material_options,
                    nav_target,
                );
            }

            ui.separator();
            ui.label(icons::label(icons::BRICK_WALL, "Walls"));
            for (label, dir) in [
                ("North", GridDirection::North),
                ("East", GridDirection::East),
                ("South", GridDirection::South),
                ("West", GridDirection::West),
                ("NW-SE", GridDirection::NorthWestSouthEast),
                ("NE-SW", GridDirection::NorthEastSouthWest),
            ] {
                changed |= wall_stack_row(
                    label,
                    sector.walls.get_mut(dir),
                    sector_size,
                    material_options,
                    nav_target,
                    ui,
                );
            }
        });

    changed
}

/// Stack-of-walls editor for a single sector edge (N/E/S/W).
///
/// PSX rooms commonly stack walls to model windows / arches: one
/// wall from `0..window_bottom`, another from `window_top..ceiling`.
/// The data model already allows N walls per edge -- this UI surfaces
/// it. Each wall row carries its own `[bottom, top]` and material;
/// `+` adds a new wall on top of the previous one (or `0..ceil` for
/// the first), `×` removes that row.
pub(crate) fn wall_stack_row(
    edge_label: &str,
    walls: &mut Vec<GridVerticalFace>,
    sector_size: i32,
    material_options: &[(ResourceId, String)],
    nav_target: &mut Option<ResourceId>,
    ui: &mut egui::Ui,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(edge_label);
        if ui
            .small_button("+")
            .on_hover_text("Add wall stack")
            .clicked()
        {
            // New wall sits above the highest existing wall, or
            // spans the full sector when this edge is empty.
            let bottom = walls
                .iter()
                .map(|w| w.heights[2].max(w.heights[3]))
                .max()
                .unwrap_or(0);
            let top = (bottom + sector_size).max(bottom + 1);
            walls.push(GridVerticalFace::flat(bottom, top, None));
            changed = true;
        }
    });
    let mut remove_at: Option<usize> = None;
    for (i, wall) in walls.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("    #{i}"));
            ui.label("bot");
            // bottom height = heights[0] = heights[1]; top = heights[2] = heights[3].
            let mut bot = wall.heights[0];
            let mut top = wall.heights[2];
            if ui
                .add(egui::DragValue::new(&mut bot).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                let bot = snap_height(bot);
                wall.heights[0] = bot;
                wall.heights[1] = bot;
                changed = true;
            }
            ui.label("top");
            if ui
                .add(egui::DragValue::new(&mut top).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                let top = snap_height(top);
                wall.heights[2] = top;
                wall.heights[3] = top;
                changed = true;
            }
            if ui.small_button("×").on_hover_text("Remove wall").clicked() {
                remove_at = Some(i);
            }
        });
        let pick_label = format!("    #{i} mat");
        let material_before = wall.material;
        let material_changed = material_picker(
            ui,
            &pick_label,
            &mut wall.material,
            material_options,
            nav_target,
        );
        if material_changed && wall.material != material_before && wall.material.is_some() {
            wall.autotile_uv(sector_size);
        }
        changed |= material_changed;
    }
    if let Some(i) = remove_at {
        walls.remove(i);
        changed = true;
    }
    changed
}

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

/// Editable row for a `[NW, NE, SE, SW]` corner-height array.
///
/// Renders one DragValue when the four corners agree (the common
/// "flat floor" case) and switches to a 2×2 grid of independent
/// DragValues -- laid out NW-NE / SW-SE so the on-screen position
/// matches the world-space corner -- once the heights diverge or
/// the user clicks the "Slope" toggle. Returns `true` whenever any
/// corner changed so the caller can mark the project dirty.
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
            // Collapse back to the NW value so the floor is flat
            // again -- predictable, matches how `flat()` builds.
            heights[1] = heights[0];
            heights[2] = heights[0];
            heights[3] = heights[0];
            changed = true;
        }
    });

    // DragValue speed must equal HEIGHT_QUANTUM so each "tick" of
    // mouse drag advances by one snap step. Combined with the
    // `snap_height` post-clamp, the value visibly walks
    // 0 → 64 → 128 → … without intermediate noise.
    if sloped {
        // 2×2 grid: NW NE on top row (z+), SW SE on bottom (z−).
        // The order in `heights` is [NW, NE, SE, SW] -- index map:
        //   top row: [0]=NW, [1]=NE
        //   bottom:  [3]=SW, [2]=SE
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
            // Indent so the field aligns with the per-corner grid above.
            ui.label("    ");
            let mut h = heights[0];
            if ui
                .add(egui::DragValue::new(&mut h).speed(HEIGHT_QUANTUM as f32))
                .changed()
            {
                let snapped = snap_height(h);
                *heights = [snapped; 4];
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

/// Material picker used by the sector / face inspector.
///
/// `jump_to` is an out-param: clicking the `→` button writes
/// the selected material's resource id into it. The caller
/// applies the navigation after its borrows release. Returns
/// `true` if the picker changed `current`.
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
                    .map(|(_, n)| n.as_str())
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
