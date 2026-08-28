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
    draw_line_mono, draw_quad_flat, draw_quad_textured_gouraud_material,
    draw_quad_textured_material, draw_tri_flat, draw_tri_flat_blended, draw_tri_gouraud,
    draw_tri_gouraud_blended,
    material::{BlendMode, TextureMaterial, TextureWindow},
};
use psx_level::{
    ui_node_flags, ui_shape, AssetId, LevelOptionDef, LevelUiFocusEffect, LevelUiFocusStyle,
    LevelUiGradientDirection, LevelUiImageEffect, LevelUiNodeKind, LevelUiNodeRecord,
    LevelUiPaintRecord, LevelUiValueBinding, NavRect, UI_OPTION_NONE, UI_PAINT_NONE,
};
use psx_math::{cos_q12, sin_q12};

/// Canvas width used as the fallback parent rectangle for a node
/// whose parent chain does not resolve to a [`LevelUiNodeKind::Canvas`].
/// Matches the PS1 standard 320x240 framebuffer.
pub const UI_CANVAS_W: u16 = 320;
/// Canvas height counterpart to [`UI_CANVAS_W`].
pub const UI_CANVAS_H: u16 = 240;
/// Presented frames held between a CROSS press and its UI action.
///
/// The source control remains on screen for this entire beat while two
/// shape-aware perimeter echoes expand and fade. Eight frames is long enough
/// to read at 60 Hz without making menu input feel laggy.
pub const ACTIVATION_ECHO_FRAMES: u16 = 8;

/// Placement preset for the shared Archive-message chrome.
///
/// Point-of-interest copy stays close to the bottom edge and is limited to
/// two lines. The world-introduction variant is centred and has room for
/// three. Both use the same PS1-native chamfer, translucent fill, and scanner
/// treatment as authored UI shapes, without allocating an image or VRAM slot.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MessagePanelVariant {
    /// Bottom-centred, two-line point-of-interest message.
    PointOfInterest,
    /// Screen-centred, three-line scene/world introduction.
    World,
}

/// Zero-based page position presented by [`draw_message_panel`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MessagePageMeta {
    /// Current zero-based page index.
    pub index: u8,
    /// Total page count. Zero is treated as one page.
    pub count: u8,
}

impl MessagePageMeta {
    /// Build page metadata, clamping an empty page set to one page and the
    /// current index to the final available page.
    pub const fn new(index: u8, count: u8) -> Self {
        let count = if count == 0 { 1 } else { count };
        Self {
            index: if index < count { index } else { count - 1 },
            count,
        }
    }
}

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

/// Allocation-free live-text bridge used by cooked UI scenes. The resolver
/// may borrow static copy or compose into the per-node scratch buffer.
pub trait UiTextResolver {
    /// Resolve `tag`, composing into `scratch` when the text is not static.
    fn resolve<'a>(&self, tag: &str, scratch: &'a mut [u8]) -> Option<&'a str>;
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct UiAffine {
    m00: i32,
    m01: i32,
    m10: i32,
    m11: i32,
    tx: i32,
    ty: i32,
}

impl UiAffine {
    const IDENTITY: Self = Self {
        m00: 4096,
        m01: 0,
        m10: 0,
        m11: 4096,
        tx: 0,
        ty: 0,
    };

    fn from_node_rect(x: i16, y: i16, width: u16, height: u16, node: &LevelUiNodeRecord) -> Self {
        let angle = degrees_to_q12(node.rotation_degrees);
        let sin = sin_q12(angle);
        let cos = cos_q12(angle);
        let fx = if node.flags & ui_node_flags::FLIP_X != 0 {
            -1
        } else {
            1
        };
        let fy = if node.flags & ui_node_flags::FLIP_Y != 0 {
            -1
        } else {
            1
        };
        let m00 = cos.saturating_mul(fx);
        let m01 = -sin.saturating_mul(fy);
        let m10 = sin.saturating_mul(fx);
        let m11 = cos.saturating_mul(fy);
        let half_w_q12 = i32::from(width) << 11;
        let half_h_q12 = i32::from(height) << 11;
        let center_x_q12 = (i32::from(x) << 12).saturating_add(half_w_q12);
        let center_y_q12 = (i32::from(y) << 12).saturating_add(half_h_q12);
        Self {
            m00,
            m01,
            m10,
            m11,
            tx: center_x_q12
                .saturating_sub(mul_q12(m00, half_w_q12))
                .saturating_sub(mul_q12(m01, half_h_q12)),
            ty: center_y_q12
                .saturating_sub(mul_q12(m10, half_w_q12))
                .saturating_sub(mul_q12(m11, half_h_q12)),
        }
    }

    fn compose(self, child: Self) -> Self {
        Self {
            m00: mul_q12(self.m00, child.m00).saturating_add(mul_q12(self.m01, child.m10)),
            m01: mul_q12(self.m00, child.m01).saturating_add(mul_q12(self.m01, child.m11)),
            m10: mul_q12(self.m10, child.m00).saturating_add(mul_q12(self.m11, child.m10)),
            m11: mul_q12(self.m10, child.m01).saturating_add(mul_q12(self.m11, child.m11)),
            tx: mul_q12(self.m00, child.tx)
                .saturating_add(mul_q12(self.m01, child.ty))
                .saturating_add(self.tx),
            ty: mul_q12(self.m10, child.tx)
                .saturating_add(mul_q12(self.m11, child.ty))
                .saturating_add(self.ty),
        }
    }

    fn point(self, x: i32, y: i32) -> (i16, i16) {
        let sx = self
            .tx
            .saturating_add(self.m00.saturating_mul(x))
            .saturating_add(self.m01.saturating_mul(y));
        let sy = self
            .ty
            .saturating_add(self.m10.saturating_mul(x))
            .saturating_add(self.m11.saturating_mul(y));
        (round_q12_to_i16(sx), round_q12_to_i16(sy))
    }

    fn subrect(self, x: i16, y: i16, width: i16, height: i16) -> [(i16, i16); 4] {
        let x0 = i32::from(x);
        let y0 = i32::from(y);
        let x1 = x0.saturating_add(i32::from(width));
        let y1 = y0.saturating_add(i32::from(height));
        [
            self.point(x0, y0),
            self.point(x1, y0),
            self.point(x0, y1),
            self.point(x1, y1),
        ]
    }

