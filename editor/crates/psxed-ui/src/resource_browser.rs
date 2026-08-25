use super::*;

#[derive(Debug, Clone)]
pub(crate) struct ProjectFileRow {
    pub(crate) depth: usize,
    pub(crate) name: String,
    pub(crate) folder: bool,
    pub(crate) key: String,
    pub(crate) ancestors: Vec<String>,
    pub(crate) icon: char,
    pub(crate) resource: Option<ResourceId>,
}

pub(crate) fn project_filesystem_rows(project: &ProjectDocument) -> Vec<ProjectFileRow> {
    let mut rows = Vec::new();
    rows.push(ProjectFileRow {
        depth: 0,
        name: "res://".to_string(),
        folder: true,
        key: "res://".to_string(),
        ancestors: Vec::new(),
        icon: icons::FOLDER,
        resource: None,
    });
    rows.push(ProjectFileRow {
        depth: 1,
        name: "maps".to_string(),
        folder: true,
        key: "res://maps".to_string(),
        ancestors: vec!["res://".to_string()],
        icon: icons::FOLDER,
        resource: None,
    });
    rows.push(ProjectFileRow {
        depth: 2,
        name: format!("{}.map", snake_name(&project.active_scene().name)),
        folder: false,
        key: "res://maps/main.map".to_string(),
        ancestors: vec!["res://".to_string(), "res://maps".to_string()],
        icon: icons::GRID,
        resource: None,
    });

    push_resource_folder(project, &mut rows, "materials", ResourceFilter::Material);
    push_resource_folder(project, &mut rows, "models", ResourceFilter::Model);
    push_resource_folder(project, &mut rows, "animations", ResourceFilter::Animation);
    push_resource_folder(project, &mut rows, "characters", ResourceFilter::Character);
    push_resource_folder(project, &mut rows, "weapons", ResourceFilter::Weapon);
    push_resource_folder(project, &mut rows, "meshes", ResourceFilter::Mesh);
    push_resource_folder(project, &mut rows, "other", ResourceFilter::Other);
    rows
}

pub(crate) fn push_resource_folder(
    project: &ProjectDocument,
    rows: &mut Vec<ProjectFileRow>,
    folder: &str,
    filter: ResourceFilter,
) {
    let key = format!("res://{folder}");
    rows.push(ProjectFileRow {
        depth: 1,
        name: folder.to_string(),
        folder: true,
        key: key.clone(),
        ancestors: vec!["res://".to_string()],
        icon: icons::FOLDER,
        resource: None,
    });
    for resource in project
        .resources
        .iter()
        .filter(|resource| filter.matches(&resource.data))
    {
        rows.push(ProjectFileRow {
            depth: 2,
            name: resource_file_name(resource),
            folder: false,
            key: format!("{key}/{}", resource.id.raw()),
            ancestors: vec!["res://".to_string(), key.clone()],
            icon: resource_lucide_icon(&resource.data),
            resource: Some(resource.id),
        });
    }
}

pub(crate) fn project_filesystem_display_rows<'a>(
    rows: &'a [ProjectFileRow],
    filter: &str,
    collapsed_folders: &HashSet<String>,
) -> Vec<&'a ProjectFileRow> {
    rows.iter()
        .filter(|row| {
            if row
                .ancestors
                .iter()
                .any(|ancestor| collapsed_folders.contains(ancestor))
            {
                return false;
            }
            row.folder || filter.is_empty() || row.name.to_ascii_lowercase().contains(filter)
        })
        .collect()
}

