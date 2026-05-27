//! Debug/status overlays for editor-playtest.

use super::*;
use psx_gpu::draw_quad_flat;
use psx_level::{ui_node_flags, LevelUiNodeKind, LevelUiNodeRecord, LevelUiValueBinding};

const PLAYER_HEALTH_MAX_Q12: i32 = 4096;
const HUD_X: i16 = 18;
const HUD_Y: i16 = 16;
const HEALTH_BAR_W: i16 = 120;
const HEALTH_BAR_H: i16 = 8;
const STAMINA_BAR_W: i16 = 96;
const STAMINA_BAR_H: i16 = 5;
const HUD_BAR_GAP: i16 = 5;

pub(crate) fn draw_player_hud(
    nodes: &[LevelUiNodeRecord],
    font: Option<&FontAtlas>,
    stamina_q12: i32,
    stamina_max_q12: i32,
) {
    if nodes.is_empty() {
        draw_legacy_player_hud(stamina_q12, stamina_max_q12);
        return;
    }

    for (index, node) in nodes.iter().enumerate() {
        let (x, y, width, height) = ui_node_absolute_rect(nodes, index);
        match node.kind {
            LevelUiNodeKind::Canvas | LevelUiNodeKind::Group => {}
            LevelUiNodeKind::Rect => {
                draw_rect(x, y, width as i16, height as i16, rgb(node.color));
            }
            LevelUiNodeKind::Label => {
                if let Some(font) = font {
                    draw_ui_label(font, node, x, y, width);
                }
            }
            LevelUiNodeKind::Image => {
                draw_ui_image(node, x, y, width, height);
            }
            LevelUiNodeKind::Bar => {
                let max_q12 = ui_binding_value(node.max, stamina_q12, stamina_max_q12).max(1);
                let value_q12 =
                    ui_binding_value(node.value, stamina_q12, stamina_max_q12).clamp(0, max_q12);
                draw_status_bar(
                    x,
                    y,
                    width as i16,
                    height as i16,
                    value_q12,
                    max_q12,
                    rgb(node.color),
                    rgb(node.background),
                );
            }
        }
    }
}

fn ui_node_absolute_rect(nodes: &[LevelUiNodeRecord], index: usize) -> (i16, i16, u16, u16) {
    ui_node_absolute_rect_inner(nodes, index, 0).unwrap_or((0, 0, 1, 1))
}

fn ui_node_absolute_rect_inner(
    nodes: &[LevelUiNodeRecord],
    index: usize,
    depth: usize,
) -> Option<(i16, i16, u16, u16)> {
    if depth > nodes.len() {
        return None;
    }
    let node = nodes.get(index)?;
    if matches!(node.kind, LevelUiNodeKind::Canvas) {
        return Some((0, 0, node.width.max(1), node.height.max(1)));
    }
    let (parent_x, parent_y, parent_w, parent_h) = node
        .parent
        .and_then(|parent| ui_node_absolute_rect_inner(nodes, parent.to_usize(), depth + 1))
        .unwrap_or((0, 0, SCREEN_W as u16, SCREEN_H as u16));
    let (anchor_x, anchor_y) = ui_anchor_factors(node.flags);
    let x = parent_x as i32 + (parent_w as i32 * anchor_x) / 2 + node.x as i32;
    let y = parent_y as i32 + (parent_h as i32 * anchor_y) / 2 + node.y as i32;
    (
        x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        node.width.max(1),
        node.height.max(1),
    )
}

fn ui_anchor_factors(flags: u16) -> (i32, i32) {
    match flags & ui_node_flags::ANCHOR_MASK {
        1 => (1, 0),
        2 => (2, 0),
        3 => (0, 1),
        4 => (1, 1),
        5 => (2, 1),
        6 => (0, 2),
        7 => (1, 2),
        8 => (2, 2),
        _ => (0, 0),
    }
}

fn draw_ui_label(font: &FontAtlas, node: &LevelUiNodeRecord, x: i16, y: i16, width: u16) {
    let tint = rgb(node.color);
    let align = (node.flags & ui_node_flags::TEXT_ALIGN_MASK) >> ui_node_flags::TEXT_ALIGN_SHIFT;
    if node.flags & ui_node_flags::TEXT_WRAP == 0 {
        let text_x = aligned_text_x(font, node.text, x, width, align);
        font.draw_text(text_x, y, node.text, tint);
        return;
    }

    let mut start = 0usize;
    let mut line_y = y;
    while start < node.text.len() {
        while matches!(node.text.as_bytes().get(start), Some(b' ' | b'\n')) {
            start += 1;
        }
        if start >= node.text.len() {
            break;
        }
        let end = wrapped_line_end(font, node.text, start, width);
        let line = &node.text[start..end];
        let text_x = aligned_text_x(font, line, x, width, align);
        font.draw_text(text_x, line_y, line, tint);
        line_y = line_y.saturating_add(font.line_height() as i16);
        start = end;
    }
}

fn wrapped_line_end(font: &FontAtlas, text: &str, start: usize, width: u16) -> usize {
    let bytes = text.as_bytes();
    let mut end = start;
    let mut last_space = None;
    while end < bytes.len() {
        if bytes[end] == b'\n' {
            return end;
        }
        let next = end + 1;
        if bytes[end] == b' ' {
            last_space = Some(end);
        }
        if next > start && font.text_width(&text[start..next]) > width {
            return last_space.filter(|space| *space > start).unwrap_or(end.max(start + 1));
        }
        end = next;
    }
    end
}

