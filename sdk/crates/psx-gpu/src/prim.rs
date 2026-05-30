//! Primitive packet types for DMA-based GPU submission.
//!
//! Each struct is `#[repr(C)]` with the tag word first -- that's the
//! shape the DMA linked-list walker expects. The field names match
//! the on-wire GP0 word order so a reader can cross-reference
//! PSX-SPX without redundant decoding.
//!
//! Builders (`new` constructors) zero the tag; [`crate::ot::OrderingTable::add`]
//! fills it in during insertion with `(words_after_tag << 24) | next`.

use crate::material::{TextureMaterial, TexturedGouraudPacketMaterial, TexturedPacketMaterial};
use psx_hw::gpu::{gp0, pack_color, pack_texcoord, pack_vertex, pack_xy};

const fn pack_packet_texcoord(u: u8, v: u8, extra: u16) -> u32 {
    (u as u32) | ((v as u32) << 8) | ((extra as u32) << 16)
}

/// Flat-shaded triangle. 5 words (tag + 4 data).
#[repr(C, align(4))]
pub struct TriFlat {
    /// DMA / OT linkage word. Written by the OT at insert time.
    pub tag: u32,
    /// `0x20000000 | rgb24` header.
    pub color_cmd: u32,
    /// Vertex 0 packed via [`pack_vertex`].
    pub v0: u32,
    /// Vertex 1.
    pub v1: u32,
    /// Vertex 2.
    pub v2: u32,
}

impl TriFlat {
    /// Data-word count after the tag. Passed to `ot::add`.
    pub const WORDS: u8 = 4;

    /// Build a flat triangle ready for OT insertion.
    pub const fn new(verts: [(i16, i16); 3], r: u8, g: u8, b: u8) -> Self {
        Self {
            tag: 0,
            color_cmd: gp0::polygon_opcode(false, false, false, false, false) | pack_color(r, g, b),
            v0: pack_vertex(verts[0].0, verts[0].1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            v2: pack_vertex(verts[2].0, verts[2].1),
        }
    }
}

/// Gouraud-shaded triangle. 7 words (tag + 6 data).
#[repr(C, align(4))]
pub struct TriGouraud {
    /// OT linkage.
    pub tag: u32,
    /// Vertex 0: `opcode | color0`.
    pub color0_cmd: u32,
    /// Vertex 0 position.
    pub v0: u32,
    /// Vertex 1 color.
    pub color1: u32,
    /// Vertex 1 position.
    pub v1: u32,
    /// Vertex 2 color.
    pub color2: u32,
    /// Vertex 2 position.
    pub v2: u32,
}

impl TriGouraud {
    /// Data-word count after the tag.
    pub const WORDS: u8 = 6;

    /// Build a Gouraud-shaded triangle.
    pub const fn new(verts: [(i16, i16); 3], colors: [(u8, u8, u8); 3]) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        Self {
            tag: 0,
            color0_cmd: gp0::polygon_opcode(true, false, false, false, false)
                | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
        }
    }
}

/// Flat-shaded quad. 6 words (tag + 5 data).
#[repr(C, align(4))]
pub struct QuadFlat {
    /// OT linkage.
    pub tag: u32,
    /// `opcode | color`.
    pub color_cmd: u32,
    /// Vertex 0.
    pub v0: u32,
    /// Vertex 1.
    pub v1: u32,
    /// Vertex 2.
    pub v2: u32,
    /// Vertex 3.
    pub v3: u32,
}

impl QuadFlat {
    /// Data-word count.
    pub const WORDS: u8 = 5;

