//! Shared GP0 command-log interpreter.
//!
//! Both host-GPU backends consume the same `GpuCmdLogEntry` stream:
//! the accurate compute path (`replay::ComputeBackend`) and the
//! enhanced render path (`translator::Translator`). Before this
//! module each kept its own copy of the per-opcode FIFO layouts,
//! state-setter handling and primitive decoding, and the copies had
//! already drifted (the compute side lost the GP0(E2) texture window
//! on every textured primitive's tpage word).
//!
//! [`Interpreter`] owns the [`ReplayState`] and does ALL of the
//! decoding once: state setters (`0xE1..=0xE6`) update the state and
//! produce no event; drawable packets decode into a [`GpuEvent`]
//! carrying vertices (draw-offset applied), UVs, CLUT, raw colour
//! words and sizes. Backends only lower events: quad splitting,
//! flag packing and dispatch stay per-backend because the two
//! execution models genuinely differ.
//!
//! Decode order inside each packet mirrors the CPU
//! (`emulator-core::Gpu`) exactly, including applying the UV1-word
//! tpage mid-packet, so the state a backend observes after
//! [`Interpreter::interpret`] matches what the CPU rasterizer saw.

use emulator_core::gpu::GpuCmdLogEntry;

use crate::decode::{
    apply_primitive_tpage, decode_clut, decode_uv, decode_vertex, sign_extend_11, ReplayState,
};

/// One decoded drawable GP0 packet. Colour fields carry the raw
/// command/colour words (low 24 bits significant); each backend
/// converts to its own representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuEvent {
    /// GP0 0x02 quick fill. Absolute VRAM coords (no draw offset,
    /// no draw-area clip), already masked to hardware ranges.
    Fill {
        cmd: u32,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
    },
    /// GP0 0x20..=0x23.
    MonoTri { cmd: u32, v: [(i32, i32); 3] },
    /// GP0 0x28..=0x2B. Backends split into two triangles with their
    /// own ordering.
    MonoQuad { cmd: u32, v: [(i32, i32); 4] },
    /// GP0 0x24..=0x27. The UV1 word's tpage half has already been
    /// applied to the state.
    TexTri {
        cmd: u32,
        v: [(i32, i32); 3],
        uv: [(u8, u8); 3],
        clut: (u32, u32),
    },
    /// GP0 0x2C..=0x2F.
    TexQuad {
        cmd: u32,
        v: [(i32, i32); 4],
        uv: [(u8, u8); 4],
        clut: (u32, u32),
    },
    /// GP0 0x30..=0x33. `colors[0]` is the command word itself.
    ShadedTri {
        cmd: u32,
        v: [(i32, i32); 3],
        colors: [u32; 3],
    },
    /// GP0 0x38..=0x3B.
    ShadedQuad {
        cmd: u32,
        v: [(i32, i32); 4],
        colors: [u32; 4],
    },
    /// GP0 0x34..=0x37.
    ShadedTexTri {
        cmd: u32,
        v: [(i32, i32); 3],
        uv: [(u8, u8); 3],
        colors: [u32; 3],
        clut: (u32, u32),
    },
    /// GP0 0x3C..=0x3F.
    ShadedTexQuad {
        cmd: u32,
        v: [(i32, i32); 4],
        uv: [(u8, u8); 4],
        colors: [u32; 4],
        clut: (u32, u32),
    },
    /// GP0 0x60..=0x63 / 0x68..=0x7B mono rectangles; fixed sizes
    /// already resolved into `w`/`h`.
    MonoRect {
        cmd: u32,
        xy: (i32, i32),
        w: u32,
        h: u32,
    },
    /// GP0 0x64..=0x67 / 0x6C..=0x7F textured rectangles. Sprites
    /// use the active state tpage (their packets carry no tpage).
    TexRect {
        cmd: u32,
        xy: (i32, i32),
        uv: (u8, u8),
        clut: (u32, u32),
        w: u32,
        h: u32,
    },
    /// GP0 0x40..=0x47 single / 0x48..=0x4F polyline, monochrome.
    /// `points` holds the decoded vertices in draw order: the two
    /// start-packet vertices plus any polyline continuation words the
    /// CPU GPU appended to the entry's fifo (terminator excluded).
    /// One segment per consecutive pair.
    MonoLine { cmd: u32, points: Vec<(i32, i32)> },
    /// GP0 0x50..=0x57 single / 0x58..=0x5F polyline, Gouraud.
    /// `colors[i]` pairs with `points[i]`; `colors[0]` is the command
    /// word itself (low 24 bits significant, like the tri events).
    ShadedLine {
        cmd: u32,
        points: Vec<(i32, i32)>,
        colors: Vec<u32>,
    },
    /// GP0 0x80..=0x9F VRAM-to-VRAM copy, masked to hardware ranges
    /// with the zero-means-full-size rule applied.
    VramCopy {
        sx: u32,
        sy: u32,
        dx: u32,
        dy: u32,
        w: u32,
        h: u32,
    },
    /// Anything not yet lowered (unknown opcodes).
    Unhandled { opcode: u8 },
}

