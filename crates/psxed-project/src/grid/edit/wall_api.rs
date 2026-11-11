//! One wall: a single API over the two stores walls live in.
//!
//! A caller sees walls on an edge and their properties, each marked
//! [`Prop::Auto`] (it follows the cells, as derivation does) or
//! [`Prop::Set`] (the user put it there and it stays). It never needs to know
//! whether a wall is a derived face with an edge override or an authored
//! record:
//!
//! * **Reading** ([`walls_at`]) lists every wall on an edge of a leaf, per
//!   storey, derived and authored alike, with the same fields.
//! * **Setting a height** on a generated wall materialises it: the derived
//!   face is hidden (a ghost, so its collision goes with it) and an authored
//!   record with the heights asked for takes its place, with the material and
//!   collision the face had. From then on it stays where it was put when the
//!   cells move. [`clear_wall_property`] on its heights removes the record and
//!   gives the face back to derivation. On a hand-placed wall a height edit is
//!   the same write to the record, so both give the same result.
//! * **Gaps are reported, never refused**: a top below the ceiling the room
//!   has, or a bottom above the floor, comes back as a [`WallWarning`].
//! * **Storeys**: the edge override is keyed by edge, not by storey, so
//!   hiding a derived face also hides the faces of the other storeys on that
//!   segment. Every such face is written back as an authored record first,
//!   so only the storey asked for changes.
//!
//! What stays split in storage: derived faces and authored records are still
//! two stores (the review's list). The API hides that for reads and writes;
//! what a caller can still notice is that a Set height wall no longer rises
//! with the floor (that is what Set means), and that the derived face it
//! replaced is kept hidden under it.

use crate::grid::derive::{self, DerivedKind, DerivedWall};
use crate::grid::leaf::{LeafKey, SECTOR_LEVEL};
use crate::grid::leaf_derive::derived_walls_of;
use crate::grid::limits::MAX_WALLS_PER_EDGE;
use crate::grid::sector::WallEdge;
use crate::grid::surface::{GridWall, WallFacing};
use crate::grid::units::FINE_UNITS;
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::verbs::opening::{cut_opening_on_span, OpeningKind, OpeningParams, OpeningProfile};

/// A property that follows the cells or was set by the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prop<T> {
    /// Derived from the cells.
    Auto(T),
    /// Put there by the user.
    Set(T),
}

impl<T: Copy> Prop<T> {
    /// The value, whichever it is.
    pub fn value(&self) -> T {
        match self {
            Self::Auto(v) | Self::Set(v) => *v,
        }
    }

    /// Whether it was set.
    pub fn is_set(&self) -> bool {
        matches!(self, Self::Set(_))
    }
}

/// Where a wall is, whatever it is stored as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallAddr {
    /// The room.
    pub room: usize,
    /// The leaf the wall is seen from.
    pub leaf: LeafKey,
    /// The side of that leaf.
    pub side: WallEdge,
    /// The storey (span of the column), 0 for the ground one.
    pub span: u8,
    /// Position among the walls of that kind on the side.
    pub index: usize,
    /// Whether it is an authored record (else a derived face).
    pub authored: bool,
}

/// A wall property, for [`clear_wall_property`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallProp {
    /// Corner heights.
    Heights,
    /// Material.
    Material,
    /// The opening cut in the wall.
    Opening,
    /// Collision.
    Solid,
    /// Band set.
    Bands,
}

/// An opening seen on a wall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpeningView {
    /// Whether the segment is hidden and passable (a doorway or window cut).
    pub open: bool,
}

/// What a wall looks like to a caller.
#[derive(Clone, Debug, PartialEq)]
pub struct WallView {
    /// Where it is.
    pub addr: WallAddr,
    /// `[bottom a, bottom b, top b, top a]`, room relative.
    pub heights: Prop<[i32; 4]>,
    /// Material.
    pub material: Prop<Option<ResourceId>>,
    /// Collision.
    pub solid: Prop<bool>,
    /// Band set.
    pub bands: Prop<Option<u8>>,
    /// An opening cut in it.
    pub opening: Option<OpeningView>,
    /// Whether another room has a wall on the same physical edge.
    pub both_sides: bool,
}

