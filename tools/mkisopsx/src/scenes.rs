//! The `--scenes` manifest: the images of a scene disc and the scenes that
//! enter them, written as `SCENES.TBL` (`psx-scene-abi`, table version 2).
//!
//! ```text
//! # images, in image order (the index is the loaded-mask bit)
//! program PSX.EXE   -          reserve=0x80100000-0x80180000
//! program GAME.EXE  game.exe   reaches=BOOT,GAME
//! overlay TOWN.OVL  town.bin   owner=PSX.EXE load=0x80100000 class=resident
//! overlay BOSS.OVL  boss.bin   owner=PSX.EXE load=0x80140000 class=swap
//! overlay MENU.OVL  menu.ovl   owner=PSX.EXE load=0x80140000 class=volatile
//! # scenes, in id order
//! scene   BOOT      PSX.EXE    start
//! scene   MENU      PSX.EXE    entry=0x80012345
//! scene   TOWN      TOWN.OVL   entry=0x80100000 credit=2 show-credit
//! scene   GAME      GAME.EXE   start
//! ```
//!
//! An image line is `program|overlay NAME file [options]`; NAME is its root
//! file name. Image 0 must be `program PSX.EXE -`, `-` standing for `--exe`
//! (the boot exe at LBA 23). A relative file is read from the manifest's
//! directory. Program options: `core-hash=<hex>` (instead of the
//! `--core-image` one; `core-hash=0` for a program not on the core, which no
//! switch can start), `reserve=<lo>-<hi>` (the RAM its overlays load into,
//! below PAYLOAD_END), `reaches=<scene>,...` (the scenes it can switch to, for
//! its compact table copy; default every scene). Overlay options, all needed
//! but `class`'s default `swap`: `owner=<program>`, `load=<addr>` (for an
//! overlay linked with its program by psoxide-overlay.ld, the option
//! `psx-exe --overlay` prints),
//! `class=resident|swap|volatile` (`volatile`: a swap overlay over memory its
//! owner also uses, which the core reads again at every load), and optionally
//! `place=before-world` (its file ends where `WORLD.PAK` starts, LBA 1024, for an
//! overlay its owner reads right before the pack: one seek and a short hop
//! instead of two long seeks). An overlay's owner needs `reserve=` on its
//! `program` line (mkisopsx refuses with `NoReserve` otherwise).
//!
//! A scene line is `scene NAME IMAGE (start | entry=<addr>) [options]`:
//! `start` for the one scene that starts each Program (its entry is the exe's
//! PC), `entry=` for a function inside a running Program or an Overlay.
//! Options: `credit=<id>`, `show-credit`, `block=<n>` (minimum handoff block
//! version). Image lines and scene lines may interleave; each kind keeps its
//! own order. Numbers are decimal or `0x` hex.

use psx_iso::scenes::abi::{Class, Span, SCENE_FLAG_PARTNER_CREDIT};
use psx_iso::scenes::{ImageSpec, Placement, ScenePlan, SceneSpec};
use std::path::Path;

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

/// Parse `text` into a plan without a core image; `read` loads a file, given its path joined to
/// `base`.
pub fn parse_manifest(
    text: &str,
    base: &Path,
    read: &mut dyn FnMut(&Path) -> Result<Vec<u8>, String>,
) -> Result<ScenePlan, String> {
    let mut plan = ScenePlan::default();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let at = |msg: String| format!("scenes line {}: {msg}", line_index + 1);
        let mut words = line.split_whitespace();
        let (Some(kind), Some(name), Some(third)) = (words.next(), words.next(), words.next())
        else {
            return Err(at(
                "want program|overlay <NAME> <file|-> or scene <NAME> <IMAGE> ...".into(),
            ));
        };
        match kind {
            "program" | "overlay" => {
                let bytes = if third == "-" {
                    None
                } else {
                    let path = Path::new(third);
                    let path = if path.is_absolute() {
                        path.to_path_buf()
                    } else {
                        base.join(path)
                    };
                    Some(read(&path).map_err(at)?)
                };
                let image = if kind == "program" {
                    program(name, bytes, words)
                } else {
                    let bytes = bytes.ok_or_else(|| "an overlay needs a file".to_string());
                    bytes.and_then(|bytes| overlay(name, bytes, words))
                };
                plan.images.push(image.map_err(at)?);
            }
            "scene" => plan.scenes.push(scene(name, third, words).map_err(at)?),
            _ => return Err(at(format!("unknown kind {kind}"))),
        }
    }
    Ok(plan)
}

