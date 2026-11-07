//! Deciding what a break does: a list of small operations, each a pure
//! function of the seed and a world position.

use std::collections::BTreeSet;

use crate::grid::derive;
use crate::grid::derive_diag::fold_edge;
use crate::grid::sector::{GridSector, WallEdge};
use crate::grid::surface::{GridSplit, GridSurface, VOID_A, VOID_B};
use crate::grid::units::{CLICK_UNITS, FINE_UNITS};
use crate::grid::world::GridWorld;

use super::noise::{below, chance, hash, Op};
use super::BreakParams;

/// Floor or ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Surf {
    Floor,
    Ceiling,
}

/// One operation.
#[derive(Clone, Debug)]
pub(super) enum Plan {
    /// Drop (or raise) one triangle of a tile: a diagonal riser appears.
    Bite {
        cell: [i32; 2],
        surf: Surf,
        tri: usize,
        split: GridSplit,
        /// Grid units, negative lowers.
        delta: i32,
        /// A bite on a plateau edge (else a dent inside).
        lip: bool,
        /// The floor the dropped half now borders, for the material mix.
        from: Option<[i32; 2]>,
    },
    /// Move one lattice vertex of every listed cell.
    Vertex {
        at: [i32; 2],
        surf: Surf,
        delta: i32,
        lip: bool,
        cells: Vec<[i32; 2]>,
    },
    /// A ragged top on an open-topped closure wall.
    Top {
        cell: [i32; 2],
        edge: WallEdge,
        ends: [i16; 2],
        stub: bool,
    },
    /// A cell with nothing in it.
    Gap { cell: [i32; 2] },
}

impl Plan {
    /// The sectors the operation changes.
    pub(super) fn cells(&self) -> Vec<[i32; 2]> {
        match self {
            Self::Bite { cell, .. } | Self::Top { cell, .. } | Self::Gap { cell } => vec![*cell],
            Self::Vertex { cells, .. } => cells.clone(),
        }
    }
}

/// Chances in a thousand at intensity 100, per operation.
const RATE_LIP: u32 = 900;
const RATE_BITE_LIP: u32 = 600;
const RATE_TILT: u32 = 450;
const RATE_BITE_IN: u32 = 100;
const RATE_TOP: u32 = 700;
const RATE_STUB: u32 = 200;
const RATE_CEIL: u32 = 350;
const RATE_CEIL_CRACK: u32 = 80;
const RATE_GAP: u32 = 30;
/// Gaps only appear from this intensity on.
const GAP_FROM: u32 = 70;

fn scaled(rate: u32, intensity: u32) -> u32 {
    rate * intensity / 100
}

/// The corner of a sector, `[NW, NE, SE, SW]`, that sits on lattice vertex
/// `at`.
fn corner_at(cell: [i32; 2], at: [i32; 2]) -> usize {
    match (at[0] - cell[0], at[1] - cell[1]) {
        (0, 0) => 0,
        (1, 0) => 1,
        (1, 1) => 2,
        _ => 3,
    }
}

fn sector_at(world: &GridWorld, room: usize, cell: [i32; 2]) -> Option<&GridSector> {
    let r = world.rooms.get(room)?;
    r.local(cell).and_then(|[x, z]| r.sector(x, z))
}

/// The split and triangle whose triangle holds cardinal `edge`, for the two
/// diagonals.
fn bite_shapes(edge: WallEdge) -> [(GridSplit, usize); 2] {
    match edge {
        WallEdge::North => [(GridSplit::NwSe, 0), (GridSplit::NeSw, 0)],
        WallEdge::East => [(GridSplit::NwSe, 0), (GridSplit::NeSw, 1)],
        WallEdge::South => [(GridSplit::NwSe, 1), (GridSplit::NeSw, 1)],
        _ => [(GridSplit::NwSe, 1), (GridSplit::NeSw, 0)],
    }
}

struct Ctx<'a> {
    world: &'a GridWorld,
    room: usize,
    params: &'a BreakParams,
    intensity: u32,
    seed: u32,
    /// Fine steps at this intensity.
    cap: u32,
}

impl Ctx<'_> {
    /// A change of 1 to `cap` fine steps, rounded up to the snap step, grid
    /// units.
    fn amount(&self, op: Op, at: [i32; 2], salt: u32) -> i32 {
        let steps = 1 + below(self.seed, op, at, salt, self.cap) as i32;
        let step = self.params.snap.step();
        (steps * FINE_UNITS + step - 1) / step * step
    }

    fn sign(&self, op: Op, at: [i32; 2]) -> i32 {
        if hash(self.seed, op, at, 77) & 1 == 0 {
            -1
        } else {
            1
        }
    }

    fn floor(&self, cell: [i32; 2]) -> Option<&GridSurface> {
        sector_at(self.world, self.room, cell)?.ground_floor()
    }

    fn ceiling(&self, cell: [i32; 2]) -> Option<&GridSurface> {
        sector_at(self.world, self.room, cell)?.ground_ceiling()
    }
}

