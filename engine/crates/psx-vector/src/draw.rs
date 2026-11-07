//! Drawing a group: transform once, then emit one Gouraud triangle per index
//! triple.

use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::prim::{QuadGouraud, TriGouraud};
use psx_gpu::Gpu;
use psx_math::{cos_q12, sin_q12};

use crate::asset::{GroupId, VectorAsset};
use crate::clip::{clip_triangle, ClipVertex};
use crate::format::{blend_from_code, GroupRecord, Vertex, COORD_SHIFT, RUN_QUADS};

/// The largest [`Placement::scale_q12`] (4.0). Larger values are clamped so
/// the 32-bit transform cannot overflow for coordinates inside the canvas
/// limit.
pub const MAX_SCALE_Q12: u16 = 4 * 4096;

/// Where and how large a group is drawn.
///
/// The group's authored position is the identity placement. Rotation and
/// scale act about the pivot, which stays at its authored position plus
/// `offset_q4`: `screen = pivot + offset + R * S * (p - pivot)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Placement {
    /// Translation in Q4 pixels (1/16 px), so animation can move sub-pixel.
    pub offset_q4: [i32; 2],
    /// Uniform scale, Q12 (4096 is 1.0), at most [`MAX_SCALE_Q12`].
    pub scale_q12: u16,
    /// Rotation in psx-math Q0.12 turns (4096 is a full turn), clockwise on
    /// screen because screen Y points down.
    pub angle_q12: u16,
    /// Pivot override in Q4 pixels, in the asset's coordinates; `None` uses the
    /// group's cooked pivot. Give every group of a rigid assembly the same
    /// pivot to rotate them together.
    pub pivot_q4: Option<[i32; 2]>,
}

impl Placement {
    /// Authored position, size and orientation.
    pub const IDENTITY: Self = Self {
        offset_q4: [0, 0],
        scale_q12: 4096,
        angle_q12: 0,
        pivot_q4: None,
    };

    /// Translate by whole pixels.
    pub const fn at(x: i32, y: i32) -> Self {
        Self::at_q4(x << COORD_SHIFT, y << COORD_SHIFT)
    }

    /// Translate by Q4 pixels.
    pub const fn at_q4(x: i32, y: i32) -> Self {
        Self {
            offset_q4: [x, y],
            ..Self::IDENTITY
        }
    }

    /// The same placement scaled by `scale_q12` (clamped to [`MAX_SCALE_Q12`]).
    pub const fn scaled(mut self, scale_q12: u16) -> Self {
        self.scale_q12 = if scale_q12 > MAX_SCALE_Q12 {
            MAX_SCALE_Q12
        } else {
            scale_q12
        };
        self
    }

    /// The same placement rotated by `angle_q12` turns.
    pub const fn rotated(mut self, angle_q12: u16) -> Self {
        self.angle_q12 = angle_q12;
        self
    }

    /// The same placement about an explicit pivot, in Q4 asset pixels.
    pub const fn about(mut self, x_q4: i32, y_q4: i32) -> Self {
        self.pivot_q4 = Some([x_q4, y_q4]);
        self
    }
}

/// Half-open pixel rectangle, in the same coordinates triangles are drawn in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipRect {
    /// Left edge, inclusive.
    pub x0: i32,
    /// Top edge, inclusive.
    pub y0: i32,
    /// Right edge, exclusive.
    pub x1: i32,
    /// Bottom edge, exclusive.
    pub y1: i32,
}

/// What a draw does to the blend mode the asset was authored with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendOverride {
    /// Draw each run in the mode it was cooked with.
    Authored,
    /// Draw every run in this mode, for fades and glows over whatever is
    /// behind. [`BlendMode::Opaque`] forces opaque.
    Force(BlendMode),
}

/// Colour state and clipping for one draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Style {
    /// Per-channel multiply, 255 leaves a channel unchanged.
    pub tint: [u8; 3],
    /// 255 keeps the authored colours, 0 turns them to grey of equal luma.
    pub saturation: u8,
    /// Blend override.
    pub blend: BlendOverride,
    /// Clip rectangle. `None` draws unclipped, which is only valid while every
    /// transformed vertex stays inside the GPU's -1024..1023 coordinate range.
    pub clip: Option<ClipRect>,
}

impl Style {
    /// Authored colours and blend, no clip.
    pub const DEFAULT: Self = Self {
        tint: [255; 3],
        saturation: 255,
        blend: BlendOverride::Authored,
        clip: None,
    };

