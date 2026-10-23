//! Guest side of the sprite stance HUD: put the atlas and palettes in VRAM once, build the emblem,
//! the pills and the mask packets once, lay the two bars out whenever what they show changes, and
//! every frame link the packets into an ordering table.
//!
//! Each packet carries its own draw mode, so it is correct wherever an ordering table walk puts it:
//! a rectangle is three words (four when translucent), a sprite is six (draw mode, texture window
//! reset, then the textured rectangle command), a flat quad five (six when translucent) and a
//! textured quad ten (texture window reset, then the command with its four corners and their texture
//! coordinates). Include this file after `stance_hud_sprites.rs` as `hud_sprites`.
//!
//! A bar at rest is rectangles and sprites, the art drawn 1:1, which is what the GPU draws fastest.
//! A bar on the move is quads, the same pieces placed wherever the scene puts them, so a swap is
//! smooth at any frame rate. The scene code of `stance_hud_sprites.rs` decides where each bar goes
//! and in which order, the same code the host viewer paints from, so the result is pixel for pixel
//! what `compose` says. Both kinds live in the same two sets of nodes, laid out again only when the
//! state they depend on changes: a frame at rest does no layout.

use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::prim::{QuadFlat, QuadTexturedMaterial, RectFlat, Sprite};
use psx_vram::{upload_16bpp, Clut, TextureDepth, TexturePage, VramRect};

use super::hud_sprites::{
    bar_at, bar_bounds, emblem_op, fold_at, gem_clut, gem_op, gems_shown, pill_state, scene,
    EmblemLook, Fold, HudState, Op, Part, Placement, Scene, Stance, ATLAS, ATLAS_H, ATLAS_HW_W,
    CLUTS, CLUT_COUNT, COUNT, FX_SLOTS, MASK_CHECK, MASK_OFF, MASK_SET, MASK_SET_CHECK,
};

/// Where the HUD's texels and palettes live in VRAM.
#[derive(Clone, Copy)]
pub struct HudVram {
    /// The 4-bit page holding the atlas.
    pub page: TexturePage,
    /// Where the atlas's top-left corner is in the page, in texels (a multiple of 4 across).
    pub atlas_u: u8,
    pub atlas_v: u8,
    /// The CLUT word of each palette of [`CLUTS`], in table order: a palette is 16 entries, and
    /// they need not be next to each other.
    pub clut_words: [u16; CLUT_COUNT],
}

impl HudVram {
    /// Page 15, top half (right of the display buffers), and the palette strip (36 palettes, to x 896) under the second
    /// display buffer (palettes of 16 entries side by side): free in the showcase.
    pub const SHOWCASE: Self = Self::strip(TexturePage::new(960, 0, TextureDepth::Bit4), 320, 480);

    /// Atlas at the top-left of `page` and the palettes side by side from `(clut_x, clut_y)`.
    pub const fn strip(page: TexturePage, clut_x: u16, clut_y: u16) -> Self {
        let mut clut_words = [0u16; CLUT_COUNT];
        let mut i = 0;
        while i < CLUT_COUNT {
            clut_words[i] = Clut::new(clut_x + 16 * i as u16, clut_y).uv_word();
            i += 1;
        }
        Self {
            page,
            atlas_u: 0,
            atlas_v: 0,
            clut_words,
        }
    }

    /// Copy the atlas and the palettes into VRAM, the palettes as one strip starting where the first
    /// palette's word says. Do this once, before the first draw. (The game puts them where its VRAM
    /// allocator says and uses the allocator's own upload instead.)
    pub fn upload(&self) {
        upload_16bpp(
            VramRect::new(
                self.page.x() + u16::from(self.atlas_u) / 4,
                self.page.y() + u16::from(self.atlas_v),
                ATLAS_HW_W as u16,
                ATLAS_H as u16,
            ),
            &ATLAS,
        );
        // A CLUT word is the palette's x in 16 pixel steps in its low 6 bits, the row above.
        let first = self.clut_words[0];
        upload_16bpp(
            VramRect::new((first & 0x3f) * 16, first >> 6, (CLUT_COUNT * 16) as u16, 1),
            &CLUTS,
        );
    }

    fn clut_word(&self, clut: usize) -> u16 {
        self.clut_words[clut]
    }
}

/// GP0 E6, the mask bit setting: bit 0 marks the pixels drawn, bit 1 skips marked pixels.
const MASK_SETTING: u32 = 0xE600_0000;

/// The payload length in the tag word. The low 24 bits hold the link the last ordering-table walk
/// left there and must not be passed on as the length of the next one.
const TAG_LEN: u32 = 0xFF00_0000;

/// Bit 25 of a draw command: semi-transparent.
const SEMI_TRANSPARENT: u32 = 1 << 25;

/// One ordering-table node: the tag (the payload length in its high byte), and up to six payload
/// words.
#[derive(Clone, Copy)]
#[repr(C, align(4))]
pub struct Node {
    tag: u32,
    words: [u32; 6],
}

impl Node {
    const ZERO: Self = Self {
        tag: 0,
        words: [0; 6],
    };

    /// Payload words, from the tag's high byte.
    fn len(&self) -> u8 {
        (self.tag >> 24) as u8
    }

