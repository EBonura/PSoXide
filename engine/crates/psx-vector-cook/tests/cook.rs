//! Cooker behaviour: curves, holes, gradients, groups, blend runs, budgets and
//! determinism, checked through the public API and the host rasterisers.

use psx_vector::format::{blend_from_code, COORD_SHIFT};
use psx_vector::{Placement, Style, VectorAsset};
use psx_vector_cook::raster::{compare, draw_group, draw_reference, AlignedAsset, Image};
use psx_vector_cook::{cook, Budget, CookError, CookOptions, Cooked};

fn svg(body: &str) -> String {
    format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"60\">{body}</svg>")
}

fn cook_ok(body: &str) -> Cooked {
    cook(&svg(body), &CookOptions::default()).unwrap_or_else(|e| panic!("cook failed: {e}"))
}

/// Total triangle area of the group named `name`, in square pixels.
fn area_px(asset: &VectorAsset<'_>, name: &str) -> f64 {
    let id = asset.group(name).expect("group");
    let mut sum = 0.0;
    let mut scratch = Box::new(psx_vector::Scratch::<4096>::new());
    let identity = Placement::IDENTITY;
    // Pull the triangles through the runtime so the test covers decoding too.
    asset
        .emit_group(id, &identity, &Style::DEFAULT, &mut scratch, &mut |step| {
            let p = |w: u32| ((w as u16 as i16) as f64, ((w >> 16) as u16 as i16) as f64);
            let tri = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
                ((b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)).abs() / 2.0
            };
            match step {
                psx_vector::Emit::Triangle(t) => sum += tri(p(t.v0), p(t.v1), p(t.v2)),
                psx_vector::Emit::Quad(q) => {
                    sum += tri(p(q.v0), p(q.v1), p(q.v2)) + tri(p(q.v1), p(q.v2), p(q.v3));
                }
                psx_vector::Emit::Blend(_) => {}
            }
        })
        .unwrap();
    sum
}

