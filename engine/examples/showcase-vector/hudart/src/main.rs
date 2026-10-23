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
        } else if let Err(e) = std::fs::create_dir_all(&out).and_then(|()| std::fs::write(&path, text))
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
    use super::art;

    #[test]
    fn symmetric_parts_mirror_exactly() {
        assert!(art::emblem().is_mirror_symmetric());
        assert!(art::gem().is_mirror_symmetric());
    }

    #[test]
    fn every_index_has_a_palette_entry() {
        use crate::emit::cluts;
        let table = cluts();
        for (_, sprite) in art::sprites() {
            let used: Vec<u8> = sprite.px.iter().copied().filter(|&i| i != 0).collect();
            assert!(used.iter().all(|&i| i < 16));
        }
        assert!(table.iter().all(|c| c[0] == 0));
    }
}
