//! Runtime behaviour on the host: parsing, transforms against a float
//! reference, clipping, colour state and blend handling.

use psx_gpu::material::BlendMode;
use psx_vector::encode::{encode, GroupSource};
use psx_vector::format::{pack_rgb, Vertex, RUN_QUADS, RUN_TRIANGLES};
use psx_vector::{
    draw_mode_word, BlendOverride, ClipRect, DrawError, Emit, PacketCache, PacketError, ParseError,
    Placement, Scratch, Style, VectorAsset, VectorPacket, QUAD_WORDS, TRIANGLE_WORDS,
};

/// 4-byte-aligned copy of `bytes`.
fn aligned(bytes: &[u8]) -> Vec<u32> {
    let mut w = vec![0u32; bytes.len().div_ceil(4)];
    for (i, c) in bytes.chunks(4).enumerate() {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        w[i] = u32::from_le_bytes(b);
    }
    w
}

fn as_bytes(w: &[u32], len: usize) -> &[u8] {
    // SAFETY: `w` owns at least `len` initialised bytes and u8 needs no alignment.
    unsafe { core::slice::from_raw_parts(w.as_ptr().cast::<u8>(), len) }
}

fn q4(px: i32) -> i16 {
    (px * 16) as i16
}

/// A 64x32 px rectangle at (10, 20) with a left-red to right-blue gradient,
/// as group "box", plus a one-triangle additive group "glow".
fn sample() -> Vec<u8> {
    let v = |x: i32, y: i32, c: u32| Vertex {
        x: q4(x),
        y: q4(y),
        color: c,
    };
    let red = pack_rgb(255, 0, 0);
    let blue = pack_rgb(0, 0, 255);
    let boxg = GroupSource {
        name: "box".into(),
        vertices: vec![
            v(10, 20, red),
            v(74, 20, blue),
            v(74, 52, blue),
            v(10, 52, red),
        ],
        triangles: vec![[0, 1, 2], [0, 2, 3]],
        quads: vec![],
        runs: vec![(0, RUN_TRIANGLES, 2)],
        pivot: None,
    };
    let glow = GroupSource {
        name: "glow".into(),
        vertices: vec![
            v(0, 0, pack_rgb(9, 9, 9)),
            v(8, 0, pack_rgb(9, 9, 9)),
            v(0, 8, pack_rgb(9, 9, 9)),
        ],
        triangles: vec![[0, 1, 2]],
        quads: vec![],
        runs: vec![(2, RUN_TRIANGLES, 1)],
        pivot: Some([0, 0]),
    };
    // A 20x10 quad at (100, 10): red on the left corners, blue on the right.
    let quadg = GroupSource {
        name: "quad".into(),
        vertices: vec![
            v(100, 10, red),
            v(120, 10, blue),
            v(100, 20, red),
            v(120, 20, blue),
        ],
        triangles: vec![],
        quads: vec![[0, 1, 2, 3]],
        runs: vec![(0, RUN_QUADS, 1)],
        pivot: None,
    };
    encode([q4(100) as u16, q4(60) as u16], &[boxg, glow, quadg]).unwrap()
}

#[derive(Debug, Clone, PartialEq)]
struct Tri {
    p: [(i32, i32); 3],
    c: [u32; 3],
    translucent: bool,
}

