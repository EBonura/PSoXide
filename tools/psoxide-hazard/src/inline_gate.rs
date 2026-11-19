//! `inline-gate`: refuse a linked image that holds a frame-path function as a function.
//!
//! The compiler decides whether a function without `inline(always)` is inlined, and it decides by the
//! size of the program as well as the size of the function: the SDK's examples are small enough that it
//! inlines nearly everything, a game-sized program is not. `psx_gpu::chain::start_walk`, the kick behind
//! every chain submit, was compiled as a function in one game and cost it about a hundred cycles of
//! call, return, load stall and instruction-cache refill on every submit, which the examples and every
//! gate passed. `sdk/profile.toml` lists, under `[inline]`, the free functions of the per-frame path that
//! must be inlined into their callers (`crate::module::function`); this gate reads the link map and fails
//! when any of them is linked as a function (a `.text.<symbol>` input section or a symbol with a size).
//! A function that is not in the image at all passes: a program that never calls it does not link it.
//!
//! The map names functions by their mangled symbol, so a listed path is turned into the run of
//! length-prefixed names the mangling writes (`psx_gpu::chain::start_walk` is `7psx_gpu5chain10start_walk`)
//! and looked for in the symbol. That finds the function under the legacy and the v0 scheme, generic
//! instances included, and cannot match a function whose name only starts alike (the length prefix is
//! part of the pattern). Methods (`Type::method`) are not matched: their path is not one run.
//!
//! ```text
//! inline-gate path/to/game.exe --map path/to/game.map [--profile path/to/profile.toml]
//! ```
//! Without `--profile` the list is the `sdk/profile.toml` of the SDK this tool was built from.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use psx_gate::profile::Profile;

/// The command line, for a usage error.
pub const USAGE: &str = "usage: inline-gate <exe> --map <link.map> [--profile <profile.toml>]\n";

/// The run of length-prefixed names `path` (`crate::module::function`) has in a mangled symbol, or
/// `None` for a path of fewer than two names (a bare name would match anything alike).
#[must_use]
pub fn mangled_fragment(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split("::").collect();
    if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(
        parts
            .iter()
            .map(|part| format!("{}{part}", part.len()))
            .collect(),
    )
}