fn program<'a>(
    name: &str,
    file: Option<Vec<u8>>,
    options: impl Iterator<Item = &'a str>,
) -> Result<ImageSpec, String> {
    let mut image = ImageSpec::program(name, file);
    for option in options {
        match option.split_once('=') {
            Some(("core-hash", v)) => {
                let hex = v.strip_prefix("0x").unwrap_or(v);
                let hash =
                    u32::from_str_radix(hex, 16).map_err(|_| format!("core-hash={v}: want hex"))?;
                image.core_hash = Some(hash);
            }
            Some(("reserve", v)) => {
                let (lo, hi) = v
                    .split_once('-')
                    .ok_or_else(|| format!("reserve={v}: want <lo>-<hi>"))?;
                image.reserve = Some(Span {
                    start: number("reserve", lo)?,
                    end: number("reserve", hi)?,
                });
            }
            Some(("reaches", v)) => {
                image.reaches = Some(
                    v.split(',')
                        .filter(|s| !s.is_empty())
                        .map(Into::into)
                        .collect(),
                );
            }
            _ => return Err(format!("unknown program option {option}")),
        }
    }
    Ok(image)
}

fn overlay<'a>(
    name: &str,
    file: Vec<u8>,
    options: impl Iterator<Item = &'a str>,
) -> Result<ImageSpec, String> {
    let (mut owner, mut load, mut class) = (None, None, Class::Swap);
    let mut placement = Placement::InOrder;
    for option in options {
        match option.split_once('=') {
            Some(("owner", v)) => owner = Some(v),
            Some(("load", v)) => load = Some(number::<u32>("load", v)?),
            Some(("class", "resident")) => class = Class::ResidentSet,
            Some(("class", "swap")) => class = Class::Swap,
            Some(("class", "volatile")) => class = Class::Volatile,
            Some(("place", "before-world")) => placement = Placement::BeforeWorld,
            _ => return Err(format!("unknown overlay option {option}")),
        }
    }
    let owner = owner.ok_or("an overlay needs owner=<program>")?;
    let load = load.ok_or("an overlay needs load=<address>")?;
    let mut image = ImageSpec::overlay(name, file, owner, load, class);
    image.placement = placement;
    Ok(image)
}