    /// Build a flat quad.
    pub const fn new(verts: [(i16, i16); 4], r: u8, g: u8, b: u8) -> Self {
        Self {
            tag: 0,
            color_cmd: gp0::polygon_opcode(false, true, false, false, false) | pack_color(r, g, b),
            v0: pack_vertex(verts[0].0, verts[0].1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            v2: pack_vertex(verts[2].0, verts[2].1),
            v3: pack_vertex(verts[3].0, verts[3].1),
        }
    }
}

/// Untextured variable-size rectangle. 4 words (tag + 3 data).
/// Ignores draw-area clip on some GPU revisions; prefer `QuadFlat`
/// when you need clipping.
#[repr(C, align(4))]
pub struct RectFlat {
    /// OT linkage.
    pub tag: u32,
    /// `0x60000000 | color` (monochrome rect opcode).
    pub color_cmd: u32,
    /// Top-left `xy`.
    pub xy: u32,
    /// Size `wh`.
    pub wh: u32,
}

impl RectFlat {
    /// Data-word count.
    pub const WORDS: u8 = 3;

    /// Build a rect.
    pub const fn new(x: i16, y: i16, w: u16, h: u16, r: u8, g: u8, b: u8) -> Self {
        Self {
            tag: 0,
            color_cmd: 0x6000_0000 | pack_color(r, g, b),
            xy: pack_vertex(x, y),
            wh: pack_xy(w, h),
        }
    }
}

/// Gouraud-shaded quad. 9 words (tag + 8 data).
///
/// Same vertex order as [`QuadFlat`] (V0=TL, V1=TR, V2=BL, V3=BR
/// by convention, though the GPU actually draws (V0,V1,V2) then
/// (V1,V2,V3)). Each vertex carries its own RGB; the GPU
/// gouraud-interpolates across the primitive.
#[repr(C, align(4))]
pub struct QuadGouraud {
    /// OT linkage.
    pub tag: u32,
    /// Vertex 0 colour + polygon opcode.
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
    /// Vertex 3 colour.
    pub color3: u32,
    /// Vertex 3 position.
    pub v3: u32,
}

impl QuadGouraud {
    /// Data-word count after the tag.
    pub const WORDS: u8 = 8;

    /// Build a Gouraud quad. `colors[i]` corresponds to `verts[i]`.
    pub const fn new(verts: [(i16, i16); 4], colors: [(u8, u8, u8); 4]) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        let (r3, g3, b3) = colors[3];
        Self {
            tag: 0,
            color0_cmd: gp0::polygon_opcode(true, true, false, false, false)
                | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
            color3: pack_color(r3, g3, b3),
            v3: pack_vertex(verts[3].0, verts[3].1),
        }
    }
}

/// Monochrome single line. 4 words (tag + 3 data). GP0 0x40 -- the
/// real diagonal-capable line rasteriser (unlike `RectFlat`, which
/// the GPU snaps to 16-pixel X boundaries in its GP0 0x02 fill).
#[repr(C, align(4))]
pub struct LineMono {
    /// OT linkage.
    pub tag: u32,
    /// `0x40000000 | color` header.
    pub color_cmd: u32,
    /// First endpoint.
    pub v0: u32,
    /// Second endpoint.
    pub v1: u32,
}

impl LineMono {
    /// Data-word count.
    pub const WORDS: u8 = 3;

    /// Build a mono line.
    pub const fn new(x0: i16, y0: i16, x1: i16, y1: i16, r: u8, g: u8, b: u8) -> Self {
        Self {
            tag: 0,
            color_cmd: 0x4000_0000 | pack_color(r, g, b),
            v0: pack_vertex(x0, y0),
            v1: pack_vertex(x1, y1),
        }
    }
}

/// Textured triangle with a single flat tint. 9 words (tag + 8 data).
///
/// The first data word is GP0(E2) texture-window state, followed by
/// vertex + UV pairs; CLUT rides in vertex 0's UV high word, tpage in
/// vertex 1's UV high word (PSX-SPX convention for GP0 0x24). Emitting
/// E2 per triangle keeps windowed world materials from leaking state to
/// model triangles when the ordering table interleaves both.
#[repr(C, align(4))]
pub struct TriTextured {
    /// OT linkage.
    pub tag: u32,
    /// GP0(E2) texture-window command.
    pub tex_window: u32,
    /// `0x24000000 | tint` header.
    pub color_cmd: u32,
    /// Vertex 0 position.
    pub v0: u32,
    /// `(u0, v0, clut)` packed.
    pub uv0_clut: u32,
    /// Vertex 1 position.
    pub v1: u32,
    /// `(u1, v1, tpage)` packed.
    pub uv1_tpage: u32,
    /// Vertex 2 position.
    pub v2: u32,
    /// `(u2, v2, 0)` packed.
    pub uv2: u32,
}

