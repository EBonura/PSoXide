//! Bridge: a straight walkable deck between two points at given heights over
//! open space, with an optional parapet and piers.
//!
//! **What it is made of, and why.** The deck is one static mesh instance of
//! the existing span recipe ([`crate::grid::mesh_span::SpanSpec`]), not a row
//! of floor leaves and not a free plane. Against leaves: a deck of leaves
//! needs a floor, closure walls and an open wall height on both flanks of
//! every sector, so a 12 sector bridge costs dozens of faces and collision
//! records and the height is tied to the 64 unit lattice cell by cell; the
//! span is six faces and one collision slab, and may climb at any rise.
//! Against a free plane: a plane collides as a single surface with no
//! underside, so the bridge would be paper thin and see through from below;
//! the span is a closed prism with a thick slab, classified at the cook like
//! a plane (a platform under the walkable slope), so the player stands on it
//! the same way. A parapet is two narrow spans on the edges, taller than the
//! motor's step-up so the body cannot step over; piers are columns under the
//! deck at the given spacing. Everything is integer; a bridge that is not
//! along X or Z uses the existing span solver's heading rounding.

use psx_engine::WalkLimits;

use crate::grid::cook::RuntimeUnits;
use crate::grid::mesh::{ColumnSpec, GridMesh, MeshAsset, MeshKind, MAX_MESHES_PER_ROOM};
use crate::grid::mesh_span::SpanSpec;
use crate::grid::world::GridWorld;

use super::super::meshes::{add_asset, place_mesh, solve_span, MeshRefusal};
use super::stairs::max_riser;

/// Parameters of a bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeParams {
    /// The room the instances go in.
    pub room: usize,
    /// Start of the deck top: `[x, y, z]`, grid units, `y` room relative.
    pub from: [i32; 3],
    /// End of the deck top.
    pub to: [i32; 3],
    /// Deck width, grid units.
    pub width: i32,
    /// Deck thickness, grid units.
    pub thickness: i32,
    /// Parapet height above the deck, if any.
    pub parapet: Option<i32>,
    /// Width of a parapet wall.
    pub parapet_width: i32,
    /// Distance between piers, if any.
    pub pier_spacing: Option<i32>,
    /// Height the piers stand on (the ground below), room relative.
    pub pier_foot: i32,
    /// The pier recipe (its height is solved per pier).
    pub pier: ColumnSpec,
}

impl BridgeParams {
    /// A plain deck of `width` and `thickness` between two points.
    pub fn deck(room: usize, from: [i32; 3], to: [i32; 3], width: i32, thickness: i32) -> Self {
        Self {
            room,
            from,
            to,
            width,
            thickness,
            parapet: None,
            parapet_width: 128,
            pier_spacing: None,
            pier_foot: 0,
            pier: ColumnSpec::default(),
        }
    }
}

/// Why a bridge was not built. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeRefusal {
    /// The room does not exist.
    NoSuchRoom,
    /// The end points are the same in plan.
    NoLength,
    /// The deck would rise faster than a body walks.
    TooSteep {
        /// Most rise over this length, grid units.
        max_rise: i32,
    },
    /// The parapet is low enough to step over.
    ParapetTooLow {
        /// Least height, grid units.
        least: i32,
    },
    /// A pier would be too short or too tall for the column recipe.
    PierOutOfRange {
        /// The height it needed, grid units.
        needed: i32,
    },
    /// The room would hold more instances than it may.
    TooMany,
    /// A recipe is outside its limits.
    BadRecipe(String),
    /// The room would pass its size cap.
    RoomTooLarge,
}

impl BridgeRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::NoLength => "The two ends are in the same place".to_string(),
            Self::TooSteep { max_rise } => {
                format!("Too steep to walk: at most {max_rise} units of rise over that length")
            }
            Self::ParapetTooLow { least } => {
                format!("A parapet must be at least {least} units tall or the body steps over it")
            }
            Self::PierOutOfRange { needed } => {
                format!("A pier would need to be {needed} units: outside the column's range")
            }
            Self::TooMany => format!("A room holds at most {MAX_MESHES_PER_ROOM} meshes"),
            Self::BadRecipe(why) => why.clone(),
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
        }
    }
}

/// What a bridge made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BridgeOutcome {
    /// The deck instance.
    pub deck: u16,
    /// The parapet instances (none, or two).
    pub parapets: Vec<u16>,
    /// The pier instances.
    pub piers: Vec<u16>,
    /// Length in plan, grid units.
    pub length: i32,
    /// Rise, grid units.
    pub rise: i32,
}

