//! Portals between rooms, derived from the document and never placed by hand.
//!
//! A portal is a directed opening from one room into another. Two rules:
//!
//! * Side by side: the neighbouring lattice cell across a room's boundary
//!   edge belongs to another room and the open spans of the two cells meet
//!   at the shared edge, so a body can pass. Where neither cell has a
//!   ceiling the opening is one sector tall over the higher floor, so open
//!   ground split into rooms stays connected.
//! * Stacked: the rooms overlap in plan and one of the two cells has no
//!   floor or no ceiling, so there is a vertical opening between them.
//!
//! Cells that share an edge with the same neighbour, open span and normal are
//! merged into one longer portal, so a long shared wall costs one portal
//! instead of one per cell.
//!
//! The runtime uses the graph for activation (which rooms are near enough to
//! simulate) and room visibility; both only need which rooms connect.

use psx_grid::{Portal, PORTAL_VERTICAL};

use super::units::RuntimeUnits;
use crate::grid::room::GridRoom;
use crate::grid::world::GridWorld;

/// Lowest and highest open height of a cell at its edge corners, world
/// runtime units: from the higher floor corner to the lower ceiling corner.
fn open_span(
    units: &RuntimeUnits,
    room: &GridRoom,
    x: usize,
    z: usize,
    corners: [usize; 2],
) -> Option<(i32, i32)> {
    let sector = room.sector(x, z)?;
    let h = |g: i32| units.rt(room.y_offset + g);
    let lo = sector.ground_floor().map_or(i32::MIN, |f| {
        corners
            .iter()
            .map(|&c| h(f.heights[c]))
            .max()
            .unwrap_or(i32::MIN)
    });
    let hi = sector.ground_ceiling().map_or(i32::MAX, |c| {
        corners
            .iter()
            .map(|&k| h(c.heights[k]))
            .min()
            .unwrap_or(i32::MAX)
    });
    Some((lo, hi))
}

