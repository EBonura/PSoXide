//! Packet writing: the straight-line fast path and the shared near-plane path.

use super::*;

impl GridRenderer {
    /// Write the queued quads as one packet stream into `out`.
    ///
    /// Forward (`reverse` false) the stream starts at `out[0]`. Reverse it
    /// ends at the end of `out`, written back to front, so a descending
    /// arena takes it in place instead of sliding it against the frame's
    /// packets; `GridFrame::packet_words` says how much of the tail is used.
    /// Either way the stream holds the quads in queue order.
    pub(super) fn emit(
        &mut self,
        room: &RoomView<'_>,
        eye: &RoomCamera<'_>,
        materials: &[GridMaterial],
        out: &mut [u32],
        frame: &mut GridFrame,
        reverse: bool,
    ) {
        if reverse {
            self.emit_dir::<true>(room, eye, materials, out, frame);
        } else {
            self.emit_dir::<false>(room, eye, materials, out, frame);
        }
    }

    #[inline(never)]
    fn emit_dir<const REVERSE: bool>(
        &mut self,
        room: &RoomView<'_>,
        eye: &RoomCamera<'_>,
        materials: &[GridMaterial],
        out: &mut [u32],
        frame: &mut GridFrame,
    ) {
        let start = out.as_mut_ptr();
        // SAFETY: one past the end of the slice.
        let end = unsafe { start.add(out.len()) };
        // Forward: the next word to write. Reverse: the first word written.
        let mut next = if REVERSE { end } else { start };
        let mut packets = 0u32;
        let cache = self.screen.as_ptr();
        let window_reset = TextureWindow::NONE.word();
        let mut robust_quads = 0u16;
        let mut robust_packets = 0u32;
        let quad_total = room.render.quads.len();
        let queue = self.quads.as_slice();
        let count = queue.len();
        for position in 0..count {
            // Reverse walks the queue backwards so the stream keeps its order.
            let q = usize::from(if REVERSE {
                queue[count - 1 - position]
            } else {
                queue[position]
            });
            let merged = q >= quad_total;
            // SAFETY: queued from valid quad indices (cell quads below
            // `quad_total`, merged quads after).
            let quad = unsafe {
                if merged {
                    room.render.lod_quads.get_unchecked(q - quad_total)
                } else {
                    room.render.quads.get_unchecked(q)
                }
            };
            // SAFETY: validation proved every corner index is in range. A
            // triangle repeats its third corner, so the fourth lookup is
            // harmless.
            let (s0, s1, s2, s3) = unsafe {
                (
                    &*cache.add(usize::from(quad.v[0])),
                    &*cache.add(usize::from(quad.v[1])),
                    &*cache.add(usize::from(quad.v[2])),
                    &*cache.add(usize::from(quad.v[3])),
                )
            };
            if s0.code & s1.code & s2.code & s3.code & OFF_ANY != 0 {
                frame.screen_culled += 1;
                continue;
            }
            let Some(material) = materials.get(usize::from(quad.material)) else {
                frame.material_missing += 1;
                continue;
            };
            if !material.resident {
                frame.material_missing += 1;
                continue;
            }
            let Some((origin, window)) = material.addressing(quad, window_reset) else {
                frame.material_missing += 1;
                continue;
            };
            let any = s0.code | s1.code | s2.code | s3.code;
            let triangle = quad.flags & QUAD_TRI != 0;
            // Every corner on screen and in front of the near plane: nothing
            // else to prove. Otherwise the corners must still fit the GPU.
            let fast = any == 0 || (any & NEAR == 0 && gpu_safe(&[s0, s1, s2, s3], triangle));
            if fast {
                let sum = u32::from(s0.sz) + u32::from(s1.sz) + u32::from(s2.sz);
                let otz = if triangle {
                    (sum * 0x155) >> 12
                } else {
                    (sum + u32::from(s3.sz)) >> 4
                };
                if otz == 0 || otz >= 2048 {
                    frame.depth_skipped += 1;
                    continue;
                }
                let words = if triangle {
                    QUAD_PACKET_WORDS - 3
                } else {
                    QUAD_PACKET_WORDS
                };
                // SAFETY: both pointers lie in the packet storage.
                let room_left = unsafe {
                    if REVERSE {
                        next.offset_from(start)
                    } else {
                        end.offset_from(next)
                    }
                };
                if room_left < words as isize {
                    frame.overflowed = true;
                    break;
                }
                // SAFETY: room for `words` was just checked.
                unsafe {
                    if REVERSE {
                        next = next.sub(words);
                    }
                    let after = if triangle {
                        write_triangle(next, quad, material, [s0, s1, s2], otz, origin, window)
                    } else {
                        write_quad(next, quad, material, [s0, s1, s2, s3], otz, origin, window)
                    };
                    if !REVERSE {
                        next = after;
                    }
                }
                packets += 1;
                continue;
            }
            // Near-plane, oversize and saturated quads share one path.
            // SAFETY: both pointers lie in the packet storage.
            let room_left = unsafe {
                if REVERSE {
                    next.offset_from(start)
                } else {
                    end.offset_from(next)
                }
            };
            if room_left < ROBUST_WORST_WORDS as isize {
                frame.overflowed = true;
                break;
            }
            robust_quads += 1;
            let before = packets;
            // SAFETY: room for the worst case was just checked. In reverse
            // the packets are built at the low end of the free space and then
            // moved up against the stream written so far.
            unsafe {
                if REVERSE {
                    let built = robust_quad(start, next, room, eye, quad, material, &mut packets);
                    let words = built.offset_from(start) as usize;
                    core::ptr::copy(start, next.sub(words), words);
                    next = next.sub(words);
                } else {
                    next = robust_quad(next, end, room, eye, quad, material, &mut packets);
                }
            }
            robust_packets += packets - before;
        }
        frame.robust_quads = robust_quads;
        frame.robust_packets = robust_packets.min(u32::from(u16::MAX)) as u16;
        frame.packets = packets;
        // SAFETY: both pointers lie in the packet storage.
        frame.packet_words = unsafe {
            if REVERSE {
                end.offset_from(next)
            } else {
                next.offset_from(start)
            }
        } as usize;
    }
}

