//! Debug/status overlays for editor-playtest.
//!
//! Cooked UI scenes are rendered by the engine's
//! [`psx_engine::ui::draw_scene`]; this module only supplies the two
//! project-specific resolvers (texture upload lookup, gameplay value
//! bindings) plus the legacy fallback HUD and a couple of one-off
//! prompts the playtest still draws directly.

use super::*;
use psx_engine::ui::{self, UiTextureSlot};
use psx_gpu::draw_quad_flat;
use psx_level::{LevelUiNodeRecord, LevelUiValueBinding};

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
    hud_first: usize,
    hud_count: usize,
    fonts: &[Option<&FontAtlas>],
    frame: u16,
    health: u16,
    health_max: u16,
    stamina_q12: i32,
    stamina_max_q12: i32,
) {
    // Real player HP (the combat slice) scaled onto the HUD's Q12
    // bar convention; a zero max (pre-init) reads as a full bar so
    // hand-rolled manifests keep their old look.
    let health_q12 = if health_max == 0 {
        PLAYER_HEALTH_MAX_Q12
    } else {
        (i32::from(health) * PLAYER_HEALTH_MAX_Q12) / i32::from(health_max)
    };
    if nodes.is_empty() || hud_count == 0 {
        draw_legacy_player_hud(health_q12, stamina_q12, stamina_max_q12);
        return;
    }

    // Image nodes name an AssetId; resolve it through the example's
    // asset table + VRAM residency manager into the words the engine
    // renderer needs. Skips the image when the texture is missing or
    // not resident this frame.
    let mut resolve_texture = |asset_id: AssetId| -> Option<UiTextureSlot> {
        let asset = find_asset_of_kind(ASSETS, asset_id, AssetKind::Texture)?;
        let slot = ensure_texture_uploaded(asset.id, asset.bytes)?;
        Some(UiTextureSlot {
            clut_word: slot.clut_word,
            tpage_word: slot.tpage_word,
            texture_window: slot.texture_window,
            texture_width: slot.texture_width,
            texture_height: slot.texture_height,
        })
    };

    // Bar nodes name a value binding; resolve gameplay fields here so
    // the engine stays free of stamina/health knowledge.
    let resolve_value = |binding: LevelUiValueBinding| -> i32 {
        match binding {
            LevelUiValueBinding::ConstantQ12(value) => value,
            LevelUiValueBinding::Option(_) => 0,
            LevelUiValueBinding::PlayerHealth => health_q12,
            LevelUiValueBinding::PlayerHealthMax => PLAYER_HEALTH_MAX_Q12,
            LevelUiValueBinding::PlayerStamina => stamina_q12,
            LevelUiValueBinding::PlayerStaminaMax => stamina_max_q12,
            // Loading progress is a loading-screen binding; it has no
            // meaning on the in-game HUD.
            LevelUiValueBinding::LoadingProgress => 0,
        }
    };

    // HUD overlay: draw only the HUD scene's node slice (the shared pool
    // also holds front-end menu scenes now), no menu focus so no control is
    // highlighted. The HUD carries no sliders, so the option table is empty
    // and the value resolver always reports zero.
    ui::draw_scene(
        nodes,
        hud_first,
        hud_count,
        &[],
        fonts,
        None,
        &psx_level::LevelUiFocusStyle::DEFAULT,
        frame,
        0,
        &mut resolve_texture,
        &resolve_value,
        &[],
        &|_option_id| 0,
        &|_| true,
    );
}

