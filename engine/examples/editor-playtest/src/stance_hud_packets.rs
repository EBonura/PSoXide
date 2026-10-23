//! Guest side of the sprite stance HUD: put the atlas and palettes in VRAM once, turn a
//! [`HudState`] into GPU packets once per state change, link them into an ordering table every
//! frame.
//!
//! Each packet carries its own draw mode, so it is correct wherever an ordering table walk puts it:
//! a rectangle is three words, a sprite is six (draw mode, texture window reset, then the textured
//! rectangle command). Include this file after `stance_hud_sprites.rs` as `hud_sprites`.

use psx_gpu::material::TextureMaterial;
use psx_gpu::prim::{RectFlat, Sprite};
use psx_vram::{upload_16bpp, Clut, TextureDepth, TexturePage, VramRect};

use super::hud_sprites::{
    compose, HudState, Op, ATLAS, ATLAS_H, ATLAS_HW_W, CLUTS, CLUT_COUNT, MAX_OPS,
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
    /// Page 15, top half (right of the display buffers), and the strip just under the second
    /// display buffer: free in the showcase.
    pub const SHOWCASE: Self = Self {
        page: TexturePage::new(960, 0, TextureDepth::Bit4),
        clut_x: 704,
        clut_y: 480,
    };

    /// Copy the atlas and the palettes into VRAM. Do this once, before the first draw.
    pub fn upload(&self) {
        upload_16bpp(
            VramRect::new(self.page.x(), self.page.y(), ATLAS_HW_W as u16, ATLAS_H as u16),
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

/// One ordering-table node: the tag, and up to six payload words.
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
}

/// A HUD's packets, built by [`HudPackets::build`] and linked by [`HudPackets::links`].
pub struct HudPackets<const N: usize> {
    nodes: [Node; N],
    len: usize,
}

impl<const N: usize> HudPackets<N> {
    /// An empty set, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            nodes: [Node::ZERO; N],
            len: 0,
        }
    }

    /// Packets held.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether no packets are held.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Forget the packets and rebuild them for `state` with the HUD's top-left at `origin`.
    ///
    /// # Panics
    ///
    /// When `N` is smaller than the HUD needs; [`MAX_OPS`] is enough.
    pub fn build(&mut self, vram: &HudVram, origin: (i32, i32), state: &HudState) {
        self.len = 0;
        let (nodes, len) = (&mut self.nodes, &mut self.len);
        compose(origin, state, &mut |op| {
            let node = match op {
                Op::Rect { x, y, w, h, rgb } => {
                    let r = RectFlat::new(x as i16, y as i16, w as u16, h as u16, rgb[0], rgb[1], rgb[2]);
                    Node {
                        tag: 3 << 24,
                        words: [r.color_cmd, r.xy, r.wh, 0, 0, 0],
                    }
                }
                Op::Sprite { x, y, src, clut } => {
                    let material = TextureMaterial::new(vram.clut_word(clut), vram.page.uv_word(0));
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
                            material.draw_mode_word(),
                            material.texture_window_word(),
                            s.color_cmd,
                            s.xy,
                            s.uv_clut,
                            s.wh,
                        ],
                    }
                }
            };
            nodes[*len] = node;
            *len += 1;
        });
    }

    /// The packets in link order, as `(pointer, payload words)`: back to front, so that within one
    /// ordering-table slot the first-built packet is walked first.
    ///
    /// The packets must stay unmodified until the walk that uses them ends.
    pub fn links(&mut self) -> impl Iterator<Item = (*mut u32, u8)> + '_ {
        self.nodes[..self.len]
            .iter_mut()
            .rev()
            .map(|n| ((n as *mut Node).cast::<u32>(), n.len()))
    }
}

impl<const N: usize> Default for HudPackets<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Packets a [`HudPackets`] needs for one HUD.
pub const PACKETS: usize = MAX_OPS;