pub(crate) fn draw_project_file_row(
    ui: &mut egui::Ui,
    row: &ProjectFileRow,
    selected_resource: Option<ResourceId>,
    selected_resources: &HashSet<ResourceId>,
    filter: &str,
    collapsed_folders: &HashSet<String>,
) -> Option<ProjectFileRowAction> {
    if !row.folder && !filter.is_empty() && !row.name.to_ascii_lowercase().contains(filter) {
        return None;
    }

    let mut action = None;
    ui.horizontal(|ui| {
        ui.add_space(row.depth as f32 * 14.0);
        let display_name = compact_middle(&row.name, dock_label_limit(row.depth));
        let label_was_compacted = display_name != row.name;
        if row.folder {
            let chevron = if collapsed_folders.contains(&row.key) {
                icons::CHEVRON_RIGHT
            } else {
                icons::CHEVRON_DOWN
            };
            let label = format!("{chevron}  {}", icons::label(row.icon, &display_name));
            let response = ui.add(egui::SelectableLabel::new(
                false,
                RichText::new(label).color(STUDIO_TEXT_WEAK),
            ));
            if response.clicked() {
                action = Some(ProjectFileRowAction::ToggleFolder(row.key.clone()));
            }
            if label_was_compacted {
                response.on_hover_text(row.name.clone());
            }
        } else {
            ui.add_space(18.0);
            let label = icons::label(row.icon, &display_name);
            let selected = row
                .resource
                .is_some_and(|id| selected_resources.contains(&id))
                || (selected_resources.is_empty() && row.resource == selected_resource);
            let response = ui.selectable_label(selected, label);
            if response.clicked() {
                if let Some(id) = row.resource {
                    action = Some(ProjectFileRowAction::Select(ResourceClick {
                        id,
                        modifiers: ui.input(|input| input.modifiers),
                    }));
                }
            }
            if label_was_compacted {
                response.on_hover_text(row.name.clone());
            }
        }
    });
    action
}

pub(crate) fn resource_file_name(resource: &Resource) -> String {
    match &resource.data {
        ResourceData::Texture { psxt_path } => cooked_name(&resource.name, psxt_path, "psxt"),
        ResourceData::Material(material) => cooked_name(
            &resource.name,
            material.psxt_path.as_deref().unwrap_or(""),
            "psxt",
        ),
        ResourceData::Model(model) => cooked_name(&resource.name, &model.model_path, "psxmdl"),
        ResourceData::Skeleton(_) => cooked_name(&resource.name, "", "skeleton"),
        ResourceData::AnimationSource(source) => {
            cooked_name(&resource.name, &source.source_path, "animsrc")
        }
        ResourceData::AnimationClip(clip) => {
            cooked_name(&resource.name, &clip.psxanim_path, "psxanim")
        }
        ResourceData::AnimationSet(_) => cooked_name(&resource.name, "", "animset"),
        ResourceData::Character(_) => cooked_name(&resource.name, "", "profile"),
        ResourceData::Weapon(_) => cooked_name(&resource.name, "", "weapon"),
        ResourceData::Mesh { source_path } => cooked_name(&resource.name, source_path, "psxmesh"),
        ResourceData::Scene { source_path } => cooked_name(&resource.name, source_path, "room"),
        ResourceData::Script { source_path } => cooked_name(&resource.name, source_path, "script"),
        ResourceData::Audio { source_path } => cooked_name(&resource.name, source_path, "vag"),
    }
}

pub(crate) fn cooked_name(name: &str, source_path: &str, ext: &str) -> String {
    let stem = Path::new(source_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or(name);
    format!("{}.{}", snake_name(stem), ext)
}

pub(crate) fn snake_name(name: &str) -> String {
    let mut out = String::new();
    let mut previous_was_sep = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            previous_was_sep = false;
        } else if !previous_was_sep {
            out.push('_');
            previous_was_sep = true;
        }
    }
    out.trim_matches('_').to_string()
}

pub(crate) fn resource_filter_counts(project: &ProjectDocument) -> [(ResourceFilter, usize); 7] {
    let mut material = 0;
    let mut model = 0;
    let mut animation = 0;
    let mut character = 0;
    let mut weapon = 0;
    let mut mesh = 0;
    let mut other = 0;
    for resource in &project.resources {
        if resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX) {
            continue;
        }
        match &resource.data {
            // Legacy Texture resources are folded into materials at
            // load; none survive in memory.
            ResourceData::Texture { .. } | ResourceData::Material(_) => material += 1,
            ResourceData::Model(_) => model += 1,
            ResourceData::Skeleton(_)
            | ResourceData::AnimationSource(_)
            | ResourceData::AnimationClip(_)
            | ResourceData::AnimationSet(_) => animation += 1,
            ResourceData::Character(_) => character += 1,
            ResourceData::Weapon(_) => weapon += 1,
            ResourceData::Mesh { .. } => mesh += 1,
            ResourceData::Scene { .. } => {}
            ResourceData::Script { .. } | ResourceData::Audio { .. } => other += 1,
        }
    }
    [
        (ResourceFilter::Material, material),
        (ResourceFilter::Model, model),
        (ResourceFilter::Animation, animation),
        (ResourceFilter::Character, character),
        (ResourceFilter::Weapon, weapon),
        (ResourceFilter::Mesh, mesh),
        (ResourceFilter::Other, other),
    ]
}

