//! Executable policy probes for the reference engine architecture findings whose workloads do
//! not exist in Cortex v1. These tests validate semantics and scaling
//! thresholds without adding premature runtime systems.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lod {
    Near,
    Far,
}

fn select_lod(depth: i32, current: Lod, switch: i32, hysteresis: i32, player: bool) -> Lod {
    if player {
        return Lod::Near;
    }
    match current {
        Lod::Near if depth > switch.saturating_add(hysteresis) => Lod::Far,
        Lod::Far if depth < switch.saturating_sub(hysteresis) => Lod::Near,
        _ => current,
    }
}

#[test]
fn model_lod_hysteresis_is_stable_and_never_reduces_player() {
    let mut lod = Lod::Near;
    for depth in [3900, 4100, 4000, 4200] {
        lod = select_lod(depth, lod, 4096, 256, false);
        assert_eq!(lod, Lod::Near);
    }
    lod = select_lod(4400, lod, 4096, 256, false);
    assert_eq!(lod, Lod::Far);
    lod = select_lod(4000, lod, 4096, 256, false);
    assert_eq!(lod, Lod::Far);
    lod = select_lod(3800, lod, 4096, 256, false);
    assert_eq!(lod, Lod::Near);
    assert_eq!(select_lod(i32::MAX, Lod::Near, 4096, 256, true), Lod::Near);
}

#[test]
fn prepared_visible_list_only_scales_after_multiple_global_passes() {
    const GLOBAL_INSTANCES: usize = 256;
    const VISIBLE_IN_ROOM: usize = 8;
    const CONSUMERS: usize = 3; // shadow, behind-player, in-front-player

    let repeated_global_tests = GLOBAL_INSTANCES * CONSUMERS;
    let prepared_tests = GLOBAL_INSTANCES + VISIBLE_IN_ROOM * CONSUMERS;
    assert_eq!(repeated_global_tests, 768);
    assert_eq!(prepared_tests, 280);
    assert!(prepared_tests < repeated_global_tests);

    // Cortex v1 has one placed instance. Building and consuming a stash does
    // not remove enough work to justify the extra state.
    let cortex_repeated = CONSUMERS;
    let cortex_prepared = 1 + CONSUMERS;
    assert!(cortex_prepared >= cortex_repeated);
}

fn active_entity_indices(
    rooms: &[u8],
    engaged: &[bool],
    active_room_mask: u64,
) -> Vec<usize> {
    rooms
        .iter()
        .zip(engaged)
        .enumerate()
        .filter_map(|(index, (&room, &awake))| {
            let room_active = room < 64 && active_room_mask & (1u64 << room) != 0;
            (awake || room_active).then_some(index)
        })
        .collect()
}

#[test]
fn packed_active_entity_selection_matches_gate_and_has_scaling_threshold() {
    let rooms = [0, 1, 2, 3, 4, 5, 6, 7];
    let engaged = [false, false, true, false, false, true, false, false];
    let selected = active_entity_indices(&rooms, &engaged, (1 << 1) | (1 << 4));
    assert_eq!(selected, vec![1, 2, 4, 5]);

    // A packed list wins only when the pool is materially larger than the
    // awake set; today's tiny population should keep the direct fixed scan.
    let large_pool_scan = 256usize;
    let packed_awake_walk = 16usize;
    assert!(packed_awake_walk * 8 < large_pool_scan);
}

#[derive(Clone, Copy)]
struct BudgetedSearch<const N: usize> {
    queue: [u8; N],
    head: usize,
    tail: usize,
    visited: u64,
}

impl<const N: usize> BudgetedSearch<N> {
    fn new(start: u8) -> Self {
        let mut queue = [0; N];
        queue[0] = start;
        Self {
            queue,
            head: 0,
            tail: 1,
            visited: 1u64 << start,
        }
    }

    fn advance(&mut self, graph: &[&[u8]], goal: u8, budget: usize) -> (bool, usize) {
        let mut expanded = 0;
        while self.head < self.tail && expanded < budget {
            let node = self.queue[self.head];
            self.head += 1;
            expanded += 1;
            if node == goal {
                return (true, expanded);
            }
            for &next in graph[node as usize] {
                let bit = 1u64 << next;
                if self.visited & bit == 0 && self.tail < N {
                    self.visited |= bit;
                    self.queue[self.tail] = next;
                    self.tail += 1;
                }
            }
        }
        (false, expanded)
    }
}

#[test]
fn incremental_search_never_exceeds_per_tick_expansion_budget() {
    let graph: &[&[u8]] = &[&[1, 2], &[3], &[3], &[4], &[5], &[]];
    let mut search = BudgetedSearch::<16>::new(0);
    let mut found = false;
    let mut turns = 0;
    while !found {
        let (done, expanded) = search.advance(graph, 5, 2);
        assert!(expanded <= 2);
        found = done;
        turns += 1;
        assert!(turns < 8);
    }
    assert!(turns >= 3);
}

#[repr(C)]
struct ScratchpadPlan {
    tessellation_vertices: [[i32; 3]; 9],
    leaf_descriptors: [[u32; 4]; 4],
    portal_queue: [[i16; 6]; 32],
    selected_lights: [[i32; 6]; 3],
}

#[test]
fn proposed_typed_scratchpad_regions_fit_one_kibibyte() {
    let bytes = core::mem::size_of::<ScratchpadPlan>();
    assert_eq!(bytes, 628);
    assert!(bytes <= 1024);
    assert!(core::mem::align_of::<ScratchpadPlan>() <= 4);
}
