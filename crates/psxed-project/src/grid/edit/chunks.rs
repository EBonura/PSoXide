//! Re-chunking a world into hexagonal rooms.
//!
//! A big open level streams as a ring of rooms around the player. Hexagons
//! make that ring even: every neighbour of a chunk shares an edge with it and
//! sits at the same distance, so the set around the player grows and shrinks
//! by the same amount whichever way it walks.
//!
//! The hexagons live on the sector lattice. Each leaf goes, whole, to the
//! hexagon holding its anchor: its centre when it is coarser than a sector,
//! the centre of its sector otherwise, so every sector ends up in exactly one
//! room. A chunk is a room whose rectangle bounds its leaves; the cook
//! records which sectors of that rectangle it owns, since neighbouring
//! rectangles overlap.

use std::collections::BTreeMap;

use super::super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::super::mesh::{GridMesh, MAX_MESHES_PER_ROOM};
use super::super::room::GridRoom;
use super::super::sector::GridSector;
use super::super::world::GridWorld;

/// A pointy-top hexagonal lattice over the world plan, in sectors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HexLayout {
    /// Centre-to-corner distance, sectors.
    pub size: f64,
}

impl HexLayout {
    /// The hexagon `[q, r]` (axial coordinates) holding plan point `p`,
    /// world sectors.
    pub fn cell_of(&self, p: [f64; 2]) -> [i32; 2] {
        let q = (3f64.sqrt() / 3.0 * p[0] - p[1] / 3.0) / self.size;
        let r = (2.0 / 3.0 * p[1]) / self.size;
        cube_round(q, r)
    }

    /// Plan centre of hexagon `[q, r]`, world sectors.
    pub fn centre(&self, [q, r]: [i32; 2]) -> [f64; 2] {
        [
            self.size * 3f64.sqrt() * (f64::from(q) + f64::from(r) / 2.0),
            self.size * 1.5 * f64::from(r),
        ]
    }
}

/// Nearest hexagon to fractional axial `(q, r)`.
fn cube_round(q: f64, r: f64) -> [i32; 2] {
    let s = -q - r;
    let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
    let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
    if dq > dr && dq > ds {
        rq = -rr - rs;
    } else if dr > ds {
        rr = -rq - rs;
    }
    [rq as i32, rr as i32]
}

/// Why a world cannot be re-chunked as it is.
#[derive(Clone, Debug, PartialEq)]
pub enum HexRefusal {
    /// The hexagons must be at least one sector across.
    Size,
    /// The world has no leaves to chunk.
    Empty,
    /// A room holds content that is not carried across yet.
    Content { room: usize, what: &'static str },
    /// The rooms differ in a setting a leaf's meaning depends on.
    Mismatch(&'static str),
    /// Two rooms hold a leaf at the same place (stacked rooms).
    Stacked(LeafKey),
    /// A placed mesh cannot go to one chunk.
    Mesh {
        /// Instance id.
        id: u16,
        /// Why.
        why: &'static str,
    },
}

impl std::fmt::Display for HexRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Size => write!(f, "hexagons must be at least one sector across"),
            Self::Empty => write!(f, "the world has no cells to chunk"),
            Self::Content { room, what } => {
                write!(
                    f,
                    "room {room} has {what}, which re-chunking does not carry yet"
                )
            }
            Self::Mismatch(what) => write!(f, "the rooms differ in {what}"),
            Self::Mesh { id, why } => write!(f, "mesh {id} {why}"),
            Self::Stacked(key) => write!(
                f,
                "two rooms hold a cell at clicks [{}, {}] (stacked rooms cannot be re-chunked)",
                key.x, key.z
            ),
        }
    }
}

/// What a re-chunk made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HexReport {
    /// Rooms before.
    pub rooms_before: usize,
    /// Hexagonal rooms after.
    pub rooms: usize,
    /// Leaves moved.
    pub leaves: usize,
    /// Most sectors one chunk's rectangle spans.
    pub widest: usize,
    /// Leaves subdivided to balance leaves that came from different rooms.
    pub subdivided: usize,
    /// Hanging corners moved onto the surface for the same reason.
    pub corners: usize,
}

