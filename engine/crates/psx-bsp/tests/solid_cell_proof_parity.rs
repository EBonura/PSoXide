//! Frozen generic checked-i128 oracle, independent of bounded runtime math.
pub use psx_bsp::{CompactPlane, Vec3I16, Vec3I32};
#[path = "fixtures/solid_cell_proof_checked.rs"]
mod checked;
use psx_bsp::solid_cell_proof::reduce_live;
#[test]
fn bounded_matches_frozen_checked_reduction_and_malformed_fallbacks() {
    let mut seed = 0x7a937c12u32;
    let mut random = || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed
    };
    let mut accepted = 0;
    for case in 0..30000 {
        let size = case % 3 + 1;
        let mut planes = Vec::new();
        let mut total = [0i32; 3];
        let mut bound = 0i32;
        for _ in 0..size {
            let n = core::array::from_fn::<_, 3, _>(|_| (random() % 16385) as i32 - 8192);
            let b = (random() % 200001) as i32 - 100000;
            for i in 0..3 {
                total[i] += n[i];
            }
            bound += b;
            planes.push(CompactPlane {
                normal: Vec3I16 {
                    x: n[0] as i16,
                    y: n[1] as i16,
                    z: n[2] as i16,
                },
                kind: 3,
                sign_bits: random() as u8,
                distance: b,
            });
        }
        // Sum of supports is a genuine implication (including strictness).
        planes.insert(
            0,
            CompactPlane {
                normal: Vec3I16 {
                    x: total[0] as i16,
                    y: total[1] as i16,
                    z: total[2] as i16,
                },
                kind: 3,
                sign_bits: 0,
                distance: bound,
            },
        );
        let original: Vec<_> = (0..=size).map(|i| ((i as u16) << 1) | 1).collect();
        let mut bytes = vec![(size + 1) as u8, 1];
        for i in 1..=size {
            bytes.push(i as u8 | if i == size { 128 } else { 0 });
        }
        if case % 11 == 0 {
            planes[0].distance = planes[0].distance.wrapping_sub(1);
        }
        if case % 17 == 0 {
            planes[0].kind = (random() % 3) as u8;
        }
        if case % 23 == 0 {
            bytes.push(0);
        }
        let domain = [[-1024; 3], [1024; 3]];
        let mut a = original.clone();
        let mut b = original.clone();
        let x = checked::reduce_live(&planes, &mut a, &domain, &bytes);
        let y = reduce_live(&planes, &mut b, &domain, &bytes);
        assert_eq!((x, &a), (y, &b), "case{case}");
        accepted += usize::from(x.is_some());
    }
    assert!(
        accepted > 20000,
        "must exercise successful exact proofs, got{accepted}"
    );
}
