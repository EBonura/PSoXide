//! Choosing the diagonal of a floor or ceiling from its corner heights.
//!
//! A quad with four unequal corners is two triangles, and which diagonal cuts
//! it changes the shape: the surface folds along it. The rule is integer,
//! deterministic and independent of any drawing order: **the diagonal whose two
//! end corners are closest in height wins**, so the fold runs along a contour
//! and the surface reads as a flat step or a level crease with the odd corner
//! popping out of it, the way terrain looks. On a tie the current split stays,
//! so a flat or symmetric cell never flips and a stroke does not flicker.
//!
//! The design (`docs/grid/level-tools-design.md`, 4.6) asked for the diagonal
//! that fits the bilinear surface of the four corners best, measured over the
//! quarter lattice. That measure is identical for both diagonals for every
//! input (checked over thousands of random cells and kept as a test): the
//! triangulated surface differs from the bilinear one by the same amount along
//! either fold, so it can never decide. The level-diagonal rule replaces it.
//!
//! Only surfaces with [`GridSurface::auto_split`] are touched, planar
//! surfaces keep their split (no visible difference) and split-level surfaces
//! (`heights_b`) are never re-split.

use super::surface::{GridSplit, GridSurface};

/// The diagonal for corner heights `h` (`[NW, NE, SE, SW]`): the one whose
/// ends are closest in height; `current` on a tie.
pub fn best_split(h: [i32; 4], current: GridSplit) -> GridSplit {
    let nw_se = (i64::from(h[0]) - i64::from(h[2])).abs();
    let ne_sw = (i64::from(h[1]) - i64::from(h[3])).abs();
    match nw_se.cmp(&ne_sw) {
        core::cmp::Ordering::Less => GridSplit::NwSe,
        core::cmp::Ordering::Greater => GridSplit::NeSw,
        core::cmp::Ordering::Equal => current,
    }
}

/// Re-split `surface` when it follows its heights. Returns whether the
/// diagonal changed.
pub fn resplit(surface: &mut GridSurface) -> bool {
    if !surface.auto_split || surface.heights_b.is_some() || surface.is_planar() {
        return false;
    }
    let split = best_split(surface.heights, surface.split);
    let changed = split != surface.split;
    surface.split = split;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(h: [i32; 4], split: GridSplit) -> GridSurface {
        let mut s = GridSurface::flat_auto(0, None);
        s.heights = h;
        s.split = split;
        s
    }

    /// A tiny deterministic generator, no dependency.
    fn lcg(state: &mut u64) -> i32 {
        *state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((*state >> 33) % 33) as i32 * 64 - 1024
    }

    fn random_corners(state: &mut u64) -> [i32; 4] {
        [lcg(state), lcg(state), lcg(state), lcg(state)]
    }

    #[test]
    fn the_chosen_diagonal_is_the_level_one_and_ties_keep() {
        let mut state = 7u64;
        for _ in 0..3000 {
            let h = random_corners(&mut state);
            let ends = |s: GridSplit| match s {
                GridSplit::NwSe => (h[0] - h[2]).abs(),
                GridSplit::NeSw => (h[1] - h[3]).abs(),
            };
            for current in [GridSplit::NwSe, GridSplit::NeSw] {
                let chosen = best_split(h, current);
                assert!(ends(chosen) <= ends(chosen.flipped()), "{h:?}");
                if ends(chosen) == ends(chosen.flipped()) {
                    assert_eq!(chosen, current, "a tie keeps the split: {h:?}");
                }
            }
        }
    }

    #[test]
    fn the_result_does_not_depend_on_where_it_starts_unless_tied() {
        let mut state = 99u64;
        for _ in 0..3000 {
            let h = random_corners(&mut state);
            let tied = (h[0] - h[2]).abs() == (h[1] - h[3]).abs();
            if !tied {
                assert_eq!(
                    best_split(h, GridSplit::NwSe),
                    best_split(h, GridSplit::NeSw),
                    "{h:?}"
                );
            }
        }
    }

    #[test]
    fn a_raised_corner_pops_out_of_a_level_crease() {
        // SE raised: the crease NE-SW stays level, the corner rises out of it.
        assert_eq!(best_split([0, 0, 256, 0], GridSplit::NwSe), GridSplit::NeSw);
        // NE raised: the crease NW-SE stays level.
        assert_eq!(best_split([0, 256, 0, 0], GridSplit::NeSw), GridSplit::NwSe);
    }

    #[test]
    fn flat_and_symmetric_cells_never_flip() {
        for h in [[0; 4], [256; 4], [0, 256, 0, 256], [256, 0, 256, 0]] {
            for current in [GridSplit::NwSe, GridSplit::NeSw] {
                assert_eq!(best_split(h, current), current, "{h:?}");
            }
        }
    }

    /// Why the design's rule was dropped: the whole-lattice error against the
    /// bilinear surface is the same for both diagonals, so it cannot choose.
    #[test]
    fn the_bilinear_fit_error_is_equal_for_both_diagonals() {
        const STEPS: i32 = 4;
        fn tri(h: [i32; 4], split: GridSplit, i: i32, j: i32) -> i32 {
            let [nw, ne, se, sw] = h;
            match split {
                GridSplit::NwSe if i >= j => nw * STEPS + (ne - nw) * i + (se - ne) * j,
                GridSplit::NwSe => nw * STEPS + (se - sw) * i + (sw - nw) * j,
                GridSplit::NeSw if i + j <= STEPS => nw * STEPS + (ne - nw) * i + (sw - nw) * j,
                GridSplit::NeSw => se * STEPS + (sw - se) * (STEPS - i) + (ne - se) * (STEPS - j),
            }
        }
        fn bilinear(h: [i32; 4], i: i32, j: i32) -> i32 {
            let [nw, ne, se, sw] = h;
            (STEPS - i) * (STEPS - j) * nw
                + i * (STEPS - j) * ne
                + i * j * se
                + (STEPS - i) * j * sw
        }
        let error = |h: [i32; 4], split: GridSplit| -> i64 {
            (0..=STEPS)
                .flat_map(|j| (0..=STEPS).map(move |i| (i, j)))
                .map(|(i, j)| {
                    (i64::from(tri(h, split, i, j)) * i64::from(STEPS)
                        - i64::from(bilinear(h, i, j)))
                    .abs()
                })
                .sum()
        };
        let mut state = 3u64;
        for _ in 0..3000 {
            let h = random_corners(&mut state);
            assert_eq!(
                error(h, GridSplit::NwSe),
                error(h, GridSplit::NeSw),
                "{h:?}"
            );
        }
    }

    #[test]
    fn resplit_only_touches_auto_unplanar_single_level_surfaces() {
        let bump = [0, 0, 256, 0];
        let mut auto = surface(bump, GridSplit::NwSe);
        assert!(resplit(&mut auto));
        assert_eq!(auto.split, GridSplit::NeSw);
        // Legacy: locked.
        let mut legacy = surface(bump, GridSplit::NwSe);
        legacy.auto_split = false;
        assert!(!resplit(&mut legacy));
        assert_eq!(legacy.split, GridSplit::NwSe);
        // Planar: untouched even when it is auto.
        let mut planar = surface([0, 256, 512, 256], GridSplit::NeSw);
        assert!(!resplit(&mut planar));
        // Split-level: never.
        let mut level = surface(bump, GridSplit::NwSe);
        level.heights_b = Some([0; 4]);
        assert!(!resplit(&mut level));
    }
}
