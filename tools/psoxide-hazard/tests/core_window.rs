//! `.text` and `--code` inside the scene core's window are code; nothing else
//! the old bound refused is.

mod common;

use common::TempDir;
use psoxide_hazard::{code_ranges, plausible_code, text_bounds};
use psx_scene_abi::{CORE_WINDOW, HANDOFF_BLOCK};

const WINDOW_LO: i64 = CORE_WINDOW.start as i64;
const WINDOW_HI: i64 = CORE_WINDOW.end as i64;
// The tests take the window from the one constant; this is what they assume of it.
const _: () = assert!(WINDOW_LO - 0x100 >= 0x801f_8000);

/// A psoxide.ld-shaped map whose `.text` is `lo..hi`.
fn map_with_text(dir: &TempDir, lo: i64, hi: i64) -> String {
    let row = |address: i64, name: &str| {
        format!("{address:8x} {address:8x}        0     4         {name}\n")
    };
    let mut map = String::from("     VMA      LMA     Size Align Out     In      Symbol\n");
    map += &row(lo, "__text_start = .");
    map += &row(hi, "__text_end = .");
    let path = dir.0.join("core.map");
    std::fs::write(&path, map).unwrap();
    path.to_string_lossy().into_owned()
}

/// What `text_bounds` returns.
type Bounds = Result<Option<Vec<(i64, i64)>>, Option<i32>>;

fn bounds(lo: i64, hi: i64) -> (Bounds, String) {
    let dir = TempDir::new("core-window");
    let args = vec!["--map".to_string(), map_with_text(&dir, lo, hi)];
    let mut out = Vec::new();
    let result = text_bounds(&args, &mut out);
    (result, String::from_utf8(out).unwrap())
}

#[test]
fn the_window_sits_between_the_program_bound_and_the_handoff_block() {
    assert_eq!(CORE_WINDOW.end, HANDOFF_BLOCK.start);
}

#[test]
fn text_inside_the_core_window_is_accepted() {
    // The whole window, and a core's code at its start.
    for (lo, hi) in [(WINDOW_LO, WINDOW_HI), (WINDOW_LO, WINDOW_LO + 0x1800)] {
        let (result, out) = bounds(lo, hi);
        assert_eq!(result, Ok(Some(vec![(lo, hi)])), "{lo:x}..{hi:x}: {out}");
    }
    // The program range is unchanged.
    let (result, _) = bounds(0x8001_0000, 0x801f_8000);
    assert_eq!(result, Ok(Some(vec![(0x8001_0000, 0x801f_8000)])));
}

#[test]
fn text_outside_both_ranges_is_still_refused() {
    for (lo, hi) in [
        // Between the program bound and the window: refused before, refused now.
        (0x801f_8000, 0x801f_8100),
        (WINDOW_LO - 0x100, WINDOW_LO),
        // Straddling into the window from below, or out of it into the block.
        (WINDOW_LO - 0x100, WINDOW_LO + 0x100),
        (WINDOW_LO, WINDOW_HI + 4),
        // The handoff block, the kernel's 64 KiB, empty and inverted ranges.
        (HANDOFF_BLOCK.start as i64, HANDOFF_BLOCK.end as i64),
        (0x8000_f000, 0x8001_0100),
        (WINDOW_LO, WINDOW_LO),
        (WINDOW_LO + 0x100, WINDOW_LO),
    ] {
        let (result, out) = bounds(lo, hi);
        assert_eq!(result, Err(Some(1)), "{lo:x}..{hi:x}");
        assert!(out.contains("implausible .text bounds"), "{out}");
    }
}

#[test]
fn code_ranges_inside_the_core_window_are_accepted() {
    let args = |lo: i64, hi: i64| vec!["--code".to_string(), format!("{lo:x}..{hi:x}")];
    assert_eq!(
        code_ranges(&args(WINDOW_LO, WINDOW_LO + 0x200)),
        Ok(vec![(WINDOW_LO, WINDOW_LO + 0x200)])
    );
    for (lo, hi) in [
        (0x801f_8000, 0x801f_8100),
        (WINDOW_LO - 0x100, WINDOW_LO + 0x100),
        (WINDOW_HI - 0x100, WINDOW_HI + 4),
        (HANDOFF_BLOCK.start as i64, HANDOFF_BLOCK.end as i64),
    ] {
        let bad = format!("{lo:x}..{hi:x}");
        let error = code_ranges(&args(lo, hi)).unwrap_err();
        assert!(error.contains("bad --code range"), "{bad}: {error}");
    }
}

#[test]
fn plausible_code_is_the_union_of_the_two_ranges() {
    assert!(plausible_code(0x8001_0000, 0x8001_0004));
    assert!(plausible_code(0x801f_7ffc, 0x801f_8000));
    assert!(plausible_code(WINDOW_HI - 4, WINDOW_HI));
    assert!(!plausible_code(0x801f_7ffc, 0x801f_8004));
    assert!(!plausible_code(0x801f_8000, WINDOW_HI));
    assert!(!plausible_code(0x8001_0000, WINDOW_HI));
}