pub(crate) fn resource_matches_filter(
    resource: &Resource,
    filter: ResourceFilter,
    search: &str,
) -> bool {
    if matches!(&resource.data, ResourceData::Scene { .. }) {
        return false;
    }
    if resource.name.starts_with(AUTO_PAINT_BLEND_PREFIX) {
        return filter.matches(&resource.data) && search.contains("paint blend");
    }
    if !filter.matches(&resource.data) {
        return false;
    }
    if search.is_empty() {
        return true;
    }
    resource.name.to_ascii_lowercase().contains(search)
        || resource.data.label().to_ascii_lowercase().contains(search)
        || matches!(
            &resource.data,
            ResourceData::Material(material)
                if material.texture_mode == MaterialTextureMode::Transition
                    && "transition material".contains(search)
        )
        || resource_source_path(resource)
            .is_some_and(|path| path.to_ascii_lowercase().contains(search))
}

pub(crate) fn resource_source_path(resource: &Resource) -> Option<&str> {
    match &resource.data {
        ResourceData::Texture { psxt_path } => Some(psxt_path.as_str()),
        ResourceData::Material(material) => material.psxt_path.as_deref(),
        ResourceData::Model(model) => Some(model.model_path.as_str()),
        ResourceData::AnimationSource(source) => Some(source.source_path.as_str()),
        ResourceData::AnimationClip(clip) => Some(clip.psxanim_path.as_str()),
        ResourceData::Mesh { source_path }
        | ResourceData::Scene { source_path }
        | ResourceData::Script { source_path }
        | ResourceData::Audio { source_path } => Some(source_path.as_str()),
        ResourceData::Skeleton(_)
        | ResourceData::AnimationSet(_)
        | ResourceData::Character(_)
        | ResourceData::Weapon(_) => None,
    }
}

pub(crate) fn resource_lucide_icon(data: &ResourceData) -> char {
    match data {
        ResourceData::Texture { .. } => icons::PALETTE,
        ResourceData::Material(_) => icons::BLEND,
        ResourceData::Model(_) => icons::BOX,
        ResourceData::Skeleton(_) => icons::WAYPOINT,
        ResourceData::AnimationSource(_) => icons::PLAY,
        ResourceData::AnimationClip(_) => icons::PLAY,
        ResourceData::AnimationSet(_) => icons::LAYERS,
        ResourceData::Character(_) => icons::MAP_PIN,
        ResourceData::Weapon(_) => icons::WAYPOINT,
        ResourceData::Mesh { .. } => icons::BOX,
        ResourceData::Scene { .. } => icons::GRID,
        ResourceData::Script { .. } => icons::FILE,
        ResourceData::Audio { .. } => icons::AUDIO_LINES,
    }
}

pub(crate) fn resource_lucide_color(data: &ResourceData, selected: bool) -> Color32 {
    if selected {
        return Color32::WHITE;
    }

    match data {
        ResourceData::Texture { .. } => Color32::from_rgb(163, 182, 198),
        ResourceData::Material(_) => Color32::from_rgb(208, 112, 162),
        ResourceData::Model(_) => Color32::from_rgb(186, 178, 124),
        ResourceData::Skeleton(_) => Color32::from_rgb(144, 180, 216),
        ResourceData::AnimationSource(_) => Color32::from_rgb(170, 140, 220),
        ResourceData::AnimationClip(_) => Color32::from_rgb(126, 164, 220),
        ResourceData::AnimationSet(_) => Color32::from_rgb(142, 190, 154),
        ResourceData::Character(_) => Color32::from_rgb(120, 220, 148),
        ResourceData::Weapon(_) => Color32::from_rgb(222, 196, 112),
        ResourceData::Mesh { .. } => Color32::from_rgb(156, 174, 190),
        ResourceData::Scene { .. } => Color32::from_rgb(209, 118, 71),
        ResourceData::Script { .. } => Color32::from_rgb(188, 176, 104),
        ResourceData::Audio { .. } => Color32::from_rgb(104, 202, 188),
    }
}