impl TriTextured {
    /// Data-word count.
    pub const WORDS: u8 = 8;

    /// Build a textured triangle. `tint = (128, 128, 128)` leaves
    /// texels unmodulated.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        verts: [(i16, i16); 3],
        uvs: [(u8, u8); 3],
        clut: u16,
        tpage: u16,
        tint: (u8, u8, u8),
    ) -> Self {
        Self::with_material(verts, uvs, TextureMaterial::opaque(clut, tpage, tint))
    }

    /// Build a textured triangle using a [`TextureMaterial`].
    pub const fn with_material(
        verts: [(i16, i16); 3],
        uvs: [(u8, u8); 3],
        material: TextureMaterial,
    ) -> Self {
        let (u0, v0) = uvs[0];
        let (u1, v1) = uvs[1];
        let (u2, v2) = uvs[2];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color_cmd: material.flat_textured_polygon_header(false),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: pack_texcoord(u0, v0, clut),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: pack_texcoord(u1, v1, tpage),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: pack_texcoord(u2, v2, 0),
        }
    }

    /// Build a textured triangle using the packet word layout stored in
    /// [`TriTextured`]. This avoids repacking UV words after construction
    /// when callers already need DMA/OT packet ordering.
    pub const fn with_material_packet_texcoords(
        verts: [(i16, i16); 3],
        uvs: [(u8, u8); 3],
        material: TextureMaterial,
    ) -> Self {
        let (u0, v0) = uvs[0];
        let (u1, v1) = uvs[1];
        let (u2, v2) = uvs[2];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color_cmd: material.flat_textured_polygon_header(false),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: pack_packet_texcoord(u0, v0, clut),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: pack_packet_texcoord(u1, v1, tpage),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: pack_packet_texcoord(u2, v2, 0),
        }
    }

    /// Build a textured triangle from UV words that already contain
    /// the low `(u, v)` bytes in packet layout. The material still
    /// supplies CLUT, tpage, tint, blend, and texture-window state.
    pub const fn with_material_packed_uv_words(
        verts: [(i16, i16); 3],
        uv_words: [u16; 3],
        material: TextureMaterial,
    ) -> Self {
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color_cmd: material.flat_textured_polygon_header(false),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: (uv_words[0] as u32) | ((clut as u32) << 16),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: (uv_words[1] as u32) | ((tpage as u32) << 16),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: uv_words[2] as u32,
        }
    }

    /// Build a textured triangle from UV words and prepacked material words.
    pub const fn with_packet_material_packed_uv_words(
        verts: [(i16, i16); 3],
        uv_words: [u16; 3],
        material: TexturedPacketMaterial,
    ) -> Self {
        Self {
            tag: 0,
            tex_window: material.tex_window_word,
            color_cmd: material.color_command_word,
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: (uv_words[0] as u32) | material.clut_high_word,
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: (uv_words[1] as u32) | material.tpage_high_word,
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: uv_words[2] as u32,
        }
    }
}

