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
    bar, cell_clut, emblem_op, gem_clut, gem_op, scene, EmblemLook, HudState, Op, Part, Pose,
    Scene, Stance, ATLAS, ATLAS_H, ATLAS_HW_W, BACK_SIZE, CLUTS, CLUT_COUNT, COUNT, MASK_CHECK,
    MASK_OFF, MASK_SET, MASK_SET_CHECK,
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
    /// The state the bars were last placed for, and the scene it gave: at rest a frame skips both.
    shown: Option<(HudState, Scene)>,
    /// Cells and gems lit in the packets as they stand, each stance.
    health: [u8; 2],
    energy: [u8; 2],
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
            shown: None,
            health: [0; 2],
            energy: [0; 2],
        }
    }

    /// Build the packets of every bar at every size for the HUD with its top-left at `origin`, with
    /// `health` cells lit in each stance's bar and `energy` gems lit for each stance. Do this once,
    /// and again when the HUD moves. A health or energy change needs no rebuild: see [`HudPackets::set_levels`].
    ///
    /// # Panics
    ///
    /// When the generated art has more bands than the packet arrays hold.
    pub fn build(&mut self, vram: &HudVram, origin: (i32, i32), health: [u8; 2], energy: [u8; 2]) {
        let conv = Converter::new(vram);
        self.origin = origin;
        let (health, energy) = (
            health.map(|h| h.min(COUNT as u8)),
            energy.map(|e| e.min(COUNT as u8)),
        );
        self.health = health;
        self.shown = None;
        self.energy = energy;
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

    /// Change how many cells and gems are lit, without a rebuild: only the cells and gems whose
    /// state changes get another palette in their packet (one store each, in every size of the
    /// bar). Values above ten count as ten, as in [`HudPackets::build`].
    pub fn set_levels(&mut self, vram: &HudVram, health: [u8; 2], energy: [u8; 2]) {
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
            let (old, new) = (self.energy[s], energy[s].min(top));
            let clut = vram.clut_word(gem_clut(new > old));
            for k in old.min(new)..old.max(new) {
                self.gems[s][k as usize].set_clut(clut);
            }
            self.energy[s] = new;
        }
    }

    /// Place the two bars for `state` (not again while the state stays the same) and hand every packet of the HUD to `add` as `(pointer,
    /// payload words)`, in the order to link them: back to front, so that within one ordering-table
    /// slot the first packet is walked first.
    ///
    /// The packets must stay unmodified until the walk that uses them ends.
    pub fn link(&mut self, state: &HudState, mut add: impl FnMut(*mut u32, u32)) {
        let sc = match self.shown {
            Some((shown, sc)) if shown == *state => sc,
            _ => {
                let sc = scene(state);
                let origin = self.origin;
                let (fs, fp) = (sc.front.0.index(), sc.front.1);
                let (bs, bp) = (sc.behind.0.index(), sc.behind.1);
                self.bars[fs][fp.size].place(origin.0 + fp.x, origin.1 + fp.y);
                self.bars[bs][bp.size].place(origin.0 + bp.x, origin.1 + bp.y);
                self.shown = Some((*state, sc));
                sc
            }
        };
        let (fs, fp) = (sc.front.0.index(), sc.front.1);
        let (bs, bp) = (sc.behind.0.index(), sc.behind.1);
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
        add_all!(&self.gems[sc.gems.index()]);
        add_all!(core::slice::from_ref(&self.emblem[emblem]));
        add_all!(&front.cells.nodes[..front.cells.len]);
        add_all!(&front.opaque.nodes[..front.opaque.len]);
        add_all!(&self.masks[3..4]);
        add_all!(&behind.opaque.nodes[..behind.opaque.len]);
        add_all!(&behind.glass.nodes[..behind.glass.len]);
        add_all!(&self.masks[2..3]);
        add_all!(&behind.cells.nodes[..behind.cells.len]);
        add_all!(&self.masks[1..2]);
        add_all!(&front.glass.nodes[..front.glass.len]);
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
                    && x.glass.len == y.glass.len
                    && x.opaque.len == y.opaque.len
                    && x.cells.len == y.cells.len
                    && same_nodes(&x.glass.nodes, &y.glass.nodes)
                    && same_nodes(&x.opaque.nodes, &y.opaque.nodes)
                    && same_nodes(&x.cells.nodes, &y.cells.nodes)
            })
        });
        bars && (0..2).all(|s| same_nodes(&a.gems[s], &b.gems[s]))
            && same_nodes(&a.emblem, &b.emblem)
            && same_nodes(&a.masks, &b.masks)
            && a.health == b.health
            && a.energy == b.energy
    }

    /// Patching the levels of a built HUD gives exactly the packets of a fresh build at the new
    /// levels, from every level to every level, for both stances, every size, health and energy.
    #[test]
    fn patched_levels_equal_a_fresh_build() {
        let vram = HudVram::SHOWCASE;
        for from in 0..=COUNT as u8 {
            for to in 0..=COUNT as u8 {
                let mut patched = HudPackets::new();
                patched.build(&vram, (5, 7), [from, to], [to, from]);
                patched.set_levels(&vram, [to, from], [from, to]);
                let mut fresh = HudPackets::new();
                fresh.build(&vram, (5, 7), [to, from], [from, to]);
                assert!(same(&patched, &fresh), "levels {from} -> {to}");
            }
        }
    }

    /// The same after the bars have been moved, and values above ten count as ten.
    #[test]
    fn patching_after_a_move_and_clamping() {
        let vram = HudVram::SHOWCASE;
        let mut patched = HudPackets::new();
        patched.build(&vram, (0, 0), [3, 9], [0, 10]);
        for s in 0..2 {
            for z in 0..USED {
                patched.bars[s][z].place(-12 + 5 * z as i32, 33 - 9 * s as i32);
            }
        }
        patched.set_levels(&vram, [250, 0], [4, 255]);
        let mut fresh = HudPackets::new();
        fresh.build(&vram, (0, 0), [10, 0], [4, 10]);
        for s in 0..2 {
            for z in 0..USED {
                fresh.bars[s][z].place(-12 + 5 * z as i32, 33 - 9 * s as i32);
            }
        }
        assert!(same(&patched, &fresh));
    }

    /// Linking a state again, or after other states, gives the packets of linking it on a fresh
    /// HUD: the skipped placement never leaves a bar where an earlier state put it.
    #[test]
    fn repeated_states_link_like_fresh_ones() {
        let vram = HudVram::SHOWCASE;
        let states = |stance, swap| HudState {
            stance,
            health: [7, 5],
            energy: [8, 3],
            swap,
        };
        let mut hud = HudPackets::new();
        hud.build(&vram, (6, 160), [7, 5], [8, 3]);
        let sequence = [
            (Stance::Horizon, 0),
            (Stance::Horizon, 0),
            (Stance::Zenith, 3),
            (Stance::Zenith, 3),
            (Stance::Zenith, 0),
            (Stance::Horizon, 5),
            (Stance::Horizon, 0),
        ];
        for (stance, swap) in sequence {
            let state = states(stance, swap);
            let mut fresh = HudPackets::new();
            fresh.build(&vram, (6, 160), [7, 5], [8, 3]);
            let (mut a, mut b) = (Vec::new(), Vec::new());
            hud.link(&state, |p, t| a.push((p as usize, t)));
            fresh.link(&state, |p, t| b.push((p as usize, t)));
            assert_eq!(a.len(), b.len());
            // Same packets in the same order, and the words they hold: where the bars are.
            for (i, (x, y)) in a.iter().zip(&b).enumerate() {
                assert_eq!(x.1, y.1, "{stance:?} {swap} packet {i}");
                // SAFETY: the pointers point at live nodes of the two HUDs.
                let (wx, wy) = unsafe {
                    (
                        core::slice::from_raw_parts(x.0 as *const u32, 7),
                        core::slice::from_raw_parts(y.0 as *const u32, 7),
                    )
                };
                let n = 1 + (x.1 >> 24) as usize;
                assert_eq!(wx[1..n], wy[1..n], "{stance:?} {swap} packet {i}");
            }
        }
    }

    /// A patch touches only the cells and gems that change, and a change of nothing touches nothing.
    #[test]
    fn only_changed_cells_are_written() {
        let vram = HudVram::SHOWCASE;
        let mut hud = HudPackets::new();
        hud.build(&vram, (0, 0), [7, 5], [8, 3]);
        let before = hud.bars[0][0].cells;
        hud.set_levels(&vram, [7, 5], [8, 3]);
        assert!(same_nodes(&before.nodes, &hud.bars[0][0].cells.nodes));
        hud.set_levels(&vram, [8, 5], [8, 3]);
        let changed = (0..COUNT as usize)
            .filter(|&k| !same_node(&before.nodes[k], &hud.bars[0][0].cells.nodes[k]))
            .count();
        assert_eq!(changed, 1);
    }
}