fn run(
    asset: &VectorAsset<'_>,
    name: &str,
    place: &Placement,
    style: &Style,
) -> (Vec<Emit<'static>>, Vec<Tri>, Result<u32, DrawError>) {
    let mut scratch = Box::new(Scratch::<64>::new());
    let mut tris = Vec::new();
    let mut steps = Vec::new();
    let id = asset.group(name).unwrap();
    let r = asset.emit_group(id, place, style, &mut scratch, &mut |s| match s {
        Emit::Blend(m) => steps.push(Emit::Blend(m)),
        Emit::Triangle(t) => {
            let p = |w: u32| ((w as u16 as i16) as i32, ((w >> 16) as u16 as i16) as i32);
            tris.push(Tri {
                p: [p(t.v0), p(t.v1), p(t.v2)],
                c: [t.color0_cmd & 0xFF_FFFF, t.color1, t.color2],
                translucent: t.color0_cmd & (1 << 25) != 0,
            });
        }
        // Report a quad as the two triangles the GPU splits it into.
        Emit::Quad(q) => {
            let p = |w: u32| ((w as u16 as i16) as i32, ((w >> 16) as u16 as i16) as i32);
            let translucent = q.color0_cmd & (1 << 25) != 0;
            let c0 = q.color0_cmd & 0xFF_FFFF;
            tris.push(Tri {
                p: [p(q.v0), p(q.v1), p(q.v2)],
                c: [c0, q.color1, q.color2],
                translucent,
            });
            tris.push(Tri {
                p: [p(q.v1), p(q.v2), p(q.v3)],
                c: [q.color1, q.color2, q.color3],
                translucent,
            });
        }
    });
    (steps, tris, r)
}

#[test]
fn parse_rejects_bad_input() {
    let bytes = sample();
    let w = aligned(&bytes);
    let good = as_bytes(&w, bytes.len());
    assert!(VectorAsset::parse(good).is_ok());
    assert_eq!(
        VectorAsset::parse(&good[..40]).unwrap_err(),
        ParseError::Truncated
    );
    assert_eq!(
        VectorAsset::parse(&good[..good.len() - 4]).unwrap_err(),
        ParseError::Truncated
    );
    let mut bad = good.to_vec();
    bad[0] = b'X';
    let wb = aligned(&bad);
    assert_eq!(
        VectorAsset::parse(as_bytes(&wb, bad.len())).unwrap_err(),
        ParseError::BadMagic
    );
    // Misaligned view of an otherwise valid image.
    let mut shifted = vec![0u8; bytes.len() + 1];
    shifted[1..].copy_from_slice(good);
    let ws = aligned(&shifted);
    assert_eq!(
        VectorAsset::parse(&as_bytes(&ws, shifted.len())[1..]).unwrap_err(),
        ParseError::Misaligned
    );
    // A triangle index past the vertex count is caught at parse time.
    let mut corrupt = good.to_vec();
    let off_indices = u32::from_le_bytes(good[36..40].try_into().unwrap()) as usize;
    corrupt[off_indices] = 200;
    let wc = aligned(&corrupt);
    assert_eq!(
        VectorAsset::parse(as_bytes(&wc, corrupt.len())).unwrap_err(),
        ParseError::BadIndex
    );
}

#[test]
fn lookup_and_info() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    assert_eq!(a.group_count(), 3);
    let id = a.group("box").unwrap();
    assert_eq!(a.group_name(id), "box");
    assert_eq!(a.group("nope"), None);
    let info = a.group_info(id);
    assert_eq!((info.triangles, info.vertices), (2, 4));
    assert_eq!(info.bounds_q4, [q4(10), q4(20), q4(74), q4(52)]);
    // Default pivot is the bounds centre.
    assert_eq!(info.pivot_q4, [q4(42), q4(36)]);
    assert_eq!(a.max_group_vertices(), 4);
    assert_eq!(a.triangle_count(), 5); // 2 + 1 triangles and one quad
}

#[test]
fn identity_draw_reproduces_authored_pixels() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let (steps, tris, r) = run(&a, "box", &Placement::IDENTITY, &Style::DEFAULT);
    assert_eq!(r, Ok(2));
    assert!(matches!(steps.as_slice(), [Emit::Blend(BlendMode::Opaque)]));
    assert_eq!(tris[0].p, [(10, 20), (74, 20), (74, 52)]);
    assert_eq!(tris[0].c, [0x0000FF, 0xFF0000, 0xFF0000]);
    assert!(!tris[0].translucent);
}

