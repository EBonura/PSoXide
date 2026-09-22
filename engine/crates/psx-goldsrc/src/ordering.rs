//! One ordering-key policy for every world primitive.
//!
//! The PS1 has no Z buffer, so a primitive's ordering-table key is part of
//! surface correctness. Keep representative-depth selection and material
//! tie-breaking together: fast GTE, clipped, refined and cached paths must not
//! independently reinterpret the same face.
//!
//! The ordering-table length is the caller's: each port sizes its OT for its
//! own draw distance and passes it as the `OT_LEN` const parameter, so the
//! clamps below fold to the same immediates as a local constant.

pub const OT_SHIFT: u32 = 2;

/// Sixteen quarter-unit ranks inside the existing four-unit world bucket.
/// These affect only ties between studio triangles; world/actor bucket
/// relationships and the physical OT allocation are unchanged.
pub const MODEL_DEPTH_BANDS: usize = 16;
pub const MODEL_PACKET_WORDS: usize = 9;
pub const MODEL_DEPTH_WORD: usize = 8;
pub const MODEL_LINK_END: u32 = u32::MAX;

#[inline(always)]
pub fn model_depth_key<const OT_LEN: usize>(a: u16, b: u16, c: u16, scale_shift: u8) -> u16 {
    let quarter_depth = (((a as u32 + b as u32 + c as u32) * 4) / 3) >> scale_shift;
    quarter_depth.clamp(1 << (OT_SHIFT + 2), ((OT_LEN as u32) << (OT_SHIFT + 2)) - 1) as u16
}

/// A distant or nearly flat moving face already has a useful coarse key.
/// Nearby faces spanning several buckets need local samples around actors.
#[inline]
pub fn moving_face_needs_local_depth(depths: [u16; 3]) -> bool {
    let near = depths[0].min(depths[1]).min(depths[2]);
    let far = depths[0].max(depths[1]).max(depths[2]);
    near < 384 && far - near >= 16
}

/// Stable, bounded linear partition of the tightly packed studio packet
/// stream. Before GPU submission, UV2's unused high half holds the depth key
/// and tags are scratch links expressed as word offsets. Walk the returned
/// bands in ascending order and prepend each packet to its ordinary OT bucket.
/// Equal fine keys retain the original OT's reverse submission order.
#[inline(always)]
pub fn model_depth_bands(words: &mut [u32]) -> [u32; MODEL_DEPTH_BANDS] {
    debug_assert_eq!(words.len() % MODEL_PACKET_WORDS, 0);
    let packet_count = words.len() / MODEL_PACKET_WORDS;
    let mut heads = [MODEL_LINK_END; MODEL_DEPTH_BANDS];
    for (reverse_index, packet) in words.rchunks_exact_mut(MODEL_PACKET_WORDS).enumerate() {
        let band = ((packet[MODEL_DEPTH_WORD] >> 16) as usize) & (MODEL_DEPTH_BANDS - 1);
        packet[0] = heads[band];
        // The original offset is recovered from the iterator's reverse index.
        heads[band] = ((packet_count - reverse_index - 1) * MODEL_PACKET_WORDS) as u32;
    }
    heads
}

