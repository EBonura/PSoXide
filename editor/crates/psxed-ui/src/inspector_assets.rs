use super::*;

/// Orbit-camera state and cook settings shared by the import-preview
/// wrapper and the animated renderer behind it.
pub(crate) struct ModelImportPreviewContext<'a> {
    pub(crate) preview_yaw_q12: &'a mut i32,
    pub(crate) preview_pitch_q12: &'a mut i32,
    pub(crate) preview_radius: &'a mut i32,
    pub(crate) collision_radius: i32,
    pub(crate) visual_scale_q8: i32,
    pub(crate) default_visual_yaw_q12: i32,
    pub(crate) show_animation_root: bool,
    pub(crate) preview_in_place: bool,
}

pub(crate) fn draw_model_import_preview(
    ui: &mut egui::Ui,
    preview: &mut ModelImportPreview,
    selected_clip: &mut usize,
    ctx: ModelImportPreviewContext<'_>,
) {
    ui.label(RichText::new("Cooked Model").strong());
    if !draw_model_animated_import_preview(ui, preview, *selected_clip, ctx) {
        draw_model_wireframe_preview(ui, &preview.model_bytes);
    }
}

/// Atlas, cook stats, and baked-clip details for the import dialog's
/// side pane, kept separate from the preview viewport so the dialog
/// can lay them out in their own column.
pub(crate) fn draw_model_import_details(
    ui: &mut egui::Ui,
    preview: &ModelImportPreview,
    selected_clip: &mut usize,
    collision_radius: i32,
    visual_scale_q8: i32,
    default_visual_yaw_q12: i32,
) {
    ui.label(RichText::new("Atlas").strong());
    match &preview.atlas {
        Some((handle, stats)) => {
            draw_psxt_preview_block(ui, Some((handle.id(), *stats)));
        }
        None => {
            draw_psxt_preview_block(ui, None);
        }
    }

    ui.separator();
    egui::Grid::new("model-import-stats")
        .num_columns(4)
        .spacing([10.0, 3.0])
        .show(ui, |ui| {
            stat_cell(ui, "Source verts", preview.report.source_vertices);
            stat_cell(ui, "Cooked verts", preview.report.cooked_vertices);
            ui.end_row();
            stat_cell(ui, "Faces", preview.report.faces);
            stat_cell(ui, "Parts", preview.report.parts);
            ui.end_row();
            stat_cell(ui, "Joints", preview.report.joints);
            stat_cell(ui, "Local height", preview.report.local_height);
            ui.end_row();
            stat_cell(ui, "World height", preview.world_height.max(0) as usize);
            stat_cell(ui, "Actor radius", collision_radius.max(0) as usize);
            ui.end_row();
            ui.label(RichText::new("Scale").color(STUDIO_TEXT_WEAK).small());
            ui.label(
                RichText::new(format!(
                    "{} ({:.3}x)",
                    visual_scale_q8.max(1),
                    visual_scale_q8.max(1) as f32 / MODEL_SCALE_ONE_Q8 as f32
                ))
                .color(STUDIO_TEXT_WEAK)
                .monospace(),
            );
            ui.label(RichText::new("Default yaw").color(STUDIO_TEXT_WEAK).small());
            ui.label(
                RichText::new(format!(
                    "{:.1} deg",
                    q12_turns_to_degrees(default_visual_yaw_q12)
                ))
                .color(STUDIO_TEXT_WEAK)
                .monospace(),
            );
            ui.end_row();
            stat_cell(ui, "Model bytes", preview.report.model_bytes);
            stat_cell(ui, "Anim bytes", preview.report.animation_bytes);
            ui.end_row();
        });

    ui.separator();
    ui.label(RichText::new("Baked Animation Clips").strong());
    if preview.clips.is_empty() {
        ui.weak("No animation clips found in the source.");
        return;
    }
    *selected_clip = (*selected_clip).min(preview.clips.len().saturating_sub(1));
    egui::ScrollArea::vertical()
        .max_height(150.0)
        .show(ui, |ui| {
            for (index, clip) in preview.clips.iter().enumerate() {
                let root = clip
                    .root_motion
                    .map(root_motion_brief)
                    .unwrap_or_else(|| "root n/a".to_string());
                let label = format!(
                    "{}  ·  {} frames  ·  {}  ·  {}",
                    clip.name,
                    clip.frames,
                    human_bytes(clip.byte_len as u32),
                    root
                );
                if ui
                    .selectable_label(*selected_clip == index, label)
                    .clicked()
                {
                    *selected_clip = index;
                }
            }
        });
    if let Some(clip) = preview.clips.get(*selected_clip) {
        egui::CollapsingHeader::new(icons::label(icons::MOVE, "Root Motion"))
            .default_open(true)
            .show(ui, |ui| match clip.root_motion {
                Some(stats) => draw_root_motion_stats(ui, stats),
                None => {
                    ui.weak("Clip could not be parsed for root-motion stats.");
                }
            });
    }
}

pub(crate) fn draw_model_animated_import_preview(
    ui: &mut egui::Ui,
    preview: &mut ModelImportPreview,
    selected_clip: usize,
    ctx: ModelImportPreviewContext<'_>,
) -> bool {
    let ModelImportPreviewContext {
        preview_yaw_q12,
        preview_pitch_q12,
        preview_radius,
        collision_radius,
        visual_scale_q8,
        default_visual_yaw_q12,
        show_animation_root,
        preview_in_place,
    } = ctx;
    let Some(atlas) = preview.atlas_image.as_ref() else {
        return false;
    };
    let Some(clip) = preview.clips.get(selected_clip) else {
        return false;
    };

    let width = ui.available_width().clamp(560.0, 820.0);
    let height = width
        * (model_import_preview::PREVIEW_HEIGHT as f32
            / model_import_preview::PREVIEW_WIDTH as f32);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::drag());
    if response.dragged() {
        let delta = ui.input(|i| i.pointer.delta());
        *preview_yaw_q12 = (*preview_yaw_q12 - (delta.x * 6.0) as i32).rem_euclid(4096);
        *preview_pitch_q12 = (*preview_pitch_q12 - (delta.y * 4.0) as i32).clamp(64, 960);
    }
    if response.hovered() {
        let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
        if scroll_y.abs() > 0.0 {
            *preview_radius = (*preview_radius - (scroll_y * 3.0) as i32).clamp(640, 8192);
        }
        ui.ctx().set_cursor_icon(if response.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }

    let options = model_import_preview::ImportPreviewOptions {
        world_height: preview.world_height,
        visual_scale_q8: visual_scale_q8.clamp(1, u16::MAX as i32) as u16,
        visual_yaw_q12: q12_turns_to_i16(default_visual_yaw_q12),
        collision_radius: collision_radius.clamp(1, i32::MAX),
        time_seconds: ui.input(|i| i.time),
        yaw_q12: (*preview_yaw_q12).rem_euclid(4096) as u16,
        pitch_q12: (*preview_pitch_q12).rem_euclid(4096) as u16,
        radius: *preview_radius,
        focus_on_animated_bounds: true,
        preview_in_place,
        pose_offset: [0, 0, 0],
        show_animation_root,
        show_collision_guides: true,
        show_bones: false,
    };
    let Some(image) = model_import_preview::render_import_model_preview_with_options(
        &preview.model_bytes,
        &clip.bytes,
        atlas,
        options,
    ) else {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, STUDIO_PANEL);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "preview failed",
            FontId::proportional(12.0),
            Color32::from_rgb(220, 120, 100),
        );
        return true;
    };

    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));

    let texture_id = match &mut preview.animated_texture {
        Some(handle) => {
            handle.set(image, egui::TextureOptions::NEAREST);
            handle.id()
        }
        None => {
            let handle = ui.ctx().load_texture(
                "model-import-animated-preview",
                image,
                egui::TextureOptions::NEAREST,
            );
            let id = handle.id();
            preview.animated_texture = Some(handle);
            id
        }
    };

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, STUDIO_PANEL);
    painter.image(
        texture_id,
        rect,
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, STUDIO_BORDER),
        StrokeKind::Inside,
    );
    true
}