/// Textured **Gouraud-shaded** triangle. 11 words (tag + 10 data).
///
/// Per-vertex tint: the GPU multiplies each texel by the
/// interpolated vertex colour, so GTE-lit-and-fogged per-vertex
/// colours drive the final shade smoothly across the triangle.
/// The first data word is GP0(E2) texture-window state, matching
/// [`TriTextured`] so windowed/tiled world materials do not leak
/// state across ordering-table interleaving. CLUT rides in v0's
/// UV high word, tpage in v1's UV high word (PSX-SPX convention
/// for GP0 0x34).
#[repr(C, align(4))]
pub struct TriTexturedGouraud {
    /// OT linkage.
    pub tag: u32,
    /// GP0(E2) texture-window command.
    pub tex_window: u32,
    /// `0x34000000 | color0` header -- v0's RGB is packed into the
    /// same word as the polygon opcode.
    pub color0_cmd: u32,
    /// Vertex 0 position.
    pub v0: u32,
    /// `(u0, v0, clut)` packed.
    pub uv0_clut: u32,
    /// Vertex 1 colour (RGB in low 24 bits; top byte ignored).
    pub color1: u32,
    /// Vertex 1 position.
    pub v1: u32,
    /// `(u1, v1, tpage)` packed.
    pub uv1_tpage: u32,
    /// Vertex 2 colour.
    pub color2: u32,
    /// Vertex 2 position.
    pub v2: u32,
    /// `(u2, v2, 0)` packed.
    pub uv2: u32,
}

impl TriTexturedGouraud {
    /// Data-word count.
    pub const WORDS: u8 = 10;

    /// Build a textured Gouraud triangle. Each vertex carries its
    /// own RGB (the NCDT-lit-and-fogged colour in the typical
    /// commercial-game path) which modulates the sampled texel.
    pub const fn new(
        verts: [(i16, i16); 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        clut: u16,
        tpage: u16,
    ) -> Self {
        Self::with_material(verts, uvs, colors, TextureMaterial::new(clut, tpage))
    }

    /// Build a textured Gouraud triangle using a [`TextureMaterial`].
    pub const fn with_material(
        verts: [(i16, i16); 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
    ) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        let (u0, v0) = uvs[0];
        let (u1, v1) = uvs[1];
        let (u2, v2) = uvs[2];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color0_cmd: material.textured_polygon_command(true, false) | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: pack_texcoord(u0, v0, clut),
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: pack_texcoord(u1, v1, tpage),
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: pack_texcoord(u2, v2, 0),
        }
    }

    /// Build a textured Gouraud triangle using the packet word layout
    /// stored in [`TriTexturedGouraud`].
    pub const fn with_material_packet_texcoords(
        verts: [(i16, i16); 3],
        uvs: [(u8, u8); 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
    ) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        let (u0, v0) = uvs[0];
        let (u1, v1) = uvs[1];
        let (u2, v2) = uvs[2];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color0_cmd: material.textured_polygon_command(true, false) | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: pack_packet_texcoord(u0, v0, clut),
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: pack_packet_texcoord(u1, v1, tpage),
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: pack_packet_texcoord(u2, v2, 0),
        }
    }

    /// Build a textured Gouraud triangle from UV words that already
    /// contain the low `(u, v)` packet bytes.
    pub const fn with_material_packed_uv_words(
        verts: [(i16, i16); 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TextureMaterial,
    ) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color0_cmd: material.textured_polygon_command(true, false) | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: (uv_words[0] as u32) | ((clut as u32) << 16),
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: (uv_words[1] as u32) | ((tpage as u32) << 16),
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: uv_words[2] as u32,
        }
    }

    /// Build a textured Gouraud triangle from UV words and material
    /// packet words that were precomputed once for a hot material.
    pub const fn with_packet_material_packed_uv_words(
        verts: [(i16, i16); 3],
        uv_words: [u16; 3],
        colors: [(u8, u8, u8); 3],
        material: TexturedGouraudPacketMaterial,
    ) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        Self {
            tag: 0,
            tex_window: material.tex_window_word,
            color0_cmd: material.color0_command_word | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: (uv_words[0] as u32) | material.clut_high_word,
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: (uv_words[1] as u32) | material.tpage_high_word,
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: uv_words[2] as u32,
        }
    }
}

