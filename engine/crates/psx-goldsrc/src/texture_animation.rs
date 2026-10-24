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

// ---- Retargeting already-built packets at a chain step ----
//
// A stationary view keeps its world packets from frame to frame. A chain step
// changes only which texture some of those packets sample, so instead of
// building every packet again the caller rewrites the texture words of the
// packets that showed a chain's previous frame. The packet's own texture words
// identify it: the caller proves at map load (`packets_retargetable`) that no
// other drawable texture samples the same window, CLUT and page as a chain
// member, and that nothing else a packet builder reads from the texture
// differs between the frames of a chain.

/// Textured Gouraud triangle and quad packets (psx-gpu `TriTexturedGouraud`
/// and `QuadTexturedGouraud`) share their first eight words: tag, texture
/// window, colour-0 command, v0, uv0|clut, colour-1, v1, uv1|tpage.
const WINDOW_WORD: usize = 1;
const CLUT_WORD: usize = 4;
const TPAGE_WORD: usize = 7;
/// The tpage semi-transparency mode, in the high half of the uv1|tpage word.
/// A blended face picks it for any texture, so it belongs to the packet, not
/// to the texture, and a retarget keeps the packet's value.
const TPAGE_BLEND: u32 = 0x0060 << 16;

/// The words that select what a textured Gouraud packet samples, in the
/// positions the packet stores them (psx-gpu `TexturedGouraudPacketMaterial`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PacketTexture {
    /// GP0(E2) texture-window word.
    pub window: u32,
    /// CLUT word shifted into the high half.
    pub clut: u32,
    /// Tpage word shifted into the high half.
    pub tpage: u32,
}

impl PacketTexture {
    const EMPTY: Self = Self {
        window: 0,
        clut: 0,
        tpage: 0,
    };

    /// CLUT and page (address and depth, blend mode cleared) in one word.
    #[inline(always)]
    const fn place(self) -> u32 {
        (self.clut & 0xffff_0000) | ((self.tpage & !TPAGE_BLEND) >> 16)
    }
}

/// Prefilter bit for a CLUT half-word: mixes the CLUT's x slot and VRAM row.
#[inline(always)]
const fn clut_filter_bit(clut_high: u32) -> u32 {
    let c = clut_high >> 16;
    1u32 << ((c ^ (c >> 6)) & 31)
}

/// Whether a chain step can be applied to built packets by
/// [`PacketRetarget`] with the same result as building them again.
///
/// `texture(id)` returns the packet words of a texture that can be drawn and
/// `fixed`, every other per-texture value the caller's packet builders read
/// (command word, ordering or fog flags); `None` for a texture that is never
/// drawn. `textures` bounds the ids to compare. Requires that every member of
/// a chain pair (either row can show the primary or the alternate frame) is
/// drawable with one `fixed` value and one material blend mode, that no id is
/// in two chains or twice in one, that no other drawable texture samples the
/// same window, CLUT and page as a member, and that at most `max_sides` sides
/// (primary or alternate, with two or more frames) can change frame. Cold:
/// run once per map after the textures are resident.
#[inline(never)]
pub fn packets_retargetable<'a>(
    chains: impl Iterator<Item = (&'a [u8], &'a [u8])>,
    textures: usize,
    max_sides: usize,
    texture: impl Fn(usize) -> Option<(PacketTexture, u32)>,
) -> bool {
    let mut members: TextureMask = [0; TEXTURE_COUNT / 32];
    let mut sides = 0usize;
    for (primary, alternate) in chains {
        sides += (primary.len() > 1) as usize + (alternate.len() > 1) as usize;
        let mut first = None;
        if sides > max_sides
            || !side_retargetable(primary, textures, &texture, &mut members, &mut first)
            || !side_retargetable(alternate, textures, &texture, &mut members, &mut first)
        {
            return false;
        }
    }
    true
}

/// One side of [`packets_retargetable`]; `first` carries the pair's shared
/// `fixed` value and material blend mode across both sides.
#[inline(never)]
fn side_retargetable(
    side: &[u8],
    textures: usize,
    texture: &impl Fn(usize) -> Option<(PacketTexture, u32)>,
    members: &mut TextureMask,
    first: &mut Option<(u32, u32)>,
) -> bool {
    for &id in side {
        let bit = 1u32 << (id & 31);
        if members[(id >> 5) as usize] & bit != 0 {
            return false;
        }
        members[(id >> 5) as usize] |= bit;
        let Some((t, fixed)) = texture(id as usize) else {
            return false;
        };
        let key = (fixed, t.tpage & TPAGE_BLEND);
        if *first.get_or_insert(key) != key {
            return false;
        }
        for other in 0..textures {
            if other == id as usize {
                continue;
            }
            if let Some((o, _)) = texture(other) {
                if o.window == t.window && o.place() == t.place() {
                    return false;
                }
            }
        }
    }
    true
}