    /// Authored colours multiplied by `tint`.
    pub const fn tinted(tint: [u8; 3]) -> Self {
        Self {
            tint,
            ..Self::DEFAULT
        }
    }

    /// The same style, clipped to `rect`.
    pub const fn clipped(mut self, rect: ClipRect) -> Self {
        self.clip = Some(rect);
        self
    }

    const fn recolors(&self) -> bool {
        self.saturation != 255 || self.tint[0] != 255 || self.tint[1] != 255 || self.tint[2] != 255
    }
}

impl Default for Style {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Reusable per-draw storage for transformed vertices.
///
/// `N` must be at least [`VectorAsset::max_group_vertices`] of the asset
/// drawn. It lives in a `static` or on the caller's stack; it is not part of
/// the asset so the HUD pays RAM for its own largest group only.
pub struct Scratch<const N: usize> {
    pos: [u32; N],
    col: [u32; N],
    /// Clip outcodes, used only by clipped draws.
    code: [u8; N],
}

impl<const N: usize> Scratch<N> {
    /// Zeroed scratch, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            pos: [0; N],
            col: [0; N],
            code: [0; N],
        }
    }
}

impl<const N: usize> Default for Scratch<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a draw did nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawError {
    /// The group has more vertices than the [`Scratch`] holds.
    ScratchTooSmall,
}

/// One step of a group's draw, handed to the sink of [`VectorAsset::emit_group`].
#[derive(Clone, Copy, Debug)]
pub enum Emit<'a> {
    /// Switch the draw mode to this blend before the next triangles. Emitted
    /// first on every draw, then at each change.
    Blend(BlendMode),
    /// A triangle, translucent exactly when the current blend is not opaque.
    Triangle(&'a TriGouraud),
    /// A quad (GP0 0x38), drawn by the GPU as triangles `0 1 2` and `1 2 3`,
    /// translucent exactly when the current blend is not opaque.
    Quad(&'a QuadGouraud),
}

/// Opaque and semi-transparent Gouraud-triangle command words (no colour).
const OPCODE_OPAQUE: u32 = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]).color0_cmd;
const OPCODE_TRANSLUCENT: u32 = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3])
    .translucent()
    .color0_cmd;

/// The same for Gouraud quads.
const QUAD_OPAQUE: u32 = QuadGouraud::new([(0, 0); 4], [(0, 0, 0); 4]).color0_cmd;
const QUAD_TRANSLUCENT: u32 = QuadGouraud::new([(0, 0); 4], [(0, 0, 0); 4])
    .translucent()
    .color0_cmd;

#[inline(always)]
fn pack_pos(x: i32, y: i32) -> u32 {
    (x as u16 as u32) | ((y as u16 as u32) << 16)
}

/// Scale an 8-bit factor to 0..=256 so 255 is exactly identity and 0 is zero.
#[inline(always)]
const fn unit(factor: u8) -> i32 {
    factor as i32 + (factor >> 7) as i32
}

