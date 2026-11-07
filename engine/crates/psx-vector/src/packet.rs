//! Self-contained packets for ordering tables, and a cache that builds them
//! once and relinks them every frame.
//!
//! Untextured semi-transparency reads its equation from the GPU draw mode, and
//! an ordering table gives no promise about what ran just before a node. So an
//! OT primitive carries its own GP0(E1h) word in the same node, the way the
//! SDK's `QuadGouraudBlended` does for quads.

use psx_gpu::material::{BlendMode, TextureMaterial};
use psx_gpu::prim::GpuPacket;
use psx_gpu::Gpu;

use crate::asset::{GroupId, VectorAsset};
use crate::draw::{DrawError, Emit, Placement, Scratch, Style};

/// A triangle or a quad with its draw mode, in one 10-word block.
///
/// The payload after the OT tag is `draw_mode`, then the GP0 polygon command
/// words. A quad uses all nine payload words; a triangle uses the first seven
/// (`draw_mode`, `color0_cmd`, `v0`, `color1`, `v1`, `color2`, `v2`) and
/// leaves the rest unused. The node length lives in the tag's high byte (7 or
/// 9), staged by the builders and preserved by linking, see [`Self::words`].
///
/// A packet also draws immediately: send the first `words()` payload words.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(4))]
pub struct VectorPacket {
    /// OT linkage: payload word count in bits 24..31, written by the builders;
    /// the link operation adds the next-node address below it.
    pub tag: u32,
    /// GP0(E1h) draw mode selecting the blend equation.
    pub draw_mode: u32,
    /// Vertex 0 colour with the polygon opcode.
    pub color0_cmd: u32,
    /// Vertex 0 position.
    pub v0: u32,
    /// Vertex 1 colour.
    pub color1: u32,
    /// Vertex 1 position.
    pub v1: u32,
    /// Vertex 2 colour.
    pub color2: u32,
    /// Vertex 2 position.
    pub v2: u32,
    /// Vertex 3 colour (quads only).
    pub color3: u32,
    /// Vertex 3 position (quads only).
    pub v3: u32,
}

// SAFETY: `repr(C, align(4))`, a `u32` tag first, then 9 `u32` payload words.
// A triangle packet is linked with a shorter node length through the raw add.
unsafe impl GpuPacket for VectorPacket {
    const WORDS: u8 = 9;
}

/// Payload words of a triangle node.
pub const TRIANGLE_WORDS: u8 = 7;
/// Payload words of a quad node.
pub const QUAD_WORDS: u8 = 9;

impl VectorPacket {
    /// A zeroed packet, for sizing static pools.
    pub const ZERO: Self = Self {
        tag: 0,
        draw_mode: 0,
        color0_cmd: 0,
        v0: 0,
        color1: 0,
        v1: 0,
        color2: 0,
        v2: 0,
        color3: 0,
        v3: 0,
    };

    /// Payload words this packet sends: 7 for a triangle, 9 for a quad.
    #[inline]
    pub const fn words(&self) -> u8 {
        (self.tag >> 24) as u8
    }
}

/// A [`VectorPacket`] sent as a triangle: the first 7 payload words.
#[repr(transparent)]
struct TriangleView(VectorPacket);

// SAFETY: transparent over `VectorPacket`, which is `repr(C, align(4))` with a
// tag word and at least 7 payload words.
unsafe impl GpuPacket for TriangleView {
    const WORDS: u8 = TRIANGLE_WORDS;
}

impl VectorPacket {
    /// Send this packet to the GPU now, without an ordering table: 7 payload
    /// words for a triangle, 9 for a quad. Draws exactly what linking it into
    /// a table would.
    #[inline]
    pub fn draw(&self, gpu: &mut Gpu) {
        if self.words() == QUAD_WORDS {
            gpu.draw(self);
        } else {
            // SAFETY: `TriangleView` is `repr(transparent)` over `VectorPacket`,
            // so the layouts are identical and the borrow is the same one.
            gpu.draw(unsafe { &*(self as *const Self).cast::<TriangleView>() });
        }
    }
}

/// Why a packet build did not fill its output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketError {
    /// The draw itself failed, see [`DrawError`].
    Draw(DrawError),
    /// The output was too small; `needed` packets would have been written.
    OutputTooSmall {
        /// Packets the draw produces.
        needed: usize,
    },
}