/// Something a write left open. Never a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallWarning {
    /// The top is this far under the ceiling at its highest point.
    GapAbove(i32),
    /// The bottom is this far over the floor at its lowest point.
    GapBelow(i32),
}

/// Why a write changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WallRefusal {
    /// No such wall.
    NoWall,
    /// The face is not a rectangle (a triangle): it has no four corners.
    NotRectangular,
    /// The edge holds as many walls as it may.
    TooManyWalls,
    /// The leaf is larger than a sector and carries no authored walls.
    BigLeaf,
    /// The opening was refused.
    Opening(String),
}

fn derived_on(world: &GridWorld, room: usize, leaf: LeafKey, side: WallEdge) -> Vec<DerivedWall> {
    let Some(r) = world.rooms.get(room) else {
        return Vec::new();
    };
    let Some(sector) = r.leaf(leaf) else {
        return Vec::new();
    };
    let tall = leaf.level == SECTOR_LEVEL && sector.span_count() > 1;
    let all = if tall {
        derive::edge_walls(r, leaf.cell(), side)
    } else {
        derived_walls_of(r, leaf)
    };
    all.into_iter()
        .filter(|w| w.edge == side && w.kind != DerivedKind::Slab)
        .collect()
}

fn span_of(sector: &crate::grid::sector::GridSector, heights: &[i32; 4]) -> u8 {
    let mid = (heights[0] + heights[1] + heights[2] + heights[3]) / 4;
    sector.span_at(mid, 0).unwrap_or(0).min(255) as u8
}

/// Every wall on `side` of `leaf`, per storey, derived and authored alike.
pub fn walls_at(world: &GridWorld, room: usize, leaf: LeafKey, side: WallEdge) -> Vec<WallView> {
    let Some(r) = world.rooms.get(room) else {
        return Vec::new();
    };
    let Some(sector) = r.leaf(leaf) else {
        return Vec::new();
    };
    let d = side.front_direction();
    let probe = [
        leaf.x
            + if d[0] > 0 {
                -1
            } else if d[0] < 0 {
                leaf.edge_clicks()
            } else {
                0
            },
        leaf.z
            + if d[1] > 0 {
                -1
            } else if d[1] < 0 {
                leaf.edge_clicks()
            } else {
                0
            },
    ];
    let both_sides = world
        .rooms
        .iter()
        .enumerate()
        .any(|(j, o)| j != room && o.leaf_at(probe).is_some());
    let mut out = Vec::new();
    for (i, w) in derived_on(world, room, leaf, side).into_iter().enumerate() {
        // The face a Set height replaced stays hidden under its record; it is
        // not a second wall. (A door's hidden face has a lintel above the
        // opening and a window's has a sill and a lintel, so neither is one
        // record reaching the floor.)
        if w.hidden && w.solid == Some(false) {
            let floor = w.edge_heights().map_or(0, |h| h[0].min(h[1]));
            let in_span: Vec<_> = sector
                .walls(side)
                .iter()
                .filter(|a| span_of(sector, &a.heights) == w.span)
                .collect();
            let replaced = in_span.len() == 1
                && in_span[0].heights[0].min(in_span[0].heights[1]) <= floor + FINE_UNITS;
            if replaced {
                continue;
            }
        }
        let o = r.edge_overrides.get(&w.key);
        let heights = w.edge_heights().unwrap_or([0; 4]);
        out.push(WallView {
            addr: WallAddr {
                room,
                leaf,
                side,
                span: w.span,
                index: i,
                authored: false,
            },
            heights: Prop::Auto(heights),
            material: match o.and_then(|o| o.material) {
                Some(m) => Prop::Set(Some(m)),
                None => Prop::Auto(w.material),
            },
            solid: match w.solid {
                Some(v) => Prop::Set(v),
                None => Prop::Auto(w.blocks()),
            },
            bands: match w.band_set {
                Some(b) => Prop::Set(Some(b)),
                None => Prop::Auto(None),
            },
            opening: (w.hidden && w.solid == Some(false)).then_some(OpeningView { open: true }),
            both_sides,
        });
    }
    for (i, w) in sector.walls(side).iter().enumerate() {
        out.push(WallView {
            addr: WallAddr {
                room,
                leaf,
                side,
                span: span_of(sector, &w.heights),
                index: i,
                authored: true,
            },
            heights: Prop::Set(w.heights),
            material: Prop::Set(w.material),
            solid: Prop::Set(w.solid),
            bands: Prop::Set(w.band_set),
            opening: None,
            both_sides,
        });
    }
    out
}