    /// As [`QuadNode::assign`].
    fn assign(&mut self, new: &Self) {
        self.words = new.words;
        self.tag = (self.tag & !TAG_LEN) | (new.tag & TAG_LEN);
    }

    /// Point a sprite packet at another palette: the palette is the top half of the texture
    /// coordinate word, the one after the position.
    fn set_clut(&mut self, clut_word: u16) {
        if self.len() == 6 {
            self.words[4] = (self.words[4] & 0xFFFF) | ((clut_word as u32) << 16);
        }
    }

    /// Make this sprite packet draw `op` (a sprite): the position, the texels, the palette and
    /// whether it blends with the scene. The window word stays as it is.
    fn set_sprite(&mut self, op: Op, atlas: (u8, u8), vram: &HudVram, modes: (u32, u32, u32)) {
        if let Op::Sprite {
            x,
            y,
            src,
            clut,
            glass,
        } = op
        {
            let (plain, glass_mode, cmd) = modes;
            self.words[0] = if glass { glass_mode } else { plain };
            self.words[2] = if glass { cmd | SEMI_TRANSPARENT } else { cmd };
            self.words[3] = ((y as i16 as u16 as u32) << 16) | (x as i16 as u16 as u32);
            self.words[4] = u32::from(src.u + atlas.0)
                | (u32::from(src.v + atlas.1) << 8)
                | (u32::from(vram.clut_word(clut)) << 16);
            self.words[5] = ((src.h as u32) << 16) | src.w as u32;
        }
    }
}

/// One ordering-table node of a quad: the tag and up to ten payload words (a textured quad is the
/// texture window reset, the command, and four vertices with their texture coordinates).
#[derive(Clone, Copy)]
#[repr(C, align(4))]
pub struct QuadNode {
    tag: u32,
    words: [u32; 10],
}

impl QuadNode {
    const ZERO: Self = Self {
        tag: 0,
        words: [0; 10],
    };

    /// A sprite or rectangle packet in a node big enough for a quad: the bars at rest are made of
    /// those, and take the same room as when they are quads on the move.
    fn from_node(n: &Node) -> Self {
        let mut q = Self::ZERO;
        q.tag = n.tag;
        q.words[..6].copy_from_slice(&n.words);
        q
    }

    /// Take the words and length of `new` and keep this node's link to the next one. A walk that
    /// started before this frame's layout (the last frame's list may still be going) reads the old
    /// links, which are rewritten with the same values, so the nodes it reaches are the same nodes.
    fn assign(&mut self, new: &Self) {
        self.words = new.words;
        self.tag = (self.tag & !TAG_LEN) | (new.tag & TAG_LEN);
    }

    /// A node of whatever `op` asks for.
    fn of(conv: &Converter, op: Op) -> Self {
        match op {
            Op::Quad { .. } | Op::TexQuad { .. } => conv.quad_node(op),
            _ => Self::from_node(&conv.node(op)),
        }
    }
}

/// Words that do not depend on the part, worked out once per build.
#[derive(Clone, Copy)]
struct Converter {
    vram: HudVram,
    page: u16,
    plain_mode: u32,
    glass_mode: u32,
    /// The draw mode that adds the colour to the scene: with a black colour it changes nothing but
    /// the mask bit.
    add_mode: u32,
    window: u32,
    sprite_cmd: u32,
}

impl Converter {
    const ZERO: Self = Self {
        vram: HudVram::SHOWCASE,
        page: 0,
        plain_mode: 0,
        glass_mode: 0,
        add_mode: 0,
        window: 0,
        sprite_cmd: 0,
    };

    /// The node of a quad op (a flat quad or a textured one). The quad is drawn as the GPU's
    /// polygon: half-open texture coordinates, so that one texel is one pixel at full size.
    #[inline(never)]
    fn quad_node(&self, op: Op) -> QuadNode {
        let mut node = QuadNode::ZERO;
        match op {
            Op::Quad { p, rgb, glass } => {
                let q = QuadFlat::new(p, rgb[0], rgb[1], rgb[2]);
                let mut at = 0;
                if glass {
                    // Semi-transparent: the draw mode carries the equation.
                    node.words[0] = self.glass_mode;
                    at = 1;
                }
                let cmd = if glass {
                    q.color_cmd | SEMI_TRANSPARENT
                } else {
                    q.color_cmd
                };
                node.words[at..at + 5].copy_from_slice(&[cmd, q.v0, q.v1, q.v2, q.v3]);
                node.tag = (at as u32 + 5) << 24;
            }
            Op::TexQuad {
                p,
                src,
                clut,
                glass,
            } => {
                let clut_word = self.vram.clut_word(clut);
                let material = if glass {
                    TextureMaterial::blended(
                        clut_word,
                        self.page,
                        (0x80, 0x80, 0x80),
                        BlendMode::Average,
                    )
                } else {
                    TextureMaterial::new(clut_word, self.page)
                };
                let u0 = src.u + self.vram.atlas_u;
                let v0 = src.v + self.vram.atlas_v;
                let (u1, v1) = (u0.saturating_add(src.w), v0.saturating_add(src.h));
                let q = QuadTexturedMaterial::with_material(
                    p,
                    [(u0, v0), (u1, v0), (u0, v1), (u1, v1)],
                    material,
                );
                node.words = [
                    q.tex_window,
                    q.color_cmd,
                    q.v0,
                    q.uv0_clut,
                    q.v1,
                    q.uv1_tpage,
                    q.v2,
                    q.uv2,
                    q.v3,
                    q.uv3,
                ];
                node.tag = 10 << 24;
            }
            Op::Rect { .. } | Op::Sprite { .. } | Op::Mask { .. } => {}
        }
        node
    }

