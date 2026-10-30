//! Edge conformity set for the affine-error patch policy.
//!
//! A world quad that the ranking splits into a regular 2x2 patch must split
//! the same way on both sides of every edge it shares, or the screen shows a
//! crack. The patches chosen for the following frame therefore record their
//! four edges here ([`EdgePolicy::request`]); the next frame looks its edges
//! up ([`EdgePolicy::current_mask`]) and adopts the same topology for a
//! matching neighbour.
//!
//! Half-Life and Counter-Strike carried identical copies of this code over
//! thirteen `static mut` items. The caller now owns one [`EdgePolicy`] value
//! and passes it by reference, so each port keeps its own state and policy
//! constants; the bodies, inline attributes and tie-breaking are unchanged.

/// Patches that may request edges for one following frame.
pub const CANDIDATE_CAP: usize = 32;
/// Four edges per patch.
pub const EDGE_CAP: usize = CANDIDATE_CAP * 4;
/// 32-bit words in the lookup bloom filter.
pub const BLOOM_WORDS: usize = 8;
const BLOOM_MASK: usize = BLOOM_WORDS * 32 - 1;

/// Key of the undirected edge between two vertex indices (each below 2^14).
#[inline(always)]
pub fn edge_key(a: u16, b: u16) -> u32 {
    let lo = a.min(b) as u32;
    let hi = a.max(b) as u32;
    (lo << 14) | hi
}

/// Edge keys of a quad in `indices` order `[a, b, c, d]` where the quad's
/// outline runs a-b, b-d, d-c, c-a.
#[inline(always)]
pub fn quad_edge_keys(indices: [u16; 4]) -> [u32; 4] {
    [
        edge_key(indices[0], indices[1]),
        edge_key(indices[1], indices[3]),
        edge_key(indices[3], indices[2]),
        edge_key(indices[2], indices[0]),
    ]
}

#[inline(always)]
fn bloom_slot(key: u32) -> usize {
    (key ^ (key >> 7) ^ (key >> 15)) as usize & BLOOM_MASK
}

/// Current and next-frame edge sets plus the budget that bounds the next one.
pub struct EdgePolicy {
    current: [u32; EDGE_CAP],
    next: [u32; EDGE_CAP],
    bloom: [u32; BLOOM_WORDS],
    current_count: u8,
    next_count: u8,
    next_split_patches: u8,
    next_budget: u16,
    generation: u16,
    pending: bool,
}

impl EdgePolicy {
    /// An empty policy whose first following frame may spend `max_extra`
    /// extra packets.
    pub const fn new(max_extra: u16) -> Self {
        Self {
            current: [0; EDGE_CAP],
            next: [0; EDGE_CAP],
            bloom: [0; BLOOM_WORDS],
            current_count: 0,
            next_count: 0,
            next_split_patches: 0,
            next_budget: max_extra,
            generation: 0,
            pending: false,
        }
    }

    /// Start collecting the following frame's edges: forget the pending set
    /// and restore the packet budget to `max_extra`.
    #[inline(always)]
    pub fn begin_frame(&mut self, max_extra: u16) {
        self.next_count = 0;
        self.next_split_patches = 0;
        self.next_budget = max_extra;
    }

