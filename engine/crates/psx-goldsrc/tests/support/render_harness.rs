#![allow(dead_code, unused_imports, static_mut_refs)]
mod new;
mod old;
mod shared;
fn cv(v: &old::CVert) -> new::CVert {
    new::CVert {
        v: v.v,
        rgb: v.rgb,
        uv: v.uv,
    }
}
fn os(v: old::SVert) -> [i32; 8] {
    [v.x, v.y, v.z, v.rgb.0, v.rgb.1, v.rgb.2, v.uv.0, v.uv.1]
}
fn ns(v: new::SVert) -> [i32; 8] {
    [v.x, v.y, v.z, v.rgb.0, v.rgb.1, v.rgb.2, v.uv.0, v.uv.1]
}
fn oc(v: old::CVert) -> [i32; 8] {
    [
        v.v[0], v.v[1], v.v[2], v.rgb.0, v.rgb.1, v.rgb.2, v.uv.0, v.uv.1,
    ]
}
fn nc(v: new::CVert) -> [i32; 8] {
    [
        v.v[0], v.v[1], v.v[2], v.rgb.0, v.rgb.1, v.rgb.2, v.uv.0, v.uv.1,
    ]
}
fn main() {
    let mut seed = 0x58d1f00du32;
    let mut rand = |bound: u32| {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed % bound) as i32
    };
    let mut count = 0;
    for (x, y, w, h) in VIEW_RECTS {
        unsafe { SELECT_VIEW }
        for focal in [40, 80, 160, 320, 640, 906] {
            unsafe {
                old::set_projection_h(focal);
                new::set_projection_h(focal);
            }
            for _ in 0..5000 {
                let a: [old::CVert; 4] = core::array::from_fn(|_| old::CVert {
                    v: [rand(20000) - 10000, rand(20000) - 10000, rand(4096) - 32],
                    rgb: (rand(256), rand(256), rand(256)),
                    uv: (rand(256), rand(256)),
                });
                let b = a.map(|v| cv(&v));
                for i in 0..4 {
                    assert_eq!(os(old::project_soft(&a[i])), ns(new::project_soft(&b[i])));
                    assert_eq!(
                        old::close_inv_q12(a[i].v[2].max(1)),
                        new::close_inv_q12(a[i].v[2].max(1))
                    );
                }
                assert_eq!(
                    old::quad_outside_vertical(&[&a[0], &a[1], &a[2], &a[3]]),
                    new::quad_outside_vertical(&[&b[0], &b[1], &b[2], &b[3]])
                );
                assert_eq!(
                    oc(old::projected_midpoint_cv(a[0], a[1])),
                    nc(new::projected_midpoint_cv(b[0], b[1]))
                );
                let mut o = [old::EMPTY_CV; 4];
                let mut n = [new::EMPTY_CV; 4];
                let on = old::near_clip(&[a[0], a[1], a[2]], &mut o);
                let nn = new::near_clip(&[b[0], b[1], b[2]], &mut n);
                assert_eq!(on, nn);
                for i in 0..on {
                    assert_eq!(oc(o[i]), nc(n[i]));
                }
                unsafe {
                    let (op, on) = old::visible_clip([&a[0], &a[1], &a[2]]);
                    let (np, nn) = new::visible_clip([&b[0], &b[1], &b[2]]);
                    assert_eq!(on, nn);
                    for i in 0..on {
                        assert_eq!(os(*op.add(i)), ns(*np.add(i)));
                    }
                }
                let sp: [old::SVert; 4] = core::array::from_fn(|i| old::SVert {
                    x: rand(2400) - 1000,
                    y: rand(1400) - 500,
                    z: a[i].v[2].max(8),
                    rgb: a[i].rgb,
                    uv: a[i].uv,
                });
                let sn = sp.map(|v| new::SVert {
                    x: v.x,
                    y: v.y,
                    z: v.z,
                    rgb: v.rgb,
                    uv: v.uv,
                });
                let mut go = [old::EMPTY_SV; 8];
                let mut gn = [new::EMPTY_SV; 8];
                let on = old::guard_clip(&sp, 3, &mut go);
                let nn = new::guard_clip(&sn, 3, &mut gn);
                assert_eq!(on, nn);
                for i in 0..on {
                    assert_eq!(os(go[i]), ns(gn[i]));
                }
                for i in 0..4 {
                    assert_eq!(
                        old::on_visible_boundary(&sp[i]),
                        new::on_visible_boundary(&sn[i])
                    );
                    assert_eq!(old::in_band(&sp[i]), new::in_band(&sn[i]));
                }
                assert_eq!(
                    os(old::perspective_screen_midpoint(sp[0], sp[1])),
                    ns(new::perspective_screen_midpoint(sn[0], sn[1]))
                );
                let xy = sp.map(|v| (v.x as i16, v.y as i16));
                let packed = xy.map(|(x, y)| x as u16 as u32 | ((y as u16 as u32) << 16));
                assert_eq!(
                    old::quad_fits_gpu_xy(packed[0], packed[1], packed[2], packed[3]),
                    new::quad_fits_gpu_xy(packed[0], packed[1], packed[2], packed[3])
                );
                assert_eq!(
                    old::quad_underlay_corners(xy),
                    new::quad_underlay_corners(xy)
                );
                assert_eq!(
                    old::affine_edge_needs_split(
                        (sp[0].x, sp[0].y),
                        sp[0].z,
                        sp[0].uv,
                        (sp[1].x, sp[1].y),
                        sp[1].z,
                        sp[1].uv,
                        2,
                        8
                    ),
                    new::affine_edge_needs_split(
                        (sn[0].x, sn[0].y),
                        sn[0].z,
                        sn[0].uv,
                        (sn[1].x, sn[1].y),
                        sn[1].z,
                        sn[1].uv,
                        2,
                        8
                    )
                );
                count += 1;
            }
        }
    }
    println!("PASS {count} seeded polygons: exact near/frustum/guard vertices, projection, RGB/UV/depth, extent/refinement/backstop policies across focal lengths and views");
}