/// Directed portals per room, indexed like `world.rooms`.
pub fn derive(world: &GridWorld, units: &RuntimeUnits) -> Result<Vec<Vec<Portal>>, String> {
    let s = units.sector() as i32;
    let mut out: Vec<Vec<Portal>> = vec![Vec::new(); world.rooms.len()];
    // Neighbour offsets with the corner pair (into [NW, NE, SE, SW]) of the
    // edge on this side and on the far side, and the edge's corner positions.
    struct Side {
        d: [i32; 2],
        near: [usize; 2],
        far: [usize; 2],
        a: [i32; 2],
        b: [i32; 2],
        normal: [i16; 3],
    }
    let sides = [
        Side {
            d: [0, -1],
            near: [0, 1],
            far: [3, 2],
            a: [0, 0],
            b: [1, 0],
            normal: [0, 0, -4096],
        },
        Side {
            d: [1, 0],
            near: [1, 2],
            far: [0, 3],
            a: [1, 0],
            b: [1, 1],
            normal: [4096, 0, 0],
        },
        Side {
            d: [0, 1],
            near: [3, 2],
            far: [0, 1],
            a: [0, 1],
            b: [1, 1],
            normal: [0, 0, 4096],
        },
        Side {
            d: [-1, 0],
            near: [0, 3],
            far: [1, 2],
            a: [0, 0],
            b: [0, 1],
            normal: [-4096, 0, 0],
        },
    ];
    for (ra, room) in world.rooms.iter().enumerate() {
        let origin_y = units.rt(room.y_offset);
        // Open-air side portals, merged by their own rule.
        let mut air: Vec<Portal> = Vec::new();
        for (x, z, _) in room.iter() {
            let cell = room.world_cell(x, z);
            for side in &sides {
                let n = [cell[0] + side.d[0], cell[1] + side.d[1]];
                // Cells of the same room are not portals.
                if room
                    .local(n)
                    .is_some_and(|[nx, nz]| room.sector(nx, nz).is_some())
                {
                    continue;
                }
                let Some((a_lo, a_hi)) = open_span(units, room, x, z, side.near) else {
                    continue;
                };
                for (rb, other) in world.rooms.iter().enumerate() {
                    if rb == ra {
                        continue;
                    }
                    let Some([nx, nz]) = other.local(n) else {
                        continue;
                    };
                    let Some((b_lo, b_hi)) = open_span(units, other, nx, nz, side.far) else {
                        continue;
                    };
                    let lo = a_lo.max(b_lo);
                    if lo == i32::MIN {
                        continue;
                    }
                    // Open air on both sides: the opening is one sector tall
                    // over the higher floor (the walk reads only its centre).
                    let open_air = a_hi.min(b_hi) == i32::MAX;
                    let hi = if open_air { lo + s } else { a_hi.min(b_hi) };
                    if lo >= hi {
                        continue;
                    }
                    let (lo, hi) = (lo - origin_y, hi - origin_y);
                    let px = |u: i32| (x as i32 + u) * s;
                    let pz = |u: i32| (z as i32 + u) * s;
                    let c =
                        |v: i32| i16::try_from(v).map_err(|_| "portal out of range".to_string());
                    let (ax, az, bx, bz) =
                        (px(side.a[0]), pz(side.a[1]), px(side.b[0]), pz(side.b[1]));
                    let list = if open_air { &mut air } else { &mut out[ra] };
                    list.push(Portal {
                        dest: rb as u16,
                        flags: 0,
                        v: [
                            [c(ax)?, c(lo)?, c(az)?],
                            [c(bx)?, c(lo)?, c(bz)?],
                            [c(bx)?, c(hi)?, c(bz)?],
                            [c(ax)?, c(hi)?, c(az)?],
                        ],
                        normal: side.normal,
                        reserved: 0,
                    });
                }
            }
        }
        // Stacked rooms: overlapping cells with an opening above or below.
        for (x, z, sector) in room.iter() {
            let cell = room.world_cell(x, z);
            for (rb, other) in world.rooms.iter().enumerate() {
                if rb == ra {
                    continue;
                }
                let Some([nx, nz]) = other.local(cell) else {
                    continue;
                };
                let Some(neighbour) = other.sector(nx, nz) else {
                    continue;
                };
                let open_here =
                    sector.ground_floor().is_none() || sector.ground_ceiling().is_none();
                let open_there =
                    neighbour.ground_floor().is_none() || neighbour.ground_ceiling().is_none();
                if !(open_here || open_there) {
                    continue;
                }
                let y = sector
                    .ground_floor()
                    .or(sector.ground_ceiling())
                    .map_or(0, |f| units.rt(room.y_offset + f.heights[0]) - origin_y);
                let c = |v: i32| i16::try_from(v).map_err(|_| "portal out of range".to_string());
                let (x0, z0) = (x as i32 * s, z as i32 * s);
                let (x1, z1) = (x0 + s, z0 + s);
                out[ra].push(Portal {
                    dest: rb as u16,
                    flags: PORTAL_VERTICAL,
                    v: [
                        [c(x0)?, c(y)?, c(z0)?],
                        [c(x1)?, c(y)?, c(z0)?],
                        [c(x1)?, c(y)?, c(z1)?],
                        [c(x0)?, c(y)?, c(z1)?],
                    ],
                    normal: [0, if open_here { 4096 } else { -4096 }, 0],
                    reserved: 0,
                });
            }
        }
        merge_runs(&mut out[ra]);
        merge_open_air(&mut air);
        out[ra].append(&mut air);
    }
    Ok(out)
}

/// Join portals that continue each other into longer ones, until none do.
///
/// Two portals continue each other when they open into the same room, carry
/// the same flags and normal, and one's far edge is the other's near edge
/// (the quads share both end vertices of that edge). That holds for side
/// portals that meet along a wall at the same open span, and for vertical
/// openings that touch along either plan axis.
fn merge_runs(portals: &mut Vec<Portal>) {
    let mut index = 0;
    while index < portals.len() {
        let mut merged = false;
        let mut other = index + 1;
        while other < portals.len() {
            if let Some(joined) = join(&portals[index], &portals[other])
                .or_else(|| join(&portals[other], &portals[index]))
            {
                portals[index] = joined;
                portals.remove(other);
                merged = true;
            } else {
                other += 1;
            }
        }
        // A grown portal may now meet one that was already passed over.
        if !merged {
            index += 1;
        }
    }
}