/// Textured Gouraud quad with inline texture-window state. Mirrors
/// [`TriTexturedGouraud`] extended to four vertices: GP0(E2)
/// texture-window state immediately followed by the GP0(3Ch)
/// Gouraud-textured-quad command. Per-vertex RGB modulates the sampled
/// texel exactly like the triangle. 14 words (tag + 13 data).
///
/// The PS1 GPU rasterizes this quad as the two triangles `(v0,v1,v2)`
/// and `(v1,v2,v3)` -- the `1`-`2` diagonal. A caller whose engine
/// splits quads on the `0`-`2` diagonal as `tri(a,b,c)+tri(a,c,d)` must
/// reorder its perimeter `[a,b,c,d]` to `[b,a,c,d]` before building the
/// packet so the hardware split lands on the same edge; that yields
/// pixel-identical output (proved by
/// `textured_gouraud_quad_matches_two_triangle_split_bitexact` in the
/// emulator GPU tests).
#[repr(C, align(4))]
pub struct QuadTexturedGouraud {
    /// OT linkage.
    pub tag: u32,
    /// GP0(E2) texture-window command.
    pub tex_window: u32,
    /// `0x3C000000 | color0` header -- v0's RGB shares the opcode word.
    pub color0_cmd: u32,
    /// Vertex 0 position.
    pub v0: u32,
    /// `(u0, v0, clut)` packed.
    pub uv0_clut: u32,
    /// Vertex 1 colour.
    pub color1: u32,
    /// Vertex 1 position.
    pub v1: u32,
    /// `(u1, v1, tpage)` packed.
    pub uv1_tpage: u32,
    /// Vertex 2 colour.
    pub color2: u32,
    /// Vertex 2 position.
    pub v2: u32,
    /// `(u2, v2, 0)` packed.
    pub uv2: u32,
    /// Vertex 3 colour.
    pub color3: u32,
    /// Vertex 3 position.
    pub v3: u32,
    /// `(u3, v3, 0)` packed.
    pub uv3: u32,
}

impl QuadTexturedGouraud {
    /// Data-word count (tag excluded).
    pub const WORDS: u8 = 13;

    /// Opcode bit promoting the Gouraud-textured-triangle header
    /// (`0x34`) to the Gouraud-textured-quad header (`0x3C`).
    const QUAD_OPCODE_BIT: u32 = 0x0800_0000;

    /// Build a textured Gouraud quad from UV words and a packet material
    /// precomputed for a hot material, mirroring
    /// [`TriTexturedGouraud::with_packet_material_packed_uv_words`].
    pub const fn with_packet_material_packed_uv_words(
        verts: [(i16, i16); 4],
        uv_words: [u16; 4],
        colors: [(u8, u8, u8); 4],
        material: TexturedGouraudPacketMaterial,
    ) -> Self {
        let (r0, g0, b0) = colors[0];
        let (r1, g1, b1) = colors[1];
        let (r2, g2, b2) = colors[2];
        let (r3, g3, b3) = colors[3];
        Self {
            tag: 0,
            tex_window: material.tex_window_word,
            color0_cmd: (material.color0_command_word | Self::QUAD_OPCODE_BIT)
                | pack_color(r0, g0, b0),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: (uv_words[0] as u32) | material.clut_high_word,
            color1: pack_color(r1, g1, b1),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: (uv_words[1] as u32) | material.tpage_high_word,
            color2: pack_color(r2, g2, b2),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: uv_words[2] as u32,
            color3: pack_color(r3, g3, b3),
            v3: pack_vertex(verts[3].0, verts[3].1),
            uv3: uv_words[3] as u32,
        }
    }
}

/// Textured quad with a single flat tint. 10 words (tag + 9 data).
///
/// Same CLUT + tpage embedding as [`TriTextured`], extended by
/// one vertex. Vertex order: TL, TR, BL, BR.
#[repr(C, align(4))]
pub struct QuadTextured {
    /// OT linkage.
    pub tag: u32,
    /// `0x2C000000 | tint` header.
    pub color_cmd: u32,
    /// V0 position.
    pub v0: u32,
    /// `(u0, v0, clut)`.
    pub uv0_clut: u32,
    /// V1 position.
    pub v1: u32,
    /// `(u1, v1, tpage)`.
    pub uv1_tpage: u32,
    /// V2 position.
    pub v2: u32,
    /// `(u2, v2, 0)`.
    pub uv2: u32,
    /// V3 position.
    pub v3: u32,
    /// `(u3, v3, 0)`.
    pub uv3: u32,
}