/// The symbols of `symbols` that `map` links as a function, as `(listed path, linked symbol)`, once
/// each and in listed order.
#[must_use]
pub fn out_of_line(map: &str, symbols: &[String]) -> Vec<(String, String)> {
    let mut linked: BTreeSet<&str> = BTreeSet::new();
    for line in map.lines() {
        let mut fields = line.split_whitespace();
        // `VMA LMA Size Align Out In Symbol`: an input section (`file:(.text.name)`) or a symbol.
        let (Some(_), Some(_), Some(size)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if usize::from_str_radix(size, 16).unwrap_or(0) == 0 {
            continue;
        }
        let Some(last) = line.split_whitespace().last() else {
            continue;
        };
        let name = match last.rfind("(.text.") {
            Some(at) => last[at + "(.text.".len()..].trim_end_matches(')'),
            None => last,
        };
        linked.insert(name);
    }
    let mut hits = Vec::new();
    for path in symbols {
        let Some(fragment) = mangled_fragment(path) else {
            continue;
        };
        if let Some(name) = linked.iter().find(|name| name.contains(&fragment)) {
            hits.push((path.clone(), (*name).to_string()));
        }
    }
    hits
}

/// Run the gate with command-line `args` (no program name); returns the exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let (mut map_path, mut profile_path) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--map" => map_path = it.next().cloned(),
            "--profile" => profile_path = it.next().cloned(),
            _ => {}
        }
    }
    let Some(map_path) = map_path else {
        let _ = write!(out, "{USAGE}");
        return 2;
    };
    let default = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/profile.toml");
    let profile_path = profile_path.map_or(default, Into::into);
    let profile = match Profile::read(&profile_path) {
        Ok(profile) => profile,
        Err(error) => {
            let _ = writeln!(out, "inline-gate: {error}");
            return 1;
        }
    };
    let map = match std::fs::read_to_string(&map_path) {
        Ok(map) => map,
        Err(error) => {
            let _ = writeln!(
                out,
                "{}",
                crate::linkmap::io_message(&error, Path::new(&map_path))
            );
            return 1;
        }
    };
    let hits = out_of_line(&map, &profile.inline);
    if hits.is_empty() {
        let _ = writeln!(out, "inline-gate: PASS ({map_path})");
        return 0;
    }
    let _ = writeln!(
        out,
        "inline-gate: FAIL: frame-path functions linked as functions:"
    );
    for (path, linked) in &hits {
        let _ = writeln!(out, "  {path} is a function in the image ({linked})");
    }
    let _ = writeln!(
        out,
        "  sdk/profile.toml [inline] lists them as inlined into their callers; \
         sdk/docs/GATE.md, \"Inlined frame-path functions\""
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    const START_WALK: &str = "_RNvNtCsbh2AbiOuVML_7psx_gpu5chain10start_walk";

    fn map(sections: &[&str]) -> String {
        let mut text = String::from("     VMA      LMA     Size Align Out     In      Symbol\n");
        for name in sections {
            text.push_str(&format!(
                "80011a64 80011a64       4c     4         /b/deps/game.o:(.text.{name})\n"
            ));
            text.push_str(&format!(
                "80011a64 80011a64       4c     1                 {name}\n"
            ));
        }
        text
    }

    fn listed() -> Vec<String> {
        vec!["psx_gpu::chain::start_walk".to_string()]
    }

    #[test]
    fn the_fragment_is_the_length_prefixed_path() {
        assert_eq!(
            mangled_fragment("psx_gpu::chain::start_walk").as_deref(),
            Some("7psx_gpu5chain10start_walk")
        );
        assert_eq!(mangled_fragment("start_walk"), None);
        assert_eq!(mangled_fragment("a::"), None);
    }

    #[test]
    fn an_image_with_the_function_out_of_line_is_refused() {
        let hits = out_of_line(&map(&[START_WALK]), &listed());
        assert_eq!(
            hits,
            [(
                "psx_gpu::chain::start_walk".to_string(),
                START_WALK.to_string()
            )]
        );
        // The legacy scheme and a generic instance are found too.
        let legacy = "_ZN7psx_gpu5chain10start_walk17h0123456789abcdefE";
        assert_eq!(out_of_line(&map(&[legacy]), &listed()).len(), 1);
    }

    #[test]
    fn an_image_without_it_passes() {
        let other = "_RNvNtCsbh2AbiOuVML_7psx_gpu5chain9wait_walk";
        assert!(out_of_line(&map(&[other, "main"]), &listed()).is_empty());
        // A name that only starts alike is not it, and a zero-size symbol is not a function.
        let longer = "_RNvNtCsbh2AbiOuVML_7psx_gpu5chain15start_walk_fast";
        assert!(out_of_line(&map(&[longer]), &listed()).is_empty());
        let empty = format!("80011a64 80011a64        0     1                 {START_WALK}\n");
        assert!(out_of_line(&empty, &listed()).is_empty());
    }

    #[test]
    fn main_reports_and_exits_by_the_map() {
        let dir = std::env::temp_dir().join(format!("inline-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let profile = dir.join("profile.toml");
        std::fs::write(
            &profile,
            "[inline]\nsymbols = [\"psx_gpu::chain::start_walk\"]\n",
        )
        .unwrap();
        let run = |text: String| {
            let path = dir.join("game.map");
            std::fs::write(&path, text).unwrap();
            let args: Vec<String> = [
                "game.exe",
                "--map",
                path.to_str().unwrap(),
                "--profile",
                profile.to_str().unwrap(),
            ]
            .map(String::from)
            .to_vec();
            let mut out = Vec::new();
            let status = main(&args, &mut out);
            (status, String::from_utf8(out).unwrap())
        };
        let (status, text) = run(map(&[START_WALK]));
        assert_eq!(status, 1);
        assert!(
            text.contains("FAIL") && text.contains("psx_gpu::chain::start_walk"),
            "{text}"
        );
        let (status, text) = run(map(&["main"]));
        assert_eq!(status, 0, "{text}");
        assert_eq!(main(&[], &mut Vec::new()), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
