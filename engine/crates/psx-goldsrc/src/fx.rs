//! The overlay capture layer both GoldSrc ports share.
//!
//! The HUD, message and fade pass draws with immediate GP0 writes, and every one of those writes
//! first spins on "ready for command", which the GPU only asserts once its whole raster backlog has
//! drained. Being the first immediate writer after the frame's last DMA submit, that pass used to
//! fence the CPU against the whole frame's rasterisation, world included.
//!
//! While capture is armed ([`begin`]) the drawing functions here append the identical GP0 words to a
//! linked list instead, which [`chain_submit`] sends in one DMA afterwards. Same words, same order,
//! same pixels; the CPU never touches GP0, so the deferred present can overlap the frame's raster
//! with the next simulation tick. Outside capture they call the shared [`driver`].
//!
//! The list borrows the tail of the frame's packet arena: slots past every packet the frame claimed,
//! which no ordering table links while channel 2 walks the world list, and which the next frame
//! rebuilds from slot zero. Capturing therefore costs no static RAM beyond the four words below.
//!
//! The one thing a port supplies is [`Tail`]: when the arena runs out mid-pass, whatever the port
//! still has pending for the frame (its own overlay lists) must go to the GPU first so the immediate
//! fallback keeps the pass order.

use core::ptr::addr_of;

use psx_engine::{PrimitivePacketArena, PrimitiveSink};
use psx_gpu::material::{BlendMode, TextureMaterial};

use crate::driver;

/// End-of-list tag address for a GPU linked list.
pub const CHAIN_TERMINATOR: u32 = 0x00FF_FFFF;
const CHAIN_ADDR_MASK: u32 = 0x00FF_FFFF;
/// Widest captured packet: the ten-word textured quad plus its tag.
pub const PACKET_WORDS: usize = 10;

/// The port's pending overlay tail, as the layer needs it.
///
/// The type is a marker that is never constructed; the methods are resolved at compile time.
pub trait Tail {
    /// Send every stage of the frame's overlay tail that is already built and has not gone out yet,
    /// leaving later stages (which may need the captured chain to draw first) pending. Safe with
    /// nothing pending.
    ///
    /// # Safety
    ///
    /// Called while the capture list is half built; the implementation may drive the GPU but must
    /// not touch the capture list or arm and disarm capture.
    unsafe fn issue_built();
}

/// One captured GP0 command, shaped to borrow a render-arena slot.
#[repr(C)]
#[derive(Copy, Clone)]
struct Packet {
    tag: u32,
    words: [u32; PACKET_WORDS],
}

impl Packet {
    const ZERO: Self = Self {
        tag: 0,
        words: [0; PACKET_WORDS],
    };
}

static mut ARENA: *mut PrimitivePacketArena<'static> = core::ptr::null_mut();
static mut CHAIN_HEAD: u32 = CHAIN_TERMINATOR;
static mut CHAIN_TAIL: *mut u32 = core::ptr::null_mut();
static mut CAPTURE: bool = false;