pub(crate) fn resource_can_open_in_animation_viewer(data: &ResourceData) -> bool {
    matches!(
        data,
        ResourceData::Model(_)
            | ResourceData::AnimationSource(_)
            | ResourceData::AnimationClip(_)
            | ResourceData::AnimationSet(_)
            | ResourceData::Character(_)
            | ResourceData::Weapon(_)
    )
}

pub(crate) fn draw_resource_card(
    ui: &mut egui::Ui,
    project: &ProjectDocument,
    resource: &Resource,
    selected: bool,
    thumb: Option<egui::TextureId>,
) -> egui::Response {
    let size = Vec2::new(RESOURCE_CARD_WIDTH, RESOURCE_CARD_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let fill = if selected {
        if response.hovered() {
            STUDIO_SELECTION_HOVER
        } else {
            STUDIO_SELECTION
        }
    } else if response.hovered() {
        STUDIO_HOVER
    } else {
        STUDIO_PANEL_HEADER
    };
    painter.rect_filled(rect, 6.0, fill);
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(
            if selected { 1.5 } else { 1.0 },
            if selected {
                STUDIO_ACCENT
            } else if response.hovered() {
                STUDIO_BORDER
            } else {
                STUDIO_BORDER_DARK
            },
        ),
        StrokeKind::Inside,
    );

    let preview = Rect::from_min_size(rect.min + Vec2::new(8.0, 8.0), Vec2::new(104.0, 72.0));
    draw_resource_preview(&painter, preview, project, resource, thumb);
    painter.rect_stroke(
        preview,
        2.0,
        Stroke::new(1.0, STUDIO_BORDER_DARK),
        StrokeKind::Inside,
    );
    let badge = Rect::from_min_size(preview.left_top() + Vec2::new(4.0, 4.0), Vec2::splat(22.0));
    painter.rect_filled(badge, 4.0, Color32::from_rgba_unmultiplied(8, 12, 16, 192));
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        resource_lucide_icon(&resource.data).to_string(),
        icons::font(14.0),
        resource_lucide_color(&resource.data, selected),
    );
    painter.text(
        rect.center_top() + Vec2::new(0.0, 88.0),
        Align2::CENTER_TOP,
        compact_middle(&resource.name, 16),
        FontId::monospace(12.0),
        Color32::from_rgb(225, 231, 240),
    );
    painter.text(
        rect.center_top() + Vec2::new(0.0, 110.0),
        Align2::CENTER_TOP,
        resource_detail(resource),
        FontId::monospace(10.0),
        STUDIO_TEXT_WEAK,
    );
    if response.dragged() {
        response.dnd_set_drag_payload::<ResourceId>(resource.id);
        let pointer_pos = ui
            .ctx()
            .input(|input| input.pointer.interact_pos())
            .unwrap_or_else(|| rect.center());
        ui.painter().text(
            pointer_pos + Vec2::new(12.0, 0.0),
            Align2::LEFT_CENTER,
            format!("{} {}", resource_lucide_icon(&resource.data), resource.name),
            FontId::proportional(12.0),
            STUDIO_ACCENT,
        );
    }
    response
}

pub(crate) fn draw_resource_preview(
    painter: &egui::Painter,
    preview: Rect,
    _project: &ProjectDocument,
    resource: &Resource,
    thumb: Option<egui::TextureId>,
) {
    match &resource.data {
        ResourceData::Material(material) => {
            // Material: blit its decoded image thumbnail when
            // available, fall back to a procedural pattern.
            if let Some(id) = thumb {
                blit_thumb(painter, preview, id);
            } else {
                draw_texture_like_preview(painter, preview, resource);
            }
            if material.tint != [0x80, 0x80, 0x80] {
                let tint = Color32::from_rgba_unmultiplied(
                    material.tint[0].saturating_mul(2),
                    material.tint[1].saturating_mul(2),
                    material.tint[2].saturating_mul(2),
                    48,
                );
                painter.rect_filled(preview, 2.0, tint);
            }
        }
        ResourceData::Texture { .. } => {
            if let Some(id) = thumb {
                blit_thumb(painter, preview, id);
            } else {
                draw_texture_like_preview(painter, preview, resource);
            }
        }
        _ => {
            painter.rect_filled(preview, 2.0, resource_preview_color(resource));
        }
    }
    draw_palette_strip(painter, preview, palette_for_resource(resource));
}