/// Replace the rooms of `world` with one room per hexagon of `layout` that
/// holds any leaf, named `Hex q,r`, in row order. Every chunk takes its
/// settings (fog, ambient, wall mode, defaults) from the room that held the
/// most leaves, and its tree is normalised. The world then streams as a ring
/// of chunks ([`super::super::world::StreamRadii::ring`]). Nothing changes on
/// refusal.
///
/// A placed mesh goes, whole, to the chunk holding the sector of its origin,
/// and every sector it covers must lie in that hexagon: a mesh across a chunk
/// border is refused by id (cut it in two first). Free planes, wall bands and
/// wall overrides are not carried yet.
pub fn rechunk_hex(world: &mut GridWorld, layout: HexLayout) -> Result<HexReport, HexRefusal> {
    // At least a sector across, flat side to flat side.
    if layout.size.is_nan() || layout.size * 3f64.sqrt() < 1.0 {
        return Err(HexRefusal::Size);
    }
    for (index, room) in world.rooms.iter().enumerate() {
        let what = if !room.planes.is_empty() {
            "free planes"
        } else if !room.band_sets.is_empty() {
            "wall bands"
        } else if !room.edge_overrides.is_empty() {
            "wall overrides"
        } else {
            continue;
        };
        return Err(HexRefusal::Content { room: index, what });
    }
    let Some(template) = world.rooms.iter().max_by_key(|r| r.leaf_count()) else {
        return Err(HexRefusal::Empty);
    };
    if template.leaf_count() == 0 {
        return Err(HexRefusal::Empty);
    }
    if world.rooms.iter().any(|r| r.y_offset != template.y_offset) {
        return Err(HexRefusal::Mismatch("height offset"));
    }
    if world
        .rooms
        .iter()
        .any(|r| r.wall_mode != template.wall_mode)
    {
        return Err(HexRefusal::Mismatch("wall mode"));
    }
    let mut hexes: BTreeMap<[i32; 2], Chunk> = BTreeMap::new();
    for room in &world.rooms {
        for (key, sector) in room.leaves() {
            // Row order: r first.
            let [q, r] = layout.cell_of(anchor(key));
            hexes
                .entry([r, q])
                .or_default()
                .leaves
                .push((key, sector.clone()));
        }
    }
    let mut meshes_placed = Vec::new();
    for room in &world.rooms {
        for mesh in &room.meshes {
            let hex = mesh_chunk(world, layout, mesh)?;
            meshes_placed.push((hex, mesh.clone()));
        }
    }
    for ([q, r], mesh) in meshes_placed {
        let chunk = hexes.get_mut(&[r, q]).ok_or(HexRefusal::Mesh {
            id: mesh.id,
            why: "is over no cell",
        })?;
        if chunk.meshes.len() >= MAX_MESHES_PER_ROOM {
            return Err(HexRefusal::Mesh {
                id: mesh.id,
                why: "would be the one too many in its chunk",
            });
        }
        chunk.meshes.push(mesh);
    }
    let mut rooms = Vec::with_capacity(hexes.len());
    let mut report = HexReport {
        rooms_before: world.rooms.len(),
        rooms: hexes.len(),
        ..HexReport::default()
    };
    for ([r, q], chunk) in hexes {
        let Chunk { leaves, meshes } = chunk;
        let (mut lo, mut hi) = (leaves[0].0.sector_range().0, leaves[0].0.sector_range().1);
        for (key, _) in &leaves {
            let (a, b) = key.sector_range();
            lo = [lo[0].min(a[0]), lo[1].min(a[1])];
            hi = [hi[0].max(b[0]), hi[1].max(b[1])];
        }
        // The rectangle holds every sector a mesh covers as well.
        for mesh in &meshes {
            let (a, b) = mesh_span(world, mesh);
            lo = [lo[0].min(a[0]), lo[1].min(a[1])];
            hi = [hi[0].max(b[0]), hi[1].max(b[1])];
        }
        let mut room = GridRoom {
            name: format!("Hex {q},{r}"),
            origin: lo,
            width: (hi[0] - lo[0] + 1) as usize,
            depth: (hi[1] - lo[1] + 1) as usize,
            leaves: Default::default(),
            meshes,
            ..template.clone()
        };
        report.widest = report.widest.max(room.width.max(room.depth));
        report.leaves += leaves.len();
        for (key, sector) in leaves {
            match room.insert_leaf(key, sector) {
                Ok(None) => {}
                _ => return Err(HexRefusal::Stacked(key)),
            }
        }
        // Leaves that met only across a room border now share a room: keep
        // the tree balanced and its hanging corners on the surface.
        let fixed = super::super::tree::normalize::normalize(&mut room);
        report.subdivided += fixed.subdivided;
        report.corners += fixed.corners;
        rooms.push(room);
    }
    world.rooms = rooms;
    world.stream.ring = true;
    Ok(report)
}

