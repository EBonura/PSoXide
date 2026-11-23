//! The `--scenes` manifest: one `SCENES.TBL` row per line.
//!
//! ```text
//! # kind  NAME        file   options
//! program PSX.EXE     -                  # the --exe, the boot scene
//! program GAME.EXE    game.exe  return=0 attempts=5
//! data    LEVELS.DAT  levels.dat
//! ```
//!
//! The scene id is the row's order among the non-comment lines. `kind` is
//! `program`, `image`, `data`, `module`, `call` or `boot`; `-` as the file
//! stands for `--exe`, which must be named `PSX.EXE`. A relative file is read
//! from the manifest's directory. Options: `return=<id>`, `attempts=<n>`
//! (0 means 3), `block=<n>` (minimum handoff block version), `credit=<id>`,
//! `show-credit`, `core-hash=<hex>` (this row's core hash instead of the
//! `--core-image` one). The row format is `psx-scene-abi`'s, ABI version 1.

use psx_iso::scenes::abi::{Kind, ROW_FLAG_PARTNER_CREDIT};
use psx_iso::scenes::SceneSlot;
use std::path::Path;

fn kind(word: &str) -> Option<Kind> {
    Some(match word {
        "program" => Kind::Program,
        "image" => Kind::Image,
        "data" => Kind::Data,
        "module" => Kind::Module,
        "call" => Kind::Call,
        "boot" => Kind::Boot,
        _ => return None,
    })
}

fn number<T: TryFrom<u64>>(key: &str, value: &str) -> Result<T, String> {
    let parsed = match value.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => value.parse::<u64>(),
    };
    parsed
        .ok()
        .and_then(|n| T::try_from(n).ok())
        .ok_or_else(|| format!("{key}={value}: not a number in range"))
}

/// Parse `text`; `read` loads a slot's file, given its path joined to `base`.
pub fn parse_manifest(
    text: &str,
    base: &Path,
    read: &mut dyn FnMut(&Path) -> Result<Vec<u8>, String>,
) -> Result<Vec<SceneSlot>, String> {
    let mut slots = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |msg: String| format!("scenes line {}: {msg}", line_index + 1);
        let mut words = line.split_whitespace();
        let (Some(kind_word), Some(name), Some(file)) = (words.next(), words.next(), words.next())
        else {
            return Err(at("want <kind> <NAME> <file|-> [options]".into()));
        };
        let kind = kind(kind_word).ok_or_else(|| at(format!("unknown kind {kind_word}")))?;
        let bytes = if file == "-" {
            None
        } else {
            let path = Path::new(file);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            };
            Some(read(&path).map_err(at)?)
        };
        let mut slot = SceneSlot::new(name, kind, bytes);
        for option in words {
            match option.split_once('=') {
                None if option == "show-credit" => slot.flags |= ROW_FLAG_PARTNER_CREDIT,
                Some(("return", v)) => slot.return_scene = number("return", v).map_err(at)?,
                Some(("attempts", v)) => slot.read_attempts = number("attempts", v).map_err(at)?,
                Some(("block", v)) => slot.min_block_version = number("block", v).map_err(at)?,
                Some(("credit", v)) => slot.partner_credit = number("credit", v).map_err(at)?,
                Some(("core-hash", v)) => {
                    let hex = v.strip_prefix("0x").unwrap_or(v);
                    let hash = u32::from_str_radix(hex, 16)
                        .map_err(|_| at(format!("core-hash={v}: want hex")))?;
                    slot.core_hash = Some(hash);
                }
                _ => return Err(at(format!("unknown option {option}"))),
            }
        }
        slots.push(slot);
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_iso::scenes::abi::NO_SCENE;
    use std::path::PathBuf;

    #[test]
    fn a_manifest_reads_rows_in_order_with_their_options() {
        let text = "# demo\n\
                    program PSX.EXE - \n\
                    \n\
                    program game.exe sub/game.exe return=0 attempts=5 block=2 credit=0x10 show-credit\n\
                    data LEVELS.DAT /abs/levels.dat core-hash=0xdeadbeef # trailing\n";
        let mut asked = Vec::new();
        let slots = parse_manifest(text, Path::new("/base"), &mut |path| {
            asked.push(path.to_path_buf());
            Ok(vec![1, 2, 3])
        })
        .unwrap();
        assert_eq!(
            asked,
            [
                PathBuf::from("/base/sub/game.exe"),
                PathBuf::from("/abs/levels.dat")
            ]
        );
        assert_eq!(slots.len(), 3);
        assert_eq!(
            (slots[0].name.as_str(), slots[0].file.is_none()),
            ("PSX.EXE", true)
        );
        assert_eq!(slots[0].return_scene, NO_SCENE);
        let game = &slots[1];
        assert_eq!(game.name, "GAME.EXE");
        assert_eq!(game.kind, Kind::Program);
        assert_eq!(
            (
                game.return_scene,
                game.read_attempts,
                game.min_block_version
            ),
            (0, 5, 2)
        );
        assert_eq!(
            (game.partner_credit, game.flags),
            (16, ROW_FLAG_PARTNER_CREDIT)
        );
        assert_eq!(slots[2].kind, Kind::Data);
        assert_eq!(slots[2].core_hash, Some(0xdead_beef));
    }

    #[test]
    fn a_bad_manifest_line_is_named() {
        let mut read = |_: &Path| -> Result<Vec<u8>, String> { Ok(Vec::new()) };
        for (text, want) in [
            ("program PSX.EXE", "line 1: want"),
            ("\nscene PSX.EXE -", "line 2: unknown kind scene"),
            ("program PSX.EXE - colour=red", "unknown option colour=red"),
            ("program PSX.EXE - attempts=300", "attempts=300"),
            ("program PSX.EXE - core-hash=zz", "core-hash=zz"),
        ] {
            let error = parse_manifest(text, Path::new("."), &mut read).unwrap_err();
            assert!(error.contains(want), "{text}: {error}");
        }
    }
}
