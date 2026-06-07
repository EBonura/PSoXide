use super::*;

#[test]
fn ui_font_atlas_has_expected_dimensions() {
    let atlas = rasterize_ui_font_atlas(UiFontChoice::Basic);
    let basic = ui_preview_font_spec(UiFontChoice::Basic);
    let basic_rows = basic.glyph_count.div_ceil(basic.cols);
    assert_eq!(
        atlas.size,
        [basic.cols * basic.glyph_w, basic_rows * basic.glyph_h]
    );
    // A real font has many lit pixels; an all-transparent atlas would mean
    // the rasterizer read the source wrong.
    let opaque = atlas.pixels.iter().filter(|p| p.a() == 255).count();
    assert!(opaque > 500, "atlas looks empty ({opaque} opaque px)");

    let tall = rasterize_ui_font_atlas(UiFontChoice::Basic8x16);
    let tall_spec = ui_preview_font_spec(UiFontChoice::Basic8x16);
    let tall_rows = tall_spec.glyph_count.div_ceil(tall_spec.cols);
    assert_eq!(
        tall.size,
        [
            tall_spec.cols * tall_spec.glyph_w,
            tall_rows * tall_spec.glyph_h
        ]
    );
    let tall_opaque = tall.pixels.iter().filter(|p| p.a() == 255).count();
    assert!(
        tall_opaque > opaque,
        "8x16 atlas should carry more lit rows than 8x8",
    );

    let orbitron = rasterize_ui_font_atlas(UiFontChoice::Orbitron);
    let orbitron_spec = ui_preview_font_spec(UiFontChoice::Orbitron);
    let orbitron_rows = orbitron_spec.glyph_count.div_ceil(orbitron_spec.cols);
    assert_eq!(
        orbitron.size,
        [
            orbitron_spec.cols * orbitron_spec.glyph_w,
            orbitron_rows * orbitron_spec.glyph_h
        ]
    );
    assert!(orbitron_spec.glyph_w > basic.glyph_w);
    let orbitron_opaque = orbitron.pixels.iter().filter(|p| p.a() == 255).count();
    assert!(
        orbitron_opaque > 500,
        "imported TTF atlas looks empty ({orbitron_opaque} opaque px)",
    );
}

#[test]
fn ui_font_atlas_pixels_match_the_source_font_bits() {
    use psx_font::fonts::BASIC;
    let atlas = rasterize_ui_font_atlas(UiFontChoice::Basic);
    let spec = ui_preview_font_spec(UiFontChoice::Basic);
    let aw = spec.cols * spec.glyph_w;
    // Check every glyph cell against the source bitmap: an atlas pixel is
    // opaque exactly when the corresponding source bit is set. This proves
    // the rasterizer (grid placement + bit-order via glyph_row_packed) is
    // faithful, not just non-empty.
    for glyph in 0..BASIC.glyph_count {
        let gx = (glyph as usize % spec.cols) * spec.glyph_w;
        let gy = (glyph as usize / spec.cols) * spec.glyph_h;
        for row in 0..spec.glyph_h {
            let bits = BASIC.glyph_row_packed(glyph, row as u8);
            for col in 0..spec.glyph_w {
                let lit = bits & (1 << col) != 0;
                let px = atlas.pixels[(gy + row) * aw + (gx + col)];
                assert_eq!(
                    px.a() == 255,
                    lit,
                    "glyph {glyph} row {row} col {col} mismatch",
                );
            }
        }
    }
}

#[test]
fn ui_font_glyph_uv_is_in_unit_range_and_advances_by_one_cell() {
    let a = ui_font_glyph_uv(UiFontChoice::Basic, b'A');
    assert!(a.min.x >= 0.0 && a.max.x <= 1.0 && a.min.y >= 0.0 && a.max.y <= 1.0);
    // Adjacent codes on the same row differ by exactly one column step.
    let b = ui_font_glyph_uv(UiFontChoice::Basic, b'B');
    let step = 1.0 / UI_FONT_COLS as f32;
    assert!((b.min.x - a.min.x - step).abs() < 1e-6);
    // Out-of-range codes clamp into the atlas (no panic, stays in 0..1).
    let oob = ui_font_glyph_uv(UiFontChoice::Basic8x16, 255);
    assert!(oob.max.x <= 1.0 && oob.max.y <= 1.0);
}

