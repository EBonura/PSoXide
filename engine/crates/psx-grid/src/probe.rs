//! Light probes: the light a character standing in the world is lit by.
//!
//! The cook bakes the light of each sector's middle, a chest height above the
//! floor, with the rig that colours the sector's quads (the lamps, their
//! shadows, and with the global light on the occlusion, sky and bounce of a
//! full sphere), and stores it in the room chunk ([`ProbeExt`]). A model is lit
//! by blending the four probes around it, so it is dark where the world is
//! dark and lit where a lamp reaches it, and the shadow a wall casts on the
//! floor falls on a character walking through it too.
//!
//! The lookup is integer only, 32 bit, a few dozen operations: the guest
//! runs [`ResidentRooms::probe_light`] once per drawn model, the editor
//! preview runs the same function over the same tables.

use crate::format::*;
use crate::height::weight_q12;
use crate::resident::ResidentRooms;
use crate::view::{header_at, room_header, table, FormatError};

/// The probes of one room, as read from its chunk. Empty for a room cooked
/// without any.
#[derive(Copy, Clone, Debug, Default)]
pub struct ProbeView<'a> {
    /// [`ProbeExt::cells`].
    pub cells: &'a [Probe],
    /// [`ProbeExt::multi`].
    pub multi: &'a [Probe],
}

impl<'a> ProbeView<'a> {
    /// No probes.
    pub const EMPTY: Self = Self {
        cells: &[],
        multi: &[],
    };

    /// Whether the room has no probes.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Read the probe block of the chunk `bytes` whose header is `head`
    /// ([`ProbeView::EMPTY`] without [`ROOM_PROBES`]).
    pub fn parse(bytes: &'a [u8], head: &RoomHeader) -> Result<Self, FormatError> {
        if head.flags & ROOM_PROBES == 0 {
            return Ok(Self::EMPTY);
        }
        let ext = header_at::<ProbeExt>(bytes, head.render_offset as usize)?;
        if ext.layout != PROBE_LAYOUT_HASH {
            return Err(FormatError::BadLayout);
        }
        let cells = usize::from(head.width) * usize::from(head.depth);
        if ext.cells.count as usize != cells || ext.multi.count as usize > PROBE_MULTI_MAX {
            return Err(FormatError::Inconsistent);
        }
        Ok(Self {
            cells: table::<Probe>(bytes, ext.cells.offset as usize, cells)?,
            multi: table::<Probe>(bytes, ext.multi.offset as usize, ext.multi.count as usize)?,
        })
    }

    /// Read the probes of a whole chunk (or of its first bytes up to the end
    /// of the probe tables) without parsing the rest of the render block, for
    /// hosts that bake probes into chunks of their own.
    pub fn parse_chunk(bytes: &'a [u8]) -> Result<Self, FormatError> {
        let head = room_header(bytes)?;
        Self::parse(bytes, head)
    }

    /// The probe of sector `cell` nearest in height to `y` (room-local),
    /// as `(probe height, packed colour)`. `None` for a sector with no floor.
    #[inline]
    pub fn nearest(&self, cell: usize, y: i32) -> Option<(i32, u16)> {
        let entry = *self.cells.get(cell)?;
        if entry.rgb & PROBE_MULTI == 0 {
            return Some((i32::from(entry.y), entry.rgb));
        }
        let count = usize::from(entry.rgb & !PROBE_MULTI);
        let first = usize::from(entry.y as u16);
        let run = self.multi.get(first..first + count)?;
        let mut best = *run.first()?;
        let mut best_gap = (i32::from(best.y) - y).abs();
        for probe in &run[1..] {
            let gap = (i32::from(probe.y) - y).abs();
            if gap < best_gap {
                best = *probe;
                best_gap = gap;
            }
        }
        Some((i32::from(best.y), best.rgb))
    }

    /// Walk every entry. Host and debug use.
    pub fn validate_full(&self) -> Result<(), FormatError> {
        for entry in self.cells {
            if entry.rgb & PROBE_MULTI != 0 {
                let first = usize::from(entry.y as u16);
                let count = usize::from(entry.rgb & !PROBE_MULTI);
                if first + count > self.multi.len() {
                    return Err(FormatError::IndexRange);
                }
            }
        }
        if self.multi.iter().any(|p| p.rgb & PROBE_MULTI != 0) {
            return Err(FormatError::Inconsistent);
        }
        Ok(())
    }
}

/// A probe found in a column of the world.
#[derive(Copy, Clone)]
struct Found {
    /// World height of the probe.
    y: i32,
    rgb: u16,
}