/// What one hexagon receives.
#[derive(Default)]
struct Chunk {
    leaves: Vec<(LeafKey, GridSector)>,
    meshes: Vec<GridMesh>,
}

/// World sectors `(min, max)` a mesh covers (its origin's sector when the
/// asset does not build).
fn mesh_span(world: &GridWorld, mesh: &GridMesh) -> ([i32; 2], [i32; 2]) {
    match world
        .mesh_assets
        .get(usize::from(mesh.asset))
        .and_then(|a| a.build().ok())
    {
        Some(built) => mesh.cell_span(&built),
        None => (mesh.center_cell(), mesh.center_cell()),
    }
}

/// The hexagon (as `[q, r]`) that holds the mesh: the one of its origin's
/// sector, which every sector the mesh covers must share.
fn mesh_chunk(
    world: &GridWorld,
    layout: HexLayout,
    mesh: &GridMesh,
) -> Result<[i32; 2], HexRefusal> {
    let of = |cell: [i32; 2]| layout.cell_of([f64::from(cell[0]) + 0.5, f64::from(cell[1]) + 0.5]);
    let home = of(mesh.center_cell());
    let (lo, hi) = mesh_span(world, mesh);
    for z in lo[1]..=hi[1] {
        for x in lo[0]..=hi[0] {
            if of([x, z]) != home {
                return Err(HexRefusal::Mesh {
                    id: mesh.id,
                    why: "covers sectors of more than one chunk",
                });
            }
        }
    }
    Ok(home)
}

