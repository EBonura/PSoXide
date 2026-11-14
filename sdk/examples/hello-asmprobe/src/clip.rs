// SPDX-License-Identifier: GPL-2.0-or-later
//! One-plane lane clip: the portable definition against the kernel `clip_lanes8_plane` runs.

use crate::{print_dec, print_u32, Rng};
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

/// Values at and next to the limits and the divisions of the interpolation (4096).
const LIMITS: [i32; 16] = [
    i32::MIN,
    i32::MIN + 1,
    -8193,
    -4097,
    -4096,
    -4095,
    -1,
    0,
    1,
    4095,
    4096,
    4097,
    8191,
    i32::MAX - 1,
    i32::MAX,
    0x4000_0000,
];

fn limit(v: u32) -> i32 {
    LIMITS[(v & 15) as usize]
}

/// The kinds of input. 0 to 3 are GoldSrc view planes; 4 and 5 draw the plane too.
const MODES: u32 = 6;

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
        3 => {
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
        // Tiny coordinates against tiny planes: distances are zero, equal or
        // opposite all the time, and the other lanes carry arbitrary words.
        4 => [
            (r % 7) as i32 - 3,
            ((r >> 3) % 7) as i32 - 3,
            ((r >> 6) % 16) as i32 - 3,
            s as i32,
            t as i32,
            (s >> 16) as i32,
            (t >> 16) as i32,
            (r >> 9) as i32,
        ],
        // Every lane at or next to a limit or a multiple of 4096.
        _ => [
            limit(r),
            limit(r >> 4),
            limit(r >> 8),
            limit(s),
            limit(s >> 4),
            limit(s >> 8),
            limit(t),
            limit(t >> 4),
        ],
    }
}

fn make_plane(rng: &mut Rng, mode: u32, iteration: u32, capacity: u32) -> LanePlane {
    match mode {
        0..=3 => {
            let h = [256, 320, 1, 4096, -7][(iteration % 5) as usize];
            view_planes(h, capacity)[((iteration / 5) % 5) as usize]
        }
        4 => {
            let r = rng.next();
            LanePlane {
                primary_offset: (r & 1) * 4,
                primary: ((r >> 1) % 5) as i32 - 2,
                depth: ((r >> 4) % 5) as i32 - 2,
                bias: ((r >> 7) % 7) as i32 - 3,
                force_depth: (r >> 10) & 1,
                forced_depth: ((r >> 11) % 9) as i32 - 4,
                capacity,
            }
        }
        _ => {
            let r = rng.next();
            let s = rng.next();
            LanePlane {
                primary_offset: (r & 1) * 4,
                primary: limit(r >> 1),
                depth: limit(r >> 5),
                bias: limit(r >> 9),
                force_depth: (s >> 1) & 1,
                forced_depth: limit(s >> 2),
                capacity,
            }
        }
    }
}

const SRC_VERTS: usize = 8;
static mut SRC: [Lanes8; SRC_VERTS] = [[0; 8]; SRC_VERTS];
static mut DST_REF: [Lanes8; 9] = [[0; 8]; 9];
static mut DST_TST: [Lanes8; 9] = [[0; 8]; 9];

fn fold(hash: &mut u32, words: &[i32]) {
    for &w in words {
        *hash = (*hash ^ w as u32).wrapping_mul(0x0100_0193);
    }
}

/// Differential cases per kind of input.
const ITERATIONS: u32 = 20_000;