/// Walks `GpuCmdLogEntry`s, tracking GP0 state and decoding drawable
/// packets into [`GpuEvent`]s.
pub struct Interpreter {
    /// GP0 state (tpage, texture window, draw area/offset, mask
    /// bits, dither, rect flips). Backends read it after
    /// [`Interpreter::interpret`] returns; the interpreter is the
    /// only writer.
    pub state: ReplayState,
}

impl Interpreter {
    pub fn new() -> Self {
        Self {
            state: ReplayState::new(),
        }
    }

    /// Process one logged GP0 packet. State setters and non-drawing
    /// packets (CPU<->VRAM transfers, NOPs) return `None`; drawable
    /// packets return the decoded event. Malformed packets (FIFO
    /// shorter than the opcode requires) are dropped silently, like
    /// both walkers always did.
    pub fn interpret(&mut self, entry: &GpuCmdLogEntry) -> Option<GpuEvent> {
        let fifo = &entry.fifo[..];
        match entry.opcode {
            // ---------- State setters ----------
            0xE1 => {
                self.handle_e1(fifo);
                None
            }
            0xE2 => {
                self.handle_e2(fifo);
                None
            }
            0xE3 => {
                self.handle_e3(fifo);
                None
            }
            0xE4 => {
                self.handle_e4(fifo);
                None
            }
            0xE5 => {
                self.handle_e5(fifo);
                None
            }
            0xE6 => {
                self.handle_e6(fifo);
                None
            }

            // ---------- Fill ----------
            0x02 => self.decode_fill(fifo),

            // ---------- Triangles / quads ----------
            0x20..=0x23 => self.decode_mono_tri(fifo),
            0x28..=0x2B => self.decode_mono_quad(fifo),
            0x24..=0x27 => self.decode_tex_tri(fifo),
            0x2C..=0x2F => self.decode_tex_quad(fifo),
            0x30..=0x33 => self.decode_shaded_tri(fifo),
            0x38..=0x3B => self.decode_shaded_quad(fifo),
            0x34..=0x37 => self.decode_shaded_tex_tri(fifo),
            0x3C..=0x3F => self.decode_shaded_tex_quad(fifo),

            // ---------- Lines / polylines ----------
            0x40..=0x4F => self.decode_mono_line(fifo),
            0x50..=0x5F => self.decode_shaded_line(fifo),

            // ---------- Rectangles ----------
            0x60..=0x63 => self.decode_mono_rect_variable(fifo),
            0x68..=0x6B => self.decode_mono_rect_fixed(fifo, 1, 1),
            0x70..=0x73 => self.decode_mono_rect_fixed(fifo, 8, 8),
            0x78..=0x7B => self.decode_mono_rect_fixed(fifo, 16, 16),
            0x64..=0x67 => self.decode_tex_rect_variable(fifo),
            0x6C..=0x6F => self.decode_tex_rect_fixed(fifo, 1, 1),
            0x74..=0x77 => self.decode_tex_rect_fixed(fifo, 8, 8),
            0x7C..=0x7F => self.decode_tex_rect_fixed(fifo, 16, 16),

            // ---------- VRAM-to-VRAM copy ----------
            0x80..=0x9F => decode_vram_copy(fifo),

            // CPU-to-VRAM upload: pixel data streams outside the
            // cmd_log; backends pick it up via VRAM sync. VRAM-to-CPU
            // readback mutates nothing. NOPs / clear-cache likewise.
            0xA0..=0xBF | 0xC0..=0xDF | 0x00 | 0x01 | 0x03..=0x1E => None,

            other => Some(GpuEvent::Unhandled { opcode: other }),
        }
    }