pub(crate) fn stat_cell(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.label(RichText::new(label).color(STUDIO_TEXT_WEAK).small());
    ui.label(RichText::new(value.to_string()).monospace());
}

pub(crate) fn draw_model_wireframe_preview(ui: &mut egui::Ui, model_bytes: &[u8]) {
    let width = ui.available_width().clamp(280.0, 520.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 280.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, STUDIO_PANEL);
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, STUDIO_BORDER),
        StrokeKind::Inside,
    );

    let Ok(model) = psx_asset::Model::from_bytes(model_bytes) else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "model parse failed",
            FontId::proportional(12.0),
            Color32::from_rgb(220, 120, 100),
        );
        return;
    };

    let mut projected = Vec::with_capacity(model.vertex_count() as usize);
    let mut min = [f32::INFINITY, f32::INFINITY];
    let mut max = [f32::NEG_INFINITY, f32::NEG_INFINITY];
    for i in 0..model.vertex_count() {
        let Some(vertex) = model.vertex(i) else {
            projected.push([0.0, 0.0]);
            continue;
        };
        let x = vertex.position.x as f32;
        let y = vertex.position.y as f32;
        let z = vertex.position.z as f32;
        // Lightweight isometric-ish preview: no renderer, just enough
        // shape to confirm centering, scale, and triangle continuity.
        let p = [x - z * 0.45, -y + z * 0.22];
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
        projected.push(p);
    }
    let span_x = (max[0] - min[0]).max(1.0);
    let span_y = (max[1] - min[1]).max(1.0);
    let scale = ((rect.width() - 28.0) / span_x)
        .min((rect.height() - 28.0) / span_y)
        .max(0.001);
    let to_screen = |p: [f32; 2]| -> Pos2 {
        Pos2::new(
            rect.center().x + (p[0] - (min[0] + max[0]) * 0.5) * scale,
            rect.center().y + (p[1] - (min[1] + max[1]) * 0.5) * scale,
        )
    };

    let face_count = model.face_count();
    let stride = ((face_count as usize) / 900).max(1);
    for face_index in (0..face_count).step_by(stride) {
        let Some(face) = model.face(face_index) else {
            continue;
        };
        let a = projected
            .get(face.corners[0].vertex_index as usize)
            .copied();
        let b = projected
            .get(face.corners[1].vertex_index as usize)
            .copied();
        let c = projected
            .get(face.corners[2].vertex_index as usize)
            .copied();
        let (Some(a), Some(b), Some(c)) = (a, b, c) else {
            continue;
        };
        let stroke = Stroke::new(1.0, Color32::from_rgb(150, 170, 185));
        let pa = to_screen(a);
        let pb = to_screen(b);
        let pc = to_screen(c);
        painter.line_segment([pa, pb], stroke);
        painter.line_segment([pb, pc], stroke);
        painter.line_segment([pc, pa], stroke);
    }
}

pub(crate) fn root_motion_stats(bytes: &[u8], joint_index: u16) -> Option<RootMotionStats> {
    let anim = psx_asset::Animation::from_bytes(bytes).ok()?;
    if joint_index >= anim.joint_count() {
        return None;
    }
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    let mut sum = [0i64; 3];
    let mut count = 0i64;
    let mut first = None;
    let mut last = None;
    for frame in 0..anim.frame_count() {
        let pose = anim.pose(frame, joint_index)?;
        let values = [pose.translation.x, pose.translation.y, pose.translation.z];
        if first.is_none() {
            first = Some(values);
        }
        last = Some(values);
        for axis in 0..3 {
            min[axis] = min[axis].min(values[axis]);
            max[axis] = max[axis].max(values[axis]);
            sum[axis] += values[axis] as i64;
        }
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let first = first?;
    let last = last?;
    Some(RootMotionStats {
        min,
        max,
        mean: [
            (sum[0] / count) as i32,
            (sum[1] / count) as i32,
            (sum[2] / count) as i32,
        ],
        first,
        last,
        delta: [
            last[0].saturating_sub(first[0]),
            last[1].saturating_sub(first[1]),
            last[2].saturating_sub(first[2]),
        ],
    })
}

pub(crate) fn root_motion_brief(stats: RootMotionStats) -> String {
    let span_x = stats.max[0].saturating_sub(stats.min[0]).abs();
    let span_y = stats.max[1].saturating_sub(stats.min[1]).abs();
    let span_z = stats.max[2].saturating_sub(stats.min[2]).abs();
    format!(
        "root delta {}/{}/{} · span {span_x}/{span_y}/{span_z}",
        stats.delta[0], stats.delta[1], stats.delta[2]
    )
}

pub(crate) fn draw_root_motion_stats(ui: &mut egui::Ui, stats: RootMotionStats) {
    egui::Grid::new("model-import-root-motion")
        .num_columns(6)
        .spacing([8.0, 3.0])
        .show(ui, |ui| {
            ui.label("");
            ui.label(RichText::new("first").color(STUDIO_TEXT_WEAK).small());
            ui.label(RichText::new("last").color(STUDIO_TEXT_WEAK).small());
            ui.label(RichText::new("delta").color(STUDIO_TEXT_WEAK).small());
            ui.label(RichText::new("min").color(STUDIO_TEXT_WEAK).small());
            ui.label(RichText::new("max").color(STUDIO_TEXT_WEAK).small());
            ui.label(RichText::new("mean").color(STUDIO_TEXT_WEAK).small());
            ui.end_row();
            for (axis, name) in ["X", "Y", "Z"].iter().enumerate() {
                ui.label(*name);
                ui.label(RichText::new(stats.first[axis].to_string()).monospace());
                ui.label(RichText::new(stats.last[axis].to_string()).monospace());
                ui.label(RichText::new(stats.delta[axis].to_string()).monospace());
                ui.label(RichText::new(stats.min[axis].to_string()).monospace());
                ui.label(RichText::new(stats.max[axis].to_string()).monospace());
                ui.label(RichText::new(stats.mean[axis].to_string()).monospace());
                ui.end_row();
            }
        });
    ui.label(
        RichText::new("Values are cooked Q12 pose-translation units for root joint 0.")
            .color(STUDIO_TEXT_WEAK)
            .small(),
    );
}

/// Inspector preview header: a 128×128 image of the linked PSXT
/// (centered, NEAREST-sampled so individual texels are visible at
/// editor scale) above a one-line summary. Falls back to a
/// "no preview" placeholder when the resource has no decoded
/// thumbnail (missing path / unreadable / unsupported depth).
pub(crate) fn draw_psxt_preview_block(
    ui: &mut egui::Ui,
    thumb: Option<(egui::TextureId, PsxtStats)>,
) {
    draw_psxt_preview_block_sized(ui, thumb, Vec2::splat(128.0));
}

pub(crate) fn draw_psxt_preview_block_sized(
    ui: &mut egui::Ui,
    thumb: Option<(egui::TextureId, PsxtStats)>,
    preview_size: Vec2,
) {
    ui.vertical_centered(|ui| match thumb {
        Some((id, stats)) => {
            let (rect, _) = ui.allocate_exact_size(preview_size, Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, STUDIO_PANEL);
            if stats.index_zero_transparent {
                draw_checker_preview(&painter, rect, Color32::from_rgb(20, 26, 32));
            }
            painter.image(
                id,
                rect,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, STUDIO_BORDER),
                StrokeKind::Inside,
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "{}×{}  {}bpp  {}",
                    stats.width,
                    stats.height,
                    stats.depth_bits,
                    human_bytes(stats.file_bytes)
                ))
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
        }
        None => {
            let (rect, _) = ui.allocate_exact_size(preview_size, Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, STUDIO_PANEL);
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, STUDIO_BORDER),
                StrokeKind::Inside,
            );
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "no preview",
                FontId::proportional(11.0),
                STUDIO_TEXT_WEAK,
            );
        }
    });
    ui.add_space(6.0);
}