/// Apply saturation (a blend towards equal-luma grey) and tint (a per-channel
/// multiply) to one GP0 colour word.
#[inline]
fn recolor(color: u32, style: &Style) -> u32 {
    let mut r = (color & 0xFF) as i32;
    let mut g = ((color >> 8) & 0xFF) as i32;
    let mut b = ((color >> 16) & 0xFF) as i32;
    if style.saturation != 255 {
        let luma = (77 * r + 150 * g + 29 * b) >> 8;
        let k = unit(style.saturation);
        r = (r * k + luma * (256 - k)) >> 8;
        g = (g * k + luma * (256 - k)) >> 8;
        b = (b * k + luma * (256 - k)) >> 8;
    }
    let r = (r * unit(style.tint[0])) >> 8;
    let g = (g * unit(style.tint[1])) >> 8;
    let b = (b * unit(style.tint[2])) >> 8;
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// The 2x2 Q12 matrix and Q16 translations of a placement about its pivot.
#[derive(Clone, Copy)]
struct Transform {
    m00: i32,
    m01: i32,
    m10: i32,
    m11: i32,
    /// `(pivot + offset) << 12` plus half a pixel (Q16 rounding) minus the
    /// matrix applied to the pivot, per axis.
    tx: i32,
    ty: i32,
}

impl Transform {
    fn new(place: &Placement, default_pivot: [i16; 2]) -> Self {
        let scale = place.scale_q12.min(MAX_SCALE_Q12) as i32;
        // The common unrotated case skips the sine table: cos 0 is 1.0 exactly,
        // so the matrix is the scale on the diagonal.
        let (m00, m01, m10) = if place.angle_q12 & 0x0FFF == 0 {
            (scale, 0, 0)
        } else {
            let (sin, cos) = (sin_q12(place.angle_q12), cos_q12(place.angle_q12));
            (
                (cos * scale + 2048) >> 12,
                (-sin * scale + 2048) >> 12,
                (sin * scale + 2048) >> 12,
            )
        };
        let m11 = m00;
        let [px, py] = place
            .pivot_q4
            .unwrap_or([default_pivot[0] as i32, default_pivot[1] as i32]);
        // Q16 = Q4 position * Q12 matrix. 0x8000 rounds to nearest pixel.
        let tx = ((px + place.offset_q4[0]) << 12) + 0x8000 - (m00 * px + m01 * py);
        let ty = ((py + place.offset_q4[1]) << 12) + 0x8000 - (m10 * px + m11 * py);
        Self {
            m00,
            m01,
            m10,
            m11,
            tx,
            ty,
        }
    }

    /// Whether the matrix is a pure translation (scale 1, no rotation).
    fn is_translation(&self) -> bool {
        self.m00 == 4096 && self.m01 == 0 && self.m10 == 0 && self.m11 == 4096
    }

    /// Whether the matrix is a pure scale (no rotation).
    fn is_diagonal(&self) -> bool {
        self.m01 == 0 && self.m10 == 0
    }

    #[inline(always)]
    fn apply(&self, x: i32, y: i32) -> (i32, i32) {
        (
            (self.m00 * x + self.m01 * y + self.tx) >> 16,
            (self.m10 * x + self.m11 * y + self.ty) >> 16,
        )
    }
}

/// Transform `verts` into packed position words.
#[inline(always)]
fn transform_vertices(verts: &[Vertex], out: &mut [u32], t: &Transform) {
    if t.is_translation() {
        // x * 4096 >> 16 is x >> 4: no multiplies at all.
        for (v, o) in verts.iter().zip(out.iter_mut()) {
            let x = (((v.x as i32) << 12) + t.tx) >> 16;
            let y = (((v.y as i32) << 12) + t.ty) >> 16;
            *o = pack_pos(x, y);
        }
    } else if t.is_diagonal() {
        for (v, o) in verts.iter().zip(out.iter_mut()) {
            let x = (t.m00 * v.x as i32 + t.tx) >> 16;
            let y = (t.m11 * v.y as i32 + t.ty) >> 16;
            *o = pack_pos(x, y);
        }
    } else {
        for (v, o) in verts.iter().zip(out.iter_mut()) {
            let (x, y) = t.apply(v.x as i32, v.y as i32);
            *o = pack_pos(x, y);
        }
    }
}

/// Bounding box of `bounds` (Q4 `[min_x, min_y, max_x, max_y]`) under `t`, in
/// pixels, conservative by one pixel.
fn transformed_bounds(bounds: [i16; 4], t: &Transform) -> [i32; 4] {
    let mut lo = [i32::MAX; 2];
    let mut hi = [i32::MIN; 2];
    for (x, y) in [
        (bounds[0], bounds[1]),
        (bounds[2], bounds[1]),
        (bounds[0], bounds[3]),
        (bounds[2], bounds[3]),
    ] {
        let (px, py) = t.apply(x as i32, y as i32);
        lo = [lo[0].min(px), lo[1].min(py)];
        hi = [hi[0].max(px), hi[1].max(py)];
    }
    [lo[0] - 1, lo[1] - 1, hi[0] + 1, hi[1] + 1]
}

/// Clip outcode of a transformed position word against `clip`.
///
/// Low nibble: on or outside an edge (left, right, top, bottom), enough to
/// reject a primitive whose vertices share one. High nibble: strictly outside
/// that edge, which is what forces a cut.
#[inline(always)]
fn outcode(word: u32, clip: ClipRect) -> u8 {
    let x = word as u16 as i16 as i32;
    let y = (word >> 16) as u16 as i16 as i32;
    (x <= clip.x0) as u8
        | ((x >= clip.x1) as u8) << 1
        | ((y <= clip.y0) as u8) << 2
        | ((y >= clip.y1) as u8) << 3
        | ((x < clip.x0) as u8) << 4
        | ((x > clip.x1) as u8) << 5
        | ((y < clip.y0) as u8) << 6
        | ((y > clip.y1) as u8) << 7
}

impl<'a> VectorAsset<'a> {
    /// Draw `group` straight to the GPU, returning the GPU triangles emitted (a
    /// quad counts as two).
    ///
    /// Draw order within the group is the cooked paint order; draw groups
    /// back to front. See the crate docs for the draw-mode register this
    /// writes.
    #[inline]
    pub fn draw_group<const N: usize>(
        &self,
        gpu: &mut Gpu,
        group: GroupId,
        place: &Placement,
        style: &Style,
        scratch: &mut Scratch<N>,
    ) -> Result<u32, DrawError> {
        self.emit_group(group, place, style, scratch, &mut |step| match step {
            Emit::Blend(mode) => {
                gpu.set_draw_mode(TextureMaterial::blended(0, 0, (0, 0, 0), mode));
            }
            Emit::Triangle(tri) => gpu.draw(tri),
            Emit::Quad(quad) => gpu.draw(quad),
        })
    }

    /// Run `group`'s draw through `sink` instead of the GPU, returning the GPU
    /// triangles emitted (a quad counts as two).
    ///
    /// This is the path for linking packets into an ordering table or for
    /// host tests: the sink sees exactly the [`Emit`] steps the immediate
    /// draw would send, and a [`TriGouraud`] or [`QuadGouraud`] is the same
    /// packet either way.
    pub fn emit_group<const N: usize>(
        &self,
        group: GroupId,
        place: &Placement,
        style: &Style,
        scratch: &mut Scratch<N>,
        sink: &mut impl FnMut(Emit<'_>),
    ) -> Result<u32, DrawError> {
        let record = &self.groups[group.index()];
        let verts = self.group_vertices(record);
        if verts.len() > N {
            return Err(DrawError::ScratchTooSmall);
        }
        let t = Transform::new(place, record.pivot);
        // A clip rectangle that contains the whole group changes nothing, so
        // that group takes the unclipped path; one that misses it draws nothing.
        let mut clip_needed = None;
        if let Some(clip) = style.clip {
            let b = transformed_bounds(record.bounds, &t);
            if b[2] <= clip.x0 || b[0] >= clip.x1 || b[3] <= clip.y0 || b[1] >= clip.y1 {
                return Ok(0);
            }
            let inside = b[0] >= clip.x0 && b[2] <= clip.x1 && b[1] >= clip.y0 && b[3] <= clip.y1;
            if !inside {
                clip_needed = Some(clip);
            }
        }
        let n = verts.len();
        transform_vertices(verts, &mut scratch.pos[..n], &t);
        let recolor_vertices = style.recolors();
        if recolor_vertices {
            for (v, c) in verts.iter().zip(scratch.col[..n].iter_mut()) {
                *c = recolor(v.color, style);
            }
        }
        if let Some(clip) = clip_needed {
            for (p, c) in scratch.pos[..n].iter().zip(scratch.code[..n].iter_mut()) {
                *c = outcode(*p, clip);
            }
        }
        let ctx = Primitives {
            asset: self,
            record,
            verts,
            pos: &scratch.pos[..n],
            col: &scratch.col[..n],
            code: &scratch.code[..n],
            style,
            clip: clip_needed,
        };
        Ok(match (clip_needed, recolor_vertices) {
            (None, false) => ctx.emit::<false, false>(sink),
            (None, true) => ctx.emit::<false, true>(sink),
            (Some(_), false) => ctx.emit::<true, false>(sink),
            (Some(_), true) => ctx.emit::<true, true>(sink),
        })
    }
}

/// Everything the primitive loop reads, bundled so the loop's four
/// instantiations take one argument.
struct Primitives<'a> {
    asset: &'a VectorAsset<'a>,
    record: &'a GroupRecord,
    verts: &'a [Vertex],
    pos: &'a [u32],
    col: &'a [u32],
    code: &'a [u8],
    style: &'a Style,
    /// The clip rectangle when this group crosses it, else `None`.
    clip: Option<ClipRect>,
}

impl Primitives<'_> {
    #[inline(always)]
    fn emit<const CLIP: bool, const RECOLOR: bool>(&self, sink: &mut impl FnMut(Emit<'_>)) -> u32 {
        let indices = self.asset.group_indices(self.record);
        let clip = self.clip.unwrap_or(ClipRect {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        });
        let mut emitted = 0u32;
        let mut current: Option<BlendMode> = None;
        let mut next = 0usize;
        for run in self.asset.group_runs(self.record) {
            let mode = match self.style.blend {
                BlendOverride::Authored => blend_from_code(run.blend).unwrap_or(BlendMode::Opaque),
                BlendOverride::Force(mode) => mode,
            };
            if current != Some(mode) {
                sink(Emit::Blend(mode));
                current = Some(mode);
            }
            let translucent = mode.is_translucent();
            // Index values were validated at parse: every index is below the
            // group's vertex count, which `pos`, `col`, `code` and `verts` all
            // have, and `next + 3` or `next + 4` stays inside `indices` because
            // the run counts add up to the group's primitive counts.
            if run.kind == RUN_QUADS {
                let opcode = if translucent {
                    QUAD_TRANSLUCENT
                } else {
                    QUAD_OPAQUE
                };
                for _ in 0..run.count {
                    let q = [
                        indices[next] as usize,
                        indices[next + 1] as usize,
                        indices[next + 2] as usize,
                        indices[next + 3] as usize,
                    ];
                    next += 4;
                    // SAFETY: see above.
                    let (p, c) = unsafe { self.gather4::<RECOLOR>(q) };
                    if CLIP {
                        // SAFETY: see above.
                        let k = unsafe {
                            [
                                *self.code.get_unchecked(q[0]),
                                *self.code.get_unchecked(q[1]),
                                *self.code.get_unchecked(q[2]),
                                *self.code.get_unchecked(q[3]),
                            ]
                        };
                        emitted += emit_quad_clipped(opcode, p, c, k, clip, sink);
                    } else {
                        let quad = QuadGouraud {
                            tag: 0,
                            color0_cmd: opcode | c[0],
                            v0: p[0],
                            color1: c[1],
                            v1: p[1],
                            color2: c[2],
                            v2: p[2],
                            color3: c[3],
                            v3: p[3],
                        };
                        sink(Emit::Quad(&quad));
                        emitted += 2;
                    }
                }
            } else {
                let opcode = if translucent {
                    OPCODE_TRANSLUCENT
                } else {
                    OPCODE_OPAQUE
                };
                for _ in 0..run.count {
                    let t = [
                        indices[next] as usize,
                        indices[next + 1] as usize,
                        indices[next + 2] as usize,
                    ];
                    next += 3;
                    // SAFETY: see above.
                    let (p, c) = unsafe { self.gather3::<RECOLOR>(t) };
                    if CLIP {
                        // SAFETY: see above.
                        let k = unsafe {
                            [
                                *self.code.get_unchecked(t[0]),
                                *self.code.get_unchecked(t[1]),
                                *self.code.get_unchecked(t[2]),
                            ]
                        };
                        emitted += emit_tri_clipped(opcode, p, c, k, clip, sink);
                    } else {
                        let tri = TriGouraud {
                            tag: 0,
                            color0_cmd: opcode | c[0],
                            v0: p[0],
                            color1: c[1],
                            v1: p[1],
                            color2: c[2],
                            v2: p[2],
                        };
                        sink(Emit::Triangle(&tri));
                        emitted += 1;
                    }
                }
            }
        }
        emitted
    }

    /// Position and colour words of three vertices.
    ///
    /// # Safety
    ///
    /// Every index is below the group's vertex count.
    #[inline(always)]
    unsafe fn gather3<const RECOLOR: bool>(&self, i: [usize; 3]) -> ([u32; 3], [u32; 3]) {
        // SAFETY: forwarded contract.
        unsafe {
            let c = |k: usize| {
                if RECOLOR {
                    *self.col.get_unchecked(k)
                } else {
                    self.verts.get_unchecked(k).color
                }
            };
            (
                [
                    *self.pos.get_unchecked(i[0]),
                    *self.pos.get_unchecked(i[1]),
                    *self.pos.get_unchecked(i[2]),
                ],
                [c(i[0]), c(i[1]), c(i[2])],
            )
        }
    }

    /// Position and colour words of four vertices.
    ///
    /// # Safety
    ///
    /// Every index is below the group's vertex count.
    #[inline(always)]
    unsafe fn gather4<const RECOLOR: bool>(&self, i: [usize; 4]) -> ([u32; 4], [u32; 4]) {
        // SAFETY: forwarded contract.
        unsafe {
            let c = |k: usize| {
                if RECOLOR {
                    *self.col.get_unchecked(k)
                } else {
                    self.verts.get_unchecked(k).color
                }
            };
            (
                [
                    *self.pos.get_unchecked(i[0]),
                    *self.pos.get_unchecked(i[1]),
                    *self.pos.get_unchecked(i[2]),
                    *self.pos.get_unchecked(i[3]),
                ],
                [c(i[0]), c(i[1]), c(i[2]), c(i[3])],
            )
        }
    }
}

/// Emit one triangle clipped to `clip`, deciding from the vertex outcodes
/// `k`; returns the GPU triangles drawn from it.
#[inline(always)]
fn emit_tri_clipped(
    opcode: u32,
    pos: [u32; 3],
    col: [u32; 3],
    k: [u8; 3],
    clip: ClipRect,
    sink: &mut impl FnMut(Emit<'_>),
) -> u32 {
    if k[0] & k[1] & k[2] & 0x0F != 0 {
        return 0;
    }
    let cut = (k[0] | k[1] | k[2]) >> 4;
    if cut == 0 {
        let packet = TriGouraud {
            tag: 0,
            color0_cmd: opcode | col[0],
            v0: pos[0],
            color1: col[1],
            v1: pos[1],
            color2: col[2],
            v2: pos[2],
        };
        sink(Emit::Triangle(&packet));
        return 1;
    }
    clip_and_emit(opcode, pos, col, cut, clip, sink)
}

/// The slow path: cut a triangle that crosses the edges in `cut` and emit the
/// fan. Kept out of line so the common accept path stays small.
#[inline(never)]
fn clip_and_emit(
    opcode: u32,
    pos: [u32; 3],
    col: [u32; 3],
    cut: u8,
    clip: ClipRect,
    sink: &mut impl FnMut(Emit<'_>),
) -> u32 {
    let tri = [
        ClipVertex::from_words(pos[0], col[0]),
        ClipVertex::from_words(pos[1], col[1]),
        ClipVertex::from_words(pos[2], col[2]),
    ];
    let (poly, len) = clip_triangle(tri, clip, cut);
    if len < 3 {
        return 0;
    }
    for i in 1..len - 1 {
        let (a, b, c) = (poly[0], poly[i], poly[i + 1]);
        let packet = TriGouraud {
            tag: 0,
            color0_cmd: opcode | a.color_word(),
            v0: a.pos_word(),
            color1: b.color_word(),
            v1: b.pos_word(),
            color2: c.color_word(),
            v2: c.pos_word(),
        };
        sink(Emit::Triangle(&packet));
    }
    1
}

/// Emit one quad clipped to `clip`. A quad wholly inside goes out whole; one
/// that crosses an edge is drawn as the two triangles the GPU would split it
/// into, each clipped on its own.
#[inline(always)]
fn emit_quad_clipped(
    opcode: u32,
    pos: [u32; 4],
    col: [u32; 4],
    k: [u8; 4],
    clip: ClipRect,
    sink: &mut impl FnMut(Emit<'_>),
) -> u32 {
    if k[0] & k[1] & k[2] & k[3] & 0x0F != 0 {
        return 0;
    }
    if (k[0] | k[1] | k[2] | k[3]) >> 4 == 0 {
        let packet = QuadGouraud {
            tag: 0,
            color0_cmd: opcode | col[0],
            v0: pos[0],
            color1: col[1],
            v1: pos[1],
            color2: col[2],
            v2: pos[2],
            color3: col[3],
            v3: pos[3],
        };
        sink(Emit::Quad(&packet));
        return 2;
    }
    // The triangle opcode differs from the quad opcode only in the quad bit.
    let tri_opcode = opcode & !(1 << 27);
    emit_tri_clipped(
        tri_opcode,
        [pos[0], pos[1], pos[2]],
        [col[0], col[1], col[2]],
        [k[0], k[1], k[2]],
        clip,
        sink,
    ) + emit_tri_clipped(
        tri_opcode,
        [pos[1], pos[2], pos[3]],
        [col[1], col[2], col[3]],
        [k[1], k[2], k[3]],
        clip,
        sink,
    )
}