pub fn run(cases: &mut u32, failures: &mut u32) {
    let mut rng = Rng(0x1357_9bdf);
    let (mut hash_ref, mut hash_kernel) = (0x811c_9dc5u32, 0x811c_9dc5u32);
    // How often an input took each road: non-empty, grew, hit capacity, emptied, unchanged.
    let mut seen = [0u32; 5];
    // SAFETY: the statics are private to this function, the source holds
    // `SRC_VERTS` vertices and each destination 9 (capacity is at most 8).
    unsafe {
        let src = core::ptr::addr_of_mut!(SRC) as *mut Lanes8;
        let dref = core::ptr::addr_of_mut!(DST_REF) as *mut Lanes8;
        let dtst = core::ptr::addr_of_mut!(DST_TST) as *mut Lanes8;
        for mode in 0..MODES {
            for iteration in 0..ITERATIONS {
                let capacity = if iteration % 3 == 0 {
                    rng.next() % 9 // 0..=8, including a destination that cannot hold anything
                } else {
                    8
                };
                let plane = make_plane(&mut rng, mode, iteration, capacity);
                let n = (rng.next() % 9) as usize; // 0..=8
                for i in 0..n {
                    *src.add(i) = make_vertex(&mut rng, mode);
                }
                // Zero-length edges: a vertex repeated next to itself.
                if iteration % 4 == 0 && n > 1 {
                    let at = (rng.next() as usize) % (n - 1);
                    *src.add(at + 1) = *src.add(at);
                }
                if iteration % 11 == 0 && n > 2 {
                    *src.add(n - 1) = *src;
                }
                // Poison both destinations identically, including the spare slot.
                for i in 0..9 {
                    *dref.add(i) = [0x5a5a_5a5au32 as i32; 8];
                    *dtst.add(i) = [0x5a5a_5a5au32 as i32; 8];
                }
                *cases += 1;
                let a = clip_lanes8_plane_reference(src, n, dref, &plane);
                let b = clip_lanes8_plane(src, n, dtst, &plane);
                let mut same = a == b;
                for i in 0..9 {
                    if (*dref.add(i)) != (*dtst.add(i)) {
                        same = false;
                    }
                }
                seen[0] += (a != 0) as u32;
                seen[1] += (a > n) as u32; // crossings added vertices
                seen[2] += (a == capacity as usize && n > 0) as u32; // full, or exactly full
                seen[3] += (a == 0 && n > 0) as u32; // everything clipped away
                seen[4] += (a == n && n > 0) as u32;
                fold(&mut hash_ref, &[a as i32]);
                fold(&mut hash_kernel, &[b as i32]);
                for i in 0..9 {
                    fold(&mut hash_ref, &*dref.add(i));
                    fold(&mut hash_kernel, &*dtst.add(i));
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
    for (label, count) in [
        "CLIP SEEN nonempty=",
        "CLIP SEEN grew=",
        "CLIP SEEN capacity=",
        "CLIP SEEN emptied=",
        "CLIP SEEN unchanged=",
    ]
    .into_iter()
    .zip(seen)
    {
        print_u32(label, count);
    }
    print_u32("CLIP HASH reference=", hash_ref);
    print_u32("CLIP HASH kernel=", hash_kernel);
}

/// Inputs per timing mix.
const BENCH_INPUTS: usize = 100;

static mut BENCH_SRC: [[Lanes8; SRC_VERTS]; BENCH_INPUTS] = [[[0; 8]; SRC_VERTS]; BENCH_INPUTS];

/// A polygon of `n` vertices against `planes[plane]`: vertex `i` is pushed to the far side of
/// the plane when `outside(i)`, and placed inside otherwise.
fn fill_polygon(
    rng: &mut Rng,
    plane: usize,
    n: usize,
    outside: impl Fn(usize) -> bool,
) -> [Lanes8; 8] {
    let mut poly = [[0; 8]; SRC_VERTS];
    for (i, vertex) in poly.iter_mut().enumerate().take(n) {
        let mut v = make_vertex(rng, 0);
        v[2] = v[2].abs() + 8 + (i as i32 * 700);
        if outside(i) {
            match plane {
                0 => v[2] = 2 + (v[2] & 3),
                1 => v[0] = -(v[2] + 100),
                2 => v[0] = v[2] + 100,
                3 => v[1] = -(v[2] + 100),
                _ => v[1] = v[2] + 100,
            }
        } else {
            // Well inside every side plane (|x|, |y| < z / 4).
            v[0] %= v[2] / 4;
            v[1] %= v[2] / 4;
        }
        *vertex = v;
    }
    poly
}

/// Cycles of `calls` runs of `f` over every input of the mix, in tenths of a call, with the
/// two clock reads taken out. The first pass warms the instruction cache.
unsafe fn time_mix(
    f: unsafe fn(*const Lanes8, usize, *mut Lanes8, &LanePlane) -> usize,
    inputs: &[(usize, usize)],
    plane: &[LanePlane; 5],
    overhead: u32,
) -> u32 {
    let dst = core::ptr::addr_of_mut!(DST_TST) as *mut Lanes8;
    let bench = core::ptr::addr_of!(BENCH_SRC) as *const [Lanes8; SRC_VERTS];
    let mut total = 0u32;
    for _ in 0..2 {
        total = 0;
        for (k, &(n, which)) in inputs.iter().enumerate() {
            let src = bench.add(k) as *const Lanes8;
            let a = crate::clock();
            let count = black_box(f)(src, n, dst, &plane[which]);
            let b = crate::clock();
            black_box(count);
            total += (b.wrapping_sub(a) as u32).wrapping_sub(overhead);
        }
    }
    total * 10 / inputs.len() as u32
}

/// Polygon mixes the way the view clip sees them. Each is cycled over the five view planes and
/// timed on the portable definition and on `clip_lanes8_plane`.
pub fn bench() {
    let mut rng = Rng(0x2468_ace1);
    let planes = view_planes(256, 8);
    // SAFETY: the statics are private to this module and `BENCH_INPUTS` x `SRC_VERTS` large.
    unsafe {
        let bench = core::ptr::addr_of_mut!(BENCH_SRC) as *mut [Lanes8; SRC_VERTS];
        let a = crate::clock();
        let b = crate::clock();
        let overhead = b.wrapping_sub(a) as u32;
        // (name, vertices, outside)
        let mixes: [(&str, usize, fn(usize) -> bool); 9] = [
            ("tri-inside", 3, |_| false),
            ("tri-outside", 3, |_| true),
            ("tri-1out", 3, |i| i == 0),
            ("tri-2out", 3, |i| i != 1),
            ("quad-1out", 4, |i| i == 0),
            ("quad-2out", 4, |i| i < 2),
            ("quad-alt", 4, |i| i % 2 == 0),
            ("hl-mix", 0, |i| i % 2 == 0),
            ("poly7-1out", 7, |i| i == 3),
        ];
        for (name, n, outside) in mixes {
            let mut inputs = [(0usize, 0usize); BENCH_INPUTS];
            for (k, input) in inputs.iter_mut().enumerate() {
                let plane = k % 5;
                // The old probe's mix: triangles, quads and pentagons, alternate vertices out.
                let n = if n == 0 { 3 + k % 3 } else { n };
                *bench.add(k) = fill_polygon(&mut rng, plane, n, outside);
                *input = (n, plane);
            }
            let reference = time_mix(clip_lanes8_plane_reference_fn, &inputs, &planes, overhead);
            let kernel = time_mix(clip_lanes8_plane_fn, &inputs, &planes, overhead);
            crate::tty_label_clip(name, "reference");
            print_dec("", reference);
            crate::tty_label_clip(name, "kernel");
            print_dec("", kernel);
        }
    }
}

unsafe fn clip_lanes8_plane_reference_fn(
    src: *const Lanes8,
    count: usize,
    dst: *mut Lanes8,
    plane: &LanePlane,
) -> usize {
    // SAFETY: the caller's contract, forwarded.
    unsafe { clip_lanes8_plane_reference(src, count, dst, plane) }
}

unsafe fn clip_lanes8_plane_fn(
    src: *const Lanes8,
    count: usize,
    dst: *mut Lanes8,
    plane: &LanePlane,
) -> usize {
    // SAFETY: the caller's contract, forwarded.
    unsafe { clip_lanes8_plane(src, count, dst, plane) }
}