/// Whether the GPU draws a quad with these screen corners as given: every
/// corner inside its 11-bit coordinates and the polygon under the 1024 by 512
/// span limit.
#[inline(always)]
fn gpu_safe(corners: &[&GridScreen; 4], triangle: bool) -> bool {
    let count = if triangle { 3 } else { 4 };
    let mut left = i32::MAX;
    let mut right = i32::MIN;
    let mut top = i32::MAX;
    let mut bottom = i32::MIN;
    for corner in corners.iter().take(count) {
        let x = i32::from(corner.sxy as u16 as i16);
        let y = i32::from((corner.sxy >> 16) as u16 as i16);
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    left > -1000
        && right < 1000
        && top > -1000
        && bottom < 1000
        && right - left <= 1023
        && bottom - top <= 511
}

/// Write one textured Gouraud quad packet; returns the next free word.
/// `origin` is the UV offset added to every corner and `window` the GP0(E2)
/// word the packet carries (see [`GridMaterial::addressing`]).
///
/// The GPU splits a quad `(a, b, c, d)` along `b-c`, so the corners go out as
/// `(1, 0, 2, 3)` to split along the 0-2 diagonal.
///
/// # Safety
/// `next` must have room for [`QUAD_PACKET_WORDS`].
#[inline(always)]
unsafe fn write_quad(
    next: *mut u32,
    quad: &Quad,
    material: &GridMaterial,
    s: [&GridScreen; 4],
    otz: u32,
    origin: u32,
    window: u32,
) -> *mut u32 {
    let clut = u32::from(material.clut) << 16;
    let tpage = u32::from(material.tpage) << 16;
    let command = TEXTURED_GOURAUD_COMMAND | 0x0800_0000 | material.command_bits;
    let uv = |k: usize| (u32::from(quad.uv[k]).wrapping_add(origin)) & 0xffff;
    // SAFETY: the caller reserved room for the whole packet.
    unsafe {
        next.write((13 << 24) | otz);
        next.add(1).write(window);
        next.add(2).write(command | quad.color[1]);
        next.add(3).write(s[1].sxy);
        next.add(4).write(uv(1) | clut);
        next.add(5).write(quad.color[0]);
        next.add(6).write(s[0].sxy);
        next.add(7).write(uv(0) | tpage);
        next.add(8).write(quad.color[2]);
        next.add(9).write(s[2].sxy);
        next.add(10).write(uv(2));
        next.add(11).write(quad.color[3]);
        next.add(12).write(s[3].sxy);
        next.add(13).write(uv(3));
        next.add(14)
    }
}

/// Write one textured Gouraud triangle packet; returns the next free word.
///
/// # Safety
/// `next` must have room for `QUAD_PACKET_WORDS - 3`.
#[inline(always)]
unsafe fn write_triangle(
    next: *mut u32,
    quad: &Quad,
    material: &GridMaterial,
    s: [&GridScreen; 3],
    otz: u32,
    origin: u32,
    window: u32,
) -> *mut u32 {
    let clut = u32::from(material.clut) << 16;
    let tpage = u32::from(material.tpage) << 16;
    let command = TEXTURED_GOURAUD_COMMAND | material.command_bits;
    let uv = |k: usize| (u32::from(quad.uv[k]).wrapping_add(origin)) & 0xffff;
    // SAFETY: the caller reserved room for the whole packet.
    unsafe {
        next.write((10 << 24) | otz);
        next.add(1).write(window);
        next.add(2).write(command | quad.color[0]);
        next.add(3).write(s[0].sxy);
        next.add(4).write(uv(0) | clut);
        next.add(5).write(quad.color[1]);
        next.add(6).write(s[1].sxy);
        next.add(7).write(uv(1) | tpage);
        next.add(8).write(quad.color[2]);
        next.add(9).write(s[2].sxy);
        next.add(10).write(uv(2));
        next.add(11)
    }
}

/// The shared path: clip against the near plane in room space, project the
/// result, and hand it to the depth-band splitter with the exact reprojection
/// and window clip.
///
/// # Safety
/// `next..end` must have room for [`ROBUST_WORST_WORDS`].
#[inline(never)]
unsafe fn robust_quad(
    next: *mut u32,
    end: *mut u32,
    room: &RoomView<'_>,
    eye: &RoomCamera<'_>,
    quad: &Quad,
    material: &GridMaterial,
    packets: &mut u32,
) -> *mut u32 {
    let count = if quad.flags & QUAD_TRI != 0 { 3 } else { 4 };
    let Some((origin, window)) = material.addressing(quad, TextureWindow::NONE.word()) else {
        return next;
    };
    let repeat = quad.flags2 & QUAD2_REPEAT != 0;
    let mut source = [AffineVertex::default(); 4];
    // One index walks four parallel tables; the indexed form keeps the loop
    // a counted one on the guest.
    #[allow(clippy::needless_range_loop)]
    for k in 0..count {
        // SAFETY: validation proved the index.
        let v = unsafe { room.render.vertices.get_unchecked(usize::from(quad.v[k])) };
        let uv = quad.uv[k].wrapping_add(origin as u16);
        source[k] = AffineVertex {
            position: [v.x, v.y, v.z],
            uv: [uv as u8, (uv >> 8) as u8],
            color: quad.color[k],
            screen: [0, 0],
            depth: 0,
        };
    }
    // A polygon wholly outside one side plane draws nothing, whatever the
    // near plane does to it. Nearly every near-plane quad in an open room is
    // this kind, so the test comes before the clip, the projection and the
    // splitter.
    let error = eye.frustum.side_error;
    for plane in &eye.frustum.planes[1..] {
        if source[..count]
            .iter()
            .all(|v| FrustumPlanes::distance(plane, v.position) < -error)
        {
            return next;
        }
    }
    // Room for the clipped polygon plus the splitter's scratch vertices.
    let mut clipped = [AffineVertex::default(); 6 + psx_engine::AFFINE_SPLIT_SCRATCH_VERTICES];
    let near = &eye.frustum.planes[0];
    let behind = source[..count]
        .iter()
        .any(|v| FrustumPlanes::distance(near, v.position) < 0);
    let polygon = if behind {
        let n = clip_polygon_plane(near, &source[..count], &mut clipped[..6]);
        if n < 3 {
            return next;
        }
        n
    } else {
        clipped[..count].copy_from_slice(&source[..count]);
        count
    };
    for vertex in clipped[..polygon].iter_mut() {
        let p = scene::project_vertex_scheduled(GteVec3I16::new(
            vertex.position[0],
            vertex.position[1],
            vertex.position[2],
        ));
        vertex.screen = [p.sx, p.sy];
        vertex.depth = u32::from(p.sz);
    }
    let surface = AffineSurface {
        first_vertex: 0,
        vertex_count: polygon as u16,
        tpage: material.tpage,
        clut: material.clut,
        uv_offset: [0; 2],
        // A repeating quad takes the windowed packet shape: its own window
        // word in front and a reset behind, with raw UVs.
        compact: u8::from(!repeat),
        texture_window_word: if repeat { window } else { 0 },
        color_command_word: TEXTURED_GOURAUD_COMMAND | material.command_bits,
    };
    let near_view = NearView {
        rows: eye.view.rotation.m,
        translation: [
            eye.view.translation.x,
            eye.view.translation.y,
            eye.view.translation.z,
        ],
        focal_length: eye.focal_length,
        center: [eye.screen[0] / 2, eye.screen[1] / 2],
    };
    // SAFETY: `clipped` holds `polygon` projected vertices plus the splitter's
    // scratch slots; `next..end` has room per the caller's contract.
    let drawn = unsafe {
        submit_surface_batch_near(
            clipped.as_mut_ptr(),
            polygon,
            &surface,
            1,
            next,
            end,
            GRID_RENDER_PROFILE,
            &near_view,
        )
    };
    *packets += drawn.packets;
    drawn.next_packet
}

#[cfg(test)]
mod tests;
