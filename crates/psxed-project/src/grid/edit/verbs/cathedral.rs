//! Cathedral: a nave with side aisles, an arcade, a clerestory, an optional
//! transept, a polygonal apse, a raised chancel with steps and a west front
//! with a large door, composed from [`lay_room`], [`build_arcade`] and
//! [`cut_opening`].
//!
//! Plan (north up, the nave runs west to east along +X):
//!
//! ```text
//!   aisle   |  arcade  ......  |        transept arm
//!   nave    |  bay  bay  bay   | chancel (raised)  apse
//!   aisle   |  arcade  ......  |        transept arm
//! ```
//!
//! The **bay** sets everything: piers every `bay` sectors on the two arcade
//! lines, one arch per bay, one clerestory window per bay in the wall that
//! rises from the aisle ceiling to the nave ceiling. Ceilings are flat, the
//! nave higher than the aisles (stepped). **Vaults are left out**: a vault is
//! a curved ceiling over many cells and the grid's ceiling is one planar
//! quad per leaf, so a barrel or rib vault needs the Vault verb of
//! natural-terrain N8 (leaf-aware ceiling heights following a profile, and
//! the cook merge of T14 to keep the quad count down).
//!
//! Towers are not made here: put a the Building verb on the west front
//! (`west_front` on the report gives its corners).
//!
//! The layout is a pure function of the parameters; nothing is random.

use std::collections::BTreeSet;

use crate::grid::mesh_arch::ArchProfile;
use crate::grid::sector::WallEdge;
use crate::grid::world::GridWorld;

use super::style::{Roles, Style};
use super::styled::{restyle_run, Frame, StyleRefusal};
use crate::ResourceId;

use super::super::rects::CellRect;
use super::arcade::{build_arcade, ArcadeParams, ArcadeRefusal, Layout};
use super::opening::{
    arch_head, cut_opening, ArchShape, DoorPlane, OpeningKind, OpeningParams, OpeningProfile,
};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomRefusal, RoomTarget};

/// A transept crossing the nave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transept {
    /// The bay (counted from the west) it crosses.
    pub at_bay: i32,
    /// How many sectors each arm reaches beyond the aisles.
    pub arm: i32,
}

/// Parameters of a cathedral.
#[derive(Clone, Debug, PartialEq)]
pub struct CathedralParams {
    /// A new room or an existing derived one.
    pub target: RoomTarget,
    /// The north-west corner sector of the whole plan (the north aisle).
    pub origin: [i32; 2],
    /// Floor height of the nave, grid units.
    pub base: i32,
    /// Bays of the nave.
    pub bays: i32,
    /// Sectors per bay.
    pub bay: i32,
    /// Nave width in sectors.
    pub nave_width: i32,
    /// Aisle width in sectors (0 for none).
    pub aisle_width: i32,
    /// Nave floor to ceiling, grid units.
    pub nave_height: i32,
    /// Aisle floor to ceiling, grid units.
    pub aisle_height: i32,
    /// Width of a pier, grid units.
    pub pier: i32,
    /// Arch profile of the arcade.
    pub profile: ArchProfile,
    /// Clerestory windows above the arcade.
    pub clerestory: bool,
    /// A transept.
    pub transept: Option<Transept>,
    /// A polygonal apse at the east end.
    pub apse: bool,
    /// Arched heads for the west door and the clerestory windows (flat
    /// lintels when `None`).
    pub arch: Option<ArchShape>,
    /// A style for the nave ceiling (its cross profile, rhythm and vault are
    /// added to the nave height; its cycle should equal the bay in clicks).
    pub style: Option<Style>,
    /// Materials by role for the styled nave (trim and accent default to the
    /// wall material).
    pub roles: Option<Roles>,
    /// A large door in the west front.
    pub west_door: bool,
    /// Sectors the west door spans.
    pub door_sectors: i32,
    /// Rise of the chancel, grid units (0 for none, steps of a click).
    pub chancel_rise: i32,
    /// Bays of the nave the chancel takes at the east end.
    pub chancel_bays: i32,
    /// Floor material.
    pub floor_material: Option<ResourceId>,
    /// Ceiling material.
    pub ceiling_material: Option<ResourceId>,
    /// Wall material.
    pub wall_material: Option<ResourceId>,
}