fn convert(e: MeshRefusal) -> BridgeRefusal {
    match e {
        MeshRefusal::NoRoom => BridgeRefusal::NoSuchRoom,
        MeshRefusal::TooMany => BridgeRefusal::TooMany,
        MeshRefusal::RoomTooLarge => BridgeRefusal::RoomTooLarge,
        other => BridgeRefusal::BadRecipe(other.describe()),
    }
}

/// Build a bridge. One logical edit: the caller snapshots the document once.
pub fn build_bridge(
    world: &mut GridWorld,
    p: &BridgeParams,
) -> Result<BridgeOutcome, BridgeRefusal> {
    if p.room >= world.rooms.len() {
        return Err(BridgeRefusal::NoSuchRoom);
    }
    let (dx, dz) = (
        i64::from(p.to[0] - p.from[0]),
        i64::from(p.to[2] - p.from[2]),
    );
    let length = ((dx * dx + dz * dz) as f64).sqrt().round() as i32;
    if length < 1 {
        return Err(BridgeRefusal::NoLength);
    }
    let rise = p.to[1] - p.from[1];
    // A slope of at most 19 in 20, under the motor's walkable normal.
    let max_rise = length * 19 / 20;
    if rise.abs() > max_rise {
        return Err(BridgeRefusal::TooSteep { max_rise });
    }
    let units = RuntimeUnits::new(world.scale).ok();
    let least = units
        .as_ref()
        .map_or(512, |u| max_riser(u, &WalkLimits::DEFAULT))
        + 64;
    if let Some(h) = p.parapet {
        if h < least {
            return Err(BridgeRefusal::ParapetTooLow { least });
        }
    }
    let mut work = world.clone();
    let mut out = BridgeOutcome {
        length,
        rise,
        ..BridgeOutcome::default()
    };
    // Unit vector across the deck, integer: the deck's own direction turned
    // a quarter, scaled to the lateral offset.
    let lateral = |off: i32| -> [i32; 2] {
        let (ux, uz) = (dx as f64 / f64::from(length), dz as f64 / f64::from(length));
        [
            (-uz * f64::from(off)).round() as i32,
            (ux * f64::from(off)).round() as i32,
        ]
    };
    let put = |w: &mut GridWorld,
               from: [i32; 3],
               to: [i32; 3],
               spec: SpanSpec,
               name: &str|
     -> Result<u16, BridgeRefusal> {
        let placement = solve_span(from, to, spec).map_err(convert)?;
        let asset = add_asset(
            w,
            MeshAsset::new(name.to_string(), MeshKind::Span(placement.spec)),
        )
        .map_err(convert)?;
        let mut mesh = GridMesh::new(0, asset, placement.position);
        mesh.rotation = [placement.yaw, 0, 0];
        place_mesh(w, p.room, mesh).map_err(convert)
    };
    let deck = SpanSpec {
        length,
        width: p.width,
        thickness: p.thickness,
        rise,
    };
    out.deck = put(&mut work, p.from, p.to, deck, "bridge deck")?;
    if let Some(h) = p.parapet {
        let off = p.width / 2 - p.parapet_width / 2;
        for side in [-1, 1] {
            let l = lateral(side * off);
            let lift = |q: [i32; 3]| [q[0] + l[0], q[1] + h, q[2] + l[1]];
            let spec = SpanSpec {
                length,
                width: p.parapet_width,
                thickness: h,
                rise,
            };
            out.parapets
                .push(put(&mut work, lift(p.from), lift(p.to), spec, "parapet")?);
        }
    }
    if let Some(spacing) = p.pier_spacing {
        let spacing = spacing.max(256);
        let mut d = spacing;
        while d < length - spacing / 2 {
            let top = p.from[1] + (i64::from(rise) * i64::from(d) / i64::from(length)) as i32;
            let height = top - p.thickness - p.pier_foot;
            let mut column = p.pier;
            column.height = height;
            if column.check().is_err() {
                return Err(BridgeRefusal::PierOutOfRange { needed: height });
            }
            let at = [
                p.from[0] + (dx * i64::from(d) / i64::from(length)) as i32,
                p.from[2] + (dz * i64::from(d) / i64::from(length)) as i32,
            ];
            let asset = add_asset(&mut work, MeshAsset::new("pier", MeshKind::Column(column)))
                .map_err(convert)?;
            let mesh = GridMesh::new(0, asset, [at[0], p.pier_foot, at[1]]);
            out.piers
                .push(place_mesh(&mut work, p.room, mesh).map_err(convert)?);
            d += spacing;
        }
    }
    *world = work;
    Ok(out)
}