    // ========== State setters ==========

    /// GP0 0xE1 -- draw mode. Routes the tpage bits through
    /// [`apply_primitive_tpage`] (same decode, texture window
    /// preserved) by lifting the low half into a UV-word's high
    /// half, then applies the E1-only dither / rect-flip bits.
    fn handle_e1(&mut self, fifo: &[u32]) {
        let Some(&word) = fifo.first() else { return };
        apply_primitive_tpage(&mut self.state, word << 16);
        self.state.dither = (word >> 9) & 1 != 0;
        self.state.flip_x = (word >> 12) & 1 != 0;
        self.state.flip_y = (word >> 13) & 1 != 0;
    }

    /// GP0 0xE2 -- texture window. Stored pre-multiplied (x8) for
    /// the rasterizers, mirroring the CPU.
    fn handle_e2(&mut self, fifo: &[u32]) {
        let Some(&word) = fifo.first() else { return };
        self.state.tpage.tex_window_mask_x = (word & 0x1F) * 8;
        self.state.tpage.tex_window_mask_y = ((word >> 5) & 0x1F) * 8;
        self.state.tpage.tex_window_off_x = ((word >> 10) & 0x1F) * 8;
        self.state.tpage.tex_window_off_y = ((word >> 15) & 0x1F) * 8;
    }

    /// GP0 0xE3 -- drawing area top-left.
    fn handle_e3(&mut self, fifo: &[u32]) {
        let Some(&word) = fifo.first() else { return };
        self.state.draw_area.left = (word & 0x3FF) as i32;
        self.state.draw_area.top = ((word >> 10) & 0x1FF) as i32;
    }

    /// GP0 0xE4 -- drawing area bottom-right.
    fn handle_e4(&mut self, fifo: &[u32]) {
        let Some(&word) = fifo.first() else { return };
        self.state.draw_area.right = (word & 0x3FF) as i32;
        self.state.draw_area.bottom = ((word >> 10) & 0x1FF) as i32;
    }

    /// GP0 0xE5 -- drawing offset (11-bit signed).
    fn handle_e5(&mut self, fifo: &[u32]) {
        let Some(&word) = fifo.first() else { return };
        self.state.draw_offset_x = sign_extend_11((word & 0x7FF) as i32);
        self.state.draw_offset_y = sign_extend_11(((word >> 11) & 0x7FF) as i32);
    }

    /// GP0 0xE6 -- mask bit control.
    fn handle_e6(&mut self, fifo: &[u32]) {
        let Some(&word) = fifo.first() else { return };
        self.state.mask_set = word & 1 != 0;
        self.state.mask_check = word & 2 != 0;
    }

    // ========== Drawable packets ==========

    fn decode_fill(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 3 {
            return None;
        }
        Some(GpuEvent::Fill {
            cmd: fifo[0],
            // Hardware coordinate rounding (PSX-SPX, Redux cmdFillRect,
            // CPU fill_rect): X snaps DOWN to a 16-pixel boundary and
            // width rounds UP to the next multiple of 16. Decoding the
            // raw fields instead fills a narrower rect and leaves bands
            // of stale pixels at the edges (alttp gameplay, replay_bisect
            // owner=QuickFill).
            x: fifo[1] & 0x3F0,
            y: (fifo[1] >> 16) & 0x1FF,
            w: ((fifo[2] & 0x3FF) + 0xF) & !0xF,
            h: (fifo[2] >> 16) & 0x1FF,
        })
    }