pub(crate) fn draw_psxt_preview_block_pickable(
    ui: &mut egui::Ui,
    thumb: Option<&TexturePreviewSnapshot>,
) -> Option<PickedPsxtTexel> {
    draw_psxt_preview_block_sized_pickable(ui, thumb, Vec2::splat(128.0))
}

pub(crate) fn draw_psxt_preview_block_sized_pickable(
    ui: &mut egui::Ui,
    thumb: Option<&TexturePreviewSnapshot>,
    preview_size: Vec2,
) -> Option<PickedPsxtTexel> {
    let mut picked = None;
    ui.vertical_centered(|ui| match thumb {
        Some(snapshot) => {
            let (rect, response) = ui.allocate_exact_size(preview_size, Sense::click());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, STUDIO_PANEL);
            if snapshot.stats.index_zero_transparent {
                draw_checker_preview(&painter, rect, Color32::from_rgb(20, 26, 32));
            }
            painter.image(
                snapshot.texture_id,
                rect,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.rect_stroke(
                rect,
                4.0,
                Stroke::new(1.0, STUDIO_BORDER),
                StrokeKind::Inside,
            );
            if response.hovered() {
                ui.output_mut(|out| out.cursor_icon = egui::CursorIcon::Crosshair);
            }
            let response =
                response.on_hover_text("Click a texel to make that indexed colour transparent.");
            if response.clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    picked = sample_psxt_preview(snapshot, rect, pos);
                }
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "{}×{}  {}bpp  {}",
                    snapshot.stats.width,
                    snapshot.stats.height,
                    snapshot.stats.depth_bits,
                    human_bytes(snapshot.stats.file_bytes)
                ))
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
        }
        None => draw_psxt_preview_block(ui, None),
    });
    ui.add_space(6.0);
    picked
}

pub(crate) fn sample_psxt_preview(
    snapshot: &TexturePreviewSnapshot,
    rect: Rect,
    pos: Pos2,
) -> Option<PickedPsxtTexel> {
    let width = snapshot.image.size[0];
    let height = snapshot.image.size[1];
    if width == 0 || height == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let rel_x = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 0.999_999);
    let rel_y = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 0.999_999);
    let x = (rel_x * width as f32).floor() as usize;
    let y = (rel_y * height as f32).floor() as usize;
    let color = *snapshot.image.pixels.get(y * width + x)?;
    Some(PickedPsxtTexel {
        x: x as u16,
        y: y as u16,
        color,
    })
}

/// Tabular `key -- value` rows summarizing a `.psxt`. Mirrors the
/// fields the cooker writes so authors can sanity-check that their
/// material's texture lines up with the dimensions they expect.
pub(crate) fn draw_psxt_stats(ui: &mut egui::Ui, stats: PsxtStats) {
    let row = |ui: &mut egui::Ui, key: &str, value: String| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(key).color(STUDIO_TEXT_WEAK));
            ui.label(RichText::new(value).monospace());
        });
    };
    row(ui, "Size", format!("{}×{} px", stats.width, stats.height));
    row(
        ui,
        "Depth",
        match stats.depth_bits {
            4 => "4bpp indexed (16-color CLUT)".to_string(),
            8 => "8bpp indexed (256-color CLUT)".to_string(),
            15 => "15bpp direct".to_string(),
            other => format!("{other}bpp (?)"),
        },
    );
    row(ui, "CLUT entries", format!("{}", stats.clut_entries));
    row(
        ui,
        "Transparent 0",
        if stats.index_zero_transparent {
            "yes".to_string()
        } else {
            "no".to_string()
        },
    );
    row(ui, "Pixel data", human_bytes(stats.pixel_bytes));
    if stats.clut_bytes > 0 {
        row(ui, "CLUT data", human_bytes(stats.clut_bytes));
    }
    row(ui, "File total", human_bytes(stats.file_bytes));
}