/// Texture rewrites for built packets, staged per chain step.
///
/// Holds up to `N` changed sides: the previous frame's window and place, and
/// the new frame's words. The caller stages at most once per frame, applies to
/// every packet it reuses, and must only write packets the GPU has finished
/// reading (after the previous frame's DMA walk).
pub struct PacketRetarget<const N: usize> {
    from_window: [u32; N],
    from_place: [u32; N],
    to: [PacketTexture; N],
    len: usize,
    filter: u32,
}

impl<const N: usize> PacketRetarget<N> {
    /// An empty set.
    pub const fn new() -> Self {
        Self {
            from_window: [0; N],
            from_place: [0; N],
            to: [PacketTexture::EMPTY; N],
            len: 0,
            filter: 0,
        }
    }

    /// Drop every staged rewrite.
    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
        self.filter = 0;
    }

    /// True when no rewrite is staged.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Stage the frame changes between two animation clocks (the `last_tenth`
    /// values kept by [`tick`]): every side whose shown frame differs maps
    /// that frame's words to its current frame's. Returns false, with nothing
    /// staged, when more than `N` sides changed.
    #[inline(never)]
    pub fn stage<'a>(
        &mut self,
        chains: impl Iterator<Item = (&'a [u8], &'a [u8])>,
        from_tenth: u16,
        to_tenth: u16,
        texture: impl Fn(usize) -> PacketTexture,
    ) -> bool {
        self.clear();
        for (primary, alternate) in chains {
            if !self.stage_side(primary, from_tenth, to_tenth, &texture)
                || !self.stage_side(alternate, from_tenth, to_tenth, &texture)
            {
                self.clear();
                return false;
            }
        }
        true
    }

    /// One side of [`Self::stage`]; false when the set is full.
    #[inline(never)]
    fn stage_side(
        &mut self,
        side: &[u8],
        from_tenth: u16,
        to_tenth: u16,
        texture: &impl Fn(usize) -> PacketTexture,
    ) -> bool {
        if side.len() < 2 {
            return true;
        }
        let old = side[frame_index(from_tenth, side.len())];
        let new = side[frame_index(to_tenth, side.len())];
        if old == new {
            return true;
        }
        if self.len >= N {
            return false;
        }
        let from = texture(old as usize);
        self.from_window[self.len] = from.window;
        self.from_place[self.len] = from.place();
        self.to[self.len] = texture(new as usize);
        self.filter |= clut_filter_bit(from.clut);
        self.len += 1;
        true
    }

    /// Point one built packet at its texture's current frame when it shows a
    /// staged previous frame. Keeps the packet's UVs, colours, command word
    /// and blend mode; rewrites its window, CLUT and page.
    ///
    /// # Safety
    ///
    /// `packet` must point at a textured Gouraud triangle or quad packet that
    /// the GPU is not reading.
    #[inline(always)]
    pub unsafe fn apply(&self, packet: *mut u32) {
        let clut_word = packet.add(CLUT_WORD).read();
        if self.filter & clut_filter_bit(clut_word) == 0 {
            return;
        }
        self.apply_slow(packet, clut_word);
    }

    #[inline(never)]
    unsafe fn apply_slow(&self, packet: *mut u32, clut_word: u32) {
        let tpage_word = packet.add(TPAGE_WORD).read();
        let place = (clut_word & 0xffff_0000) | ((tpage_word & !TPAGE_BLEND) >> 16);
        let window = packet.add(WINDOW_WORD).read();
        let mut i = 0;
        while i < self.len {
            if self.from_place[i] == place && self.from_window[i] == window {
                let to = self.to[i];
                packet.add(WINDOW_WORD).write(to.window);
                packet
                    .add(CLUT_WORD)
                    .write((clut_word & 0xffff) | (to.clut & 0xffff_0000));
                packet.add(TPAGE_WORD).write(
                    (tpage_word & (0xffff | TPAGE_BLEND)) | (to.tpage & 0xffff_0000 & !TPAGE_BLEND),
                );
                return;
            }
            i += 1;
        }
    }
}