    fn decode_mono_tri(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 4 {
            return None;
        }
        Some(GpuEvent::MonoTri {
            cmd: fifo[0],
            v: [
                decode_vertex(&self.state, fifo[1]),
                decode_vertex(&self.state, fifo[2]),
                decode_vertex(&self.state, fifo[3]),
            ],
        })
    }

    fn decode_mono_quad(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 5 {
            return None;
        }
        Some(GpuEvent::MonoQuad {
            cmd: fifo[0],
            v: [
                decode_vertex(&self.state, fifo[1]),
                decode_vertex(&self.state, fifo[2]),
                decode_vertex(&self.state, fifo[3]),
                decode_vertex(&self.state, fifo[4]),
            ],
        })
    }

    /// Packet: `[cmd+tint, v0, uv0+clut, v1, uv1+tpage, v2, uv2]`.
    /// The UV1 word's high half updates the tpage state mid-packet,
    /// exactly where the CPU applies it.
    fn decode_tex_tri(&mut self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 7 {
            return None;
        }
        let v0 = decode_vertex(&self.state, fifo[1]);
        let uv0 = decode_uv(fifo[2]);
        let clut = decode_clut(fifo[2]);
        let v1 = decode_vertex(&self.state, fifo[3]);
        let uv1 = decode_uv(fifo[4]);
        apply_primitive_tpage(&mut self.state, fifo[4]);
        let v2 = decode_vertex(&self.state, fifo[5]);
        let uv2 = decode_uv(fifo[6]);
        Some(GpuEvent::TexTri {
            cmd: fifo[0],
            v: [v0, v1, v2],
            uv: [uv0, uv1, uv2],
            clut,
        })
    }

    fn decode_tex_quad(&mut self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 9 {
            return None;
        }
        let v0 = decode_vertex(&self.state, fifo[1]);
        let uv0 = decode_uv(fifo[2]);
        let clut = decode_clut(fifo[2]);
        let v1 = decode_vertex(&self.state, fifo[3]);
        let uv1 = decode_uv(fifo[4]);
        apply_primitive_tpage(&mut self.state, fifo[4]);
        let v2 = decode_vertex(&self.state, fifo[5]);
        let uv2 = decode_uv(fifo[6]);
        let v3 = decode_vertex(&self.state, fifo[7]);
        let uv3 = decode_uv(fifo[8]);
        Some(GpuEvent::TexQuad {
            cmd: fifo[0],
            v: [v0, v1, v2, v3],
            uv: [uv0, uv1, uv2, uv3],
            clut,
        })
    }

    fn decode_shaded_tri(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 6 {
            return None;
        }
        Some(GpuEvent::ShadedTri {
            cmd: fifo[0],
            v: [
                decode_vertex(&self.state, fifo[1]),
                decode_vertex(&self.state, fifo[3]),
                decode_vertex(&self.state, fifo[5]),
            ],
            colors: [fifo[0], fifo[2], fifo[4]],
        })
    }

    fn decode_shaded_quad(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 8 {
            return None;
        }
        Some(GpuEvent::ShadedQuad {
            cmd: fifo[0],
            v: [
                decode_vertex(&self.state, fifo[1]),
                decode_vertex(&self.state, fifo[3]),
                decode_vertex(&self.state, fifo[5]),
                decode_vertex(&self.state, fifo[7]),
            ],
            colors: [fifo[0], fifo[2], fifo[4], fifo[6]],
        })
    }

    /// Packet: `[cmd+c0, v0, uv0+clut, c1, v1, uv1+tpage, c2, v2, uv2]`.
    fn decode_shaded_tex_tri(&mut self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 9 {
            return None;
        }
        let v0 = decode_vertex(&self.state, fifo[1]);
        let uv0 = decode_uv(fifo[2]);
        let clut = decode_clut(fifo[2]);
        let v1 = decode_vertex(&self.state, fifo[4]);
        let uv1 = decode_uv(fifo[5]);
        apply_primitive_tpage(&mut self.state, fifo[5]);
        let v2 = decode_vertex(&self.state, fifo[7]);
        let uv2 = decode_uv(fifo[8]);
        Some(GpuEvent::ShadedTexTri {
            cmd: fifo[0],
            v: [v0, v1, v2],
            uv: [uv0, uv1, uv2],
            colors: [fifo[0], fifo[3], fifo[6]],
            clut,
        })
    }