/// Inspector for a [`ResourceData::Model`]. Lets the user edit
/// the model + atlas paths, manage the clip list, choose
/// preview / default clips, and view parsed model + clip
/// statistics. The "Register Cooked Folder" / "Import Model"
/// helpers run via deferred actions stored in
/// [`ModelInspectorAction`] so the caller can apply them after
/// dropping the mutable resource borrow.
pub(crate) fn draw_model_resource_editor(
    ui: &mut egui::Ui,
    model: &mut psxed_project::ModelResource,
    project_root: &Path,
    preview_thumb: Option<(egui::TextureId, PsxtStats)>,
    skeleton_options: &[(ResourceId, String)],
    preview_texture: &mut Option<egui::TextureHandle>,
) -> bool {
    let mut changed = false;

    // Atlas thumbnail block: same panel the Texture inspector
    // uses, but driven from `model.texture_path` via the shared
    // thumbnail cache that already learned about Model atlases.
    if preview_thumb.is_some() {
        draw_psxt_preview_block(ui, preview_thumb);
    }

    // Live-parse the model once for both socket validation and the
    // stats block. Cheap on every inspector frame for current target
    // model sizes; a cache can land when authoring scales beyond that.
    let model_path =
        psxed_project::model_import::resolve_path(&model.model_path, Some(project_root));
    let model_bytes = std::fs::read(&model_path).ok();
    let model_stats = model_bytes
        .as_deref()
        .and_then(|b| psxed_project::model_import::model_stats_from_bytes(b).ok());

    draw_model_resource_preview_panel(
        ui,
        model,
        project_root,
        model_bytes.as_deref(),
        preview_texture,
    );

    egui::CollapsingHeader::new(icons::label(icons::FOLDER, "Bundle helpers"))
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Register a cooked bundle (folder with one .psxmdl, optional .psxt, any number of .psxanim). Paths and clip metadata fill in automatically. Bundle dir resolves against the project root.",
                )
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
            // egui memory keeps the input + last status across
            // frames without leaking into ModelResource itself.
            let input_id = ui.id().with("model_bundle_input");
            let status_id = ui.id().with("model_bundle_status");
            let mut bundle_dir: String = ui
                .memory_mut(|m| m.data.get_persisted::<String>(input_id))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label("Bundle dir");
                ui.text_edit_singleline(&mut bundle_dir);
            });
            ui.memory_mut(|m| m.data.insert_persisted(input_id, bundle_dir.clone()));

            if ui
                .button(icons::label(icons::PLUS, "Register Cooked Folder"))
                .on_hover_text(
                    "Walks the directory, validates every blob, and replaces this Model's paths + clip list with the bundle contents.",
                )
                .clicked()
                && !bundle_dir.is_empty()
            {
                let path = if Path::new(&bundle_dir).is_absolute() {
                    PathBuf::from(&bundle_dir)
                } else {
                    project_root.join(&bundle_dir)
                };
                let new_status = match register_bundle_into_model(model, &path, project_root) {
                    Ok(clip_count) => {
                        changed = true;
                        format!("Registered: {clip_count} clip(s)")
                    }
                    Err(e) => format!("Failed: {e}"),
                };
                ui.memory_mut(|m| m.data.insert_persisted(status_id, new_status));
            }

            let status: String = ui
                .memory_mut(|m| m.data.get_persisted::<String>(status_id))
                .unwrap_or_default();
            if !status.is_empty() {
                let color = if status.starts_with("Failed") {
                    Color32::from_rgb(220, 120, 100)
                } else {
                    STUDIO_TEXT_WEAK
                };
                ui.colored_label(color, status);
            }

            ui.label(
                RichText::new(
                    "Use Resources -> Import Model for GLB/glTF/FBX preview, root-centering, and bundle import.",
                )
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
        });

    egui::CollapsingHeader::new(icons::label(icons::BOX, "Model"))
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Cooked .psxmdl path");
            changed |= ui.text_edit_singleline(&mut model.model_path).changed();

            ui.add_space(4.0);
            ui.label("Source GLB/glTF/FBX path (optional)");
            let mut source = model.source_path.clone().unwrap_or_default();
            let source_response = ui.text_edit_singleline(&mut source);
            if source_response.changed() {
                let trimmed = source.trim().to_string();
                model.source_path = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
                changed = true;
            }

            ui.add_space(4.0);
            ui.label("Atlas .psxt path (optional)");
            let mut atlas = model.texture_path.clone().unwrap_or_default();
            let atlas_response = ui.text_edit_singleline(&mut atlas);
            if atlas_response.changed() {
                model.texture_path = if atlas.is_empty() { None } else { Some(atlas) };
                changed = true;
            }

            ui.add_space(4.0);
            changed |= resource_id_picker(
                ui,
                "Skeleton",
                "model-skeleton-picker",
                &mut model.skeleton,
                skeleton_options,
            );

            ui.add_space(4.0);
            ui.label("World height (engine units)");
            let mut h = model.world_height as i32;
            let h_response = ui.add(egui::DragValue::new(&mut h).speed(8.0).range(0..=4096));
            if h_response.changed() {
                model.world_height = h.clamp(0, u16::MAX as i32) as u16;
                changed = true;
            }

            ui.add_space(4.0);
            ui.label("Actor collision radius (engine units)")
                .on_hover_text("Runtime actor-cylinder metadata. Use Collider components for explicit scene collision.");
            let mut radius = model.collision_radius as i32;
            let radius_response =
                ui.add(egui::DragValue::new(&mut radius).speed(8.0).range(1..=4096));
            if radius_response.changed() {
                model.collision_radius = radius.clamp(1, u16::MAX as i32) as u16;
                changed = true;
            }

            ui.add_space(4.0);
            ui.label("Scale (Q8 fixed)");
            changed |= model_scale_axis_editor(ui, "X", &mut model.scale_q8[0]);
            changed |= model_scale_axis_editor(ui, "Y", &mut model.scale_q8[1]);
            changed |= model_scale_axis_editor(ui, "Z", &mut model.scale_q8[2]);

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Default visual yaw");
                let mut yaw = model.default_visual_yaw_q12 as i32;
                let yaw_response = ui.add(
                    egui::DragValue::new(&mut yaw)
                        .range(0..=4095)
                        .speed(16.0),
                );
                ui.label(
                    RichText::new(format!("{:.1} deg", q12_turns_to_degrees(yaw)))
                        .color(STUDIO_TEXT_WEAK)
                        .monospace(),
                );
                if yaw_response.changed() {
                    model.default_visual_yaw_q12 = q12_turns_to_i16(yaw);
                    changed = true;
                }
            });
        });

    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Attachment Sockets"))
        .default_open(true)
        .show(ui, |ui| {
            ui.weak("Sockets bind equipment/VFX to a cooked joint plus an integer local offset.");
            changed |= attachment_socket_list_editor(
                ui,
                &mut model.attachments,
                model_stats.as_ref().map(|stats| stats.joint_count),
                None,
            );
        });

    if let Some(stats) = &model_stats {
        egui::CollapsingHeader::new(icons::label(icons::SCAN, "Stats"))
            .default_open(true)
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, key: &str, value: String| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(key).color(STUDIO_TEXT_WEAK));
                        ui.label(RichText::new(value).monospace());
                    });
                };
                row(ui, "Joints", format!("{}", stats.joint_count));
                row(ui, "Parts", format!("{}", stats.part_count));
                row(ui, "Vertices", format!("{}", stats.vertex_count));
                row(ui, "Faces", format!("{}", stats.face_count));
                row(ui, "Materials", format!("{}", stats.material_count));
                row(
                    ui,
                    "Atlas (header)",
                    format!("{}×{}", stats.texture_width, stats.texture_height),
                );
                row(
                    ui,
                    "Bounds X",
                    format!("{}..{}", stats.bounds_min[0], stats.bounds_max[0]),
                );
                row(
                    ui,
                    "Bounds Y",
                    format!("{}..{}", stats.bounds_min[1], stats.bounds_max[1]),
                );
                row(
                    ui,
                    "Bounds Z",
                    format!("{}..{}", stats.bounds_min[2], stats.bounds_max[2]),
                );
                row(ui, "Model bytes", format!("{}", stats.model_bytes));
            });
    } else if !model.model_path.is_empty() {
        ui.colored_label(
            Color32::from_rgb(220, 120, 100),
            format!("Failed to parse model at {}", model_path.display()),
        );
    }

    egui::CollapsingHeader::new(icons::label(icons::PLAY, "Animations"))
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Models are geometry only. Animations are skeleton-scoped AnimationClip resources: import them with the animation library tools and bind them via a Character's Animation Set.",
                )
                .color(STUDIO_TEXT_WEAK)
                .small(),
            );
        });

    changed
}

