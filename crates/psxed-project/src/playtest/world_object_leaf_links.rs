//! Static registry links derived from the exact runtime AABB/PVS predicate.
use super::*;
use psx_bsp::{pxbsp_resident::PxbspResidentMap, SliceReader, Vec3I32};

pub(super) fn cook_world_object_leaf_links(
    world: &PlaytestPxbspWorld,
    objects: &[PlaytestWorldObject],
) -> Result<(Vec<[u16; 2]>, Vec<u16>, Vec<u16>), String> {
    let mut map = PxbspResidentMap::default();
    map.load(0, &mut SliceReader::new(&world.bytes))
        .map_err(|error| format!("static object leaf links: {error:?}"))?;
    let domain = map
        .brush_models()
        .get(0)
        .map(|m| m.visible_leaves as usize)
        .unwrap_or(0);
    let mut row = vec![0u8; domain.div_ceil(8)];
    let mut ranges = Vec::with_capacity(objects.len());
    let mut packed = Vec::new();
    let mut deduplicated = HashMap::<Vec<u16>, [u16; 2]>::new();
    let q12 = |p: [i32; 3]| Vec3I32 {
        x: p[0].saturating_mul(4096),
        y: p[1].saturating_mul(4096),
        z: p[2].saturating_mul(4096),
    };
    for object in objects {
        let (min, max) = (q12(object.bounds_min), q12(object.bounds_max));
        let leaves = collect_leaf_links(domain, |row| {
            map.aabb_touches_visible_leaf(min, max, row, domain)
        });
        ranges.push(pack_leaf_links(leaves, &mut packed, &mut deduplicated));
    }
    // Validate the generated table against the old predicate for every world
    // observer leaf. Plane ties and straddling boxes inherit its exact policy.
    let mut comparisons = 0usize;
    for observer in 1..=domain {
        row.fill(0);
        let visible = map
            .leaf_visibility_into(observer, &mut row)
            .ok_or_else(|| format!("missing PVS row for leaf {observer}"))?;
        for (index, object) in objects.iter().enumerate() {
            let [first, count] = ranges[index];
            let expected = map.aabb_touches_visible_leaf(
                q12(object.bounds_min),
                q12(object.bounds_max),
                &row,
                visible,
            );
            let linked = if count == u16::MAX {
                expected
            } else {
                packed[usize::from(first)..usize::from(first) + usize::from(count)]
                    .iter()
                    .any(|&leaf| {
                        let leaf = usize::from(leaf);
                        leaf != 0
                            && leaf <= visible
                            && row[(leaf - 1) >> 3] & (1 << ((leaf - 1) & 7)) != 0
                    })
            };
            if expected != linked {
                return Err(format!(
                    "static object leaf-link mismatch: object {index}, observer leaf {observer}"
                ));
            }
            comparisons += 1;
        }
    }
    eprintln!("[cook-playtest] Static object leaf links: {} objects, {} packed leaves, {} bytes, {} fallbacks; {} observer/object parity checks passed", objects.len(), packed.len(), ranges.len() * 4 + packed.len() * 2, ranges.iter().filter(|r| r[1] == u16::MAX).count(), comparisons);
    let mut parents = vec![0u16; map.nodes().len()];
    if !map
        .model_collision_hull(0, 0)
        .is_some_and(|h| h.build_solid_cell_parents(&mut parents))
    {
        parents.clear();
    }
    eprintln!(
        "[cook-playtest] Certified solid-cell parents: {} nodes, {} bytes",
        parents.len(),
        parents.len() * 2
    );
    Ok((ranges, packed, parents))
}

// Derive the union using the authoritative bounds predicate. An empty row
// answering true means its conservative fail-open path was reached.
fn collect_leaf_links(domain: usize, mut touches: impl FnMut(&[u8]) -> bool) -> Option<Vec<u16>> {
    let mut row = vec![0u8; domain.div_ceil(8)];
    if touches(&row) {
        return None;
    }
    let mut leaves = Vec::new();
    for index in 0..domain {
        row[index >> 3] = 1 << (index & 7);
        if touches(&row) {
            leaves.push((index + 1) as u16);
        }
        row[index >> 3] = 0;
    }
    Some(leaves)
}

fn pack_leaf_links(
    leaves: Option<Vec<u16>>,
    packed: &mut Vec<u16>,
    deduplicated: &mut HashMap<Vec<u16>, [u16; 2]>,
) -> [u16; 2] {
    let Some(leaves) = leaves else {
        return [0, u16::MAX];
    };
    if let Some(range) = deduplicated.get(&leaves) {
        return *range;
    }
    if packed.len() + leaves.len() > usize::from(u16::MAX) || leaves.len() >= usize::from(u16::MAX)
    {
        return [0, u16::MAX];
    }
    let range = [packed.len() as u16, leaves.len() as u16];
    packed.extend_from_slice(&leaves);
    deduplicated.insert(leaves, range);
    range
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_query_failure_keeps_a_fallback_instead_of_an_empty_link() {
        assert_eq!(collect_leaf_links(9, |_| true), None);
        assert_eq!(collect_leaf_links(9, |_| false), Some(vec![]));
        assert_eq!(
            collect_leaf_links(9, |row| row[0] & 128 != 0 || row[1] & 1 != 0),
            Some(vec![8, 9])
        );
        let mut packed = Vec::new();
        let mut dedup = HashMap::new();
        assert_eq!(
            pack_leaf_links(None, &mut packed, &mut dedup),
            [0, u16::MAX]
        );
        assert!(packed.is_empty());
        assert_eq!(
            pack_leaf_links(Some(vec![]), &mut packed, &mut dedup),
            [0, 0]
        );
    }

    #[test]
    fn packed_offset_overflow_falls_back_and_duplicates_remain_reusable() {
        let mut packed = Vec::new();
        let mut dedup = HashMap::new();
        assert_eq!(
            pack_leaf_links(Some(vec![1, 9]), &mut packed, &mut dedup),
            [0, 2]
        );
        packed.resize(usize::from(u16::MAX) - 1, 1);
        let before = packed.len();
        assert_eq!(
            pack_leaf_links(Some(vec![3, 4]), &mut packed, &mut dedup),
            [0, u16::MAX]
        );
        assert_eq!(packed.len(), before);
        assert_eq!(
            pack_leaf_links(Some(vec![1, 9]), &mut packed, &mut dedup),
            [0, 2]
        );
        assert_eq!(packed.len(), before);
    }
}