fn render_both(cooked: &Cooked) -> (Image, Image) {
    let aligned = AlignedAsset::new(&cooked.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    let mut from_tris = Image::new(100, 60, [0, 0, 0]);
    for id in asset.groups() {
        draw_group(
            &mut from_tris,
            &asset,
            id,
            &Placement::IDENTITY,
            &Style::DEFAULT,
            (0, 0),
        )
        .unwrap();
    }
    let mut from_shapes = Image::new(100, 60, [0, 0, 0]);
    draw_reference(&mut from_shapes, &cooked.reference, (0, 0));
    (from_tris, from_shapes)
}

#[test]
fn rect_has_exact_area_and_two_triangles() {
    let c = cook_ok(
        "<g id=\"r\"><rect x=\"10\" y=\"5\" width=\"40\" height=\"20\" fill=\"#f00\"/></g>",
    );
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    assert_eq!(c.report.groups[0].triangles, 2);
    assert_eq!(c.report.groups[0].vertices, 4);
    // Runtime positions are rounded to whole pixels, and 10..50 x 5..25 is exact.
    assert_eq!(area_px(&asset, "r"), 800.0);
}

#[test]
fn circle_area_matches_pi_r_squared_within_flattening_tolerance() {
    let c = cook_ok("<g id=\"c\"><circle cx=\"50\" cy=\"30\" r=\"25\" fill=\"#0f0\"/></g>");
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    let ideal = std::f64::consts::PI * 25.0 * 25.0;
    let got = area_px(&asset, "c");
    // Rounding vertices to whole pixels moves the boundary by at most half a
    // pixel; allow 3% of the area.
    assert!((got - ideal).abs() / ideal < 0.03, "area {got} vs {ideal}");
    // Curve flattening: n triangles for an n-gon is n - 2.
    let t = c.report.groups[0].triangles;
    assert!((10..80).contains(&t), "triangles {t}");
}

#[test]
fn bezier_flattening_respects_the_tolerance() {
    let body = "<g id=\"b\"><path d=\"M10 50 C10 0 90 0 90 50 Z\" fill=\"#fff\"/></g>";
    let loose = cook(
        &svg(body),
        &CookOptions {
            tolerance_px: 2.0,
            ..CookOptions::default()
        },
    )
    .unwrap();
    let tight = cook(
        &svg(body),
        &CookOptions {
            tolerance_px: 0.05,
            ..CookOptions::default()
        },
    )
    .unwrap();
    assert!(tight.report.groups[0].triangles > loose.report.groups[0].triangles);
    let (tris, shapes) = render_both(&tight);
    let d = compare(&tris, &shapes, 24, [0, 0, 100, 60]);
    assert!(d.fraction() < 0.01, "diff {d:?}");
}

#[test]
fn holes_are_cut_out_under_both_fill_rules() {
    let ring = "M20 10 H80 V50 H20 Z M35 20 V40 H65 V20 Z";
    for rule in ["evenodd", "nonzero"] {
        // The inner contour runs the other way, so it is a hole under both.
        let d = ring;
        let c = cook_ok(&format!(
            "<g id=\"h\"><path d=\"{d}\" fill-rule=\"{rule}\" fill=\"#fff\"/></g>"
        ));
        let aligned = AlignedAsset::new(&c.bytes);
        let asset = VectorAsset::parse(aligned.bytes()).unwrap();
        assert_eq!(area_px(&asset, "h"), 60.0 * 40.0 - 30.0 * 20.0, "{rule}");
        let (tris, shapes) = render_both(&c);
        assert_eq!(tris.pixel(50, 30), [0, 0, 0], "{rule}: hole is empty");
        assert_eq!(
            tris.pixel(25, 30),
            [255, 255, 255],
            "{rule}: frame is filled"
        );
        let diff = compare(&tris, &shapes, 24, [0, 0, 100, 60]);
        assert_eq!(diff.differing, 0, "{rule}: {diff:?}");
    }
}

#[test]
fn same_direction_inner_contour_is_a_hole_only_for_evenodd() {
    let d = "M20 10 H80 V50 H20 Z M35 20 H65 V40 H35 Z";
    let area = |rule: &str| {
        let c = cook_ok(&format!(
            "<g id=\"h\"><path d=\"{d}\" fill-rule=\"{rule}\"/></g>"
        ));
        let aligned = AlignedAsset::new(&c.bytes);
        let asset = VectorAsset::parse(aligned.bytes()).unwrap();
        area_px(&asset, "h")
    };
    assert_eq!(area("nonzero"), 60.0 * 40.0);
    assert_eq!(area("evenodd"), 60.0 * 40.0 - 30.0 * 20.0);
}

#[test]
fn concave_star_matches_the_analytic_reference() {
    let star = "M50 5 L61 40 L95 40 L67 60 L78 95 L50 73 L22 95 L33 60 L5 40 L39 40 Z";
    let c = cook(
        &format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\" viewBox=\"0 0 100 100\">\
             <g id=\"s\"><path d=\"{star}\" fill=\"#fc0\"/></g></svg>"
        ),
        &CookOptions::default(),
    )
    .unwrap();
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    let mut a = Image::new(100, 100, [0, 0, 0]);
    let mut b = Image::new(100, 100, [0, 0, 0]);
    draw_group(
        &mut a,
        &asset,
        asset.group("s").unwrap(),
        &Placement::IDENTITY,
        &Style::DEFAULT,
        (0, 0),
    )
    .unwrap();
    draw_reference(&mut b, &c.reference, (0, 0));
    let d = compare(&a, &b, 24, [0, 0, 100, 100]);
    assert_eq!(d.differing, 0, "{d:?}");
}

#[test]
fn linear_gradient_gives_each_vertex_its_colour_and_splits_at_stops() {
    let body = "<defs><linearGradient id=\"g\" x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\">\
                <stop offset=\"0\" stop-color=\"#000\"/>\
                <stop offset=\"0.5\" stop-color=\"#f00\"/>\
                <stop offset=\"1\" stop-color=\"#00f\"/></linearGradient></defs>\
                <g id=\"grad\"><rect x=\"0\" y=\"0\" width=\"100\" height=\"60\" fill=\"url(#g)\"/></g>";
    let c = cook_ok(body);
    // The 0.5 stop splits the rectangle's two triangles into more pieces.
    assert!(c.report.groups[0].triangles > 2, "{}", c.report);
    let (tris, shapes) = render_both(&c);
    // Left, middle and right match the stops.
    let near = |p: [u8; 3], q: [u8; 3]| (0..3).all(|i| p[i].abs_diff(q[i]) <= 6);
    assert!(
        near(tris.pixel(1, 30), [5, 0, 0]),
        "{:?}",
        tris.pixel(1, 30)
    );
    assert!(
        near(tris.pixel(50, 30), [255, 0, 0]),
        "{:?}",
        tris.pixel(50, 30)
    );
    assert!(
        near(tris.pixel(98, 30), [5, 0, 250]),
        "{:?}",
        tris.pixel(98, 30)
    );
    let d = compare(&tris, &shapes, 12, [0, 0, 100, 60]);
    assert!(d.fraction() < 0.005, "{d:?}");
}

