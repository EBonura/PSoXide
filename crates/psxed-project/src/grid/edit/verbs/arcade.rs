//! Arcade and Colonnade: a run of arch-span mesh instances, or of columns,
//! along a line, along a wall run or across a corridor.
//!
//! Each bay is the single-arch recipe of [`super::super::meshes::arch_span`]
//! reused as is: one shared asset, one instance per bay, the crown hook
//! (`perch`) and the walkable top proxy of the recipe untouched, so a player
//! perches on the crown of every bay. A bay's piers stand outside its clear
//! opening, so bays are spaced `span + 2 * pier` apart or more and adjacent
//! piers touch instead of overlapping. Positions are integers; a line that is
//! not along X or Z uses the recipe's own heading rounding (the existing
//! arch solver), axis-aligned runs are exact.
//!
//! A [`Colonnade`] is the piers only: one column asset and `bays + 1`
//! instances on the same layout.

use crate::grid::mesh::{ColumnSpec, GridMesh, MeshAsset, MeshKind, MAX_MESHES_PER_ROOM};
use crate::grid::mesh_arch::{ArchProfile, ArchSpec, DEPTH_RANGE};
use crate::grid::sector::WallEdge;
use crate::grid::units::SECTOR_UNITS;
use crate::grid::world::GridWorld;

use super::super::meshes::{add_asset, place_mesh, solve_arch_span, MeshRefusal};

/// Where the bays go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Along the line from `from` to `to`, grid units `[x, z]` of the pier
    /// centre lines at its two ends.
    Line {
        /// Start.
        from: [i32; 2],
        /// End.
        to: [i32; 2],
    },
    /// Along a run of sectors on one side: from the start of `first` to the
    /// end of `last` (a row of cells along the side's axis), the centre line
    /// `inset` grid units inside the wall.
    Wall {
        /// First sector of the run.
        first: [i32; 2],
        /// Last sector of the run.
        last: [i32; 2],
        /// Which side's wall the run follows.
        side: WallEdge,
        /// Distance of the centre line from the wall, grid units.
        inset: i32,
    },
    /// One arch across a corridor that runs along `along`: at the middle of
    /// `cell`, spanning `width` sectors from wall to wall.
    Across {
        /// The sector the arch stands in.
        cell: [i32; 2],
        /// The axis the corridor runs along (`X` or `Z`).
        along: crate::grid::overrides::Axis,
        /// Corridor width in sectors.
        width: i32,
    },
}

/// Parameters of an arcade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArcadeParams {
    /// The room the instances go in.
    pub room: usize,
    /// Where.
    pub layout: Layout,
    /// Floor height under the piers, grid units relative to the room.
    pub base: i32,
    /// Bays (ignored for `Across`, which is one).
    pub bays: i32,
    /// Clear opening of a bay, grid units; `None` fills the pitch.
    pub span: Option<i32>,
    /// Width of a pier (and thickness of the band), grid units.
    pub pier: i32,
    /// Height of the springing line above the base.
    pub spring: i32,
    /// Crown rise above the springing line; `None` takes the profile's.
    pub rise: Option<i32>,
    /// Curve.
    pub profile: ArchProfile,
    /// Depth of the band along the wall.
    pub depth: i32,
    /// Whether each crown carries a hook socket.
    pub perch: bool,
}

impl ArcadeParams {
    /// A four bay round arcade of a sector pitch from `from` to `to`.
    pub fn line(room: usize, from: [i32; 2], to: [i32; 2], base: i32, bays: i32) -> Self {
        Self {
            room,
            layout: Layout::Line { from, to },
            base,
            bays,
            span: None,
            pier: 256,
            spring: 1536,
            rise: None,
            profile: ArchProfile::Round,
            depth: 1024,
            perch: true,
        }
    }
}

/// Why an arcade was not built. The document is untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArcadeRefusal {
    /// The room does not exist.
    NoSuchRoom,
    /// No bays, or the line has no length.
    NothingToBuild,
    /// The piers of neighbouring bays would overlap.
    PiersOverlap {
        /// Least pitch for this span and pier, grid units.
        least_pitch: i32,
    },
    /// The room would hold more instances than it may.
    TooMany,
    /// The recipe is outside its limits.
    BadRecipe(String),
    /// A wall layout was asked along a diagonal side.
    NotCardinal,
    /// The room would pass its size cap.
    RoomTooLarge,
}

