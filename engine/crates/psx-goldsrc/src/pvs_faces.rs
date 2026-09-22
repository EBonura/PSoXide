//! Cold visible-face compilation over caller-owned storage.
/// Pvs link end.
pub const PVS_LINK_END: u16 = u16::MAX;
/// Pvs group liquid.
pub const PVS_GROUP_LIQUID: u16 = 0x8000;
/// Pvs group face mask.
pub const PVS_GROUP_FACE_MASK: u16 = 0x7fff;
/// Compact cached face record; preserves the existing 14-byte representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PvsFaceRec {
    /// Encoded first.
    pub first: u16,
    // Low 14 bits are the loop/triangle/patch record count. Bit 14 marks a
    // face whose material/light state needs the generic setup path; bit 15 is
    // the backed-cutout ordering flag. Cooked face counts are far below 16K,
    // so the cold PVS builder can preclassify the hot stream without growing
    // this record or the resident room arena.
    /// Encoded count.
    pub count: u16,
    /// Encoded center.
    pub center: [i16; 3],
    /// Encoded radius.
    pub radius: u16,
    /// Encoded tex.
    pub tex: u8, // per-face texture (loop faces store tex on the FaceRec)
    /// Encoded meta.
    pub meta: u8, // band[2:0] | loop<<3 | liquid<<4 | refined<<5 | backdrop<<6 | patch<<7
}
/// Pvs face band mask.
pub const PVS_FACE_BAND_MASK: u8 = 0x07;
/// Pvs face loop.
pub const PVS_FACE_LOOP: u8 = 0x08;
/// Pvs face translucent.
pub const PVS_FACE_TRANSLUCENT: u8 = 0x10;
/// Pvs face refined topology.
pub const PVS_FACE_REFINED_TOPOLOGY: u8 = 0x20;
/// Pvs face coplanar backdrop.
pub const PVS_FACE_COPLANAR_BACKDROP: u8 = 0x40;
/// Pvs face patch.
pub const PVS_FACE_PATCH: u8 = 0x80;
/// Pvs face count mask.
pub const PVS_FACE_COUNT_MASK: u16 = 0x3fff;
/// Pvs face special.
pub const PVS_FACE_SPECIAL: u16 = 0x4000;
/// Pvs face cutout.
pub const PVS_FACE_CUTOUT: u16 = 0x8000;

impl PvsFaceRec {
    #[inline(always)]
    /// Band field access.
    pub fn band(self) -> u8 {
        self.meta & PVS_FACE_BAND_MASK
    }

    #[inline(always)]
    /// Count field access.
    pub fn count(self) -> usize {
        (self.count & PVS_FACE_COUNT_MASK) as usize
    }

    #[inline(always)]
    /// Cutout field access.
    pub fn cutout(self) -> bool {
        self.count & PVS_FACE_CUTOUT != 0
    }

    #[inline(always)]
    /// Special field access.
    pub fn special(self) -> bool {
        self.count & PVS_FACE_SPECIAL != 0
    }

    #[inline(always)]
    /// Set band field access.
    pub fn set_band(&mut self, band: u8) {
        self.meta = (self.meta & !PVS_FACE_BAND_MASK) | (band & PVS_FACE_BAND_MASK);
    }

    #[inline(always)]
    /// Is loop field access.
    pub fn is_loop(self) -> bool {
        self.meta & PVS_FACE_LOOP != 0
    }

    #[inline(always)]
    /// Is patch field access.
    pub fn is_patch(self) -> bool {
        self.meta & PVS_FACE_PATCH != 0
    }

    #[inline(always)]
    /// Liquid field access.
    pub fn liquid(self) -> bool {
        self.meta & PVS_FACE_TRANSLUCENT != 0
    }

    #[inline(always)]
    /// Refined topology field access.
    pub fn refined_topology(self) -> bool {
        self.meta & PVS_FACE_REFINED_TOPOLOGY != 0
    }

    #[inline(always)]
    /// Coplanar backdrop field access.
    pub fn coplanar_backdrop(self) -> bool {
        self.meta & PVS_FACE_COPLANAR_BACKDROP != 0
    }
}