const BACKDROP_OTZ_BIAS: usize = 4;
const CUTOUT_OTZ_BIAS: usize = 4;
const COPLANAR_BACKDROP_OTZ_BIAS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceOrder {
    /// Cook-refined cells use their own centroid rather than the conservative
    /// far edge of the original BSP surface.
    pub local_depth: bool,
    /// A masked surface with cook-verified solid geometry immediately behind
    /// it. This is a bounded tie-break relationship, not a new depth model.
    pub cutout_backed: bool,
    /// Liquid/terrain ownership for exact coplanar pairs.
    pub coplanar_backdrop: bool,
    /// Solid texture class which is known to be the terminal scene backdrop.
    pub texture_backdrop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimitiveDepths {
    values: [i32; 4],
    count: u8,
}

impl PrimitiveDepths {
    #[inline(always)]
    pub const fn tri(a: i32, b: i32, c: i32) -> Self {
        Self {
            values: [a, b, c, 0],
            count: 3,
        }
    }

    #[inline(always)]
    pub const fn quad(a: i32, b: i32, c: i32, d: i32) -> Self {
        Self {
            values: [a, b, c, d],
            count: 4,
        }
    }

    #[inline(always)]
    fn representative(self, local: bool) -> i32 {
        if local {
            let sum = self.values[0]
                + self.values[1]
                + self.values[2]
                + if self.count == 4 { self.values[3] } else { 0 };
            (sum / self.count as i32).max(1)
        } else if self.count == 4 {
            self.values[0]
                .max(self.values[1])
                .max(self.values[2])
                .max(self.values[3])
                .max(1)
        } else {
            self.values[0]
                .max(self.values[1])
                .max(self.values[2])
                .max(1)
        }
    }
}

#[inline(always)]
fn clamp_otz<const OT_LEN: usize>(z: usize) -> usize {
    z.clamp(1, OT_LEN - 1)
}

/// Return the final world ordering-table key, including all surface ownership
/// adjustments. A backed cutout uses the same local representative depth as a
/// refined cell; only its small final bias is special. Promoting an entire
/// grate to its nearest corner made long walkways paint across foreground
/// walls even though the cook had classified the material correctly.
#[inline(always)]
pub fn world_order_key<const OT_LEN: usize>(
    depths: PrimitiveDepths,
    surface: SurfaceOrder,
) -> usize {
    let local = surface.local_depth || surface.cutout_backed;
    let base = clamp_otz::<OT_LEN>((depths.representative(local) as usize) >> OT_SHIFT);
    let back_biased =
        base + if surface.coplanar_backdrop {
            COPLANAR_BACKDROP_OTZ_BIAS
        } else {
            0
        } + if surface.texture_backdrop {
            BACKDROP_OTZ_BIAS
        } else {
            0
        };
    let ordered = if surface.cutout_backed && !surface.texture_backdrop {
        back_biased.saturating_sub(CUTOUT_OTZ_BIAS)
    } else {
        back_biased
    };
    clamp_otz::<OT_LEN>(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Half-Life table; Counter-Strike's longer one only moves the clamp.
    const OT_LEN: usize = 320;

    fn world_order_key(depths: PrimitiveDepths, surface: SurfaceOrder) -> usize {
        super::world_order_key::<OT_LEN>(depths, surface)
    }

    #[test]
    fn moving_faces_refine_only_nearby_depth_spans() {
        assert!(moving_face_needs_local_depth([160, 208, 208]));
        assert!(!moving_face_needs_local_depth([400, 448, 448]));
        assert!(!moving_face_needs_local_depth([160, 164, 164]));
        assert_eq!(
            moving_face_needs_local_depth([160, 208, 180]),
            moving_face_needs_local_depth([208, 180, 160])
        );
    }

    const OPAQUE: SurfaceOrder = SurfaceOrder {
        local_depth: false,
        cutout_backed: false,
        coplanar_backdrop: false,
        texture_backdrop: false,
    };

    #[test]
    fn fractional_model_keys_preserve_every_coarse_bucket() {
        let mut rng = 0x7461_7065u32;
        for shift in 0..=4 {
            for _ in 0..20_000 {
                let mut depths = [0u16; 3];
                for d in &mut depths {
                    rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                    *d = (rng >> 16) as u16;
                }
                let sum = depths.iter().map(|&d| d as u64).sum::<u64>();
                let old = (((sum / 3) >> shift) >> OT_SHIFT).clamp(1, (OT_LEN - 1) as u64);
                let key = model_depth_key::<OT_LEN>(depths[0], depths[1], depths[2], shift);
                assert_eq!((key >> 4) as u64, old);
                let exact = ((sum * 4 / 3) >> shift).clamp(16, (OT_LEN * 16 - 1) as u64);
                assert_eq!(key as u64, exact);
            }
        }
    }

    #[test]
    fn studio_band_partition_preserves_packets_and_matches_depth_sort() {
        use std::vec;
        use std::vec::Vec;
        let mut rng = 0x7261_696cu32;
        let mut keys = Vec::new();
        let mut words = vec![0u32; 1024 * MODEL_PACKET_WORDS];
        for (i, p) in words.chunks_exact_mut(MODEL_PACKET_WORDS).enumerate() {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            let key = 16 + (rng % ((OT_LEN as u32 - 1) * 16)) as u16;
            keys.push(key);
            for (j, w) in p.iter_mut().enumerate().skip(1) {
                *w = (i * MODEL_PACKET_WORDS + j) as u32;
            }
            p[MODEL_DEPTH_WORD] = (key as u32) << 16 | (i as u32 & 65535);
        }
        let original = words.clone();
        let heads = model_depth_bands(&mut words);
        let mut seen = vec![false; keys.len()];
        let mut buckets = vec![Vec::new(); OT_LEN];
        for mut offset in heads {
            while offset != MODEL_LINK_END {
                let offset_usize = offset as usize;
                assert_eq!(offset_usize % MODEL_PACKET_WORDS, 0);
                let index = offset_usize / MODEL_PACKET_WORDS;
                assert!(!seen[index], "duplicate or cyclic packet link");
                seen[index] = true;
                assert_eq!(
                    &words[offset_usize + 1..offset_usize + MODEL_PACKET_WORDS],
                    &original[offset_usize + 1..offset_usize + MODEL_PACKET_WORDS]
                );
                buckets[(keys[index] >> 4) as usize].insert(0, index);
                offset = words[offset_usize];
            }
        }
        assert!(seen.iter().all(|&v| v));
        for (bucket, actual) in buckets.iter().enumerate() {
            let mut expected = (0..keys.len())
                .filter(|&i| (keys[i] >> 4) as usize == bucket)
                .collect::<Vec<_>>();
            expected.sort_by_key(|&i| (core::cmp::Reverse(keys[i]), core::cmp::Reverse(i)));
            assert_eq!(*actual, expected);
        }
    }

    #[test]
    fn empty_studio_stream_has_no_links() {
        assert_eq!(
            model_depth_bands(&mut []),
            [MODEL_LINK_END; MODEL_DEPTH_BANDS]
        );
    }

    #[test]
    fn unrefined_world_surface_keeps_conservative_far_depth() {
        assert_eq!(
            world_order_key(PrimitiveDepths::tri(64, 512, 512), OPAQUE),
            128
        );
    }

    #[test]
    fn refined_surface_uses_its_local_centroid() {
        let refined = SurfaceOrder {
            local_depth: true,
            ..OPAQUE
        };
        assert_eq!(
            world_order_key(PrimitiveDepths::tri(64, 512, 512), refined),
            90
        );
    }

    #[test]
    fn backed_cutout_stays_between_foreground_and_its_backing() {
        let grate = SurfaceOrder {
            cutout_backed: true,
            ..OPAQUE
        };
        let depths = PrimitiveDepths::tri(64, 512, 512);
        let grate_key = world_order_key(depths, grate);
        let foreground_key = world_order_key(PrimitiveDepths::tri(200, 200, 200), OPAQUE);
        let backing_key = world_order_key(PrimitiveDepths::tri(370, 370, 370), OPAQUE);

        // The OT is traversed back-to-front: larger keys draw first. The wall
        // must cover the grate, while the grate must cover its close backing.
        assert!(foreground_key < grate_key);
        assert!(grate_key < backing_key);
        // The removed nearest-corner path would have produced bucket 12.
        assert_eq!(grate_key, 86);
    }
}