/// The room's ceiling and floor edge heights at `side` of `leaf` for storey
/// `span`, `([floor a, floor b], [ceiling a, ceiling b])`.
/// Floor and ceiling edge heights of one storey.
type SpanEdges = (Option<[i32; 2]>, Option<[i32; 2]>);

fn span_edges(world: &GridWorld, addr: &WallAddr) -> Option<SpanEdges> {
    let sector = world.rooms.get(addr.room)?.leaf(addr.leaf)?;
    let span = sector.spans().get(usize::from(addr.span))?;
    let f = span
        .floor
        .as_ref()
        .map(|f| super::walls::surface_edge_heights(f, addr.side));
    let c = span
        .ceiling
        .as_ref()
        .map(|c| super::walls::surface_edge_heights(c, addr.side));
    Some((f, c))
}

fn warnings(world: &GridWorld, addr: &WallAddr, h: &[i32; 4]) -> Vec<WallWarning> {
    let mut out = Vec::new();
    let Some((floor, ceiling)) = span_edges(world, addr) else {
        return out;
    };
    if let Some(c) = ceiling {
        let top = h[2].min(h[3]);
        let ceil = c[0].max(c[1]);
        if top < ceil {
            out.push(WallWarning::GapAbove(ceil - top));
        }
    }
    if let Some(f) = floor {
        let bottom = h[0].max(h[1]);
        let floor_lo = f[0].min(f[1]);
        if bottom > floor_lo {
            out.push(WallWarning::GapBelow(bottom - floor_lo));
        }
    }
    out
}

/// Write back every derived face of `leaf`'s `side` other than the one at
/// `keep` (its key) of the other storeys as authored records, so hiding the
/// segment's override leaves them standing.
fn pin_other_storeys(world: &mut GridWorld, addr: &WallAddr, key: crate::grid::overrides::EdgeKey) {
    let walls = derived_on(world, addr.room, addr.leaf, addr.side);
    let Some(room) = world.rooms.get_mut(addr.room) else {
        return;
    };
    for w in walls {
        if w.key != key || w.span == addr.span {
            continue;
        }
        let Some(h) = w.edge_heights() else { continue };
        let rec = GridWall {
            heights: h,
            material: w.material,
            facing: WallFacing::Front,
            solid: w.blocks(),
            ..GridWall::span(0, 0, None)
        };
        if let Some(s) = room.leaf_mut(addr.leaf) {
            s.walls_mut(addr.side).push(rec);
        }
    }
}

