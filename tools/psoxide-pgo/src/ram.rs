//! Free RAM of a linked guest, read from its ld.lld link map.
//!
//! Profile-guided inlining moves `.text` by several kilobytes on a tiny
//! change of profile: one function's entry samples crossing the hot
//! call-site cutoff inlined it into two callers on hl-psx and cost 3.5 KB,
//! and the same source swung between 5.5 KB and 28 KB free across
//! hot-callsite thresholds. A game states the RAM it must keep free, and a
//! PGO build that leaves less is rebuilt with less inlining instead of
//! shipping short (see `apply --ram-floor` and the `ram` mode).
//!
//! The measure is `psoxide-size ram`'s, not a second copy of it: free RAM is the
//! bytes between `__heap_start` and `STACK_INIT - STACK_RESERVE` (the address the
//! link asserts against), and a link fits when that is at least the floor. Both
//! functions live in `psoxide_size::ram` and every decision in this tool goes
//! through them.

pub use psoxide_size::ram::{fits, free_ram};

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
    fn free_ram_is_the_gap_below_the_stack_reserve() {
        // hl-psx final-7 with the default hot-callsite threshold: 5,500 B up
        // to the end of the RAM region, 256 fewer up to the stack's reach.
        assert_eq!(free_ram(MAP), Some(5_500 - 256));
        // A game linking with a smaller reserve gets it back.
        let small = MAP.replace("STACK_RESERVE = 0x8000", "STACK_RESERVE = 25600");
        assert_eq!(free_ram(&small), Some(5_500 - 256 + 0x8000 - 25_600));
    }

    #[test]
    fn a_map_without_a_heap_start_has_no_answer() {
        assert_eq!(free_ram("801f6a84 801f6a84 0 1 __bss_end = .\n"), None);
    }

    #[test]
    fn a_link_fits_exactly_at_the_floor_and_not_a_byte_under() {
        let free = free_ram(MAP).unwrap();
        assert!(fits(free, free));
        assert!(!fits(free, free + 1));
        assert!(fits(free, free - 1));
    }
}
