//! Free RAM of a linked guest, read from its ld.lld link map.
//!
//! Profile-guided inlining moves `.text` by several kilobytes on a tiny
//! change of profile: one function's entry samples crossing the hot
//! call-site cutoff inlined it into two callers on hl-psx and cost 3.5 KB,
//! and the same source swung between 5.5 KB and 28 KB free across
//! hot-callsite thresholds. A game states the RAM it must keep free, and a
//! PGO build that leaves less is rebuilt with less inlining instead of
//! shipping short (see `apply --ram-floor` and the `ram` mode).

/// Where the heap ends in psoxide.ld as shipped: `STACK_INIT - STACK_RESERVE`,
/// 0x801f_ff00 less 32 KiB. The stack grows down from `STACK_INIT`, so this is
/// the lowest address it may reach. The script's RAM region ends 256 bytes
/// higher (2 MiB less the reserve), so the link alone would let `.bss` reach
/// into the stack's last 256 bytes. Used when a map does not say.
pub const RAM_END: u32 = 0x801f_7f00;

/// The value of a linker-script symbol assignment printed in the map
/// (`       0        0        0     1 STACK_RESERVE = 0x6400`): sums and
/// differences of hex or decimal numbers (with `K` or `M`) and symbols
/// assigned on earlier lines. `None` when it is anything else.
fn symbols(map: &str) -> std::collections::HashMap<String, u32> {
    let mut values = std::collections::HashMap::new();
    for line in map.lines() {
        let Some((name, expression)) = line.get(33..).and_then(|rest| rest.split_once(" = "))
        else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        if let Some(value) = evaluate(expression.trim(), &values) {
            values.entry(name.to_string()).or_insert(value);
        }
    }
    values
}

fn evaluate(expression: &str, values: &std::collections::HashMap<String, u32>) -> Option<u32> {
    let mut total = 0u32;
    let mut sign = 1i64;
    for token in expression.split_whitespace() {
        match token {
            "+" => sign = 1,
            "-" => sign = -1,
            _ => {
                let (digits, scale) = match token.strip_suffix('K') {
                    Some(digits) => (digits, 1024),
                    None => match token.strip_suffix('M') {
                        Some(digits) => (digits, 1024 * 1024),
                        None => (token, 1),
                    },
                };
                let value = match digits
                    .strip_prefix("0x")
                    .or_else(|| digits.strip_prefix("0X"))
                {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None if digits.starts_with(|c: char| c.is_ascii_digit()) => {
                        digits.parse().ok()?
                    }
                    None => *values.get(digits)?,
                }
                .checked_mul(scale)?;
                total = if sign > 0 {
                    total.wrapping_add(value)
                } else {
                    total.wrapping_sub(value)
                };
            }
        }
    }
    Some(total)
}

/// Where the heap ends for this link: `STACK_INIT - STACK_RESERVE` from the
/// map's own assignments, or [`RAM_END`] when the map has neither.
pub fn heap_end(map: &str) -> u32 {
    let values = symbols(map);
    match (values.get("STACK_INIT"), values.get("STACK_RESERVE")) {
        (Some(&init), Some(&reserve)) => init.wrapping_sub(reserve),
        _ => RAM_END,
    }
}

/// `__bss_end` from a link map: the address column of the line that assigns
/// it (`801f6a84 801f6a84        0     1         __bss_end = .`).
pub fn bss_end(map: &str) -> Option<u32> {
    map.lines()
        .filter(|line| line.contains("__bss_end = ."))
        .find_map(|line| {
            let address = line.split_whitespace().next()?;
            u32::from_str_radix(address, 16).ok()
        })
}

/// Bytes between `__bss_end` and [`heap_end`], or `None` when the map has no
/// `__bss_end` (a map that is not a psoxide.ld link). Zero when `.bss` runs
/// into the stack's reserve.
pub fn free_ram(map: &str) -> Option<u32> {
    let end = heap_end(map);
    bss_end(map).map(|bss| end.saturating_sub(bss))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = "\
     VMA      LMA     Size Align Out     In      Symbol
       0        0        0     1 RAM_BASE = 0x80000000
       0        0        0     1 STACK_INIT = RAM_BASE + 0x001FFF00
       0        0        0     1 STACK_RESERVE = 0x8000
801f3194 801f3194        0     1         . = ALIGN ( 4 )
801f6a84 801f6a84        0     1         __bss_end = .
801f6a84 801f6a84        0     1 __heap_start = .
801f6a84 801f6a84        0     1 __heap_end = STACK_INIT - STACK_RESERVE
";

    #[test]
    fn reads_bss_end_from_its_assignment() {
        assert_eq!(bss_end(MAP), Some(0x801f_6a84));
    }

    #[test]
    fn free_ram_is_the_gap_below_the_stack_reserve() {
        // hl-psx final-7 with the default hot-callsite threshold: 5,500 B up
        // to the end of the RAM region, 256 fewer up to the stack's reach.
        assert_eq!(free_ram(MAP), Some(5_500 - 256));
        // A game linking with a smaller reserve gets it back.
        let small = MAP.replace("STACK_RESERVE = 0x8000", "STACK_RESERVE = 25600");
        assert_eq!(free_ram(&small), Some(5_500 - 256 + 0x8000 - 25_600));
        // A map without the assignments falls back to psoxide.ld's values.
        let bare: String = MAP
            .lines()
            .filter(|line| !line.contains("STACK"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(free_ram(&bare), Some(5_500 - 256));
    }

    #[test]
    fn a_map_without_bss_end_has_no_answer() {
        assert_eq!(free_ram("801f6a84 801f6a84 0 1 __heap_start = .\n"), None);
    }
}