#[test]
fn rotated_gradient_in_user_space_matches_the_reference() {
    let body = "<defs><linearGradient id=\"g\" gradientUnits=\"userSpaceOnUse\" x1=\"20\" y1=\"10\" x2=\"80\" y2=\"50\">\
                <stop offset=\"0\" stop-color=\"#fff\"/><stop offset=\"1\" stop-color=\"#048\"/></linearGradient></defs>\
                <g id=\"grad\" transform=\"translate(2 1)\"><circle cx=\"50\" cy=\"30\" r=\"28\" fill=\"url(#g)\"/></g>";
    let c = cook_ok(body);
    let (tris, shapes) = render_both(&c);
    let d = compare(&tris, &shapes, 16, [0, 0, 100, 60]);
    assert!(d.fraction() < 0.03, "{d:?}");
}

#[test]
fn groups_follow_the_nearest_id_and_keep_document_order() {
    let c = cook_ok(
        "<rect width=\"4\" height=\"4\"/>\
         <g id=\"outer\"><rect width=\"5\" height=\"5\"/>\
           <g><rect x=\"10\" width=\"5\" height=\"5\"/></g>\
           <g id=\"inner\"><rect x=\"20\" width=\"5\" height=\"5\"/></g></g>\
         <path id=\"solo\" d=\"M0 0 H5 V5 Z\"/>",
    );
    let names: Vec<&str> = c.report.groups.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["", "outer", "inner", "solo"]);
    assert_eq!(c.report.groups[1].triangles, 4); // own rect + the id-less child group's rect
    assert_eq!(c.report.groups[2].triangles, 2);
}

#[test]
fn duplicate_ids_and_unsupported_elements_are_named_errors() {
    let dup = cook(&svg("<g id=\"a\"/><g id=\"a\"/>"), &CookOptions::default()).unwrap_err();
    assert!(dup.to_string().contains("duplicate group id `a`"), "{dup}");
    let text = cook(
        &svg("<g id=\"t\"><text>hi</text></g>"),
        &CookOptions::default(),
    )
    .unwrap_err();
    assert!(text.to_string().contains("<text>"), "{text}");
    let bad = cook(
        &svg("<path id=\"x\" d=\"M0 0 L 10 10 L 10 0 L 0 10 Z\"/>"),
        &CookOptions::default(),
    )
    .unwrap_err();
    assert!(bad.to_string().contains("cross"), "{bad}");
}

#[test]
fn semi_transparency_becomes_blend_runs_in_paint_order() {
    let c = cook_ok(
        "<g id=\"fx\">\
         <rect width=\"10\" height=\"10\" fill=\"#fff\"/>\
         <rect x=\"20\" width=\"10\" height=\"10\" fill=\"#fff\" data-blend=\"add\"/>\
         <rect x=\"40\" width=\"10\" height=\"10\" fill=\"#fff\" fill-opacity=\"0.5\"/>\
         <rect x=\"60\" width=\"10\" height=\"10\" fill=\"#fff\"/></g>",
    );
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    let mut modes = Vec::new();
    let mut scratch = Box::new(psx_vector::Scratch::<64>::new());
    asset
        .emit_group(
            asset.group("fx").unwrap(),
            &Placement::IDENTITY,
            &Style::DEFAULT,
            &mut scratch,
            &mut |s| {
                if let psx_vector::Emit::Blend(m) = s {
                    modes.push(m);
                }
            },
        )
        .unwrap();
    use psx_gpu::material::BlendMode::*;
    assert_eq!(modes, [Opaque, Add, Average, Opaque]);
    assert_eq!(c.report.groups[0].runs, 4);
    assert!(blend_from_code(2).is_some());
}

#[test]
fn unrepresentable_opacity_is_an_error() {
    let e = cook(
        &svg("<rect width=\"4\" height=\"4\" fill-opacity=\"0.3\"/>"),
        &CookOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(e, CookError::Svg { .. }), "{e}");
}

#[test]
fn additive_alpha_scales_the_colour() {
    let c = cook_ok("<g id=\"g\"><rect width=\"10\" height=\"10\" fill=\"#fff\" fill-opacity=\"0.5\" data-blend=\"add\"/></g>");
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    let mut img = Image::new(100, 60, [10, 10, 10]);
    draw_group(
        &mut img,
        &asset,
        asset.group("g").unwrap(),
        &Placement::IDENTITY,
        &Style::DEFAULT,
        (0, 0),
    )
    .unwrap();
    assert_eq!(img.pixel(5, 5), [138, 138, 138]);
}

