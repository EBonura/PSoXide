//! Region graphs and routes for the simulator.

use alloc::collections::BinaryHeap;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;

/// Ticks per second the simulator's clocks run at (60 Hz NTSC).
pub const TICKS_PER_SECOND: u32 = 60;

/// One region: how big it is and where the cooker put it on the disc.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SimRegion {
    /// Sectors (and pages: one page per sector).
    pub sectors: u32,
    /// First sector on the disc.
    pub lba: u32,
}

/// Regions, the walk distance between neighbours, and for each region the set
/// of regions that must be resident while the player is in it (the cooker's
/// closure `V(R)`).
#[derive(Clone, Debug, Default)]
pub struct RegionGraph {
    regions: Vec<SimRegion>,
    edges: Vec<Vec<(u32, u32)>>,
    needs: Vec<Vec<u32>>,
}

impl RegionGraph {
    /// An empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// A chain of `count` regions of `sectors` sectors, neighbours `length`
    /// units apart, each needing itself only until [`Self::needs_with_neighbors`].
    pub fn chain(count: u32, sectors: u32, length: u32) -> Self {
        let mut graph = Self::new();
        for _ in 0..count {
            graph.add_region(sectors);
        }
        for region in 1..count {
            graph.connect(region - 1, region, length);
        }
        graph.layout_in_order(0, 0);
        graph
    }

    /// A `width` x `height` grid, row-major on disc, neighbours `length` apart.
    pub fn grid(width: u32, height: u32, sectors: u32, length: u32) -> Self {
        let mut graph = Self::new();
        for _ in 0..width * height {
            graph.add_region(sectors);
        }
        for y in 0..height {
            for x in 0..width {
                let here = y * width + x;
                if x + 1 < width {
                    graph.connect(here, here + 1, length);
                }
                if y + 1 < height {
                    graph.connect(here, here + width, length);
                }
            }
        }
        graph.layout_in_order(0, 0);
        graph
    }

    /// Add a region; returns its id (ids are dense from 0). Its disc position
    /// is set by [`Self::layout_in_order`] or [`Self::place`].
    pub fn add_region(&mut self, sectors: u32) -> u32 {
        self.regions.push(SimRegion { sectors, lba: 0 });
        self.edges.push(Vec::new());
        let id = self.regions.len() as u32 - 1;
        self.needs.push(vec![id]);
        id
    }

    /// Put `region` at sector `lba`.
    pub fn place(&mut self, region: u32, lba: u32) {
        self.regions[region as usize].lba = lba;
    }

    /// Lay regions out in id order from `start_lba`, `gap` sectors apart.
    pub fn layout_in_order(&mut self, start_lba: u32, gap: u32) {
        let mut lba = start_lba;
        for region in &mut self.regions {
            region.lba = lba;
            lba += region.sectors + gap;
        }
    }

    /// Connect two regions both ways, `length` units apart.
    pub fn connect(&mut self, a: u32, b: u32, length: u32) {
        self.edges[a as usize].push((b, length));
        self.edges[b as usize].push((a, length));
    }

    /// Set the regions that must be resident while the player is in `region`.
    pub fn set_needs(&mut self, region: u32, needs: &[u32]) {
        self.needs[region as usize] = needs.to_vec();
    }

    /// Every region needs itself and its direct neighbours.
    pub fn needs_with_neighbors(&mut self) {
        for region in 0..self.regions.len() {
            let mut needs = vec![region as u32];
            needs.extend(self.edges[region].iter().map(|&(to, _)| to));
            self.needs[region] = needs;
        }
    }

    /// Number of regions.
    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    /// Region `id`.
    pub fn region(&self, id: u32) -> SimRegion {
        self.regions[id as usize]
    }

    /// Regions needed while the player is in `region`.
    pub fn needs(&self, region: u32) -> &[u32] {
        &self.needs[region as usize]
    }

    /// Edge length between two neighbours.
    pub fn edge_length(&self, a: u32, b: u32) -> Option<u32> {
        self.edges[a as usize]
            .iter()
            .find(|&&(to, _)| to == b)
            .map(|&(_, length)| length)
    }

    /// Total sectors of all regions.
    pub fn total_sectors(&self) -> u64 {
        self.regions.iter().map(|r| r.sectors as u64).sum()
    }