impl<'a> VectorAsset<'a> {
    /// Write `group`'s primitives as [`VectorPacket`]s into `out`, in paint
    /// order, and return how many.
    ///
    /// An ordering table walks one slot last-in-first-out, so to keep paint
    /// order within a slot link them back to front:
    /// `out[..n].iter_mut().rev()`. [`PacketCache`] does this for you. A clip
    /// rectangle can split a triangle into up to five, so size `out` for five
    /// times the group's primitives when clipping.
    pub fn emit_packets<const N: usize>(
        &self,
        group: GroupId,
        place: &Placement,
        style: &Style,
        scratch: &mut Scratch<N>,
        out: &mut [VectorPacket],
    ) -> Result<usize, PacketError> {
        let mut mode_word = 0u32;
        let mut written = 0usize;
        self.emit_group(group, place, style, scratch, &mut |step| {
            let packet = match step {
                Emit::Blend(mode) => {
                    mode_word = draw_mode_word(mode);
                    return;
                }
                Emit::Triangle(t) => VectorPacket {
                    tag: (TRIANGLE_WORDS as u32) << 24,
                    draw_mode: mode_word,
                    color0_cmd: t.color0_cmd,
                    v0: t.v0,
                    color1: t.color1,
                    v1: t.v1,
                    color2: t.color2,
                    v2: t.v2,
                    color3: 0,
                    v3: 0,
                },
                Emit::Quad(q) => VectorPacket {
                    tag: (QUAD_WORDS as u32) << 24,
                    draw_mode: mode_word,
                    color0_cmd: q.color0_cmd,
                    v0: q.v0,
                    color1: q.color1,
                    v1: q.v1,
                    color2: q.color2,
                    v2: q.v2,
                    color3: q.color3,
                    v3: q.v3,
                },
            };
            if let Some(slot) = out.get_mut(written) {
                *slot = packet;
            }
            written += 1;
        })
        .map_err(PacketError::Draw)?;
        if written > out.len() {
            Err(PacketError::OutputTooSmall { needed: written })
        } else {
            Ok(written)
        }
    }
}

/// The GP0(E1h) word for `mode`, with dithering off.
pub fn draw_mode_word(mode: BlendMode) -> u32 {
    TextureMaterial::blended(0, 0, (0, 0, 0), mode).draw_mode_word()
}

/// Packets built once and linked into an ordering table every frame.
///
/// Building a packet costs a transform and eight or ten stores to RAM. Linking
/// one costs a handful of cycles. So a HUD that changes only when its state
/// changes should [`push`](Self::push) its groups when that happens and
/// [`links`](Self::links) the result each frame, which is what makes it cheap.
///
/// `N` is the packet capacity; each packet is 40 bytes.
pub struct PacketCache<const N: usize> {
    packets: [VectorPacket; N],
    slots: [u8; N],
    len: usize,
}

impl<const N: usize> PacketCache<N> {
    /// An empty cache, usable in a `static`.
    pub const fn new() -> Self {
        Self {
            packets: [VectorPacket::ZERO; N],
            slots: [0; N],
            len: 0,
        }
    }

    /// Forget every packet; the next [`push`](Self::push) starts over.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Packets held.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the cache holds no packets.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append `group` drawn with `place` and `style`, to be linked at OT slot
    /// `slot`. Groups appended later draw on top of earlier ones that share a
    /// slot. Returns the packets added; on error nothing is added.
    pub fn push<const V: usize>(
        &mut self,
        asset: &VectorAsset<'_>,
        group: GroupId,
        place: &Placement,
        style: &Style,
        scratch: &mut Scratch<V>,
        slot: u8,
    ) -> Result<usize, PacketError> {
        let start = self.len;
        let added = asset.emit_packets(group, place, style, scratch, &mut self.packets[start..])?;
        self.slots[start..start + added].fill(slot);
        self.len += added;
        Ok(added)
    }

    /// Draw every cached packet straight to the GPU in paint order, ignoring
    /// slots: the HUD path for a scene that draws its overlay immediately.
    pub fn draw(&self, gpu: &mut Gpu) {
        for packet in &self.packets[..self.len] {
            packet.draw(gpu);
        }
    }

    /// The packets in link order, each with the slot it belongs in.
    ///
    /// Link them in the order given: back to front, so that within one slot
    /// the first-pushed packet is walked first, because a slot is walked
    /// last-in-first-out. Use [`VectorPacket::words`] as the node length:
    ///
    /// ```ignore
    /// for (slot, packet) in cache.links() {
    ///     unsafe { ot.add_raw(slot, packet as *mut _ as *mut u32, packet.words()) };
    /// }
    /// ```
    ///
    /// The packets must stay unmodified and live until the walk ends, so link
    /// from a `static` cache and do not [`push`](Self::push) or
    /// [`clear`](Self::clear) while a walk may be reading it.
    pub fn links(&mut self) -> impl Iterator<Item = (usize, &mut VectorPacket)> {
        let len = self.len;
        self.slots[..len]
            .iter()
            .zip(self.packets[..len].iter_mut())
            .rev()
            .map(|(&slot, packet)| (slot as usize, packet))
    }
}

impl<const N: usize> Default for PacketCache<N> {
    fn default() -> Self {
        Self::new()
    }
}
