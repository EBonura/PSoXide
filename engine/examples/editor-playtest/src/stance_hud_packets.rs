//! Guest side of the sprite stance HUD: put the atlas and palettes in VRAM once, build every bar's
//! GPU packets once, and every frame only move the two bars that are drawn and link the packets
//! into an ordering table.
//!
//! Each packet carries its own draw mode, so it is correct wherever an ordering table walk puts it:
//! a rectangle is three words (four when translucent), a sprite is six (draw mode, texture window
//! reset, then the textured rectangle command). Include this file after `stance_hud_sprites.rs` as
//! `hud_sprites`.
//!
//! The bars are built at the bar's own origin, once per stance and size, and moved by patching the
//! position word of each packet (a swap frame costs a few stores per packet and a relink, not a
//! build). The scene code of `stance_hud_sprites.rs` decides where each bar goes and in which order,
//! the same code the host viewer paints from, so the result is pixel for pixel what `compose` says.

use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::prim::{RectFlat, Sprite};
use psx_vram::{upload_16bpp, Clut, TextureDepth, TexturePage, VramRect};

use super::hud_sprites::{
    bar, cell_clut, emblem_op, fold_of, gem_clut, gem_op, gems_shown, pill_state, scene,
    EmblemLook, Fold, HudState, Op, Part, Pose, Scene, Stance, ATLAS, ATLAS_H, ATLAS_HW_W,
    BACK_SIZE, CLUTS, CLUT_COUNT, COUNT, FX_SLOTS, GEO, MASK_CHECK, MASK_OFF, MASK_SET,
    MASK_SET_CHECK,
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

    /// Make a rectangle packet `dw` pixels wider (narrower when negative): the size is the word after
    /// the position.
    fn add_width(&mut self, dw: i32) {
        let at = match self.len() {
            3 => 2,
            4 => 3,
            _ => return,
        };
        let w = self.words[at];
        let width = (w & 0xFFFF) as i32 + dw;
        self.words[at] = (w & 0xFFFF_0000) | (width as u32 & 0xFFFF);
    }

    /// Move the primitive by `(dx, dy)` pixels: the position word is the one after the command (and
    /// the draw mode and texture window, when it has them).
    fn translate(&mut self, dx: i32, dy: i32) {
        let at = match self.len() {
            3 => 1,
            4 => 2,
            6 => 3,
            _ => return,
        };
        let w = self.words[at];
        let x = (w as u16 as i16 as i32) + dx;
        let y = ((w >> 16) as u16 as i16 as i32) + dy;
        self.words[at] = ((y as u16 as u32) << 16) | (x as u16 as u32);
    }
}

/// Words that do not depend on the part, worked out once per build.
struct Converter {
    vram: HudVram,
    page: u16,
    plain_mode: u32,
    glass_mode: u32,
    window: u32,
    sprite_cmd: u32,
}

impl Converter {
    fn new(vram: &HudVram) -> Self {
        let page = vram.page.uv_word(0);
        let plain = TextureMaterial::new(0, page);
        let average = TextureMaterial::blended(0, page, (0x80, 0x80, 0x80), BlendMode::Average);
        Self {
            vram: *vram,
            page,
            plain_mode: plain.draw_mode_word(),
            glass_mode: average.draw_mode_word(),
            window: plain.texture_window_word(),
            sprite_cmd: Sprite::with_material(0, 0, 1, 1, (0, 0), plain).color_cmd,
        }
    }

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

/// Packets of one part of a bar, at most `N`.
#[derive(Clone, Copy)]
struct Packets<const N: usize> {
    nodes: [Node; N],
    len: usize,
}

impl<const N: usize> Packets<N> {
    const NONE: Self = Self {
        nodes: [Node::ZERO; N],
        len: 0,
    };

    fn build(conv: &Converter, stance: Stance, size: usize, lit: u8, part: Part) -> Self {
        let mut p = Self::NONE;
        let pose = Pose { x: 0, y: 0, size };
        bar((0, 0), stance, pose, lit, Fold::FULL, part, &mut |op| {
            p.nodes[p.len] = conv.node(op);
            p.len += 1;
        });
        p
    }

