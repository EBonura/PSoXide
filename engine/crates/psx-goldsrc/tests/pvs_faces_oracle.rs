//! Frozen face-list oracle, exact order/record flags and caller visibility unions.
use psx_goldsrc::pvs_faces::*;
#[derive(Clone, Debug)]
struct Face {
    first: usize,
    count: usize,
    group: usize,
    tex: usize,
    flags: u8,
    center: [i32; 3],
    radius: i32,
}
#[derive(Clone, Debug)]
struct Fixture {
    n_visleaves: usize,
    n_marks: usize,
    leaves: Vec<(i32, usize, usize)>,
    marks: Vec<usize>,
    faces: Vec<Face>,
}
impl FaceSource for Fixture {
    fn visible_leaf_count(&self) -> usize {
        self.n_visleaves
    }
    fn mark_count(&self) -> usize {
        self.n_marks
    }
    fn leaf(&self, i: usize) -> (i32, usize, usize) {
        self.leaves[i]
    }
    fn mark(&self, i: usize) -> usize {
        self.marks[i]
    }
    fn face_tris(&self, i: usize) -> (usize, usize) {
        (self.faces[i].first, self.faces[i].count)
    }
    fn face_group(&self, i: usize) -> usize {
        self.faces[i].group
    }
    fn face_tex(&self, i: usize) -> usize {
        self.faces[i].tex
    }
    fn face_bounds(&self, i: usize) -> ([i32; 3], i32) {
        (self.faces[i].center, self.faces[i].radius)
    }
    fn face_liquid(&self, i: usize) -> bool {
        self.faces[i].flags & 1 != 0
    }
    fn face_cutout_backed(&self, i: usize) -> bool {
        self.faces[i].flags & 2 != 0
    }
    fn face_coplanar_backdrop(&self, i: usize) -> bool {
        self.faces[i].flags & 4 != 0
    }
    fn face_is_loop(&self, i: usize) -> bool {
        self.faces[i].flags & 8 != 0
    }
    fn face_is_patch(&self, i: usize) -> bool {
        self.faces[i].flags & 16 != 0
    }
    fn face_refined_topology(&self, i: usize) -> bool {
        self.faces[i].flags & 32 != 0
    }
    fn face_needs_generic_emit(&self, i: usize) -> bool {
        self.faces[i].flags & 64 != 0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Storage {
    indices: Vec<u16>,
    next: Vec<u16>,
    records: Vec<PvsFaceRec>,
    marks: Vec<u32>,
    group_first: Vec<u16>,
    group_face: Vec<u16>,
    active_groups: Vec<u16>,
    animated_textures: Vec<u32>,
}
impl Storage {
    fn new(f: usize, g: usize, r: usize) -> Self {
        Self {
            indices: vec![123; f],
            next: vec![456; f],
            records: vec![EMPTY_PVS_FACE_REC; r],
            marks: vec![u32::MAX; f.div_ceil(32)],
            group_first: vec![PVS_LINK_END; g],
            group_face: vec![PVS_LINK_END; g],
            active_groups: vec![789; g],
            animated_textures: vec![u32::MAX; 2],
        }
    }
    fn view(&mut self) -> FaceBuffers<'_> {
        FaceBuffers {
            indices: &mut self.indices,
            next: &mut self.next,
            records: &mut self.records,
            marks: &mut self.marks,
            group_first: &mut self.group_first,
            group_face: &mut self.group_face,
            active_groups: &mut self.active_groups,
            animated_textures: &mut self.animated_textures,
        }
    }
}
mod legacy {
    include!("oracles/pvs_faces_legacy.rs");
    pub(super) fn compile(
        m: &Fixture,
        v: &[u8],
        a: &[u32],
        p: usize,
        s: &mut Storage,
    ) -> FaceSummary {
        run(m, v, a, p, s)
    }
}
fn fixture() -> Fixture {
    let mut faces: Vec<_> = (0..16)
        .map(|i| Face {
            first: i * 3,
            count: 3,
            group: i % 3,
            tex: i + 30,
            flags: 0,
            center: [-32769, 32768, 71],
            radius: 65537,
        })
        .collect();
    faces[0].group = 2;
    faces[1].group = 1;
    faces[1].flags = 127;
    faces[2].count = 0;
    faces[3].first = 65536;
    faces[4].count = 16384;
    faces[5].group = 999;
    faces[6].group = 2;
    let marks = vec![0, 1, 2, 0, 5, 1, 3, 4, 6, 15, 65535];
    Fixture {
        n_visleaves: 3,
        n_marks: marks.len(),
        leaves: vec![(0, 0, 0), (0, 0, 5), (0, 5, 4), (0, 9, 99)],
        marks,
        faces,
    }
}
fn pair(
    m: &Fixture,
    vis: &[u8],
    animated: &[u32],
    previous: usize,
    storage: &Storage,
) -> (FaceSummary, Storage) {
    let mut old = storage.clone();
    let a = legacy::compile(m, vis, animated, previous, &mut old);
    let mut new = storage.clone();
    let b = compile_faces(m, vis, animated, previous, new.view());
    assert_eq!(a, b);
    assert_eq!(old, new);
    (b, new)
}
#[test]
fn exact_first_seen_group_order_head_insertion_and_partial_records() {
    let m = fixture();
    let (sum, s) = pair(&m, &[3], &[0, 1 << 4], 0, &Storage::new(16, 4, 1));
    assert_eq!(sum.leaf_count, 2);
    assert_eq!(sum.face_count, 3);
    assert_eq!(sum.group_count, 2);
    assert_eq!(&s.indices[..3], &[0, 1, 6]);
    assert_eq!(&s.active_groups[..2], &[2, 1]);
    assert_eq!(s.group_first[2], 2);
    assert_eq!(&s.next[..3], &[PVS_LINK_END, PVS_LINK_END, 0]);
    assert!(sum.has_translucent);
    assert!(sum.has_texture_animation);
    assert_eq!(sum.triangle_references, 9);
    assert_eq!(s.records.len(), 1);
    assert_eq!(s.animated_textures, [0, 1 << 4]);
}
#[test]
fn primary_partner_and_underwater_union_recompile_matches_legacy() {
    let m = fixture();
    let mut s = Storage::new(16, 4, 16);
    let mut previous = 0;
    for bits in [1u8, 1 | 2, 1 | 2 | 4, 2, 4, 1 | 2, 0] {
        let (summary, next) = pair(&m, &[bits], &[u32::MAX; 2], previous, &s);
        previous = summary.group_count;
        s = next;
    }
    assert_eq!(previous, 0);
    assert!(s.group_face.iter().all(|x| *x == PVS_LINK_END));
}
#[test]
fn zero_small_and_maximum_link_capacities_and_truncated_marks() {
    let m = fixture();
    for (f, g, r) in [
        (0, 0, 0),
        (1, 1, 0),
        (3, 2, 1),
        (16, 4, 16),
        (65535, 65535, 0),
    ] {
        pair(&m, &[7], &[0; 2], 0, &Storage::new(f, g, r));
    }
}
#[test]
fn record_flags_narrowing_and_band_mutation_are_exact() {
    let m = fixture();
    for i in 0..m.faces.len() {
        let mut rec = build_pvs_face_rec(&m, i);
        let old = legacy::legacy_record(&m, i);
        assert_eq!(rec, old);
        let original = rec.meta;
        rec.set_band(255);
        assert_eq!(rec.band(), 7);
        assert_eq!(rec.meta & !7, original & !7);
        assert_eq!(rec.center, [32767, -32768, 71]);
        assert_eq!(rec.radius, 1);
    }
    let r = build_pvs_face_rec(&m, 1);
    assert_eq!(r.count, 3 | PVS_FACE_SPECIAL | PVS_FACE_CUTOUT);
    assert_eq!(r.meta, 0xf8);
    assert!(
        r.is_loop() && r.is_patch() && r.liquid() && r.refined_topology() && r.coplanar_backdrop()
    );
    assert_eq!(core::mem::size_of::<PvsFaceRec>(), 14);
}
#[test]
fn exhaustive_visibility_subsets_and_material_flags() {
    let mut m = fixture();
    for flags in 0..128 {
        m.faces[0].flags = flags;
        for vis in 0..8 {
            pair(
                &m,
                &[vis],
                &[0x55555555, 0xaaaaaaaa],
                0,
                &Storage::new(16, 4, 4),
            );
        }
    }
}

#[test]
fn last_representable_links_and_record_layout_match_the_old_declaration() {
    let mut m = fixture();
    let face = m.faces[0].clone();
    m.faces.resize(65535, face);
    m.faces[65534].group = 65534;
    m.faces[65534].first = 65535;
    m.faces[65534].count = 16383;
    m.marks = vec![65534, 65534, 65535];
    m.n_marks = 3;
    m.n_visleaves = 1;
    m.leaves = vec![(0, 0, 0), (0, 0, 3)];
    let (sum, out) = pair(&m, &[1], &[0; 2], 0, &Storage::new(65535, 65535, 1));
    assert_eq!(sum.face_count, 1);
    assert_eq!(sum.triangle_references, 16383);
    assert_eq!(out.indices[0], 65534);
    assert_eq!(out.active_groups[0], 65534);
    assert_eq!(out.group_face[65534], 65534 & 0x7fff);
    assert_eq!(out.records[0].first, 65535);
    assert_eq!(out.records[0].count(), 16383);
    struct OldLayout {
        first: u16,
        count: u16,
        center: [i16; 3],
        radius: u16,
        tex: u8,
        meta: u8,
    }
    assert_eq!(
        core::mem::size_of::<OldLayout>(),
        core::mem::size_of::<PvsFaceRec>()
    );
    assert_eq!(
        core::mem::align_of::<OldLayout>(),
        core::mem::align_of::<PvsFaceRec>()
    );
    macro_rules! same_offset {
        ($field:ident) => {
            assert_eq!(
                core::mem::offset_of!(OldLayout, $field),
                core::mem::offset_of!(PvsFaceRec, $field)
            );
        };
    }
    same_offset!(first);
    same_offset!(count);
    same_offset!(center);
    same_offset!(radius);
    same_offset!(tex);
    same_offset!(meta);
}