/// Plan point that decides a leaf's hexagon, world sectors: the leaf's
/// centre when it spans several sectors, its sector's centre otherwise.
fn anchor(key: LeafKey) -> [f64; 2] {
    let n = key.edge_clicks().max(CLICKS_PER_SECTOR);
    let x = key.x.div_euclid(n) * n;
    let z = key.z.div_euclid(n) * n;
    let s = f64::from(CLICKS_PER_SECTOR);
    [
        (f64::from(x) + f64::from(n) / 2.0) / s,
        (f64::from(z) + f64::from(n) / 2.0) / s,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::surface::GridSurface;

    /// One room of `w` by `d` sectors floored everywhere at height 0.
    fn field(w: usize, d: usize) -> GridWorld {
        let mut room = GridRoom::new_derived("field", w, d);
        for z in 0..d {
            for x in 0..w {
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(0, None)));
            }
        }
        GridWorld {
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    #[test]
    fn the_lattice_round_trips_its_centres() {
        let layout = HexLayout { size: 4.0 };
        for q in -3..4 {
            for r in -3..4 {
                assert_eq!(layout.cell_of(layout.centre([q, r])), [q, r]);
            }
        }
    }

    #[test]
    fn every_sector_lands_in_exactly_one_chunk() {
        let mut world = field(24, 20);
        let report = rechunk_hex(&mut world, HexLayout { size: 4.0 }).unwrap();
        assert_eq!(report.leaves, 24 * 20);
        assert!(report.rooms > 4);
        let mut owners = BTreeMap::new();
        for (index, room) in world.rooms.iter().enumerate() {
            for (x, z, _) in room.iter() {
                assert!(owners.insert(room.world_cell(x, z), index).is_none());
            }
        }
        assert_eq!(owners.len(), 24 * 20);
        // Neighbouring chunks overlap as rectangles, so the cook carries
        // occupancy.
        assert!(!crate::grid::cook::occupancy::derive(&world).is_empty());
    }

    #[test]
    fn a_sector_split_into_quarters_stays_in_one_chunk() {
        let mut world = field(12, 12);
        let room = &mut world.rooms[0];
        let keys: Vec<LeafKey> = room.leaves().map(|(k, _)| k).collect();
        crate::grid::tree::ops::subdivide_all_to(room, &keys, 4);
        rechunk_hex(&mut world, HexLayout { size: 2.0 }).unwrap();
        let mut owners: BTreeMap<[i32; 2], usize> = BTreeMap::new();
        for (index, room) in world.rooms.iter().enumerate() {
            for (key, _) in room.leaves() {
                let owner = *owners.entry(key.cell()).or_insert(index);
                assert_eq!(owner, index, "sector {:?} split between chunks", key.cell());
            }
        }
    }

    /// A column asset placed standing in sector `cell` of the world's room.
    fn column_in(world: &mut GridWorld, cell: [i32; 2], id: u16) {
        use crate::grid::mesh::{ColumnSpec, MeshAsset, MeshKind};
        if world.mesh_assets.is_empty() {
            world.mesh_assets.push(MeshAsset::new(
                "column",
                MeshKind::Column(ColumnSpec::default()),
            ));
        }
        world.rooms[0].meshes.push(GridMesh::new(
            id,
            0,
            [cell[0] * 1024 + 512, 0, cell[1] * 1024 + 512],
        ));
    }

    #[test]
    fn a_mesh_follows_its_anchor_into_its_chunk() {
        let mut world = field(12, 12);
        column_in(&mut world, [5, 5], 3);
        column_in(&mut world, [0, 0], 4);
        rechunk_hex(&mut world, HexLayout { size: 2.0 }).unwrap();
        let holders: Vec<(usize, u16)> = world
            .rooms
            .iter()
            .enumerate()
            .flat_map(|(i, r)| r.meshes.iter().map(move |m| (i, m.id)))
            .collect();
        assert_eq!(holders.len(), 2);
        for (room, id) in holders {
            let cell = if id == 3 { [5, 5] } else { [0, 0] };
            assert!(
                !world.rooms[room].leaves.in_sector(cell).is_empty(),
                "mesh {id} is in a room that does not hold its sector"
            );
        }
        // The world is still valid: no instance reaches another chunk.
        let issues = crate::grid::limits::validate(&world);
        assert!(
            issues.iter().all(|i| !matches!(
                i.kind,
                crate::grid::limits::GridIssueKind::MeshOutsideRoom { .. }
                    | crate::grid::limits::GridIssueKind::MeshOverOtherChunk { .. }
            )),
            "{issues:?}"
        );
    }

    #[test]
    fn a_mesh_across_a_chunk_border_is_refused_by_id_and_nothing_changes() {
        use crate::grid::mesh::{ImportProxy, ImportedMesh, MeshAsset, MeshKind};
        let cube = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1\nv 1 0 1\nv 1 1 1\nv 0 1 1\n\
                    f 5 6 7 8\nf 2 1 4 3\nf 1 5 8 4\nf 6 2 3 7\nf 8 7 3 4\nf 1 2 6 5\n";
        let mut world = field(12, 12);
        let mesh = ImportedMesh::from_obj(cube, 1024.0, "cube.obj", ImportProxy::Slab).unwrap();
        world
            .mesh_assets
            .push(MeshAsset::new("Block", MeshKind::Imported(mesh)));
        let mut wide = GridMesh::new(7, 0, [6 * 1024, 0, 6 * 1024]);
        wide.scale = [2048, 256, 256];
        world.rooms[0].meshes.push(wide);
        let before = world.clone();
        assert_eq!(
            rechunk_hex(&mut world, HexLayout { size: 2.0 }),
            Err(HexRefusal::Mesh {
                id: 7,
                why: "covers sectors of more than one chunk"
            })
        );
        assert_eq!(world, before);
    }

    #[test]
    fn a_mesh_over_another_chunks_sector_is_an_error_in_a_ring_world() {
        let mut world = field(12, 12);
        column_in(&mut world, [5, 5], 3);
        rechunk_hex(&mut world, HexLayout { size: 2.0 }).unwrap();
        // Take the sector under the instance out of its chunk (as if another
        // chunk held it): the rectangle still covers it, the chunk does not.
        let home = world
            .rooms
            .iter()
            .position(|r| !r.meshes.is_empty())
            .unwrap();
        world.rooms[home].remove_leaf(LeafKey::sector([5, 5]));
        let issues = crate::grid::limits::validate(&world);
        assert!(issues.iter().any(|i| matches!(
            i.kind,
            crate::grid::limits::GridIssueKind::MeshOverOtherChunk { id: 3 }
        )));
    }

    #[test]
    fn a_room_with_planes_is_refused_and_left_alone() {
        let mut world = field(4, 4);
        world.rooms[0]
            .planes
            .push(crate::grid::plane::GridPlane::new(
                [512, 0, 512],
                [512, 512],
            ));
        let before = world.clone();
        assert_eq!(
            rechunk_hex(&mut world, HexLayout { size: 2.0 }),
            Err(HexRefusal::Content {
                room: 0,
                what: "free planes"
            })
        );
        assert_eq!(world, before);
    }
}
