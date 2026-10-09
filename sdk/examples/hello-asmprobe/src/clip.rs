// SPDX-License-Identifier: GPL-2.0-or-later
//! One-plane lane clip: the portable definition against the kernel.

use crate::{print_dec, Rng};
use core::hint::black_box;
use psx_math::clip_lanes::{clip_lanes8_plane, clip_lanes8_plane_reference, LanePlane, Lanes8};

/// GoldSrc-shaped planes for a 320x240 view with projection distance `h`.
fn view_planes(h: i32, capacity: u32) -> [LanePlane; 5] {
    let (ofx, ofy, vx0, vx1, vy0, vy1) = (160, 120, 0, 320, 0, 240);
    let plane = |primary_offset, primary, depth, bias, force| LanePlane {
        primary_offset,
        primary,
        depth,
        bias,
        force_depth: force,
        forced_depth: 8,
        capacity,
    };
    [
        plane(0, 0, 1, -8, 1),         // near: z - 8, crossing lands on z = 8
        plane(0, h, ofx - vx0, 0, 0),  // left
        plane(0, -h, vx1 - ofx, 0, 0), // right
        plane(4, h, ofy - vy0, 0, 0),  // top
        plane(4, -h, vy1 - ofy, 0, 0), // bottom
    ]
}

fn make_vertex(rng: &mut Rng, mode: u32) -> Lanes8 {
    let r = rng.next();
    let s = rng.next();
    let t = rng.next();
    match mode {
        0 => [
            ((r & 0x1fff) as i32) - 4096,
            (((r >> 13) & 0x1fff) as i32) - 4096,
            ((s & 0x3fff) as i32) - 2000,
            (t & 0xff) as i32,
            ((t >> 8) & 0xff) as i32,
            ((t >> 16) & 0xff) as i32,
            (s >> 14 & 0xff) as i32,
            (s >> 22 & 0xff) as i32,
        ],
        // The whole i32 range on every lane: products and differences wrap.
        1 => [
            r as i32,
            s as i32,
            t as i32,
            rng.next() as i32,
            rng.next() as i32,
            rng.next() as i32,
            rng.next() as i32,
            rng.next() as i32,
        ],
        // Small coordinates and a z around the near plane, so distances hit 0
        // and equal x or y make the endpoint order fall through to later lanes.
        2 => [
            (r & 3) as i32 - 1,
            ((r >> 2) & 3) as i32 - 1,
            4 + ((s & 7) as i32),
            (t & 0xff) as i32,
            ((t >> 8) & 0xff) as i32,
            ((t >> 16) & 0xff) as i32,
            (s >> 8 & 0xff) as i32,
            (s >> 16 & 0xff) as i32,
        ],
        // Extremes.
        _ => {
            let pick = |v: u32| match v % 5 {
                0 => i32::MIN,
                1 => i32::MAX,
                2 => 0,
                3 => -1,
                _ => 1,
            };
            [
                pick(r),
                pick(r >> 4),
                pick(r >> 8),
                pick(s),
                pick(s >> 4),
                pick(s >> 8),
                pick(t),
                pick(t >> 4),
            ]
        }
    }
}

static mut SRC: [Lanes8; 8] = [[0; 8]; 8];
static mut DST_REF: [Lanes8; 9] = [[0; 8]; 9];
static mut DST_TST: [Lanes8; 9] = [[0; 8]; 9];

pub fn run(cases: &mut u32, failures: &mut u32) {
    let mut rng = Rng(0x1357_9bdf);
    unsafe {
        let src = core::ptr::addr_of_mut!(SRC) as *mut Lanes8;
        let dref = core::ptr::addr_of_mut!(DST_REF) as *mut Lanes8;
        let dtst = core::ptr::addr_of_mut!(DST_TST) as *mut Lanes8;
        for mode in 0..4u32 {
            for iteration in 0..400u32 {
                let h = [256, 320, 1, 4096, -7][(iteration % 5) as usize];
                let capacity = if iteration % 7 == 0 {
                    2 + (iteration / 7) % 7
                } else {
                    8
                };
                let planes = view_planes(h, capacity);
                let n = (rng.next() % 9) as usize; // 0..=8
                for i in 0..n {
                    *src.add(i) = make_vertex(&mut rng, mode);
                }
                if iteration % 11 == 0 && n > 2 {
                    // Duplicate vertices: equal endpoints, d_a == d_b.
                    *src.add(1) = *src;
                }
                let plane = &planes[(iteration % 5) as usize];
                // Poison both destinations identically, including the spare slot.
                for i in 0..9 {
                    *dref.add(i) = [0x5a5a_5a5au32 as i32; 8];
                    *dtst.add(i) = [0x5a5a_5a5au32 as i32; 8];
                }
                *cases += 1;
                let a = clip_lanes8_plane_reference(src, n, dref, plane);
                let b = clip_lanes8_plane(src, n, dtst, plane);
                let mut same = a == b;
                for i in 0..9 {
                    if (*dref.add(i)) != (*dtst.add(i)) {
                        same = false;
                    }
                }
                if !same {
                    *failures += 1;
                    if *failures < 6 {
                        crate::tty_fail("clip", mode, iteration, (a as u32) << 8 | b as u32);
                    }
                }
            }
        }
    }
}

/// Representative HL polygons: triangles with one or two vertices outside the
/// plane, the way the view clip sees them, cycled across the five planes.
pub fn bench() {
    let mut rng = Rng(0x2468_ace1);
    unsafe {
        let src = core::ptr::addr_of_mut!(SRC) as *mut Lanes8;
        let dst = core::ptr::addr_of_mut!(DST_TST) as *mut Lanes8;
        let planes = view_planes(256, 8);
        let mut total_ref = 0u32;
        let mut total_k = 0u32;
        let mut over = 0u32;
        let mut calls = 0u32;
        for iteration in 0..200u32 {
            // A triangle across the near plane or one side plane: vertices
            // straddle by construction.
            let plane = &planes[(iteration % 5) as usize];
            let n = 3 + (iteration % 3) as usize; // triangles, quads, pentagons
            for i in 0..n {
                let mut v = make_vertex(&mut rng, 0);
                v[2] = v[2].abs() + 8 + (i as i32 * 700);
                // Push alternate vertices to the far side of the plane.
                if i % 2 == 0 {
                    match iteration % 5 {
                        0 => v[2] = 2 + (v[2] & 3),
                        1 => v[0] = -(v[2] + 100),
                        2 => v[0] = v[2] + 100,
                        3 => v[1] = -(v[2] + 100),
                        _ => v[1] = v[2] + 100,
                    }
                }
                *src.add(i) = v;
            }
            let a = crate::clock();
            let b = crate::clock();
            over += b.wrapping_sub(a) as u32;
            let a = crate::clock();
            let nr = black_box(clip_lanes8_plane_reference)(src, n, dst, plane);
            let b = crate::clock();
            total_ref += b.wrapping_sub(a) as u32;
            let a = crate::clock();
            let nk = black_box(clip_lanes8_plane)(src, n, dst, plane);
            let b = crate::clock();
            total_k += b.wrapping_sub(a) as u32;
            black_box((nr, nk));
            calls += 1;
        }
        crate::tty_label_clip("reference");
        print_dec("", (total_ref - over) * 10 / calls);
        crate::tty_label_clip("kernel");
        print_dec("", (total_k - over) * 10 / calls);
    }
}