impl ArcadeRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoSuchRoom => "No such room".to_string(),
            Self::NothingToBuild => "An arcade needs a bay and a line with length".to_string(),
            Self::PiersOverlap { least_pitch } => {
                format!("Piers overlap: bays need a pitch of at least {least_pitch} units")
            }
            Self::TooMany => format!("A room holds at most {MAX_MESHES_PER_ROOM} meshes"),
            Self::BadRecipe(why) => why.clone(),
            Self::NotCardinal => "Wall runs follow a north, east, south or west side".to_string(),
            Self::RoomTooLarge => "The room would grow past its size limit".to_string(),
        }
    }
}

/// What an arcade made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArcadeOutcome {
    /// Instance ids, in bay order (piers for a colonnade).
    pub instances: Vec<u16>,
    /// The asset index every instance shares.
    pub asset: u16,
    /// The pitch between bay (or pier) centres, grid units.
    pub pitch: i32,
    /// Centre of each bay (each pier), `[x, z]` grid units.
    pub centres: Vec<[i32; 2]>,
}

fn convert(e: MeshRefusal) -> ArcadeRefusal {
    match e {
        MeshRefusal::NoRoom => ArcadeRefusal::NoSuchRoom,
        MeshRefusal::TooMany => ArcadeRefusal::TooMany,
        MeshRefusal::RoomTooLarge => ArcadeRefusal::RoomTooLarge,
        other => ArcadeRefusal::BadRecipe(other.describe()),
    }
}

/// The two ends of the centre line and the bay count a layout names.
fn line_of(layout: Layout, bays: i32) -> Result<([i32; 2], [i32; 2], i32), ArcadeRefusal> {
    Ok(match layout {
        Layout::Line { from, to } => (from, to, bays),
        Layout::Wall {
            first,
            last,
            side,
            inset,
        } => {
            let s = SECTOR_UNITS;
            let (lo, hi) = (
                [first[0].min(last[0]), first[1].min(last[1])],
                [first[0].max(last[0]), first[1].max(last[1])],
            );
            let (a, b) = match side {
                WallEdge::North => (
                    [lo[0] * s, lo[1] * s + inset],
                    [(hi[0] + 1) * s, lo[1] * s + inset],
                ),
                WallEdge::South => (
                    [lo[0] * s, (hi[1] + 1) * s - inset],
                    [(hi[0] + 1) * s, (hi[1] + 1) * s - inset],
                ),
                WallEdge::West => (
                    [lo[0] * s + inset, lo[1] * s],
                    [lo[0] * s + inset, (hi[1] + 1) * s],
                ),
                WallEdge::East => (
                    [(hi[0] + 1) * s - inset, lo[1] * s],
                    [(hi[0] + 1) * s - inset, (hi[1] + 1) * s],
                ),
                _ => return Err(ArcadeRefusal::NotCardinal),
            };
            (a, b, bays)
        }
        Layout::Across { cell, along, width } => {
            let s = SECTOR_UNITS;
            let mid = [cell[0] * s + s / 2, cell[1] * s + s / 2];
            let half = width * s / 2;
            // The opening runs across the corridor.
            match along {
                crate::grid::overrides::Axis::X => {
                    ([mid[0], mid[1] - half], [mid[0], mid[1] + half], 1)
                }
                _ => ([mid[0] - half, mid[1]], [mid[0] + half, mid[1]], 1),
            }
        }
    })
}

/// Centres of `n` equal bays on the line.
fn centres(a: [i32; 2], b: [i32; 2], n: i32) -> (Vec<[i32; 2]>, i32) {
    let (dx, dz) = (i64::from(b[0] - a[0]), i64::from(b[1] - a[1]));
    let len = ((dx * dx + dz * dz) as f64).sqrt().round() as i32;
    let pitch = len / n.max(1);
    let out = (0..n)
        .map(|i| {
            let num = i64::from(2 * i + 1);
            let den = i64::from(2 * n);
            [
                a[0] + ((dx * num + den / 2 * dx.signum()) / den) as i32,
                a[1] + ((dz * num + den / 2 * dz.signum()) / den) as i32,
            ]
        })
        .collect();
    (out, pitch)
}