impl QuadTextured {
    /// Data-word count.
    pub const WORDS: u8 = 9;

    /// Build a textured quad.
    pub const fn new(
        verts: [(i16, i16); 4],
        uvs: [(u8, u8); 4],
        clut: u16,
        tpage: u16,
        tint: (u8, u8, u8),
    ) -> Self {
        Self::with_material(verts, uvs, TextureMaterial::opaque(clut, tpage, tint))
    }

    /// Build a textured quad using a [`TextureMaterial`].
    pub const fn with_material(
        verts: [(i16, i16); 4],
        uvs: [(u8, u8); 4],
        material: TextureMaterial,
    ) -> Self {
        let (u0, v0) = uvs[0];
        let (u1, v1) = uvs[1];
        let (u2, v2) = uvs[2];
        let (u3, v3) = uvs[3];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            color_cmd: material.flat_textured_polygon_header(true),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: pack_texcoord(u0, v0, clut),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: pack_texcoord(u1, v1, tpage),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: pack_texcoord(u2, v2, 0),
            v3: pack_vertex(verts[3].0, verts[3].1),
            uv3: pack_texcoord(u3, v3, 0),
        }
    }
}

/// Textured quad with inline texture-window state. 11 words (tag + 10 data).
///
/// This mirrors [`TriTextured`]'s self-contained OT packet shape for
/// quads: GP0(E2) texture-window state immediately followed by the
/// GP0(2Ch) textured-quad command. Use it when OT interleaving must
/// not leak window state across different textured draws.
#[repr(C, align(4))]
pub struct QuadTexturedMaterial {
    /// OT linkage.
    pub tag: u32,
    /// GP0(E2) texture-window command.
    pub tex_window: u32,
    /// `0x2C000000 | tint` header.
    pub color_cmd: u32,
    /// V0 position.
    pub v0: u32,
    /// `(u0, v0, clut)`.
    pub uv0_clut: u32,
    /// V1 position.
    pub v1: u32,
    /// `(u1, v1, tpage)`.
    pub uv1_tpage: u32,
    /// V2 position.
    pub v2: u32,
    /// `(u2, v2, 0)`.
    pub uv2: u32,
    /// V3 position.
    pub v3: u32,
    /// `(u3, v3, 0)`.
    pub uv3: u32,
}

impl QuadTexturedMaterial {
    /// Data-word count.
    pub const WORDS: u8 = 10;

    /// Build a textured quad with self-contained material state.
    pub const fn with_material(
        verts: [(i16, i16); 4],
        uvs: [(u8, u8); 4],
        material: TextureMaterial,
    ) -> Self {
        let (u0, v0) = uvs[0];
        let (u1, v1) = uvs[1];
        let (u2, v2) = uvs[2];
        let (u3, v3) = uvs[3];
        let clut = material.clut_word();
        let tpage = material.tpage_word();
        Self {
            tag: 0,
            tex_window: material.texture_window_word(),
            color_cmd: material.flat_textured_polygon_header(true),
            v0: pack_vertex(verts[0].0, verts[0].1),
            uv0_clut: pack_texcoord(u0, v0, clut),
            v1: pack_vertex(verts[1].0, verts[1].1),
            uv1_tpage: pack_texcoord(u1, v1, tpage),
            v2: pack_vertex(verts[2].0, verts[2].1),
            uv2: pack_texcoord(u2, v2, 0),
            v3: pack_vertex(verts[3].0, verts[3].1),
            uv3: pack_texcoord(u3, v3, 0),
        }
    }
}

/// Textured sprite (variable size). 5 words (tag + 4 data).
#[repr(C, align(4))]
pub struct Sprite {
    /// OT linkage.
    pub tag: u32,
    /// `0x64000000 | color` header (blend color applied over texture).
    pub color_cmd: u32,
    /// Top-left `xy`.
    pub xy: u32,
    /// `uv | clut` (U/V in low half, CLUT handle in high half).
    pub uv_clut: u32,
    /// Size `wh`.
    pub wh: u32,
}