impl<'a> ResidentRooms<'a> {
    /// The probe nearest in height to `y` in the sector column holding the
    /// world point `(x, z)`, over every resident room that has probes there
    /// (stacked rooms are told apart by height).
    #[inline]
    fn column_probe(&self, x: i32, z: i32, y: i32) -> Option<Found> {
        let s = self.s as u32;
        let mut best: Option<Found> = None;
        let mut best_gap = i32::MAX;
        for &room in self.level().lattice_rooms_at(x, z) {
            let room = usize::from(room);
            let Some(slot) = self.get(room) else {
                continue;
            };
            if slot.probes.is_empty() {
                continue;
            }
            let rec = &self.level().rooms[room];
            let (lx, lz) = (x.wrapping_sub(rec.origin[0]), z.wrapping_sub(rec.origin[2]));
            if lx < 0 || lz < 0 {
                continue;
            }
            let (w, d) = (u32::from(rec.width), u32::from(rec.depth));
            if lx as u32 >= w * s || lz as u32 >= d * s {
                continue;
            }
            let cell = (self.recip.cell(lz as u32) * w + self.recip.cell(lx as u32)) as usize;
            let local = y - rec.origin[1];
            if let Some((py, rgb)) = slot.probes.nearest(cell, local) {
                let gap = (py - local).abs();
                if gap < best_gap {
                    best_gap = gap;
                    best = Some(Found {
                        y: py + rec.origin[1],
                        rgb,
                    });
                }
            }
        }
        best
    }

    /// The light a character at the world point `p` is lit by (128 neutral,
    /// the colour a face of the same spot would carry), blended from the four
    /// probes whose sector middles surround it. `None` when no resident room
    /// with probes holds the point.
    ///
    /// A neighbour on another floor (more than a sector off the height of the
    /// probe of the sector the point is in) or with no floor counts as that
    /// probe, so a ledge does not borrow the light of the room below it.
    #[inline]
    pub fn probe_light(&self, p: [i32; 3]) -> Option<[u8; 3]> {
        // The room that lit the last point first: models move a little between
        // draws, and a room that no other room overlaps in plan needs no
        // search to be the right one.
        if let Some(light) = self.probe_light_in_hint_room(p) {
            return Some(light);
        }
        self.probe_light_searching(p)
    }

    /// Whether no other room of the level overlaps `room` in plan (cached).
    fn plan_is_solo(&self, room: usize) -> bool {
        match self.solo[room].get() {
            1 => return true,
            2 => return false,
            _ => {}
        }
        let s = self.s;
        let rooms = self.level().rooms;
        let span = |r: &crate::format::RoomRecord| {
            (
                r.origin[0],
                r.origin[2],
                r.origin[0] + i32::from(r.width) * s,
                r.origin[2] + i32::from(r.depth) * s,
            )
        };
        let (x0, z0, x1, z1) = span(&rooms[room]);
        let shared = rooms.iter().enumerate().any(|(i, r)| {
            let (a0, b0, a1, b1) = span(r);
            i != room && a0 < x1 && x0 < a1 && b0 < z1 && z0 < b1
        });
        self.solo[room].set(if shared { 2 } else { 1 });
        !shared
    }

    /// [`Self::probe_light`] for a point in the room that served the last one,
    /// when that room is alone over its footprint and all four sectors
    /// around the point are in it.
    fn probe_light_in_hint_room(&self, p: [i32; 3]) -> Option<[u8; 3]> {
        let room = usize::from(self.probe_hint.get());
        let rec = self.level().rooms.get(room)?;
        let slot = self.get(room)?;
        if slot.probes.is_empty() || !self.plan_is_solo(room) {
            return None;
        }
        let (lx, lz) = (
            p[0].wrapping_sub(rec.origin[0]),
            p[2].wrapping_sub(rec.origin[2]),
        );
        if lx < 0 || lz < 0 {
            return None;
        }
        let s = self.s;
        let u = s as u32;
        if lx as u32 >= u32::from(rec.width) * u || lz as u32 >= u32::from(rec.depth) * u {
            return None;
        }
        let (cx, cz) = (self.recip.cell(lx as u32), self.recip.cell(lz as u32));
        let (fx, fz) = (lx - (cx * u) as i32, lz - (cz * u) as i32);
        let half = s / 2;
        let (cx0, cz0) = (
            cx as i32 - i32::from(fx < half),
            cz as i32 - i32::from(fz < half),
        );
        if cx0 < 0 || cz0 < 0 || cx0 + 1 >= i32::from(rec.width) || cz0 + 1 >= i32::from(rec.depth)
        {
            return None;
        }
        let probes = direct_block(slot, rec, cx0, cz0, p[1]);
        self.blend(probes, (fx, fz))
    }