#[test]
fn offset_scale_and_rotation_match_a_float_reference() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let corners = [(10.0f64, 20.0f64), (74.0, 20.0), (74.0, 52.0)];
    for (angle_q12, scale_q12, dx, dy) in [
        (0u16, 4096u16, 5, -3),
        (0, 8192, 0, 0),
        (512, 4096, 100, 40),
        (1024, 6144, 7, 9),
        (2048, 4096, 0, 0),
        (3500, 2048, -10, 20),
    ] {
        let place = Placement::at(dx, dy).scaled(scale_q12).rotated(angle_q12);
        let (_, tris, r) = run(&a, "box", &place, &Style::DEFAULT);
        assert_eq!(r, Ok(2));
        let (pivot_x, pivot_y) = (42.0, 36.0);
        let (s, c) = (
            (angle_q12 as f64 / 4096.0 * std::f64::consts::TAU).sin(),
            (angle_q12 as f64 / 4096.0 * std::f64::consts::TAU).cos(),
        );
        let k = scale_q12 as f64 / 4096.0;
        for (got, want) in tris[0].p.iter().zip(corners) {
            let (rx, ry) = (want.0 - pivot_x, want.1 - pivot_y);
            let ex = pivot_x + dx as f64 + k * (c * rx - s * ry);
            let ey = pivot_y + dy as f64 + k * (s * rx + c * ry);
            assert!(
                (got.0 as f64 - ex).abs() <= 1.0,
                "x {got:?} vs {ex} at {angle_q12}/{scale_q12}"
            );
            assert!(
                (got.1 as f64 - ey).abs() <= 1.0,
                "y {got:?} vs {ey} at {angle_q12}/{scale_q12}"
            );
        }
    }
}

#[test]
fn explicit_pivot_rotates_about_it() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    // A quarter turn about (10, 20): the corner (74, 20) swings to (10, 84).
    let place = Placement::IDENTITY
        .rotated(1024)
        .about(q4(10) as i32, q4(20) as i32);
    let (_, tris, _) = run(&a, "box", &place, &Style::DEFAULT);
    assert_eq!(tris[0].p[0], (10, 20));
    assert_eq!(tris[0].p[1], (10, 84));
}

#[test]
fn tint_and_saturation_recolour_vertices() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let half = Style::tinted([128, 128, 128]);
    let (_, tris, _) = run(&a, "box", &Placement::IDENTITY, &half);
    assert_eq!(tris[0].c[0], pack_rgb(128, 0, 0));
    let grey = Style {
        saturation: 0,
        ..Style::DEFAULT
    };
    let (_, tris, _) = run(&a, "box", &Placement::IDENTITY, &grey);
    let [r, g, b] = [
        tris[0].c[0] & 255,
        (tris[0].c[0] >> 8) & 255,
        (tris[0].c[0] >> 16) & 255,
    ];
    assert!(
        r == g && g == b && (r as i32 - 76).abs() <= 1,
        "{r} {g} {b}"
    );
    // Saturation 255 and tint 255 leave the stored words untouched.
    let (_, tris, _) = run(&a, "box", &Placement::IDENTITY, &Style::DEFAULT);
    assert_eq!(tris[0].c[1], pack_rgb(0, 0, 255));
}

#[test]
fn blend_runs_and_overrides() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let (steps, tris, _) = run(&a, "glow", &Placement::IDENTITY, &Style::DEFAULT);
    assert!(matches!(steps.as_slice(), [Emit::Blend(BlendMode::Add)]));
    assert!(tris[0].translucent);
    let forced = Style {
        blend: BlendOverride::Force(BlendMode::Opaque),
        ..Style::DEFAULT
    };
    let (steps, tris, _) = run(&a, "glow", &Placement::IDENTITY, &forced);
    assert!(matches!(steps.as_slice(), [Emit::Blend(BlendMode::Opaque)]));
    assert!(!tris[0].translucent);
    let fade = Style {
        blend: BlendOverride::Force(BlendMode::Average),
        ..Style::DEFAULT
    };
    let (_, tris, _) = run(&a, "box", &Placement::IDENTITY, &fade);
    assert!(tris.iter().all(|t| t.translucent));
}

