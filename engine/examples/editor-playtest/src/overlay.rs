//! Debug/status overlays for editor-playtest.
//!
//! Cooked UI scenes are rendered by the engine's scene-state UI pass. This
//! module keeps only diagnostic overlays the playtest still draws directly.

use super::*;
use psx_engine::ui::{
    draw_expanding_message_panel, draw_interaction_prompt_panel, draw_item_acquired_panel,
    draw_message_panel,
};
pub(crate) use psx_engine::ui::{MessagePageMeta, MessagePanelVariant};
use psx_gpu::draw_quad_flat;
use psx_gpu::draw_tri_textured_material;

/// Apply the six-step demo-disc brightness control as a native PS1 blend over
/// the composed frame. This costs two flat triangles and no VRAM. Level one
/// matches the old level three, level four is the authored image, and the top
/// two levels lift the image above neutral.
pub(crate) fn draw_brightness_overlay(level: u8) {
    let (amount, blend_mode) = match level.clamp(1, BRIGHTNESS_LEVELS) {
        1 => (24, BlendMode::Subtract),
        2 => (14, BlendMode::Subtract),
        3 => (6, BlendMode::Subtract),
        4 => (0, BlendMode::Opaque),
        5 => (6, BlendMode::Add),
        _ => (14, BlendMode::Add),
    };
    if amount == 0 {
        return;
    }
    draw_tri_flat_blended(
        [(0, 0), (SCREEN_W, 0), (0, SCREEN_H)],
        amount,
        amount,
        amount,
        blend_mode,
    );
    draw_tri_flat_blended(
        [(SCREEN_W, 0), (0, SCREEN_H), (SCREEN_W, SCREEN_H)],
        amount,
        amount,
        amount,
        blend_mode,
    );
}

pub(crate) fn draw_interaction_prompt(font: &FontAtlas, prompt: &str) {
    draw_interaction_prompt_animated(font, prompt, 0);
}

/// Animated proximity prompt. `prompt` is the action verb; the shared engine
/// chrome adds the `X -` control prefix.
pub(crate) fn draw_interaction_prompt_animated(font: &FontAtlas, prompt: &str, frame: u16) {
    draw_interaction_prompt_panel(font, prompt, frame);
}

pub(crate) fn draw_interactable_message(
    font: &FontAtlas,
    _title: &str,
    body: &str,
    typewriter_frame: u16,
) {
    draw_message_page(
        font,
        body,
        MessagePanelVariant::PointOfInterest,
        MessagePageMeta::new(0, 1),
        0,
        typewriter_frame,
    );
}

/// Shared POI/world-message bridge used by the playtest state machine.
pub(crate) fn draw_message_page(
    font: &FontAtlas,
    page_text: &str,
    variant: MessagePanelVariant,
    page: MessagePageMeta,
    frame: u16,
    typewriter_frame: u16,
) {
    draw_message_panel(font, page_text, variant, page, frame, typewriter_frame);
}

/// POI-only presentation bridge that visibly morphs the active interaction
/// prompt into the first message page.
pub(crate) fn draw_expanding_poi_message(
    font: &FontAtlas,
    action: &str,
    page_text: &str,
    page: MessagePageMeta,
    frame: u16,
    transition_frame: u16,
    typewriter_frame: u16,
) {
    draw_expanding_message_panel(
        font,
        action,
        page_text,
        page,
        frame,
        transition_frame,
        typewriter_frame,
    );
}

