//! Free RAM of a linked guest, read from its ld.lld link map.
//!
//! Profile-guided inlining moves `.text` by several kilobytes on a tiny
//! change of profile: one function's entry samples crossing the hot
//! call-site cutoff inlined it into two callers on hl-psx and cost 3.5 KB,
//! and the same source swung between 5.5 KB and 28 KB free across
//! hot-callsite thresholds. A game states the RAM it must keep free, and a
//! PGO build that leaves less is rebuilt with less inlining instead of
//! shipping short (see `apply --ram-floor` and the `ram` mode).

/// End of psoxide.ld's RAM region: 2 MiB of RAM from 0x8000_0000, less the
/// 64 KiB the BIOS keeps below `LOAD_ADDR` and the 32 KiB `STACK_RESERVE`
/// at the top. `.bss` must end below it; whatever lies between is free.
pub const RAM_END: u32 = 0x801F_8000;

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

/// Bytes between `__bss_end` and [`RAM_END`], or `None` when the map has no
/// `__bss_end` (a map that is not a psoxide.ld link). An image that overran
/// the region never links, so this is never negative.
pub fn free_ram(map: &str) -> Option<u32> {
    bss_end(map).map(|end| RAM_END.saturating_sub(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = "\
     VMA      LMA     Size Align Out     In      Symbol
       0        0        0     1 STACK_INIT = RAM_BASE + 0x001FFF00
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
        // hl-psx final-7 with the default hot-callsite threshold.
        assert_eq!(free_ram(MAP), Some(5_500));
    }

    #[test]
    fn a_map_without_bss_end_has_no_answer() {
        assert_eq!(free_ram("801f6a84 801f6a84 0 1 __heap_start = .\n"), None);
    }
}