fn scene<'a>(
    name: &str,
    image: &str,
    options: impl Iterator<Item = &'a str>,
) -> Result<SceneSpec, String> {
    let mut scene = SceneSpec::start(name, image);
    let (mut start, mut entry) = (false, None);
    for option in options {
        match option.split_once('=') {
            None if option == "start" => start = true,
            None if option == "show-credit" => scene.flags |= SCENE_FLAG_PARTNER_CREDIT,
            Some(("entry", v)) => entry = Some(number::<u32>("entry", v)?),
            Some(("credit", v)) => scene.partner_credit = number("credit", v)?,
            Some(("block", v)) => scene.min_block_version = number("block", v)?,
            _ => return Err(format!("unknown scene option {option}")),
        }
    }
    if start == entry.is_some() {
        return Err(format!(
            "scene {name}: want exactly one of start and entry="
        ));
    }
    scene.start = start;
    scene.entry = entry;
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_iso::scenes::abi::Kind;
    use std::path::PathBuf;

    #[test]
    fn a_manifest_reads_images_and_scenes_in_order_with_their_options() {
        let text = "# demo\n\
            program PSX.EXE - reserve=0x80100000-0x80180000 \n\
            \n\
            scene BOOT psx.exe start\n\
            program game.exe sub/game.exe reaches=BOOT,GAME core-hash=0xdeadbeef\n\
            overlay TOWN.OVL /abs/town.bin owner=PSX.EXE load=0x80100000 class=resident # trailing\n\
            scene TOWN TOWN.OVL entry=0x80100010 credit=0x10 show-credit block=2\n\
            scene GAME GAME.EXE start\n";
        let mut asked = Vec::new();
        let plan = parse_manifest(text, Path::new("/base"), &mut |path| {
            asked.push(path.to_path_buf());
            Ok(vec![1, 2, 3])
        })
        .unwrap();
        assert_eq!(
            asked,
            [
                PathBuf::from("/base/sub/game.exe"),
                PathBuf::from("/abs/town.bin")
            ]
        );
        let names: Vec<&str> = plan.images.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["PSX.EXE", "GAME.EXE", "TOWN.OVL"]);
        let boot = &plan.images[0];
        assert_eq!((boot.kind, boot.file.is_none()), (Kind::Program, true));
        assert_eq!(
            boot.reserve,
            Some(Span {
                start: 0x8010_0000,
                end: 0x8018_0000
            })
        );
        let game = &plan.images[1];
        assert_eq!(game.core_hash, Some(0xdead_beef));
        assert_eq!(game.reaches, Some(vec!["BOOT".into(), "GAME".into()]));
        let town = &plan.images[2];
        assert_eq!(town.kind, Kind::Overlay);
        assert_eq!(town.owner.as_deref(), Some("PSX.EXE"));
        assert_eq!(
            (town.load_addr, town.class),
            (0x8010_0000, Some(Class::ResidentSet))
        );

        let names: Vec<&str> = plan.scenes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["BOOT", "TOWN", "GAME"]);
        assert_eq!(plan.scenes[0].image, "PSX.EXE");
        assert!(plan.scenes[0].start && plan.scenes[0].entry.is_none());
        let scene = &plan.scenes[1];
        assert_eq!((scene.start, scene.entry), (false, Some(0x8010_0010)));
        assert_eq!(
            (scene.partner_credit, scene.flags, scene.min_block_version),
            (16, SCENE_FLAG_PARTNER_CREDIT, 2)
        );
        assert!(plan.core_image.is_none());
    }

    #[test]
    fn an_overlay_over_memory_its_owner_reuses_is_volatile() {
        let text = "program PSX.EXE - reserve=0x80100000-0x80180000\n\
            overlay MENU.OVL menu.ovl owner=PSX.EXE load=0x80100000 class=volatile\n\
            scene BOOT PSX.EXE start\n";
        let plan = parse_manifest(text, Path::new("."), &mut |_| Ok(vec![0; 4])).unwrap();
        assert_eq!(plan.images[1].class, Some(Class::Volatile));
        assert_eq!(plan.images[1].placement, Placement::InOrder);
        let text = text.replace("class=volatile", "class=volatile place=before-world");
        let plan = parse_manifest(&text, Path::new("."), &mut |_| Ok(vec![0; 4])).unwrap();
        assert_eq!(plan.images[1].placement, Placement::BeforeWorld);
    }

    #[test]
    fn a_bad_manifest_line_is_named() {
        let mut read = |_: &Path| -> Result<Vec<u8>, String> { Ok(Vec::new()) };
        for (text, want) in [
            ("program PSX.EXE", "line 1: want"),
            ("\nslot PSX.EXE -", "line 2: unknown kind slot"),
            (
                "program PSX.EXE - colour=red",
                "unknown program option colour=red",
            ),
            ("program PSX.EXE - core-hash=zz", "core-hash=zz"),
            ("program PSX.EXE - reserve=0x10", "want <lo>-<hi>"),
            ("overlay A.OVL - owner=PSX.EXE load=0", "needs a file"),
            ("overlay A.OVL a.bin load=0", "needs owner"),
            ("overlay A.OVL a.bin owner=PSX.EXE", "needs load"),
            ("overlay A.OVL a.bin owner=X load=0 class=big", "class=big"),
            ("overlay A.OVL a.bin owner=X load=0 place=end", "place=end"),
            ("scene A PSX.EXE", "exactly one of start and entry"),
            (
                "scene A PSX.EXE start entry=0",
                "exactly one of start and entry",
            ),
            ("scene A PSX.EXE start block=300", "block=300"),
            (
                "scene A PSX.EXE start return=0",
                "unknown scene option return=0",
            ),
        ] {
            let error = parse_manifest(text, Path::new("."), &mut read).unwrap_err();
            assert!(error.contains(want), "{text}: {error}");
        }
    }
}
