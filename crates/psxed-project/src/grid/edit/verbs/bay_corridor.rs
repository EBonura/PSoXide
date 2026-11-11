//! Corridors made of a captured bay, and the bays the editor ships.
//!
//! A bay (see [`capture`](super::capture)) is one hand-shaped stretch of a
//! hall, cut out as leaves. [`lay_bay_corridor`] lays copies of it end to end
//! along every straight leg of a path, so the corridor is the hall the bay
//! came from, exactly, for as long as the leg runs.
//!
//! **Convention.** A corridor bay is cut with its width along X and its
//! length along Z: `size = [width, length]`, and it runs along +Z as
//! captured. It is turned (see [`BayTemplate::turned`]) to run along the
//! other three headings.
//!
//! **Width.** A bay has one width. A request for another width is refused
//! with the width the bay has; nothing is stretched or cropped.
//!
//! **Height.** A bay is a fixed shape, so a corridor of bays is level: every
//! waypoint that names a height must name the same one. The bay's lowest
//! floor sits at that height. A path that rises is refused with a sentence
//! saying so.
//!
//! **Turns.** Each straight leg is laid with whole bays from its start. A
//! turn leaves the square of the corridor's width around the waypoint plain
//! (a flat landing), the same as a plain corridor's corner, and the bays of
//! the two legs stop short of it. Rows of a leg that do not fill a bay are
//! left plain at its far end. A path where no leg has room for one bay is
//! refused.
//!
//! **Materials.** The caller binds the bay to the project first
//! ([`BayTemplate::bound_to`]): materials found by name stay, the rest take
//! the project's roles, and the plain landings use the same roles.

use std::collections::BTreeSet;

use crate::grid::world::GridWorld;

use super::capture::{repeat_bay, Along, BayTemplate, CaptureRefusal};
use super::corridor::{CorridorRefusal, Waypoint};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomRefusal, RoomTarget};
use super::style::Roles;

/// The name of the bay the editor ships.
pub const RIB_CORRIDOR: &str = "Rib corridor";

/// The source of the shipped bay: the middle stretch of a hand-shaped hall
/// (six sectors wide, three long, one rib), captured with the Capture
/// command and committed as it came out.
const RIB_CORRIDOR_RON: &str = include_str!("bays/rib_corridor.bay.ron");

/// The bays that ship with the editor, by name.
pub fn built_in_bays() -> Vec<(String, BayTemplate)> {
    vec![(RIB_CORRIDOR.to_string(), rib_corridor())]
}

/// The shipped Rib corridor bay, materials named as the source project had
/// them.
pub fn rib_corridor() -> BayTemplate {
    ron::from_str(RIB_CORRIDOR_RON).expect("the shipped bay is valid RON")
}

/// A corridor of bays.
#[derive(Clone, Debug, PartialEq)]
pub struct BayCorridor {
    /// Where it goes.
    pub target: RoomTarget,
    /// The path (axis-aligned legs).
    pub waypoints: Vec<Waypoint>,
    /// Width in whole sectors: must be the bay's.
    pub width: i32,
    /// Floor height of the first waypoint when it names none.
    pub base: i32,
    /// The bay, already bound to the project's materials.
    pub bay: BayTemplate,
    /// The project's roles, for the plain landings.
    pub roles: Roles,
}

/// What a corridor of bays made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BayOutcome {
    /// The room.
    pub room: usize,
    /// Whether it is new.
    pub created: bool,
    /// Bays laid, over all legs.
    pub bays: usize,
    /// Sectors of the path left plain: landings and short ends.
    pub plain: usize,
    /// Leaves in the room afterwards.
    pub leaves: usize,
}

/// Why a corridor of bays was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum BayRefusal {
    /// The bay holds nothing or has no size.
    EmptyBay,
    /// The width is not the bay's.
    WrongWidth {
        /// The bay's width.
        want: i32,
        /// The width asked for.
        got: i32,
    },
    /// A waypoint names another height.
    NotLevel,
    /// No leg has room for one bay.
    TooShort {
        /// Sectors one bay takes along the run.
        bay: i32,
    },
    /// The path is not made of straight legs along X or Z.
    Corridor(CorridorRefusal),
    /// The plain ground was refused.
    Room(RoomRefusal),
    /// The bay could not be turned or laid.
    Bay(CaptureRefusal),
}

impl BayRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::EmptyBay => "The bay holds no cells".to_string(),
            Self::WrongWidth { want, got } => format!(
                "This corridor is {want} sectors wide: set the width to {want} (it is {got})"
            ),
            Self::NotLevel => {
                "A corridor of bays runs level: give every waypoint the same height".to_string()
            }
            Self::TooShort { bay } => {
                format!("No straight stretch is long enough for one bay of {bay} sectors")
            }
            Self::Corridor(c) => c.describe(),
            Self::Room(r) => r.describe(),
            Self::Bay(b) => b.describe(),
        }
    }
}

fn offsets(width: i32) -> std::ops::RangeInclusive<i32> {
    -((width - 1) / 2)..=(width / 2)
}

/// Quarter turns that take a run along +Z to run along `d`.
fn quarter_turns(d: [i32; 2]) -> u8 {
    match d {
        [0, 1] => 0,
        [1, 0] => 1,
        [0, -1] => 2,
        _ => 3,
    }
}

/// A straight run of a path: `len + 1` cells from `start` along unit `dir`.
struct Straight {
    start: [i32; 2],
    dir: [i32; 2],
    len: i32,
}