fn draw_legacy_player_hud(health_q12: i32, stamina_q12: i32, stamina_max_q12: i32) {
    draw_status_bar(
        HUD_X,
        HUD_Y,
        HEALTH_BAR_W,
        HEALTH_BAR_H,
        health_q12,
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

pub(crate) fn draw_interaction_prompt(font: &FontAtlas, prompt: &str) {
    const PAD_X: i16 = 10;
    const BOX_Y: i16 = SCREEN_H - 34;
    const BOX_H: i16 = 20;
    let label_width = font.text_width(prompt) as i16;
    let box_w = (label_width + PAD_X * 2).clamp(72, SCREEN_W - 32);
    let box_x = (SCREEN_W - box_w) / 2;
    draw_rect(box_x, BOX_Y, box_w, BOX_H, (18, 24, 34));
    draw_rect(box_x, BOX_Y, box_w, 1, (96, 154, 134));
    font.draw_text(
        box_x + (box_w - label_width) / 2,
        BOX_Y + 6,
        prompt,
        (205, 248, 226),
    );
}

pub(crate) fn draw_interactable_message(font: &FontAtlas, title: &str, body: &str) {
    const BOX_X: i16 = 26;
    const BOX_Y: i16 = 150;
    const BOX_W: i16 = SCREEN_W - BOX_X * 2;
    const BOX_H: i16 = 72;
    draw_quad_flat(
        [
            (BOX_X, BOX_Y),
            (BOX_X + BOX_W, BOX_Y),
            (BOX_X, BOX_Y + BOX_H),
            (BOX_X + BOX_W, BOX_Y + BOX_H),
        ],
        8,
        11,
        18,
    );
    draw_rect(BOX_X, BOX_Y, BOX_W, 1, (120, 180, 156));
    draw_rect(BOX_X, BOX_Y + BOX_H - 1, BOX_W, 1, (54, 84, 76));
    font.draw_text(BOX_X + 10, BOX_Y + 10, title, (222, 255, 238));

    let mut line_y = BOX_Y + 28;
    let mut lines = 0u8;
    for line in body.split('\n') {
        if lines >= 4 {
            break;
        }
        if !line.is_empty() {
            font.draw_text(BOX_X + 10, line_y, line, (190, 218, 206));
        }
        line_y += 10;
        lines += 1;
    }
    draw_centered_text(font, BOX_Y + BOX_H - 12, "CROSS / CIRCLE", (126, 154, 148));
}

pub(crate) fn draw_centered_text(font: &FontAtlas, y: i16, text: &str, tint: (u8, u8, u8)) {
    let width = font.text_width(text) as i16;
    let x = (SCREEN_W - width) / 2;
    font.draw_text(x, y, text, tint);
}

/// Hardware-ratification overlay (burn builds only): presented frames
/// per second and the worst inter-frame gap in vblanks over the last
/// second, top-right. The emulator does not model GPU draw time, so
/// silicon framerate is the only true measurement; this makes it
/// readable from a console photo. 30 fps steady shows "30 W2".
#[cfg(feature = "fps-overlay")]
pub(crate) fn draw_fps_overlay(font: &FontAtlas, fps: u8, worst_gap_vblanks: u8) {
    let mut buf = [0u8; 8];
    let mut len = 0usize;
    push_u8_decimal(&mut buf, &mut len, fps);
    buf[len] = b' ';
    len += 1;
    buf[len] = b'W';
    len += 1;
    push_u8_decimal(&mut buf, &mut len, worst_gap_vblanks);
    let Ok(text) = core::str::from_utf8(&buf[..len]) else {
        return;
    };
    let width = font.text_width(text) as i16;
    let x = SCREEN_W - 8 - width;
    draw_rect(x - 3, 6, width + 6, 12, (10, 12, 16));
    font.draw_text(x, 8, text, (170, 255, 190));
}

#[cfg(feature = "fps-overlay")]
fn push_u8_decimal(buf: &mut [u8; 8], len: &mut usize, value: u8) {
    if value >= 100 {
        buf[*len] = b'0' + value / 100;
        *len += 1;
    }
    if value >= 10 {
        buf[*len] = b'0' + (value / 10) % 10;
        *len += 1;
    }
    buf[*len] = b'0' + value % 10;
    *len += 1;
}

/// DIAGNOSTIC (vertex-explosion probe, not for release): the controlled
/// fixed-pose diff (IMG_6161-6165) showed the stretch is HORIZONTAL -- only
/// the projected X widens on hardware, while depth/Y/projection match the
/// emulator. So the view-space X is computed wider on hardware. This overlay
/// shows the view-space X EXTENT (min..max) per skinning stage to find which
/// step widens it:
///   AX = primary-joint transform view-space X
///   BX = secondary-joint transform view-space X (AFTER the GTE matrix reload)
///   LX = blended (lerp) view-space X
///   SX = projected screen X range (the symptom), O = verts off hw bounds
/// Whichever of AX/BX/LX widens on hardware vs the emulator is the culprit.
/// Reads + RESETS the per-frame accumulator.
#[cfg(feature = "vert-debug-overlay")]
pub(crate) fn draw_player_vert_debug(font: &FontAtlas) {
    use psx_engine::render3d::player_vert_debug as vdbg;
    let b = vdbg::get();

    // No blended verts observed this frame (model culled / not yet rendered):
    // the accumulator still holds its i32::MAX/MIN sentinels, which would
    // photograph as plausible-looking garbage. Say so explicitly instead.
    if b.total == 0 {
        font.draw_text(8, 44, "NO VERTS", (255, 90, 90));
        vdbg::reset();
        return;
    }

    // ALL-STAGES capture, auto-cycling pages (~3 s each at 30 Hz render).
    // The pose is static while photographing, so each page shows the same
    // worst vertex; one photo per page = the full chain for offline replay
    // through the console-exact GTE core.
    let page = unsafe {
        PAGE_TICKS = PAGE_TICKS.wrapping_add(1);
        (PAGE_TICKS / 90) % 4
    };
    // Header carries the latch score: identical SC on every photographed
    // page proves all pages show the same latched event.
    let mut hdr = DbgLine::new();
    hdr.s(b"P");
    hdr.i(page as i32 + 1);
    hdr.s(b"/4 SC ");
    hdr.i(vdbg::snapshot().score);
    font.draw_text(216, 44, hdr.text(), (255, 255, 255));

    match page {
        0 => draw_vdbg_extents(font, &b),
        1 => draw_vdbg_vertex_page(font),
        2 => draw_vdbg_matrices_page(font),
        _ => draw_vdbg_compose_inputs_page(font),
    }

    vdbg::reset();
}

#[cfg(feature = "vert-debug-overlay")]
static mut PAGE_TICKS: u32 = 0;

/// Page 1: the original per-stage view-space X extents (kept so the new
/// burn stays comparable with the IMG_6161-6172 rounds).
#[cfg(feature = "vert-debug-overlay")]
fn draw_vdbg_extents(font: &FontAtlas, b: &psx_engine::render3d::player_vert_debug::Bounds) {
    // The stretch is HORIZONTAL, so we show the view-space X EXTENT
    // (min..max) per skinning stage -- the stage whose X widens on hardware
    // vs the emulator is the bug. AX = primary joint, BX = secondary joint
    // after the GTE matrix reload, LX = blended (lerp). SX = projected.
    let mut l1 = DbgLine::new();
    l1.s(b"AX ");
    l1.i(b.ax_min);
    l1.s(b"..");
    l1.i(b.ax_max);
    font.draw_text(8, 44, l1.text(), (255, 232, 120));

    let mut l2 = DbgLine::new();
    l2.s(b"BX ");
    l2.i(b.bx_min);
    l2.s(b"..");
    l2.i(b.bx_max);
    font.draw_text(8, 56, l2.text(), (255, 200, 120));

    let mut l3 = DbgLine::new();
    l3.s(b"LX ");
    l3.i(b.lx_min);
    l3.s(b"..");
    l3.i(b.lx_max);
    font.draw_text(8, 68, l3.text(), (255, 170, 120));

    let mut l4 = DbgLine::new();
    l4.s(b"SX ");
    l4.i(b.scr_min_x as i32);
    l4.s(b"..");
    l4.i(b.scr_max_x as i32);
    l4.s(b" O ");
    l4.i(b.oob as i32);
    font.draw_text(8, 80, l4.text(), (160, 230, 255));

    // ALL-path projected X (blended + single-bone batch + remainder), from
    // the model pass accumulator. SX above covers only the blended verts;
    // PX wider than SX on hardware = the single-bone paths widen too.
    let mut l5 = DbgLine::new();
    l5.s(b"PX ");
    l5.i(b.px_min as i32);
    l5.s(b"..");
    l5.i(b.px_max as i32);
    font.draw_text(8, 92, l5.text(), (120, 255, 220));
}

/// Page 2: worst vertex identity + every stage OUTPUT in hex. Offline:
/// va =? MVMVA(rot0, tr0, pos), vb =? MVMVA(rot1, tr1, pos),
/// vl =? lerp(va, vb, blend), (sx, sy) =? RTPS(vl).
#[cfg(feature = "vert-debug-overlay")]
fn draw_vdbg_vertex_page(font: &FontAtlas) {
    use psx_engine::render3d::player_vert_debug as vdbg;
    let s = vdbg::snapshot();
    if !s.valid {
        font.draw_text(8, 44, "NO SNAP", (255, 90, 90));
        return;
    }
    let mut l = DbgLine::new();
    l.s(b"V ");
    l.x16(s.pos.x as u16);
    l.b(b' ');
    l.x16(s.pos.y as u16);
    l.b(b' ');
    l.x16(s.pos.z as u16);
    l.s(b" J");
    l.i(s.j0 as i32);
    l.b(b'/');
    l.i(s.j1 as i32);
    l.s(b" W");
    l.i(s.blend as i32);
    font.draw_text(8, 44, l.text(), (255, 255, 160));
    draw_hex3(font, 56, b"VA", &s.va, (255, 232, 120));
    draw_hex3(font, 68, b"VB", &s.vb, (255, 200, 120));
    draw_hex3(font, 80, b"VL", &s.vl, (255, 170, 120));
    let mut l5 = DbgLine::new();
    l5.s(b"SXY ");
    l5.x16(s.sx as u16);
    l5.b(b' ');
    l5.x16(s.sy as u16);
    l5.s(b" FLG ");
    l5.x32(s.flag);
    font.draw_text(8, 92, l5.text(), (160, 230, 255));
}

/// Page 3: the matrices exactly as CTC2'd for the worst vertex (primary on
/// the left column block, secondary below) + view-space translations.
#[cfg(feature = "vert-debug-overlay")]
fn draw_vdbg_matrices_page(font: &FontAtlas) {
    use psx_engine::render3d::player_vert_debug as vdbg;
    let s = vdbg::snapshot();
    if !s.valid {
        font.draw_text(8, 44, "NO SNAP", (255, 90, 90));
        return;
    }
    draw_mat3(font, 44, b"R0", &s.rot0.m, (255, 232, 120));
    draw_hex3(font, 80, b"T0", &s.tr0, (255, 232, 120));
    draw_mat3(font, 92, b"R1", &s.rot1.m, (255, 200, 120));
    draw_hex3(font, 128, b"T1", &s.tr1, (255, 200, 120));
}

/// Page 4: the GTE compose INPUTS for the worst vertex's two joints:
/// shared view*instance matrix, then each joint's model/pose matrix and
/// pose translation. Offline: rot0 =? compose(VI, M0), rot1 =? compose(VI, M1).
#[cfg(feature = "vert-debug-overlay")]
fn draw_vdbg_compose_inputs_page(font: &FontAtlas) {
    use psx_engine::render3d::player_vert_debug as vdbg;
    let s = vdbg::snapshot();
    if !s.valid {
        font.draw_text(8, 44, "NO SNAP", (255, 90, 90));
        return;
    }
    draw_mat3(font, 44, b"VI", &s.vi.m, (160, 230, 255));
    if s.m0.valid {
        draw_mat3(font, 80, b"M0", &s.m0.model.m, (255, 232, 120));
        let pt0 = [
            s.m0.ptrans.x as i32,
            s.m0.ptrans.y as i32,
            s.m0.ptrans.z as i32,
        ];
        draw_hex3(font, 116, b"P0", &pt0, (255, 232, 120));
    } else {
        font.draw_text(8, 80, "M0 MISSING", (255, 90, 90));
    }
    if s.m1.valid {
        draw_mat3(font, 128, b"M1", &s.m1.model.m, (255, 200, 120));
        let pt1 = [
            s.m1.ptrans.x as i32,
            s.m1.ptrans.y as i32,
            s.m1.ptrans.z as i32,
        ];
        draw_hex3(font, 164, b"P1", &pt1, (255, 200, 120));
    } else {
        font.draw_text(8, 128, "M1 MISSING", (255, 90, 90));
    }
}

/// One labelled row of three 32-bit hex values.
#[cfg(feature = "vert-debug-overlay")]
fn draw_hex3(font: &FontAtlas, y: i16, label: &[u8], v: &[i32; 3], color: (u8, u8, u8)) {
    let mut l = DbgLine::new();
    l.s(label);
    l.b(b' ');
    l.x32(v[0] as u32);
    l.b(b' ');
    l.x32(v[1] as u32);
    l.b(b' ');
    l.x32(v[2] as u32);
    font.draw_text(8, y, l.text(), color);
}

/// A 3x3 i16 matrix as three rows of three 16-bit hex values.
#[cfg(feature = "vert-debug-overlay")]
fn draw_mat3(font: &FontAtlas, y: i16, label: &[u8], m: &[[i16; 3]; 3], color: (u8, u8, u8)) {
    for (row, values) in m.iter().enumerate() {
        let mut l = DbgLine::new();
        l.s(label);
        l.i(row as i32);
        l.b(b' ');
        l.x16(values[0] as u16);
        l.b(b' ');
        l.x16(values[1] as u16);
        l.b(b' ');
        l.x16(values[2] as u16);
        font.draw_text(8, y + (row as i16) * 12, l.text(), color);
    }
}

/// Tiny stack-buffer line builder for the diagnostic overlay (no_std, no alloc).
#[cfg(feature = "vert-debug-overlay")]
struct DbgLine {
    buf: [u8; 48],
    len: usize,
}

#[cfg(feature = "vert-debug-overlay")]
impl DbgLine {
    fn new() -> Self {
        Self {
            buf: [0; 48],
            len: 0,
        }
    }
    fn b(&mut self, c: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = c;
            self.len += 1;
        }
    }
    fn s(&mut self, t: &[u8]) {
        for &c in t {
            self.b(c);
        }
    }
    fn i(&mut self, v: i32) {
        if v < 0 {
            self.b(b'-');
        }
        let mut n = v.unsigned_abs();
        let mut tmp = [0u8; 10];
        let mut t = 0;
        loop {
            tmp[t] = b'0' + (n % 10) as u8;
            t += 1;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        while t > 0 {
            t -= 1;
            self.b(tmp[t]);
        }
    }
    fn x16(&mut self, v: u16) {
        self.hex(v as u32, 4);
    }
    fn x32(&mut self, v: u32) {
        self.hex(v, 8);
    }
    fn hex(&mut self, v: u32, digits: u32) {
        let mut d = digits;
        while d > 0 {
            d -= 1;
            let nibble = ((v >> (d * 4)) & 0xF) as u8;
            self.b(if nibble < 10 {
                b'0' + nibble
            } else {
                b'A' + nibble - 10
            });
        }
    }
    fn text(&self) -> &str {
        // SAFETY: only ASCII digits/letters/punctuation pushed above.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
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