/// Build an arcade. One logical edit: the caller snapshots the document once.
pub fn build_arcade(
    world: &mut GridWorld,
    p: &ArcadeParams,
) -> Result<ArcadeOutcome, ArcadeRefusal> {
    if p.room >= world.rooms.len() {
        return Err(ArcadeRefusal::NoSuchRoom);
    }
    let (a, b, bays) = line_of(p.layout, p.bays)?;
    if bays < 1 || a == b {
        return Err(ArcadeRefusal::NothingToBuild);
    }
    let (centres, pitch) = centres(a, b, bays);
    let span = p.span.unwrap_or(pitch - 2 * p.pier);
    let least_pitch = span + 2 * p.pier;
    if bays > 1 && pitch < least_pitch {
        return Err(ArcadeRefusal::PiersOverlap { least_pitch });
    }
    let spec = bay_spec(p, span);
    spec.check().map_err(ArcadeRefusal::BadRecipe)?;
    let mut work = world.clone();
    let dir = {
        let (dx, dz) = (f64::from(b[0] - a[0]), f64::from(b[1] - a[1]));
        let l = dx.hypot(dz);
        [dx / l, dz / l]
    };
    let half = span / 2;
    let mut ids = Vec::new();
    let mut asset = 0;
    for c in &centres {
        let off = |s: i32| {
            [
                c[0] + (f64::from(s) * dir[0]).round() as i32,
                c[1] + (f64::from(s) * dir[1]).round() as i32,
            ]
        };
        let (f, t) = (off(-half), off(half));
        let placement =
            solve_arch_span([f[0], p.base, f[1]], [t[0], p.base, t[1]], spec).map_err(convert)?;
        let label = format!(
            "{} arch {}",
            placement.spec.profile.label(),
            placement.spec.span
        );
        asset = add_asset(
            &mut work,
            MeshAsset::new(label, MeshKind::Arch(placement.spec)),
        )
        .map_err(convert)?;
        let mut mesh = GridMesh::new(0, asset, placement.position);
        mesh.rotation = [placement.yaw, 0, 0];
        ids.push(place_mesh(&mut work, p.room, mesh).map_err(convert)?);
    }
    *world = work;
    Ok(ArcadeOutcome {
        instances: ids,
        asset,
        pitch,
        centres,
    })
}

/// The recipe of one bay.
fn bay_spec(p: &ArcadeParams, span: i32) -> ArchSpec {
    let base = ArchSpec::spanning(p.profile, span);
    ArchSpec {
        profile: p.profile,
        span,
        rise: p.rise.unwrap_or(base.rise),
        spring: p.spring,
        thickness: p.pier,
        depth: p.depth.clamp(DEPTH_RANGE.0, DEPTH_RANGE.1),
        segments: base.segments,
        perch: p.perch,
    }
}

/// Parameters of a colonnade: piers only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colonnade {
    /// The room.
    pub room: usize,
    /// Where (the bays' pier centre lines).
    pub layout: Layout,
    /// Floor height under the columns.
    pub base: i32,
    /// Bays; the colonnade has `bays + 1` columns.
    pub bays: i32,
    /// The column recipe.
    pub column: ColumnSpec,
}

/// Build a colonnade: `bays + 1` columns on the pier lines of the layout.
pub fn build_colonnade(
    world: &mut GridWorld,
    c: &Colonnade,
) -> Result<ArcadeOutcome, ArcadeRefusal> {
    if c.room >= world.rooms.len() {
        return Err(ArcadeRefusal::NoSuchRoom);
    }
    let (a, b, bays) = line_of(c.layout, c.bays)?;
    if bays < 1 || a == b {
        return Err(ArcadeRefusal::NothingToBuild);
    }
    c.column.check().map_err(ArcadeRefusal::BadRecipe)?;
    // Pier lines: the bay boundaries, so the end points are columns.
    let (dx, dz) = (i64::from(b[0] - a[0]), i64::from(b[1] - a[1]));
    let n = i64::from(bays);
    let points: Vec<[i32; 2]> = (0..=n)
        .map(|i| {
            [
                a[0] + ((dx * i + n / 2 * dx.signum()) / n) as i32,
                a[1] + ((dz * i + n / 2 * dz.signum()) / n) as i32,
            ]
        })
        .collect();
    let mut work = world.clone();
    let asset = add_asset(
        &mut work,
        MeshAsset::new("column", MeshKind::Column(c.column)),
    )
    .map_err(convert)?;
    let mut ids = Vec::new();
    for pt in &points {
        let mesh = GridMesh::new(0, asset, [pt[0], c.base, pt[1]]);
        ids.push(place_mesh(&mut work, c.room, mesh).map_err(convert)?);
    }
    let len = ((dx * dx + dz * dz) as f64).sqrt().round() as i32;
    *world = work;
    Ok(ArcadeOutcome {
        instances: ids,
        asset,
        pitch: len / bays,
        centres: points,
    })
}