    /// Walk distance from `from` to every region (Dijkstra over edge
    /// lengths); `u32::MAX` for unreachable regions.
    pub fn distances_from(&self, from: u32) -> Vec<u32> {
        let mut best = vec![u32::MAX; self.regions.len()];
        let mut heap = BinaryHeap::new();
        best[from as usize] = 0;
        heap.push(Reverse((0u32, from)));
        while let Some(Reverse((distance, region))) = heap.pop() {
            if distance > best[region as usize] {
                continue;
            }
            for &(to, length) in &self.edges[region as usize] {
                let next = distance.saturating_add(length);
                if next < best[to as usize] {
                    best[to as usize] = next;
                    heap.push(Reverse((next, to)));
                }
            }
        }
        best
    }
}

/// The player is in `region` from `tick` on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RouteSample {
    /// Tick the player enters the region.
    pub tick: u32,
    /// The region.
    pub region: u32,
}

/// Positions over time: a list of region entries in tick order.
#[derive(Clone, Debug, Default)]
pub struct Route {
    samples: Vec<RouteSample>,
    end_tick: u32,
}

impl Route {
    /// A route from explicit samples (sorted by tick) ending at `end_tick`.
    pub fn from_samples(samples: Vec<RouteSample>, end_tick: u32) -> Self {
        debug_assert!(samples.windows(2).all(|w| w[0].tick <= w[1].tick));
        Self { samples, end_tick }
    }

    /// Walk `path` (consecutive regions must be neighbours) at
    /// `units_per_second`, entering each region when the previous edge is
    /// covered; the route ends one second after the last entry.
    pub fn walk(graph: &RegionGraph, path: &[u32], units_per_second: u32) -> Self {
        let mut samples = Vec::new();
        let mut distance = 0u64;
        for (index, &region) in path.iter().enumerate() {
            if index > 0 {
                distance += graph
                    .edge_length(path[index - 1], region)
                    .expect("route steps must follow edges") as u64;
            }
            let tick = distance * TICKS_PER_SECOND as u64 / units_per_second.max(1) as u64;
            samples.push(RouteSample {
                tick: tick as u32,
                region,
            });
        }
        let end_tick = samples.last().map_or(0, |s| s.tick) + TICKS_PER_SECOND;
        Self { samples, end_tick }
    }

    /// The samples.
    pub fn samples(&self) -> &[RouteSample] {
        &self.samples
    }

    /// Last tick (exclusive).
    pub fn end_tick(&self) -> u32 {
        self.end_tick
    }

    /// The region the player is in at `tick`.
    pub fn region_at(&self, tick: u32) -> Option<u32> {
        let at = self.samples.partition_point(|s| s.tick <= tick);
        at.checked_sub(1).map(|i| self.samples[i].region)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dijkstra_follows_edge_lengths() {
        let mut graph = RegionGraph::new();
        for _ in 0..4 {
            graph.add_region(1);
        }
        graph.connect(0, 1, 10);
        graph.connect(1, 2, 10);
        graph.connect(0, 2, 50);
        let distances = graph.distances_from(0);
        assert_eq!(&distances[..3], &[0, 10, 20]);
        assert_eq!(distances[3], u32::MAX);
    }

    #[test]
    fn grid_and_layout() {
        let mut graph = RegionGraph::grid(3, 2, 8, 100);
        assert_eq!(graph.region_count(), 6);
        assert_eq!(graph.region(4).lba, 32);
        graph.layout_in_order(1000, 2);
        assert_eq!(graph.region(1).lba, 1010);
        graph.needs_with_neighbors();
        assert_eq!(graph.needs(0), &[0, 1, 3]);
        assert_eq!(graph.distances_from(0)[5], 300);
    }

    #[test]
    fn a_walk_enters_regions_at_the_right_ticks() {
        let graph = RegionGraph::chain(4, 4, 300);
        // 300 units/s: one edge per second.
        let route = Route::walk(&graph, &[0, 1, 2, 3], 300);
        let ticks: Vec<u32> = route.samples().iter().map(|s| s.tick).collect();
        assert_eq!(ticks, [0, 60, 120, 180]);
        assert_eq!(route.region_at(59), Some(0));
        assert_eq!(route.region_at(60), Some(1));
        assert_eq!(route.region_at(10_000), Some(3));
        assert_eq!(route.end_tick(), 240);
    }
}