/// Join open-air side portals into the same room that meet at a corner and
/// do not face each other, whatever their heights or direction, until none
/// do. Each piece stands one sector over its own floor and a boundary that is
/// not axis-aligned (a hexagon's slanted side) is a staircase of pieces, so
/// joining only straight runs would leave a portal per step. The joined
/// opening is the straight chord between the far ends, from the lowest
/// bottom to the highest top; the walk reads only its centre.
fn merge_open_air(portals: &mut Vec<Portal>) {
    let mut index = 0;
    while index < portals.len() {
        let mut merged = false;
        let mut other = index + 1;
        while other < portals.len() {
            if let Some(joined) = join_air(&portals[index], &portals[other]) {
                portals[index] = joined;
                portals.remove(other);
                merged = true;
            } else {
                other += 1;
            }
        }
        if !merged {
            index += 1;
        }
    }
}

/// `a` and `b` as one open-air opening, if they share a plan end and face
/// the same side.
fn join_air(a: &Portal, b: &Portal) -> Option<Portal> {
    let dot = |n: [i16; 3], m: [i16; 3]| {
        i32::from(n[0]) * i32::from(m[0]) + i32::from(n[2]) * i32::from(m[2])
    };
    if a.dest != b.dest || dot(a.normal, b.normal) < 0 {
        return None;
    }
    let plan = |v: [i16; 3]| [v[0], v[2]];
    let (a0, a1, b0, b1) = (plan(a.v[0]), plan(a.v[1]), plan(b.v[0]), plan(b.v[1]));
    let (from, to) = if a1 == b0 {
        (a0, b1)
    } else if b1 == a0 {
        (b0, a1)
    } else if a1 == b1 {
        (a0, b0)
    } else if a0 == b0 {
        (b1, a1)
    } else {
        return None;
    };
    if from == to {
        return None;
    }
    // The chord's horizontal normal, on the side the pieces face.
    let (dx, dz) = (f64::from(to[0] - from[0]), f64::from(to[1] - from[1]));
    let (mut nx, mut nz) = (dz, -dx);
    let (sx, sz) = (
        f64::from(a.normal[0]) + f64::from(b.normal[0]),
        f64::from(a.normal[2]) + f64::from(b.normal[2]),
    );
    if nx * sx + nz * sz < 0.0 {
        (nx, nz) = (-nx, -nz);
    }
    let length = (nx * nx + nz * nz).sqrt();
    let normal = [
        (nx / length * 4096.0).round() as i16,
        0,
        (nz / length * 4096.0).round() as i16,
    ];
    let lo = a.v[0][1].min(b.v[0][1]);
    let hi = a.v[3][1].max(b.v[3][1]);
    let at = |p: [i16; 2], y: i16| [p[0], y, p[1]];
    Some(Portal {
        v: [at(from, lo), at(to, lo), at(to, hi), at(from, hi)],
        normal,
        ..*a
    })
}

/// `a` followed by `b` as one quad, if `b` continues `a`.
fn join(a: &Portal, b: &Portal) -> Option<Portal> {
    if a.dest != b.dest || a.flags != b.flags || a.normal != b.normal || a.reserved != b.reserved {
        return None;
    }
    let [a0, a1, a2, a3] = a.v;
    let [b0, b1, b2, b3] = b.v;
    // Along the first edge direction (v0 -> v1).
    if a1 == b0 && a2 == b3 {
        return Some(Portal {
            v: [a0, b1, b2, a3],
            ..*a
        });
    }
    // Along the other direction (v0 -> v3), for vertical openings.
    if a.flags & PORTAL_VERTICAL != 0 && a3 == b0 && a2 == b1 {
        return Some(Portal {
            v: [a0, a1, b2, b3],
            ..*a
        });
    }
    None
}

/// Rooms reachable from `from` within `hops` portal hops, nearest first.
pub fn ring(portals: &[Vec<Portal>], from: usize, hops: usize) -> Vec<usize> {
    let mut seen = vec![from];
    let mut frontier = vec![from];
    for _ in 0..hops {
        let mut next = Vec::new();
        for &room in &frontier {
            for portal in portals.get(room).map_or(&[][..], Vec::as_slice) {
                let dest = usize::from(portal.dest);
                if !seen.contains(&dest) {
                    seen.push(dest);
                    next.push(dest);
                }
            }
        }
        frontier = next;
    }
    seen
}
