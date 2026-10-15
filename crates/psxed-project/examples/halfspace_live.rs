//! Host replay of exactly the metadata/decoder/live verifier used by the guest.
use psx_bsp::solid_cell_proof::{contains_integral_segment, entry};
use psx_bsp::{pxbsp_resident::PxbspResidentMap, SliceReader, Vec3I32};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).unwrap();
    let proofs = psxed_project::solid_cell_proofs::cook(&bytes);
    let mut map = PxbspResidentMap::default();
    map.load(0, &mut SliceReader::new(&bytes)).unwrap();
    let hull = map.model_collision_hull(0, 0).unwrap();
    let mut parents = vec![0; map.nodes().len()];
    assert!(hull.build_solid_cell_parents(&mut parents));
    let mut paths = std::collections::BTreeMap::new();
    let mut lines = String::new();
    for &[edge, _, _] in &proofs.index {
        let mut path = [0u16; 72];
        let n = hull
            .solid_cell_constraints_into(&parents, edge, &mut path)
            .unwrap();
        let count = hull
            .reduce_solid_cell_constraints(
                &mut path[..n],
                &proofs.domain,
                entry(&proofs.index, &proofs.data, edge).unwrap(),
            )
            .expect("cooked entry must validate live");
        lines += &format!(
            "{edge} {n} {}\n",
            path[..count]
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        paths.insert(edge, path[..count].to_vec());
    }
    std::fs::write(format!("{}-paths.txt", args[3]), lines).unwrap();
    std::fs::write(format!("{}-proof.bin", args[3]), &proofs.data).unwrap();
    let rays = std::fs::read(&args[2]).unwrap();
    let mut covered = 0;
    let mut hits = 0;
    for record in rays.chunks_exact(28) {
        let r: Vec<_> = record
            .chunks_exact(4)
            .map(|x| i32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        let edge = r[0] as u16;
        let a = Vec3I32 {
            x: r[1],
            y: r[2],
            z: r[3],
        };
        let b = Vec3I32 {
            x: r[4],
            y: r[5],
            z: r[6],
        };
        let old = hull.certifies_solid_cell(&parents, edge, &a, &b);
        let new = if let Some(path) = paths
            .get(&edge)
            .filter(|_| contains_integral_segment(&proofs.domain, &a, &b))
        {
            covered += 1;
            hull.segment_intersects_halfspaces(path, &a, &b)
        } else {
            old
        };
        assert_eq!(old, new, "ray {:?}", r);
        hits += usize::from(old);
    }
    println!(
        "entries={} resident_bytes={} rays={} covered={} successful={} domain={:?}",
        proofs.index.len(),
        proofs.resident_bytes(),
        rays.len() / 28,
        covered,
        hits,
        proofs.domain
    );
}