/// Paint a registered egui texture into the preview rect, full-image
/// UV (no atlasing) and untinted. Used for real `.psxt`-decoded
/// thumbnails so the resource card mirrors the actual texels the
/// runtime would sample.
pub(crate) fn blit_thumb(painter: &egui::Painter, preview: Rect, id: egui::TextureId) {
    painter.image(
        id,
        preview,
        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

pub(crate) fn draw_texture_like_preview(
    painter: &egui::Painter,
    preview: Rect,
    resource: &Resource,
) {
    let name = resource.name.to_ascii_lowercase();
    if name.contains("brick") {
        draw_brick_preview(painter, preview, resource_preview_color(resource));
    } else if name.contains("floor") || name.contains("stone") {
        draw_stone_preview(painter, preview, resource_preview_color(resource));
    } else {
        draw_checker_preview(painter, preview, resource_preview_color(resource));
    }
}

pub(crate) fn draw_brick_preview(painter: &egui::Painter, preview: Rect, base: Color32) {
    painter.rect_filled(preview, 2.0, darken(base, 8));
    let row_h = 14.0;
    let rows = (preview.height() / row_h).ceil() as i32;
    for row in 0..rows {
        let top = preview.top() + row as f32 * row_h;
        let y = top.min(preview.bottom());
        painter.line_segment(
            [Pos2::new(preview.left(), y), Pos2::new(preview.right(), y)],
            Stroke::new(1.0, darken(base, 48)),
        );
        let offset = if row % 2 == 0 { 0.0 } else { 18.0 };
        let mut x = preview.left() + offset;
        while x < preview.right() {
            painter.line_segment(
                [
                    Pos2::new(x, top + 2.0),
                    Pos2::new(x, (top + row_h - 2.0).min(preview.bottom())),
                ],
                Stroke::new(1.0, darken(base, 45)),
            );
            x += 36.0;
        }
        let stripe = Rect::from_min_max(
            Pos2::new(preview.left(), top + 2.0),
            Pos2::new(preview.right(), (top + 5.0).min(preview.bottom())),
        );
        painter.rect_filled(
            stripe,
            0.0,
            Color32::from_rgba_unmultiplied(188, 110, 60, 35),
        );
    }
}

pub(crate) fn draw_stone_preview(painter: &egui::Painter, preview: Rect, base: Color32) {
    painter.rect_filled(preview, 2.0, darken(base, 18));
    let cols = 3;
    let rows = 3;
    let cell = Vec2::new(
        preview.width() / cols as f32,
        preview.height() / rows as f32,
    );
    for y in 0..rows {
        for x in 0..cols {
            let min = preview.min + Vec2::new(x as f32 * cell.x, y as f32 * cell.y);
            let rect = Rect::from_min_size(min, cell).shrink(1.0);
            let shade = if (x + y) % 2 == 0 {
                lighten(base, 12)
            } else {
                darken(base, 6)
            };
            painter.rect_filled(rect, 1.0, shade);
            painter.rect_stroke(
                rect,
                1.0,
                Stroke::new(1.0, darken(base, 38)),
                StrokeKind::Inside,
            );
        }
    }
}

pub(crate) fn draw_checker_preview(painter: &egui::Painter, preview: Rect, base: Color32) {
    let alt = Color32::from_rgb(
        base.r().saturating_add(28),
        base.g().saturating_add(24),
        base.b().saturating_add(20),
    );
    let cell = 16.0;
    let cols = (preview.width() / cell).ceil() as i32;
    let rows = (preview.height() / cell).ceil() as i32;
    for y in 0..rows {
        for x in 0..cols {
            let min = preview.min + Vec2::new(x as f32 * cell, y as f32 * cell);
            let rect = Rect::from_min_size(min, Vec2::splat(cell)).intersect(preview);
            painter.rect_filled(rect, 0.0, if (x + y) % 2 == 0 { base } else { alt });
        }
    }
}

/// Decode the bytes of a cooked `.psxt` blob into an egui
/// [`ColorImage`] suitable for [`Context::load_texture`], plus the
/// declared `(width, height)` in texels so the cache can pick a
/// reasonable sample rate.
///
/// Supports 4bpp + 8bpp indexed and 15bpp direct. The CLUT/direct
/// colour STP bit (bit 15, set by the runtime so semi-transparent
/// draws can mask fully transparent black) is masked out before
/// producing display RGB.
pub(crate) fn decode_psxt_thumbnail(bytes: &[u8]) -> Option<(ColorImage, PsxtStats)> {
    let texture = psx_asset::Texture::from_bytes(bytes).ok()?;
    let width = texture.width() as usize;
    let height = texture.height() as usize;
    let clut_entries = texture.clut_entries() as usize;
    let depth_bits = match clut_entries {
        16 => 4,
        256 => 8,
        0 => 15,
        _ => return None,
    };
    if width == 0 || height == 0 {
        return None;
    }
    let pixel_count = width.checked_mul(height)?;
    let clut_bytes = texture.clut_bytes();
    if clut_entries > 0 && clut_bytes.len() < clut_entries * 2 {
        return None;
    }
    let stats = PsxtStats {
        width: texture.width(),
        height: texture.height(),
        depth_bits,
        clut_entries: clut_entries as u16,
        index_zero_transparent: texture.index_zero_transparent(),
        pixel_bytes: texture.pixel_bytes().len() as u32,
        clut_bytes: clut_bytes.len() as u32,
        file_bytes: bytes.len() as u32,
    };
    let palette: Vec<Color32> = (0..clut_entries)
        .map(|i| {
            let raw_full = u16::from_le_bytes([clut_bytes[i * 2], clut_bytes[i * 2 + 1]]);
            if i == 0 && texture.index_zero_transparent() && raw_full == 0 {
                return Color32::TRANSPARENT;
            }
            let raw = raw_full & 0x7FFF;
            let r5 = (raw & 0x1F) as u8;
            let g5 = ((raw >> 5) & 0x1F) as u8;
            let b5 = ((raw >> 10) & 0x1F) as u8;
            Color32::from_rgb(
                (r5 << 3) | (r5 >> 2),
                (g5 << 3) | (g5 >> 2),
                (b5 << 3) | (b5 >> 2),
            )
        })
        .collect();

    let pixel_bytes = texture.pixel_bytes();
    let mut pixels = Vec::with_capacity(pixel_count);
    if clut_entries == 0 {
        for i in 0..pixel_count {
            let off = i * 2;
            if off + 1 >= pixel_bytes.len() {
                return None;
            }
            let raw = u16::from_le_bytes([pixel_bytes[off], pixel_bytes[off + 1]]) & 0x7FFF;
            let r5 = (raw & 0x1F) as u8;
            let g5 = ((raw >> 5) & 0x1F) as u8;
            let b5 = ((raw >> 10) & 0x1F) as u8;
            pixels.push(Color32::from_rgb(
                (r5 << 3) | (r5 >> 2),
                (g5 << 3) | (g5 >> 2),
                (b5 << 3) | (b5 >> 2),
            ));
        }
    } else if clut_entries == 16 {
        // 4bpp: 4 texels per halfword, low nibble first.
        let halfwords_per_row = width.div_ceil(4);
        for row in 0..height {
            for hw in 0..halfwords_per_row {
                let off = (row * halfwords_per_row + hw) * 2;
                if off + 1 >= pixel_bytes.len() {
                    break;
                }
                let word = u16::from_le_bytes([pixel_bytes[off], pixel_bytes[off + 1]]);
                for nibble in 0..4 {
                    let texel = (word >> (nibble * 4)) & 0xF;
                    if hw * 4 + nibble < width {
                        pixels.push(palette[texel as usize]);
                    }
                }
            }
        }
    } else {
        // 8bpp: 2 texels per halfword, low byte first.
        let halfwords_per_row = width.div_ceil(2);
        for row in 0..height {
            for hw in 0..halfwords_per_row {
                let off = (row * halfwords_per_row + hw) * 2;
                if off + 1 >= pixel_bytes.len() {
                    break;
                }
                let lo = pixel_bytes[off] as usize;
                let hi = pixel_bytes[off + 1] as usize;
                if hw * 2 < width {
                    pixels.push(palette[lo]);
                }
                if hw * 2 + 1 < width {
                    pixels.push(palette[hi]);
                }
            }
        }
    }
    if pixels.len() != pixel_count {
        return None;
    }
    Some((
        ColorImage {
            size: [width, height],
            pixels,
        },
        stats,
    ))
}

pub(crate) fn draw_palette_strip(painter: &egui::Painter, preview: Rect, swatches: [Color32; 5]) {
    let width = preview.width() / swatches.len() as f32;
    for (idx, color) in swatches.iter().enumerate() {
        let min = Pos2::new(preview.left() + idx as f32 * width, preview.bottom() - 10.0);
        let rect = Rect::from_min_size(min, Vec2::new(width, 10.0));
        painter.rect_filled(rect, 0.0, *color);
    }
}

pub(crate) fn palette_for_resource(resource: &Resource) -> [Color32; 5] {
    let base = resource_preview_color(resource);
    [
        Color32::from_rgb(28, 30, 34),
        darken(base, 70),
        darken(base, 35),
        base,
        lighten(base, 44),
    ]
}

pub(crate) fn darken(color: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        color.r().saturating_sub(amount),
        color.g().saturating_sub(amount),
        color.b().saturating_sub(amount),
    )
}

pub(crate) fn lighten(color: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        color.r().saturating_add(amount),
        color.g().saturating_add(amount),
        color.b().saturating_add(amount),
    )
}