#[test]
fn ui_preview_text_width_applies_letter_spacing_between_glyphs() {
    assert_eq!(
        ui_preview_text_width(UiFontChoice::Basic, "ABC", 1.0, 2, 1.0),
        28.0
    );
    assert_eq!(
        ui_preview_text_width(UiFontChoice::Basic, "ABC", 2.0, -1, 1.0),
        46.0
    );
    assert_eq!(
        ui_preview_text_width(UiFontChoice::Basic, "A", 1.0, 9, 1.0),
        8.0
    );
}

#[test]
fn ui_preview_image_effect_colors_animate_and_keep_split_edge_continuous() {
    let left = UiRect::new(0, 0, 160, 100);
    let right = UiRect::new(160, 0, 160, 100);

    assert_eq!(
        ui_preview_image_effect_overlay_colors(UiImageEffect::None, 0, left),
        [Color32::TRANSPARENT; 4]
    );
    assert_ne!(
        ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 0, left),
        ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 64, left)
    );

    let left_colors = ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 48, left);
    let right_colors = ui_preview_image_effect_overlay_colors(UiImageEffect::Shimmer, 48, right);
    assert_eq!(left_colors[1], right_colors[0]);
    assert_eq!(left_colors[3], right_colors[2]);

    let pulse = ui_preview_image_effect_overlay_colors(UiImageEffect::SoftPulse, 12, left);
    assert_eq!(pulse[0], pulse[3]);
}

#[test]
fn preview_wrap_hard_split_counts_spacing_between_included_glyphs_only() {
    assert_eq!(
        preview_wrap_hard_split("ABC", UiFontChoice::Basic, 18.0, 1.0, 2, 1.0),
        2
    );
}

/// The shared multi-select math (`apply_range_modifiers`) backs both scene
/// node and resource selection. Exercise it directly over plain ids so the
/// branching is covered without a project, a workspace, or egui.
#[test]
fn range_modifiers_cover_replace_toggle_and_shift() {
    let order = [10u64, 20, 30, 40, 50];
    let mut set = HashSet::new();
    let mut anchor = None;

    // Plain click replaces the selection and sets the anchor.
    let primary = apply_range_modifiers(&mut set, &mut anchor, 30, false, false, &order, 0);
    assert_eq!(set, HashSet::from([30]));
    assert_eq!(anchor, Some(30));
    assert_eq!(primary, Some(30));

    // Toggle adds without clearing.
    let primary = apply_range_modifiers(&mut set, &mut anchor, 10, false, true, &order, 0);
    assert_eq!(set, HashSet::from([10, 30]));
    assert_eq!(anchor, Some(10));
    assert_eq!(primary, Some(10));

    // Toggling a selected id removes it; the primary falls back to the
    // first still-selected id in order.
    let primary = apply_range_modifiers(&mut set, &mut anchor, 30, false, true, &order, 0);
    assert_eq!(set, HashSet::from([10]));
    assert_eq!(primary, Some(10));

    // Shift without toggle clears, then selects the inclusive range from
    // the existing anchor; the anchor is preserved.
    anchor = Some(20);
    let primary = apply_range_modifiers(&mut set, &mut anchor, 50, true, false, &order, 0);
    assert_eq!(set, HashSet::from([20, 30, 40, 50]));
    assert_eq!(anchor, Some(20));
    assert_eq!(primary, Some(50));

    // Shift with toggle keeps the prior selection and unions the range.
    let mut set = HashSet::from([10u64]);
    let mut anchor = Some(20);
    apply_range_modifiers(&mut set, &mut anchor, 40, true, true, &order, 0);
    assert_eq!(set, HashSet::from([10, 20, 30, 40]));

    // With no anchor yet, the fallback anchors the range.
    let mut set = HashSet::new();
    let mut anchor = None;
    apply_range_modifiers(&mut set, &mut anchor, 30, true, false, &order, 10);
    assert_eq!(set, HashSet::from([10, 20, 30]));
    assert_eq!(anchor, Some(10));
}