fn aligned_text_x(font: &FontAtlas, text: &str, x: i16, width: u16, align: u16) -> i16 {
    let text_w = font.text_width(text) as i32;
    let base = x as i32;
    let available = width as i32;
    let offset = match align {
        1 => (available - text_w) / 2,
        2 => available - text_w,
        _ => 0,
    };
    (base + offset.max(0)).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn draw_ui_image(node: &LevelUiNodeRecord, x: i16, y: i16, width: u16, height: u16) {
    if node.texture_asset.0 == u16::MAX {
        draw_rect(x, y, width as i16, height as i16, rgb(node.color));
        return;
    }
    let Some(asset) = find_asset_of_kind(ASSETS, node.texture_asset, AssetKind::Texture) else {
        return;
    };
    let Some(slot) = ensure_texture_uploaded(asset.id, asset.bytes) else {
        return;
    };
    let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, rgb(node.color))
        .with_texture_window(slot.texture_window);
    let tex_w = vram_slot_texture_size_u8(slot.texture_width).saturating_sub(1);
    let tex_h = vram_slot_texture_size_u8(slot.texture_height).saturating_sub(1);
    draw_quad_textured_material(
        [
            (x, y),
            (x.saturating_add(width as i16), y),
            (x, y.saturating_add(height as i16)),
            (
                x.saturating_add(width as i16),
                y.saturating_add(height as i16),
            ),
        ],
        [(0, 0), (tex_w, 0), (0, tex_h), (tex_w, tex_h)],
        material,
    );
}

fn draw_legacy_player_hud(stamina_q12: i32, stamina_max_q12: i32) {
    draw_status_bar(
        HUD_X,
        HUD_Y,
        HEALTH_BAR_W,
        HEALTH_BAR_H,
        PLAYER_HEALTH_MAX_Q12,
        PLAYER_HEALTH_MAX_Q12,
        (94, 16, 24),
        (30, 26, 28),
    );
    draw_status_bar(
        HUD_X,
        HUD_Y + HEALTH_BAR_H + HUD_BAR_GAP,
        STAMINA_BAR_W,
        STAMINA_BAR_H,
        stamina_q12,
        stamina_max_q12,
        (44, 98, 48),
        (30, 26, 28),
    );
}

fn ui_binding_value(binding: LevelUiValueBinding, stamina_q12: i32, stamina_max_q12: i32) -> i32 {
    match binding {
        LevelUiValueBinding::ConstantQ12(value) => value,
        LevelUiValueBinding::PlayerHealth => PLAYER_HEALTH_MAX_Q12,
        LevelUiValueBinding::PlayerHealthMax => PLAYER_HEALTH_MAX_Q12,
        LevelUiValueBinding::PlayerStamina => stamina_q12,
        LevelUiValueBinding::PlayerStaminaMax => stamina_max_q12,
    }
}

fn rgb(color: [u8; 3]) -> (u8, u8, u8) {
    (color[0], color[1], color[2])
}

pub(crate) fn draw_analog_required_prompt(font: &FontAtlas) {
    const BOX_X0: i16 = 32;
    const BOX_Y0: i16 = (SCREEN_H - 64) / 2;
    const BOX_X1: i16 = 288;
    const BOX_Y1: i16 = BOX_Y0 + 64;
    draw_quad_flat(
        [
            (BOX_X0, BOX_Y0),
            (BOX_X1, BOX_Y0),
            (BOX_X0, BOX_Y1),
            (BOX_X1, BOX_Y1),
        ],
        18,
        20,
        28,
    );
    draw_quad_flat(
        [
            (BOX_X0 - 2, BOX_Y0 - 2),
            (BOX_X1 + 2, BOX_Y0 - 2),
            (BOX_X0 - 2, BOX_Y0),
            (BOX_X1 + 2, BOX_Y0),
        ],
        120,
        130,
        160,
    );
    draw_centered_text(font, 104, "ANALOG MODE REQUIRED", (245, 245, 255));
    draw_centered_text(font, 121, "TURN ON ANALOG MODE", (200, 220, 245));
    draw_centered_text(font, 134, "TO START PLAYTEST", (200, 220, 245));
}

pub(crate) fn draw_centered_text(font: &FontAtlas, y: i16, text: &str, tint: (u8, u8, u8)) {
    let width = font.text_width(text) as i16;
    let x = (SCREEN_W - width) / 2;
    font.draw_text(x, y, text, tint);
}

fn draw_status_bar(
    x: i16,
    y: i16,
    width: i16,
    height: i16,
    value: i32,
    max_value: i32,
    fill: (u8, u8, u8),
    background: (u8, u8, u8),
) {
    draw_rect(x - 1, y - 1, width + 2, height + 2, (12, 14, 18));
    draw_rect(x, y, width, height, background);

    let fill_width = status_fill_width(width, value, max_value);
    if fill_width > 0 {
        draw_rect(x, y, fill_width, height, fill);
        if height > 3 {
            draw_rect(x, y, fill_width, 1, brighten(fill));
        }
    }
}

fn draw_rect(x: i16, y: i16, width: i16, height: i16, color: (u8, u8, u8)) {
    if width <= 0 || height <= 0 {
        return;
    }
    draw_quad_flat(
        [
            (x, y),
            (x + width, y),
            (x, y + height),
            (x + width, y + height),
        ],
        color.0,
        color.1,
        color.2,
    );
}

fn status_fill_width(width: i16, value: i32, max_value: i32) -> i16 {
    if width <= 0 || max_value <= 0 {
        return 0;
    }
    let clamped = value.clamp(0, max_value);
    ((width as i32).saturating_mul(clamped) / max_value) as i16
}

fn brighten(color: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        color.0.saturating_add(34),
        color.1.saturating_add(34),
        color.2.saturating_add(34),
    )
}