/// Adapt `psxed_project::model_import::register_cooked_model_bundle`
/// to in-place editing: build a fresh ModelResource from the
/// bundle, then overwrite `target`. Returns the registered clip
/// count so the UI status line can confirm what landed.
pub(crate) fn register_bundle_into_model(
    target: &mut psxed_project::ModelResource,
    bundle_dir: &Path,
    project_root: &Path,
) -> Result<usize, String> {
    // The library helper takes a `&mut ProjectDocument` and
    // pushes a *new* resource. Here we want to overwrite the
    // existing resource's payload in place. Easiest path:
    // build a throwaway scratch project, register into it, then
    // copy the produced ModelResource back over `target`.
    let mut scratch = psxed_project::ProjectDocument::new("scratch");
    let id = psxed_project::model_import::register_cooked_model_bundle(
        &mut scratch,
        bundle_dir,
        "Scratch",
        Some(project_root),
    )
    .map_err(|e| e.to_string())?;
    let resource = scratch.resource(id).ok_or_else(|| "lost id".to_string())?;
    let psxed_project::ResourceData::Model(model) = &resource.data else {
        return Err("scratch resource is not a Model".to_string());
    };
    let mut model = model.clone();
    // Don't copy the scratch project's Skeleton ResourceId into the
    // real project; the animation library import resolves the shared
    // skeleton by signature.
    model.skeleton = None;
    *target = model;
    // A Model is geometry only -- no clips are registered in place.
    Ok(0)
}

pub(crate) fn draw_model_resource_preview_panel(
    ui: &mut egui::Ui,
    _model: &psxed_project::ModelResource,
    _project_root: &Path,
    model_bytes: Option<&[u8]>,
    _preview_texture: &mut Option<egui::TextureHandle>,
) {
    egui::CollapsingHeader::new(icons::label(icons::EYE, "Model Preview"))
        .default_open(true)
        .show(ui, |ui| {
            let Some(model_bytes) = model_bytes else {
                ui.colored_label(Color32::from_rgb(220, 120, 100), "Model file is missing.");
                return;
            };
            // A Model is geometry only; animations live on the skeleton.
            // Preview the static mesh here and use the animation viewer
            // (bound to a Character + Animation Set) for animated playback.
            draw_model_wireframe_preview(ui, model_bytes);
        });
}

pub(crate) fn draw_animation_clip_calibration_controls(
    ui: &mut egui::Ui,
    calibration: &mut psxed_project::AnimationClipCalibration,
    id: egui::Id,
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Placement").color(STUDIO_TEXT_WEAK));
        changed |= ui
            .checkbox(&mut calibration.in_place, "In-place")
            .on_hover_text("Cancels this clip's root translation while previewing and at runtime")
            .changed();
        ui.separator();
        for (axis, label) in ["X", "Y", "Z"].iter().enumerate() {
            ui.label(RichText::new(*label).color(STUDIO_TEXT_WEAK));
            changed |= ui
                .push_id(id.with(axis), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut calibration.offset[axis])
                            .speed(4.0)
                            .range(-8192..=8192),
                    )
                    .changed()
                })
                .inner;
        }
        if ui.button("Reset").clicked() {
            *calibration = psxed_project::AnimationClipCalibration::default();
            changed = true;
        }
    });
    changed
}

pub(crate) fn resource_id_picker(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &str,
    current: &mut Option<ResourceId>,
    options: &[(ResourceId, String)],
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
            id_salt,
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)"),
        );
    });
    if let Some(id) = *current {
        if !options.iter().any(|(rid, _)| *rid == id) {
            ui.colored_label(
                Color32::from_rgb(220, 120, 100),
                "Referenced resource is missing.",
            );
        }
    }
    changed
}

pub(crate) fn draw_skeleton_resource_editor(
    ui: &mut egui::Ui,
    skeleton: &mut psxed_project::SkeletonResource,
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::WAYPOINT, "Skeleton"))
        .default_open(true)
        .show(ui, |ui| {
            changed |= drag_u16(ui, "Joint count", &mut skeleton.joint_count, 0, 512);
            ui.label("Signature");
            changed |= ui.text_edit_singleline(&mut skeleton.signature).changed();
            ui.label("Note");
            changed |= ui.text_edit_multiline(&mut skeleton.note).changed();
            ui.label(
                RichText::new(format!("Parent records: {}", skeleton.parents.len()))
                    .color(STUDIO_TEXT_WEAK)
                    .small(),
            );
        });
    changed
}