/// Set any of the four corner heights of a wall (`[bottom a, bottom b, top b,
/// top a]`; `None` keeps that corner). Slide, stretch and slope are all this
/// one call. Returns the wall's address afterwards (a generated wall becomes
/// an authored record) and what the heights leave open.
pub fn set_wall_heights(
    world: &mut GridWorld,
    addr: WallAddr,
    corners: [Option<i32>; 4],
) -> Result<(WallAddr, Vec<WallWarning>), WallRefusal> {
    let views = walls_at(world, addr.room, addr.leaf, addr.side);
    let view = views
        .iter()
        .find(|v| v.addr == addr)
        .ok_or(WallRefusal::NoWall)?;
    let base = view.heights.value();
    let mut h = base;
    for i in 0..4 {
        if let Some(v) = corners[i] {
            h[i] = v;
        }
    }
    if addr.authored {
        let room = &mut world.rooms[addr.room];
        let s = room.leaf_mut(addr.leaf).ok_or(WallRefusal::NoWall)?;
        s.walls_mut(addr.side)
            .get_mut(addr.index)
            .ok_or(WallRefusal::NoWall)?
            .heights = h;
        let w = warnings(world, &addr, &h);
        return Ok((addr, w));
    }
    // Materialise the generated wall.
    let derived = derived_on(world, addr.room, addr.leaf, addr.side);
    let dw = derived.get(addr.index).ok_or(WallRefusal::NoWall)?.clone();
    if dw.edge_heights().is_none() {
        return Err(WallRefusal::NotRectangular);
    }
    if world.rooms[addr.room]
        .leaf(addr.leaf)
        .ok_or(WallRefusal::NoWall)?
        .walls(addr.side)
        .len()
        >= MAX_WALLS_PER_EDGE
    {
        return Err(WallRefusal::TooManyWalls);
    }
    pin_other_storeys(world, &addr, dw.key);
    let room = &mut world.rooms[addr.room];
    room.edge_overrides.edit(dw.key, |o| {
        o.hidden = true;
        o.solid = Some(false);
    });
    let s = room.leaf_mut(addr.leaf).ok_or(WallRefusal::NoWall)?;
    s.walls_mut(addr.side).push(GridWall {
        heights: h,
        material: dw.material,
        facing: WallFacing::Front,
        solid: dw.blocks(),
        uv: dw.uv.unwrap_or_default(),
        band_set: dw.band_set,
    });
    let index = s.walls(addr.side).len() - 1;
    let new = WallAddr {
        authored: true,
        index,
        ..addr
    };
    let w = warnings(world, &new, &h);
    Ok((new, w))
}

/// Set the material of a wall.
pub fn set_wall_material(
    world: &mut GridWorld,
    addr: WallAddr,
    material: Option<ResourceId>,
) -> Result<(), WallRefusal> {
    let room = world.rooms.get_mut(addr.room).ok_or(WallRefusal::NoWall)?;
    if addr.authored {
        let s = room.leaf_mut(addr.leaf).ok_or(WallRefusal::NoWall)?;
        s.walls_mut(addr.side)
            .get_mut(addr.index)
            .ok_or(WallRefusal::NoWall)?
            .material = material;
        return Ok(());
    }
    let derived = derived_on(world, addr.room, addr.leaf, addr.side);
    let key = derived.get(addr.index).ok_or(WallRefusal::NoWall)?.key;
    world.rooms[addr.room]
        .edge_overrides
        .edit(key, |o| o.material = material);
    Ok(())
}

/// What to cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpeningSpec {
    /// Door or window.
    pub kind: OpeningKind,
    /// Start along the wall, world clicks.
    pub from: i32,
    /// End along the wall, world clicks.
    pub to: i32,
    /// Top of the opening above the floor (the apex of an arch).
    pub top: i32,
    /// Flat or arched head.
    pub profile: OpeningProfile,
    /// Material of the sill and lintel.
    pub material: Option<ResourceId>,
}

/// Cut an opening in a wall (`Some`), or close one (`None`).
pub fn set_wall_opening(
    world: &mut GridWorld,
    addr: WallAddr,
    opening: Option<OpeningSpec>,
) -> Result<Vec<WallWarning>, WallRefusal> {
    match opening {
        None => {
            clear_wall_property(world, addr, WallProp::Opening)?;
            Ok(Vec::new())
        }
        Some(o) => {
            let p = OpeningParams {
                profile: o.profile,
                room: addr.room,
                cell: addr.leaf.cell(),
                side: addr.side,
                from: o.from,
                to: o.to,
                kind: o.kind,
                top: o.top,
                material: o.material,
            };
            cut_opening_on_span(world, &p, addr.span)
                .map_err(|e| WallRefusal::Opening(e.describe()))?;
            Ok(Vec::new())
        }
    }
}

