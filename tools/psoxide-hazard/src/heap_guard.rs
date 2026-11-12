//! `heap-guard MAP GENERATED_DIR`: proves the guest's heap holds what the cooked level needs.
//!
//! The editor Play guest links everything else first and gives the heap what is left of the RAM
//! (`__heap_start..__heap_end` in the link map). The cook writes what the level needs of it in
//! `heap_budget.txt` (`psxed_project::playtest::heap_budget`); a heap below that need plus
//! [`MARGIN_BYTES`] fails here, at build time, with the numbers, instead of at runtime as a failed
//! allocation and a halted guest. A directory with no budget file, or one that says `need=0`, passes.

use std::io::Write;

/// File the cook writes next to the manifest.
pub const BUDGET_FILE: &str = "heap_budget.txt";
/// Bytes the heap keeps beyond the cooked need: the free-list fragmentation a long walk leaves,
/// measured as the most any shipped level used over the cook's figure.
pub const MARGIN_BYTES: u64 = 4096;

/// `__heap_end` in `sdk/psoxide.ld`: `STACK_INIT - STACK_RESERVE`. The link map prints a symbol
/// assigned from an expression with the location counter's value, so the end is not read from it;
/// a test keeps these in step with the script.
pub const HEAP_END: u64 = 0x8000_0000 + 0x001f_ff00 - 0x8000;

fn hex(field: &str) -> Option<u64> {
    u64::from_str_radix(field, 16).ok()
}

/// `(heap_start, heap_end)`: the start from the link map, the end [`HEAP_END`].
#[must_use]
pub fn heap_range(map: &str) -> Option<(u64, u64)> {
    let start = map.lines().find_map(|line| {
        let address = hex(line.split_whitespace().next()?)?;
        line.contains(" __heap_start ").then_some(address)
    })?;
    Some((start, HEAP_END))
}

/// Size of the largest input section whose symbol ends in `suffix`.
fn symbol_size(map: &str, suffix: &str) -> Option<u64> {
    map.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.len() == 5 && fields[4].ends_with(suffix) && !line.contains(":("))
            .then(|| hex(fields[2]))
            .flatten()
    })
}

fn value(budget: &str, key: &str) -> Option<u64> {
    budget.lines().find_map(|line| {
        line.strip_prefix(key)?
            .strip_prefix('=')?
            .trim()
            .parse()
            .ok()
    })
}

/// Address the guest's image starts at (`LOAD_ADDR` in `sdk/psoxide.ld`): the heap starts at this
/// plus the linked image (code, data, bss and the room pool).
pub const IMAGE_BASE: u64 = 0x8001_0000;

/// Where the heap ends, as an image size: the bytes of RAM the image and the heap share.
pub const RAM_BYTES: u64 = HEAP_END - IMAGE_BASE;

/// Check `map` against `budget`; the message on a failure.
///
/// # Errors
/// The message naming the level, the bytes short and what uses the RAM when the heap is short.
pub fn check(map: &str, budget: &str) -> Result<String, String> {
    if value(budget, "need").unwrap_or(0) == 0 {
        return Ok("heap guard: no cooked grid level, nothing to check".into());
    }
    let (start, _) =
        heap_range(map).ok_or("heap guard: the link map has no __heap_start/__heap_end")?;
    verdict(budget, start, Some(map), "")
}