pub(crate) fn draw_animation_clip_resource_editor(
    ui: &mut egui::Ui,
    clip: &mut psxed_project::AnimationClipResource,
    project_root: &Path,
    skeleton_options: &[(ResourceId, String)],
    model_options: &[(ResourceId, String, Option<ResourceId>)],
    source_options: &[(ResourceId, String, Option<ResourceId>, Option<ResourceId>)],
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::PLAY, "Animation Clip"))
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Cooked .psxanim path");
            changed |= ui.text_edit_singleline(&mut clip.psxanim_path).changed();
            changed |= resource_id_picker(
                ui,
                "Skeleton",
                "animation-clip-skeleton-picker",
                &mut clip.skeleton,
                skeleton_options,
            );
            let compatible_models: Vec<_> = model_options
                .iter()
                .filter(|(_, _, skeleton)| clip.skeleton.is_some() && *skeleton == clip.skeleton)
                .map(|(id, name, _)| (*id, name.clone()))
                .collect();
            if clip
                .target_model
                .is_some_and(|target| !compatible_models.iter().any(|(id, _)| *id == target))
            {
                clip.target_model = None;
                changed = true;
            }
            if clip.skeleton.is_some() {
                changed |= resource_id_picker(
                    ui,
                    "Target model",
                    "animation-clip-target-model-picker",
                    &mut clip.target_model,
                    &compatible_models,
                );
            } else {
                ui.horizontal(|ui| {
                    ui.label("Target model");
                    ui.weak("Select a skeleton first");
                });
            }

            let compatible_sources: Vec<_> = source_options
                .iter()
                .filter(|(_, _, skeleton, target_model)| {
                    clip.skeleton.is_some()
                        && *skeleton == clip.skeleton
                        && match clip.target_model {
                            Some(target) => {
                                target_model.is_none_or(|source_target| source_target == target)
                            }
                            None => target_model.is_none(),
                        }
                })
                .map(|(id, name, _, _)| (*id, name.clone()))
                .collect();
            if clip
                .source
                .is_some_and(|source| !compatible_sources.iter().any(|(id, _)| *id == source))
            {
                clip.source = None;
                changed = true;
            }
            if clip.skeleton.is_some() {
                changed |= resource_id_picker(
                    ui,
                    "Source",
                    "animation-clip-source-picker",
                    &mut clip.source,
                    &compatible_sources,
                );
            } else {
                ui.horizontal(|ui| {
                    ui.label("Source");
                    ui.weak("Select a skeleton first");
                });
            }

            ui.horizontal(|ui| {
                ui.label("Bake");
                let before = clip.bake;
                egui::ComboBox::from_id_salt("animation-clip-bake")
                    .selected_text(clip.bake.label())
                    .show_ui(ui, |ui| {
                        for bake in psxed_project::AnimationClipBakeKind::ALL {
                            ui.selectable_value(&mut clip.bake, bake, bake.label());
                        }
                    });
                if clip.bake != before {
                    changed = true;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Role");
                let before = clip.role;
                egui::ComboBox::from_id_salt("animation-clip-role")
                    .selected_text(clip.role.label())
                    .show_ui(ui, |ui| {
                        for role in psxed_project::AnimationRole::ALL {
                            ui.selectable_value(&mut clip.role, role, role.label());
                        }
                    });
                if clip.role != before {
                    changed = true;
                }
            });
            changed |= ui.checkbox(&mut clip.looping, "Looping").changed();
            let calibration_id = ui.id().with("animation-clip-calibration");
            changed |=
                draw_animation_clip_calibration_controls(ui, &mut clip.calibration, calibration_id);

            let mut tags = clip.tags.join(", ");
            ui.horizontal(|ui| {
                ui.label("Tags");
                if ui.text_edit_singleline(&mut tags).changed() {
                    clip.tags = tags
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(ToString::to_string)
                        .collect();
                    changed = true;
                }
            });
        });

    let path = psxed_project::model_import::resolve_path(&clip.psxanim_path, Some(project_root));
    if !clip.psxanim_path.trim().is_empty() {
        match std::fs::read(&path) {
            Ok(bytes) => match psx_asset::Animation::from_bytes(&bytes) {
                Ok(anim) => {
                    egui::CollapsingHeader::new(icons::label(icons::SCAN, "Stats"))
                        .default_open(true)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} frames @ {} Hz, {} joints, {} bytes",
                                    anim.frame_count(),
                                    anim.sample_rate_hz(),
                                    anim.joint_count(),
                                    bytes.len()
                                ))
                                .color(STUDIO_TEXT_WEAK),
                            );
                        });
                }
                Err(_) => {
                    ui.colored_label(
                        Color32::from_rgb(220, 120, 100),
                        format!("Failed to parse animation at {}", path.display()),
                    );
                }
            },
            Err(_) => {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    format!("Failed to parse animation at {}", path.display()),
                );
            }
        }
    }
    changed
}

pub(crate) fn draw_animation_source_resource_editor(
    ui: &mut egui::Ui,
    source: &mut psxed_project::AnimationSourceResource,
    project_root: &Path,
    skeleton_options: &[(ResourceId, String)],
    model_options: &[(ResourceId, String, Option<ResourceId>)],
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::PLAY, "Animation Source"))
        .default_open(true)
        .show(ui, |ui| {
            ui.label("Source file");
            changed |= ui.text_edit_singleline(&mut source.source_path).changed();
            ui.horizontal(|ui| {
                ui.label("Clip name");
                changed |= ui.text_edit_singleline(&mut source.clip_name).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Provider");
                let before = source.provider;
                egui::ComboBox::from_id_salt("animation-source-provider")
                    .selected_text(source.provider.label())
                    .show_ui(ui, |ui| {
                        for provider in psxed_project::AnimationSourceProvider::ALL {
                            ui.selectable_value(&mut source.provider, provider, provider.label());
                        }
                    });
                if source.provider != before {
                    changed = true;
                }
            });
            changed |= resource_id_picker(
                ui,
                "Skeleton",
                "animation-source-skeleton-picker",
                &mut source.skeleton,
                skeleton_options,
            );
            let compatible_models: Vec<_> = model_options
                .iter()
                .filter(|(_, _, skeleton)| {
                    source.skeleton.is_some() && *skeleton == source.skeleton
                })
                .map(|(id, name, _)| (*id, name.clone()))
                .collect();
            if source
                .target_model
                .is_some_and(|target| !compatible_models.iter().any(|(id, _)| *id == target))
            {
                source.target_model = None;
                changed = true;
            }
            if source.skeleton.is_some() {
                changed |= resource_id_picker(
                    ui,
                    "Target model",
                    "animation-source-target-model-picker",
                    &mut source.target_model,
                    &compatible_models,
                );
            } else {
                ui.horizontal(|ui| {
                    ui.label("Target model");
                    ui.weak("Select a skeleton first");
                });
            }

            ui.horizontal(|ui| {
                ui.label("Role");
                let before = source.role;
                egui::ComboBox::from_id_salt("animation-source-role")
                    .selected_text(source.role.label())
                    .show_ui(ui, |ui| {
                        for role in psxed_project::AnimationRole::ALL {
                            ui.selectable_value(&mut source.role, role, role.label());
                        }
                    });
                if source.role != before {
                    changed = true;
                }
            });
            changed |= ui.checkbox(&mut source.looping, "Looping").changed();

            let mut tags = source.tags.join(", ");
            ui.horizontal(|ui| {
                ui.label("Tags");
                if ui.text_edit_singleline(&mut tags).changed() {
                    source.tags = tags
                        .split(',')
                        .map(str::trim)
                        .filter(|tag| !tag.is_empty())
                        .map(ToString::to_string)
                        .collect();
                    changed = true;
                }
            });
        });

    if !source.source_path.trim().is_empty() {
        let status = animation_source_path_status(&source.source_path, project_root);
        egui::CollapsingHeader::new(icons::label(icons::SCAN, "Source Status"))
            .default_open(true)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(status.label).color(if status.found {
                        STUDIO_TEXT_WEAK
                    } else {
                        Color32::from_rgb(220, 120, 100)
                    }),
                );
                ui.label(
                    RichText::new(
                        "Source clips are library candidates. Bake or retarget them into cooked Animation Clip resources before runtime.",
                    )
                    .color(STUDIO_TEXT_WEAK)
                    .small(),
                );
            });
    }
    changed
}