// Packing three one-byte fields into `meta` saves 4 KiB across the 2,048-face
// cache without reducing either visibility or GPU packet capacity.
const _: () = assert!(core::mem::size_of::<PvsFaceRec>() == 14);
/// Empty pvs face rec.
pub const EMPTY_PVS_FACE_REC: PvsFaceRec = PvsFaceRec {
    first: 0,
    count: 0,
    center: [0; 3],
    radius: 0,
    tex: 0,
    meta: 0,
};
/// Statically dispatched map access. The producer retains map validity policy.
/// Marked face indices must refer to valid faces whenever below output capacity;
/// leaf mark ranges must have a representable end. No game or camera state is owned.
pub trait FaceSource {
    /// Number of potentially visible non-solid leaves.
    fn visible_leaf_count(&self) -> usize;
    /// Number of valid marksurface entries.
    fn mark_count(&self) -> usize;
    /// Leaf visibility offset, first marksurface and marksurface count.
    fn leaf(&self, leaf: usize) -> (i32, usize, usize);
    /// Face index at a marksurface entry.
    fn mark(&self, mark: usize) -> usize;
    /// First triangle/loop record and record count.
    fn face_tris(&self, face: usize) -> (usize, usize);
    /// Existing face-group identifier.
    fn face_group(&self, face: usize) -> usize;
    /// Authored liquid flag, also the existing translucent-summary criterion.
    fn face_liquid(&self, face: usize) -> bool;
    /// Backed-cutout ordering flag.
    fn face_cutout_backed(&self, face: usize) -> bool;
    /// Coplanar backdrop flag.
    fn face_coplanar_backdrop(&self, face: usize) -> bool;
    /// Bounding sphere center and radius.
    fn face_bounds(&self, face: usize) -> ([i32; 3], i32);
    /// Texture identifier before record narrowing.
    fn face_tex(&self, face: usize) -> usize;
    /// Loop topology flag.
    fn face_is_loop(&self, face: usize) -> bool;
    /// Patch topology flag.
    fn face_is_patch(&self, face: usize) -> bool;
    /// Refined topology flag.
    fn face_refined_topology(&self, face: usize) -> bool;
    /// Caller material/light classification for the generic emission path.
    fn face_needs_generic_emit(&self, face: usize) -> bool;
}

/// Borrowed existing arrays. No resident storage is allocated or retained.
/// Every buffer must be disjoint for the duration of compilation. Callers retire
/// packet caches that borrow unused array suffixes before constructing this view.
pub struct FaceBuffers<'a> {
    /// First-seen face indices in active order; length defines face capacity.
    pub indices: &'a mut [u16],
    /// Per-entry next link within its group; at least `indices.len()` entries.
    pub next: &'a mut [u16],
    /// Optional bounded fast face records; may be smaller than face capacity.
    pub records: &'a mut [PvsFaceRec],
    /// One-bit duplicate markers; enough words for face capacity.
    pub marks: &'a mut [u32],
    /// Group list heads; length defines group capacity.
    pub group_first: &'a mut [u16],
    /// Group representative face and liquid flag; same capacity as group heads.
    pub group_face: &'a mut [u16],
    /// First-seen active group order; at least group capacity.
    pub active_groups: &'a mut [u16],
    /// Animated textures used by accepted faces; same shape as animation input.
    pub animated_textures: &'a mut [u32],
}

/// Counts and flags published by the caller after the cold compile finishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaceSummary {
    /// Visible leaves visited, including leaves with no accepted faces.
    pub leaf_count: usize,
    /// Accepted unique faces, including those beyond fast-record capacity.
    pub face_count: usize,
    /// First-seen groups containing accepted faces.
    pub group_count: usize,
    /// Sum of accepted face record counts.
    pub triangle_references: usize,
    /// Whether an accepted face has the authored liquid flag.
    pub has_translucent: bool,
    /// Whether an accepted face references an animated texture.
    pub has_texture_animation: bool,
}
/// Encode one valid face using the existing compact flags and narrowing.
#[inline(always)]
pub fn build_pvs_face_rec<M: FaceSource>(m: &M, face: usize) -> PvsFaceRec {
    let (first, cnt) = m.face_tris(face);
    let liquid = m.face_liquid(face);
    let cutout = m.face_cutout_backed(face);
    let coplanar_backdrop = m.face_coplanar_backdrop(face);
    let (bc, radius) = m.face_bounds(face);
    PvsFaceRec {
        first: first as u16,
        count: cnt as u16
            | if m.face_needs_generic_emit(face) {
                PVS_FACE_SPECIAL
            } else {
                0
            }
            | if cutout { PVS_FACE_CUTOUT } else { 0 },
        center: [bc[0] as i16, bc[1] as i16, bc[2] as i16],
        radius: radius as u16,
        tex: m.face_tex(face) as u8,
        meta: (if m.face_is_loop(face) {
            PVS_FACE_LOOP
        } else {
            0
        }) | (if m.face_is_patch(face) {
            PVS_FACE_PATCH
        } else {
            0
        }) | (if liquid { PVS_FACE_TRANSLUCENT } else { 0 })
            | (if m.face_refined_topology(face) {
                PVS_FACE_REFINED_TOPOLOGY
            } else {
                0
            })
            | (if coplanar_backdrop {
                PVS_FACE_COPLANAR_BACKDROP
            } else {
                0
            }),
    }
}