#[test]
fn scratch_too_small_is_reported() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let mut scratch = Scratch::<3>::new();
    let id = a.group("box").unwrap();
    let r = a.emit_group(
        id,
        &Placement::IDENTITY,
        &Style::DEFAULT,
        &mut scratch,
        &mut |_| {},
    );
    assert_eq!(r, Err(DrawError::ScratchTooSmall));
}

#[test]
fn clipping_keeps_pixels_inside_and_interpolates_colour() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let clip = ClipRect {
        x0: 20,
        y0: 25,
        x1: 50,
        y1: 45,
    };
    let (_, tris, r) = run(
        &a,
        "box",
        &Placement::IDENTITY,
        &Style::DEFAULT.clipped(clip),
    );
    assert!(r.unwrap() > 0);
    assert!(!tris.is_empty());
    for t in &tris {
        for &(x, y) in &t.p {
            assert!(
                x >= clip.x0 && x <= clip.x1 && y >= clip.y0 && y <= clip.y1,
                "{t:?}"
            );
        }
    }
    // The clipped fan covers the whole clip rectangle: the box spans it.
    let area: i64 = tris
        .iter()
        .map(|t| {
            let (a, b, c) = (t.p[0], t.p[1], t.p[2]);
            (((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)) as i64).abs()
        })
        .sum();
    // Crossing points round to whole pixels, so the fan is within a pixel of
    // the window along its diagonal cuts.
    assert!(
        (area - 2 * 30 * 20).abs() <= 2 * 30 * 20 / 25,
        "area {area}"
    );
    // Colour at the clip's left edge is a mix, not an end colour.
    let left = tris
        .iter()
        .flat_map(|t| t.p.iter().zip(t.c.iter()))
        .find(|((x, _), _)| *x == 20)
        .unwrap();
    let red = left.1 & 255;
    assert!(red > 100 && red < 255, "red {red}");
}

#[test]
fn offscreen_group_is_culled_by_bounds() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let clip = ClipRect {
        x0: 200,
        y0: 0,
        x1: 300,
        y1: 100,
    };
    let (_, tris, r) = run(
        &a,
        "box",
        &Placement::IDENTITY,
        &Style::DEFAULT.clipped(clip),
    );
    assert_eq!(r, Ok(0));
    assert!(tris.is_empty());
}

#[test]
fn packets_carry_their_draw_mode_and_match_the_emitted_triangles() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let place = Placement::at(3, 4);
    let mut scratch = Box::new(Scratch::<64>::new());
    let mut out = [VectorPacket::ZERO; 4];
    let n = a
        .emit_packets(
            a.group("box").unwrap(),
            &place,
            &Style::DEFAULT,
            &mut scratch,
            &mut out,
        )
        .unwrap();
    assert_eq!(n, 2);
    let (_, tris, _) = run(&a, "box", &place, &Style::DEFAULT);
    for (packet, tri) in out.iter().zip(&tris) {
        assert_eq!(packet.draw_mode, draw_mode_word(BlendMode::Opaque));
        assert_eq!(packet.color0_cmd & 0xFF_FFFF, tri.c[0]);
        assert_eq!(packet.color0_cmd >> 24, 0x30);
    }
    let n = a
        .emit_packets(
            a.group("glow").unwrap(),
            &place,
            &Style::DEFAULT,
            &mut scratch,
            &mut out,
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(out[0].draw_mode, draw_mode_word(BlendMode::Add));
    assert_eq!(out[0].color0_cmd >> 24, 0x32);
    let mut small = [VectorPacket::ZERO; 1];
    let err = a
        .emit_packets(
            a.group("box").unwrap(),
            &place,
            &Style::DEFAULT,
            &mut scratch,
            &mut small,
        )
        .unwrap_err();
    assert_eq!(err, PacketError::OutputTooSmall { needed: 2 });
}