    /// The rectangle over `bounds` (left, top, right, bottom) that clears the mask bit: black
    /// added to the scene is the scene, and the pixel is written without the bit.
    fn clear_node(&self, bounds: [i32; 4]) -> Node {
        let (x, y) = (bounds[0].max(0), bounds[1].max(0));
        let (w, h) = ((bounds[2] - x).max(1), (bounds[3] - y).max(1));
        let r = RectFlat::new(x as i16, y as i16, w as u16, h as u16, 0, 0, 0);
        Node {
            tag: 4 << 24,
            words: [
                self.add_mode,
                r.color_cmd | SEMI_TRANSPARENT,
                r.xy,
                r.wh,
                0,
                0,
            ],
        }
    }

    fn new(vram: &HudVram) -> Self {
        let page = vram.page.uv_word(0);
        let plain = TextureMaterial::new(0, page);
        let average = TextureMaterial::blended(0, page, (0x80, 0x80, 0x80), BlendMode::Average);
        Self {
            vram: *vram,
            page,
            plain_mode: plain.draw_mode_word(),
            glass_mode: average.draw_mode_word(),
            add_mode: TextureMaterial::blended(0, page, (0x80, 0x80, 0x80), BlendMode::Add)
                .draw_mode_word(),
            window: plain.texture_window_word(),
            sprite_cmd: Sprite::with_material(0, 0, 1, 1, (0, 0), plain).color_cmd,
        }
    }

    #[inline(never)]
    fn node(&self, op: Op) -> Node {
        match op {
            Op::Rect {
                x,
                y,
                w,
                h,
                rgb,
                glass,
            } => {
                let r = RectFlat::new(
                    x as i16, y as i16, w as u16, h as u16, rgb[0], rgb[1], rgb[2],
                );
                if glass {
                    // Semi-transparent rectangle: the draw mode carries the equation.
                    Node {
                        tag: 4 << 24,
                        words: [
                            self.glass_mode,
                            r.color_cmd | SEMI_TRANSPARENT,
                            r.xy,
                            r.wh,
                            0,
                            0,
                        ],
                    }
                } else {
                    Node {
                        tag: 3 << 24,
                        words: [r.color_cmd, r.xy, r.wh, 0, 0, 0],
                    }
                }
            }
            // Quads are `QuadNode`s, see `quad_node`.
            Op::Quad { .. } | Op::TexQuad { .. } => Node::ZERO,
            Op::Mask { set, check } => Node {
                tag: 1 << 24,
                words: [
                    MASK_SETTING | set as u32 | (check as u32) << 1,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
            },
            Op::Sprite {
                x,
                y,
                src,
                clut,
                glass,
            } => {
                let material = TextureMaterial::new(self.vram.clut_word(clut), self.page);
                let s = Sprite::with_material(
                    x as i16,
                    y as i16,
                    src.w as u16,
                    src.h as u16,
                    (src.u + self.vram.atlas_u, src.v + self.vram.atlas_v),
                    material,
                );
                Node {
                    tag: 6 << 24,
                    words: [
                        if glass {
                            self.glass_mode
                        } else {
                            self.plain_mode
                        },
                        self.window,
                        if glass {
                            s.color_cmd | SEMI_TRANSPARENT
                        } else {
                            s.color_cmd
                        },
                        s.xy,
                        s.uv_clut,
                        s.wh,
                    ],
                }
            }
        }
    }
}

/// Translucent bands (the well, and the outline and shadow in the new styles) and two ends; the most
/// any size has.
const GLASS_MAX: usize = 7;
/// Opaque bands: the most any size has (the art has nine bands a size, four of them opaque).
const OPAQUE_MAX: usize = 4;

/// One bar, as quads: made again whenever what it shows changes (a frame of a swap, a step of the
/// fold, a point of health).
#[derive(Clone, Copy)]
struct MovingBar {
    glass: [QuadNode; GLASS_MAX],
    glass_len: usize,
    opaque: [QuadNode; OPAQUE_MAX],
    opaque_len: usize,
    cells: [QuadNode; COUNT as usize],
    cells_len: usize,
}

impl MovingBar {
    const NONE: Self = Self {
        glass: [QuadNode::ZERO; GLASS_MAX],
        glass_len: 0,
        opaque: [QuadNode::ZERO; OPAQUE_MAX],
        opaque_len: 0,
        cells: [QuadNode::ZERO; COUNT as usize],
        cells_len: 0,
    };