/// Compile one already-unioned visibility row into the caller's face lists.
///
/// The caller keeps primary/partner/underwater row selection, entity candidates,
/// animation-generation resets and camera-key publication. Previous active groups
/// are reset before new links are written. Asset-map validity remains a producer
/// precondition; this is not a corrupt-map validator.
///
/// # Panics
/// Panics on inconsistent buffer shapes or previous-group metadata. Face and
/// group capacities must fit u16 links, and the animation domain must be a nonzero
/// power of two. The game adapters supply their existing compile-time capacities.
#[optimize(size)]
pub fn compile_faces<M: FaceSource>(
    m: &M,
    visibility: &[u8],
    animated: &[u32],
    previous_groups: usize,
    out: FaceBuffers<'_>,
) -> FaceSummary {
    let max_faces = out.indices.len();
    let max_groups = out.group_first.len();
    let max_leaves = visibility.len() * 8;
    let texture_count = animated.len() * 32;
    assert!(max_faces <= u16::MAX as usize && max_groups <= u16::MAX as usize);
    assert!(out.next.len() >= max_faces && out.marks.len() >= max_faces.div_ceil(32));
    assert!(out.group_face.len() == max_groups && out.active_groups.len() >= max_groups);
    assert!(previous_groups <= out.active_groups.len());
    assert!(out.animated_textures.len() == animated.len() && texture_count.is_power_of_two());
    let mut old_group = 0usize;
    while old_group < previous_groups {
        out.group_face[out.active_groups[old_group] as usize] = PVS_LINK_END;
        old_group += 1;
    }
    let mut summary = FaceSummary::default();
    out.animated_textures.fill(0);
    // A 1-bit duplicate marker recovers 7.4 KiB over the old byte-per-face
    // token array. PVS rebuilds already walk the visible marks; clearing 272
    // words here is cold compared with retaining that RAM every frame.
    out.marks.fill(0);

    for i in 0..m.visible_leaf_count().min(max_leaves) {
        if visibility[i >> 3] & (1u8 << (i & 7)) == 0 {
            continue;
        }
        let leaf = i + 1;
        summary.leaf_count += 1;

        let (_, m0, mc) = m.leaf(leaf);
        for mj in m0..m0 + mc {
            if mj >= m.mark_count() {
                break;
            }
            let face = m.mark(mj);
            if face >= max_faces {
                continue;
            }
            let mark_word = face >> 5;
            let mark_bit = 1u32 << (face & 31);
            if out.marks[mark_word] & mark_bit != 0 {
                continue;
            }
            out.marks[mark_word] |= mark_bit;
            let (first, cnt) = m.face_tris(face);
            if cnt == 0 || first > u16::MAX as usize || cnt > PVS_FACE_COUNT_MASK as usize {
                continue;
            }

            if summary.face_count >= max_faces {
                continue;
            }

            let group = m.face_group(face);
            if group >= max_groups {
                continue;
            }
            if out.group_face[group] == PVS_LINK_END {
                if summary.group_count >= max_groups {
                    break;
                }
                out.group_first[group] = PVS_LINK_END;
                out.group_face[group] = face as u16 & PVS_GROUP_FACE_MASK;
                out.active_groups[summary.group_count] = group as u16;
                summary.group_count += 1;
            }
            if m.face_liquid(face) {
                out.group_face[group] |= PVS_GROUP_LIQUID;
            }

            let entry = summary.face_count;
            out.indices[entry] = face as u16;
            let liquid = m.face_liquid(face);
            let face_tex = m.face_tex(face);
            let tex_index = face_tex & (texture_count - 1);
            let animated = animated[tex_index >> 5] & (1 << (tex_index & 31)) != 0;
            summary.has_translucent |= liquid;
            summary.has_texture_animation |= animated;
            if animated {
                let tex = face_tex & (texture_count - 1);
                out.animated_textures[tex >> 5] |= 1u32 << (tex & 31);
            }
            if entry < out.records.len() {
                out.records[entry] = build_pvs_face_rec(m, face);
            }
            summary.triangle_references += cnt;
            out.next[entry] = out.group_first[group];
            out.group_first[group] = entry as u16;
            summary.face_count += 1;
        }
    }

    summary
}
