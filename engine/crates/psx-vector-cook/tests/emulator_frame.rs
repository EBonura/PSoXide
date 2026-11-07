//! The host reference raster against a frame the emulator rendered from the
//! guest runtime: the `showcase-vector` example draws the cooked gallery at
//! (8, 6) on a (16, 24, 40) clear, and the golden below is the top-left
//! 176 x 114 pixels of that frame.
//!
//! The two differ only on the one-pixel edges where the host's fill rule and
//! the GPU's break ties differently on diagonals and curves, and by the GPU's
//! 15-bit colour; everything inside a shape must agree. To refresh the golden
//! after a change to the runtime or the gallery, run
//! `engine/examples/showcase-vector/run.sh <dir> --golden`.

use std::fs;
use std::path::Path;

use psx_vector::{Placement, Style, VectorAsset};
use psx_vector_cook::raster::{compare, draw_group, AlignedAsset, Image};

fn engine_file(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn host_raster_of_the_gallery_matches_the_emulator_frame() {
    let golden = Image::from_ppm(
        &fs::read(engine_file(
            "crates/psx-vector-cook/tests/data/showcase_gallery_emulator_176x114.ppm",
        ))
        .expect("golden frame"),
    )
    .expect("golden ppm");
    let bytes =
        fs::read(engine_file("examples/showcase-vector/assets/gallery.psvg")).expect("psvg");
    let aligned = AlignedAsset::new(&bytes);
    let asset = VectorAsset::parse(aligned.bytes()).expect("parse");

    let mut host = Image::new(golden.width, golden.height, [16, 24, 40]);
    for group in asset.groups() {
        draw_group(
            &mut host,
            &asset,
            group,
            &Placement::IDENTITY,
            &Style::DEFAULT,
            (8, 6),
        )
        .expect("draw");
    }
    let diff = compare(&host, &golden, 24, [0, 0, golden.width, golden.height]);
    // Edge pixels only: a few percent at most, and a small mean error overall.
    assert!(
        diff.fraction() < 0.025,
        "{} of {} pixels differ: {diff:?}",
        diff.differing,
        diff.total
    );
    assert!(diff.mean_abs < 4.0, "{diff:?}");
    // Flat interiors are exact to the GPU's 15-bit colour.
    for (x, y, want) in [
        (20, 16, [232, 74, 58]),
        (130, 16, [122, 214, 92]),
        (38, 86, [255, 210, 58]),
    ] {
        let got = golden.pixel(x, y);
        for c in 0..3 {
            assert!(
                got[c].abs_diff(want[c]) <= 8,
                "emulator pixel ({x},{y}) {got:?} vs {want:?}"
            );
        }
        let h = host.pixel(x, y);
        for c in 0..3 {
            assert!(
                h[c].abs_diff(want[c]) <= 1,
                "host pixel ({x},{y}) {h:?} vs {want:?}"
            );
        }
    }
}