#[test]
fn quads_emit_as_one_primitive_and_transform_like_triangles() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let id = a.group("quad").unwrap();
    assert_eq!(a.group_info(id).quads, 1);
    let mut scratch = Box::new(Scratch::<8>::new());
    let mut quads = 0;
    let r = a.emit_group(
        id,
        &Placement::at(5, 5),
        &Style::DEFAULT,
        &mut scratch,
        &mut |s| {
            if let Emit::Quad(q) = s {
                quads += 1;
                assert_eq!(q.v0, (105u32) | (15u32 << 16));
                assert_eq!(q.v3, (125u32) | (25u32 << 16));
                assert_eq!(q.color0_cmd >> 24, 0x38);
            }
        },
    );
    assert_eq!((r, quads), (Ok(2), 1));
}

#[test]
fn clipping_a_quad_cuts_it_as_two_triangles() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let id = a.group("quad").unwrap();
    let mut scratch = Box::new(Scratch::<8>::new());
    let mut seen = Vec::new();
    let clip = ClipRect {
        x0: 105,
        y0: 0,
        x1: 115,
        y1: 100,
    };
    a.emit_group(
        id,
        &Placement::IDENTITY,
        &Style::DEFAULT.clipped(clip),
        &mut scratch,
        &mut |s| {
            if let Emit::Triangle(t) = s {
                seen.push([t.v0, t.v1, t.v2]);
            }
            assert!(!matches!(s, Emit::Quad(_)));
        },
    )
    .unwrap();
    assert!(!seen.is_empty());
    for tri in seen {
        for w in tri {
            let x = (w as u16 as i16) as i32;
            assert!((105..=115).contains(&x), "x {x}");
        }
    }
    // Wholly inside stays a quad.
    let mut quads = 0;
    let inside = ClipRect {
        x0: 0,
        y0: 0,
        x1: 200,
        y1: 200,
    };
    a.emit_group(
        id,
        &Placement::IDENTITY,
        &Style::DEFAULT.clipped(inside),
        &mut scratch,
        &mut |s| quads += matches!(s, Emit::Quad(_)) as u32,
    )
    .unwrap();
    assert_eq!(quads, 1);
}

#[test]
fn packet_cache_links_back_to_front_with_the_right_node_lengths() {
    let bytes = sample();
    let w = aligned(&bytes);
    let a = VectorAsset::parse(as_bytes(&w, bytes.len())).unwrap();
    let mut scratch = Box::new(Scratch::<8>::new());
    let mut cache = PacketCache::<8>::new();
    let place = Placement::IDENTITY;
    let boxed = cache.push(
        &a,
        a.group("box").unwrap(),
        &place,
        &Style::DEFAULT,
        &mut scratch,
        5,
    );
    let quad = cache.push(
        &a,
        a.group("quad").unwrap(),
        &place,
        &Style::DEFAULT,
        &mut scratch,
        3,
    );
    assert_eq!((boxed, quad), (Ok(2), Ok(1)));
    assert_eq!(cache.len(), 3);
    let order: Vec<(usize, u8)> = cache.links().map(|(slot, p)| (slot, p.words())).collect();
    // Reverse of push order: the quad first, then the box's two triangles.
    assert_eq!(
        order,
        [(3, QUAD_WORDS), (5, TRIANGLE_WORDS), (5, TRIANGLE_WORDS)]
    );
    // Nothing is added when a push does not fit.
    let mut full = PacketCache::<2>::new();
    assert!(full
        .push(
            &a,
            a.group("box").unwrap(),
            &place,
            &Style::DEFAULT,
            &mut scratch,
            0
        )
        .is_ok());
    assert!(full
        .push(
            &a,
            a.group("quad").unwrap(),
            &place,
            &Style::DEFAULT,
            &mut scratch,
            0
        )
        .is_err());
    assert_eq!(full.len(), 2);
    full.clear();
    assert!(full.is_empty());
}