pub(crate) struct AnimationSourcePathStatus {
    pub(crate) found: bool,
    pub(crate) label: String,
}

pub(crate) fn animation_source_path_status(
    source_path: &str,
    project_root: &Path,
) -> AnimationSourcePathStatus {
    if let Some((archive, entry)) = split_archive_animation_source_path(source_path) {
        let archive_path = psxed_project::model_import::resolve_path(archive, Some(project_root));
        let found = archive_path.is_file();
        let label = if found {
            format!("Found archive: {} :: {}", archive_path.display(), entry)
        } else {
            format!("Missing archive: {} :: {}", archive_path.display(), entry)
        };
        return AnimationSourcePathStatus { found, label };
    }

    let path = psxed_project::model_import::resolve_path(source_path, Some(project_root));
    let found = path.is_file();
    let label = if found {
        format!("Found: {}", path.display())
    } else {
        format!("Missing: {}", path.display())
    };
    AnimationSourcePathStatus { found, label }
}

pub(crate) fn split_archive_animation_source_path(path: &str) -> Option<(&str, &str)> {
    let (archive, entry) = path.split_once("::")?;
    (!archive.trim().is_empty() && !entry.trim().is_empty()).then_some((archive, entry))
}

pub(crate) fn make_animation_bake_temp_dir() -> Result<PathBuf, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "psoxide-animation-bake-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

pub(crate) fn materialize_authoring_source_path(
    source_path: &str,
    project_root: &Path,
    temp_dir: &Path,
) -> Result<PathBuf, String> {
    if let Some((archive, entry)) = split_archive_animation_source_path(source_path) {
        let archive_path = psxed_project::model_import::resolve_path(archive, Some(project_root));
        return extract_archive_entry_to_temp(&archive_path, entry, temp_dir);
    }
    let path = psxed_project::model_import::resolve_path(source_path, Some(project_root));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("missing source file {}", path.display()))
    }
}

pub(crate) fn extract_archive_entry_to_temp(
    archive_path: &Path,
    entry_name: &str,
    temp_dir: &Path,
) -> Result<PathBuf, String> {
    let file = std::fs::File::open(archive_path)
        .map_err(|error| format!("{}: {error}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("{}: {error}", archive_path.display()))?;
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|error| format!("{}::{}: {error}", archive_path.display(), entry_name))?;
    let out_path = temp_dir.join(sanitized_archive_entry_filename(entry_name));
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    std::io::Read::read_to_end(&mut entry, &mut bytes)
        .map_err(|error| format!("{}::{}: {error}", archive_path.display(), entry_name))?;
    std::fs::write(&out_path, bytes).map_err(|error| format!("{}: {error}", out_path.display()))?;
    Ok(out_path)
}

pub(crate) fn sanitized_archive_entry_filename(entry_name: &str) -> String {
    let file_name = Path::new(entry_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source.fbx");
    let mut out = String::with_capacity(file_name.len());
    for ch in file_name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "source.fbx".to_string()
    } else {
        out
    }
}

pub(crate) fn draw_animation_set_resource_editor(
    ui: &mut egui::Ui,
    set: &mut psxed_project::AnimationSetResource,
    skeleton_options: &[(ResourceId, String)],
    clip_options: &[AnimationClipOption],
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(icons::label(icons::LAYERS, "Action Map"))
        .default_open(true)
        .show(ui, |ui| {
            changed |= resource_id_picker(
                ui,
                "Skeleton",
                "animation-set-skeleton-picker",
                &mut set.skeleton,
                skeleton_options,
            );
            for action in psxed_project::CharacterAnimationAction::AUTHORABLE {
                let mut current = set.action_clip(action);
                changed |= animation_resource_picker(
                    ui,
                    action.label(),
                    &format!("animation-set-action-{}", action.to_index()),
                    &mut current,
                    set.skeleton,
                    clip_options,
                    action.role_hint(),
                );
                if current != set.action_clip(action) {
                    set.set_action_clip(action, current);
                    changed = true;
                }

                let Some(clip) = set.action_clip(action) else {
                    continue;
                };
                let defaults = psxed_project::CharacterActionOptions::for_action(action);
                let mut options = set
                    .action_binding(action)
                    .and_then(|binding| binding.options)
                    .unwrap_or(defaults);
                let mut speed = f32::from(options.speed_q8) / 256.0;
                let speed_changed = ui
                    .horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.label(RichText::new("Runtime speed").small().color(STUDIO_TEXT_WEAK));
                        ui.add(
                            egui::DragValue::new(&mut speed)
                                .speed(0.01)
                                .range(0.25..=4.0)
                                .fixed_decimals(2)
                                .suffix("x"),
                        )
                        .on_hover_text(
                            "Saved playback speed for this action; Animation Studio and the game use the same value",
                        )
                        .changed()
                    })
                    .inner;
                if speed_changed {
                    options.speed_q8 = (speed * 256.0).round().clamp(
                        psxed_project::ACTION_SPEED_MIN_Q8 as f32,
                        psxed_project::ACTION_SPEED_MAX_Q8 as f32,
                    ) as u16;
                    if let Some(binding) = set
                        .action_clips
                        .iter_mut()
                        .find(|binding| binding.action == action)
                    {
                        binding.options = (options != defaults).then_some(options);
                    } else {
                        set.action_clips.push(psxed_project::AnimationActionBinding {
                            action,
                            clip,
                            options: Some(options),
                        });
                    }
                    changed = true;
                }
            }
        });

    egui::CollapsingHeader::new(icons::label(icons::PLAY, "Extra Clips"))
        .default_open(false)
        .show(ui, |ui| {
            let mut remove: Option<usize> = None;
            for (index, clip_id) in set.clips.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let mut current = Some(*clip_id);
                    changed |= animation_resource_picker(
                        ui,
                        &format!("Clip {index}"),
                        &format!("animation-set-extra-{index}"),
                        &mut current,
                        set.skeleton,
                        clip_options,
                        None,
                    );
                    if let Some(new_id) = current {
                        if *clip_id != new_id {
                            *clip_id = new_id;
                            changed = true;
                        }
                    }
                    if ui.small_button(icons::label(icons::TRASH, "")).clicked() {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                set.clips.remove(index);
                changed = true;
            }
            if ui.button(icons::label(icons::PLUS, "Add Clip")).clicked() {
                if let Some(option) = clip_options
                    .iter()
                    .find(|option| animation_option_matches_skeleton(option, set.skeleton))
                {
                    set.clips.push(option.id);
                    changed = true;
                }
            }
        });
    changed
}