#[test]
fn budget_is_enforced_per_group_and_in_total() {
    let body = "<g id=\"a\"><circle cx=\"30\" cy=\"30\" r=\"20\"/></g><g id=\"b\"><rect width=\"9\" height=\"9\"/></g>";
    let mut opts = CookOptions {
        budget: Budget {
            total: Some(500),
            per_group: None,
            groups: [("a".to_string(), 8usize)].into(),
        },
        ..CookOptions::default()
    };
    let err = cook(&svg(body), &opts).unwrap_err();
    assert!(err.to_string().contains("group a"), "{err}");
    opts.budget = Budget {
        total: Some(3),
        per_group: None,
        groups: Default::default(),
    };
    let err = cook(&svg(body), &opts).unwrap_err();
    assert!(err.to_string().contains("total"), "{err}");
    opts.budget = Budget {
        total: Some(500),
        per_group: Some(500),
        groups: Default::default(),
    };
    assert!(cook(&svg(body), &opts).is_ok());
}

#[test]
fn cooking_is_deterministic_byte_for_byte() {
    let body = "<defs><linearGradient id=\"g\"><stop offset=\"0\" stop-color=\"#f80\"/><stop offset=\"1\" stop-color=\"#08f\"/></linearGradient></defs>\
                <g id=\"x\"><circle cx=\"50\" cy=\"30\" r=\"20\" fill=\"url(#g)\"/>\
                <path d=\"M5 5 C 30 0 60 20 95 5 L 95 50 Q 50 30 5 50 Z\" fill=\"#fff\" stroke=\"#0f0\" stroke-width=\"3\" stroke-linejoin=\"round\"/></g>";
    let a = cook_ok(body);
    let b = cook_ok(body);
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(a.report, b.report);
}

#[test]
fn view_box_and_group_transforms_apply() {
    let c = cook(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"60\" viewBox=\"0 0 50 30\">\
         <g id=\"m\" transform=\"translate(10 0) scale(2)\"><rect width=\"10\" height=\"5\"/></g></svg>",
        &CookOptions::default(),
    )
    .unwrap();
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    // viewBox scale 2: x runs (10 + 2 x) * 2 = 20..60, y runs 2 * 2 y = 0..20.
    assert_eq!(
        asset.group_info(asset.group("m").unwrap()).bounds_q4,
        [20 << COORD_SHIFT, 0, 60 << COORD_SHIFT, 20 << COORD_SHIFT]
    );
}

#[test]
fn strokes_cover_the_expected_area() {
    let c = cook_ok(
        "<g id=\"s\"><path d=\"M10 30 H90\" fill=\"none\" stroke=\"#fff\" stroke-width=\"6\"/></g>",
    );
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    assert_eq!(area_px(&asset, "s"), 80.0 * 6.0);
}

#[test]
fn pivot_attribute_sets_the_group_pivot() {
    let c = cook_ok("<g id=\"p\" data-pivot=\"20 10\"><rect width=\"40\" height=\"20\"/></g>");
    let aligned = AlignedAsset::new(&c.bytes);
    let asset = VectorAsset::parse(aligned.bytes()).unwrap();
    assert_eq!(
        asset.group_info(asset.group("p").unwrap()).pivot_q4,
        [20 * 16, 10 * 16]
    );
}

#[test]
fn rect_and_parallelogram_cook_to_single_quads() {
    let c = cook_ok(
        "<g id=\"q\"><rect width=\"10\" height=\"6\" fill=\"#fff\"/>\
         <path d=\"M20 0 H32 L30 6 H18 Z\" fill=\"#888\"/></g>",
    );
    let g = &c.report.groups[0];
    assert_eq!(
        (g.triangles, g.quads, g.vertices),
        (4, 2, 8),
        "{}",
        c.report
    );
}

#[test]
fn per_corner_colours_shade_a_quad_without_splitting_it() {
    let c = cook_ok(
        "<g id=\"q\"><path d=\"M10 10 H50 L46 30 H14 Z\" \
         data-corners=\"#ffffff #ffffff #000000 #000000\"/></g>",
    );
    let g = &c.report.groups[0];
    assert_eq!((g.triangles, g.quads), (2, 1), "{}", c.report);
    let (tris, _) = render_both(&c);
    // White at the top, black at the bottom, a mix between.
    let top = tris.pixel(30, 11)[0];
    let mid = tris.pixel(30, 20)[0];
    let bottom = tris.pixel(30, 29)[0];
    assert!(
        top > 200 && bottom < 50 && (80..180).contains(&mid),
        "{top} {mid} {bottom}"
    );
}

#[test]
fn wrong_number_of_corner_colours_is_an_error() {
    let e = cook(
        &svg("<path d=\"M0 0 H9 V9 H0 Z\" data-corners=\"#fff #000\"/>"),
        &CookOptions::default(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("data-corners"), "{e}");
}