/// Heights `[a, b]` of a surface along the cardinal `edge` (the two corner
/// heights, from the triangle that holds it).
fn edge_heights(surface: &GridSurface, edge: WallEdge) -> [i32; 2] {
    let [a, b] = crate::grid::edit::walls::edge_corner_indices(edge);
    let tri = bite_shapes(edge)
        .iter()
        .position(|(s, _)| *s == surface.split)
        .map_or(0, |i| bite_shapes(edge)[i].1);
    let h = surface.heights_for(tri);
    [h[a], h[b]]
}

const CARDINALS: [(WallEdge, [i32; 2]); 4] = [
    (WallEdge::North, [0, -1]),
    (WallEdge::East, [1, 0]),
    (WallEdge::South, [0, 1]),
    (WallEdge::West, [-1, 0]),
];

/// Plan a break of `cells` (sorted, of `room`).
pub(super) fn plan(
    world: &GridWorld,
    room: usize,
    cells: &[[i32; 2]],
    params: &BreakParams,
    intensity: u32,
    seed: u32,
) -> Vec<Plan> {
    let cap = (params.amplitude.max(1) as u32 * intensity / 100).max(1);
    let cx = Ctx {
        world,
        room,
        params,
        intensity,
        seed,
        cap,
    };
    let floors: BTreeSet<[i32; 2]> = cells
        .iter()
        .copied()
        .filter(|c| cx.floor(*c).is_some())
        .collect();
    let roofs: BTreeSet<[i32; 2]> = cells
        .iter()
        .copied()
        .filter(|c| cx.ceiling(*c).is_some())
        .collect();
    let mut out = Vec::new();
    let t = params.targets;
    if t.floor || t.walls {
        bites(&cx, &floors, &mut out);
    }
    if t.floor && params.gaps {
        gaps(&cx, &floors, &mut out);
    }
    if t.floor || t.walls {
        vertices(&cx, &floors, &mut out);
    }
    if t.walls {
        tops(&cx, &floors, &mut out);
    }
    if t.ceiling {
        ceilings(&cx, &roofs, &mut out);
    }
    out
}

/// The cardinal sides of `cell` whose neighbour floor lies lower along the
/// whole shared edge, with how much (the smaller end difference).
fn lower_sides(
    cx: &Ctx<'_>,
    cell: [i32; 2],
    floor: &GridSurface,
) -> Vec<(WallEdge, [i32; 2], i32)> {
    let mut out = Vec::new();
    for (edge, d) in CARDINALS {
        let other = [cell[0] + d[0], cell[1] + d[1]];
        let Some(nf) = cx.floor(other) else { continue };
        let opposite = edge.opposite().map_or(edge, |(e, _)| e);
        let mine = edge_heights(floor, edge);
        let mut theirs = edge_heights(nf, opposite);
        // The neighbour's profile runs the other way along the same line.
        theirs.reverse();
        let diff = (mine[0] - theirs[0]).min(mine[1] - theirs[1]);
        if diff >= FINE_UNITS {
            out.push((edge, other, diff));
        }
    }
    out
}

fn bites(cx: &Ctx<'_>, floors: &BTreeSet<[i32; 2]>, out: &mut Vec<Plan>) {
    let t = cx.params.targets;
    for &cell in floors {
        let Some(floor) = cx.floor(cell) else {
            continue;
        };
        if floor.heights_b.is_some() || floor.void != 0 {
            continue;
        }
        let sides = lower_sides(cx, cell, floor);
        if !sides.is_empty() {
            if t.walls
                && chance(
                    cx.seed,
                    Op::Crack,
                    cell,
                    scaled(RATE_BITE_LIP, cx.intensity),
                )
            {
                let (edge, other, diff) =
                    sides[below(cx.seed, Op::CrackShape, cell, 1, sides.len() as u32) as usize];
                let (split, tri) =
                    bite_shapes(edge)[below(cx.seed, Op::CrackShape, cell, 2, 2) as usize];
                let drop = cx
                    .amount(Op::CrackDrop, cell, 0)
                    .min(diff / FINE_UNITS * FINE_UNITS);
                let step = cx.params.snap.step();
                let drop = drop / step * step;
                if drop > 0 {
                    out.push(Plan::Bite {
                        cell,
                        surf: Surf::Floor,
                        tri,
                        split,
                        delta: -drop,
                        lip: true,
                        from: Some(other),
                    });
                }
            }
        } else if t.floor && chance(cx.seed, Op::Crack, cell, scaled(RATE_BITE_IN, cx.intensity)) {
            let (split, tri) =
                bite_shapes(WallEdge::North)[below(cx.seed, Op::CrackShape, cell, 2, 2) as usize];
            let tri = if hash(cx.seed, Op::CrackShape, cell, 3) & 1 == 0 {
                tri
            } else {
                1 - tri
            };
            out.push(Plan::Bite {
                cell,
                surf: Surf::Floor,
                tri,
                split,
                delta: cx.sign(Op::CrackDrop, cell) * cx.amount(Op::CrackDrop, cell, 0),
                lip: false,
                from: None,
            });
        }
    }
}

