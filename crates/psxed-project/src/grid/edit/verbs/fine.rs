//! The level verbs on cells smaller than a sector (and mixed rooms), through
//! the resolution [lens](super::super::lens).
//!
//! The footprint is a set of cells of one level: `level` 2 sectors, 3 half
//! sectors, 4 quarter sectors, each addressed by its cell coordinate at that
//! level (`world click / edge`). A [`FineDrag`] has the shape of every other
//! drag: `begin` keeps the room as it was, and every `apply` restores it and
//! runs the verb from absolute values, so a drag can be repeated, reversed or
//! cancelled.

use crate::grid::room::GridRoom;
use crate::grid::units::FINE_UNITS;
use crate::grid::walk::Spawn;
use crate::grid::world::GridWorld;

use super::super::lens::{dedup_tiles, Lens};
use super::break_verb::{BreakParams, BreakReport, BreakSnap};
use super::raise::{raise_region, RaiseParams};
use super::ramp::{ramp_region, RampParams};
use super::rules::{ensure_strata, ensure_trim};
use super::stairs::{lay_stairs, Flight, StairsParams};
use super::Applied;

/// A verb and what it needs besides the amount.
#[derive(Clone, Debug)]
pub enum FineVerb {
    /// Raise or Lower the footprint.
    Raise(RaiseParams),
    /// Slope the footprint.
    Ramp(RampParams),
    /// Lay a flight along `path` (cells of the footprint's level, in order).
    Stairs {
        params: StairsParams,
        path: Vec<[i32; 2]>,
    },
    /// Break the footprint; the amount is the intensity.
    Break { params: BreakParams, seed: u32 },
}

/// What a fine verb did.
#[derive(Clone, Debug)]
pub enum FineReport {
    /// Raise and Ramp.
    Applied(Applied),
    /// Stairs.
    Flight(Flight),
    /// Break.
    Break(BreakReport),
    /// Nothing was done, and why.
    Refused(String),
}

/// A started verb on cells of any size.
#[derive(Clone, Debug)]
pub struct FineDrag {
    room: usize,
    before: GridRoom,
    level: u8,
    footprint: Vec<[i32; 2]>,
    verb: FineVerb,
}

impl FineDrag {
    /// Start `verb` on `footprint`, cells of `level` of `room`. `None` for a
    /// missing room or an empty footprint.
    pub fn begin(
        world: &GridWorld,
        room: usize,
        level: u8,
        footprint: &[[i32; 2]],
        verb: FineVerb,
    ) -> Option<Self> {
        let before = world.rooms.get(room)?.clone();
        let footprint = dedup_tiles(footprint);
        (!footprint.is_empty()).then_some(Self {
            room,
            before,
            level,
            footprint,
            verb,
        })
    }

    /// The verb and its parameters.
    pub fn verb(&self) -> &FineVerb {
        &self.verb
    }

    /// Change the verb between applies (options).
    pub fn set_verb(&mut self, verb: FineVerb) {
        self.verb = verb;
    }