impl CathedralParams {
    /// A modest cathedral: five bays of two sectors, a four sector nave with
    /// two sector aisles, a pointed arcade, clerestory, apse, west door and a
    /// two click chancel.
    pub fn new(target: RoomTarget, origin: [i32; 2]) -> Self {
        Self {
            target,
            origin,
            base: 0,
            bays: 5,
            bay: 2,
            nave_width: 4,
            aisle_width: 2,
            nave_height: 9216,
            aisle_height: 5120,
            pier: 256,
            profile: ArchProfile::Pointed,
            clerestory: true,
            transept: None,
            apse: true,
            arch: None,
            style: None,
            roles: None,
            west_door: true,
            door_sectors: 2,
            chancel_rise: 512,
            chancel_bays: 1,
            floor_material: None,
            ceiling_material: None,
            wall_material: None,
        }
    }
}

/// Why a cathedral was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum CathedralRefusal {
    /// A number is out of range (the message names it).
    BadParameter(&'static str),
    /// A room step was refused.
    Room(RoomRefusal),
    /// The arcade was refused.
    Arcade(ArcadeRefusal),
    /// The nave style was refused.
    Style(StyleRefusal),
}

impl CathedralRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::BadParameter(what) => format!("{what} is out of range"),
            Self::Room(r) => r.describe(),
            Self::Arcade(a) => a.describe(),
            Self::Style(s) => s.describe(),
        }
    }
}

/// What a cathedral made.
#[derive(Clone, Debug, PartialEq)]
pub struct CathedralReport {
    /// The room that holds it.
    pub room: usize,
    /// The nave's cells.
    pub nave: CellRect,
    /// Sectors of the whole plan.
    pub cells: usize,
    /// Arches placed.
    pub arches: usize,
    /// Clerestory windows cut.
    pub windows: usize,
    /// The west door's plane (one entry per sector it spans).
    pub doors: Vec<DoorPlane>,
    /// The west front's sectors `(first, last)`, the line a tower goes on.
    pub west_front: ([i32; 2], [i32; 2]),
    /// Things left out and why.
    pub notes: Vec<String>,
}

fn cells_of(r: CellRect) -> BTreeSet<[i32; 2]> {
    r.cells().collect()
}

