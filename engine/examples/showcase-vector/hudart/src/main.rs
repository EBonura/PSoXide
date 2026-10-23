//! hudart: draws the stance HUD's sprites and writes the data the guest includes.
//!
//! hudart [--out <dir>] [--check]
//!
//! Writes `stance_hud_art.rs`, `layout.txt` and `layout-size<N>.txt` (for `cellcheck --strict`) and
//! `atlas.txt` (the sprites as text) into `<dir>`, by default the HUD sprite asset directory of
//! editor-playtest. With `--check` it writes nothing and fails when the files there are not what it
//! would write.

mod art;
mod emit;
mod palette;

use std::path::PathBuf;
use std::process::ExitCode;

fn atlas_text() -> String {
    let (placed, height) = art::pack();
    let mut s = format!("atlas {} x {height}\n\n", art::ATLAS_W);
    for p in placed {
        s += &format!(
            "{} {}x{} at {},{}\n{}\n",
            p.name,
            p.sprite.w,
            p.sprite.h,
            p.pos.0,
            p.pos.1,
            p.sprite.ascii()
        );
    }
    s
}

/// Every file the generator writes: name and text.
fn files() -> Vec<(String, String)> {
    let mut v = vec![
        ("stance_hud_art.rs".to_string(), emit::art_module()),
        ("layout.txt".to_string(), emit::layout()),
        ("atlas.txt".to_string(), atlas_text()),
    ];
    for size in 0..art::SIZES {
        v.push((format!("layout-size{size}.txt"), emit::size_layout(size)));
    }
    v
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let default_out =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../editor-playtest/assets/hud/sprites");
    let out = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map_or(default_out, PathBuf::from);
    let check = args.iter().any(|a| a == "--check");
    let mut stale = false;
    for (name, text) in &files() {
        let path = out.join(name);
        if check {
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                eprintln!("{} is out of date", path.display());
                stale = true;
            }
        } else if let Err(e) =
            std::fs::create_dir_all(&out).and_then(|()| std::fs::write(&path, text))
        {
            eprintln!("{}: {e}", path.display());
            return ExitCode::from(2);
        }
    }
    if stale {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::{art, files};

    /// The committed data is what the generator writes now.
    #[test]
    fn committed_files_are_fresh() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../editor-playtest/assets/hud/sprites");
        for (name, want) in files() {
            if name == "atlas.txt" {
                continue;
            }
            let have = std::fs::read_to_string(dir.join(&name)).expect("committed file");
            assert!(
                have == want,
                "{name} is out of date: run `cargo run` in hudart"
            );
        }
    }

    /// Every sprite sits inside the atlas without overlapping another sprite.
    #[test]
    fn atlas_placements_do_not_overlap() {
        let (placed, height) = art::pack();
        let rects: Vec<(usize, usize, usize, usize)> = placed
            .iter()
            .map(|p| (p.pos.0, p.pos.1, p.sprite.w, p.sprite.h))
            .collect();
        for (i, a) in rects.iter().enumerate() {
            assert!(a.0 + a.2 <= art::ATLAS_W && a.1 + a.3 <= height);
            for b in &rects[i + 1..] {
                let apart =
                    a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1;
                assert!(apart);
            }
        }
    }

    /// Every sprite uses some pixels and every CLUT keeps entry 0 transparent.
    #[test]
    fn sprites_have_pixels_and_cluts_start_transparent() {
        let (placed, _) = art::pack();
        for p in &placed {
            assert!(p.sprite.px.iter().any(|&i| i != 0), "{} is empty", p.name);
        }
        for clut in &super::emit::cluts() {
            assert_eq!(clut[0], 0);
        }
    }

    #[test]
    fn symmetric_parts_mirror_exactly() {
        assert!(art::emblem().is_mirror_symmetric());
        assert!(art::gem().is_mirror_symmetric());
    }

    /// At every size both ends of the bar lean like the cells, one pixel per two rows, the bottom
    /// further left: the left end's rim sits one column left of its edge, and the right end's plate
    /// ends at its edge, every row.
    #[test]
    fn bar_ends_lean_like_the_cells() {
        for (n, g) in art::geos().iter().enumerate() {
            let (l, r) = (art::lcap(g), art::cap(g));
            let first =
                |s: &art::Sprite, y: usize| (0..s.w).find(|&x| s.get(x, y) != 0).expect("row");
            let last = |s: &art::Sprite, y: usize| {
                (0..s.w).rev().find(|&x| s.get(x, y) != 0).expect("row")
            };
            for y in 1..g.rows.len() - 1 {
                assert_eq!(
                    first(&l, y),
                    g.edge_col(y) - 1,
                    "size {n} left end, row {y}"
                );
                assert_eq!(last(&r, y), g.edge_col(y), "size {n} right end, row {y}");
            }
        }
    }

    /// At every size the cell is the same parallelogram in every row pair: the lean is one pixel per
    /// two rows.
    #[test]
    fn cell_leans_one_pixel_per_two_rows() {
        for (n, g) in art::geos().iter().enumerate() {
            let c = art::cell(g);
            let first = |y: usize| {
                (0..c.w)
                    .find(|&x| c.get(x, y) != 0)
                    .expect("row has pixels")
            };
            for y in 0..g.cell_h {
                assert_eq!(first(y), g.lean() - y / 2, "size {n}, row {y}");
            }
        }
    }

    /// At every size the first cell is as tight to the left end as the last cell is to the right end,
    /// in every row: two pixels (one of well and the wall) between a cell and the end edge.
    #[test]
    fn cells_are_tight_to_both_ends() {
        for (n, g) in art::geos().iter().enumerate() {
            for r in 0..g.cell_h {
                let b = g.cell_row() + r;
                let lean = g.lean() as i32 - (r as i32) / 2;
                let left_cell = g.cell_x0() + lean;
                let right_cell = g.cell_x0() + 9 * g.pitch + lean + g.body - 1;
                let left_edge = art::LCAP_X + g.edge_col(b) as i32;
                let right_edge = g.cap_x() + g.edge_col(b) as i32 - 1;
                assert_eq!(left_cell - left_edge - 1, 2, "size {n}, row {r}, left");
                assert_eq!(right_edge - right_cell - 1, 2, "size {n}, row {r}, right");
            }
        }
    }

    /// The sizes shrink: each is narrower and shorter than the one before it.
    #[test]
    fn sizes_get_smaller() {
        let g = art::geos();
        for w in g.windows(2) {
            assert!(w[1].width() < w[0].width());
            assert!(w[1].bar_h() < w[0].bar_h());
            assert!(w[1].pitch < w[0].pitch);
        }
    }
}