    fn translate(&mut self, dx: i32, dy: i32) {
        for n in &mut self.nodes[..self.len] {
            n.translate(dx, dy);
        }
    }
}

/// Translucent bands (the well, and the outline and shadow in the new styles) and two ends; the most
/// any size has.
const GLASS_MAX: usize = 7;
/// Opaque bands: the most any size has.
const OPAQUE_MAX: usize = 8;

/// One bar of one stance at one size, as packets, and where they are now.
#[derive(Clone, Copy)]
struct BarPackets {
    glass: Packets<GLASS_MAX>,
    opaque: Packets<OPAQUE_MAX>,
    cells: Packets<{ COUNT as usize }>,
    /// How far the packets have been moved from where they were built (the bar's own origin).
    at: (i32, i32),
    /// How much shorter the plate and how much further left the right end are than when built.
    shrink: i32,
}

impl BarPackets {
    const NONE: Self = Self {
        glass: Packets::NONE,
        opaque: Packets::NONE,
        cells: Packets::NONE,
        at: (0, 0),
        shrink: 0,
    };

    fn build(conv: &Converter, stance: Stance, size: usize, lit: u8) -> Self {
        Self {
            glass: Packets::build(conv, stance, size, lit, Part::Glass),
            opaque: Packets::build(conv, stance, size, lit, Part::Opaque),
            cells: Packets::build(conv, stance, size, lit, Part::Cells),
            at: (0, 0),
            shrink: 0,
        }
    }

