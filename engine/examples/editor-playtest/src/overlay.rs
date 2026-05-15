//! Debug/status overlays for editor-playtest.

use super::*;
use psx_gpu::draw_quad_flat;

const PLAYER_HEALTH_MAX_Q12: i32 = 4096;
const HUD_X: i16 = 18;
const HUD_Y: i16 = 16;
const HEALTH_BAR_W: i16 = 120;
const HEALTH_BAR_H: i16 = 8;
const STAMINA_BAR_W: i16 = 96;
const STAMINA_BAR_H: i16 = 5;
const HUD_BAR_GAP: i16 = 5;

pub(crate) fn draw_player_hud(stamina_q12: i32, stamina_max_q12: i32) {
    draw_status_bar(
        HUD_X,
        HUD_Y,
        HEALTH_BAR_W,
        HEALTH_BAR_H,
        PLAYER_HEALTH_MAX_Q12,
        PLAYER_HEALTH_MAX_Q12,
        (94, 16, 24),
    );
    draw_status_bar(
        HUD_X,
        HUD_Y + HEALTH_BAR_H + HUD_BAR_GAP,
        STAMINA_BAR_W,
        STAMINA_BAR_H,
        stamina_q12,
        stamina_max_q12,
        (44, 98, 48),
    );
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
) {
    draw_rect(x - 1, y - 1, width + 2, height + 2, (12, 14, 18));
    draw_rect(x, y, width, height, (30, 26, 28));

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