impl<const N: usize> Default for PacketRetarget<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXTURES: usize = 24;

    // Distinct words per texture: window by index, CLUT row by index, page
    // alternating between two 4/8bpp pages, material blend mode 0.
    fn words(id: usize) -> PacketTexture {
        PacketTexture {
            window: 0xe200_0000 | (id as u32 * 0x421),
            clut: ((id as u32) << 6 | 0x10) << 16,
            tpage: ((if id & 1 == 0 { 0x0085 } else { 0x0106 }) as u32 | 0x0200) << 16,
        }
    }

    fn texture(id: usize) -> Option<(PacketTexture, u32)> {
        (id < TEXTURES).then(|| (words(id), 0x3400_0000))
    }

    // What a fresh build emits for a face: display frame `shown`, the face's
    // blend mode (packet-owned tpage bits) and per-vertex UVs and colours.
    fn build(shown: usize, blend: u32, quad: bool) -> [u32; 13] {
        let t = words(shown);
        let mut p = [0u32; 13];
        p[0] = 0x0900_0000;
        p[1] = t.window;
        p[2] = if quad { 0x3e40_5060 } else { 0x3610_2030 };
        p[3] = 0x0010_0020;
        p[4] = 0x1234 | t.clut;
        p[5] = 0x0040_5060;
        p[6] = 0x0030_0040;
        p[7] = 0x5678 | ((t.tpage & !TPAGE_BLEND) | (blend << 21));
        p[8] = 0x0070_8090;
        p[9] = 0x0050_0060;
        p[10] = 0x9abc;
        p[11] = 0x00a0_b0c0;
        p[12] = 0x0070_0080;
        p
    }

    // The display rows `tick` would leave for a given clock.
    fn rows(chains: &[(&[u8], &[u8])], tenth: u16) -> [[u8; 256]; 2] {
        let mut rows = [[0u8; 256]; 2];
        for (i, r) in rows[0].iter_mut().enumerate() {
            *r = i as u8;
        }
        rows[1] = rows[0];
        let (mut last, mut g, mut v) = (tenth.wrapping_add(1), 0, 0);
        tick(
            chains.iter().copied(),
            tenth << 1,
            &mut last,
            &mut g,
            &mut v,
            &[0; 8],
            |i, a, b| {
                rows[0][i] = a;
                rows[1][i] = b;
                true
            },
        );
        rows
    }

    #[test]
    fn retarget_equals_a_fresh_build_for_every_member_row_and_blend() {
        let chains: &[(&[u8], &[u8])] = &[
            (&[1, 2, 3], &[4, 5]),
            (&[6, 7], &[]),
            (&[], &[8, 9, 10, 11]),
            (&[12], &[13]),
        ];
        assert!(packets_retargetable(
            chains.iter().copied(),
            TEXTURES,
            8,
            texture
        ));
        let mut r = PacketRetarget::<8>::new();
        for from in 0..40u16 {
            for to in 0..40u16 {
                assert!(r.stage(chains.iter().copied(), from, to, words));
                let (a, b) = (rows(chains, from), rows(chains, to));
                for face in 0..TEXTURES {
                    for row in 0..2 {
                        for blend in 0..4 {
                            for quad in [false, true] {
                                let mut p = build(a[row][face] as usize, blend, quad);
                                unsafe { r.apply(p.as_mut_ptr()) };
                                assert_eq!(p, build(b[row][face] as usize, blend, quad));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn staging_reports_overflow_and_leaves_nothing() {
        let chains: &[(&[u8], &[u8])] = &[(&[1, 2], &[3, 4]), (&[5, 6], &[])];
        let mut r = PacketRetarget::<2>::new();
        assert!(!r.stage(chains.iter().copied(), 0, 2, words));
        assert!(r.is_empty());
        let mut r = PacketRetarget::<3>::new();
        assert!(r.stage(chains.iter().copied(), 0, 2, words));
        assert!(!r.is_empty());
        assert!(r.stage(chains.iter().copied(), 0, 1, words));
        assert!(r.is_empty());
    }

    #[test]
    fn unsafe_maps_are_rejected() {
        let chains: &[(&[u8], &[u8])] = &[(&[1, 2], &[3])];
        let ok = |c: &[(&[u8], &[u8])], f: &dyn Fn(usize) -> Option<(PacketTexture, u32)>| {
            packets_retargetable(c.iter().copied(), TEXTURES, 4, f)
        };
        assert!(ok(chains, &texture));
        // Too many changing sides.
        assert!(!packets_retargetable(
            chains.iter().copied(),
            TEXTURES,
            0,
            texture
        ));
        // An undrawable member.
        assert!(!ok(chains, &|id| if id == 3 { None } else { texture(id) }));
        // Members that differ in something else the builder reads.
        assert!(!ok(chains, &|id| texture(id)
            .map(|(t, f)| (t, f | (id == 2) as u32))));
        // Members with different material blend modes.
        assert!(!ok(chains, &|id| {
            texture(id).map(|(mut t, f)| {
                if id == 1 {
                    t.tpage |= 0x20 << 16;
                }
                (t, f)
            })
        }));
        // Another texture sampling the same window, CLUT and page as a member.
        assert!(!ok(chains, &|id| texture(if id == 20 { 2 } else { id })));
        // The same texture in two chains, or twice in one.
        assert!(!ok(&[(&[1, 2], &[]), (&[2, 3], &[])], &texture));
        assert!(!ok(&[(&[1, 1], &[])], &texture));
    }

    #[test]
    fn packets_of_other_textures_are_untouched() {
        let chains: &[(&[u8], &[u8])] = &[(&[1, 2, 3], &[])];
        let mut r = PacketRetarget::<4>::new();
        assert!(r.stage(chains.iter().copied(), 0, 2, words));
        for id in [0usize, 4, 5, 17] {
            let mut p = build(id, 1, false);
            let q = p;
            unsafe { r.apply(p.as_mut_ptr()) };
            assert_eq!(p, q);
        }
    }
}