impl Sprite {
    /// Data-word count.
    pub const WORDS: u8 = 4;

    /// Build a textured sprite. `clut` is the CLUT register handle
    /// (`y << 6 | x >> 4`); `uv` is the 8-bit texcoord within the
    /// texture page.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        uv: (u8, u8),
        clut: u16,
        r: u8,
        g: u8,
        b: u8,
    ) -> Self {
        let material = TextureMaterial::opaque(clut, 0, (r, g, b));
        Self::with_material(x, y, w, h, uv, material)
    }

    /// Build a textured sprite using a [`TextureMaterial`].
    ///
    /// Sprite packets do not carry a tpage word. The material's CLUT,
    /// tint, raw-texture bit, and semi-transparent command bit are
    /// encoded in the packet; the caller must set the matching draw
    /// mode before OT submission if the sprite samples a non-current
    /// tpage.
    pub const fn with_material(
        x: i16,
        y: i16,
        w: u16,
        h: u16,
        uv: (u8, u8),
        material: TextureMaterial,
    ) -> Self {
        Self {
            tag: 0,
            color_cmd: material.textured_rect_header(),
            xy: pack_vertex(x, y),
            uv_clut: pack_texcoord(uv.0, uv.1, material.clut_word()),
            wh: pack_xy(w, h),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Gouraud-textured quad packet must serialize to the exact GP0
    /// 0x3C word stream: a GP0(E2) texture-window prefix, then
    /// `[0x3C|c0, v0, uv0|clut, c1, v1, uv1|tpage, c2, v2, uv2, c3, v3, uv3]`.
    /// This guards the on-wire layout the DMA walker and emulator decode.
    #[test]
    fn quad_textured_gouraud_serializes_to_gp0_3c_stream() {
        let material =
            TexturedGouraudPacketMaterial::from_texture(TextureMaterial::new(0x1234, 0x0105));
        let uvw = [0x0201u16, 0x3C04, 0x3C42, 0x0240];
        let cols = [
            (0x11u8, 0x22u8, 0x33u8),
            (0x44, 0x55, 0x66),
            (0x77, 0x88, 0x99),
            (0xAA, 0xBB, 0xCC),
        ];
        let verts = [(10i16, 20i16), (110, 20), (110, 90), (10, 90)];
        let quad =
            QuadTexturedGouraud::with_packet_material_packed_uv_words(verts, uvw, cols, material);

        assert_eq!(core::mem::size_of::<QuadTexturedGouraud>(), 14 * 4);
        assert_eq!(QuadTexturedGouraud::WORDS, 13);

        let words = unsafe {
            core::slice::from_raw_parts((&quad as *const QuadTexturedGouraud).cast::<u32>(), 14)
        };
        assert_eq!(words[0], 0, "tag zero until OT insert");
        assert_eq!(words[1], material.tex_window_word, "E2 window prefix");
        assert_eq!(words[2] >> 24, 0x3C, "Gouraud+textured+quad opcode");
        assert_eq!(words[2] & 0x00FF_FFFF, pack_color(0x11, 0x22, 0x33));
        assert_eq!(words[3], pack_vertex(10, 20));
        assert_eq!(words[4], 0x0201 | material.clut_high_word);
        assert_eq!(words[5], pack_color(0x44, 0x55, 0x66));
        assert_eq!(words[6], pack_vertex(110, 20));
        assert_eq!(words[7], 0x3C04 | material.tpage_high_word);
        assert_eq!(words[8], pack_color(0x77, 0x88, 0x99));
        assert_eq!(words[9], pack_vertex(110, 90));
        assert_eq!(words[10], 0x3C42);
        assert_eq!(words[11], pack_color(0xAA, 0xBB, 0xCC));
        assert_eq!(words[12], pack_vertex(10, 90));
        assert_eq!(words[13], 0x0240);
    }
}