/// Start capturing into the tail of `packets`, with an empty list.
///
/// # Safety
///
/// `packets` must outlive the capture and stay untouched by the caller until [`end`].
pub unsafe fn begin(packets: &mut PrimitivePacketArena<'_>) {
    chain_reset();
    ARENA = (packets as *mut PrimitivePacketArena<'_>).cast();
    CAPTURE = true;
}

/// Stop capturing. The list built so far stays until [`chain_submit`] or [`chain_reset`].
pub fn end() {
    // SAFETY: single-threaded; plain stores to the layer's own statics.
    unsafe {
        CAPTURE = false;
        ARENA = core::ptr::null_mut();
    }
}

/// Whether capture is armed.
#[inline(always)]
#[must_use]
pub fn capturing() -> bool {
    // SAFETY: single-threaded read of the layer's own static.
    unsafe { CAPTURE }
}

/// Forget the captured list without sending it.
pub fn chain_reset() {
    // SAFETY: single-threaded; plain stores to the layer's own statics.
    unsafe {
        CHAIN_HEAD = CHAIN_TERMINATOR;
        CHAIN_TAIL = core::ptr::null_mut();
    }
}

/// The address of the first captured node ([`CHAIN_TERMINATOR`] when empty), for a port that
/// keeps one capture aside before starting the next.
#[must_use]
pub fn chain_head() -> u32 {
    // SAFETY: single-threaded read of the layer's own static.
    unsafe { CHAIN_HEAD }
}

/// Whether anything is captured.
#[must_use]
pub fn chain_is_empty() -> bool {
    // SAFETY: single-threaded read of the layer's own static.
    unsafe { CHAIN_TAIL.is_null() }
}

/// Submit and clear whatever has been captured. Safe with nothing queued.
pub fn chain_submit() {
    // SAFETY: single-threaded; the list lives in the frame's packet arena, which stays untouched
    // until `submit_list` returns, and its tag words were written by `chain_push`.
    unsafe {
        if CHAIN_TAIL.is_null() {
            return;
        }
        submit_list(addr_of!(CHAIN_HEAD));
    }
    chain_reset();
}

/// One out-of-line copy of the blocking kick-and-wait for the overlay lists; inlined into each
/// submit it cost 1.8 KB of `.text`.
///
/// # Safety
///
/// `head` must start a well-formed GPU linked list whose every node stays live and unmodified
/// until this returns, which it does once the walk ends.
#[inline(never)]
pub unsafe fn submit_list(head: *const u32) {
    driver::with(|gpu| psx_gpu::chain::submit_raw(gpu.dma_mut(), head));
}

/// Append one GP0 command packet, preserving issue order. Returns false when the arena is
/// exhausted (or capture is not armed), having first sent what was already built and captured, so
/// the caller can fall back to an immediate draw without reordering the pass.
pub fn chain_push<T: Tail>(words: &[u32]) -> bool {
    let n = words.len();
    // SAFETY: single-threaded; `begin` promised the arena outlives the capture.
    unsafe {
        if n == 0 || n > PACKET_WORDS || ARENA.is_null() {
            return false;
        }
        let arena = &mut *ARENA;
        let Some(slot) = arena.push(Packet::ZERO) else {
            // The frame's pending overlay lists draw before this capture, so they go first and
            // the immediate fallback keeps the pass order.
            T::issue_built();
            chain_submit();
            return false;
        };
        slot.tag = ((n as u32) << 24) | CHAIN_TERMINATOR;
        slot.words[..n].copy_from_slice(words);
        let packet = core::ptr::addr_of_mut!(slot.tag);
        let addr = packet as u32 & CHAIN_ADDR_MASK;
        if CHAIN_TAIL.is_null() {
            CHAIN_HEAD = addr;
        } else {
            CHAIN_TAIL.write((CHAIN_TAIL.read() & 0xFF00_0000) | addr);
        }
        CHAIN_TAIL = packet;
    }
    true
}

// GP0 word builders. These mirror `psx_hw::gpu`'s `pack_*` / `polygon_opcode` exactly: the whole
// point is that a captured primitive is bit-identical to the immediate one.

/// A GP0 vertex word.
#[inline(always)]
#[must_use]
pub fn gp0_vertex(x: i16, y: i16) -> u32 {
    ((x as u16) as u32) | (((y as u16) as u32) << 16)
}

/// A GP0 size or position word from unsigned halves.
#[inline(always)]
#[must_use]
pub fn gp0_xy(x: u16, y: u16) -> u32 {
    x as u32 | ((y as u32) << 16)
}

/// A GP0 colour word (low 24 bits).
#[inline(always)]
#[must_use]
pub fn gp0_color(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// A GP0 texture-coordinate word with its CLUT or page half.
#[inline(always)]
#[must_use]
pub fn gp0_texcoord(u: u8, v: u8, extra: u16) -> u32 {
    (u as u32) | ((v as u32) << 8) | ((extra as u32) << 16)
}

/// A flat triangle through a translucent blend mode, captured when armed. An opaque mode is the
/// plain immediate triangle.
pub fn tri_flat_blended<T: Tail>(
    verts: [(i16, i16); 3],
    r: u8,
    g: u8,
    b: u8,
    blend_mode: BlendMode,
) {
    if !capturing() || !blend_mode.is_translucent() {
        driver::tri_flat_blended(verts, r, g, b, blend_mode);
        return;
    }
    let material = TextureMaterial::blended(0, 0, (r, g, b), blend_mode);
    let captured = chain_push::<T>(&[
        material.draw_mode_word(),
        material.texture_window_word(),
        // GP0(22h): flat, triangle, untextured, semi-transparent.
        0x2200_0000 | gp0_color(r, g, b),
        gp0_vertex(verts[0].0, verts[0].1),
        gp0_vertex(verts[1].0, verts[1].1),
        gp0_vertex(verts[2].0, verts[2].1),
    ]);
    if !captured {
        driver::tri_flat_blended(verts, r, g, b, blend_mode);
    }
}

/// A flat opaque triangle, captured when armed.
pub fn tri_flat<T: Tail>(verts: [(i16, i16); 3], r: u8, g: u8, b: u8) {
    if !capturing() {
        driver::tri_flat(verts, r, g, b);
        return;
    }
    let captured = chain_push::<T>(&[
        // GP0(20h): flat, triangle, untextured, opaque.
        0x2000_0000 | gp0_color(r, g, b),
        gp0_vertex(verts[0].0, verts[0].1),
        gp0_vertex(verts[1].0, verts[1].1),
        gp0_vertex(verts[2].0, verts[2].1),
    ]);
    if !captured {
        driver::tri_flat(verts, r, g, b);
    }
}

/// A flat opaque quad, captured when armed.
pub fn quad_flat<T: Tail>(verts: [(i16, i16); 4], r: u8, g: u8, b: u8) {
    if !capturing() {
        driver::quad_flat(verts, r, g, b);
        return;
    }
    let captured = chain_push::<T>(&[
        // GP0(28h): flat, quad, untextured, opaque.
        0x2800_0000 | gp0_color(r, g, b),
        gp0_vertex(verts[0].0, verts[0].1),
        gp0_vertex(verts[1].0, verts[1].1),
        gp0_vertex(verts[2].0, verts[2].1),
        gp0_vertex(verts[3].0, verts[3].1),
    ]);
    if !captured {
        driver::quad_flat(verts, r, g, b);
    }
}

/// A textured quad, captured when armed.
pub fn quad_textured_material<T: Tail>(
    verts: [(i16, i16); 4],
    uvs: [(u8, u8); 4],
    material: TextureMaterial,
) {
    if !capturing() {
        driver::quad_textured_material(verts, uvs, material);
        return;
    }
    let captured = chain_push::<T>(&[
        material.texture_window_word(),
        material.flat_textured_polygon_header(true),
        gp0_vertex(verts[0].0, verts[0].1),
        gp0_texcoord(uvs[0].0, uvs[0].1, material.clut_word()),
        gp0_vertex(verts[1].0, verts[1].1),
        gp0_texcoord(uvs[1].0, uvs[1].1, material.texture_page_word()),
        gp0_vertex(verts[2].0, verts[2].1),
        gp0_texcoord(uvs[2].0, uvs[2].1, 0),
        gp0_vertex(verts[3].0, verts[3].1),
        gp0_texcoord(uvs[3].0, uvs[3].1, 0),
    ]);
    if !captured {
        driver::quad_textured_material(verts, uvs, material);
    }
}

/// A textured rectangle, captured when armed. Native-size text uses this exact rectangle path to
/// match the menu font's PS1 sampling.
pub fn sprite_textured_material<T: Tail>(
    x: i16,
    y: i16,
    w: u16,
    h: u16,
    uv: (u8, u8),
    material: TextureMaterial,
) {
    if !capturing() {
        driver::sprite_material(x, y, w, h, uv, material);
        return;
    }
    let captured = chain_push::<T>(&[
        material.draw_mode_word(),
        material.texture_window_word(),
        material.textured_rect_header(),
        gp0_vertex(x, y),
        gp0_texcoord(uv.0, uv.1, material.clut_word()),
        gp0_xy(w, h),
    ]);
    if !captured {
        driver::sprite_material(x, y, w, h, uv, material);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_pack_like_the_immediate_path() {
        assert_eq!(gp0_vertex(-1, 2), 0x0002_FFFF);
        assert_eq!(gp0_xy(5, 6), 0x0006_0005);
        assert_eq!(gp0_color(1, 2, 3), 0x0003_0201);
        assert_eq!(gp0_texcoord(7, 8, 0x1234), 0x1234_0807);
    }
}