fn gaps(cx: &Ctx<'_>, floors: &BTreeSet<[i32; 2]>, out: &mut Vec<Plan>) {
    if cx.intensity < GAP_FROM {
        return;
    }
    for &cell in floors {
        let inside = CARDINALS
            .iter()
            .all(|(_, d)| floors.contains(&[cell[0] + d[0], cell[1] + d[1]]));
        if inside && chance(cx.seed, Op::Gap, cell, scaled(RATE_GAP, cx.intensity)) {
            out.push(Plan::Gap { cell });
        }
    }
}

/// The corners of the sectors around lattice vertex `at`: `(cell, corner)`.
fn around(at: [i32; 2]) -> [[i32; 2]; 4] {
    [
        [at[0] - 1, at[1] - 1],
        [at[0], at[1] - 1],
        [at[0] - 1, at[1]],
        [at[0], at[1]],
    ]
}

fn vertices(cx: &Ctx<'_>, floors: &BTreeSet<[i32; 2]>, out: &mut Vec<Plan>) {
    let t = cx.params.targets;
    let mut lattice: BTreeSet<[i32; 2]> = BTreeSet::new();
    for c in floors {
        for (dx, dz) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
            lattice.insert([c[0] + dx, c[1] + dz]);
        }
    }
    for at in lattice {
        // Every floor that touches the vertex, with its corner height there.
        let around: Vec<([i32; 2], i32)> = around(at)
            .into_iter()
            .filter_map(|c| Some((c, cx.floor(c)?.heights[corner_at(c, at)])))
            .collect();
        let Some(top) = around.iter().map(|(_, h)| *h).max() else {
            continue;
        };
        // The cells on the high side that the break may move.
        let movers: Vec<[i32; 2]> = around
            .iter()
            .filter(|(c, h)| *h == top && floors.contains(c))
            .map(|(c, _)| *c)
            .collect();
        if movers.is_empty() {
            continue;
        }
        let below = around
            .iter()
            .filter(|(_, h)| *h <= top - FINE_UNITS)
            .map(|(_, h)| *h)
            .max();
        if let Some(below) = below {
            // A plateau lip: the highest corner drops by up to the step to the
            // next level, so the riser's top follows and its height shrinks.
            if t.walls && chance(cx.seed, Op::Lip, at, scaled(RATE_LIP, cx.intensity)) {
                let step = cx.params.snap.step();
                let drop = cx
                    .amount(Op::VertexDelta, at, 1)
                    .min((top - below) / step * step);
                if drop > 0 {
                    out.push(Plan::Vertex {
                        at,
                        surf: Surf::Floor,
                        delta: -drop,
                        lip: true,
                        cells: movers,
                    });
                }
            }
            continue;
        }
        // Level all round: an interior vertex of the region tilts.
        let all_inside = around.len() == 4 && around.iter().all(|(c, _)| floors.contains(c));
        if t.floor && all_inside && chance(cx.seed, Op::Vertex, at, scaled(RATE_TILT, cx.intensity))
        {
            out.push(Plan::Vertex {
                at,
                surf: Surf::Floor,
                delta: cx.sign(Op::VertexDelta, at) * cx.amount(Op::VertexDelta, at, 0),
                lip: false,
                cells: movers,
            });
        }
    }
}

/// Where a closure wall's two ends are on the lattice, in the key's
/// direction.
fn segment_ends(cell: [i32; 2], edge: WallEdge) -> [[i32; 2]; 2] {
    let [x, z] = cell;
    match edge {
        WallEdge::North => [[x, z], [x + 1, z]],
        WallEdge::South => [[x, z + 1], [x + 1, z + 1]],
        WallEdge::West => [[x, z], [x, z + 1]],
        WallEdge::East => [[x + 1, z], [x + 1, z + 1]],
        WallEdge::Nwse => [[x, z], [x + 1, z + 1]],
        WallEdge::Nesw => [[x + 1, z], [x, z + 1]],
    }
}