    /// Put the bar at `(x, y)`, folded as `fold` says, patching only what differs from where the
    /// packets are: every packet moves by the same amount, the rectangles of the plate change width
    /// and the right end (the last translucent packet) moves with the end of the plate.
    fn place(&mut self, x: i32, y: i32, fold: Fold) {
        let (tx, ty) = (x - fold.slide, y);
        let (dx, dy) = (tx - self.at.0, ty - self.at.1);
        let dshrink = fold.shrink - self.shrink;
        if dx == 0 && dy == 0 && dshrink == 0 {
            return;
        }
        let last = self.glass.len.saturating_sub(1);
        for (i, n) in self.glass.nodes[..self.glass.len].iter_mut().enumerate() {
            if n.len() == 6 && i == last {
                n.translate(dx - dshrink, dy);
            } else {
                n.translate(dx, dy);
                n.add_width(-dshrink);
            }
        }
        for n in &mut self.opaque.nodes[..self.opaque.len] {
            n.translate(dx, dy);
            n.add_width(-dshrink);
        }
        self.cells.translate(dx, dy);
        self.at = (tx, ty);
        self.shrink = fold.shrink;
    }
}

/// Sizes that are ever drawn: the full size and the ones down to the size of the bar behind.
const USED: usize = BACK_SIZE + 1;

/// Every packet of the HUD, built once by [`HudPackets::build`] and linked each frame by
/// [`HudPackets::link`].
pub struct HudPackets {
    bars: [[BarPackets; USED]; 2],
    /// The emblem of each stance, halfway between them, and the flash of each stance.
    emblem: [Node; 5],
    /// The pills, as they are when lit or empty.
    gems: [Node; COUNT as usize],
    /// Sprites for the effects: a pool of five for each pill that is in one, and four for the
    /// quadrants of the pill that is being recharged. They are linked over their pill's own node.
    effects: [[Node; 5]; FX_SLOTS],
    partial: [Node; 4],
    masks: [Node; 4],
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
    /// Cells lit in the packets as they stand.
    health: [u8; 2],
}

/// The pool of the pill being recharged, in [`HudPackets::over`].
const PARTIAL: u8 = FX_SLOTS as u8;
/// No pool.
const NO_POOL: u8 = u8::MAX;

impl HudPackets {
    /// An empty set, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            bars: [[BarPackets::NONE; USED]; 2],
            emblem: [Node::ZERO; 5],
            gems: [Node::ZERO; COUNT as usize],
            effects: [[Node::ZERO; 5]; FX_SLOTS],
            partial: [Node::ZERO; 4],
            masks: [Node::ZERO; 4],
            origin: (0, 0),
            vram: HudVram::SHOWCASE,
            modes: (0, 0, 0),
            shown: None,
            over: [(NO_POOL, 0); COUNT as usize],
            gem_lit: [false; COUNT as usize],
            health: [0; 2],
        }
    }

    /// Build the packets of every bar at every size for the HUD with its top-left at `origin`, with
    /// `health` cells lit in each stance's bar. Do this once, and again when the HUD moves. A health
    /// change needs no rebuild: see [`HudPackets::set_levels`]. The pills, and the sprites the
    /// effects on them are drawn with, are set up from the state in each [`HudPackets::link`].
    ///
    /// # Panics
    ///
    /// When the generated art has more bands than the packet arrays hold.
    pub fn build(&mut self, vram: &HudVram, origin: (i32, i32), health: [u8; 2]) {
        let conv = Converter::new(vram);
        self.origin = origin;
        self.vram = *vram;
        self.modes = (conv.plain_mode, conv.glass_mode, conv.sprite_cmd);
        let health = health.map(|h| h.min(COUNT as u8));
        self.health = health;
        self.shown = None;
        for (s, stance) in [Stance::Horizon, Stance::Zenith].into_iter().enumerate() {
            for size in 0..USED {
                self.bars[s][size] = BarPackets::build(&conv, stance, size, health[s]);
            }
        }
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

    /// Change how many cells are lit, without a rebuild: only the cells whose state changes get
    /// another palette in their packet (one store each, in every size of the bar). Values above ten
    /// count as ten, as in [`HudPackets::build`].
    pub fn set_levels(&mut self, vram: &HudVram, health: [u8; 2]) {
        let top = COUNT as u8;
        for (s, stance) in [Stance::Horizon, Stance::Zenith].into_iter().enumerate() {
            let (old, new) = (self.health[s], health[s].min(top));
            let lit = new > old;
            for k in old.min(new)..old.max(new) {
                for size in 0..USED {
                    let clut = vram.clut_word(cell_clut(stance, size, lit));
                    self.bars[s][size].cells.nodes[k as usize].set_clut(clut);
                }
            }
            self.health[s] = new;
        }
    }

    /// Set the packets up for `state`: place the two bars, and patch the pills and the sprites of
    /// their effects. Nothing is rebuilt.
    fn set_up(&mut self, state: &HudState, sc: &Scene) {
        let origin = self.origin;
        let fold_of_bar =
            |(stance, pose): (Stance, Pose)| fold_of(state.reveal[stance.index()], &GEO[pose.size]);
        let (fs, fp) = (sc.front.0.index(), sc.front.1);
        let (bs, bp) = (sc.behind.0.index(), sc.behind.1);
        if let Some(fold) = fold_of_bar(sc.front) {
            self.bars[fs][fp.size].place(origin.0 + fp.x, origin.1 + fp.y, fold);
        }
        if let Some(fold) = fold_of_bar(sc.behind) {
            self.bars[bs][bp.size].place(origin.0 + bp.x, origin.1 + bp.y, fold);
        }
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
            self.set_up(state, &sc);
            self.shown = Some(*state);
        }
        let fold_of_bar =
            |(stance, pose): (Stance, Pose)| fold_of(state.reveal[stance.index()], &GEO[pose.size]);
        let (front_fold, behind_fold) = (fold_of_bar(sc.front), fold_of_bar(sc.behind));
        let (fs, fp) = (sc.front.0.index(), sc.front.1);
        let (bs, bp) = (sc.behind.0.index(), sc.behind.1);
        let front = &self.bars[fs][fp.size];
        let behind = &self.bars[bs][bp.size];
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
                        (n as *const Node as *mut Node).cast::<u32>(),
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
        if let Some(fold) = front_fold {
            add_all!(&front.cells.nodes[..fold.cells as usize]);
            add_all!(&front.opaque.nodes[..front.opaque.len]);
        }
        add_all!(&self.masks[3..4]);
        if let Some(fold) = behind_fold {
            add_all!(&behind.opaque.nodes[..behind.opaque.len]);
            add_all!(&behind.glass.nodes[..behind.glass.len]);
            add_all!(&self.masks[2..3]);
            add_all!(&behind.cells.nodes[..fold.cells as usize]);
        } else {
            add_all!(&self.masks[2..3]);
        }
        add_all!(&self.masks[1..2]);
        if front_fold.is_some() {
            add_all!(&front.glass.nodes[..front.glass.len]);
        }
        add_all!(&self.masks[0..1]);
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
    use crate::hud_sprites::{
        compose, AmmoFx, HudAnim, PillFx, BREAK_FRAMES, ENERGY_PER_PILL, LOAD_FRAMES, REVEAL_MAX,
        SWAP_FRAMES,
    };

    fn same_node(a: &Node, b: &Node) -> bool {
        a.tag == b.tag && a.words == b.words
    }

    fn same_nodes(a: &[Node], b: &[Node]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same_node(x, y))
    }

    /// Every packet, in every bar and size, equal word for word.
    fn same(a: &HudPackets, b: &HudPackets) -> bool {
        let bars = (0..2).all(|s| {
            (0..USED).all(|z| {
                let (x, y) = (&a.bars[s][z], &b.bars[s][z]);
                x.at == y.at
                    && x.shrink == y.shrink
                    && x.glass.len == y.glass.len
                    && x.opaque.len == y.opaque.len
                    && x.cells.len == y.cells.len
                    && same_nodes(&x.glass.nodes, &y.glass.nodes)
                    && same_nodes(&x.opaque.nodes, &y.opaque.nodes)
                    && same_nodes(&x.cells.nodes, &y.cells.nodes)
            })
        });
        bars && same_nodes(&a.gems, &b.gems)
            && same_nodes(&a.emblem, &b.emblem)
            && same_nodes(&a.masks, &b.masks)
            && a.health == b.health
    }

    /// Patching the levels of a built HUD gives exactly the packets of a fresh build at the new
    /// levels, from every level to every level, for both stances and every size.
    #[test]
    fn patched_levels_equal_a_fresh_build() {
        let vram = HudVram::SHOWCASE;
        for from in 0..=COUNT as u8 {
            for to in 0..=COUNT as u8 {
                let mut patched = HudPackets::new();
                patched.build(&vram, (5, 7), [from, to]);
                patched.set_levels(&vram, [to, from]);
                let mut fresh = HudPackets::new();
                fresh.build(&vram, (5, 7), [to, from]);
                assert!(same(&patched, &fresh), "levels {from} -> {to}");
            }
        }
    }

    /// Values above ten count as ten.
    #[test]
    fn levels_above_ten_clamp() {
        let vram = HudVram::SHOWCASE;
        let mut patched = HudPackets::new();
        patched.build(&vram, (0, 0), [3, 9]);
        patched.set_levels(&vram, [250, 0]);
        let mut fresh = HudPackets::new();
        fresh.build(&vram, (0, 0), [10, 0]);
        assert!(same(&patched, &fresh));
    }

    /// A patch touches only the cells that change, and a change of nothing touches nothing.
    #[test]
    fn only_changed_cells_are_written() {
        let vram = HudVram::SHOWCASE;
        let mut hud = HudPackets::new();
        hud.build(&vram, (0, 0), [7, 5]);
        let before = hud.bars[0][0].cells;
        hud.set_levels(&vram, [7, 5]);
        assert!(same_nodes(&before.nodes, &hud.bars[0][0].cells.nodes));
        hud.set_levels(&vram, [8, 5]);
        let changed = (0..COUNT as usize)
            .filter(|&k| !same_node(&before.nodes[k], &hud.bars[0][0].cells.nodes[k]))
            .count();
        assert_eq!(changed, 1);
    }

    /// The words of the packets a `link` hands over, in paint order, sorted: the order of parts that
    /// do not overlap does not matter.
    fn linked(hud: &mut HudPackets, state: &HudState) -> Vec<Vec<u32>> {
        let mut out = Vec::new();
        hud.link(state, |p, tag| {
            let words = (tag >> 24) as usize;
            // SAFETY: the pointer is to a node of `hud`, which has six payload words after the tag.
            let payload = unsafe { core::slice::from_raw_parts((p as *const u32).add(1), words) };
            out.push(payload.to_vec());
        });
        out.sort();
        out
    }

    /// The words of the packets the composition of `state` asks for, sorted.
    fn composed(vram: &HudVram, origin: (i32, i32), state: &HudState) -> Vec<Vec<u32>> {
        let conv = Converter::new(vram);
        let mut out = Vec::new();
        compose(origin, state, &mut |op| {
            let n = conv.node(op);
            out.push(n.words[..n.len() as usize].to_vec());
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
            state.swap = next(u32::from(SWAP_FRAMES)) as u8;
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
        hud.build(&vram, origin, [7, 5]);
        let mut seed = 0x1234_5678u32;
        let mut next = |n: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) % n
        };
        for i in 0..6000 {
            let state = random_state(&mut next);
            hud.set_levels(&vram, state.health);
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
}
