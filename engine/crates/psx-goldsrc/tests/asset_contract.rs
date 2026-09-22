#![allow(static_mut_refs)]
use psx_goldsrc::{model_variant, texture_animation as animation};
mod legacy {
    pub struct Map {
        pub n_tex_anim: usize,
        pub chains: &'static [(&'static [u8], &'static [u8])],
    }
    impl Map {
        fn tex_anim_chain(&self, i: usize) -> (&'static [u8], &'static [u8]) {
            self.chains[i]
        }
    }
    pub static mut SIM_NOW: u16 = 0;
    pub static mut TEX_ANIM_TENTH: u16 = u16::MAX;
    pub static mut TEX_ANIM_GEN: u8 = 0;
    pub static mut PVS_TEX_ANIM_GEN: u8 = 0;
    pub static mut VISIBLE: [u32; 8] = [0; 8];
    pub static mut ROWS: [[u8; 256]; 2] = [[0; 256]; 2];
    unsafe fn pvs_has_texanim(id: usize) -> bool {
        VISIBLE[id >> 5] & (1 << (id & 31)) != 0
    }
    mod map {
        pub fn tex_anim_frame_index(tenth: u16, count: usize) -> usize {
            if count == 0 {
                0
            } else {
                (tenth as usize / 2) % count
            }
        }
        pub fn tex_anim_set(id: usize, a: u8, b: u8) -> bool {
            unsafe {
                let i = id & 255;
                let changed = super::ROWS[0][i] != a || super::ROWS[1][i] != b;
                super::ROWS[0][i] = a;
                super::ROWS[1][i] = b;
                changed
            }
        }
    }
    include!("oracles/legacy-texture-tick.rs");
    pub unsafe fn run(m: &Map) {
        tex_anim_tick(m)
    }
}
fn old_lookup(
    offsets: &[u16],
    bytes: &[u8],
    type_count: usize,
    map_index: usize,
    ty: usize,
) -> Option<u32> {
    if map_index + 1 >= offsets.len() || ty >= type_count {
        return None;
    }
    let mut entry = offsets[map_index] as usize;
    let end = offsets[map_index + 1] as usize;
    while entry < end {
        let offset = entry * 3;
        if offset + 2 >= bytes.len() {
            return None;
        }
        let entry_ty = bytes[offset] as usize;
        if entry_ty == ty {
            return Some(u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]) as u32);
        }
        if entry_ty > ty {
            return None;
        }
        entry += 1;
    }
    None
}

#[test]
fn texture_driver_matches_frozen_source_over_entire_simulation_clock() {
    let configurations: &[&[(&[u8], &[u8])]] = &[
        &[],
        &[(&[], &[])],
        &[(&[0], &[])],
        &[(&[], &[255])],
        &[(&[1, 2, 3], &[4, 5]), (&[250, 251], &[])],
        &[(&[1, 1, 2], &[2, 3]), (&[3, 4], &[1])],
    ];
    for &chains in configurations {
        let chains: Vec<(&'static [u8], &'static [u8])> = chains
            .iter()
            .map(|&(a, b)| {
                (
                    &*Box::leak(a.to_vec().into_boxed_slice()),
                    &*Box::leak(b.to_vec().into_boxed_slice()),
                )
            })
            .collect();
        let chains = Box::leak(chains.into_boxed_slice());
        let m = legacy::Map {
            n_tex_anim: chains.len(),
            chains,
        };
        let mut rows = [[0; 256]; 2];
        for row in &mut rows {
            for (i, x) in row.iter_mut().enumerate() {
                *x = i as u8;
            }
        }
        let mut last = u16::MAX;
        let mut generation = 254;
        let mut visible_generation = 254;
        let mut visible = [0u32; 8];
        visible[0] = 1 << 2;
        visible[7] = 1 << 31;
        unsafe {
            legacy::ROWS = rows;
            legacy::VISIBLE = visible;
            legacy::TEX_ANIM_TENTH = last;
            legacy::TEX_ANIM_GEN = generation;
            legacy::PVS_TEX_ANIM_GEN = visible_generation;
        }
        let mut mask = [0; 8];
        animation::mark_members(chains.iter().copied(), &mut mask);
        for id in 0..256 {
            assert_eq!(
                mask[id >> 5] & (1 << (id & 31)) != 0,
                chains
                    .iter()
                    .any(|(a, b)| a.contains(&(id as u8)) || b.contains(&(id as u8)))
            );
        }
        for now in (0..=u16::MAX).chain([0, 0, 1, 4, 4, 65535, 0]) {
            unsafe {
                legacy::SIM_NOW = now;
                legacy::run(&m);
            }
            animation::tick(
                chains.iter().copied(),
                now,
                &mut last,
                &mut generation,
                &mut visible_generation,
                &visible,
                |i, a, b| {
                    let change = rows[0][i] != a || rows[1][i] != b;
                    rows[0][i] = a;
                    rows[1][i] = b;
                    change
                },
            );
            unsafe {
                assert_eq!(rows, legacy::ROWS);
                assert_eq!(
                    (last, generation, visible_generation),
                    (
                        legacy::TEX_ANIM_TENTH,
                        legacy::TEX_ANIM_GEN,
                        legacy::PVS_TEX_ANIM_GEN
                    )
                );
            }
        }
    }
}
#[test]
fn sparse_decoder_preserves_empty_truncated_and_sorted_row_behavior() {
    let bytes = [0, 0x34, 0x12, 2, 0xff, 0xab, 5, 0, 0x80, 1, 0x78, 0x56];
    for offsets in [&[0, 0, 3, 4][..], &[0, 3, 3, 4], &[0, 9], &[], &[0]] {
        for end in 0..=bytes.len() {
            for map in 0..6 {
                for ty in 0..=256 {
                    assert_eq!(
                        model_variant::lookup(offsets, &bytes[..end], map, ty, 256),
                        old_lookup(offsets, &bytes[..end], 256, map, ty)
                    );
                }
            }
        }
    }
    assert_eq!(
        model_variant::lookup(&[0, 3], &bytes, 0, 0, 6),
        Some(0x1234)
    );
    assert_eq!(
        model_variant::lookup(&[0, 3], &bytes, 0, 2, 6),
        Some(0xabff)
    );
}

#[test]
fn parameterized_lookup_oracle_is_only_a_table_access_adapter() {
    let frozen = include_str!("oracles/legacy-model-variant.rs");
    let expected = frozen
        .replace("fn map_model_variant_chunk(map_index: usize, ty: usize)",
                 "fn old_lookup(offsets: &[u16], bytes: &[u8], type_count: usize, map_index: usize, ty: usize)")
        .replace("    let offsets = &room_budget::MODEL_VARIANT_OFFSETS;\n", "")
        .replace("        let bytes = &room_budget::MODEL_VARIANT_BYTES;\n", "")
        .replace("N_MODEL_TYPES", "type_count");
    let tests = include_str!("asset_contract.rs");
    let start = tests.find("fn old_lookup(").unwrap();
    let end = start + tests[start..].find("\n}").unwrap() + 2;
    let normalize = |s: &str| {
        s.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .replace(",)", ")")
    };
    assert_eq!(normalize(&expected), normalize(&tests[start..end]));
}