    /// [`Self::probe_light`] by searching the world lattice for the rooms
    /// that hold the point and its four sectors.
    pub(crate) fn probe_light_searching(&self, p: [i32; 3]) -> Option<[u8; 3]> {
        let s = self.s;
        let half = s / 2;
        // Where the point lies in its sector. Every room origin is a multiple
        // of the sector size, so the sector lattice is the same in all rooms
        // and any room that holds the point gives its phase.
        let mut cell_at = None;
        let mut holders = 0u32;
        let mut anchor_room = 0usize;
        let mut anchor_cell = (0u32, 0u32);
        for &room in self.level().lattice_rooms_at(p[0], p[2]) {
            let rec = &self.level().rooms[usize::from(room)];
            let (lx, lz) = (
                p[0].wrapping_sub(rec.origin[0]),
                p[2].wrapping_sub(rec.origin[2]),
            );
            if lx < 0 || lz < 0 {
                continue;
            }
            let u = s as u32;
            if lx as u32 >= u32::from(rec.width) * u || lz as u32 >= u32::from(rec.depth) * u {
                continue;
            }
            let (cx, cz) = (self.recip.cell(lx as u32), self.recip.cell(lz as u32));
            holders += 1;
            if holders == 1 {
                anchor_room = usize::from(room);
                anchor_cell = (cx, cz);
                cell_at = Some((
                    rec.origin[0] + (cx * u) as i32,
                    rec.origin[2] + (cz * u) as i32,
                    lx - (cx * u) as i32,
                    lz - (cz * u) as i32,
                ));
            }
        }
        let (bx, bz, lx, lz) = cell_at?;
        let mut probes: [[Option<Found>; 2]; 2] = [[None; 2]; 2];
        let (cx0, cz0) = (
            anchor_cell.0 as i32 - i32::from(lx < half),
            anchor_cell.1 as i32 - i32::from(lz < half),
        );
        let direct = if holders == 1 {
            let rec = &self.level().rooms[anchor_room];
            self.get(anchor_room)
                .filter(|slot| {
                    !slot.probes.is_empty()
                        && cx0 >= 0
                        && cz0 >= 0
                        && cx0 + 1 < i32::from(rec.width)
                        && cz0 + 1 < i32::from(rec.depth)
                })
                .map(|slot| (slot, rec))
        } else {
            None
        };
        if let Some((slot, rec)) = direct {
            probes = direct_block(slot, rec, cx0, cz0, p[1]);
        } else {
            // The two sector middles on each axis.
            let xs = if lx >= half {
                [bx + half, bx + s + half]
            } else {
                [bx - s + half, bx + half]
            };
            let zs = if lz >= half {
                [bz + half, bz + s + half]
            } else {
                [bz - s + half, bz + half]
            };
            for (i, &x) in xs.iter().enumerate() {
                for (j, &z) in zs.iter().enumerate() {
                    probes[i][j] = self.column_probe(x, z, p[1]);
                }
            }
        }
        let light = self.blend(probes, (lx, lz))?;
        self.probe_hint.set(anchor_room as u16);
        Some(light)
    }

    /// Blend the four probes around a point that lies `(lx, lz)` into its
    /// sector (`probes[i][j]` is the `i`-th sector along X and `j`-th along Z).
    #[inline]
    fn blend(&self, probes: [[Option<Found>; 2]; 2], (lx, lz): (i32, i32)) -> Option<[u8; 3]> {
        let s = self.s;
        let half = s / 2;
        let (fx, fz) = (
            if lx >= half { lx - half } else { lx + half },
            if lz >= half { lz - half } else { lz + half },
        );
        let own = (usize::from(lx < half), usize::from(lz < half));
        // The probe of the sector the point is in sets the floor; without one
        // the first neighbour that has a probe does.
        let reference = probes[own.0][own.1].or_else(|| {
            probes
                .iter()
                .flatten()
                .copied()
                .find(Option::is_some)
                .flatten()
        })?;
        let mut colour = [[[0i32; 3]; 2]; 2];
        for i in 0..2 {
            for j in 0..2 {
                let rgb = match probes[i][j] {
                    Some(f) if (f.y - reference.y).abs() <= s => f.rgb,
                    _ => reference.rgb,
                };
                let c = probe_unpack(rgb);
                colour[i][j] = [i32::from(c[0]), i32::from(c[1]), i32::from(c[2])];
            }
        }
        let wx = weight_q12(fx as u32, self.inv24);
        let wz = weight_q12(fz as u32, self.inv24);
        let w00 = ((4096 - wx) * (4096 - wz) + 2048) >> 12;
        let w10 = (wx * (4096 - wz) + 2048) >> 12;
        let w01 = ((4096 - wx) * wz + 2048) >> 12;
        let w11 = (4096 - w00 - w10 - w01).max(0);
        let mix = |k: usize| -> u8 {
            let sum = w00 * colour[0][0][k]
                + w10 * colour[1][0][k]
                + w01 * colour[0][1][k]
                + w11 * colour[1][1][k];
            ((sum + 2048) >> 12).clamp(0, 255) as u8
        };
        Some([mix(0), mix(1), mix(2)])
    }
}

/// The four probes of the 2 by 2 sectors from `(cx0, cz0)` of a room, nearest
/// in height to the world height `y`.
#[inline]
fn direct_block(
    slot: &crate::resident::RoomSlot<'_>,
    rec: &crate::format::RoomRecord,
    cx0: i32,
    cz0: i32,
    y: i32,
) -> [[Option<Found>; 2]; 2] {
    let local = y - rec.origin[1];
    let w = usize::from(rec.width);
    let mut probes: [[Option<Found>; 2]; 2] = [[None; 2]; 2];
    for (i, column) in probes.iter_mut().enumerate() {
        for (j, entry) in column.iter_mut().enumerate() {
            let cell = (cz0 as usize + j) * w + cx0 as usize + i;
            *entry = slot.probes.nearest(cell, local).map(|(py, rgb)| Found {
                y: py + rec.origin[1],
                rgb,
            });
        }
    }
    probes
}