fn tops(cx: &Ctx<'_>, floors: &BTreeSet<[i32; 2]>, out: &mut Vec<Plan>) {
    let Some(room) = cx.world.rooms.get(cx.room) else {
        return;
    };
    let open = room.defaults.open_wall;
    if open <= 0 {
        return;
    }
    let limit = (open - CLICK_UNITS).max(0);
    for &cell in floors {
        let Some(sector) = sector_at(cx.world, cx.room, cell) else {
            continue;
        };
        let Some(floor) = sector.ground_floor() else {
            continue;
        };
        let mut edges: Vec<WallEdge> = Vec::new();
        for (edge, d) in CARDINALS {
            let other = [cell[0] + d[0], cell[1] + d[1]];
            let neighbour_empty = sector_at(cx.world, cx.room, other)
                .is_none_or(|s| s.ground_floor().is_none() && s.ground_ceiling().is_none());
            // Closed walls take their top from the ceiling, not from here.
            let tri = bite_shapes(edge)
                .iter()
                .find(|(s, _)| *s == floor.split)
                .map_or(0, |(_, t)| *t);
            let void_side = floor.is_void(tri);
            if neighbour_empty && !void_side && sector.ground_ceiling().is_none() {
                edges.push(edge);
            }
        }
        if floor.void & 3 == VOID_A || floor.void & 3 == VOID_B {
            // The diagonal closure of a chamfered corner.
            let near = if floor.void & 3 == VOID_A { 1 } else { 0 };
            if sector.ground_ceiling().is_none_or(|c| c.is_void(near)) {
                edges.push(fold_edge(floor.split));
            }
        }
        for edge in edges {
            let [va, vb] = segment_ends(cell, edge);
            let end = |v: [i32; 2]| -> i32 {
                if chance(cx.seed, Op::Top, v, scaled(RATE_TOP, cx.intensity)) {
                    -cx.amount(Op::TopDrop, v, 0)
                } else {
                    0
                }
            };
            let (mut a, mut b) = (end(va), end(vb));
            let key = derive::edge_key(cell, edge);
            let seg = [key.x, key.z];
            let stub = chance(cx.seed, Op::Stub, seg, scaled(RATE_STUB, cx.intensity));
            if stub {
                let clicks = 1 + below(cx.seed, Op::Stub, seg, 5, 3) as i32;
                a -= clicks * CLICK_UNITS;
                b -= clicks * CLICK_UNITS;
            }
            let (a, b) = (a.max(-limit), b.max(-limit));
            if a == 0 && b == 0 {
                continue;
            }
            // The override stores the ends in the key's direction; the two
            // vertices above are in it for every edge but the diagonal NE-SW
            // and the cardinal ones already (see `segment_ends`).
            out.push(Plan::Top {
                cell,
                edge,
                ends: [a as i16, b as i16],
                stub,
            });
        }
    }
}

fn ceilings(cx: &Ctx<'_>, roofs: &BTreeSet<[i32; 2]>, out: &mut Vec<Plan>) {
    let mut lattice: BTreeSet<[i32; 2]> = BTreeSet::new();
    for c in roofs {
        for (dx, dz) in [(0, 0), (1, 0), (1, 1), (0, 1)] {
            lattice.insert([c[0] + dx, c[1] + dz]);
        }
    }
    for at in lattice {
        let cells: Vec<[i32; 2]> = around(at)
            .into_iter()
            .filter(|c| roofs.contains(c))
            .collect();
        if cells.is_empty() || !chance(cx.seed, Op::Ceil, at, scaled(RATE_CEIL, cx.intensity)) {
            continue;
        }
        out.push(Plan::Vertex {
            at,
            surf: Surf::Ceiling,
            delta: cx.sign(Op::VertexDelta, at) * cx.amount(Op::VertexDelta, at, 2),
            lip: false,
            cells,
        });
    }
    for &cell in roofs {
        let Some(c) = cx.ceiling(cell) else { continue };
        if c.heights_b.is_some()
            || c.void != 0
            || !chance(
                cx.seed,
                Op::Crack,
                cell,
                scaled(RATE_CEIL_CRACK, cx.intensity),
            )
        {
            continue;
        }
        let (split, tri) =
            bite_shapes(WallEdge::North)[below(cx.seed, Op::CrackShape, cell, 4, 2) as usize];
        out.push(Plan::Bite {
            cell,
            surf: Surf::Ceiling,
            tri,
            split,
            delta: cx.sign(Op::CrackDrop, cell) * cx.amount(Op::CrackDrop, cell, 5),
            lip: false,
            from: None,
        });
    }
}
