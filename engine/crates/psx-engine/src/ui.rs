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
use psx_level::{
    ui_node_flags, AssetId, LevelOptionDef, LevelUiNodeKind, LevelUiNodeRecord,
    LevelUiValueBinding, NavRect, UI_OPTION_NONE,
};

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

/// Draw the cooked nodes `nodes[first..first + count]` of one UI scene
/// to the framebuffer.
///
/// `nodes` is always the *full* shared node pool, never a sub-slice:
/// cooked parent indices are pool-relative (see the cooker's
/// `cook_ui_scene_nodes`), so anchor/parent layout walks the whole pool
/// even though only the `[first, first + count)` block is painted.
/// `first` / `count` come straight from the active
/// [`psx_level::LevelUiScene`]; pass `first = 0`, `count = nodes.len()`
/// to draw a single-scene pool whole (the HUD overlay does this).
///
/// `font` supplies glyph metrics and the draw path for
/// [`LevelUiNodeKind::Label`] nodes; when `None`, labels are skipped.
///
/// `focused` is the *pool* index of the currently focused node, if any,
/// so a focused [`LevelUiNodeKind::Button`] / [`LevelUiNodeKind::Slider`]
/// gets a focus ring. The game-flow driver tracks focus as a pool index
/// and resolves moves through [`psx_level::next_focus`] over the same
/// pool, so the highlight here matches the control input lands on. Pass
/// `None` for non-interactive overlays such as a HUD.
///
/// `textures` resolves an image node's [`AssetId`] to an uploaded
/// [`UiTextureSlot`], or `None` to skip that image. It is `FnMut` so
/// the resolver may lazily upload / mutate residency state.
///
/// `value` resolves a [`LevelUiValueBinding`] to a Q12 fixed-point
/// integer for bar fill ratios.
///
/// `options` is the cooked project-option table and `option_value`
/// resolves an option id to its live runtime value. A [`LevelUiNodeKind::Slider`]
/// draws its knob from the bound option's value: the fill proportion is
/// `(value - min) / (max - min)` clamped to `[0, 1]`, looking the option
/// up in `options` by id. A slider bound to [`UI_OPTION_NONE`], or to an
/// id missing from `options`, or with a degenerate `min == max` range,
/// draws an empty track. The caller owns the option store, so a HUD with
/// no options passes an empty slice and a resolver returning `0`.
///
/// Drawing order follows pool order, so authoring order is the
/// back-to-front paint order.
#[allow(clippy::too_many_arguments)]
pub fn draw_scene(
    nodes: &[LevelUiNodeRecord],
    first: usize,
    count: usize,
    font: Option<&FontAtlas>,
    focused: Option<usize>,
    textures: &mut impl FnMut(AssetId) -> Option<UiTextureSlot>,
    value: &impl Fn(LevelUiValueBinding) -> i32,
    options: &[LevelOptionDef],
    option_value: &impl Fn(u16) -> i32,
) {
    let end = first.saturating_add(count).min(nodes.len());
    for index in first..end {
        let node = &nodes[index];
        let (x, y, width, height) = node_absolute_rect(nodes, index);
        let is_focused = focused == Some(index);
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
            LevelUiNodeKind::Button => {
                draw_button(font, node, x, y, width, height);
                if is_focused {
                    draw_focus_ring(x, y, width as i16, height as i16);
                }
            }
            LevelUiNodeKind::Slider => {
                // Resolve the bound option's live value to a fill
                // proportion (num/den). An unbound / unknown / degenerate
                // option yields 0/1 (empty track).
                let (fill_num, fill_den) =
                    slider_fill(node.option, options, option_value);
                draw_slider(
                    x,
                    y,
                    width as i16,
                    height as i16,
                    fill_num,
                    fill_den,
                    rgb(node.color),
                    rgb(node.background),
                    rgb(node.accent),
                );
                if is_focused {
                    draw_focus_ring(x, y, width as i16, height as i16);
                }
            }
        }
    }
}

/// Bright 1px outline drawn just outside a focused control's rect so
/// the highlight reads regardless of the control's own colours. Four
/// thin [`draw_rect`] edges, integer-only, no allocation.
fn draw_focus_ring(x: i16, y: i16, width: i16, height: i16) {
    if width <= 0 || height <= 0 {
        return;
    }
    const RING: (u8, u8, u8) = (248, 224, 96);
    let left = x - 1;
    let top = y - 1;
    let outer_w = width + 2;
    let outer_h = height + 2;
    // Top and bottom edges span the full outer width; the side edges
    // fill the gap between them so the corners paint exactly once.
    draw_rect(left, top, outer_w, 1, RING);
    draw_rect(left, top + outer_h - 1, outer_w, 1, RING);
    draw_rect(left, top + 1, 1, outer_h - 2, RING);
    draw_rect(left + outer_w - 1, top + 1, 1, outer_h - 2, RING);
}