    /// Lay out the bar of `stance` at `at`, `lit` cells lit, folded as `fold` says: sprites and
    /// rectangles at rest, quads on the move.
    #[inline(never)]
    fn fill(
        &mut self,
        conv: &Converter,
        origin: (i32, i32),
        stance: Stance,
        at: &Placement,
        lit: u8,
        fold: Fold,
    ) {
        // One closure for the three parts, so the layout code is in the program once.
        for part in [Part::Glass, Part::Opaque, Part::Cells] {
            let (nodes, len): (&mut [QuadNode], &mut usize) = match part {
                Part::Glass => (&mut self.glass[..], &mut self.glass_len),
                Part::Opaque => (&mut self.opaque[..], &mut self.opaque_len),
                _ => (&mut self.cells[..], &mut self.cells_len),
            };
            *len = 0;
            bar_at(origin, stance, at, lit, fold, part, &mut |op| {
                nodes[*len].assign(&QuadNode::of(conv, op));
                *len += 1;
            });
        }
    }
}

/// Every packet of the HUD, built once by [`HudPackets::build`] and linked each frame by
/// [`HudPackets::link`].
pub struct HudPackets {
    /// The emblem of each stance, halfway between them, and the flash of each stance.
    emblem: [Node; 5],
    /// The pills, as they are when lit or empty.
    gems: [Node; COUNT as usize],
    /// Sprites for the effects: a pool of five for each pill that is in one, and four for the
    /// quadrants of the pill that is being recharged. They are linked over their pill's own node.
    effects: [[Node; 5]; FX_SLOTS],
    partial: [Node; 4],
    masks: [Node; 4],
    /// The bars. The list of a frame may still be walked while the next frame is built, so they are
    /// laid out again over the same nodes without ever touching the links between them: see
    /// [`QuadNode::assign`].
    set: BarSet,
    conv: Converter,
    origin: (i32, i32),
    /// Where the texels and palettes are, and the draw modes and command of a sprite packet, for
    /// patching the effect sprites.
    vram: HudVram,
    modes: (u32, u32, u32),
    /// The state the bars and pills were last set up for: at rest a frame skips all of that.
    shown: Option<HudState>,
    /// For each pill: what is over it (the pool: an effect slot, or [`PARTIAL`], or none) and how
    /// many sprites; and whether its own node is patched to the lit palette.
    over: [(u8, u8); COUNT as usize],
    gem_lit: [bool; COUNT as usize],
}

/// The two bars, front and behind, with what they were laid out for and the rectangle that clears
/// the mask bit under the bar behind.
#[derive(Clone, Copy)]
struct BarSet {
    bars: [MovingBar; 2],
    /// A black rectangle added to the scene over the bar behind: it changes no colour and clears the
    /// mask bit that the world's own pixels may carry there (opaque black texels are drawn with it),
    /// which would make the bar skip those pixels.
    clear: Node,
    /// What the bars were laid out for: they are laid out again when this changes.
    for_key: Option<BarKey>,
}

impl BarSet {
    const NONE: Self = Self {
        bars: [MovingBar::NONE; 2],
        clear: Node::ZERO,
        for_key: None,
    };
}

/// What decides how the bars look: everything of the state that is not the pills.
#[derive(Clone, Copy, PartialEq, Eq)]
struct BarKey {
    stance: Stance,
    health: [u8; 2],
    swap: u16,
    reveal: [u8; 2],
}

/// The pool of the pill being recharged, in [`HudPackets::over`].
const PARTIAL: u8 = FX_SLOTS as u8;
/// No pool.
const NO_POOL: u8 = u8::MAX;

impl HudPackets {
    /// An empty set, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            emblem: [Node::ZERO; 5],
            gems: [Node::ZERO; COUNT as usize],
            effects: [[Node::ZERO; 5]; FX_SLOTS],
            partial: [Node::ZERO; 4],
            masks: [Node::ZERO; 4],
            set: BarSet::NONE,
            conv: Converter::ZERO,
            origin: (0, 0),
            vram: HudVram::SHOWCASE,
            modes: (0, 0, 0),
            shown: None,
            over: [(NO_POOL, 0); COUNT as usize],
            gem_lit: [false; COUNT as usize],
        }
    }

    /// Set up the packets for the HUD with its top-left at `origin`: the emblem, the pills and the
    /// masks are built here, once, and again when the HUD moves. The bars, the pills and the sprites
    /// the effects on them are drawn with are set up from the state in each [`HudPackets::link`].
    pub fn build(&mut self, vram: &HudVram, origin: (i32, i32)) {
        let conv = Converter::new(vram);
        self.conv = conv;
        self.origin = origin;
        self.vram = *vram;
        self.modes = (conv.plain_mode, conv.glass_mode, conv.sprite_cmd);
        self.shown = None;
        self.set = BarSet::NONE;
        for k in 0..COUNT {
            self.gems[k as usize] = conv.node(gem_op(origin, k, false));
        }
        self.gem_lit = [false; COUNT as usize];
        let probe = conv.node(gem_op(origin, 0, false));
        self.effects = [[probe; 5]; FX_SLOTS];
        self.partial = [probe; 4];
        for (i, look) in [
            EmblemLook::Of(Stance::Horizon),
            EmblemLook::Of(Stance::Zenith),
            EmblemLook::Halfway,
            EmblemLook::Pulse(Stance::Horizon),
            EmblemLook::Pulse(Stance::Zenith),
        ]
        .into_iter()
        .enumerate()
        {
            self.emblem[i] = conv.node(emblem_op(origin, look));
        }
        for (i, op) in [MASK_SET, MASK_SET_CHECK, MASK_CHECK, MASK_OFF]
            .into_iter()
            .enumerate()
        {
            self.masks[i] = conv.node(op);
        }
    }

    /// Lay out the bars of set `at` for `state` when they are not as it asks.
    #[inline(never)]
    fn lay_out(&mut self, state: &HudState, sc: &Scene) {
        let key = BarKey {
            stance: state.stance,
            health: state.health,
            swap: state.swap,
            reveal: state.reveal,
        };
        let (origin, conv) = (self.origin, self.conv);
        let set = &mut self.set;
        if set.for_key == Some(key) {
            return;
        }
        set.for_key = Some(key);
        for (i, (stance, xf)) in [sc.front, sc.behind].into_iter().enumerate() {
            if let Some(fold) = fold_at(state.reveal[stance.index()], &xf) {
                let lit = state.health[stance.index()].min(COUNT as u8);
                set.bars[i].fill(&conv, origin, stance, &xf, lit, fold);
                if i == 1 {
                    set.clear
                        .assign(&conv.clear_node(bar_bounds(origin, &xf, &fold)));
                }
            }
        }
    }

    /// Set the packets up for `state`: patch the pills and the sprites of their effects.
    #[inline(never)]
    fn set_up(&mut self, state: &HudState) {
        let origin = self.origin;
        let vram = self.vram;
        let atlas = (vram.atlas_u, vram.atlas_v);
        for k in 0..COUNT as usize {
            let extras = pill_state(origin, state, k as i32);
            if extras.lit != self.gem_lit[k] {
                self.gems[k].set_clut(vram.clut_word(gem_clut(extras.lit)));
                self.gem_lit[k] = extras.lit;
            }
            let pool = match state.fx.iter().position(|f| f.pill() == Some(k as u8)) {
                Some(s) => &mut self.effects[s][..],
                None if extras.n > 0 => &mut self.partial[..],
                None => &mut self.partial[..0],
            };
            for (node, op) in pool.iter_mut().zip(&extras.ops[..extras.n]) {
                node.set_sprite(*op, atlas, &vram, self.modes);
            }
            self.over[k] = (
                match state.fx.iter().position(|f| f.pill() == Some(k as u8)) {
                    Some(s) => s as u8,
                    None if extras.n > 0 => PARTIAL,
                    None => NO_POOL,
                },
                extras.n as u8,
            );
        }
    }

    /// Place the two bars for `state` (not again while the state stays the same) and hand every
    /// packet of the HUD to `add` as `(pointer, payload words)`, in the order to link them: back to
    /// front, so that within one ordering-table slot the first packet is walked first. A bar that is
    /// folded away, and the pills that are, are left out.
    ///
    /// The packets must stay unmodified until the walk that uses them ends.
    pub fn link(&mut self, state: &HudState, mut add: impl FnMut(*mut u32, u32)) {
        let sc = scene(state);
        if self.shown != Some(*state) {
            self.set_up(state);
            self.shown = Some(*state);
        }
        self.lay_out(state, &sc);
        let emblem = match sc.emblem {
            EmblemLook::Of(s) => s.index(),
            EmblemLook::Halfway => 2,
            EmblemLook::Pulse(s) => 3 + s.index(),
        };
        let pills = if state.ammo_hint {
            COUNT as usize
        } else {
            gems_shown(state.ammo_reveal) as usize
        };
        // Paint order, as `compose` has it, walked backwards because a slot is walked last in,
        // first out: the pills (each with what is drawn over it), the emblem, the front bar's
        // opaque parts and cells, the mask off, the back bar's plate, the mask set to check, the
        // back bar's cells, the mask set and checked, the front bar's translucent parts, the mask
        // set.
        // A macro, not a closure, so that every packet is a plain store with no call in between.
        macro_rules! add_all {
            ($nodes:expr) => {
                for n in $nodes.iter().rev() {
                    add(
                        core::ptr::from_ref(n).cast::<u32>().cast_mut(),
                        n.tag & TAG_LEN,
                    );
                }
            };
        }
        let mut k = pills;
        while k > 0 {
            k -= 1;
            let (pool, n) = (self.over[k].0, self.over[k].1 as usize);
            if pool < PARTIAL {
                add_all!(&self.effects[pool as usize][..n]);
            } else if pool == PARTIAL {
                add_all!(&self.partial[..n]);
            }
            add_all!(core::slice::from_ref(&self.gems[k]));
        }
        add_all!(core::slice::from_ref(&self.emblem[emblem]));
        let front_fold = fold_at(state.reveal[sc.front.0.index()], &sc.front.1);
        let behind_fold = fold_at(state.reveal[sc.behind.0.index()], &sc.behind.1);
        let (front, behind) = (&self.set.bars[0], &self.set.bars[1]);
        if front_fold.is_some() {
            add_all!(&front.cells[..front.cells_len]);
            add_all!(&front.opaque[..front.opaque_len]);
        }
        add_all!(&self.masks[3..4]);
        if behind_fold.is_some() {
            add_all!(&behind.opaque[..behind.opaque_len]);
            add_all!(&behind.glass[..behind.glass_len]);
            add_all!(&self.masks[2..3]);
            add_all!(&behind.cells[..behind.cells_len]);
        } else {
            add_all!(&self.masks[2..3]);
        }
        add_all!(&self.masks[1..2]);
        if front_fold.is_some() {
            add_all!(&front.glass[..front.glass_len]);
        }
        add_all!(&self.masks[0..1]);
        // Walked first: the world's pixels under the bar behind may carry the mask bit (opaque
        // black texels are drawn with it), and a pixel that carries it is skipped by that bar.
        if behind_fold.is_some() {
            add_all!(core::slice::from_ref(&self.set.clear));
        }
    }
}