    /// Restore the room, then run the verb by `amount` (grid units of the
    /// real world: a rise, a lift, or the break intensity 0 to 100).
    pub fn apply(&self, world: &mut GridWorld, amount: i32) -> FineReport {
        if let Some(room) = world.rooms.get_mut(self.room) {
            *room = self.before.clone();
        }
        if amount == 0 {
            // Nothing to do: the room stays exactly as it began.
            return match &self.verb {
                FineVerb::Break { seed, .. } => FineReport::Break(BreakReport {
                    seed: *seed,
                    ..BreakReport::default()
                }),
                FineVerb::Stairs { .. } => FineReport::Flight(Flight::default()),
                _ => FineReport::Applied(Applied::default()),
            };
        }
        let Some((lens, mut virt, region)) =
            Lens::open(world, self.room, self.level, &self.footprint)
        else {
            return FineReport::Refused("no cell there".to_string());
        };
        let f = lens.factor();
        let report = match &self.verb {
            FineVerb::Raise(params) => {
                let mut p = *params;
                p.reference *= f;
                match raise_region(&mut virt, &region, p, amount * f) {
                    Some(a) => FineReport::Applied(a),
                    None => FineReport::Refused("no floor in those cells".to_string()),
                }
            }
            FineVerb::Ramp(params) => match ramp_region(&mut virt, &region, *params, amount * f) {
                Some(a) => FineReport::Applied(a),
                None => FineReport::Refused("no floor in those cells".to_string()),
            },
            FineVerb::Stairs { params, path } => {
                if lens.level() != self.level {
                    return FineReport::Refused(
                        "cells finer than the path are next to it: use cells of that size"
                            .to_string(),
                    );
                }
                let mut p = *params;
                if let super::stairs::RiserOption::Typed(v) = p.riser {
                    p.riser = super::stairs::RiserOption::Typed(v * f);
                }
                match lay_stairs(&mut virt, 0, path, p, amount * f) {
                    Some(flight) => FineReport::Flight(flight),
                    None => FineReport::Refused("the path crosses no cell".to_string()),
                }
            }
            FineVerb::Break { params, seed } => {
                let mut p = params.clone();
                p.amplitude *= f;
                let step = match p.snap {
                    BreakSnap::Fine => FINE_UNITS,
                    BreakSnap::Click => crate::grid::units::CLICK_UNITS,
                    BreakSnap::Units(v) => v,
                };
                p.snap = BreakSnap::Units(step * f);
                p.spawns = p
                    .spawns
                    .iter()
                    .filter(|s| s.room == self.room)
                    .map(|s| Spawn {
                        room: 0,
                        cell: lens.tile_of_sector(s.cell),
                    })
                    .collect();
                match super::break_verb::break_region(
                    &mut virt,
                    &region,
                    p,
                    amount.clamp(0, 100) as u32,
                    *seed,
                ) {
                    Some(r) => FineReport::Break(r),
                    None => FineReport::Refused("nothing to break there".to_string()),
                }
            }
        };
        lens.close(world, &virt);
        if amount != 0 {
            if let Some(room) = world.rooms.get_mut(self.room) {
                ensure_strata(room);
                ensure_trim(room);
            }
        }
        report
    }
}

impl FineDrag {
    /// For a Ramp: the largest rise, grid units, that keeps every floor of
    /// the footprint walkable, read through the lens. `None` for another verb.
    pub fn max_walkable_rise(&self, world: &GridWorld) -> Option<i32> {
        let FineVerb::Ramp(params) = &self.verb else {
            return None;
        };
        let mut probe = world.clone();
        *probe.rooms.get_mut(self.room)? = self.before.clone();
        let (lens, virt, region) = Lens::open(&probe, self.room, self.level, &self.footprint)?;
        let drag = super::ramp::RampDrag::begin(&virt, &region, *params)?;
        Some(drag.max_walkable_rise() / lens.factor())
    }
}