    fn decode_shaded_tex_quad(&mut self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 12 {
            return None;
        }
        let v0 = decode_vertex(&self.state, fifo[1]);
        let uv0 = decode_uv(fifo[2]);
        let clut = decode_clut(fifo[2]);
        let v1 = decode_vertex(&self.state, fifo[4]);
        let uv1 = decode_uv(fifo[5]);
        apply_primitive_tpage(&mut self.state, fifo[5]);
        let v2 = decode_vertex(&self.state, fifo[7]);
        let uv2 = decode_uv(fifo[8]);
        let v3 = decode_vertex(&self.state, fifo[10]);
        let uv3 = decode_uv(fifo[11]);
        Some(GpuEvent::ShadedTexQuad {
            cmd: fifo[0],
            v: [v0, v1, v2, v3],
            uv: [uv0, uv1, uv2, uv3],
            colors: [fifo[0], fifo[3], fifo[6], fifo[9]],
            clut,
        })
    }

    /// GP0 0x40..=0x47 / 0x48..=0x4F. Packet: `[cmd+color, v0, v1]`;
    /// polyline entries carry extra vertex words appended by the CPU
    /// GPU's cmd_log capture. The capture never logs the terminator,
    /// but the per-word sentinel check is kept for hand-built logs so
    /// both parsers apply the same rule as `Gpu::ingest_polyline_word`.
    fn decode_mono_line(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 3 {
            return None;
        }
        let mut points = vec![
            decode_vertex(&self.state, fifo[1]),
            decode_vertex(&self.state, fifo[2]),
        ];
        for &word in &fifo[3..] {
            if is_polyline_terminator(word) {
                break;
            }
            points.push(decode_vertex(&self.state, word));
        }
        Some(GpuEvent::MonoLine {
            cmd: fifo[0],
            points,
        })
    }

    /// GP0 0x50..=0x57 / 0x58..=0x5F. Packet: `[cmd+c0, v0, c1, v1]`;
    /// polyline continuations alternate (colour, vertex) words. The
    /// terminator check applies to EVERY word (colour slots included),
    /// mirroring the CPU; a trailing colour without its vertex is
    /// dropped, exactly like the CPU's `pending_color` never drawing.
    fn decode_shaded_line(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 4 {
            return None;
        }
        let mut points = vec![
            decode_vertex(&self.state, fifo[1]),
            decode_vertex(&self.state, fifo[3]),
        ];
        let mut colors = vec![fifo[0], fifo[2]];
        let mut pending_color: Option<u32> = None;
        for &word in &fifo[4..] {
            if is_polyline_terminator(word) {
                break;
            }
            match pending_color.take() {
                None => pending_color = Some(word),
                Some(color) => {
                    colors.push(color);
                    points.push(decode_vertex(&self.state, word));
                }
            }
        }
        Some(GpuEvent::ShadedLine {
            cmd: fifo[0],
            points,
            colors,
        })
    }

    fn decode_mono_rect_variable(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 3 {
            return None;
        }
        Some(GpuEvent::MonoRect {
            cmd: fifo[0],
            xy: decode_vertex(&self.state, fifo[1]),
            w: fifo[2] & 0xFFFF,
            h: (fifo[2] >> 16) & 0xFFFF,
        })
    }

    fn decode_mono_rect_fixed(&self, fifo: &[u32], w: u32, h: u32) -> Option<GpuEvent> {
        if fifo.len() < 2 {
            return None;
        }
        Some(GpuEvent::MonoRect {
            cmd: fifo[0],
            xy: decode_vertex(&self.state, fifo[1]),
            w,
            h,
        })
    }

    fn decode_tex_rect_variable(&self, fifo: &[u32]) -> Option<GpuEvent> {
        if fifo.len() < 4 {
            return None;
        }
        Some(GpuEvent::TexRect {
            cmd: fifo[0],
            xy: decode_vertex(&self.state, fifo[1]),
            uv: decode_uv(fifo[2]),
            clut: decode_clut(fifo[2]),
            w: fifo[3] & 0xFFFF,
            h: (fifo[3] >> 16) & 0xFFFF,
        })
    }

