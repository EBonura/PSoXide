//! Screen-space UI scene renderer.
//!
//! Draws a slice of cooked [`LevelUiNodeRecord`]s straight to the
//! framebuffer using the SDK's flat / textured quad and font paths.
//! This is the engine-level home for the HUD/UI rendering that used
//! to live inside the editor-playtest example, so any project can
//! render its cooked UI nodes without copying the layout maths.
//!
//! # Decoupling from asset streaming
//!
//! The renderer never reaches into a project's asset table or VRAM
//! residency manager. The two project-specific concerns are passed
//! in as closures:
//!
//! - **Texture resolution.** Image nodes name a [`AssetId`]; the
//!   caller turns that into an already-uploaded VRAM [`UiTextureSlot`]
//!   (or `None` if it is not resident this frame). The engine owns no
//!   upload path.
//! - **Value bindings.** Bar nodes name a [`LevelUiValueBinding`]
//!   (player health, stamina, a literal, ...); the caller resolves it
//!   to a Q12 fixed-point integer. The engine hardcodes no gameplay
//!   field.
//!
//! Everything else (rect fills, label alignment + word wrap, the
//! 9-point anchor layout, bar fill geometry) is integer-only and
//! lives here.
//!
//! # `no_std`
//!
//! Integer-only, no allocator, no `f32`/`f64`. Layout recursion is
//! depth-bounded by the node count so a malformed parent chain cannot
//! loop forever.

use psx_font::FontAtlas;
use psx_gpu::{
    draw_quad_flat, draw_quad_textured_material,
    material::{TextureMaterial, TextureWindow},
};
use psx_level::{ui_node_flags, AssetId, LevelUiNodeKind, LevelUiNodeRecord, LevelUiValueBinding};

/// Canvas width used as the fallback parent rectangle for a node
/// whose parent chain does not resolve to a [`LevelUiNodeKind::Canvas`].
/// Matches the PS1 standard 320x240 framebuffer.
pub const UI_CANVAS_W: u16 = 320;
/// Canvas height counterpart to [`UI_CANVAS_W`].
pub const UI_CANVAS_H: u16 = 240;

/// Everything [`draw_scene`] needs to turn an image node into a
/// textured quad, mirroring the fields the example's per-asset VRAM
/// slot record exposes.
///
/// The caller produces one of these from its own upload bookkeeping;
/// the engine treats the words as opaque GPU state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UiTextureSlot {
    /// Packed CLUT word for [`TextureMaterial::opaque`].
    pub clut_word: u16,
    /// Packed tpage word for [`TextureMaterial::opaque`].
    pub tpage_word: u16,
    /// Texture window confining sampling to this slot's sub-rectangle.
    pub texture_window: TextureWindow,
    /// Texture width in texels (used to derive the far UV; clamped to
    /// the GP0 8-bit UV range).
    pub texture_width: u16,
    /// Texture height in texels (far UV counterpart to
    /// [`Self::texture_width`]).
    pub texture_height: u16,
}

/// Draw a cooked UI scene to the framebuffer.
///
/// `nodes` is the full node slice; layout walks parent links inside
/// it. `font` supplies glyph metrics and the draw path for
/// [`LevelUiNodeKind::Label`] nodes; when `None`, labels are skipped.
///
/// `textures` resolves an image node's [`AssetId`] to an uploaded
/// [`UiTextureSlot`], or `None` to skip that image. It is `FnMut` so
/// the resolver may lazily upload / mutate residency state.
///
/// `value` resolves a [`LevelUiValueBinding`] to a Q12 fixed-point
/// integer for bar fill ratios.
///
/// Drawing order follows slice order, so authoring order is the
/// back-to-front paint order.
pub fn draw_scene(
    nodes: &[LevelUiNodeRecord],
    font: Option<&FontAtlas>,
    textures: &mut impl FnMut(AssetId) -> Option<UiTextureSlot>,
    value: &impl Fn(LevelUiValueBinding) -> i32,
) {
    for (index, node) in nodes.iter().enumerate() {
        let (x, y, width, height) = node_absolute_rect(nodes, index);
        match node.kind {
            LevelUiNodeKind::Canvas | LevelUiNodeKind::Group => {}
            LevelUiNodeKind::Rect => {
                draw_rect(x, y, width as i16, height as i16, rgb(node.color));
            }
            LevelUiNodeKind::Label => {
                if let Some(font) = font {
                    draw_label(font, node, x, y, width);
                }
            }
            LevelUiNodeKind::Image => {
                draw_image(node, x, y, width, height, textures);
            }
            LevelUiNodeKind::Bar => {
                let max_q12 = value(node.max).max(1);
                let value_q12 = value(node.value).clamp(0, max_q12);
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

/// Resolve a node's absolute on-screen rectangle, applying the
/// parent chain and 9-point anchor. Falls back to a 1x1 rect at the
/// origin for an out-of-range index.
fn node_absolute_rect(nodes: &[LevelUiNodeRecord], index: usize) -> (i16, i16, u16, u16) {
    node_absolute_rect_inner(nodes, index, 0).unwrap_or((0, 0, 1, 1))
}

fn node_absolute_rect_inner(
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
        .and_then(|parent| node_absolute_rect_inner(nodes, parent.to_usize(), depth + 1))
        .unwrap_or((0, 0, UI_CANVAS_W, UI_CANVAS_H));
    let (anchor_x, anchor_y) = anchor_factors(node.flags);
    let x = parent_x as i32 + (parent_w as i32 * anchor_x) / 2 + node.x as i32;
    let y = parent_y as i32 + (parent_h as i32 * anchor_y) / 2 + node.y as i32;
    Some((
        x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        node.width.max(1),
        node.height.max(1),
    ))
}

/// Map the anchor nibble to half-step (x, y) factors. The factors are
/// halves (0, 1, 2) so `parent_extent * factor / 2` lands on the
/// near edge, centre, or far edge without fractional maths.
fn anchor_factors(flags: u16) -> (i32, i32) {
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

fn draw_label(font: &FontAtlas, node: &LevelUiNodeRecord, x: i16, y: i16, width: u16) {
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
            return last_space
                .filter(|space| *space > start)
                .unwrap_or(end.max(start + 1));
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

fn draw_image(
    node: &LevelUiNodeRecord,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    textures: &mut impl FnMut(AssetId) -> Option<UiTextureSlot>,
) {
    if node.texture_asset.0 == u16::MAX {
        draw_rect(x, y, width as i16, height as i16, rgb(node.color));
        return;
    }
    let Some(slot) = textures(node.texture_asset) else {
        return;
    };
    let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, rgb(node.color))
        .with_texture_window(slot.texture_window);
    let tex_w = texture_size_u8(slot.texture_width).saturating_sub(1);
    let tex_h = texture_size_u8(slot.texture_height).saturating_sub(1);
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

/// Clamp a texel dimension into the GP0 8-bit UV range.
fn texture_size_u8(size: u16) -> u8 {
    size.min(u16::from(u8::MAX)) as u8
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

fn status_fill_width(width: i16, value: i32, max_value: i32) -> i16 {
    if width <= 0 || max_value <= 0 {
        return 0;
    }
    let clamped = value.clamp(0, max_value);
    ((width as i32).saturating_mul(clamped) / max_value) as i16
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

fn brighten(color: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        color.0.saturating_add(34),
        color.1.saturating_add(34),
        color.2.saturating_add(34),
    )
}

fn rgb(color: [u8; 3]) -> (u8, u8, u8) {
    (color[0], color[1], color[2])
}
