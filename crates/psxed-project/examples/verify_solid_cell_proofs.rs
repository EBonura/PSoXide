//! Validate cooker wire output with the independent runtime verifier on a real BSP.
use psx_bsp::{pxbsp_resident::PxbspResidentMap, solid_cell_proof, SliceReader};
fn main() {
    let bytes = std::fs::read(std::env::args().nth(1).expect("BSP path")).unwrap();
    let start = std::time::Instant::now();
    let metadata = psxed_project::solid_cell_proofs::cook(&bytes);
    let mut map = PxbspResidentMap::with_capacity(bytes.len());
    map.load(0, &mut SliceReader::new(&bytes)).unwrap();
    let hull = map.model_collision_hull(0, 0).unwrap();
    let mut parents = vec![0u16; map.nodes().len()];
    assert!(hull.build_solid_cell_parents(&mut parents));
    let (mut before, mut after) = (0, 0);
    for &[edge, _, _] in &metadata.index {
        let mut path = [0u16; 72];
        let count = hull
            .solid_cell_constraints_into(&parents, edge, &mut path)
            .unwrap();
        let blob = solid_cell_proof::entry(&metadata.index, &metadata.data, edge).unwrap();
        let retained =
            solid_cell_proof::reduce_live(map.planes(), &mut path[..count], &metadata.domain, blob)
                .expect("cooker/runtime agreement");
        before += count;
        after += retained;
    }
    println!("entries={} bytes={} constraints_before={} constraints_after={} generation_and_verification_ms={}",metadata.index.len(),metadata.resident_bytes(),before,after,start.elapsed().as_millis());
}