impl Default for HudPackets {
    fn default() -> Self {
        Self::new()
    }
}

/// One bar of one size on its own, as packets, for checking a size by itself.
pub struct AlonePackets {
    nodes: [Node; 2 * GLASS_MAX + OPAQUE_MAX + COUNT as usize],
    len: usize,
}

impl AlonePackets {
    /// An empty set, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            nodes: [Node::ZERO; 2 * GLASS_MAX + OPAQUE_MAX + COUNT as usize],
            len: 0,
        }
    }

    /// Build the bar of `stance` at `size` with `lit` cells lit, its origin at `origin`.
    pub fn build(
        &mut self,
        vram: &HudVram,
        origin: (i32, i32),
        stance: Stance,
        size: usize,
        lit: u8,
    ) {
        let conv = Converter::new(vram);
        self.len = 0;
        let (nodes, len) = (&mut self.nodes, &mut self.len);
        super::hud_sprites::bar_alone(origin, stance, size, lit, &mut |op| {
            nodes[*len] = conv.node(op);
            *len += 1;
        });
    }

    /// Hand the packets to `add` in link order (see [`HudPackets::link`]).
    pub fn link(&mut self, mut add: impl FnMut(*mut u32, u8)) {
        for n in self.nodes[..self.len].iter_mut().rev() {
            add((n as *mut Node).cast::<u32>(), n.len());
        }
    }
}

