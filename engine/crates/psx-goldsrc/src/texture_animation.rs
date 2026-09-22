//! GoldSrc texture-chain scheduling with caller-owned display rows and masks.

/// Number of compact texture identifiers representable by the cooked format.
pub const TEXTURE_COUNT: usize = 256;
/// Membership bits for all compact texture identifiers.
pub type TextureMask = [u32; TEXTURE_COUNT / 32];

/// Mark primary and alternate chain members after the caller clears the mask.
/// No display rows or resident buffers are allocated.
pub fn mark_members<'a>(
    chains: impl Iterator<Item = (&'a [u8], &'a [u8])>,
    mask: &mut TextureMask,
) {
    for (primary, alternate) in chains {
        for &id in primary.iter().chain(alternate.iter()) {
            mask[(id >> 5) as usize] |= 1u32 << (id & 31);
        }
    }
}

/// Select the existing GoldSrc frame: two tenths per texture, zero for no frames.
#[inline(always)]
pub const fn frame_index(tenth: u16, frame_count: usize) -> usize {
    if frame_count == 0 {
        0
    } else {
        (tenth as usize / 2) % frame_count
    }
}

/// Advance chains at the port's 20 Hz simulation timebase.
///
/// `set` updates the caller's two display rows and returns whether either row
/// changed. The caller retains the rows, clock and generations in their existing
/// storage. `visible` contains current-PVS membership. Repeated tenths are a no-op;
/// empty counterpart chains display their own frame. Generations wrap and only
/// advance for actual row changes (the visible generation only for PVS members).
///
/// Keep this cold driver outlined: inlining it into the map-load and simulation
/// callers would duplicate the chain traversal on a RAM-constrained guest.
#[inline(never)]
pub fn tick<'a>(
    chains: impl ExactSizeIterator<Item = (&'a [u8], &'a [u8])>,
    simulation_tick: u16,
    last_tenth: &mut u16,
    generation: &mut u8,
    visible_generation: &mut u8,
    visible: &TextureMask,
    mut set: impl FnMut(usize, u8, u8) -> bool,
) {
    if chains.len() == 0 {
        return;
    }
    let tenth = simulation_tick >> 1;
    if tenth == *last_tenth {
        return;
    }
    *last_tenth = tenth;
    let mut changed = false;
    let mut visible_changed = false;
    for (primary, alternate) in chains {
        let pcur = primary
            .get(frame_index(tenth, primary.len()))
            .copied()
            .unwrap_or(0);
        let acur = alternate
            .get(frame_index(tenth, alternate.len()))
            .copied()
            .unwrap_or(0);
        for &id in primary {
            let id_changed = set(
                id as usize,
                pcur,
                if alternate.is_empty() { pcur } else { acur },
            );
            changed |= id_changed;
            visible_changed |= id_changed && visible[(id >> 5) as usize] & (1u32 << (id & 31)) != 0;
        }
        for &id in alternate {
            let id_changed = set(
                id as usize,
                acur,
                if primary.is_empty() { acur } else { pcur },
            );
            changed |= id_changed;
            visible_changed |= id_changed && visible[(id >> 5) as usize] & (1u32 << (id & 31)) != 0;
        }
    }
    if changed {
        *generation = generation.wrapping_add(1);
    }
    if visible_changed {
        *visible_generation = visible_generation.wrapping_add(1);
    }
}