pub(crate) fn resource_preview_color(resource: &Resource) -> Color32 {
    let name = resource.name.to_ascii_lowercase();
    if name.contains("brick") {
        Color32::from_rgb(130, 70, 42)
    } else if name.contains("floor") {
        Color32::from_rgb(106, 112, 120)
    } else if name.contains("glass") {
        Color32::from_rgb(80, 150, 165)
    } else {
        match &resource.data {
            ResourceData::Texture { .. } => Color32::from_rgb(92, 116, 140),
            ResourceData::Material(_) => Color32::from_rgb(120, 92, 135),
            ResourceData::Model(_) => Color32::from_rgb(140, 124, 96),
            ResourceData::Skeleton(_) => Color32::from_rgb(82, 112, 145),
            ResourceData::AnimationSource(_) => Color32::from_rgb(104, 82, 145),
            ResourceData::AnimationClip(_) => Color32::from_rgb(76, 108, 170),
            ResourceData::AnimationSet(_) => Color32::from_rgb(82, 136, 100),
            ResourceData::Character(_) => Color32::from_rgb(96, 144, 110),
            ResourceData::Weapon(_) => Color32::from_rgb(150, 132, 76),
            ResourceData::Mesh { .. } => Color32::from_rgb(110, 120, 130),
            ResourceData::Scene { .. } => Color32::from_rgb(92, 130, 106),
            ResourceData::Script { .. } => Color32::from_rgb(128, 126, 80),
            ResourceData::Audio { .. } => Color32::from_rgb(80, 128, 128),
        }
    }
}

pub(crate) fn resource_detail(resource: &Resource) -> &'static str {
    match &resource.data {
        ResourceData::Texture { .. } => "Texture - 4bpp",
        ResourceData::Material(material)
            if material.texture_mode == MaterialTextureMode::Transition =>
        {
            "Transition Material - 4bpp"
        }
        ResourceData::Material(_) => "Material - 4bpp",
        ResourceData::Model(_) => "Model",
        ResourceData::Skeleton(_) => "Skeleton",
        ResourceData::AnimationSource(_) => "Animation Source",
        ResourceData::AnimationClip(_) => "Animation Clip",
        ResourceData::AnimationSet(_) => "Clip Role Map",
        ResourceData::Character(_) => "Character Profile",
        ResourceData::Weapon(_) => "Weapon",
        ResourceData::Mesh { .. } => "Mesh",
        ResourceData::Scene { .. } => "Room",
        ResourceData::Script { .. } => "Script",
        ResourceData::Audio { .. } => "Audio",
    }
}