impl Default for AlonePackets {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Grow `bounds` (left, top, right, bottom) over the pixels `op` covers.
    fn grow(bounds: &mut [i32; 4], op: Op) {
        let mut cover = |x0: i32, y0: i32, x1: i32, y1: i32| {
            bounds[0] = bounds[0].min(x0);
            bounds[1] = bounds[1].min(y0);
            bounds[2] = bounds[2].max(x1);
            bounds[3] = bounds[3].max(y1);
        };
        match op {
            Op::Rect { x, y, w, h, .. } => cover(x, y, x + w, y + h),
            Op::Sprite { x, y, src, .. } => cover(x, y, x + i32::from(src.w), y + i32::from(src.h)),
            Op::Quad { p, .. } | Op::TexQuad { p, .. } => {
                for (x, y) in p {
                    cover(
                        i32::from(x),
                        i32::from(y),
                        i32::from(x) + 1,
                        i32::from(y) + 1,
                    );
                }
            }
            Op::Mask { .. } => {}
        }
    }

    use crate::hud_sprites::{
        compose, AmmoFx, HudAnim, PillFx, BREAK_FRAMES, ENERGY_PER_PILL, LOAD_FRAMES, REVEAL_MAX,
    };

    /// The words of the packets a `link` hands over, in paint order, sorted: the order of parts that
    /// do not overlap does not matter.
    fn linked(hud: &mut HudPackets, state: &HudState) -> Vec<Vec<u32>> {
        let mut out = Vec::new();
        let add_mode = hud.conv.add_mode;
        hud.link(state, |p, tag| {
            let words = (tag >> 24) as usize;
            // SAFETY: the pointer is to a node of `hud`, which has six payload words after the tag.
            let payload = unsafe { core::slice::from_raw_parts((p as *const u32).add(1), words) };
            // The rectangle that clears the mask bit is not part of the composition.
            if !(words == 4 && payload[0] == add_mode) {
                out.push(payload.to_vec());
            }
        });
        out.sort();
        out
    }

    /// The words of the packets the composition of `state` asks for, sorted.
    fn composed(vram: &HudVram, origin: (i32, i32), state: &HudState) -> Vec<Vec<u32>> {
        let conv = Converter::new(vram);
        let mut out = Vec::new();
        compose(origin, state, &mut |op| match op {
            Op::Quad { .. } | Op::TexQuad { .. } => {
                let n = conv.quad_node(op);
                out.push(n.words[..(n.tag >> 24) as usize].to_vec());
            }
            _ => {
                let n = conv.node(op);
                out.push(n.words[..n.len() as usize].to_vec());
            }
        });
        out.sort();
        out
    }