/// The guard's verdict for a heap that starts at `heap_start` (the end is [`HEAP_END`]): the one
/// computation the link-time guard and the cook's own check share, so a level the cook passes is
/// a level the guard passes whenever `heap_start` is the link's. `map` adds what uses the RAM to
/// the refusal when the link is at hand; `source` is appended to the refusal's first words (the
/// cook says its figure is an estimate).
///
/// # Errors
/// The message naming the level, the bytes short and what uses the RAM when the heap is short.
pub fn verdict(
    budget: &str,
    heap_start: u64,
    map: Option<&str>,
    source: &str,
) -> Result<String, String> {
    let need = value(budget, "need").unwrap_or(0);
    if need == 0 {
        return Ok("heap guard: no cooked grid level, nothing to check".into());
    }
    let (start, end) = (heap_start, HEAP_END);
    let heap = end.saturating_sub(start);
    let required = need + MARGIN_BYTES;
    let level = budget
        .lines()
        .find_map(|line| line.strip_prefix("level="))
        .unwrap_or("?");
    // The cook writes its estimate of the linked image (pool included); with the link at hand the
    // difference is printed, so a drifting model is seen on the next build.
    let drift = match (map, value(budget, "image_estimate")) {
        (Some(_), Some(estimate)) => format!(
            "; the cook estimated an image of {estimate} B, the link has {} B ({:+} B)",
            start.saturating_sub(IMAGE_BASE),
            start.saturating_sub(IMAGE_BASE) as i64 - estimate as i64
        ),
        _ => String::new(),
    };
    if heap >= required {
        return Ok(format!(
            "heap guard{source}: level '{level}' needs {need} B (+{MARGIN_BYTES} margin), the heap is {heap} B: {} B to spare{drift}",
            heap - required
        ));
    }
    let pool = value(budget, "pool").unwrap_or(0);
    let arenas = map
        .and_then(|m| symbol_size(m, "RUNTIME_ARENAS"))
        .unwrap_or(0);
    let scene = map.and_then(|m| symbol_size(m, "SCENE")).unwrap_or(0);
    let image = start.saturating_sub(IMAGE_BASE);
    Err(format!(
        "heap guard{source}: level '{level}' will run out of heap: it needs {need} B (renderer scratch {}, resident table {}, fixed {}) plus a {MARGIN_BYTES} B margin, \
         and the link leaves {heap} B, short by {} B. The RAM goes to: room pool {pool} B ({} rooms), runtime arenas {arenas} B, \
         scene {scene} B; the whole linked image (code, data and bss) is {} B{drift}. \
         Fewer or smaller rooms shrink the pool, a level that cannot hold all its rooms at once should stream by flood, and the largest room sets the renderer scratch.",
        value(budget, "renderer").unwrap_or(0),
        value(budget, "resident_table").unwrap_or(0),
        value(budget, "fixed").unwrap_or(0),
        required - heap,
        value(budget, "rooms").unwrap_or(0),
        image,
    ))
}

/// Run the guard with command-line `args` (no program name); returns the exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let [map_path, dir] = args else {
        let _ = writeln!(out, "usage: heap-guard MAP GENERATED_DIR");
        return 2;
    };
    let Ok(map) = std::fs::read_to_string(map_path) else {
        let _ = writeln!(out, "heap guard: cannot read {map_path}");
        return 2;
    };
    let budget =
        std::fs::read_to_string(std::path::Path::new(dir).join(BUDGET_FILE)).unwrap_or_default();
    match check(&map, &budget) {
        Ok(message) => {
            let _ = writeln!(out, "{message}");
            0
        }
        Err(message) => {
            let _ = writeln!(out, "{message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = "801ecbb0 801ecbb0        0     1 __heap_start = .\n\
                       801ecbb0 801ecbb0        0     1 __heap_end = STACK_INIT - STACK_RESERVE\n";

    #[test]
    fn the_heap_end_matches_the_linker_script() {
        let script: String = include_str!("../../../sdk/psoxide.ld")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(script.contains("STACK_INIT = RAM_BASE + 0x001FFF00;"));
        assert!(script.contains("STACK_RESERVE = 0x8000;"));
        assert!(script.contains("__heap_end = STACK_INIT - STACK_RESERVE;"));
    }

    #[test]
    fn a_map_without_the_symbols_has_no_range() {
        assert_eq!(heap_range("nothing"), None);
    }

    #[test]
    fn no_budget_passes() {
        assert!(check(MAP, "").is_ok());
        assert!(check(MAP, "need=0\n").is_ok());
    }

    #[test]
    fn a_short_heap_fails_with_the_numbers() {
        let map = MAP.replace(
            "801ecbb0 801ecbb0        0     1 __heap_start",
            "801f7000 801f7000        0     1 __heap_start",
        );
        let budget =
            "level=T\nneed=50000\nrenderer=1\nresident_table=2\nfixed=3\npool=400\nrooms=7\n";
        let error = check(&map, budget).unwrap_err();
        assert!(
            error.contains("level 'T'") && error.contains("short by"),
            "{error}"
        );
        let ok = check(MAP, "need=1000\n").unwrap();
        assert!(ok.contains("to spare"), "{ok}");
    }
}
