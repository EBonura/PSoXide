//! Cooked vector graphics for the PS1.
//!
//! An SVG subset is cooked on the host (`psx-vector-cook`) into a compact
//! binary asset of fixed-point Gouraud triangles grouped by SVG group id. This
//! crate holds the wire format ([`format`]), a zero-copy reader
//! ([`VectorAsset`]) and the guest runtime that draws one group at a time with
//! an offset, scale, rotation, colour state and clip rectangle
//! ([`VectorAsset::draw_group`], [`VectorAsset::emit_group`]).
//!
//! # Why polygons
//!
//! Converted bitmaps look low-resolution at HUD sizes and cost VRAM. Polygons
//! stay crisp at the native resolution, cost no VRAM, and each named group
//! animates on its own (a health cell, a crystal, an emblem).
//!
//! # Cost model
//!
//! Vertices are transformed once per group draw and triangles reference them
//! by index, so a triangulated polygon pays for `n` transforms rather than
//! `3 * (n - 2)`. Everything is 32-bit integer maths with no float, no 64-bit
//! integers and no allocation: positions are Q4 pixels (1/16 px), the
//! transform matrix is Q12, angles are psx-math's Q0.12 turns.
//!
//! # Draw state
//!
//! Untextured semi-transparency reads its equation from the GPU's draw mode
//! (GP0(E1h)), so a draw may write that register. [`VectorAsset::draw_group`]
//! always writes it once on entry, and again at each change of blend mode
//! between runs of triangles, and leaves it in the last mode used. Callers that
//! draw textured primitives afterwards set their own mode as they always do.

#![no_std]

#[cfg(feature = "encode")]
extern crate alloc;

mod asset;
mod clip;
mod draw;
#[cfg(feature = "encode")]
pub mod encode;
pub mod format;
mod packet;

pub use asset::{GroupId, GroupInfo, ParseError, VectorAsset};
pub use draw::{
    BlendOverride, ClipRect, DrawError, Emit, Placement, Scratch, Style, MAX_SCALE_Q12,
};
pub use packet::{
    draw_mode_word, PacketCache, PacketError, VectorPacket, QUAD_WORDS, TRIANGLE_WORDS,
};

#[cfg(not(target_endian = "little"))]
compile_error!("PSVG assets are little-endian and are read in place");

/// Include a cooked `.psvg` file with the 4-byte alignment [`VectorAsset::parse`]
/// requires, as a `&'static [u8]`.
///
/// The path is relative to the invoking source file, as for [`include_bytes!`].
#[macro_export]
macro_rules! include_vector {
    ($path:expr) => {{
        #[repr(C, align(4))]
        struct Aligned<T: ?Sized>(T);
        static BYTES: &Aligned<[u8]> = &Aligned(*include_bytes!($path));
        &BYTES.0
    }};
}
