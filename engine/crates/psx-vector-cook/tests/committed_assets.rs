//! The cooked `.psvg` files checked in next to their `.svg` sources must be
//! exactly what the cooker produces now, and the HUD must meet its budget and
//! carry the groups the game code looks up by name.

use std::fs;
use std::path::{Path, PathBuf};

use psx_vector::VectorAsset;
use psx_vector_cook::raster::AlignedAsset;
use psx_vector_cook::{cook, Budget, CookOptions};

fn engine_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Cook `svg` and compare it with the committed `psvg`.
fn assert_committed_is_current(svg: &str, psvg: &str, budget: Budget) {
    let source = fs::read_to_string(engine_file(svg)).expect("read svg");
    let committed = fs::read(engine_file(psvg)).expect("read committed psvg");
    let cooked = cook(
        &source,
        &CookOptions {
            budget,
            ..CookOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{svg} no longer cooks: {e}"));
    assert!(
        cooked.bytes == committed,
        "{psvg} is stale. From engine/ run:\n  cargo run -p psx-vector-cook --bin psx-svg-cook -- \
         cook {svg} -o {psvg}"
    );
}

const HUD_SVG: &str = "examples/showcase-vector/assets/hud/stance_hud.svg";
const HUD_PSVG: &str = "examples/showcase-vector/assets/hud/stance_hud.psvg";

/// The stance HUD's budget: 80 GPU triangles for the whole asset.
fn hud_budget() -> Budget {
    Budget {
        total: Some(80),
        ..Budget::default()
    }
}

#[test]
fn committed_stance_hud_is_the_current_cook() {
    assert_committed_is_current(HUD_SVG, HUD_PSVG, hud_budget());
}

#[test]
fn committed_showcase_gallery_is_the_current_cook() {
    assert_committed_is_current(
        "examples/showcase-vector/assets/gallery.svg",
        "examples/showcase-vector/assets/gallery.psvg",
        Budget::default(),
    );
}

#[test]
fn stance_hud_has_the_groups_the_game_looks_up_and_fits_its_budget() {
    let bytes = fs::read(engine_file(HUD_PSVG)).expect("read psvg");
    let aligned = AlignedAsset::new(&bytes);
    let asset = VectorAsset::parse(aligned.bytes()).expect("parse");
    let mut names = vec![
        "plate".to_string(),
        "emblem".into(),
        "emblem_core".into(),
        "rail".into(),
    ];
    for i in 0..10 {
        names.push(format!("health_{i}"));
        names.push(format!("energy_{i}"));
    }
    for name in &names {
        assert!(
            asset.group(name).is_some(),
            "stance_hud.psvg lacks group `{name}`"
        );
    }
    assert!(
        asset.triangle_count() <= 80,
        "{} triangles",
        asset.triangle_count()
    );
    // The runtime scratch the game sizes for this asset.
    assert!(asset.max_group_vertices() <= 64);
}

#[test]
fn an_over_budget_hud_fails_to_cook() {
    let source = fs::read_to_string(engine_file(HUD_SVG)).expect("read svg");
    let tight = Budget {
        total: Some(20),
        ..Budget::default()
    };
    let err = cook(
        &source,
        &CookOptions {
            budget: tight,
            ..CookOptions::default()
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
}