/// Take a property back to what the cells give: heights remove the record a
/// Set height made and unhide the face; material, collision and bands clear
/// the override; an opening is closed (its sill and lintel records go and the
/// segment is shown again).
pub fn clear_wall_property(
    world: &mut GridWorld,
    addr: WallAddr,
    prop: WallProp,
) -> Result<(), WallRefusal> {
    let side_walls = derived_on(world, addr.room, addr.leaf, addr.side);
    match prop {
        WallProp::Heights | WallProp::Opening => {
            // The derived face a record replaced: hidden segments of this side.
            let hidden: Vec<DerivedWall> =
                side_walls.iter().filter(|w| w.hidden).cloned().collect();
            let room = world.rooms.get_mut(addr.room).ok_or(WallRefusal::NoWall)?;
            if prop == WallProp::Heights && addr.authored {
                let s = room.leaf_mut(addr.leaf).ok_or(WallRefusal::NoWall)?;
                let stack = s.walls_mut(addr.side);
                if addr.index >= stack.len() {
                    return Err(WallRefusal::NoWall);
                }
                stack.remove(addr.index);
            } else if prop == WallProp::Opening {
                let s = room.leaf_mut(addr.leaf).ok_or(WallRefusal::NoWall)?;
                s.walls_mut(addr.side).clear();
            }
            for w in hidden {
                room.edge_overrides.edit(w.key, |o| {
                    o.hidden = false;
                    o.solid = None;
                });
            }
            Ok(())
        }
        _ => {
            let key = side_walls.get(addr.index).ok_or(WallRefusal::NoWall)?.key;
            world.rooms[addr.room]
                .edge_overrides
                .edit(key, |o| match prop {
                    WallProp::Material => o.material = None,
                    WallProp::Solid => o.solid = None,
                    _ => o.band_set = None,
                });
            Ok(())
        }
    }
}

/// Stand a wall where none is generated: a partition or a parapet.
pub fn add_wall(
    world: &mut GridWorld,
    room: usize,
    leaf: LeafKey,
    side: WallEdge,
    heights: [i32; 4],
    material: Option<ResourceId>,
) -> Result<WallAddr, WallRefusal> {
    let r = world.rooms.get_mut(room).ok_or(WallRefusal::NoWall)?;
    if leaf.level < SECTOR_LEVEL {
        return Err(WallRefusal::BigLeaf);
    }
    let s = r.leaf_mut(leaf).ok_or(WallRefusal::NoWall)?;
    if s.walls(side).len() >= MAX_WALLS_PER_EDGE {
        return Err(WallRefusal::TooManyWalls);
    }
    s.walls_mut(side).push(GridWall {
        heights,
        material,
        facing: WallFacing::Both,
        ..GridWall::span(0, 0, None)
    });
    let index = s.walls(side).len() - 1;
    let span = span_of(s, &heights);
    Ok(WallAddr {
        room,
        leaf,
        side,
        span,
        index,
        authored: true,
    })
}

/// Remove a wall: an authored record goes; a generated face is hidden and
/// made passable (the wall is gone, the cells are as they were).
pub fn remove_wall(world: &mut GridWorld, addr: WallAddr) -> Result<(), WallRefusal> {
    if addr.authored {
        let room = world.rooms.get_mut(addr.room).ok_or(WallRefusal::NoWall)?;
        let s = room.leaf_mut(addr.leaf).ok_or(WallRefusal::NoWall)?;
        let stack = s.walls_mut(addr.side);
        if addr.index >= stack.len() {
            return Err(WallRefusal::NoWall);
        }
        stack.remove(addr.index);
        return Ok(());
    }
    let derived = derived_on(world, addr.room, addr.leaf, addr.side);
    let dw = derived.get(addr.index).ok_or(WallRefusal::NoWall)?.clone();
    pin_other_storeys(world, &addr, dw.key);
    world.rooms[addr.room].edge_overrides.edit(dw.key, |o| {
        o.hidden = true;
        o.solid = Some(false);
    });
    Ok(())
}