/// Fill proportion `(num, den)` for a slider bound to option `option_id`,
/// for [`draw_slider`]'s `[0, 1]` knob position.
///
/// `num` is `value - min` and `den` is `max - min`, where `value` comes
/// from `option_value(option_id)` and `min` / `max` come from the matching
/// [`LevelOptionDef`] in `options`. A slider bound to [`UI_OPTION_NONE`],
/// to an id not present in `options`, or to a degenerate `min == max`
/// range returns `(0, 1)` (an empty track). `draw_slider` clamps `num`
/// into `[0, den]`, so an out-of-range value cannot overflow the track.
fn slider_fill(
    option_id: u16,
    options: &[LevelOptionDef],
    option_value: &impl Fn(u16) -> i32,
) -> (i32, i32) {
    if option_id == UI_OPTION_NONE {
        return (0, 1);
    }
    let Some(option) = options.iter().find(|option| option.id == option_id) else {
        return (0, 1);
    };
    let den = option.max - option.min;
    if den <= 0 {
        return (0, 1);
    }
    let num = option_value(option_id) - option.min;
    (num, den)
}

/// Resolve a node's absolute on-screen rectangle, applying the
/// parent chain and 9-point anchor. Falls back to a 1x1 rect at the
/// origin for an out-of-range index.
fn node_absolute_rect(nodes: &[LevelUiNodeRecord], index: usize) -> (i16, i16, u16, u16) {
    node_absolute_rect_inner(nodes, index, 0).unwrap_or((0, 0, 1, 1))
}

/// `true` when a node kind takes menu focus and so should be drawn
/// with a focus ring and visited by [`psx_level::next_focus`]. Only
/// [`LevelUiNodeKind::Button`] and [`LevelUiNodeKind::Slider`] are
/// interactive; everything else is decoration.
#[inline]
pub fn is_focusable(kind: LevelUiNodeKind) -> bool {
    matches!(kind, LevelUiNodeKind::Button | LevelUiNodeKind::Slider)
}

/// Absolute on-screen rectangle of node `index` as a
/// [`NavRect`], so the game-flow driver can build the focusable-rect
/// list the resolver consumes without duplicating the anchor/parent
/// layout maths. Uses the same resolution as [`draw_scene`], so the
/// focus ring and the navigation geometry never drift apart.
pub fn node_nav_rect(nodes: &[LevelUiNodeRecord], index: usize) -> NavRect {
    let (x, y, w, h) = node_absolute_rect(nodes, index);
    NavRect { x, y, w, h }
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

/// Draw an interactive button: a filled rectangle with a thin top
/// highlight, then its label aligned inside the rect using the same
/// horizontal alignment + word-wrap path as [`LevelUiNodeKind::Label`]
/// and vertically centred. The focus ring is drawn by [`draw_scene`].
fn draw_button(
    font: Option<&FontAtlas>,
    node: &LevelUiNodeRecord,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
) {
    if node.flags & ui_node_flags::BUTTON_TRANSPARENT == 0 {
        let fill = rgb(node.color);
        draw_rect(x, y, width as i16, height as i16, fill);
        if height > 3 {
            draw_rect(x, y, width as i16, 1, brighten(fill));
        }
    }
    let Some(font) = font else {
        return;
    };
    if node.text.is_empty() {
        return;
    }
    let line_h = font.line_height() as i32;
    let text_y = (y as i32 + (height as i32 - line_h).max(0) / 2)
        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let align = (node.flags & ui_node_flags::TEXT_ALIGN_MASK) >> ui_node_flags::TEXT_ALIGN_SHIFT;
    let text_x = aligned_text_x(font, node.text, x, width, align);
    font.draw_text(text_x, text_y, node.text, rgb(node.accent));
}

/// Draw a slider: a recessed track, a proportional fill, and a knob
/// rectangle centred on the fill edge. `fill_num / fill_den` is the
/// current proportion; `fill_num` is clamped into `[0, fill_den]` here so
/// an out-of-range value cannot run the knob off the track. The bound
/// option's value feeds this through [`slider_fill`] in [`draw_scene`].
#[allow(clippy::too_many_arguments)]
fn draw_slider(
    x: i16,
    y: i16,
    width: i16,
    height: i16,
    fill_num: i32,
    fill_den: i32,
    track: (u8, u8, u8),
    fill: (u8, u8, u8),
    knob: (u8, u8, u8),
) {
    if width <= 0 || height <= 0 {
        return;
    }
    draw_rect(x - 1, y - 1, width + 2, height + 2, (12, 14, 18));
    draw_rect(x, y, width, height, track);

    let den = fill_den.max(1);
    let num = fill_num.clamp(0, den);
    let fill_width = ((width as i32).saturating_mul(num) / den) as i16;
    if fill_width > 0 {
        draw_rect(x, y, fill_width, height, fill);
    }

    // Knob: a fixed-width rect centred on the fill edge, clamped so it
    // stays inside the track.
    let knob_w = (height + 2).clamp(3, width.max(3));
    let edge = x as i32 + fill_width as i32;
    let knob_x = (edge - knob_w as i32 / 2).clamp(x as i32, x as i32 + width as i32 - knob_w as i32);
    let knob_x = knob_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    draw_rect(knob_x, y - 1, knob_w, height + 2, knob);
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