/// Build a cathedral. One logical edit: the caller snapshots once.
pub fn build_cathedral(
    world: &mut GridWorld,
    p: &CathedralParams,
) -> Result<CathedralReport, CathedralRefusal> {
    let bad = CathedralRefusal::BadParameter;
    if !(1..=16).contains(&p.bays) {
        return Err(bad("bays"));
    }
    if !(2..=4).contains(&p.bay) {
        return Err(bad("bay"));
    }
    if !(2..=8).contains(&p.nave_width) {
        return Err(bad("nave width"));
    }
    if !(0..=4).contains(&p.aisle_width) {
        return Err(bad("aisle width"));
    }
    if p.aisle_width > 0 && p.aisle_height >= p.nave_height {
        return Err(bad("aisle height"));
    }
    if p.chancel_rise < 0 || p.chancel_rise % 256 != 0 {
        return Err(bad("chancel rise"));
    }
    let mut work = world.clone();
    let [ox, oz] = p.origin;
    let len = p.bays * p.bay;
    let aw = p.aisle_width;
    let nw = p.nave_width;
    let total_depth = nw + 2 * aw;
    let nave = CellRect {
        min: [ox, oz + aw],
        max: [ox + len - 1, oz + aw + nw - 1],
    };
    let plan = CellRect {
        min: [ox, oz],
        max: [ox + len - 1, oz + total_depth - 1],
    };
    let room_params = |rect: CellRect, base: i32, height: i32| RoomParams {
        footprint: Footprint::Rect(rect),
        target: RoomTarget::Existing(0),
        base,
        height,
        floor_material: p.floor_material,
        ceiling_material: p.ceiling_material,
        wall_material: p.wall_material,
        leaves: LeafPolicy::Sectors,
        aim: None,
    };
    let cells_params = |cells: BTreeSet<[i32; 2]>, base: i32, height: i32| RoomParams {
        footprint: Footprint::Cells(cells),
        ..room_params(plan, base, height)
    };
    // 1. The whole plan at the aisle height (the nave height without aisles),
    //    in a new room or the given one.
    let first = RoomParams {
        target: p.target.clone(),
        ..room_params(
            plan,
            p.base,
            if aw > 0 {
                p.aisle_height
            } else {
                p.nave_height
            },
        )
    };
    let made = lay_room(&mut work, &first).map_err(CathedralRefusal::Room)?;
    let room = made.room;
    let into = |mut rp: RoomParams| {
        rp.target = RoomTarget::Existing(room);
        rp
    };
    if made.created {
        let r = &mut work.rooms[room];
        r.defaults.closure_facing = crate::grid::overrides::ClosureFacing::Both;
        r.defaults.wall_material = p.wall_material;
    }
    let mut notes = Vec::new();
    // 2. The nave strip, taller.
    if aw > 0 {
        lay_room(&mut work, &into(room_params(nave, p.base, p.nave_height)))
            .map_err(CathedralRefusal::Room)?;
    }
    // 3. Transept: the crossing and its arms at the nave height.
    let mut transept_x: Option<(i32, i32)> = None;
    if let Some(t) = p.transept {
        if !(0..p.bays).contains(&t.at_bay) || !(1..=8).contains(&t.arm) {
            return Err(bad("transept"));
        }
        let x0 = ox + t.at_bay * p.bay;
        let x1 = x0 + p.bay - 1;
        transept_x = Some((x0, x1));
        let arms = CellRect {
            min: [x0, oz - t.arm],
            max: [x1, oz + total_depth - 1 + t.arm],
        };
        lay_room(&mut work, &into(room_params(arms, p.base, p.nave_height)))
            .map_err(CathedralRefusal::Room)?;
    }
    // 4. The chancel: steps west of it, the raised floor over its bays.
    let c_len = p.chancel_bays.clamp(0, p.bays - 1) * p.bay;
    let chancel_x0 = ox + len - c_len;
    if p.chancel_rise > 0 && c_len > 0 {
        let steps = p.chancel_rise / 256;
        for i in 1..steps {
            let col = CellRect {
                min: [chancel_x0 - (steps - i), nave.min[1]],
                max: [chancel_x0 - (steps - i), nave.max[1]],
            };
            let h = p.chancel_rise * i / steps;
            lay_room(
                &mut work,
                &into(room_params(col, p.base + h, p.nave_height - h)),
            )
            .map_err(CathedralRefusal::Room)?;
        }
        let chancel = CellRect {
            min: [chancel_x0, nave.min[1]],
            max: [ox + len - 1, nave.max[1]],
        };
        lay_room(
            &mut work,
            &into(room_params(
                chancel,
                p.base + p.chancel_rise,
                p.nave_height - p.chancel_rise,
            )),
        )
        .map_err(CathedralRefusal::Room)?;
    }
    // 5. The apse: the east half of an octagon as wide as the nave.
    if p.apse {
        let rect = CellRect {
            min: [ox + len - nw / 2, nave.min[1]],
            max: [ox + len - nw / 2 + nw - 1, nave.max[1]],
        };
        let oct: BTreeSet<[i32; 2]> = super::common::octagon_cells(rect, (nw / 3).max(1))
            .into_iter()
            .filter(|c| c[0] >= ox + len)
            .collect();
        if !oct.is_empty() {
            lay_room(
                &mut work,
                &into(cells_params(
                    oct,
                    p.base + p.chancel_rise,
                    p.nave_height - p.chancel_rise,
                )),
            )
            .map_err(CathedralRefusal::Room)?;
        }
    }
    // 5b. The nave ceiling in the style, over what the steps and chancel
    //     left: the style's offsets are added to each sector's own heights.
    if let Some(style) = &p.style {
        let roles = p.roles.unwrap_or(Roles {
            floor: p.floor_material,
            wall: p.wall_material,
            ceiling: p.ceiling_material,
            trim: p.wall_material,
            accent: p.wall_material,
            ground: p.floor_material,
        });
        let frame = Frame {
            along_axis: 0,
            along_sign: 1,
            along_edge: ox * 4,
            across_axis: 1,
            across_sign: 1,
            across_edge: nave.min[1] * 4,
        };
        let cells: BTreeSet<[i32; 2]> = nave.cells().collect();
        restyle_run(
            &mut work,
            room,
            &cells,
            frame,
            nave.depth() * 4,
            style,
            roles,
        )
        .map_err(CathedralRefusal::Style)?;
    }
    // 6. The arcade: one arch per bay on each nave border, none in the
    //    transept.
    let mut arches = 0;
    let rise_guess = match p.profile {
        ArchProfile::Pointed => 3 * (p.bay * 1024 - 2 * p.pier) / 4,
        ArchProfile::Round => (p.bay * 1024 - 2 * p.pier) / 2,
        ArchProfile::Segmental => (p.bay * 1024 - 2 * p.pier) / 4,
        ArchProfile::Flat => 0,
    };
    let spring = (p.aisle_height - rise_guess - p.pier - 128).max(512);
    if aw > 0 {
        for k in 0..p.bays {
            let (x0, x1) = (ox + k * p.bay, ox + (k + 1) * p.bay - 1);
            if transept_x.is_some_and(|(a, b)| x0 <= b && x1 >= a) {
                continue;
            }
            for z in [nave.min[1], nave.max[1] + 1] {
                let line_z = z * 1024;
                let mut a = ArcadeParams::line(
                    room,
                    [x0 * 1024, line_z],
                    [(x1 + 1) * 1024, line_z],
                    p.base,
                    1,
                );
                a.layout = Layout::Line {
                    from: [x0 * 1024, line_z],
                    to: [(x1 + 1) * 1024, line_z],
                };
                a.profile = p.profile;
                a.pier = p.pier;
                a.spring = spring;
                a.depth = 512;
                match build_arcade(&mut work, &a) {
                    Ok(_) => arches += 1,
                    Err(e) => notes.push(format!("arch skipped: {}", e.describe())),
                }
            }
        }
    }
    // 7. Clerestory windows: in the wall from the aisle ceiling up, seen
    //    from the nave, one in the middle of each bay.
    let mut windows = 0;
    let sill = p.aisle_height + 768;
    let top = p.nave_height - 1024;
    if p.clerestory && aw > 0 && top > sill + 256 {
        for k in 0..p.bays {
            let x = ox + k * p.bay + (p.bay - 1) / 2;
            if transept_x.is_some_and(|(a, b)| x >= a && x <= b) {
                continue;
            }
            for (z, side) in [
                (nave.min[1], WallEdge::North),
                (nave.max[1], WallEdge::South),
            ] {
                let start = x * 4;
                // An arched window takes the whole sector so its head has
                // four pieces; a flat one is two clicks wide.
                let (from, to, profile) = match p.arch {
                    Some(shape) => (start, start + 4, arch_head(shape, 4, top)),
                    None => (start + 1, start + 3, OpeningProfile::Rectangular),
                };
                let op = OpeningParams {
                    profile,
                    room,
                    cell: [x, z],
                    side,
                    from,
                    to,
                    kind: OpeningKind::Window { sill },
                    top,
                    material: p.wall_material,
                };
                match cut_opening(&mut work, &op) {
                    Ok(_) => windows += 1,
                    Err(e) => notes.push(format!("window skipped: {}", e.describe())),
                }
            }
        }
    } else if p.clerestory {
        notes.push("no clerestory: no room between the aisle roof and the nave ceiling".into());
    }
    // 8. The west door: whole sectors, centred on the nave.
    let mut doors = Vec::new();
    if p.west_door {
        let n = p.door_sectors.clamp(1, nw);
        let first_z = nave.min[1] + (nw - n) / 2;
        let apex = (p.nave_height - 1024).min(6144);
        // Flat lintels go one sector at a time; an arch is one arch over the
        // whole run of sectors.
        let runs: Vec<(i32, i32, OpeningProfile)> = match p.arch {
            Some(shape) => vec![(
                first_z * 4,
                (first_z + n) * 4,
                arch_head(shape, n * 4, apex),
            )],
            None => (first_z..first_z + n)
                .map(|z| (z * 4, z * 4 + 4, OpeningProfile::Rectangular))
                .collect(),
        };
        for (from, to, profile) in runs {
            let op = OpeningParams {
                profile,
                room,
                cell: [ox, from.div_euclid(4)],
                side: WallEdge::West,
                from,
                to,
                kind: OpeningKind::Door,
                top: apex,
                material: p.wall_material,
            };
            match cut_opening(&mut work, &op) {
                Ok(o) => doors.extend(o.door),
                Err(e) => notes.push(format!("door skipped: {}", e.describe())),
            }
        }
    }
    let cells = cells_of(plan).len();
    *world = work;
    Ok(CathedralReport {
        room,
        nave,
        cells,
        arches,
        windows,
        doors,
        west_front: ([ox, oz], [ox, oz + total_depth - 1]),
        notes,
    })
}
