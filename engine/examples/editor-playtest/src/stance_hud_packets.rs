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
    bar, emblem_op, gem_op, scene, EmblemLook, HudState, Op, Part, Pose, Stance, ATLAS, ATLAS_H,
    ATLAS_HW_W, BACK_SIZE, CLUTS, CLUT_COUNT, COUNT, MASK_CHECK, MASK_OFF, MASK_SET,
    MASK_SET_CHECK,
};

/// Where the HUD's texels and palettes live in VRAM.
#[derive(Clone, Copy)]
pub struct HudVram {
    /// The 4-bit page holding the atlas, at its top-left corner.
    pub page: TexturePage,
    /// Left edge and row of the palette strip: [`CLUT_COUNT`] palettes of 16 entries side by side.
    pub clut_x: u16,
    pub clut_y: u16,
}

impl HudVram {
    /// Page 15, top half (right of the display buffers), and the palette strip just under the second
    /// display buffer (29 palettes of 16 entries, so it ends at x 976): free in the showcase.
    pub const SHOWCASE: Self = Self {
        page: TexturePage::new(960, 0, TextureDepth::Bit4),
        clut_x: 512,
        clut_y: 480,
    };

    /// Copy the atlas and the palettes into VRAM. Do this once, before the first draw.
    pub fn upload(&self) {
        upload_16bpp(
            VramRect::new(
                self.page.x(),
                self.page.y(),
                ATLAS_HW_W as u16,
                ATLAS_H as u16,
            ),
            &ATLAS,
        );
        upload_16bpp(
            VramRect::new(self.clut_x, self.clut_y, (CLUT_COUNT * 16) as u16, 1),
            &CLUTS,
        );
    }

    fn clut_word(&self, clut: usize) -> u16 {
        Clut::new(self.clut_x + 16 * clut as u16, self.clut_y).uv_word()
    }
}

/// GP0 E6, the mask bit setting: bit 0 marks the pixels drawn, bit 1 skips marked pixels.
const MASK_SETTING: u32 = 0xE600_0000;

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
                    (src.u, src.v),
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
        bar((0, 0), stance, pose, lit, part, &mut |op| {
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

/// Translucent bands and two ends; the most any size has.
const GLASS_MAX: usize = 5;
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
}

impl BarPackets {
    const NONE: Self = Self {
        glass: Packets::NONE,
        opaque: Packets::NONE,
        cells: Packets::NONE,
        at: (0, 0),
    };

    fn build(conv: &Converter, stance: Stance, size: usize, lit: u8) -> Self {
        Self {
            glass: Packets::build(conv, stance, size, lit, Part::Glass),
            opaque: Packets::build(conv, stance, size, lit, Part::Opaque),
            cells: Packets::build(conv, stance, size, lit, Part::Cells),
            at: (0, 0),
        }
    }

    /// Move to `(x, y)`, patching only when that differs from where the packets are.
    fn place(&mut self, x: i32, y: i32) {
        let (dx, dy) = (x - self.at.0, y - self.at.1);
        if dx != 0 || dy != 0 {
            self.glass.translate(dx, dy);
            self.opaque.translate(dx, dy);
            self.cells.translate(dx, dy);
            self.at = (x, y);
        }
    }
}

/// Sizes that are ever drawn: the full size and the ones down to the size of the bar behind.
const USED: usize = BACK_SIZE + 1;

/// Every packet of the HUD, built once by [`HudPackets::build`] and linked each frame by
/// [`HudPackets::link`].
pub struct HudPackets {
    bars: [[BarPackets; USED]; 2],
    emblem: [Node; 3],
    gems: [[Node; COUNT as usize]; 2],
    masks: [Node; 4],
    origin: (i32, i32),
}

impl HudPackets {
    /// An empty set, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            bars: [[BarPackets::NONE; USED]; 2],
            emblem: [Node::ZERO; 3],
            gems: [[Node::ZERO; COUNT as usize]; 2],
            masks: [Node::ZERO; 4],
            origin: (0, 0),
        }
    }

    /// Build the packets of every bar at every size for the HUD with its top-left at `origin`, with
    /// `health` cells lit in each stance's bar and `energy` gems lit for each stance. Do this once,
    /// and again when a health or energy value changes.
    ///
    /// # Panics
    ///
    /// When the generated art has more bands than the packet arrays hold.
    pub fn build(&mut self, vram: &HudVram, origin: (i32, i32), health: [u8; 2], energy: [u8; 2]) {
        let conv = Converter::new(vram);
        self.origin = origin;
        for (s, stance) in [Stance::Horizon, Stance::Zenith].into_iter().enumerate() {
            for size in 0..USED {
                self.bars[s][size] = BarPackets::build(&conv, stance, size, health[s]);
            }
            for k in 0..COUNT {
                self.gems[s][k as usize] = conv.node(gem_op(origin, k, (k as u8) < energy[s]));
            }
        }
        for (i, look) in [
            EmblemLook::Of(Stance::Horizon),
            EmblemLook::Of(Stance::Zenith),
            EmblemLook::Halfway,
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

    /// Place the two bars for `state` and hand every packet of the HUD to `add` as `(pointer,
    /// payload words)`, in the order to link them: back to front, so that within one ordering-table
    /// slot the first packet is walked first.
    ///
    /// The packets must stay unmodified until the walk that uses them ends.
    pub fn link(&mut self, state: &HudState, mut add: impl FnMut(*mut u32, u8)) {
        let sc = scene(state);
        let (fs, fp) = (sc.front.0.index(), sc.front.1);
        let (bs, bp) = (sc.behind.0.index(), sc.behind.1);
        let origin = self.origin;
        self.bars[fs][fp.size].place(origin.0 + fp.x, origin.1 + fp.y);
        self.bars[bs][bp.size].place(origin.0 + bp.x, origin.1 + bp.y);
        let front = &self.bars[fs][fp.size];
        let behind = &self.bars[bs][bp.size];
        let emblem = match sc.emblem {
            EmblemLook::Of(s) => s.index(),
            EmblemLook::Halfway => 2,
        };
        // Paint order, as `compose` has it, walked backwards because a slot is walked last in,
        // first out: gems, emblem, the front bar's opaque parts and cells, the mask off, the back
        // bar's plate, the mask set to check, the back bar's cells, the mask set and checked, the
        // front bar's translucent parts, the mask set.
        let mut add_all = |nodes: &[Node]| {
            for n in nodes.iter().rev() {
                add((n as *const Node as *mut Node).cast::<u32>(), n.len());
            }
        };
        add_all(&self.gems[sc.gems.index()]);
        add_all(core::slice::from_ref(&self.emblem[emblem]));
        add_all(&front.cells.nodes[..front.cells.len]);
        add_all(&front.opaque.nodes[..front.opaque.len]);
        add_all(&self.masks[3..4]);
        add_all(&behind.opaque.nodes[..behind.opaque.len]);
        add_all(&behind.glass.nodes[..behind.glass.len]);
        add_all(&self.masks[2..3]);
        add_all(&behind.cells.nodes[..behind.cells.len]);
        add_all(&self.masks[1..2]);
        add_all(&front.glass.nodes[..front.glass.len]);
        add_all(&self.masks[0..1]);
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