    /// A state with every part of it chosen at random from `next`.
    fn random_state(next: &mut impl FnMut(u32) -> u32) -> HudState {
        let stance = if next(2) == 0 {
            Stance::Horizon
        } else {
            Stance::Zenith
        };
        let mut state = HudState::rest(stance, [next(11) as u8, next(11) as u8], next(11) as u8);
        if next(3) == 0 {
            state.swap = next(4095) as u16 + 1;
        }
        state.reveal = [
            next(u32::from(REVEAL_MAX) + 1) as u8,
            next(u32::from(REVEAL_MAX) + 1) as u8,
        ];
        state.ammo_reveal = next(u32::from(REVEAL_MAX) + 1) as u8;
        state.pulse = next(4) == 0;
        state.ammo_hint = next(4) == 0;
        state.partial = next(4) as u8;
        for slot in state.fx.iter_mut() {
            let (pill, wait) = (next(COUNT as u32) as u8, next(4) as u8);
            *slot = match next(3) {
                0 => PillFx::Idle,
                1 => PillFx::Break {
                    pill,
                    frame: next(u32::from(BREAK_FRAMES)) as u8,
                    wait,
                },
                _ => PillFx::Load {
                    pill,
                    frame: next(u32::from(LOAD_FRAMES)) as u8,
                    wait,
                },
            };
        }
        state
    }