#[derive(Clone, Debug)]
pub(crate) struct AnimationClipOption {
    pub(crate) id: ResourceId,
    pub(crate) name: String,
    pub(crate) skeleton: Option<ResourceId>,
    pub(crate) role: psxed_project::AnimationRole,
}

pub(crate) fn animation_resource_picker(
    ui: &mut egui::Ui,
    label: &str,
    id_salt: &str,
    current: &mut Option<ResourceId>,
    skeleton: Option<ResourceId>,
    options: &[AnimationClipOption],
    role_hint: Option<psxed_project::AnimationRole>,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        let preview = current
            .and_then(|id| {
                options
                    .iter()
                    .find(|option| option.id == id)
                    .map(|option| option.name.as_str())
            })
            .unwrap_or("(none)");
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(preview)
            .height(360.0)
            .show_ui(ui, |ui| {
                ui.set_min_width(380.0);
                let filter = animation_picker_filter(ui, ui.id().with((id_salt, "filter")));
                let matching = options
                    .iter()
                    .filter(|option| {
                        animation_option_matches_skeleton(option, skeleton)
                            && animation_name_matches_filter(&option.name, &filter)
                    })
                    .count();
                ui.label(
                    RichText::new(format!("{matching} compatible imported clips"))
                        .small()
                        .color(STUDIO_TEXT_WEAK),
                );
                ui.separator();
                if ui.selectable_label(current.is_none(), "(none)").clicked() {
                    *current = None;
                    changed = true;
                }
                for option in options
                    .iter()
                    .filter(|option| animation_option_matches_skeleton(option, skeleton))
                    .filter(|option| animation_name_matches_filter(&option.name, &filter))
                {
                    let label = if matches!(option.role, psxed_project::AnimationRole::Generic) {
                        option.name.clone()
                    } else {
                        format!("{} ({})", option.name, option.role.label())
                    };
                    if ui
                        .selectable_label(*current == Some(option.id), label)
                        .clicked()
                    {
                        *current = Some(option.id);
                        changed = true;
                    }
                }
            });
    });
    if let Some(id) = *current {
        match options.iter().find(|option| option.id == id) {
            None => {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Animation clip resource is missing.",
                );
            }
            Some(option) if !animation_option_matches_skeleton(option, skeleton) => {
                ui.colored_label(
                    Color32::from_rgb(220, 120, 100),
                    "Animation clip targets a different skeleton.",
                );
            }
            Some(option)
                if role_hint.is_some_and(|role| {
                    option.role != role
                        && !matches!(option.role, psxed_project::AnimationRole::Generic)
                }) =>
            {
                ui.colored_label(
                    Color32::from_rgb(220, 160, 80),
                    format!(
                        "Clip role is {}; expected {}.",
                        option.role.label(),
                        role_hint.unwrap().label()
                    ),
                );
            }
            Some(_) => {}
        }
    }
    changed
}

pub(crate) fn animation_option_matches_skeleton(
    option: &AnimationClipOption,
    skeleton: Option<ResourceId>,
) -> bool {
    skeleton.is_none() || option.skeleton == skeleton
}

/// Combo-box picker for a Material's linked texture. `current` is
/// the live `material.texture` field; `options` is every Texture
/// resource in the project. Returns true when the selection moved
/// One segment of the inspector breadcrumb.
///
/// Rendered as plain bold text when `nav` is `None` (the current
/// view, no click target) or as a clickable link otherwise -- a
/// click fires the deferred jump-to that the inspector applies
/// once its mutable borrows release.
pub(crate) struct BreadcrumbCrumb {
    pub(crate) label: String,
    pub(crate) nav: Option<ResourceId>,
}

/// Render an inspector breadcrumb: `Face › Material › Texture`
/// (or any subset). Click any link-style crumb to jump.
pub(crate) fn draw_breadcrumb(
    ui: &mut egui::Ui,
    crumbs: &[BreadcrumbCrumb],
    nav_target: &mut Option<ResourceId>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, crumb) in crumbs.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("›").color(STUDIO_TEXT_WEAK));
            }
            match crumb.nav {
                None => {
                    // Current view -- non-interactive, slightly
                    // brighter so the eye lands on "where I am".
                    ui.label(RichText::new(&crumb.label).strong());
                }
                Some(id) => {
                    if ui.link(&crumb.label).clicked() {
                        *nav_target = Some(id);
                    }
                }
            }
        }
    });
}

pub(crate) fn texture_resource_picker(
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
            ui.id().with(("texture-resource-picker", label)),
            current,
            preview,
            options,
            SearchablePickerConfig::optional("(none)"),
        );
        if let Some(id) = *current {
            if ui
                .small_button("→")
                .on_hover_text("Open this texture in the inspector")
                .clicked()
            {
                *jump_to = Some(id);
            }
        }
    });
    changed
}

/// Snapshot of every Model resource and its clip names. Built
/// before the mutable borrow on a Resource so the Character Profile
/// inspector can populate model + clip dropdowns without
/// fighting the live `&mut CharacterResource`.
pub(crate) struct CharacterEditorContext {
    /// `(model id, model display name, clip names in order)`.
    pub(crate) models: Vec<(ResourceId, String, Vec<String>)>,
    pub(crate) materials: Vec<(ResourceId, String)>,
    /// `(model id, skeleton id)`.
    pub(crate) model_skeletons: Vec<(ResourceId, Option<ResourceId>)>,
    pub(crate) animation_sets: Vec<AnimationSetOption>,
    pub(crate) animation_clips: Vec<(ResourceId, String)>,
}

pub(crate) fn build_character_editor_context(project: &ProjectDocument) -> CharacterEditorContext {
    CharacterEditorContext {
        models: collect_model_options(project),
        materials: project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Material(_) => Some((resource.id, resource.name.clone())),
                _ => None,
            })
            .collect(),
        model_skeletons: project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::Model(model) => Some((resource.id, model.skeleton)),
                _ => None,
            })
            .collect(),
        animation_sets: collect_animation_set_options(project),
        animation_clips: project
            .resources
            .iter()
            .filter_map(|resource| match &resource.data {
                ResourceData::AnimationClip(_) => Some((resource.id, resource.name.clone())),
                _ => None,
            })
            .collect(),
    }
}

impl CharacterEditorContext {
    pub(crate) fn model_skeleton(&self, model: Option<ResourceId>) -> Option<ResourceId> {
        let model = model?;
        self.model_skeletons
            .iter()
            .find_map(|(id, skeleton)| (*id == model).then_some(*skeleton))
            .flatten()
    }

    pub(crate) fn animation_set(&self, set: Option<ResourceId>) -> Option<&AnimationSetOption> {
        let set = set?;
        self.animation_sets.iter().find(|option| option.id == set)
    }

    pub(crate) fn animation_clip_name(&self, clip: ResourceId) -> &str {
        self.animation_clips
            .iter()
            .find_map(|(id, name)| (*id == clip).then_some(name.as_str()))
            .unwrap_or("(missing)")
    }
}