    /// Whether a collected set waits to be installed on a hold frame.
    #[inline(always)]
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Mark a collected set as waiting (or not) for installation.
    #[inline(always)]
    pub fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
    }

    /// Bumps whenever an installed set differs from the one before it.
    #[inline(always)]
    pub fn generation(&self) -> u16 {
        self.generation
    }

    /// Atomically record all four edges of a patch selected for the following
    /// frame. A matching edge makes its neighbour use the same regular 2x2
    /// topology, so conformity never requires an irregular triangle fan.
    ///
    /// Returns whether the patch was admitted; on `false` nothing changed.
    /// `cost` is the extra packets the patch adds; a caller that tracks
    /// requested cost adds it when this returns `true`.
    #[inline(never)]
    pub fn request(&mut self, keys: [u32; 4], cost: u16) -> bool {
        if self.next_split_patches as usize >= CANDIDATE_CAP || self.next_budget < cost {
            return false;
        }
        let edges = self.next.as_mut_ptr();
        let mut needed = 0usize;
        let mut missing = 0u8;
        let mut edge = 0usize;
        while edge < 4 {
            let key = keys[edge];
            let mut found = false;
            let mut i = 0usize;
            while i < self.next_count as usize {
                // SAFETY: `next_count <= EDGE_CAP` is kept by the check below.
                if unsafe { edges.add(i).read() } == key {
                    found = true;
                    break;
                }
                i += 1;
            }
            if !found {
                needed += 1;
                missing |= 1 << edge;
            }
            edge += 1;
        }
        if self.next_count as usize + needed > EDGE_CAP {
            return false;
        }
        edge = 0;
        while edge < 4 {
            if missing & (1 << edge) != 0 {
                let key = keys[edge];
                // Preserve the old second scan's duplicate behavior without
                // walking the complete accumulated list again. An earlier missing
                // copy is exactly the only way this key could have been appended
                // since the first pass.
                let mut duplicate = false;
                let mut earlier = 0usize;
                while earlier < edge {
                    if missing & (1 << earlier) != 0 && keys[earlier] == key {
                        duplicate = true;
                        break;
                    }
                    earlier += 1;
                }
                if !duplicate {
                    // SAFETY: the check above left room for `needed` more keys.
                    unsafe { edges.add(self.next_count as usize).write(key) };
                    self.next_count += 1;
                }
            }
            edge += 1;
        }
        self.next_split_patches += 1;
        self.next_budget -= cost;
        true
    }

    /// Sort the collected set ascending so lookups are a binary search.
    #[inline(never)]
    pub fn sort_next(&mut self) {
        let next = self.next.as_mut_ptr();
        let count = self.next_count as usize;
        // Tiny deterministic insertion sort; lookup on every visible patch is then
        // a seven-comparison binary search instead of a 128-entry scan.
        let mut i = 1usize;
        while i < count {
            // SAFETY: `count <= EDGE_CAP` and every index below is `< count`.
            unsafe {
                let value = next.add(i).read();
                let mut j = i;
                while j > 0 && next.add(j - 1).read() > value {
                    next.add(j).write(next.add(j - 1).read());
                    j -= 1;
                }
                next.add(j).write(value);
            }
            i += 1;
        }
    }

    /// Make the collected set current, rebuild the lookup filter and bump
    /// the generation if the set changed.
    #[inline(never)]
    pub fn install_next(&mut self) {
        let next = self.next.as_ptr();
        let count = self.next_count as usize;
        let current = self.current.as_mut_ptr();
        self.bloom = [0; BLOOM_WORDS];
        let mut changed = count != self.current_count as usize;
        let mut i = 0usize;
        while i < count {
            // SAFETY: `count <= EDGE_CAP`.
            let value = unsafe { next.add(i).read() };
            // SAFETY: as above, `i < count <= EDGE_CAP`.
            unsafe {
                changed |= current.add(i).read() != value;
                current.add(i).write(value);
            }
            let hash = bloom_slot(value);
            self.bloom[hash >> 5] |= 1u32 << (hash & 31);
            i += 1;
        }
        self.current_count = count as u8;
        if changed {
            self.generation = self.generation.wrapping_add(1);
        }
        self.pending = false;
    }

    /// Whether the current set holds `key` (bloom filter, then binary search
    /// of the sorted set).
    #[inline(always)]
    pub fn current_contains(&self, key: u32) -> bool {
        let hash = bloom_slot(key);
        if self.bloom[hash >> 5] & (1u32 << (hash & 31)) == 0 {
            return false;
        }
        let edges = self.current.as_ptr();
        let mut lo = 0usize;
        let mut hi = self.current_count as usize;
        while lo < hi {
            let mid = (lo + hi) >> 1;
            // SAFETY: `mid < current_count <= EDGE_CAP`.
            let value = unsafe { edges.add(mid).read() };
            if value < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        // SAFETY: `lo < current_count <= EDGE_CAP` is checked first.
        lo < self.current_count as usize && unsafe { edges.add(lo).read() } == key
    }

    /// Bit `e` is set when edge `e` of the quad is in the current set and is
    /// not in `blocked`.
    #[inline(always)]
    pub fn current_mask(&self, keys: [u32; 4], blocked: u8) -> u8 {
        if self.current_count == 0 {
            return 0;
        }
        let mut mask = 0u8;
        let mut edge = 0usize;
        while edge < 4 {
            if blocked & (1 << edge) == 0 && self.current_contains(keys[edge]) {
                mask |= 1 << edge;
            }
            edge += 1;
        }
        mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(seed: u32) -> [u32; 4] {
        let mut k = [0u32; 4];
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        for slot in &mut k {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            *slot = ((x >> 8) % 40) << 14 | ((x >> 3) % 40 + 41);
        }
        k
    }

    #[test]
    fn edge_key_is_undirected() {
        assert_eq!(edge_key(3, 9), edge_key(9, 3));
        assert_eq!(edge_key(3, 9), (3 << 14) | 9);
        assert_eq!(quad_edge_keys([0, 1, 2, 3])[1], edge_key(1, 3));
    }

    /// Reference model with `Vec`s: same admission rule, duplicate handling,
    /// ordering and lookup.
    struct Model {
        next: Vec<u32>,
        patches: usize,
        budget: u16,
    }

    impl Model {
        fn request(&mut self, k: [u32; 4], cost: u16) -> bool {
            if self.patches >= CANDIDATE_CAP || self.budget < cost {
                return false;
            }
            let mut add = Vec::new();
            for key in k {
                if !self.next.contains(&key) && !add.contains(&key) {
                    add.push(key);
                }
            }
            if self.next.len() + add.len() > EDGE_CAP {
                return false;
            }
            self.next.extend(add);
            self.patches += 1;
            self.budget -= cost;
            true
        }
    }

    #[test]
    fn request_sort_install_match_the_vec_model() {
        let mut p = EdgePolicy::new(176);
        for round in 0..6u32 {
            p.begin_frame(176);
            let mut m = Model { next: Vec::new(), patches: 0, budget: 176 };
            for i in 0..50u32 {
                let k = keys(round * 1000 + i);
                let cost = 3 + (i % 9) as u16;
                assert_eq!(p.request(k, cost), m.request(k, cost), "round {round} patch {i}");
                assert_eq!(p.next_count as usize, m.next.len());
            }
            assert_eq!(&p.next[..p.next_count as usize], m.next.as_slice());
            p.sort_next();
            m.next.sort_unstable();
            assert_eq!(&p.next[..p.next_count as usize], m.next.as_slice());
            let before = p.generation();
            p.install_next();
            assert!(!p.pending());
            for probe in 0..400u32 {
                let k = keys(probe);
                for key in k {
                    assert_eq!(p.current_contains(key), m.next.contains(&key));
                }
                let blocked = (probe % 16) as u8;
                let mut want = 0u8;
                for (e, key) in k.iter().enumerate() {
                    if blocked & (1 << e) == 0 && m.next.contains(key) {
                        want |= 1 << e;
                    }
                }
                assert_eq!(p.current_mask(k, blocked), want);
            }
            // A second install of the same set must not bump the generation.
            let g = p.generation();
            assert!(g == before.wrapping_add(1) || round > 0 && g == before);
            p.install_next();
            assert_eq!(p.generation(), g);
        }
    }

    #[test]
    fn a_full_set_refuses_without_changing_anything() {
        let mut p = EdgePolicy::new(u16::MAX);
        p.begin_frame(u16::MAX);
        let mut n = 0u32;
        while p.request([n * 4, n * 4 + 1, n * 4 + 2, n * 4 + 3], 1) {
            n += 1;
        }
        assert_eq!(n as usize, CANDIDATE_CAP);
        let count = p.next_count;
        assert!(!p.request([9_000_000, 9_000_001, 9_000_002, 9_000_003], 1));
        assert_eq!(p.next_count, count);
    }

    #[test]
    fn budget_is_spent_atomically() {
        let mut p = EdgePolicy::new(10);
        assert!(p.request([1, 2, 3, 4], 6));
        assert!(!p.request([5, 6, 7, 8], 6));
        assert_eq!(p.next_count, 4);
        assert!(p.request([5, 6, 7, 8], 4));
    }
}