    /// Whatever the history of states a HUD went through (swap frames, every fold level of each
    /// bar, the pills, their effects, the flash, the hint), its packets are the ones the
    /// composition asks for. One HUD is taken through a scrambled sequence, so the patches pile up
    /// the way they do in play.
    #[test]
    fn linked_packets_equal_the_composition_in_any_order() {
        let vram = HudVram::SHOWCASE;
        let origin = (6, 160);
        let mut hud = HudPackets::new();
        hud.build(&vram, origin);
        let mut seed = 0x1234_5678u32;
        let mut next = |n: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) % n
        };
        for i in 0..6000 {
            let state = random_state(&mut next);
            assert_eq!(
                linked(&mut hud, &state),
                composed(&vram, origin, &state),
                "step {i}: {state:?}"
            );
        }
    }

    /// The fold animation takes the front bar through every step in order and ends with the bar
    /// behind two frames later, and the emblem flashes for the first two frames of an unfold.
    #[test]
    fn the_fold_animation_steps_in_order() {
        let mut anim = HudAnim::open();
        let mut state = HudState::rest(Stance::Horizon, [10, 10], 10);
        let mut fronts = Vec::new();
        for _ in 0..10 {
            anim.step(false, Stance::Horizon);
            anim.apply(&mut state);
            fronts.push((state.reveal, state.ammo_reveal, state.pulse));
        }
        assert_eq!(fronts[0], ([7, 8], 7, false));
        assert_eq!(fronts[1], ([6, 8], 6, false));
        assert_eq!(fronts[2], ([5, 7], 5, false));
        assert_eq!(fronts[7], ([0, 2], 0, false));
        assert_eq!(fronts[9], ([0, 0], 0, false));
        assert!(!anim.moving());
        let mut pulses = 0;
        for _ in 0..10 {
            anim.step(true, Stance::Horizon);
            anim.apply(&mut state);
            pulses += usize::from(state.pulse);
        }
        assert_eq!(pulses, 2);
        assert_eq!(state.reveal, [REVEAL_MAX; 2]);
        assert!(!anim.moving());
    }

    /// A shot breaks the two pills it spends from the right, a frame apart; a recharge that
    /// completes a pill shows its last quadrant and the echo; a jump of energy loads the pills it
    /// gains one after another; and each pill is in at most one effect.
    #[test]
    fn ammunition_effects_follow_the_energy() {
        let mut fx = AmmoFx::new(100);
        let mut state = HudState::rest(Stance::Horizon, [10, 10], 10);
        fx.step(80);
        fx.apply(&mut state);
        assert_eq!(state.energy, 8);
        let breaks: Vec<_> = state.fx.iter().filter(|f| **f != PillFx::Idle).collect();
        assert_eq!(breaks.len(), 2);
        assert!(state.fx.contains(&PillFx::Break {
            pill: 9,
            frame: 0,
            wait: 0
        }));
        assert!(state.fx.contains(&PillFx::Break {
            pill: 8,
            frame: 0,
            wait: 1
        }));
        for _ in 0..BREAK_FRAMES + 1 {
            fx.step(80);
        }
        assert!(!fx.busy());
        // Recharging a unit at a time: quadrants, then a pill completes.
        let mut quadrants = Vec::new();
        for energy in 80..=90 {
            fx.step(energy);
            fx.apply(&mut state);
            quadrants.push(state.partial);
        }
        assert_eq!(quadrants[..9], [0, 0, 0, 1, 1, 2, 2, 2, 3]);
        assert!(state.fx.contains(&PillFx::Load {
            pill: 8,
            frame: 3,
            wait: 0
        }));
        for _ in 0..LOAD_FRAMES {
            fx.step(90);
        }
        assert!(!fx.busy());
        // A jump of energy loads the pills it gains one after another, a stagger apart.
        fx.step(100);
        fx.apply(&mut state);
        assert!(state.fx.contains(&PillFx::Load {
            pill: 9,
            frame: 0,
            wait: 0
        }));
        assert_eq!(state.queued.0, state.queued.1);
        let mut fx = AmmoFx::new(40);
        fx.step(70);
        fx.apply(&mut state);
        assert_eq!(state.queued, (5, 7));
        assert!(state.fx.contains(&PillFx::Load {
            pill: 4,
            frame: 0,
            wait: 0
        }));
        fx.step(70);
        fx.apply(&mut state);
        assert_eq!(state.queued, (5, 7));
        fx.step(70);
        fx.apply(&mut state);
        assert_eq!(state.queued, (6, 7));
        assert!(state.fx.contains(&PillFx::Load {
            pill: 5,
            frame: 0,
            wait: 0
        }));
        // Spending while the recharge is under way drops the pills that never showed lit.
        fx.step(30);
        fx.apply(&mut state);
        assert!(state.queued.0 >= state.queued.1);
        let _ = ENERGY_PER_PILL;
    }

    /// The mask-clearing rectangle comes first in the walk (last in the links), covers every pixel
    /// the bar behind draws, and is only there while that bar is.
    #[test]
    fn a_rectangle_clears_the_mask_under_the_bar_behind() {
        let vram = HudVram::SHOWCASE;
        let origin = (6, 160);
        let mut hud = HudPackets::new();
        hud.build(&vram, origin);
        let mut seed = 0x9e37_79b9u32;
        let mut next = |n: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) % n
        };
        for i in 0..400 {
            let state = random_state(&mut next);
            let mut nodes = Vec::new();
            hud.link(&state, |p, tag| {
                let words = (tag >> 24) as usize;
                // SAFETY: as in `linked`.
                let payload =
                    unsafe { core::slice::from_raw_parts((p as *const u32).add(1), words) };
                nodes.push(payload.to_vec());
            });
            // Walked first is linked last.
            let clears = nodes
                .iter()
                .filter(|n| n.len() == 4 && n[0] == hud.conv.add_mode);
            let behind_shown = state.reveal[scene(&state).behind.0.index()] > 0;
            assert_eq!(
                clears.count(),
                usize::from(behind_shown),
                "step {i}: {state:?}"
            );
            if behind_shown {
                let last = nodes.last().unwrap();
                assert!(last.len() == 4 && last[0] == hud.conv.add_mode, "step {i}");
                let (x, y) = (
                    (last[2] & 0xffff) as i16 as i32,
                    (last[2] >> 16) as i16 as i32,
                );
                let (w, h) = ((last[3] & 0xffff) as i32, (last[3] >> 16) as i32);
                // Every piece of the bar behind sits inside the rectangle.
                let sc = scene(&state);
                let fold = fold_at(state.reveal[sc.behind.0.index()], &sc.behind.1).unwrap();
                let lit = state.health[sc.behind.0.index()].min(COUNT as u8);
                let mut covered = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
                for part in [Part::Glass, Part::Opaque, Part::Cells] {
                    bar_at(
                        origin,
                        sc.behind.0,
                        &sc.behind.1,
                        lit,
                        fold,
                        part,
                        &mut |op| grow(&mut covered, op),
                    );
                }
                assert!(
                    x <= covered[0].max(0)
                        && y <= covered[1].max(0)
                        && x + w >= covered[2]
                        && y + h >= covered[3],
                    "step {i}: rectangle {x} {y} {w} {h} against {covered:?}"
                );
                assert!(
                    w > 0 && h > 0 && x >= 0 && y >= 0,
                    "step {i}: {x} {y} {w} {h}"
                );
            }
        }
    }

    /// The list of a frame may still be walked while the next frame is built, and a walk follows the
    /// links between nodes: laying the bars out again never changes a link, whatever the new nodes
    /// are.
    #[test]
    fn laying_the_bars_out_again_keeps_every_link() {
        let vram = HudVram::SHOWCASE;
        let mut hud = HudPackets::new();
        hud.build(&vram, (6, 160));
        let base = HudState::rest(Stance::Horizon, [7, 5], 8);
        hud.link(&base, |_, _| {});
        // Give every node of the bars and the clearing rectangle a link, as a walk would have left.
        let mark = |hud: &mut HudPackets| {
            for (i, bar) in hud.set.bars.iter_mut().enumerate() {
                for (j, n) in bar
                    .glass
                    .iter_mut()
                    .chain(bar.opaque.iter_mut())
                    .chain(bar.cells.iter_mut())
                    .enumerate()
                {
                    n.tag = (n.tag & TAG_LEN) | 0x0010_0000 | (i as u32) << 12 | j as u32;
                }
            }
            hud.set.clear.tag = (hud.set.clear.tag & TAG_LEN) | 0x0020_0000;
        };
        mark(&mut hud);
        for swap in (1u16..4096).step_by(131) {
            let state = HudState { swap, ..base };
            hud.link(&state, |_, _| {});
            for (i, bar) in hud.set.bars.iter().enumerate() {
                for (j, n) in bar
                    .glass
                    .iter()
                    .chain(bar.opaque.iter())
                    .chain(bar.cells.iter())
                    .enumerate()
                {
                    let want = 0x0010_0000 | (i as u32) << 12 | j as u32;
                    assert_eq!(
                        n.tag & !TAG_LEN,
                        want,
                        "swap {swap}: node {i}/{j} lost its link"
                    );
                }
            }
            assert_eq!(
                hud.set.clear.tag & !TAG_LEN,
                0x0020_0000,
                "swap {swap}: clear"
            );
        }
    }
}
