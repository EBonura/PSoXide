//! Post-link checks for PS1 guest executables, run after every link.
//!
//! * [`patch`] (`hazard-patch`) reroutes every R3000 load-delay hazard a
//!   branch delay slot leaves through psx-rt's `HAZARD_TRAMPOLINES`, without
//!   moving any code, and rescans.
//! * [`scan`] (`hazard-scan`) proves an image free of those hazards,
//!   whatever built it.
//! * [`stack_guard`] (`stack-guard`) proves every scratchpad stack call tree
//!   fits its region.
//!
//! These replace `tools/hazard_patch.py`, `hazard_scan.py` and
//! `stack_guard.py`, with the same command lines, reports and patched bytes,
//! and need no objdump: [`psx_disasm`] decodes the image as objdump did.
//! [`detect`] is the one hazard detector all three read.

use std::io::Write;

pub mod detect;
pub mod linkmap;
pub mod listing;
pub mod patch;
pub mod scan;
pub mod stack_guard;
pub mod text;

use detect::Unlisted;
use linkmap::LinkMap;

/// The patcher's and scanner's command line: `(paths, --check given,
/// --map path)`, or `None` when `--map` has no value. Other `--` flags are
/// ignored.
pub fn cli_args(args: &[String]) -> Option<(Vec<String>, bool, Option<String>)> {
    let (mut paths, mut check_only, mut map_path) = (Vec::new(), false, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--check" {
            check_only = true;
        } else if arg == "--map" {
            map_path = Some(it.next()?.clone());
        } else if arg.starts_with("--") {
            continue;
        } else {
            paths.push(arg.clone());
        }
    }
    Some((paths, check_only, map_path))
}

/// The link map for `data`, or `None` without one. A map that cannot be
/// read or comes from another link is reported to `out` and gives the exit
/// status 1.
pub fn open_map(
    map_path: Option<&str>,
    data: &[u8],
    out: &mut dyn Write,
) -> Result<Option<LinkMap>, i32> {
    let Some(map_path) = map_path else {
        return Ok(None);
    };
    match LinkMap::open(std::path::Path::new(map_path))
        .and_then(|map| map.check(data).map(|()| map))
    {
        Ok(map) => Ok(Some(map)),
        Err(error) => {
            let _ = writeln!(out, "{error}");
            Err(1)
        }
    }
}

/// True when `--whole-image` is given: the caller accepts the heuristic
/// data guard over every word of the load (see [`detect::looks_like_code`]).
pub fn whole_image(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--whole-image")
}

/// The executable bounds the tools work within. With `--map` they are the
/// map's `.text` (`__text_start..__text_end`, the only executable section
/// of `psoxide.ld`), whether or not `--text-only` is also given: the rest of
/// the load is `.data`, `.rodata` and assets, and words there decode as
/// plausible instructions (a slice length of 8 is `jr zero`, 0x11111111 is
/// `beq t0,s1`), so nothing outside `.text` may be read as code or written.
/// `Ok(None)` means no bounds: no `--map`, or `--whole-image`, the explicit
/// request for the old heuristic over the whole load. `Err(None)` is a usage
/// error (`--text-only` without `--map`); `Err(Some(status))` means the map
/// could not be read or its bounds are not in RAM, already reported to
/// `out`.
pub fn text_bounds(
    args: &[String],
    out: &mut dyn Write,
) -> Result<Option<(i64, i64)>, Option<i32>> {
    let text_only = args.iter().any(|arg| arg == "--text-only");
    let Some((_, _, map_path)) = cli_args(args) else {
        return Err(None);
    };
    if whole_image(args) {
        return Ok(None);
    }
    let Some(map_path) = map_path else {
        return if text_only { Err(None) } else { Ok(None) };
    };
    let map = LinkMap::open(std::path::Path::new(&map_path)).map_err(|error| {
        let _ = writeln!(out, "{error}");
        Some(1)
    })?;
    let (lo, hi) = map.text;
    if !(0x8001_0000 <= lo && lo < hi && hi <= 0x801F_8000) {
        let _ = writeln!(out, "implausible .text bounds {lo:#x}..{hi:#x}");
        return Err(Some(1));
    }
    Ok(Some((lo, hi)))
}

/// Report a jump table entry that lands on a word the listing does not
/// have; returns the exit status.
pub fn report_unlisted(missing: &Unlisted, out: &mut dyn Write) -> i32 {
    let _ = writeln!(
        out,
        "error: a jump table entry lands on {:08x}, which does not disassemble",
        missing.0
    );
    1
}

/// Run `main` with the process arguments and exit with its status.
pub fn run_cli(main: fn(&[String], &mut dyn Write) -> i32) -> ! {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let status = main(&args, &mut out);
    let _ = out.flush();
    std::process::exit(status)
}