    fn decode_tex_rect_fixed(&self, fifo: &[u32], w: u32, h: u32) -> Option<GpuEvent> {
        if fifo.len() < 3 {
            return None;
        }
        Some(GpuEvent::TexRect {
            cmd: fifo[0],
            xy: decode_vertex(&self.state, fifo[1]),
            uv: decode_uv(fifo[2]),
            clut: decode_clut(fifo[2]),
            w,
            h,
        })
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

/// Polyline end sentinel -- both halves carry the `0x5xxx` pattern.
/// Same rule as `emulator-core::Gpu::ingest_polyline_word` (Redux:
/// `(word & 0xF000F000) == 0x50005000`).
#[inline]
fn is_polyline_terminator(word: u32) -> bool {
    (word & 0xF000_F000) == 0x5000_5000
}

/// Decode a GP0 0x80..=0x9F packet: `[cmd, src_xy, dst_xy, wh]`,
/// coordinates masked to VRAM and zero width/height meaning the full
/// 1024 / 512 extent (hardware rule).
fn decode_vram_copy(fifo: &[u32]) -> Option<GpuEvent> {
    if fifo.len() < 4 {
        return None;
    }
    const W: u32 = 1024;
    const H: u32 = 512;
    let raw_w = fifo[3] & (W - 1);
    let raw_h = (fifo[3] >> 16) & (H - 1);
    Some(GpuEvent::VramCopy {
        sx: fifo[1] & (W - 1),
        sy: (fifo[1] >> 16) & (H - 1),
        dx: fifo[2] & (W - 1),
        dy: (fifo[2] >> 16) & (H - 1),
        w: if raw_w == 0 { W } else { raw_w },
        h: if raw_h == 0 { H } else { raw_h },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(opcode: u8, fifo: Vec<u32>) -> GpuCmdLogEntry {
        GpuCmdLogEntry {
            index: 0,
            opcode,
            fifo: fifo.into(),
        }
    }

    #[test]
    fn vram_copy_masks_psx_fields() {
        let mut it = Interpreter::new();
        let ev = it
            .interpret(&entry(
                0x80,
                vec![0x80_00_00_00, 0x0203_0402, 0x0206_0405, 0x0201_0402],
            ))
            .unwrap();
        assert_eq!(
            ev,
            GpuEvent::VramCopy {
                sx: 2,
                sy: 3,
                dx: 5,
                dy: 6,
                w: 2,
                h: 1,
            }
        );
    }

    #[test]
    fn texture_window_survives_primitive_tpage_words() {
        let mut it = Interpreter::new();
        // GP0(E2): mask 8px / offset 16px on both axes.
        it.interpret(&entry(
            0xE2,
            vec![0xE200_0000 | 1 | (1 << 5) | (2 << 10) | (2 << 15)],
        ));
        assert_eq!(it.state.tpage.tex_window_mask_x, 8);
        assert_eq!(it.state.tpage.tex_window_off_x, 16);
        // A textured tri whose UV1 word carries a tpage half must
        // update the tpage WITHOUT clearing the E2 window (the CPU
        // stores them independently).
        it.interpret(&entry(
            0x24,
            vec![
                0x2480_8080,
                0,
                0,
                0x0000_0010,
                0x0001_0000, // tpage_x unit 1 in the high half
                0x0010_0010,
                0,
            ],
        ));
        assert_eq!(it.state.tpage.tpage_x, 64);
        assert_eq!(it.state.tpage.tex_window_mask_x, 8);
        assert_eq!(it.state.tpage.tex_window_mask_y, 8);
        assert_eq!(it.state.tpage.tex_window_off_x, 16);
        assert_eq!(it.state.tpage.tex_window_off_y, 16);
    }

    #[test]
    fn e1_applies_dither_and_flips() {
        let mut it = Interpreter::new();
        it.interpret(&entry(0xE1, vec![(1 << 9) | (1 << 12) | (1 << 13) | 0x2]));
        assert!(it.state.dither);
        assert!(it.state.flip_x);
        assert!(it.state.flip_y);
        assert_eq!(it.state.tpage.tpage_x, 2 * 64);
    }

    #[test]
    fn state_setters_yield_no_event_and_short_fifos_drop() {
        let mut it = Interpreter::new();
        assert!(it.interpret(&entry(0xE5, vec![0])).is_none());
        // Truncated mono tri: dropped, no panic.
        assert!(it.interpret(&entry(0x20, vec![0x2000_0000, 0])).is_none());
    }

    #[test]
    fn mono_line_single_decodes_two_points_with_offset() {
        let mut it = Interpreter::new();
        // Draw offset (16, 32) must apply to line vertices too.
        it.interpret(&entry(0xE5, vec![0xE500_0000 | 16 | (32 << 11)]));
        let ev = it
            .interpret(&entry(0x40, vec![0x40FF_FFFF, 0x0005_000A, 0x0014_001E]))
            .unwrap();
        assert_eq!(
            ev,
            GpuEvent::MonoLine {
                cmd: 0x40FF_FFFF,
                points: vec![(26, 37), (46, 52)],
            }
        );
    }

    #[test]
    fn mono_polyline_decodes_continuations_and_stops_at_terminator() {
        let mut it = Interpreter::new();
        let ev = it
            .interpret(&entry(
                0x48,
                vec![
                    0x48FF_FFFF,
                    0x0000_0000,
                    0x0000_0005,
                    0x0000_000A,
                    // Hand-built logs may still carry the sentinel;
                    // parsing must stop exactly like the CPU receive
                    // mode does.
                    0x5555_5555,
                    0x0000_0014,
                ],
            ))
            .unwrap();
        assert_eq!(
            ev,
            GpuEvent::MonoLine {
                cmd: 0x48FF_FFFF,
                points: vec![(0, 0), (5, 0), (10, 0)],
            }
        );
    }

    #[test]
    fn shaded_polyline_alternates_colors_and_drops_trailing_color() {
        let mut it = Interpreter::new();
        let ev = it
            .interpret(&entry(
                0x58,
                vec![
                    0x5800_00FF, // cmd + c0
                    0x0000_0000, // v0
                    0x0000_FF00, // c1
                    0x0000_0008, // v1
                    0x00FF_0000, // c2
                    0x0008_0008, // v2
                    0x0012_3456, // trailing colour without a vertex
                ],
            ))
            .unwrap();
        assert_eq!(
            ev,
            GpuEvent::ShadedLine {
                cmd: 0x5800_00FF,
                points: vec![(0, 0), (8, 0), (8, 8)],
                colors: vec![0x5800_00FF, 0x0000_FF00, 0x00FF_0000],
            }
        );
    }

    #[test]
    fn shaded_polyline_terminator_in_color_slot_ends_parse() {
        let mut it = Interpreter::new();
        let ev = it
            .interpret(&entry(
                0x58,
                vec![
                    0x5800_00FF,
                    0x0000_0000,
                    0x0000_FF00,
                    0x0000_0008,
                    0x5000_5000, // terminator where a colour would go
                    0x0008_0008,
                ],
            ))
            .unwrap();
        assert_eq!(
            ev,
            GpuEvent::ShadedLine {
                cmd: 0x5800_00FF,
                points: vec![(0, 0), (8, 0)],
                colors: vec![0x5800_00FF, 0x0000_FF00],
            }
        );
    }

    #[test]
    fn line_alias_opcodes_decode_like_their_base_families() {
        let mut it = Interpreter::new();
        for op in [0x44u8, 0x4C] {
            assert!(matches!(
                it.interpret(&entry(op, vec![(op as u32) << 24, 0, 1])),
                Some(GpuEvent::MonoLine { .. })
            ));
        }
        for op in [0x54u8, 0x5C] {
            assert!(matches!(
                it.interpret(&entry(op, vec![(op as u32) << 24, 0, 0, 1])),
                Some(GpuEvent::ShadedLine { .. })
            ));
        }
    }
}