/// The path as maximal straight runs, repeated points dropped and collinear
/// legs joined, with the waypoints where it turns.
fn straights(waypoints: &[Waypoint]) -> (Vec<Straight>, Vec<[i32; 2]>) {
    let mut points: Vec<[i32; 2]> = Vec::new();
    for w in waypoints {
        if points.last() != Some(&w.cell) {
            points.push(w.cell);
        }
    }
    let mut runs: Vec<Straight> = Vec::new();
    let mut corners: Vec<[i32; 2]> = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let dir = [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
        let len = (b[0] - a[0]).abs() + (b[1] - a[1]).abs();
        match runs.last_mut() {
            Some(last) if last.dir == dir => last.len += len,
            Some(_) => {
                corners.push(a);
                runs.push(Straight { start: a, dir, len });
            }
            None => runs.push(Straight { start: a, dir, len }),
        }
    }
    (runs, corners)
}

/// Lay a corridor of bays. One logical edit: the caller snapshots once.
pub fn lay_bay_corridor(world: &mut GridWorld, p: &BayCorridor) -> Result<BayOutcome, BayRefusal> {
    let [width, bay_len] = p.bay.size;
    if width < 1 || bay_len < 1 || p.bay.leaves.is_empty() {
        return Err(BayRefusal::EmptyBay);
    }
    if p.width != width {
        return Err(BayRefusal::WrongWidth {
            want: width,
            got: p.width,
        });
    }
    let level = p.waypoints.first().and_then(|w| w.floor).unwrap_or(p.base);
    if p.waypoints
        .iter()
        .any(|w| w.floor.is_some_and(|f| f != level))
    {
        return Err(BayRefusal::NotLevel);
    }
    // The landings and short ends are plain, as tall as the bay's tallest
    // ceiling.
    let top = p
        .bay
        .leaves
        .iter()
        .filter_map(|(_, s)| s.ground_ceiling().map(|c| c.heights.into_iter().max()))
        .flatten()
        .max()
        .unwrap_or(3072);
    for (leg, pair) in p.waypoints.windows(2).enumerate() {
        if pair[0].cell[0] != pair[1].cell[0] && pair[0].cell[1] != pair[1].cell[1] {
            return Err(BayRefusal::Corridor(CorridorRefusal::DiagonalLeg { leg }));
        }
    }
    let (runs, corners) = straights(&p.waypoints);
    if runs.is_empty() {
        return Err(BayRefusal::Corridor(CorridorRefusal::TooFewWaypoints));
    }
    // The ground: every straight run as wide as the bay, and the square of
    // that width at every turn, which stays a plain landing.
    let mut ground: BTreeSet<[i32; 2]> = BTreeSet::new();
    let mut landing: BTreeSet<[i32; 2]> = BTreeSet::new();
    for run in &runs {
        let lat = [run.dir[1].abs(), run.dir[0].abs()];
        for k in 0..=run.len {
            for o in offsets(width) {
                ground.insert([
                    run.start[0] + run.dir[0] * k + lat[0] * o,
                    run.start[1] + run.dir[1] * k + lat[1] * o,
                ]);
            }
        }
    }
    for c in &corners {
        for ox in offsets(width) {
            for oz in offsets(width) {
                landing.insert([c[0] + ox, c[1] + oz]);
            }
        }
    }
    ground.extend(landing.iter().copied());
    let mut work = world.clone();
    let mut rp = RoomParams::new(Footprint::Cells(ground.clone()), "", top);
    rp.target = p.target.clone();
    rp.base = level;
    rp.floor_material = p.roles.floor;
    rp.ceiling_material = p.roles.ceiling;
    rp.wall_material = p.roles.wall;
    // One leaf per sector, so clearing a bay never takes a landing with it.
    rp.leaves = LeafPolicy::Sectors;
    let laid = lay_room(&mut work, &rp).map_err(BayRefusal::Room)?;
    let omin = offsets(width).next().unwrap_or(0);
    let mut bays = 0;
    for run in &runs {
        let lat = [run.dir[1].abs(), run.dir[0].abs()];
        let cell = |k: i32| {
            [
                run.start[0] + run.dir[0] * k + lat[0] * omin,
                run.start[1] + run.dir[1] * k + lat[1] * omin,
            ]
        };
        let free: Vec<i32> = (0..=run.len)
            .filter(|&k| !landing.contains(&cell(k)))
            .collect();
        let turned = p
            .bay
            .turned(quarter_turns(run.dir))
            .map_err(BayRefusal::Bay)?;
        let along = if run.dir[0] != 0 { Along::X } else { Along::Z };
        // Each unbroken stretch of free rows takes as many whole bays as fit.
        for group in free.chunk_by(|a, b| b - a == 1) {
            let count = group.len() as i32 / bay_len;
            if count == 0 {
                continue;
            }
            let (first, last) = (cell(group[0]), cell(group[0] + count * bay_len - 1));
            let origin = [first[0].min(last[0]), first[1].min(last[1])];
            repeat_bay(&mut work, laid.room, &turned, origin, along, count, level)
                .map_err(BayRefusal::Bay)?;
            bays += count as usize;
        }
    }
    if bays == 0 {
        return Err(BayRefusal::TooShort { bay: bay_len });
    }
    let leaves = work.rooms[laid.room].leaf_count();
    *world = work;
    Ok(BayOutcome {
        room: laid.room,
        created: laid.created,
        bays,
        plain: ground.len() - bays * (width * bay_len) as usize,
        leaves,
    })
}