    fn font_matrix(self, scale_q8: u16) -> [[i16; 2]; 2] {
        [
            [
                mul_scale_q8_to_i16(self.m00, scale_q8),
                mul_scale_q8_to_i16(self.m01, scale_q8),
            ],
            [
                mul_scale_q8_to_i16(self.m10, scale_q8),
                mul_scale_q8_to_i16(self.m11, scale_q8),
            ],
        ]
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct UiResolvedNode {
    width: u16,
    height: u16,
    transform: UiAffine,
    verts: [(i16, i16); 4],
}

impl UiResolvedNode {
    fn subrect(self, x: i16, y: i16, width: i16, height: i16) -> [(i16, i16); 4] {
        self.transform.subrect(x, y, width, height)
    }

    fn bounds(self) -> (i16, i16, u16, u16) {
        quad_bounds(self.verts)
    }
}

fn mul_q12(a: i32, b_q12: i32) -> i32 {
    // psx-numeric-allow-next-line: Q12 multiply widens through i64; R3000 mult yields the 64-bit product natively
    ((i64::from(a) * i64::from(b_q12)) >> 12).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn mul_scale_q8_to_i16(value_q12: i32, scale_q8: u16) -> i16 {
    // psx-numeric-allow-next-line: Q12 scale widens through i64; R3000 mult yields the 64-bit product natively
    ((i64::from(value_q12) * i64::from(scale_q8)) >> 8)
        // psx-numeric-allow-next-line: Q12 clamp widens through i64; R3000 mult yields the 64-bit product natively
        .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

fn round_q12_to_i16(value: i32) -> i16 {
    let rounded = if value >= 0 {
        value.saturating_add(2048) >> 12
    } else {
        -(value.saturating_neg().saturating_add(2048) >> 12)
    };
    rounded.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn degrees_to_q12(degrees: i16) -> u16 {
    let mut degrees = i32::from(degrees) % 360;
    if degrees < 0 {
        degrees += 360;
    }
    ((degrees * 4096 + 180) / 360) as u16 & 0x0fff
}

fn quad_bounds(verts: [(i16, i16); 4]) -> (i16, i16, u16, u16) {
    let mut min_x = verts[0].0;
    let mut max_x = verts[0].0;
    let mut min_y = verts[0].1;
    let mut max_y = verts[0].1;
    let mut i = 1usize;
    while i < verts.len() {
        min_x = min_x.min(verts[i].0);
        max_x = max_x.max(verts[i].0);
        min_y = min_y.min(verts[i].1);
        max_y = max_y.max(verts[i].1);
        i += 1;
    }
    (
        min_x,
        min_y,
        (i32::from(max_x) - i32::from(min_x))
            .max(1)
            .min(i32::from(u16::MAX)) as u16,
        (i32::from(max_y) - i32::from(min_y))
            .max(1)
            .min(i32::from(u16::MAX)) as u16,
    )
}

fn default_resolved_node() -> UiResolvedNode {
    let transform = UiAffine::IDENTITY;
    let verts = transform.subrect(0, 0, 1, 1);
    UiResolvedNode {
        width: 1,
        height: 1,
        transform,
        verts,
    }
}

fn node_resolved(nodes: &[LevelUiNodeRecord], index: usize) -> Option<UiResolvedNode> {
    node_resolved_inner(nodes, index, 0)
}

fn node_resolved_inner(
    nodes: &[LevelUiNodeRecord],
    index: usize,
    depth: usize,
) -> Option<UiResolvedNode> {
    if depth > nodes.len() {
        return None;
    }
    let node = nodes.get(index)?;
    if matches!(node.kind, LevelUiNodeKind::Canvas) {
        return Some(resolved_from_parent(
            UiAffine::IDENTITY,
            0,
            0,
            node.width.max(1),
            node.height.max(1),
            node,
        ));
    }

    let parent = node
        .parent
        .and_then(|parent| node_resolved_inner(nodes, parent.to_usize(), depth + 1))
        .unwrap_or(UiResolvedNode {
            width: UI_CANVAS_W,
            height: UI_CANVAS_H,
            transform: UiAffine::IDENTITY,
            verts: UiAffine::IDENTITY.subrect(0, 0, UI_CANVAS_W as i16, UI_CANVAS_H as i16),
        });
    let (anchor_x, anchor_y) = anchor_factors(node.flags);
    let local_x = (i32::from(parent.width) * anchor_x / 2).saturating_add(i32::from(node.x));
    let local_y = (i32::from(parent.height) * anchor_y / 2).saturating_add(i32::from(node.y));
    Some(resolved_from_parent(
        parent.transform,
        local_x.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        local_y.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        node.width.max(1),
        node.height.max(1),
        node,
    ))
}

fn resolved_from_parent(
    parent: UiAffine,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    node: &LevelUiNodeRecord,
) -> UiResolvedNode {
    let local = UiAffine::from_node_rect(x, y, width, height, node);
    let transform = parent.compose(local);
    let verts = transform.subrect(0, 0, width as i16, height as i16);
    UiResolvedNode {
        width,
        height,
        transform,
        verts,
    }
}

fn node_visible_in_tree(
    nodes: &[LevelUiNodeRecord],
    index: usize,
    visible: &impl Fn(&LevelUiNodeRecord) -> bool,
) -> bool {
    let mut cursor = Some(index);
    let mut depth = 0;
    while let Some(current) = cursor {
        let Some(node) = nodes.get(current) else {
            return false;
        };
        if !visible(node) {
            return false;
        }
        depth += 1;
        if depth > nodes.len() {
            return false;
        }
        cursor = node.parent.map(|parent| usize::from(parent.raw()));
    }
    true
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
/// `fonts` is the font table indexed by each node's
/// [`LevelUiNodeRecord::font`] selector: a `Label` / `Button` draws with
/// `fonts[node.font]`, falling back to `fonts[0]` when its selector is out of
/// range or the selected slot is empty. An empty table skips all text (the
/// slice is the multi-font replacement for the old single `Option<&FontAtlas>`;
/// pass `[Some(font)]` for the common single-font case).
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
///
/// `visible` lets higher-level runtime code apply game/input state to
/// authored nodes without baking those states into this renderer.
pub fn draw_scene(
    nodes: &[LevelUiNodeRecord],
    first: usize,
    count: usize,
    paints: &[LevelUiPaintRecord],
    fonts: &[Option<&FontAtlas>],
    focused: Option<usize>,
    focus_style: &LevelUiFocusStyle,
    frame: u16,
    text_seed: u16,
    textures: &mut impl FnMut(AssetId) -> Option<UiTextureSlot>,
    value: &impl Fn(LevelUiValueBinding) -> i32,
    options: &[LevelOptionDef],
    option_value: &impl Fn(u16) -> i32,
    visible: &impl Fn(&LevelUiNodeRecord) -> bool,
    text: &impl UiTextResolver,
) {
    let end = first.saturating_add(count).min(nodes.len());
    for index in first..end {
        let authored_node = &nodes[index];
        if !node_visible_in_tree(nodes, index, visible) {
            continue;
        }
        let mut text_scratch = [0u8; 96];
        let resolved_text = if authored_node.tag.is_empty() {
            None
        } else {
            text.resolve(authored_node.tag, &mut text_scratch)
        };
        let node = authored_node;
        let resolved = node_resolved(nodes, index).unwrap_or_else(default_resolved_node);
        let is_focused = focused == Some(index);
        match node.kind {
            LevelUiNodeKind::Canvas
            | LevelUiNodeKind::Group
            | LevelUiNodeKind::Music
            | LevelUiNodeKind::Timer => {}
            LevelUiNodeKind::Rect => {
                draw_shape_node(node, resolved, paints, None, None);
            }
            LevelUiNodeKind::Label => {
                if let Some(font) = node_font(fonts, node.font) {
                    draw_label(
                        font,
                        node,
                        resolved,
                        paints,
                        text_seed.wrapping_add((index as u16).wrapping_mul(0x9e37)),
                        frame,
                        resolved_text,
                    );
                }
            }
            LevelUiNodeKind::Image => {
                draw_image(node, resolved, frame, textures);
            }
            LevelUiNodeKind::Bar => {
                let max_q12 = value(node.max).max(1);
                let value_q12 = value(node.value).clamp(0, max_q12);
                if node.texture_asset.0 != u16::MAX && node.option >= 2 {
                    draw_sprite_bar(node, resolved, value_q12, max_q12, textures);
                } else {
                    draw_status_bar(
                        resolved,
                        value_q12,
                        max_q12,
                        shape_paint(node.color, node.color_paint, paints),
                        shape_paint(node.background, node.background_paint, paints),
                    );
                }
            }
            LevelUiNodeKind::Button => {
                draw_button(
                    node_font(fonts, node.font),
                    node,
                    resolved,
                    paints,
                    is_focused,
                    frame,
                    focus_style,
                    resolved_text,
                );
                if is_focused {
                    draw_focus_ring(resolved, focus_style, frame, Some(shape_config(node)));
                }
            }
            LevelUiNodeKind::Slider => {
                // Resolve the bound option's live value to a fill
                // proportion (num/den). An unbound / unknown / degenerate
                // option yields 0/1 (empty track).
                let (fill_num, fill_den) = slider_fill(node.option, options, option_value);
                draw_slider(
                    resolved,
                    fill_num,
                    fill_den,
                    shape_paint(node.color, node.color_paint, paints),
                    shape_paint(node.background, node.background_paint, paints),
                    shape_paint(node.accent, node.accent_paint, paints),
                );
                if is_focused {
                    draw_focus_ring(resolved, focus_style, frame, None);
                }
            }
        }
    }
}

/// Draw the standard Cortex-style confirmation burst for one cooked UI node.
///
/// This is deliberately separate from [`draw_scene`]: the flow driver owns
/// the deferred action and asks for the overlay only while that action is
/// armed. The outline is derived from the node's encoded shape, so clipped
/// corners expand along their real perimeter instead of falling back to an
/// axis-aligned rectangle. Everything is integer-only and allocation-free.
pub fn draw_activation_echo(
    nodes: &[LevelUiNodeRecord],
    node_index: usize,
    style: &LevelUiFocusStyle,
    frame: u16,
) {
    if frame >= ACTIVATION_ECHO_FRAMES {
        return;
    }
    let Some(node) = nodes.get(node_index) else {
        return;
    };
    if !matches!(
        node.kind,
        LevelUiNodeKind::Button | LevelUiNodeKind::Rect | LevelUiNodeKind::Image
    ) {
        return;
    }
    let Some(resolved) = node_resolved(nodes, node_index) else {
        return;
    };
    if resolved.width == 0 || resolved.height == 0 {
        return;
    }
    let config = shape_config(node);

    // A short additive-looking body flash makes the press read before the
    // outlines leave the control. Average blending is the same PS1-cheap path
    // used by translucent authored panels and behaves well over dark menus.
    if frame < 3 {
        let strength = 96u8.saturating_sub((frame as u8).saturating_mul(28));
        let flash = scale_color_q8(style.color_a, strength);
        let polygon = shape_polygon(resolved, config.corners, config.cut, 1);
        draw_shape_polygon(
            &polygon,
            UiPaint::Solid(flash),
            resolved.width,
            resolved.height,
            true,
        );
    }

    // Keep a bright copy on the original perimeter for the first half of the
    // beat, then let the two delayed echoes carry the release outward.
    if frame < 4 {
        let strength = 255u8.saturating_sub((frame as u8).saturating_mul(44));
        let color = scale_color_q8(style.color_a, strength);
        let polygon = shape_polygon(resolved, config.corners, config.cut, 0);
        draw_shape_outline(&polygon, color);
    }

    for ring in 0..2u16 {
        let delay = ring.saturating_mul(2);
        let Some((expansion, strength)) = activation_echo_layer(frame, delay, ring) else {
            continue;
        };
        let color = scale_color_q8(style.color_a, strength);
        let polygon = shape_polygon(resolved, config.corners, config.cut, -i32::from(expansion));
        draw_shape_outline(&polygon, color);
    }
}

/// Pure timing helper for one activation contour. The second contour starts
/// two frames later and travels farther, preventing the result from reading as
/// a static double border.
fn activation_echo_layer(frame: u16, delay: u16, ring: u16) -> Option<(u8, u8)> {
    if frame < delay || frame >= ACTIVATION_ECHO_FRAMES {
        return None;
    }
    let age = frame - delay;
    let life = ACTIVATION_ECHO_FRAMES - delay;
    let remaining = life.saturating_sub(age);
    let peak = if ring == 0 { 224u16 } else { 176u16 };
    let strength = ((peak * remaining) / life.max(1)).min(255) as u8;
    let expansion = if ring == 0 { 1 + age / 2 } else { 2 + age }.min(u16::from(u8::MAX)) as u8;
    Some((expansion, strength))
}

/// Outline drawn just outside a focused control's rect so the
/// highlight reads regardless of the control's own colours. The
/// scene's [`LevelUiFocusStyle`] selects the animation; everything
/// is integer-only flat quads, no allocation.
fn draw_focus_ring(
    node: UiResolvedNode,
    style: &LevelUiFocusStyle,
    frame: u16,
    shape: Option<UiShapeConfig>,
) {
    let width = node.width as i16;
    let height = node.height as i16;
    if width <= 0 || height <= 0 {
        return;
    }
    let thickness = i16::from(style.thickness.clamp(1, 4));
    let margin = i16::from(style.margin);
    match style.effect {
        LevelUiFocusEffect::Solid => {
            draw_ring_edges(node, width, height, thickness, margin, style.color_a);
        }
        LevelUiFocusEffect::Pulse => {
            let wave = focus_wave(frame, style.period);
            let color = lerp_color(style.color_a, style.color_b, wave);
            draw_ring_edges(node, width, height, thickness, margin, color);
        }
        LevelUiFocusEffect::Corners => {
            draw_focus_corners(node, width, height, thickness, margin, style, frame);
        }
        LevelUiFocusEffect::Tracer => {
            if let Some(config) = shape {
                if config.corners != 0 && config.cut != 0 {
                    draw_focus_shape_tracer(node, thickness, margin, config, style, frame);
                    return;
                }
            }
            draw_focus_tracer(node, width, height, thickness, margin, style, frame);
        }
    }
}

/// Four-edge outline whose inner boundary sits `margin` pixels outside
/// the control and whose band extends `thickness` pixels further out.
/// `margin = 1, thickness = 1` reproduces the classic 1px ring.
fn draw_ring_edges(
    node: UiResolvedNode,
    width: i16,
    height: i16,
    thickness: i16,
    margin: i16,
    (r, g, b): (u8, u8, u8),
) {
    let e = margin + thickness - 1;
    let outer_w = width + 2 * e;
    draw_quad_flat(node.subrect(-e, -e, outer_w, thickness), r, g, b);
    draw_quad_flat(
        node.subrect(-e, height + margin - 1, outer_w, thickness),
        r,
        g,
        b,
    );
    draw_quad_flat(node.subrect(-e, 0, thickness, height), r, g, b);
    draw_quad_flat(
        node.subrect(width + margin - 1, 0, thickness, height),
        r,
        g,
        b,
    );
}

/// Triangle wave (0..=255..=0) over one `period`-frame cycle. A zero
/// period freezes the animation at its brightest phase.
fn focus_wave(frame: u16, period: u16) -> u8 {
    if period == 0 {
        return 255;
    }
    let pos = u32::from(frame % period);
    triangle_wave_u8(((pos * 512) / u32::from(period)) as u16)
}

/// Integer colour mix: `t = 255` selects `a`, `t = 0` selects `b`.
fn lerp_color(a: (u8, u8, u8), b: (u8, u8, u8), t: u8) -> (u8, u8, u8) {
    let t = i32::from(t);
    let mix = |x: u8, y: u8| ((i32::from(x) * t + i32::from(y) * (255 - t)) / 255) as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Targeting-reticle brackets: an L at each corner that breathes up to
/// 2px outward while its colour pulses `color_b -> color_a`.
fn draw_focus_corners(
    node: UiResolvedNode,
    width: i16,
    height: i16,
    thickness: i16,
    margin: i16,
    style: &LevelUiFocusStyle,
    frame: u16,
) {
    let wave = focus_wave(frame, style.period);
    let (r, g, b) = lerp_color(style.color_a, style.color_b, wave);
    let push = i16::from(wave / 96); // 0..=2px breathing
    let d = margin + push;
    let e = d + thickness - 1;
    // Arm length, clamped so opposing corners never merge.
    let len = i16::from(style.corner_len.max(2))
        .min(width / 2 + e)
        .min(height / 2 + e);
    let t = thickness;
    // Top-left.
    draw_quad_flat(node.subrect(-e, -e, len, t), r, g, b);
    draw_quad_flat(node.subrect(-e, -e, t, len), r, g, b);
    // Top-right.
    draw_quad_flat(node.subrect(width + e - len, -e, len, t), r, g, b);
    draw_quad_flat(node.subrect(width + d - 1, -e, t, len), r, g, b);
    // Bottom-left.
    draw_quad_flat(node.subrect(-e, height + d - 1, len, t), r, g, b);
    draw_quad_flat(node.subrect(-e, height + e - len, t, len), r, g, b);
    // Bottom-right.
    draw_quad_flat(
        node.subrect(width + e - len, height + d - 1, len, t),
        r,
        g,
        b,
    );
    draw_quad_flat(
        node.subrect(width + d - 1, height + e - len, t, len),
        r,
        g,
        b,
    );
}

/// Number of comet segments behind the tracer head; each one steps
/// the colour further from `color_a` toward the `color_b` base ring.
const TRACER_SEGMENTS: i32 = 6;

/// A faint base ring plus a bright head orbiting the perimeter with a
/// gradient tail. One lap per `period` frames, clockwise.
fn draw_focus_tracer(
    node: UiResolvedNode,
    width: i16,
    height: i16,
    thickness: i16,
    margin: i16,
    style: &LevelUiFocusStyle,
    frame: u16,
) {
    draw_ring_edges(node, width, height, thickness, margin, style.color_b);
    if style.period == 0 {
        return;
    }
    let e = i32::from(margin + thickness - 1);
    let outer_w = i32::from(width) + 2 * e;
    let outer_h = i32::from(height) + 2 * e;
    let perimeter = 2 * (outer_w + outer_h);
    if perimeter <= 0 {
        return;
    }
    let pos = i32::from(frame % style.period);
    let head = (pos * perimeter) / i32::from(style.period);
    let seg = (perimeter / 28).max(2);
    for k in 0..TRACER_SEGMENTS {
        let brightness = (255 - k * 255 / TRACER_SEGMENTS) as u8;
        let color = lerp_color(style.color_a, style.color_b, brightness);
        let start = (head - (k + 1) * seg).rem_euclid(perimeter);
        draw_perimeter_run(
            node, width, height, thickness, margin, perimeter, outer_w, outer_h, start, seg, color,
        );
    }
}

/// Shape-aware tracer for chamfered buttons. Each thickness layer is a
/// parallel convex outline, and the bright comet is split at polygon
/// vertices so its head and tail traverse the 45-degree edges instead of
/// orbiting an invisible rectangle.
fn draw_focus_shape_tracer(
    node: UiResolvedNode,
    thickness: i16,
    margin: i16,
    config: UiShapeConfig,
    style: &LevelUiFocusStyle,
    frame: u16,
) {
    for layer in 0..thickness {
        let expansion = margin.saturating_add(layer);
        let polygon = shape_polygon(node, config.corners, config.cut, -i32::from(expansion));
        draw_shape_outline(&polygon, style.color_b);
        if style.period == 0 {
            continue;
        }
        let perimeter = shape_perimeter(&polygon);
        if perimeter <= 0 {
            continue;
        }
        let head = (i32::from(frame % style.period) * perimeter) / i32::from(style.period);
        let segment_len = (perimeter / 28).max(2);
        for tail in 0..TRACER_SEGMENTS {
            let brightness = (255 - tail * 255 / TRACER_SEGMENTS) as u8;
            let color = lerp_color(style.color_a, style.color_b, brightness);
            let start = (head - (tail + 1) * segment_len).rem_euclid(perimeter);
            draw_shape_perimeter_run(&polygon, perimeter, start, segment_len, color);
        }
    }
}

fn draw_shape_outline(polygon: &UiShapePolygon, (r, g, b): (u8, u8, u8)) {
    if polygon.len < 2 {
        return;
    }
    for index in 0..polygon.len {
        let next = (index + 1) % polygon.len;
        let from = polygon.vertices[index].screen;
        let to = polygon.vertices[next].screen;
        draw_line_mono(from.0, from.1, to.0, to.1, r, g, b);
    }
}

fn shape_edge_len(from: UiShapeVertex, to: UiShapeVertex) -> i32 {
    let dx = (to.local.0 - from.local.0).abs();
    let dy = (to.local.1 - from.local.1).abs();
    dx.max(dy)
}

fn shape_perimeter(polygon: &UiShapePolygon) -> i32 {
    let mut perimeter = 0;
    for index in 0..polygon.len {
        perimeter += shape_edge_len(
            polygon.vertices[index],
            polygon.vertices[(index + 1) % polygon.len],
        );
    }
    perimeter
}

fn shape_edge_point(
    from: UiShapeVertex,
    to: UiShapeVertex,
    offset: i32,
    length: i32,
) -> (i16, i16) {
    if length <= 0 {
        return from.screen;
    }
    let interpolate = |start: i16, end: i16| {
        let start = i32::from(start);
        (start + (i32::from(end) - start) * offset / length) as i16
    };
    (
        interpolate(from.screen.0, to.screen.0),
        interpolate(from.screen.1, to.screen.1),
    )
}

fn draw_shape_perimeter_run(
    polygon: &UiShapePolygon,
    perimeter: i32,
    mut start: i32,
    mut length: i32,
    (r, g, b): (u8, u8, u8),
) {
    if polygon.len < 2 || perimeter <= 0 {
        return;
    }
    while length > 0 {
        start = start.rem_euclid(perimeter);
        let mut edge_start = start;
        let mut edge_index = 0;
        let mut edge_length = 0;
        for index in 0..polygon.len {
            let candidate = shape_edge_len(
                polygon.vertices[index],
                polygon.vertices[(index + 1) % polygon.len],
            );
            if edge_start < candidate {
                edge_index = index;
                edge_length = candidate;
                break;
            }
            edge_start -= candidate;
        }
        if edge_length <= 0 {
            return;
        }
        let run = length.min(edge_length - edge_start);
        let from_vertex = polygon.vertices[edge_index];
        let to_vertex = polygon.vertices[(edge_index + 1) % polygon.len];
        let from = shape_edge_point(from_vertex, to_vertex, edge_start, edge_length);
        let to = shape_edge_point(from_vertex, to_vertex, edge_start + run, edge_length);
        draw_line_mono(from.0, from.1, to.0, to.1, r, g, b);
        start += run;
        length -= run;
    }
}

/// Draw `len` perimeter pixels starting at clockwise offset `start`
/// along the focus ring band (top edge left-to-right, then right edge
/// down, bottom right-to-left, left edge up), splitting across edges
/// and the wrap point as needed.
fn draw_perimeter_run(
    node: UiResolvedNode,
    width: i16,
    height: i16,
    thickness: i16,
    margin: i16,
    perimeter: i32,
    outer_w: i32,
    outer_h: i32,
    mut start: i32,
    mut len: i32,
    (r, g, b): (u8, u8, u8),
) {
    let e = i32::from(margin + thickness - 1);
    let d = margin;
    while len > 0 {
        start = start.rem_euclid(perimeter);
        let (edge_off, edge_len, edge) = if start < outer_w {
            (start, outer_w, 0)
        } else if start < outer_w + outer_h {
            (start - outer_w, outer_h, 1)
        } else if start < 2 * outer_w + outer_h {
            (start - outer_w - outer_h, outer_w, 2)
        } else {
            (start - 2 * outer_w - outer_h, outer_h, 3)
        };
        let run = len.min(edge_len - edge_off);
        if run <= 0 {
            return;
        }
        let run16 = run as i16;
        let off16 = edge_off as i16;
        let e16 = e as i16;
        match edge {
            // Top, left-to-right.
            0 => draw_quad_flat(node.subrect(-e16 + off16, -e16, run16, thickness), r, g, b),
            // Right, top-to-bottom.
            1 => draw_quad_flat(
                node.subrect(width + d - 1, -e16 + off16, thickness, run16),
                r,
                g,
                b,
            ),
            // Bottom, right-to-left.
            2 => draw_quad_flat(
                node.subrect(
                    width + e16 - off16 - run16,
                    height + d - 1,
                    run16,
                    thickness,
                ),
                r,
                g,
                b,
            ),
            // Left, bottom-to-top.
            _ => draw_quad_flat(
                node.subrect(-e16, height + e16 - off16 - run16, thickness, run16),
                r,
                g,
                b,
            ),
        }
        start += run;
        len -= run;
    }
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

/// Resolve a node's [`LevelUiNodeRecord::font`] selector against the font
/// table. Returns `fonts[selector]`, falling back to `fonts[0]` when the
/// selector is out of range, or `None` when the table is empty (text is then
/// skipped). One indirection, no allocation.
fn node_font<'f>(fonts: &[Option<&'f FontAtlas>], selector: u8) -> Option<&'f FontAtlas> {
    let index = font_index(fonts.len(), selector)?;
    fonts[index].or_else(|| if index == 0 { None } else { fonts[0] })
}

/// Pure index-selection rule behind [`node_font`], split out so it is testable
/// without constructing a real (GPU-backed) [`FontAtlas`]. Given a font-table
/// length and a node's selector, returns the index to draw with: the selector
/// itself when in range, else `0` (fallback to the default font), else `None`
/// for an empty table.
fn font_index(table_len: usize, selector: u8) -> Option<usize> {
    if table_len == 0 {
        None
    } else if (selector as usize) < table_len {
        Some(selector as usize)
    } else {
        Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        activation_echo_layer, bar_frame_index, bar_frame_v_range, diamond_quad,
        focus_chrome_sweep_paint, focus_sweep_rect, font_index, font_tint,
        image_effect_vertex_colors, image_effect_verts, message_panel_layout,
        message_panel_transition_layout, node_absolute_rect, scale_u16_q8, selected_label_text,
        shape_config, shape_edge_point, shape_perimeter, shape_polygon, text_prefix_chars,
        MessagePageMeta, MessagePanelLayout, MessagePanelVariant, UiPaint, UiShapeConfig,
        ACTIVATION_ECHO_FRAMES, MESSAGE_PANEL_EXPAND_FRAMES,
    };
    use psx_level::{
        ui_node_flags, AssetId, LevelUiAction, LevelUiGradientDirection, LevelUiImageEffect,
        LevelUiNodeKind, LevelUiNodeRecord, LevelUiValueBinding, UI_OPTION_NONE, UI_PAINT_NONE,
        UI_SFX_NONE,
    };

    fn ui_node(
        parent: Option<psx_level::UiNodeIndex>,
        kind: LevelUiNodeKind,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    ) -> LevelUiNodeRecord {
        LevelUiNodeRecord {
            parent,
            kind,
            x,
            y,
            width,
            height,
            color: [0, 0, 0],
            background: [0, 0, 0],
            accent: [0, 0, 0],
            color_paint: UI_PAINT_NONE,
            background_paint: UI_PAINT_NONE,
            accent_paint: UI_PAINT_NONE,
            value: LevelUiValueBinding::ConstantQ12(0),
            max: LevelUiValueBinding::ConstantQ12(0),
            texture_asset: AssetId(u16::MAX),
            image_effect: LevelUiImageEffect::None,
            text: "",
            tag: "",
            action: LevelUiAction::Back,
            option: UI_OPTION_NONE,
            rotation_degrees: 0,
            flags: 0,
            sfx_first: UI_SFX_NONE,
            sfx_count: 0,
            font: 0,
            font_scale: 256,
            letter_spacing: 0,
        }
    }

    #[test]
    fn font_index_picks_selector_when_in_range() {
        assert_eq!(font_index(3, 0), Some(0));
        assert_eq!(font_index(3, 1), Some(1));
        assert_eq!(font_index(3, 2), Some(2));
    }

    #[test]
    fn font_index_falls_back_to_zero_when_out_of_range() {
        // A node asks for font 5 but the table only has 2: use the default.
        assert_eq!(font_index(2, 5), Some(0));
    }

    #[test]
    fn font_index_is_none_for_empty_table() {
        // No fonts uploaded: text is skipped entirely.
        assert_eq!(font_index(0, 0), None);
        assert_eq!(font_index(0, 7), None);
    }

    #[test]
    fn activation_echo_staggers_and_expires_both_contours() {
        let first = activation_echo_layer(0, 0, 0).expect("first contour starts immediately");
        assert_eq!(first.0, 1);
        assert!(first.1 > 0);

        assert_eq!(activation_echo_layer(1, 2, 1), None);
        let second = activation_echo_layer(2, 2, 1).expect("second contour starts after delay");
        assert_eq!(second.0, 2);
        assert!(second.1 > 0);

        assert_eq!(activation_echo_layer(ACTIVATION_ECHO_FRAMES, 0, 0), None);
        assert_eq!(activation_echo_layer(ACTIVATION_ECHO_FRAMES, 2, 1), None);
    }

    #[test]
    fn focused_chrome_fill_sweeps_bright_end_left_then_right() {
        let authored = UiPaint::Gradient {
            from: (10, 20, 30),
            to: (110, 120, 130),
            direction: LevelUiGradientDirection::Vertical,
        };
        assert_eq!(
            focus_chrome_sweep_paint(authored, 0, 100),
            UiPaint::Gradient {
                from: (110, 120, 130),
                to: (10, 20, 30),
                direction: LevelUiGradientDirection::Horizontal,
            }
        );
        assert_eq!(
            focus_chrome_sweep_paint(authored, 50, 100),
            UiPaint::Gradient {
                from: (10, 20, 30),
                to: (110, 120, 130),
                direction: LevelUiGradientDirection::Horizontal,
            }
        );
    }

    #[test]
    fn focused_chrome_scanner_travels_between_clipped_corner_insets() {
        let config = UiShapeConfig {
            corners: 5,
            cut: 4,
            border: 1,
            transparent: false,
            semi_transparent_fill: true,
        };
        assert_eq!(
            focus_sweep_rect(126, 16, config, 0, 100),
            Some((4, 1, 25, 14))
        );
        assert_eq!(
            focus_sweep_rect(126, 16, config, 50, 100),
            Some((97, 1, 25, 14))
        );
    }

    #[test]
    fn archive_message_variants_fit_the_ps1_safe_area_and_line_budgets() {
        let poi = message_panel_layout(MessagePanelVariant::PointOfInterest);
        assert_eq!(
            (poi.x, poi.y, poi.width, poi.height, poi.max_lines),
            (28, 184, 264, 42, 2)
        );
        assert!(i32::from(poi.y) + i32::from(poi.height) <= 240);

        let world = message_panel_layout(MessagePanelVariant::World);
        assert_eq!(
            (world.x, world.y, world.width, world.height, world.max_lines),
            (28, 92, 264, 56, 3)
        );
        assert_eq!(i32::from(world.y) * 2 + i32::from(world.height), 240);
    }

    #[test]
    fn archive_prompt_morph_preserves_its_centre_and_bottom_edge() {
        let prompt = MessagePanelLayout {
            x: 122,
            y: 209,
            width: 76,
            height: 17,
            max_lines: 1,
        };
        let message = message_panel_layout(MessagePanelVariant::PointOfInterest);
        assert_eq!(message_panel_transition_layout(prompt, message, 0), prompt);
        assert_eq!(
            message_panel_transition_layout(prompt, message, MESSAGE_PANEL_EXPAND_FRAMES),
            message
        );

        let halfway =
            message_panel_transition_layout(prompt, message, MESSAGE_PANEL_EXPAND_FRAMES / 2);
        assert_eq!(i32::from(halfway.x) * 2 + i32::from(halfway.width), 320);
        assert_eq!(i32::from(halfway.y) + i32::from(halfway.height), 226);
        assert!(halfway.width > prompt.width && halfway.width < message.width);
        assert!(halfway.height > prompt.height && halfway.height < message.height);
    }

    #[test]
    fn archive_typewriter_never_slices_inside_a_utf8_character() {
        let text = "A·Ω";
        assert_eq!(text_prefix_chars(text, 0), "");
        assert_eq!(text_prefix_chars(text, 1), "A");
        assert_eq!(text_prefix_chars(text, 2), "A·");
        assert_eq!(text_prefix_chars(text, 3), text);
        assert_eq!(text_prefix_chars(text, 99), text);
    }

    #[test]
    fn archive_page_metadata_never_addresses_outside_the_page_set() {
        assert_eq!(
            MessagePageMeta::new(0, 0),
            MessagePageMeta { index: 0, count: 1 }
        );
        assert_eq!(
            MessagePageMeta::new(9, 3),
            MessagePageMeta { index: 2, count: 3 }
        );
        assert_eq!(
            MessagePageMeta::new(1, 3),
            MessagePageMeta { index: 1, count: 3 }
        );
    }

    #[test]
    fn sprite_bar_frame_mapping_reaches_empty_and_full_terminal_frames() {
        assert_eq!(bar_frame_index(0, 4096, 7), 0);
        assert_eq!(bar_frame_index(1, 4096, 7), 1);
        assert_eq!(bar_frame_index(2048, 4096, 7), 3);
        assert_eq!(bar_frame_index(4095, 4096, 7), 6);
        assert_eq!(bar_frame_index(4096, 4096, 7), 6);
        assert_eq!(bar_frame_index(8192, 4096, 7), 6);
        assert_eq!(bar_frame_v_range(203, 7, 0), Some((0, 28)));
        assert_eq!(bar_frame_v_range(203, 7, 6), Some((174, 202)));
    }

    #[test]
    fn clipped_shape_decodes_and_builds_the_expected_six_vertex_outline() {
        let mut nodes = [
            ui_node(None, LevelUiNodeKind::Canvas, 0, 0, 320, 240),
            ui_node(
                Some(psx_level::UiNodeIndex(0)),
                LevelUiNodeKind::Rect,
                10,
                20,
                100,
                30,
            ),
        ];
        nodes[1].option = psx_level::ui_shape::encode(
            psx_level::ui_shape::TOP_LEFT | psx_level::ui_shape::BOTTOM_RIGHT,
            6,
            2,
            true,
            true,
        );
        let resolved = super::node_resolved(&nodes, 1).expect("resolved shape");
        let config = shape_config(&nodes[1]);
        assert_eq!(config.corners, 0b0101);
        assert_eq!(config.cut, 6);
        assert_eq!(config.border, 2);
        assert!(config.transparent);
        assert!(config.semi_transparent_fill);

        let outer = shape_polygon(resolved, config.corners, config.cut, 0);
        assert_eq!(outer.len, 6);
        let expected_outer = [(6, 0), (100, 0), (100, 24), (94, 30), (0, 30), (0, 6)];
        for (vertex, expected) in outer.vertices[..outer.len].iter().zip(expected_outer) {
            assert_eq!(vertex.local, expected);
        }
        let inner = shape_polygon(resolved, config.corners, config.cut, 2);
        let expected_inner = [(8, 2), (98, 2), (98, 22), (92, 28), (2, 28), (2, 8)];
        for (vertex, expected) in inner.vertices[..inner.len].iter().zip(expected_inner) {
            assert_eq!(vertex.local, expected);
        }
    }

    #[test]
    fn chamfered_focus_perimeter_includes_both_diagonal_edges() {
        let nodes = [
            ui_node(None, LevelUiNodeKind::Canvas, 0, 0, 320, 240),
            ui_node(
                Some(psx_level::UiNodeIndex(0)),
                LevelUiNodeKind::Button,
                10,
                20,
                100,
                30,
            ),
        ];
        let resolved = super::node_resolved(&nodes, 1).expect("resolved shape");
        let polygon = shape_polygon(
            resolved,
            psx_level::ui_shape::TOP_LEFT | psx_level::ui_shape::BOTTOM_RIGHT,
            6,
            -2,
        );
        let expected = [(4, -2), (102, -2), (102, 26), (96, 32), (-2, 32), (-2, 4)];
        for (vertex, expected) in polygon.vertices[..polygon.len].iter().zip(expected) {
            assert_eq!(vertex.local, expected);
        }
        assert_eq!(shape_perimeter(&polygon), 264);
        assert_eq!(
            shape_edge_point(polygon.vertices[2], polygon.vertices[3], 3, 6),
            (109, 49)
        );
    }

    #[test]
    fn node_absolute_rect_bounds_rotated_node() {
        let mut nodes = [
            ui_node(None, LevelUiNodeKind::Canvas, 0, 0, 320, 240),
            ui_node(
                Some(psx_level::UiNodeIndex(0)),
                LevelUiNodeKind::Rect,
                10,
                20,
                30,
                10,
            ),
        ];
        nodes[1].rotation_degrees = 90;

        assert_eq!(node_absolute_rect(&nodes, 1), (20, 10, 10, 30));
    }

    #[test]
    fn q8_font_scale_rounds_to_screen_pixels() {
        assert_eq!(scale_u16_q8(8, 256), 8);
        assert_eq!(scale_u16_q8(8, 384), 12);
        assert_eq!(scale_u16_q8(17, 384), 26);
    }

    #[test]
    fn font_tint_converts_authored_rgb_to_psx_modulation() {
        assert_eq!(font_tint([0, 1, 2]), (0, 1, 1));
        assert_eq!(font_tint([148, 136, 255]), (74, 68, 128));
    }

    #[test]
    fn image_effect_vertex_colors_animate_presets() {
        let base = (128, 128, 128);
        let verts = [(0, 0), (160, 0), (0, 100), (160, 100)];
        assert_eq!(
            image_effect_vertex_colors(base, LevelUiImageEffect::None, 0, verts),
            [base; 4]
        );
        assert_ne!(
            image_effect_vertex_colors(base, LevelUiImageEffect::Shimmer, 0, verts),
            image_effect_vertex_colors(base, LevelUiImageEffect::Shimmer, 64, verts)
        );
        assert_eq!(
            image_effect_vertex_colors(base, LevelUiImageEffect::SoftPulse, 0, verts)[0],
            image_effect_vertex_colors(base, LevelUiImageEffect::SoftPulse, 0, verts)[3]
        );
    }

    #[test]
    fn rise_effect_moves_up_and_random_label_choice_is_stable() {
        let verts = [(10, 120), (13, 120), (10, 123), (13, 123)];
        let risen = image_effect_verts(verts, LevelUiImageEffect::Rise, 24);
        assert!(risen[0].1 <= verts[0].1);
        assert_eq!(risen[0].0, verts[0].0);
        assert_eq!(
            diamond_quad([(0, 0), (4, 0), (0, 4), (4, 4)]),
            [(2, 0), (4, 2), (0, 2), (2, 4)]
        );
        let wind_start = image_effect_verts(verts, LevelUiImageEffect::Wind, 0);
        let wind_next = image_effect_verts(verts, LevelUiImageEffect::Wind, 1);
        assert!(wind_next[0].0 > wind_start[0].0);
        assert!(wind_next[0].0 - wind_start[0].0 > (wind_next[0].1 - wind_start[0].1).abs());

        let mut label = ui_node(None, LevelUiNodeKind::Label, 0, 0, 100, 20);
        label.flags |= ui_node_flags::TEXT_RANDOM_MESSAGE;
        label.text = "one\u{1f}two\u{1f}three";
        let first = selected_label_text(&label, 0x1234);
        assert_eq!(first, selected_label_text(&label, 0x1234));
        assert!(["one", "two", "three"].contains(&first));
    }

    #[test]
    fn image_effect_sweep_is_continuous_across_split_fragments() {
        let base = (128, 128, 128);
        let left = [(0, 0), (160, 0), (0, 100), (160, 100)];
        let right = [(160, 0), (320, 0), (160, 100), (320, 100)];
        let left_colors = image_effect_vertex_colors(base, LevelUiImageEffect::Shimmer, 48, left);
        let right_colors = image_effect_vertex_colors(base, LevelUiImageEffect::Shimmer, 48, right);
        assert_eq!(left_colors[1], right_colors[0]);
        assert_eq!(left_colors[3], right_colors[2]);
    }
}

/// Resolve a node's absolute on-screen rectangle, applying the
/// parent chain and 9-point anchor. Falls back to a 1x1 rect at the
/// origin for an out-of-range index.
fn node_absolute_rect(nodes: &[LevelUiNodeRecord], index: usize) -> (i16, i16, u16, u16) {
    node_resolved(nodes, index)
        .map(UiResolvedNode::bounds)
        .unwrap_or((0, 0, 1, 1))
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

const UI_FONT_SCALE_ONE_Q8: u16 = 256;
const UI_FONT_SCALE_MIN_Q8: u16 = UI_FONT_SCALE_ONE_Q8 / 2;
const UI_FONT_SCALE_MAX_Q8: u16 = UI_FONT_SCALE_ONE_Q8 * 8;

fn node_font_scale_q8(node: &LevelUiNodeRecord) -> u16 {
    node.font_scale
        .clamp(UI_FONT_SCALE_MIN_Q8, UI_FONT_SCALE_MAX_Q8)
}

fn node_letter_spacing(node: &LevelUiNodeRecord) -> i8 {
    node.letter_spacing
}

fn scale_u16_q8(value: u16, scale_q8: u16) -> u16 {
    let scaled = u32::from(value)
        .saturating_mul(u32::from(scale_q8))
        .saturating_add(128)
        >> 8;
    scaled.min(u32::from(u16::MAX)) as u16
}

fn scaled_text_width(font: &FontAtlas, text: &str, scale_q8: u16, letter_spacing: i8) -> u16 {
    let base = i32::from(scale_u16_q8(font.text_width(text), scale_q8));
    let gaps = text
        .chars()
        .count()
        .saturating_sub(1)
        .min(i32::MAX as usize) as i32;
    let spacing = gaps.saturating_mul(i32::from(letter_spacing));
    base.saturating_add(spacing).clamp(0, i32::from(u16::MAX)) as u16
}

fn scaled_line_height(font: &FontAtlas, scale_q8: u16) -> i16 {
    scale_u16_q8(font.line_height(), scale_q8)
        .max(1)
        .min(i16::MAX as u16) as i16
}

fn draw_scaled_text(
    font: &FontAtlas,
    x: i16,
    y: i16,
    text: &str,
    scale_q8: u16,
    letter_spacing: i8,
    tint: (u8, u8, u8),
) {
    if scale_q8 == UI_FONT_SCALE_ONE_Q8 {
        font.draw_text_with_spacing(x, y, text, letter_spacing, tint);
    } else {
        font.draw_text_scaled_with_spacing_q8(x, y, text, scale_q8, scale_q8, letter_spacing, tint);
    }
}

fn draw_scaled_text_paint(
    font: &FontAtlas,
    x: i16,
    y: i16,
    text: &str,
    scale_q8: u16,
    letter_spacing: i8,
    paint: UiPaint,
) {
    match paint {
        UiPaint::Solid(tint) => draw_scaled_text(font, x, y, text, scale_q8, letter_spacing, tint),
        UiPaint::Gradient {
            from,
            to,
            direction,
        } => font.draw_text_scaled_gouraud_with_spacing_q8(
            x,
            y,
            text,
            scale_q8,
            scale_q8,
            letter_spacing,
            gradient_vertex_colors(from, to, direction),
        ),
    }
}

fn draw_transformed_text_paint(
    font: &FontAtlas,
    resolved: UiResolvedNode,
    x: i16,
    y: i16,
    text: &str,
    scale_q8: u16,
    letter_spacing: i8,
    paint: UiPaint,
) {
    if resolved.transform.m00 == 4096
        && resolved.transform.m01 == 0
        && resolved.transform.m10 == 0
        && resolved.transform.m11 == 4096
    {
        let origin = resolved.transform.point(i32::from(x), i32::from(y));
        draw_scaled_text_paint(
            font,
            origin.0,
            origin.1,
            text,
            scale_q8,
            letter_spacing,
            paint,
        );
        return;
    }

    let origin = resolved.transform.point(i32::from(x), i32::from(y));
    font.draw_text_affine(
        origin,
        text,
        resolved.transform.font_matrix(scale_q8),
        paint.from(),
    );
}

fn draw_label(
    font: &FontAtlas,
    node: &LevelUiNodeRecord,
    resolved: UiResolvedNode,
    paints: &[LevelUiPaintRecord],
    text_seed: u16,
    frame: u16,
    text_override: Option<&str>,
) {
    let text = text_override.unwrap_or_else(|| selected_label_text(node, text_seed));
    let paint = text_paint(node.color, node.color_paint, paints);
    let scale = node_font_scale_q8(node);
    let letter_spacing = node_letter_spacing(node);
    let align = (node.flags & ui_node_flags::TEXT_ALIGN_MASK) >> ui_node_flags::TEXT_ALIGN_SHIFT;
    if node.flags & ui_node_flags::TEXT_WRAP == 0 {
        let text_x = aligned_text_x(font, text, 0, resolved.width, align, scale, letter_spacing);
        // Shimmer / FastShimmer on a Label is the boot-tag idiom: the
        // sweeping sheen every hand-rolled "Built with PSoXide" intro
        // (celeste collection, voxide) draws, so authored splash scenes
        // can carry the same line. Single-line, untransformed labels
        // only; wrapped or rotated text keeps the static paint.
        if draw_label_sheen(
            font,
            node.image_effect,
            resolved,
            text_x,
            text,
            scale,
            letter_spacing,
            paint,
            frame,
        ) {
            return;
        }
        draw_transformed_text_paint(
            font,
            resolved,
            text_x,
            0,
            text,
            scale,
            letter_spacing,
            paint,
        );
        return;
    }

    let mut start = 0usize;
    let mut line_y = 0i16;
    while start < text.len() {
        while matches!(text.as_bytes().get(start), Some(b' ' | b'\n')) {
            start += 1;
        }
        if start >= text.len() {
            break;
        }
        let end = wrapped_line_end(font, text, start, resolved.width, scale, letter_spacing);
        let line = &text[start..end];
        let text_x = aligned_text_x(font, line, 0, resolved.width, align, scale, letter_spacing);
        draw_transformed_text_paint(
            font,
            resolved,
            text_x,
            line_y,
            line,
            scale,
            letter_spacing,
            paint,
        );
        line_y = line_y.saturating_add(scaled_line_height(font, scale));
        start = end;
    }
}

/// Draw a single-line label with a sweeping sheen when its
/// `image_effect` asks for one. Returns false when this label does not
/// sheen (wrong effect, or a non-identity transform) so the caller
/// falls back to the static paint path.
///
/// The math is the celeste-collection / voxide boot intro's, verbatim:
/// a highlight head sweeps `char_count + 18` positions, each glyph
/// brightens by `max(0, 18 - |i - head| * 6) / 18` toward PSX
/// texture-modulation white (128 = full brightness, which is what the
/// hand-rolled intros converge on during their hold phase). Shimmer
/// advances the head every other frame like the originals;
/// FastShimmer every frame.
fn draw_label_sheen(
    font: &FontAtlas,
    effect: LevelUiImageEffect,
    resolved: UiResolvedNode,
    text_x: i16,
    text: &str,
    scale_q8: u16,
    letter_spacing: i8,
    paint: UiPaint,
    frame: u16,
) -> bool {
    let head = {
        let t = i32::from(frame);
        let span = text.chars().count() as i32 + 18;
        match effect {
            LevelUiImageEffect::Shimmer => (t / 2).rem_euclid(span),
            LevelUiImageEffect::FastShimmer => t.rem_euclid(span),
            _ => return false,
        }
    };
    if resolved.transform.m00 != 4096
        || resolved.transform.m01 != 0
        || resolved.transform.m10 != 0
        || resolved.transform.m11 != 4096
    {
        return false;
    }
    let origin = resolved.transform.point(i32::from(text_x), 0);
    // Per-glyph draws at a self-accumulated Q8 cursor: the same advance
    // the font's own string loop uses, so kerning is identical to the
    // static path and the sheen cannot drift the layout.
    let mut cursor_q8 = i32::from(origin.0) << 8;
    let gap_q8 = i32::from(letter_spacing) << 8;
    for (i, ch) in text.chars().enumerate() {
        let k = (18 - (i as i32 - head).abs() * 6).max(0);
        let x = ((cursor_q8 + 128) >> 8) as i16;
        let mut buf = [0u8; 4];
        draw_scaled_text_paint(
            font,
            x,
            origin.1,
            ch.encode_utf8(&mut buf),
            scale_q8,
            letter_spacing,
            sheened_paint(paint, k),
        );
        cursor_q8 = cursor_q8
            .saturating_add(i32::from(font.font().glyph_advance(ch)) * i32::from(scale_q8))
            .saturating_add(gap_q8);
    }
    true
}

/// Push a resolved (already font-tinted) paint toward modulation white
/// by `k / 18`.
fn sheened_paint(paint: UiPaint, k: i32) -> UiPaint {
    if k <= 0 {
        return paint;
    }
    let mix = |color: (u8, u8, u8)| {
        let f = |v: u8| (i32::from(v) + (128 - i32::from(v)) * k / 18) as u8;
        (f(color.0), f(color.1), f(color.2))
    };
    match paint {
        UiPaint::Solid(color) => UiPaint::Solid(mix(color)),
        UiPaint::Gradient {
            from,
            to,
            direction,
        } => UiPaint::Gradient {
            from: mix(from),
            to: mix(to),
            direction,
        },
    }
}

fn selected_label_text(node: &LevelUiNodeRecord, seed: u16) -> &str {
    if node.flags & ui_node_flags::TEXT_RANDOM_MESSAGE == 0 {
        return node.text;
    }
    let count = node.text.bytes().filter(|byte| *byte == 0x1f).count() + 1;
    let mut mixed = seed ^ 0xa361;
    mixed ^= mixed << 7;
    mixed ^= mixed >> 9;
    mixed ^= mixed << 8;
    node.text
        .split('\u{1f}')
        .nth(usize::from(mixed) % count)
        .unwrap_or(node.text)
}

fn wrapped_line_end(
    font: &FontAtlas,
    text: &str,
    start: usize,
    width: u16,
    scale: u16,
    letter_spacing: i8,
) -> usize {
    if start >= text.len() || !text.is_char_boundary(start) {
        return start;
    }
    let mut last_space = None;
    for (offset, ch) in text[start..].char_indices() {
        let end = start + offset;
        if ch == '\n' {
            return end;
        }
        let next = end + ch.len_utf8();
        if ch == ' ' {
            last_space = Some(end);
        }
        if next > start
            && scaled_text_width(font, &text[start..next], scale, letter_spacing) > width
        {
            return last_space
                .filter(|space| *space > start)
                .unwrap_or(if end > start { end } else { next });
        }
    }
    text.len()
}

fn aligned_text_x(
    font: &FontAtlas,
    text: &str,
    x: i16,
    width: u16,
    align: u16,
    scale: u16,
    letter_spacing: i8,
) -> i16 {
    let text_w = scaled_text_width(font, text, scale, letter_spacing) as i32;
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
    resolved: UiResolvedNode,
    frame: u16,
    textures: &mut impl FnMut(AssetId) -> Option<UiTextureSlot>,
) {
    let verts = image_effect_verts(resolved.verts, node.image_effect, frame);
    if node.texture_asset.0 == u16::MAX {
        let verts = if node.image_effect == LevelUiImageEffect::Rise {
            diamond_quad(verts)
        } else {
            verts
        };
        if matches!(
            node.image_effect,
            LevelUiImageEffect::Shimmer
                | LevelUiImageEffect::FastShimmer
                | LevelUiImageEffect::DiagonalSweep
                | LevelUiImageEffect::SoftPulse
        ) {
            let colors =
                image_effect_vertex_colors(rgb(node.color), node.image_effect, frame, verts);
            draw_tri_gouraud(
                [verts[0], verts[1], verts[2]],
                [colors[0], colors[1], colors[2]],
            );
            draw_tri_gouraud(
                [verts[1], verts[2], verts[3]],
                [colors[1], colors[2], colors[3]],
            );
        } else {
            draw_quad_flat(verts, node.color[0], node.color[1], node.color[2]);
        }
        return;
    }
    let Some(slot) = textures(node.texture_asset) else {
        return;
    };
    let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, rgb(node.color))
        .with_texture_window(slot.texture_window);
    let tex_w = texture_size_u8(slot.texture_width).saturating_sub(1);
    let tex_h = texture_size_u8(slot.texture_height).saturating_sub(1);
    let uvs = [(0, 0), (tex_w, 0), (0, tex_h), (tex_w, tex_h)];
    if node.image_effect == LevelUiImageEffect::None {
        draw_quad_textured_material(verts, uvs, material);
    } else {
        draw_quad_textured_gouraud_material(
            verts,
            uvs,
            image_effect_vertex_colors(rgb(node.color), node.image_effect, frame, verts),
            material,
        );
    }
}

fn diamond_quad(verts: [(i16, i16); 4]) -> [(i16, i16); 4] {
    let midpoint = |a: (i16, i16), b: (i16, i16)| {
        (
            ((i32::from(a.0) + i32::from(b.0)) / 2) as i16,
            ((i32::from(a.1) + i32::from(b.1)) / 2) as i16,
        )
    };
    [
        midpoint(verts[0], verts[1]),
        midpoint(verts[1], verts[3]),
        midpoint(verts[0], verts[2]),
        midpoint(verts[2], verts[3]),
    ]
}

/// Clamp a texel dimension into the GP0 8-bit UV range.
fn texture_size_u8(size: u16) -> u8 {
    size.min(u16::from(u8::MAX)) as u8
}

/// Per-effect vertex displacement, applied before drawing. Colour-only
/// effects pass through; `Bob` oscillates the whole quad vertically by
/// a few pixels on a triangle wave (the loading-screen mascot idiom).
fn image_effect_verts(
    verts: [(i16, i16); 4],
    effect: LevelUiImageEffect,
    frame: u16,
) -> [(i16, i16); 4] {
    match effect {
        LevelUiImageEffect::Bob => {
            const BOB_AMPLITUDE: i16 = 4;
            let wave = i16::from(triangle_wave_u8(frame.wrapping_mul(2)));
            let offset = ((wave - 128) * BOB_AMPLITUDE) / 128;
            [
                (verts[0].0, verts[0].1.saturating_add(offset)),
                (verts[1].0, verts[1].1.saturating_add(offset)),
                (verts[2].0, verts[2].1.saturating_add(offset)),
                (verts[3].0, verts[3].1.saturating_add(offset)),
            ]
        }
        LevelUiImageEffect::Rise => {
            let origin = verts[0];
            let spatial_phase = origin
                .0
                .wrapping_mul(13)
                .wrapping_add(origin.1.wrapping_mul(7)) as u16;
            let phase = frame.wrapping_div(2).wrapping_add(spatial_phase) & 0x003f;
            let offset = -(phase as i16);
            [
                (verts[0].0, verts[0].1.saturating_add(offset)),
                (verts[1].0, verts[1].1.saturating_add(offset)),
                (verts[2].0, verts[2].1.saturating_add(offset)),
                (verts[3].0, verts[3].1.saturating_add(offset)),
            ]
        }
        LevelUiImageEffect::Wind => {
            let origin = verts[0];
            let seed = (origin.1 as u16)
                .wrapping_mul(11)
                .wrapping_add((origin.0 as u16).wrapping_mul(3));
            let phase = frame.wrapping_mul(2).wrapping_add(seed) & 0x007f;
            let gust = i16::from(triangle_wave_u8(frame.wrapping_mul(5).wrapping_add(seed))) - 128;
            let offset_x = (phase as i16).saturating_add(gust / 32);
            let offset_y = (-((phase as i16) / 6)).saturating_add(gust / 64);
            [
                (
                    verts[0].0.saturating_add(offset_x),
                    verts[0].1.saturating_add(offset_y),
                ),
                (
                    verts[1].0.saturating_add(offset_x),
                    verts[1].1.saturating_add(offset_y),
                ),
                (
                    verts[2].0.saturating_add(offset_x),
                    verts[2].1.saturating_add(offset_y),
                ),
                (
                    verts[3].0.saturating_add(offset_x),
                    verts[3].1.saturating_add(offset_y),
                ),
            ]
        }
        _ => verts,
    }
}

fn image_effect_vertex_colors(
    base: (u8, u8, u8),
    effect: LevelUiImageEffect,
    frame: u16,
    verts: [(i16, i16); 4],
) -> [(u8, u8, u8); 4] {
    match effect {
        LevelUiImageEffect::None => [base; 4],
        LevelUiImageEffect::Shimmer => sweep_colors(
            base,
            frame,
            3,
            88,
            [verts[0].0, verts[1].0, verts[2].0, verts[3].0],
        ),
        LevelUiImageEffect::FastShimmer => sweep_colors(
            base,
            frame,
            7,
            112,
            [verts[0].0, verts[1].0, verts[2].0, verts[3].0],
        ),
        LevelUiImageEffect::DiagonalSweep => sweep_colors(
            base,
            frame,
            4,
            96,
            [
                verts[0].0.saturating_add(verts[0].1 / 2),
                verts[1].0.saturating_add(verts[1].1 / 2),
                verts[2].0.saturating_add(verts[2].1 / 2),
                verts[3].0.saturating_add(verts[3].1 / 2),
            ],
        ),
        LevelUiImageEffect::SoftPulse => {
            let lift = 10 + (u16::from(triangle_wave_u8(frame.wrapping_mul(3))) * 44 / 255) as u8;
            [add_light(base, lift); 4]
        }
        // Bob displaces vertices (see `image_effect_verts`); colours
        // stay flat.
        LevelUiImageEffect::Bob => [base; 4],
        LevelUiImageEffect::Rise => [base; 4],
        LevelUiImageEffect::Wind => [base; 4],
    }
}

fn sweep_colors(
    base: (u8, u8, u8),
    frame: u16,
    speed: u16,
    intensity: u8,
    positions: [i16; 4],
) -> [(u8, u8, u8); 4] {
    let phase = ((frame.wrapping_mul(speed) & 0x01ff) as i16) - 128;
    [
        add_light(base, sweep_lift(positions[0], phase, intensity)),
        add_light(base, sweep_lift(positions[1], phase, intensity)),
        add_light(base, sweep_lift(positions[2], phase, intensity)),
        add_light(base, sweep_lift(positions[3], phase, intensity)),
    ]
}

fn sweep_lift(position: i16, phase: i16, intensity: u8) -> u8 {
    let distance = (position - phase).unsigned_abs();
    let falloff = (distance / 2).min(u16::from(u8::MAX)) as u8;
    intensity.saturating_sub(falloff)
}

fn triangle_wave_u8(value: u16) -> u8 {
    let phase = value & 0x01ff;
    if phase < 256 {
        phase as u8
    } else {
        (511 - phase) as u8
    }
}

fn add_light(color: (u8, u8, u8), lift: u8) -> (u8, u8, u8) {
    (
        color.0.saturating_add(lift),
        color.1.saturating_add(lift),
        color.2.saturating_add(lift),
    )
}

fn draw_status_bar(
    resolved: UiResolvedNode,
    value: i32,
    max_value: i32,
    fill: UiPaint,
    background: UiPaint,
) {
    let width = resolved.width as i16;
    let height = resolved.height as i16;
    draw_quad_flat(resolved.subrect(-1, -1, width + 2, height + 2), 12, 14, 18);
    draw_quad_paint(resolved.verts, background);

    let fill_width = status_fill_width(width, value, max_value);
    if fill_width > 0 {
        draw_quad_paint(resolved.subrect(0, 0, fill_width, height), fill);
        if height > 3 && fill.is_solid() {
            let color = brighten(fill.from());
            draw_quad_flat(
                resolved.subrect(0, 0, fill_width, 1),
                color.0,
                color.1,
                color.2,
            );
        }
    }
}

fn draw_sprite_bar(
    node: &LevelUiNodeRecord,
    resolved: UiResolvedNode,
    value: i32,
    max_value: i32,
    textures: &mut impl FnMut(AssetId) -> Option<UiTextureSlot>,
) {
    let Some(slot) = textures(node.texture_asset) else {
        return;
    };
    let frame_count = node.option.min(u16::from(u8::MAX)) as u8;
    let frame = bar_frame_index(value, max_value, frame_count);
    let Some((v0, v1)) = bar_frame_v_range(slot.texture_height, frame_count, frame) else {
        return;
    };
    let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, rgb(node.color))
        .with_texture_window(slot.texture_window);
    let u1 = texture_size_u8(slot.texture_width).saturating_sub(1);
    draw_quad_textured_material(
        resolved.verts,
        [(0, v0), (u1, v0), (0, v1), (u1, v1)],
        material,
    );
}

fn bar_frame_index(value: i32, max_value: i32, frame_count: u8) -> u8 {
    if max_value <= 0 || frame_count < 2 || value <= 0 {
        return 0;
    }
    let value = value.min(max_value);
    let last = i32::from(frame_count - 1);

    // Find the smallest frame whose exact value threshold contains `value`.
    // Writing floor(frame * max / last) as two bounded terms avoids the
    // overflow-prone wide multiply previously used by this UI-only helper.
    let max_whole = max_value / last;
    let max_remainder = max_value % last;
    let mut low = 1;
    let mut high = last;
    while low < high {
        let middle = low + ((high - low) >> 1);
        let threshold = max_whole * middle + (max_remainder * middle) / last;
        if value <= threshold {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low as u8
}

fn bar_frame_v_range(texture_height: u16, frame_count: u8, frame: u8) -> Option<(u8, u8)> {
    if frame_count < 2
        || frame >= frame_count
        || !texture_height.is_multiple_of(u16::from(frame_count))
    {
        return None;
    }
    let frame_height = texture_height / u16::from(frame_count);
    if frame_height == 0 {
        return None;
    }
    let v0 = u16::from(frame).checked_mul(frame_height)?;
    let v1 = v0.checked_add(frame_height)?.checked_sub(1)?;
    Some((u8::try_from(v0).ok()?, u8::try_from(v1).ok()?))
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
    resolved: UiResolvedNode,
    paints: &[LevelUiPaintRecord],
    focused: bool,
    frame: u16,
    focus_style: &LevelUiFocusStyle,
    text_override: Option<&str>,
) {
    let focus_chrome = node.flags & ui_node_flags::BUTTON_FOCUS_CHROME != 0;
    let draw_chrome = !focus_chrome || focused;
    let fill = shape_paint(node.color, node.color_paint, paints);
    let fill = if focus_chrome && focused {
        focus_chrome_sweep_paint(fill, frame, focus_style.period)
    } else {
        fill
    };
    let sweep = (focus_chrome && focused).then_some(UiShapeSweep {
        frame,
        period: focus_style.period,
        color: focus_style.color_a,
    });
    let config = if draw_chrome {
        draw_shape_node(node, resolved, paints, Some(fill), sweep)
    } else {
        shape_config(node)
    };
    if draw_chrome
        && !config.transparent
        && config.corners == 0
        && config.border == 0
        && resolved.height > 3
        && fill.is_solid()
    {
        let color = brighten(fill.from());
        draw_quad_flat(
            resolved.subrect(0, 0, resolved.width as i16, 1),
            color.0,
            color.1,
            color.2,
        );
    }
    let Some(font) = font else {
        return;
    };
    let text = text_override.unwrap_or(node.text);
    if text.is_empty() {
        return;
    }
    let scale = node_font_scale_q8(node);
    let letter_spacing = node_letter_spacing(node);
    let line_h = scaled_line_height(font, scale) as i32;
    let text_y = ((resolved.height as i32 - line_h).max(0) / 2)
        .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    let align = (node.flags & ui_node_flags::TEXT_ALIGN_MASK) >> ui_node_flags::TEXT_ALIGN_SHIFT;
    let text_x = aligned_text_x(
        font,
        text,
        0,
        resolved.width,
        align,
        scale,
        letter_spacing,
    );
    let paint = text_paint(node.accent, node.accent_paint, paints);
    let paint = if focus_chrome && !focused {
        dim_paint(paint)
    } else {
        paint
    };
    draw_transformed_text_paint(
        font,
        resolved,
        text_x,
        text_y,
        text,
        scale,
        letter_spacing,
        paint,
    );
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UiShapeConfig {
    corners: u8,
    cut: u8,
    border: u8,
    transparent: bool,
    semi_transparent_fill: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UiShapeSweep {
    frame: u16,
    period: u16,
    color: (u8, u8, u8),
}

fn shape_config(node: &LevelUiNodeRecord) -> UiShapeConfig {
    if ui_shape::is_encoded(node.option) {
        return UiShapeConfig {
            corners: ui_shape::corners(node.option),
            cut: ui_shape::cut(node.option),
            border: ui_shape::border(node.option),
            transparent: ui_shape::transparent(node.option)
                || node.flags & ui_node_flags::BUTTON_TRANSPARENT != 0,
            semi_transparent_fill: ui_shape::semi_transparent_fill(node.option),
        };
    }
    UiShapeConfig {
        transparent: node.flags & ui_node_flags::BUTTON_TRANSPARENT != 0,
        ..UiShapeConfig::default()
    }
}

/// Draw the fill and optional inset border shared by Rect and Button nodes.
/// Legacy nodes stay on the original quad path; styled nodes use a convex
/// 4..=8 vertex polygon and remain allocation-free on PS1.
fn draw_shape_node(
    node: &LevelUiNodeRecord,
    resolved: UiResolvedNode,
    paints: &[LevelUiPaintRecord],
    fill_override: Option<UiPaint>,
    sweep: Option<UiShapeSweep>,
) -> UiShapeConfig {
    let config = shape_config(node);
    let fill = fill_override.unwrap_or_else(|| shape_paint(node.color, node.color_paint, paints));
    if config.corners == 0 && config.border == 0 && !config.semi_transparent_fill {
        if !config.transparent {
            draw_quad_paint(resolved.verts, fill);
        }
        return config;
    }

    let outer = shape_polygon(resolved, config.corners, config.cut, 0);
    if !config.transparent {
        draw_shape_polygon(
            &outer,
            fill,
            resolved.width,
            resolved.height,
            config.semi_transparent_fill,
        );
        if let Some(sweep) = sweep {
            draw_focus_sweep(resolved, config, sweep);
        }
    }
    if config.border != 0 {
        draw_shape_border(
            resolved,
            outer,
            config,
            shape_paint(node.background, node.background_paint, paints),
        );
    }
    config
}

fn focus_sweep_rect(
    width: u16,
    height: u16,
    config: UiShapeConfig,
    frame: u16,
    period: u16,
) -> Option<(i16, i16, i16, i16)> {
    let width = i32::from(width);
    let height = i32::from(height);
    let safe_inset = i32::from(config.cut.max(config.border).max(1));
    let available = width.saturating_sub(safe_inset.saturating_mul(2));
    if available < 3 || height < 3 {
        return None;
    }
    let band_width = (width / 5).clamp(12, 28).min(available);
    let travel = available.saturating_sub(band_width);
    let x = safe_inset + travel.saturating_mul(i32::from(focus_wave(frame, period))) / 255;
    let y = i32::from(config.border.max(1)).min(height / 2);
    let band_height = height.saturating_sub(y.saturating_mul(2));
    Some((x as i16, y as i16, band_width as i16, band_height as i16))
}

fn draw_focus_sweep(resolved: UiResolvedNode, config: UiShapeConfig, sweep: UiShapeSweep) {
    let Some((x, y, width, height)) = focus_sweep_rect(
        resolved.width,
        resolved.height,
        config,
        sweep.frame,
        sweep.period,
    ) else {
        return;
    };
    let left_width = (width / 2).max(1);
    let right_width = width.saturating_sub(left_width).max(1);
    let peak = scale_color_q8(sweep.color, 112);
    draw_focus_sweep_half(resolved.subrect(x, y, left_width, height), (0, 0, 0), peak);
    draw_focus_sweep_half(
        resolved.subrect(x.saturating_add(left_width), y, right_width, height),
        peak,
        (0, 0, 0),
    );
}

fn draw_focus_sweep_half(vertices: [(i16, i16); 4], left: (u8, u8, u8), right: (u8, u8, u8)) {
    draw_tri_gouraud_blended(
        [vertices[0], vertices[1], vertices[2]],
        [left, right, left],
        BlendMode::Add,
    );
    draw_tri_gouraud_blended(
        [vertices[1], vertices[2], vertices[3]],
        [right, left, right],
        BlendMode::Add,
    );
}

fn scale_color_q8(color: (u8, u8, u8), scale: u8) -> (u8, u8, u8) {
    let scale = u16::from(scale);
    (
        ((u16::from(color.0) * scale) >> 8) as u8,
        ((u16::from(color.1) * scale) >> 8) as u8,
        ((u16::from(color.2) * scale) >> 8) as u8,
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UiShapeVertex {
    local: (i32, i32),
    screen: (i16, i16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UiShapePolygon {
    vertices: [UiShapeVertex; 8],
    len: usize,
}

impl UiShapePolygon {
    fn push(&mut self, transform: UiAffine, x: i32, y: i32) {
        if self.len >= self.vertices.len() {
            return;
        }
        self.vertices[self.len] = UiShapeVertex {
            local: (x, y),
            screen: transform.point(x, y),
        };
        self.len += 1;
    }
}

fn shape_polygon(
    resolved: UiResolvedNode,
    corners: u8,
    authored_cut: u8,
    inset: i32,
) -> UiShapePolygon {
    let width = i32::from(resolved.width);
    let height = i32::from(resolved.height);
    let left = inset.min(width / 2);
    let top = inset.min(height / 2);
    let right = width.saturating_sub(inset).max(left);
    let bottom = height.saturating_sub(inset).max(top);
    let cut = i32::from(authored_cut)
        .min((right - left) / 2)
        .min((bottom - top) / 2);
    let mut polygon = UiShapePolygon {
        vertices: [UiShapeVertex::default(); 8],
        len: 0,
    };
    if corners & ui_shape::TOP_LEFT != 0 {
        polygon.push(resolved.transform, left + cut, top);
    } else {
        polygon.push(resolved.transform, left, top);
    }
    if corners & ui_shape::TOP_RIGHT != 0 {
        polygon.push(resolved.transform, right - cut, top);
        polygon.push(resolved.transform, right, top + cut);
    } else {
        polygon.push(resolved.transform, right, top);
    }
    if corners & ui_shape::BOTTOM_RIGHT != 0 {
        polygon.push(resolved.transform, right, bottom - cut);
        polygon.push(resolved.transform, right - cut, bottom);
    } else {
        polygon.push(resolved.transform, right, bottom);
    }
    if corners & ui_shape::BOTTOM_LEFT != 0 {
        polygon.push(resolved.transform, left + cut, bottom);
        polygon.push(resolved.transform, left, bottom - cut);
    } else {
        polygon.push(resolved.transform, left, bottom);
    }
    if corners & ui_shape::TOP_LEFT != 0 {
        polygon.push(resolved.transform, left, top + cut);
    }
    polygon
}

fn draw_shape_polygon(
    polygon: &UiShapePolygon,
    paint: UiPaint,
    width: u16,
    height: u16,
    blended: bool,
) {
    if polygon.len < 3 {
        return;
    }
    for index in 1..polygon.len - 1 {
        draw_shape_triangle(
            [
                polygon.vertices[0],
                polygon.vertices[index],
                polygon.vertices[index + 1],
            ],
            paint,
            width,
            height,
            blended,
        );
    }
}

fn draw_shape_border(
    resolved: UiResolvedNode,
    outer: UiShapePolygon,
    config: UiShapeConfig,
    paint: UiPaint,
) {
    let max_border = (resolved.width.min(resolved.height) / 2).max(1);
    let border = u16::from(config.border).min(max_border) as i32;
    let inner = shape_polygon(resolved, config.corners, config.cut, border);
    if inner.len != outer.len
        || resolved.width <= border as u16 * 2
        || resolved.height <= border as u16 * 2
    {
        draw_shape_polygon(&outer, paint, resolved.width, resolved.height, false);
        return;
    }
    for index in 0..outer.len {
        let next = (index + 1) % outer.len;
        draw_shape_triangle(
            [
                outer.vertices[index],
                outer.vertices[next],
                inner.vertices[index],
            ],
            paint,
            resolved.width,
            resolved.height,
            false,
        );
        draw_shape_triangle(
            [
                outer.vertices[next],
                inner.vertices[index],
                inner.vertices[next],
            ],
            paint,
            resolved.width,
            resolved.height,
            false,
        );
    }
}

fn draw_shape_triangle(
    vertices: [UiShapeVertex; 3],
    paint: UiPaint,
    width: u16,
    height: u16,
    blended: bool,
) {
    let screen = [vertices[0].screen, vertices[1].screen, vertices[2].screen];
    match paint {
        UiPaint::Solid((r, g, b)) => {
            if blended {
                draw_tri_flat_blended(screen, r, g, b, BlendMode::Average);
            } else {
                draw_tri_flat(screen, r, g, b);
            }
        }
        UiPaint::Gradient { .. } => {
            let colors = [
                shape_paint_color(paint, vertices[0].local, width, height),
                shape_paint_color(paint, vertices[1].local, width, height),
                shape_paint_color(paint, vertices[2].local, width, height),
            ];
            if blended {
                draw_tri_gouraud_blended(screen, colors, BlendMode::Average);
            } else {
                draw_tri_gouraud(screen, colors);
            }
        }
    }
}

fn shape_paint_color(paint: UiPaint, local: (i32, i32), width: u16, height: u16) -> (u8, u8, u8) {
    match paint {
        UiPaint::Solid(color) => color,
        UiPaint::Gradient {
            from,
            to,
            direction,
        } => {
            let (position, extent) = match direction {
                LevelUiGradientDirection::Horizontal => (local.0, i32::from(width)),
                LevelUiGradientDirection::Vertical => (local.1, i32::from(height)),
            };
            let t = ((position.clamp(0, extent.max(1)) * 255) / extent.max(1)) as u8;
            lerp_color(to, from, t)
        }
    }
}

/// Draw a slider: a recessed track, a proportional fill, and a knob
/// rectangle centred on the fill edge. `fill_num / fill_den` is the
/// current proportion; `fill_num` is clamped into `[0, fill_den]` here so
/// an out-of-range value cannot run the knob off the track. The bound
/// option's value feeds this through [`slider_fill`] in [`draw_scene`].
fn draw_slider(
    resolved: UiResolvedNode,
    fill_num: i32,
    fill_den: i32,
    track: UiPaint,
    fill: UiPaint,
    knob: UiPaint,
) {
    let width = resolved.width as i16;
    let height = resolved.height as i16;
    if width <= 0 || height <= 0 {
        return;
    }
    draw_quad_flat(resolved.subrect(-1, -1, width + 2, height + 2), 12, 14, 18);
    draw_quad_paint(resolved.verts, track);

    let den = fill_den.max(1);
    let num = fill_num.clamp(0, den);
    let fill_width = ((width as i32).saturating_mul(num) / den) as i16;
    if fill_width > 0 {
        draw_quad_paint(resolved.subrect(0, 0, fill_width, height), fill);
    }

    // Knob: a fixed-width rect centred on the fill edge, clamped so it
    // stays inside the track.
    let knob_w = (height + 2).clamp(3, width.max(3));
    let edge = fill_width as i32;
    let knob_x = (edge - knob_w as i32 / 2).clamp(0, width as i32 - knob_w as i32);
    let knob_x = knob_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    draw_quad_paint(resolved.subrect(knob_x, -1, knob_w, height + 2), knob);
}

fn draw_quad_paint(verts: [(i16, i16); 4], paint: UiPaint) {
    match paint {
        UiPaint::Solid(color) => draw_quad_flat(verts, color.0, color.1, color.2),
        UiPaint::Gradient {
            from,
            to,
            direction,
        } => {
            let colors = gradient_vertex_colors(from, to, direction);
            draw_tri_gouraud(
                [verts[0], verts[1], verts[2]],
                [colors[0], colors[1], colors[2]],
            );
            draw_tri_gouraud(
                [verts[1], verts[2], verts[3]],
                [colors[1], colors[2], colors[3]],
            );
        }
    }
}

fn gradient_vertex_colors(
    from: (u8, u8, u8),
    to: (u8, u8, u8),
    direction: LevelUiGradientDirection,
) -> [(u8, u8, u8); 4] {
    match direction {
        LevelUiGradientDirection::Vertical => [from, from, to, to],
        LevelUiGradientDirection::Horizontal => [from, to, from, to],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiPaint {
    Solid((u8, u8, u8)),
    Gradient {
        from: (u8, u8, u8),
        to: (u8, u8, u8),
        direction: LevelUiGradientDirection,
    },
}

impl UiPaint {
    const fn is_solid(self) -> bool {
        matches!(self, Self::Solid(_))
    }

    const fn from(self) -> (u8, u8, u8) {
        match self {
            Self::Solid(color) | Self::Gradient { from: color, .. } => color,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct MessagePanelLayout {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    max_lines: u8,
}

/// Number of presented frames used to morph the compact interaction prompt
/// into the full two-line Archive panel.
pub const MESSAGE_PANEL_EXPAND_FRAMES: u16 = 6;
/// Presented frames elapsed between newly revealed characters.
pub const MESSAGE_PANEL_TYPE_TICKS_PER_CHAR: u16 = 1;
const ARCHIVE_PROMPT_Y: i16 = 209;
const ARCHIVE_PROMPT_HEIGHT: u16 = 17;
const ARCHIVE_PROMPT_PAD_X: i16 = 10;

const fn message_panel_layout(variant: MessagePanelVariant) -> MessagePanelLayout {
    match variant {
        MessagePanelVariant::PointOfInterest => MessagePanelLayout {
            x: 28,
            y: 184,
            width: 264,
            height: 42,
            max_lines: 2,
        },
        MessagePanelVariant::World => MessagePanelLayout {
            x: 28,
            y: 92,
            width: 264,
            height: 56,
            max_lines: 3,
        },
    }
}

fn screen_resolved_node(x: i16, y: i16, width: u16, height: u16) -> UiResolvedNode {
    let transform = UiAffine {
        tx: i32::from(x) << 12,
        ty: i32::from(y) << 12,
        ..UiAffine::IDENTITY
    };
    UiResolvedNode {
        width,
        height,
        transform,
        verts: transform.subrect(0, 0, width as i16, height as i16),
    }
}

/// Draw one page of the shared Archive message presentation.
///
/// This is deliberately a direct-mode helper rather than a transient cooked
/// UI scene: POIs can display authored copy without allocating nodes, loading
/// a texture, or perturbing project VRAM residency. `frame` drives the red
/// scanner band and should normally be the current visible/simulation frame.
/// Long text is word-wrapped and hard-capped to the variant's line budget;
/// page splitting remains the caller's responsibility.
pub fn draw_message_panel(
    font: &FontAtlas,
    page_text: &str,
    variant: MessagePanelVariant,
    page: MessagePageMeta,
    frame: u16,
) {
    let layout = message_panel_layout(variant);
    let resolved = screen_resolved_node(layout.x, layout.y, layout.width, layout.height);
    draw_archive_panel_chrome(resolved, frame);
    draw_message_panel_body(
        font,
        page_text,
        page_text,
        layout,
        page,
        UiPaint::Solid((114, 96, 92)),
        true,
    );
}

/// Morph the compact `X - action` prompt into the full bottom Archive panel.
///
/// The prompt and message share their centre and bottom edge, so the chrome,
/// clipped corners, translucent fill, and scanner are visibly the same object
/// throughout the transition. Copy is deliberately absent while the frame is
/// moving; the message types on only after the full two-line panel settles.
pub fn draw_expanding_message_panel(
    font: &FontAtlas,
    action: &str,
    page_text: &str,
    page: MessagePageMeta,
    frame: u16,
    transition_frame: u16,
    typewriter_frame: u16,
) {
    let prompt = interaction_prompt_layout(font, action);
    let target = message_panel_layout(MessagePanelVariant::PointOfInterest);
    let layout = message_panel_transition_layout(prompt, target, transition_frame);
    let resolved = screen_resolved_node(layout.x, layout.y, layout.width, layout.height);
    draw_archive_panel_chrome(resolved, frame);

    // Activation clears the compact prompt immediately. Let the panel itself
    // be the only thing on screen until its geometry has completely settled;
    // the body then types on without competing with a moving frame.
    if transition_frame < MESSAGE_PANEL_EXPAND_FRAMES {
        return;
    }
    let visible_characters = typewriter_frame / MESSAGE_PANEL_TYPE_TICKS_PER_CHAR;
    let visible_text = text_prefix_chars(page_text, usize::from(visible_characters));
    draw_message_panel_body(
        font,
        page_text,
        visible_text,
        layout,
        page,
        UiPaint::Solid((114, 96, 92)),
        visible_text.len() == page_text.len(),
    );
}

/// Morph a completed POI message into the compact unique-item acquisition
/// panel. The geometry shrinks in place with no copy during motion; the exact
/// acquired item then types on and remains explicitly dismissible with Cross.
pub fn draw_item_acquired_panel(
    font: &FontAtlas,
    item_name: &str,
    frame: u16,
    transition_frame: u16,
    typewriter_frame: u16,
) {
    const PREFIX: &str = "ITEM ACQUIRED - ";
    const TARGET: MessagePanelLayout = MessagePanelLayout {
        x: 55,
        y: 196,
        width: 210,
        height: 30,
        max_lines: 1,
    };
    let source = message_panel_layout(MessagePanelVariant::PointOfInterest);
    let layout = message_panel_transition_layout(source, TARGET, transition_frame);
    let resolved = screen_resolved_node(layout.x, layout.y, layout.width, layout.height);
    draw_archive_panel_chrome(resolved, frame);
    if transition_frame < MESSAGE_PANEL_EXPAND_FRAMES {
        return;
    }

    let visible_characters =
        usize::from(typewriter_frame / MESSAGE_PANEL_TYPE_TICKS_PER_CHAR);
    let prefix_chars = PREFIX.chars().count();
    let visible_prefix = text_prefix_chars(PREFIX, visible_characters.min(prefix_chars));
    let visible_name = text_prefix_chars(item_name, visible_characters.saturating_sub(prefix_chars));
    let total_width = font
        .text_width(PREFIX)
        .saturating_add(font.text_width(item_name)) as i16;
    let text_x = layout
        .x
        .saturating_add((i32::from(layout.width).saturating_sub(i32::from(total_width)).max(0) / 2) as i16);
    let text_y = layout.y.saturating_add(4);
    draw_scaled_text_paint(
        font,
        text_x,
        text_y,
        visible_prefix,
        UI_FONT_SCALE_ONE_Q8,
        0,
        UiPaint::Solid((124, 64, 54)),
    );
    draw_scaled_text_paint(
        font,
        text_x.saturating_add(font.text_width(PREFIX) as i16),
        text_y,
        visible_name,
        UI_FONT_SCALE_ONE_Q8,
        0,
        UiPaint::Solid((202, 154, 136)),
    );
    if visible_name.len() == item_name.len() {
        draw_message_dismiss_hint(font, layout);
    }
}

fn text_prefix_chars(text: &str, character_count: usize) -> &str {
    if character_count == 0 {
        return "";
    }
    let end = text
        .char_indices()
        .nth(character_count)
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[..end]
}

fn draw_message_panel_body(
    font: &FontAtlas,
    page_text: &str,
    visible_text: &str,
    layout: MessagePanelLayout,
    page: MessagePageMeta,
    text_paint: UiPaint,
    show_page_pips: bool,
) {
    const TEXT_INSET_X: u16 = 10;

    let text_width = layout.width.saturating_sub(TEXT_INSET_X * 2).max(1);
    let line_height = scaled_line_height(font, UI_FONT_SCALE_ONE_Q8).max(1);
    let line_count = wrapped_line_count(
        font,
        page_text,
        text_width,
        UI_FONT_SCALE_ONE_Q8,
        0,
        layout.max_lines,
    );
    let text_height = i32::from(line_height).saturating_mul(i32::from(line_count));
    let mut line_y = layout
        .y
        .saturating_add(((i32::from(layout.height) - text_height).max(0) / 2) as i16);
    let mut start = 0usize;
    let mut drawn = 0u8;
    while start < page_text.len() && drawn < layout.max_lines {
        while matches!(page_text.as_bytes().get(start), Some(b' ' | b'\n' | b'\r')) {
            start += 1;
        }
        if start >= page_text.len() {
            break;
        }
        let end = wrapped_line_end(font, page_text, start, text_width, UI_FONT_SCALE_ONE_Q8, 0);
        if end <= start {
            break;
        }
        let visible_end = end.min(visible_text.len());
        if visible_end <= start {
            break;
        }
        draw_scaled_text_paint(
            font,
            layout.x.saturating_add(TEXT_INSET_X as i16),
            line_y,
            &page_text[start..visible_end],
            UI_FONT_SCALE_ONE_Q8,
            0,
            text_paint,
        );
        line_y = line_y.saturating_add(line_height);
        drawn += 1;
        start = end;
    }

    if show_page_pips {
        let resolved = screen_resolved_node(layout.x, layout.y, layout.width, layout.height);
        draw_message_page_pips(resolved, MessagePageMeta::new(page.index, page.count));
    }
    draw_message_dismiss_hint(font, layout);
}

/// Keep the close control visible for the entire readable phase. Cross may
/// first complete the typewriter or advance a paged message, but it is always
/// the control that ultimately dismisses the open panel.
fn draw_message_dismiss_hint(font: &FontAtlas, layout: MessagePanelLayout) {
    const PREFIX: &str = "X - ";
    const ACTION: &str = "DISMISS";
    const INSET_X: i16 = 10;
    const INSET_BOTTOM: u16 = 3;

    let line_height = scaled_line_height(font, UI_FONT_SCALE_ONE_Q8).max(0) as u16;
    let text_y = layout.y.saturating_add(
        layout
            .height
            .saturating_sub(line_height)
            .saturating_sub(INSET_BOTTOM) as i16,
    );
    let text_x = layout.x.saturating_add(INSET_X);
    draw_scaled_text_paint(
        font,
        text_x,
        text_y,
        PREFIX,
        UI_FONT_SCALE_ONE_Q8,
        0,
        UiPaint::Solid((124, 64, 54)),
    );
    draw_scaled_text_paint(
        font,
        text_x.saturating_add(font.text_width(PREFIX) as i16),
        text_y,
        ACTION,
        UI_FONT_SCALE_ONE_Q8,
        0,
        UiPaint::Solid((114, 96, 92)),
    );
}

/// Draw the compact proximity prompt used beneath an Archive Beacon.
///
/// `action` is the verb only (normally `"READ"`); the control prefix is kept
/// in the shared visual so every POI consistently presents `X - action`.
pub fn draw_interaction_prompt_panel(font: &FontAtlas, action: &str, frame: u16) {
    let layout = interaction_prompt_layout(font, action);
    let resolved = screen_resolved_node(layout.x, layout.y, layout.width, layout.height);
    draw_archive_panel_chrome(resolved, frame);
    draw_interaction_prompt_text(font, action, layout);
}

fn interaction_prompt_layout(font: &FontAtlas, action: &str) -> MessagePanelLayout {
    const PREFIX: &str = "X - ";
    let prefix_width = font.text_width(PREFIX) as i16;
    let action_width = font.text_width(action) as i16;
    let text_width = prefix_width.saturating_add(action_width);
    let box_width = text_width
        .saturating_add(ARCHIVE_PROMPT_PAD_X * 2)
        .clamp(76, UI_CANVAS_W as i16 - 32);
    let box_x = (UI_CANVAS_W as i16 - box_width) / 2;
    MessagePanelLayout {
        x: box_x,
        y: ARCHIVE_PROMPT_Y,
        width: box_width as u16,
        height: ARCHIVE_PROMPT_HEIGHT,
        max_lines: 1,
    }
}

fn draw_interaction_prompt_text(font: &FontAtlas, action: &str, layout: MessagePanelLayout) {
    const PREFIX: &str = "X - ";
    let prefix_width = font.text_width(PREFIX) as i16;
    let action_width = font.text_width(action) as i16;
    let text_width = prefix_width.saturating_add(action_width);
    let text_x = layout
        .x
        .saturating_add((layout.width as i16 - text_width) / 2);
    let text_y = layout.y.saturating_add(
        ((i32::from(layout.height) - i32::from(scaled_line_height(font, UI_FONT_SCALE_ONE_Q8)))
            .max(0)
            / 2) as i16,
    );
    draw_scaled_text_paint(
        font,
        text_x,
        text_y,
        PREFIX,
        UI_FONT_SCALE_ONE_Q8,
        0,
        UiPaint::Solid((124, 64, 54)),
    );
    draw_scaled_text_paint(
        font,
        text_x.saturating_add(prefix_width),
        text_y,
        action,
        UI_FONT_SCALE_ONE_Q8,
        0,
        UiPaint::Solid((114, 96, 92)),
    );
}

fn message_panel_transition_layout(
    prompt: MessagePanelLayout,
    message: MessagePanelLayout,
    frame: u16,
) -> MessagePanelLayout {
    let progress = message_panel_transition_progress_q8(frame);
    let width = lerp_u16_q8(prompt.width, message.width, progress);
    let height = lerp_u16_q8(prompt.height, message.height, progress);
    let prompt_centre_twice = i32::from(prompt.x) * 2 + i32::from(prompt.width);
    let message_centre_twice = i32::from(message.x) * 2 + i32::from(message.width);
    let centre_twice = lerp_i32_q8(prompt_centre_twice, message_centre_twice, progress);
    let bottom = lerp_i32_q8(
        i32::from(prompt.y) + i32::from(prompt.height),
        i32::from(message.y) + i32::from(message.height),
        progress,
    );
    MessagePanelLayout {
        x: ((centre_twice - i32::from(width)) / 2).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
            as i16,
        y: bottom
            .saturating_sub(i32::from(height))
            .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        width,
        height,
        max_lines: if progress == 0 {
            prompt.max_lines
        } else {
            message.max_lines
        },
    }
}

fn message_panel_transition_progress_q8(frame: u16) -> u16 {
    let linear = ((u32::from(frame.min(MESSAGE_PANEL_EXPAND_FRAMES)) * 256)
        / u32::from(MESSAGE_PANEL_EXPAND_FRAMES)) as u16;
    let squared = u32::from(linear).saturating_mul(u32::from(linear)) >> 8;
    ((squared.saturating_mul(768u32.saturating_sub(u32::from(linear) * 2))) >> 8).min(256) as u16
}

fn lerp_i32_q8(from: i32, to: i32, progress: u16) -> i32 {
    from.saturating_add(to.saturating_sub(from).saturating_mul(i32::from(progress)) >> 8)
}

fn lerp_u16_q8(from: u16, to: u16, progress: u16) -> u16 {
    let delta = i32::from(to).saturating_sub(i32::from(from));
    i32::from(from)
        .saturating_add(delta.saturating_mul(i32::from(progress)) >> 8)
        .clamp(0, i32::from(u16::MAX)) as u16
}

fn draw_archive_panel_chrome(resolved: UiResolvedNode, frame: u16) {
    let config = UiShapeConfig {
        corners: ui_shape::TOP_LEFT | ui_shape::BOTTOM_RIGHT,
        cut: 5,
        border: 1,
        transparent: false,
        semi_transparent_fill: true,
    };
    let outer = shape_polygon(resolved, config.corners, config.cut, 0);
    // Two Average passes leave 25% of the world visible. The black under-pass
    // keeps pale copy readable over bright characters/geometry; the red pass
    // above it retains the established translucent Archive language.
    draw_shape_polygon(
        &outer,
        UiPaint::Solid((2, 1, 2)),
        resolved.width,
        resolved.height,
        true,
    );
    draw_shape_polygon(
        &outer,
        UiPaint::Gradient {
            from: (70, 18, 18),
            to: (10, 5, 8),
            direction: LevelUiGradientDirection::Horizontal,
        },
        resolved.width,
        resolved.height,
        true,
    );
    draw_focus_sweep(
        resolved,
        config,
        UiShapeSweep {
            frame,
            period: 120,
            color: (210, 58, 40),
        },
    );
    draw_shape_border(
        resolved,
        outer,
        config,
        UiPaint::Gradient {
            from: (92, 20, 16),
            to: (220, 92, 58),
            direction: LevelUiGradientDirection::Horizontal,
        },
    );
}

fn wrapped_line_count(
    font: &FontAtlas,
    text: &str,
    width: u16,
    scale: u16,
    letter_spacing: i8,
    max_lines: u8,
) -> u8 {
    let mut start = 0usize;
    let mut lines = 0u8;
    while start < text.len() && lines < max_lines {
        while matches!(text.as_bytes().get(start), Some(b' ' | b'\n' | b'\r')) {
            start += 1;
        }
        if start >= text.len() {
            break;
        }
        let end = wrapped_line_end(font, text, start, width, scale, letter_spacing);
        if end <= start {
            break;
        }
        lines += 1;
        start = end;
    }
    lines.max(1)
}

fn draw_message_page_pips(resolved: UiResolvedNode, page: MessagePageMeta) {
    if page.count <= 1 {
        return;
    }
    let visible = page.count.min(8);
    let width = i16::from(visible).saturating_mul(3).saturating_sub(1);
    let start_x = resolved.width as i16 - 7 - width;
    let y = resolved.height as i16 - 5;
    let selected = page.index.min(visible - 1);
    let mut index = 0u8;
    while index < visible {
        let color = if index == selected {
            (220, 92, 58)
        } else {
            (64, 26, 24)
        };
        draw_quad_flat(
            resolved.subrect(start_x + i16::from(index) * 3, y, 2, 1),
            color.0,
            color.1,
            color.2,
        );
        index += 1;
    }
}

fn shape_paint(color: [u8; 3], paint_index: u16, paints: &[LevelUiPaintRecord]) -> UiPaint {
    resolve_paint(color, paint_index, paints, rgb)
}

fn text_paint(color: [u8; 3], paint_index: u16, paints: &[LevelUiPaintRecord]) -> UiPaint {
    resolve_paint(color, paint_index, paints, font_tint)
}

fn resolve_paint(
    color: [u8; 3],
    paint_index: u16,
    paints: &[LevelUiPaintRecord],
    map_color: fn([u8; 3]) -> (u8, u8, u8),
) -> UiPaint {
    let solid = map_color(color);
    if paint_index == UI_PAINT_NONE {
        return UiPaint::Solid(solid);
    }
    let Some(paint) = paints.get(paint_index as usize) else {
        return UiPaint::Solid(solid);
    };
    UiPaint::Gradient {
        from: map_color(paint.from),
        to: map_color(paint.to),
        direction: paint.direction,
    }
}

fn brighten(color: (u8, u8, u8)) -> (u8, u8, u8) {
    (
        color.0.saturating_add(34),
        color.1.saturating_add(34),
        color.2.saturating_add(34),
    )
}

fn dim_paint(paint: UiPaint) -> UiPaint {
    let dim = |color: (u8, u8, u8)| {
        (
            ((u16::from(color.0) * 9) / 16) as u8,
            ((u16::from(color.1) * 9) / 16) as u8,
            ((u16::from(color.2) * 9) / 16) as u8,
        )
    };
    match paint {
        UiPaint::Solid(color) => UiPaint::Solid(dim(color)),
        UiPaint::Gradient {
            from,
            to,
            direction,
        } => UiPaint::Gradient {
            from: dim(from),
            to: dim(to),
            direction,
        },
    }
}

/// Turn the selected button's authored fill into a horizontal, ping-pong
/// gradient. Because this replaces the shape fill itself, clipped corners and
/// PS1 average-blend transparency remain intact without extra geometry.
fn focus_chrome_sweep_paint(paint: UiPaint, frame: u16, period: u16) -> UiPaint {
    let (dark, bright) = match paint {
        UiPaint::Solid(color) => (color, brighten(color)),
        UiPaint::Gradient { from, to, .. } => (from, to),
    };
    let phase = focus_wave(frame, period);
    UiPaint::Gradient {
        from: lerp_color(dark, bright, phase),
        to: lerp_color(bright, dark, phase),
        direction: LevelUiGradientDirection::Horizontal,
    }
}

fn rgb(color: [u8; 3]) -> (u8, u8, u8) {
    (color[0], color[1], color[2])
}

fn font_tint(color: [u8; 3]) -> (u8, u8, u8) {
    (
        psx_texture_modulation_color(color[0]),
        psx_texture_modulation_color(color[1]),
        psx_texture_modulation_color(color[2]),
    )
}

fn psx_texture_modulation_color(component: u8) -> u8 {
    (component as u16).div_ceil(2) as u8
}