pub(crate) fn draw_acquired_module(
    font: &FontAtlas,
    item_name: &str,
    frame: u16,
    transition_frame: u16,
    typewriter_frame: u16,
) {
    draw_item_acquired_panel(font, item_name, frame, transition_frame, typewriter_frame);
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

#[inline(never)]
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

/// A rectangle with its top-left and bottom-right corners chamfered.
///
/// The authored stance shells carried `corner_cut: 2` on exactly those two
/// corners, and that slant is the HUD's signature. When the bars moved into
/// the overlay so a swap could animate, the cut did not come with them and
/// both bars went square.
///
/// The overlay's primitive is a four-point polygon and a two-corner chamfer is
/// a hexagon, so this draws three quads split at the cut columns: a sliver
/// with a sloped top edge, the square middle, and a sliver with a sloped
/// bottom edge. At the authored cut of two pixels the outer two are slivers,
/// so the whole shell costs two extra polygons.
#[inline(never)]
fn draw_chamfered(x: i16, y: i16, w: i16, h: i16, cut: i16, color: (u8, u8, u8)) {
    if w <= 0 || h <= 0 {
        return;
    }
    // A cut wider than half the box would cross the slopes over each other.
    let cut = cut.min(w / 2).min(h / 2).max(0);
    if cut == 0 {
        draw_rect(x, y, w, h, color);
        return;
    }
    let (r, g, b) = color;
    // Left sliver: top edge rises from (x, y + cut) to (x + cut, y).
    draw_quad_flat(
        [(x, y + cut), (x + cut, y), (x, y + h), (x + cut, y + h)],
        r,
        g,
        b,
    );
    draw_rect(x + cut, y, w - cut * 2, h, color);
    // Right sliver: bottom edge falls from (x + w - cut, y + h) to (x + w, y + h - cut).
    draw_quad_flat(
        [
            (x + w - cut, y),
            (x + w, y),
            (x + w - cut, y + h),
            (x + w, y + h - cut),
        ],
        r,
        g,
        b,
    );
}

/// Draw the complete player vitality cluster. Owning the two bars and cooldown
/// dial in one pass prevents the old animated bar from surviving behind the
/// authored shell, and lets the active channel stay deliberately longer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_player_vitality_hud(
    font: &FontAtlas,
    dial_material: Option<TextureMaterial>,
    active: VitalityChannelId,
    active_fill_q12: u16,
    inactive_fill_q12: u16,
    swap_motion_progress_q12: u16,
    swap_cooldown_progress_q12: u16,
    completion_echo_elapsed: Option<u16>,
) {
    const HORIZON_RGB: (u8, u8, u8) = (234, 82, 48);
    const ZENITH_RGB: (u8, u8, u8) = (76, 202, 181);
    let channel_style = |channel| {
        if channel == VitalityChannelId::One {
            (HORIZON_RGB, "HRZ")
        } else {
            (ZENITH_RGB, "ZTH")
        }
    };
    let (active_rgb, active_label) = channel_style(active);
    let (inactive_rgb, inactive_label) = channel_style(active.other());

    // The gameplay state changes channel as soon as Triangle is accepted, but
    // the HUD keeps the previous frame's composition at progress zero. The
    // newly active channel then rises and grows into the dominant slot while
    // the old channel drops and contracts. Their small opposing horizontal
    // arcs let the two ribbons visibly pass one another instead of occupying
    // the exact same pixels at the midpoint.
    let (incoming, outgoing) = vitality_swap_geometry(swap_motion_progress_q12);
    if swap_motion_progress_q12 < 2048 {
        // The old active bar is still on the near side of the orbit.
        draw_vitality_bar(
            font,
            incoming.bounds,
            incoming.cut,
            active_fill_q12,
            active_rgb,
            active_label,
        );
        draw_vitality_bar(
            font,
            outgoing.bounds,
            outgoing.cut,
            inactive_fill_q12,
            inactive_rgb,
            inactive_label,
        );
    } else {
        // After the crossing, the newly active bar comes to the near side.
        draw_vitality_bar(
            font,
            outgoing.bounds,
            outgoing.cut,
            inactive_fill_q12,
            inactive_rgb,
            inactive_label,
        );
        draw_vitality_bar(
            font,
            incoming.bounds,
            incoming.cut,
            active_fill_q12,
            active_rgb,
            active_label,
        );
    }
    let circle_rgb = lerp_rgb(inactive_rgb, active_rgb, swap_motion_progress_q12);
    draw_swap_cooldown_circle(
        20,
        18,
        dial_material,
        swap_cooldown_progress_q12,
        circle_rgb,
        completion_echo_elapsed,
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VitalityBarGeometry {
    bounds: (i16, i16, i16, i16),
    cut: i16,
}

const ACTIVE_VITALITY_BAR: VitalityBarGeometry = VitalityBarGeometry {
    bounds: (34, 5, 152, 13),
    cut: 8,
};
const INACTIVE_VITALITY_BAR: VitalityBarGeometry = VitalityBarGeometry {
    bounds: (34, 19, 104, 11),
    cut: 5,
};

/// Return the geometry of the new active bar and the previous active bar.
///
/// The endpoints exactly match the resting HUD. At progress zero this makes
/// the first swap frame indistinguishable from the preceding frame even
/// though gameplay has already changed channel; at one the new layout is
/// likewise identical to the normal resting composition.
#[inline(always)]
fn vitality_swap_geometry(progress_q12: u16) -> (VitalityBarGeometry, VitalityBarGeometry) {
    let raw_t = progress_q12.min(4096);
    let t = smoothstep_q12(raw_t);
    // 4t(1-t), in Q12: zero at both endpoints and one at the midpoint.
    let arc_q12 = ((i32::from(raw_t) * i32::from(4096 - raw_t)) >> 10) as u16;

    let incoming = lerp_bar(INACTIVE_VITALITY_BAR, ACTIVE_VITALITY_BAR, t, -3, arc_q12);
    let outgoing = lerp_bar(ACTIVE_VITALITY_BAR, INACTIVE_VITALITY_BAR, t, 8, arc_q12);
    (incoming, outgoing)
}

#[inline(always)]
fn lerp_bar(
    from: VitalityBarGeometry,
    to: VitalityBarGeometry,
    t_q12: u16,
    arc_pixels: i16,
    arc_q12: u16,
) -> VitalityBarGeometry {
    let (from_x, from_y, from_w, from_h) = from.bounds;
    let (to_x, to_y, to_w, to_h) = to.bounds;
    VitalityBarGeometry {
        bounds: (
            lerp_i16(from_x, to_x, t_q12) + lerp_i16(0, arc_pixels, arc_q12),
            lerp_i16(from_y, to_y, t_q12),
            lerp_i16(from_w, to_w, t_q12),
            lerp_i16(from_h, to_h, t_q12),
        ),
        cut: lerp_i16(from.cut, to.cut, t_q12),
    }
}

#[inline(always)]
fn smoothstep_q12(value: u16) -> u16 {
    let t = i32::from(value.min(4096));
    let t_squared = (t * t) >> 12;
    ((t_squared * (12288 - 2 * t)) >> 12) as u16
}

#[inline(always)]
fn lerp_i16(from: i16, to: i16, t_q12: u16) -> i16 {
    let delta = i32::from(to) - i32::from(from);
    let scaled = delta * i32::from(t_q12.min(4096));
    let rounded = if scaled >= 0 {
        scaled + 2048
    } else {
        scaled - 2048
    };
    from + (rounded / 4096) as i16
}

#[inline(always)]
fn lerp_rgb(from: (u8, u8, u8), to: (u8, u8, u8), t_q12: u16) -> (u8, u8, u8) {
    (
        lerp_i16(i16::from(from.0), i16::from(to.0), t_q12) as u8,
        lerp_i16(i16::from(from.1), i16::from(to.1), t_q12) as u8,
        lerp_i16(i16::from(from.2), i16::from(to.2), t_q12) as u8,
    )
}

#[inline(never)]
fn draw_vitality_bar(
    font: &FontAtlas,
    (x, y, w, h): (i16, i16, i16, i16),
    cut: i16,
    fill_q12: u16,
    rgb: (u8, u8, u8),
    label: &str,
) {
    draw_chamfered(x, y, w, h, cut, (rgb.0 / 2, rgb.1 / 2, rgb.2 / 2));
    draw_chamfered(
        x + 1,
        y + 1,
        w - 2,
        h - 2,
        cut.saturating_sub(1),
        (rgb.0 / 11, rgb.1 / 11, rgb.2 / 11),
    );

    const LABEL_SLOT_W: i16 = 28;
    const LABEL_INK_H: i16 = 6;
    const LABEL_TOP_BEARING: i16 = 1;
    let fill_x = x + LABEL_SLOT_W;
    let fill_w = (w - 34).max(0);
    let fill_h = (h - 10).max(3);
    let fill_y = y + (h - fill_h) / 2;
    let filled = ((i32::from(fill_w) * i32::from(fill_q12.min(4096))) >> 12) as i16;
    draw_rect(
        fill_x,
        fill_y,
        fill_w,
        fill_h,
        (rgb.0 / 12, rgb.1 / 12, rgb.2 / 12),
    );
    if filled > 0 {
        draw_rect(fill_x, fill_y, filled, fill_h, rgb);
        let mut step = 1;
        while step < 8 {
            let tx = fill_x + (fill_w * step) / 8;
            if tx < fill_x + filled {
                draw_rect(tx, fill_y, 1, fill_h, (rgb.0 / 3, rgb.1 / 3, rgb.2 / 3));
            }
            step += 1;
        }
    }
    // These fixed three-letter uppercase labels use rows 1..=6 of the 5x8
    // atlas. Centre their visible ink rather than the atlas cell, whose blank
    // first and last rows made the compact inactive shell look bottom-heavy.
    let label_w = font.text_width(label) as i16;
    let label_x = x + (LABEL_SLOT_W - label_w + 1) / 2;
    let label_y = y + (h - LABEL_INK_H) / 2 - LABEL_TOP_BEARING;
    font.draw_text(label_x, label_y, label, (rgb.0, rgb.1, rgb.2));
}

#[inline(never)]
fn draw_swap_cooldown_circle(
    cx: i16,
    cy: i16,
    dial_material: Option<TextureMaterial>,
    progress_q12: u16,
    rgb: (u8, u8, u8),
    echo_elapsed: Option<u16>,
) {
    // The authored hollow dial is always visible in neutral grey. A second
    // pass reveals the active stance tint clockwise in sixteen radial wedges,
    // providing a direct reading of the remaining lockout without replacing
    // the texture's fine concentric detail.
    if let Some(material) = dial_material {
        let x0 = cx - 13;
        let y0 = cy - 13;
        let x1 = cx + 13;
        let y1 = cy + 13;
        let u0 = psx_game_runtime::vram::VITALITY_HUD_DIAL_TEXEL_U;
        let v0 = psx_game_runtime::vram::VITALITY_HUD_DIAL_TEXEL_V;
        let u1 = u0.saturating_add(31);
        let v1 = v0.saturating_add(31);
        let verts = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
        let uvs = [(u0, v0), (u1, v0), (u0, v1), (u1, v1)];
        if progress_q12 >= 4096 {
            // The stable ready state is one quad, not sixteen persistent
            // wedges. The radial work exists only during the short lockout.
            draw_quad_textured_material(verts, uvs, material.with_tint(rgb));
        } else {
            draw_quad_textured_material(verts, uvs, material.with_tint((72, 72, 72)));
            draw_clockwise_dial_fill(cx, cy, u0, v0, material.with_tint(rgb), progress_q12);
        }
    }

    // Completion sends two additive rings out from the charge cell, echoing
    // the main-menu confirmation language without any text or extra chrome.
    if let Some(elapsed) = echo_elapsed {
        let radius = 12 + ((elapsed.min(12) >> 2) as i16);
        let echo_rgb = if elapsed < 6 {
            rgb
        } else {
            (rgb.0 / 2, rgb.1 / 2, rgb.2 / 2)
        };
        draw_rect(cx - 3, cy - radius, 6, 1, echo_rgb);
        draw_rect(cx + radius, cy - 3, 1, 6, echo_rgb);
        draw_rect(cx - 3, cy + radius, 6, 1, echo_rgb);
        draw_rect(cx - radius, cy - 3, 1, 6, echo_rgb);
        let diagonal = (radius * 3) / 4;
        draw_rect(cx + diagonal, cy - diagonal, 2, 2, echo_rgb);
        draw_rect(cx + diagonal, cy + diagonal, 2, 2, echo_rgb);
        draw_rect(cx - diagonal, cy + diagonal, 2, 2, echo_rgb);
        draw_rect(cx - diagonal, cy - diagonal, 2, 2, echo_rgb);
    }
}

#[inline(never)]
fn draw_clockwise_dial_fill(
    cx: i16,
    cy: i16,
    u0: u8,
    v0: u8,
    material: TextureMaterial,
    progress_q12: u16,
) {
    const SCREEN_RING: [(i16, i16); 17] = [
        (0, -13),
        (7, -13),
        (13, -13),
        (13, -7),
        (13, 0),
        (13, 7),
        (13, 13),
        (7, 13),
        (0, 13),
        (-7, 13),
        (-13, 13),
        (-13, 7),
        (-13, 0),
        (-13, -7),
        (-13, -13),
        (-7, -13),
        (0, -13),
    ];
    const UV_RING: [(u8, u8); 17] = [
        (16, 0),
        (24, 0),
        (31, 0),
        (31, 8),
        (31, 16),
        (31, 24),
        (31, 31),
        (24, 31),
        (16, 31),
        (8, 31),
        (0, 31),
        (0, 24),
        (0, 16),
        (0, 8),
        (0, 0),
        (8, 0),
        (16, 0),
    ];

    let scaled = u32::from(progress_q12.min(4096)) * 16;
    let complete = (scaled >> 12).min(16) as usize;
    let partial_q12 = (scaled & 4095) as i32;
    let center_uv = (u0.saturating_add(16), v0.saturating_add(16));
    let mut wedge = 0usize;
    while wedge < complete {
        let a = SCREEN_RING[wedge];
        let b = SCREEN_RING[wedge + 1];
        let auv = UV_RING[wedge];
        let buv = UV_RING[wedge + 1];
        draw_tri_textured_material(
            [(cx, cy), (cx + a.0, cy + a.1), (cx + b.0, cy + b.1)],
            [
                center_uv,
                (u0.saturating_add(auv.0), v0.saturating_add(auv.1)),
                (u0.saturating_add(buv.0), v0.saturating_add(buv.1)),
            ],
            material,
        );
        wedge += 1;
    }
    if complete < 16 && partial_q12 > 0 {
        let a = SCREEN_RING[complete];
        let b = SCREEN_RING[complete + 1];
        let auv = UV_RING[complete];
        let buv = UV_RING[complete + 1];
        let end = (
            a.0 + (((i32::from(b.0) - i32::from(a.0)) * partial_q12) >> 12) as i16,
            a.1 + (((i32::from(b.1) - i32::from(a.1)) * partial_q12) >> 12) as i16,
        );
        let end_uv = (
            i32::from(auv.0) + ((i32::from(buv.0) - i32::from(auv.0)) * partial_q12 >> 12),
            i32::from(auv.1) + ((i32::from(buv.1) - i32::from(auv.1)) * partial_q12 >> 12),
        );
        draw_tri_textured_material(
            [(cx, cy), (cx + a.0, cy + a.1), (cx + end.0, cy + end.1)],
            [
                center_uv,
                (u0.saturating_add(auv.0), v0.saturating_add(auv.1)),
                (
                    u0.saturating_add(end_uv.0 as u8),
                    v0.saturating_add(end_uv.1 as u8),
                ),
            ],
            material,
        );
    }
}

#[inline(always)]
pub(crate) fn swap_cooldown_display_progress_q12(total: u16, remaining: u16) -> u16 {
    if total == 0 || remaining == 0 {
        return 4096;
    }
    // The request and the first cooldown tick share one update. Hold that
    // first presented frame at zero (fully grey), then map the remaining
    // ticks across the complete radial sweep.
    let visible_span = total.saturating_sub(1).max(1);
    let spent = total
        .saturating_sub(remaining)
        .saturating_sub(1)
        .min(visible_span);
    ((u32::from(spent) * 4096) / u32::from(visible_span)) as u16
}

#[cfg(test)]
mod vitality_hud_tests {
    use super::{
        swap_cooldown_display_progress_q12, vitality_swap_geometry, ACTIVE_VITALITY_BAR,
        INACTIVE_VITALITY_BAR,
    };

    #[test]
    fn cooldown_display_starts_grey_and_finishes_fully_coloured() {
        assert_eq!(swap_cooldown_display_progress_q12(30, 29), 0);
        assert!(swap_cooldown_display_progress_q12(30, 15) > 1800);
        assert!(swap_cooldown_display_progress_q12(30, 15) < 2200);
        assert_eq!(swap_cooldown_display_progress_q12(30, 0), 4096);
        assert_eq!(swap_cooldown_display_progress_q12(0, 0), 4096);
    }

    #[test]
    fn vitality_swap_preserves_both_resting_endpoints() {
        let (incoming, outgoing) = vitality_swap_geometry(0);
        assert_eq!(incoming, INACTIVE_VITALITY_BAR);
        assert_eq!(outgoing, ACTIVE_VITALITY_BAR);

        let (incoming, outgoing) = vitality_swap_geometry(4096);
        assert_eq!(incoming, ACTIVE_VITALITY_BAR);
        assert_eq!(outgoing, INACTIVE_VITALITY_BAR);
    }

    #[test]
    fn active_bar_is_two_pixels_thicker_and_keeps_the_tight_gap() {
        assert_eq!(ACTIVE_VITALITY_BAR.bounds.3, INACTIVE_VITALITY_BAR.bounds.3 + 2);
        assert_eq!(
            ACTIVE_VITALITY_BAR.bounds.1 + ACTIVE_VITALITY_BAR.bounds.3 + 1,
            INACTIVE_VITALITY_BAR.bounds.1
        );
    }

    #[test]
    fn vitality_bars_pass_on_separate_horizontal_arcs() {
        let (incoming, outgoing) = vitality_swap_geometry(2048);
        assert!(incoming.bounds.0 < outgoing.bounds.0);
        assert_eq!(incoming.bounds.1, outgoing.bounds.1);
        assert_eq!(incoming.bounds.2, outgoing.bounds.2);
        assert_eq!(incoming.bounds.3, outgoing.bounds.3);
    }
}
