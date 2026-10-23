//! hudart: draws the stance HUD's sprites and writes the data the guest includes.
//!
//! hudart [--out <dir>] [--check]
//!
//! Writes `stance_hud_art.rs`, `layout.txt` (for `cellcheck --strict`) and `atlas.txt` (the sprites
//! as text) into `<dir>`, by default the HUD sprite asset directory of editor-playtest. With
//! `--check` it writes nothing and fails when the files there are not what it would write.

mod art;
mod emit;
mod palette;

use std::path::PathBuf;
use std::process::ExitCode;

fn atlas_text() -> String {
    let mut s = String::new();
    for (name, sprite) in art::sprites() {
        s += &format!("{name} {}x{}\n{}\n", sprite.w, sprite.h, sprite.ascii());
    }
    s
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
    let files = [
        ("stance_hud_art.rs", emit::art_module()),
        ("layout.txt", emit::layout()),
        ("atlas.txt", atlas_text()),
    ];
    let mut stale = false;
    for (name, text) in &files {
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
    use super::{art, emit};

    /// The committed data is what the generator writes now.
    #[test]
    fn committed_files_are_fresh() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../editor-playtest/assets/hud/sprites");
        for (name, want) in [
            ("stance_hud_art.rs", emit::art_module()),
            ("layout.txt", emit::layout()),
        ] {
            let have = std::fs::read_to_string(dir.join(name)).expect("committed file");
            assert!(
                have == want,
                "{name} is out of date: run `cargo run` in hudart"
            );
        }
    }

    /// Every sprite pixel sits inside the atlas without overlapping another sprite.
    #[test]
    fn atlas_placements_do_not_overlap() {
        let rects: Vec<(usize, usize, usize, usize)> = art::sprites()
            .iter()
            .zip(art::ATLAS_POS.iter())
            .map(|((_, s), (_, (x, y)))| (*x, *y, s.w, s.h))
            .collect();
        for (i, a) in rects.iter().enumerate() {
            assert!(a.0 + a.2 <= art::ATLAS_W && a.1 + a.3 <= art::ATLAS_H);
            for b in &rects[i + 1..] {
                let apart =
                    a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1;
                assert!(apart);
            }
        }
    }

    /// A CLUT never makes an opaque pixel transparent, and every index a sprite uses has a colour.
    #[test]
    fn used_indices_have_colours_in_every_clut() {
        let table = emit::cluts();
        for (_, sprite) in art::sprites() {
            let mut used: Vec<u8> = sprite.px.iter().copied().filter(|&i| i != 0).collect();
            used.sort_unstable();
            used.dedup();
            assert!(!used.is_empty());
        }
        for clut in &table {
            assert_eq!(clut[0], 0);
        }
    }

    #[test]
    fn symmetric_parts_mirror_exactly() {
        assert!(art::emblem().is_mirror_symmetric());
        assert!(art::gem().is_mirror_symmetric());
    }

    /// The cell is the same parallelogram in every row pair: the lean is one pixel per two rows.
    #[test]
    fn cell_leans_one_pixel_per_two_rows() {
        let c = art::cell();
        let first = |y: usize| {
            (0..c.w)
                .find(|&x| c.get(x, y) != 0)
                .expect("row has pixels")
        };
        for y in 0..8 {
            assert_eq!(first(y), 3 - y / 2);
        }
    }
}