/// Run `verb` once on `footprint` (typed amounts, tests, the MCP).
pub fn run_fine(
    world: &mut GridWorld,
    room: usize,
    level: u8,
    footprint: &[[i32; 2]],
    verb: FineVerb,
    amount: i32,
) -> Option<FineReport> {
    let drag = FineDrag::begin(world, room, level, footprint, verb)?;
    Some(drag.apply(world, amount))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::edit::verbs::ramp::RampDir;
    use crate::grid::leaf::LeafKey;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;

    fn world() -> GridWorld {
        let mut room = GridRoom::new_derived("r", 4, 4);
        for z in 0..4 {
            for x in 0..4 {
                let s = room.sector_or_insert(x, z).unwrap();
                s.set_ground_floor(Some(GridSurface::flat(0, None)));
                s.set_ground_ceiling(Some(GridSurface::flat(4096, None)));
            }
        }
        GridWorld {
            scale: GridScale { num: 5, den: 4 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn floor_of(w: &GridWorld, key: LeafKey) -> [i32; 4] {
        w.rooms[0]
            .leaf(key)
            .unwrap()
            .ground_floor()
            .unwrap()
            .heights
    }

    #[test]
    fn a_ramp_over_quarter_cells_slopes_across_them() {
        let mut w = world();
        // Eight quarter cells in a row, one click deep: a ramp rising 512
        // grid units over two sectors.
        let cells: Vec<[i32; 2]> = (0..8).map(|i| [8 + i, 8]).collect();
        let report = run_fine(
            &mut w,
            0,
            4,
            &cells,
            FineVerb::Ramp(RampParams {
                from: RampDir::West,
                follow_ceiling: true,
                wall_material: None,
            }),
            512,
        )
        .unwrap();
        assert!(matches!(report, FineReport::Applied(_)), "{report:?}");
        // The cells are quarter-sector leaves now, rising to the east in
        // eight equal strips: 64 grid units each.
        for i in 0..8 {
            let key = LeafKey::new(4, 8 + i, 8).unwrap();
            let h = floor_of(&w, key);
            assert_eq!(h[0], 64 * i, "cell {i}: {h:?}");
            assert_eq!(h[1], 64 * (i + 1), "cell {i}: {h:?}");
        }
        assert!(crate::grid::tree::check(&w.rooms[0]).is_empty());
    }

    #[test]
    fn stairs_of_quarter_cells_make_one_step_per_cell() {
        let mut w = world();
        let path: Vec<[i32; 2]> = (0..4).map(|i| [8 + i, 8]).collect();
        let report = run_fine(
            &mut w,
            0,
            4,
            &path,
            FineVerb::Stairs {
                params: StairsParams::default(),
                path: path.clone(),
            },
            256,
        )
        .unwrap();
        let FineReport::Flight(flight) = report else {
            panic!("{report:?}");
        };
        assert!(flight.refusal.is_none(), "{flight:?}");
        assert_eq!(flight.steps, 4);
        let tops: Vec<i32> = (0..4)
            .map(|i| floor_of(&w, LeafKey::new(4, 8 + i, 8).unwrap())[0])
            .collect();
        assert_eq!(tops, vec![64, 128, 192, 256], "{tops:?}");
    }

    #[test]
    fn a_drag_can_be_repeated_and_cancelled() {
        let mut w = world();
        let before = w.clone();
        let params = RaiseParams {
            mode: super::super::raise::RaiseMode::Offset,
            follow_ceiling: true,
            wall_material: None,
            reference: 0,
        };
        let drag = FineDrag::begin(&w, 0, 3, &[[5, 5], [6, 5]], FineVerb::Raise(params)).unwrap();
        drag.apply(&mut w, 512);
        let once = w.clone();
        drag.apply(&mut w, 256);
        drag.apply(&mut w, 512);
        assert_eq!(w, once);
        drag.apply(&mut w, 0);
        assert_eq!(w, before);
    }

    #[test]
    fn break_on_quarter_cells_changes_them_and_keeps_the_tree_sound() {
        let mut w = world();
        let cells: Vec<[i32; 2]> = (0..8)
            .flat_map(|z| (0..8).map(move |x| [8 + x, 8 + z]))
            .collect();
        let verb = FineVerb::Break {
            params: BreakParams {
                keep_walkable: false,
                ..BreakParams::default()
            },
            seed: 3,
        };
        let report = run_fine(&mut w, 0, 4, &cells, verb, 60).unwrap();
        let FineReport::Break(r) = report else {
            panic!("{report:?}");
        };
        assert!(r.touched > 0, "{r:?}");
        assert!(crate::grid::tree::check(&w.rooms[0]).is_empty());
        // The rest of the room is untouched.
        let far = LeafKey::sector([0, 0]);
        assert_eq!(floor_of(&w, far), [0; 4]);
    }
}
