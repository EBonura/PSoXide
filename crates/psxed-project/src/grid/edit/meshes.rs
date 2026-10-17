//! Editing static meshes: assets, instances, and the arch between two points.
//!
//! Instances are addressed by their stable id (`GridMesh::id`), which survives
//! other instances being added and removed. Every edit clamps the instance
//! into the document's ranges and grows its room so the footprint stays
//! inside it, exactly as planes do: the render box and the collision table are
//! per-room, so a mesh outside its room would silently not collide.

use crate::ResourceId;

use super::super::limits::MAX_ROOM_SIDE;
use super::super::mesh::{
    ArchProfile, ArchSpec, GridMesh, MeshAsset, MeshKind, MAX_MESHES_PER_ROOM, MAX_MESH_ASSETS,
};
use super::super::world::GridWorld;

/// Why a mesh edit was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeshRefusal {
    /// The room does not exist.
    NoRoom,
    /// The asset does not exist.
    NoAsset,
    /// The instance does not exist.
    NoMesh,
    /// The room already holds [`MAX_MESHES_PER_ROOM`] instances.
    TooMany,
    /// The world already holds [`MAX_MESH_ASSETS`] assets.
    TooManyAssets,
    /// Growing the room to hold the instance would pass [`MAX_ROOM_SIDE`].
    RoomTooLarge,
    /// The asset's recipe does not build.
    Unbuildable(String),
    /// The asset is placed this many times and cannot be removed.
    InUse(usize),
    /// Anything else, in words.
    Other(String),
}

impl MeshRefusal {
    /// One-line message for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::NoRoom => "No room to put the mesh in".to_string(),
            Self::NoAsset => "No such mesh asset".to_string(),
            Self::NoMesh => "No such mesh".to_string(),
            Self::TooMany => format!("A room holds at most {MAX_MESHES_PER_ROOM} meshes"),
            Self::TooManyAssets => format!("A world holds at most {MAX_MESH_ASSETS} mesh assets"),
            Self::RoomTooLarge => format!("The room would pass {MAX_ROOM_SIDE} sectors"),
            Self::Unbuildable(why) => why.clone(),
            Self::InUse(n) => format!("The asset is placed {n} time(s); delete those first"),
            Self::Other(why) => why.clone(),
        }
    }
}

/// Add `asset` to the world, or return the index of an equal one (same
/// recipe, whatever its name), so placing the same arch twice shares one
/// asset.
pub fn add_asset(world: &mut GridWorld, asset: MeshAsset) -> Result<u16, MeshRefusal> {
    asset.build().map_err(MeshRefusal::Unbuildable)?;
    if let Some(i) = world.mesh_assets.iter().position(|a| a.kind == asset.kind) {
        return Ok(i as u16);
    }
    if world.mesh_assets.len() >= MAX_MESH_ASSETS {
        return Err(MeshRefusal::TooManyAssets);
    }
    world.mesh_assets.push(asset);
    Ok((world.mesh_assets.len() - 1) as u16)
}

/// Replace the recipe of asset `index`; every instance follows (and their rooms
/// grow to hold the new shape). Refused, with nothing changed, when the recipe
/// does not build or a room would pass the size cap.
pub fn set_asset(world: &mut GridWorld, index: usize, kind: MeshKind) -> Result<(), MeshRefusal> {
    let old = world
        .mesh_assets
        .get(index)
        .cloned()
        .ok_or(MeshRefusal::NoAsset)?;
    let new = MeshAsset {
        name: old.name.clone(),
        kind,
    };
    new.build().map_err(MeshRefusal::Unbuildable)?;
    let before = world.rooms.clone();
    world.mesh_assets[index] = new;
    for room in 0..world.rooms.len() {
        for i in 0..world.rooms[room].meshes.len() {
            if usize::from(world.rooms[room].meshes[i].asset) == index && !fit_room(world, room, i)
            {
                world.mesh_assets[index] = old;
                world.rooms = before;
                return Err(MeshRefusal::RoomTooLarge);
            }
        }
    }
    Ok(())
}

/// Remove asset `index` if nothing places it; the assets after it move down.
pub fn remove_asset(world: &mut GridWorld, index: usize) -> Result<(), MeshRefusal> {
    if index >= world.mesh_assets.len() {
        return Err(MeshRefusal::NoAsset);
    }
    let used = world
        .rooms
        .iter()
        .flat_map(|r| &r.meshes)
        .filter(|m| usize::from(m.asset) == index)
        .count();
    if used > 0 {
        return Err(MeshRefusal::InUse(used));
    }
    world.mesh_assets.remove(index);
    for mesh in world.rooms.iter_mut().flat_map(|r| &mut r.meshes) {
        if usize::from(mesh.asset) > index {
            mesh.asset -= 1;
        }
    }
    Ok(())
}

/// Grow room `room` so the footprint of its instance `index` is inside it.
/// Returns false, leaving the room alone, when that would pass the size cap.
pub fn fit_room(world: &mut GridWorld, room: usize, index: usize) -> bool {
    let Some(r) = world.rooms.get(room) else {
        return false;
    };
    let Some(mesh) = r.meshes.get(index) else {
        return false;
    };
    let span = match world
        .mesh_assets
        .get(usize::from(mesh.asset))
        .and_then(|a| a.build().ok())
    {
        Some(built) => mesh.cell_span(&built),
        None => (mesh.center_cell(), mesh.center_cell()),
    };
    let (lo, hi) = span;
    let low = [r.origin[0].min(lo[0]), r.origin[1].min(lo[1])];
    let high = [
        (r.origin[0] + r.width as i32 - 1).max(hi[0]),
        (r.origin[1] + r.depth as i32 - 1).max(hi[1]),
    ];
    let side = MAX_ROOM_SIDE as i32;
    if high[0] - low[0] + 1 > side || high[1] - low[1] + 1 > side {
        return false;
    }
    let r = &mut world.rooms[room];
    r.grow_to_include(
        i64::from(lo[0]) - i64::from(r.origin[0]),
        i64::from(lo[1]) - i64::from(r.origin[1]),
    );
    r.grow_to_include(
        i64::from(hi[0]) - i64::from(r.origin[0]),
        i64::from(hi[1]) - i64::from(r.origin[1]),
    );
    true
}

/// Place a new instance of `asset` in `room`, growing the room to hold it.
/// Returns its stable id.
pub fn place_mesh(
    world: &mut GridWorld,
    room: usize,
    mut mesh: GridMesh,
) -> Result<u16, MeshRefusal> {
    let r = world.rooms.get(room).ok_or(MeshRefusal::NoRoom)?;
    if r.meshes.len() >= MAX_MESHES_PER_ROOM {
        return Err(MeshRefusal::TooMany);
    }
    world
        .mesh_assets
        .get(usize::from(mesh.asset))
        .ok_or(MeshRefusal::NoAsset)?
        .build()
        .map_err(MeshRefusal::Unbuildable)?;
    mesh.id = world.next_mesh_id();
    mesh.normalize();
    let id = mesh.id;
    let index = r.meshes.len();
    world.rooms[room].meshes.push(mesh);
    if !fit_room(world, room, index) {
        world.rooms[room].meshes.pop();
        return Err(MeshRefusal::RoomTooLarge);
    }
    Ok(id)
}

/// Change instance `id` with `edit`, normalise it and fit its room. When the
/// edit would push the room past the size cap, it is undone and an error
/// returned. Returns whether the instance changed.
pub fn update_mesh(
    world: &mut GridWorld,
    id: u16,
    edit: impl FnOnce(&mut GridMesh),
) -> Result<bool, MeshRefusal> {
    let (room, index) = world.find_mesh(id).ok_or(MeshRefusal::NoMesh)?;
    let mesh = &mut world.rooms[room].meshes[index];
    let before = mesh.clone();
    edit(mesh);
    mesh.id = before.id;
    mesh.normalize();
    if *mesh == before {
        return Ok(false);
    }
    let asset = usize::from(mesh.asset);
    let built = world
        .mesh_assets
        .get(asset)
        .ok_or(MeshRefusal::NoAsset)
        .and_then(|a| a.build().map_err(MeshRefusal::Unbuildable));
    if let Err(e) = built {
        world.rooms[room].meshes[index] = before;
        return Err(e);
    }
    if !fit_room(world, room, index) {
        world.rooms[room].meshes[index] = before;
        return Err(MeshRefusal::RoomTooLarge);
    }
    Ok(true)
}

/// Set the material of one slot of instance `id`.
pub fn set_mesh_material(
    world: &mut GridWorld,
    id: u16,
    slot: usize,
    material: Option<ResourceId>,
) -> Result<bool, MeshRefusal> {
    update_mesh(world, id, |m| m.set_material(slot, material))
}

/// Delete instances by id. Returns how many existed.
pub fn delete_meshes(world: &mut GridWorld, ids: &[u16]) -> usize {
    let mut removed = 0;
    for room in &mut world.rooms {
        let before = room.meshes.len();
        room.meshes.retain(|m| !ids.contains(&m.id));
        removed += before - room.meshes.len();
    }
    removed
}

/// What [`arch_span`] solved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchPlacement {
    /// The recipe.
    pub spec: ArchSpec,
    /// Where the instance goes (grid units).
    pub position: [i32; 3],
    /// Its heading, degrees.
    pub yaw: i32,
}

/// Solve an arch whose clear opening runs from `from` to `to` (points on the
/// floor under its springing, grid units, `y` relative to the room): the span
/// is their distance, the instance stands at their midpoint at the lower `y`,
/// and its yaw turns the span axis along them. `spec` supplies profile, rise,
/// spring, thickness, depth and segments; its span is replaced.
pub fn solve_arch_span(
    from: [i32; 3],
    to: [i32; 3],
    mut spec: ArchSpec,
) -> Result<ArchPlacement, MeshRefusal> {
    let (dx, dz) = (f64::from(to[0] - from[0]), f64::from(to[2] - from[2]));
    let length = dx.hypot(dz);
    if length < 1.0 {
        return Err(MeshRefusal::Other(
            "from and to must differ horizontally".to_string(),
        ));
    }
    spec.span = length.round() as i32;
    spec.check().map_err(MeshRefusal::Other)?;
    // The span axis (+x of the asset) runs from `from` to `to`: yaw turns +x
    // toward (cos yaw, -sin yaw) in (x, z).
    let yaw = (-dz).atan2(dx).to_degrees().round() as i32;
    Ok(ArchPlacement {
        spec,
        position: [
            (from[0] + to[0]) / 2,
            from[1].min(to[1]),
            (from[2] + to[2]) / 2,
        ],
        yaw,
    })
}

/// Place an arch between two points: add (or reuse) the asset and place the
/// instance. Returns the asset index and the instance id.
pub fn arch_span(
    world: &mut GridWorld,
    room: usize,
    from: [i32; 3],
    to: [i32; 3],
    spec: ArchSpec,
    name: Option<String>,
) -> Result<(u16, u16, ArchPlacement), MeshRefusal> {
    let placement = solve_arch_span(from, to, spec)?;
    let label = name.unwrap_or_else(|| {
        format!(
            "{} arch {}",
            placement.spec.profile.label(),
            placement.spec.span
        )
    });
    let asset = add_asset(world, MeshAsset::new(label, MeshKind::Arch(placement.spec)))?;
    let mut mesh = GridMesh::new(0, asset, placement.position);
    mesh.rotation = [placement.yaw, 0, 0];
    let id = place_mesh(world, room, mesh)?;
    Ok((asset, id, placement))
}

/// The profile named `name`, for tools that take words.
pub fn parse_profile(name: &str) -> Result<ArchProfile, MeshRefusal> {
    ArchProfile::parse(name).ok_or_else(|| {
        MeshRefusal::Other(format!(
            "unknown arch profile {name:?}; use round, segmental, pointed or flat"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::mesh::{ColumnSpec, RubbleSpec};
    use crate::grid::room::GridRoom;
    use crate::grid::units::{GridScale, SECTOR_UNITS};

    fn world() -> GridWorld {
        GridWorld {
            scale: GridScale { num: 5, den: 4 },
            rooms: vec![GridRoom::new("r", 4, 4)],
            ..GridWorld::default()
        }
    }

    #[test]
    fn equal_recipes_share_one_asset() {
        let mut w = world();
        let a = add_asset(
            &mut w,
            MeshAsset::new("a", MeshKind::Column(ColumnSpec::default())),
        )
        .unwrap();
        let b = add_asset(
            &mut w,
            MeshAsset::new("b", MeshKind::Column(ColumnSpec::default())),
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(w.mesh_assets.len(), 1);
        let bad = MeshAsset::new(
            "bad",
            MeshKind::Column(ColumnSpec {
                sides: 1,
                ..ColumnSpec::default()
            }),
        );
        assert!(matches!(
            add_asset(&mut w, bad),
            Err(MeshRefusal::Unbuildable(_))
        ));
    }

    #[test]
    fn placing_grows_the_room_and_gives_each_instance_its_own_id() {
        let mut w = world();
        let rubble = add_asset(
            &mut w,
            MeshAsset::new("r", MeshKind::Rubble(RubbleSpec::default())),
        )
        .unwrap();
        let first = place_mesh(
            &mut w,
            0,
            GridMesh::new(0, rubble, [SECTOR_UNITS * 2, 0, SECTOR_UNITS * 2]),
        )
        .unwrap();
        // Far outside the 4 x 4 room: it grows to hold the footprint.
        let second = place_mesh(
            &mut w,
            0,
            GridMesh::new(0, rubble, [SECTOR_UNITS * 9 + 500, 0, 500]),
        )
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(w.find_mesh(second), Some((0, 1)));
        let room = &w.rooms[0];
        assert!(room.width >= 10, "{}", room.width);
        // Deleting the first never renumbers the second.
        assert_eq!(delete_meshes(&mut w, &[first]), 1);
        assert_eq!(w.find_mesh(second), Some((0, 0)));
        assert_eq!(w.next_mesh_id(), second + 1);
    }

    #[test]
    fn an_edit_that_would_overgrow_the_room_is_undone() {
        let mut w = world();
        let rubble = add_asset(
            &mut w,
            MeshAsset::new("r", MeshKind::Rubble(RubbleSpec::default())),
        )
        .unwrap();
        let id = place_mesh(&mut w, 0, GridMesh::new(0, rubble, [1024, 0, 1024])).unwrap();
        let before = w.clone();
        let far = SECTOR_UNITS * (MAX_ROOM_SIDE as i32 + 40);
        let moved = update_mesh(&mut w, id, |m| m.position[0] = far);
        assert_eq!(moved, Err(MeshRefusal::RoomTooLarge));
        assert_eq!(w, before);
        assert_eq!(update_mesh(&mut w, 999, |_| {}), Err(MeshRefusal::NoMesh));
        // A no-op edit reports no change.
        assert_eq!(update_mesh(&mut w, id, |_| {}), Ok(false));
    }

    #[test]
    fn an_asset_in_use_cannot_be_removed_and_removal_renumbers() {
        let mut w = world();
        let a = add_asset(
            &mut w,
            MeshAsset::new("a", MeshKind::Column(ColumnSpec::default())),
        )
        .unwrap();
        let b = add_asset(
            &mut w,
            MeshAsset::new("b", MeshKind::Rubble(RubbleSpec::default())),
        )
        .unwrap();
        let id = place_mesh(&mut w, 0, GridMesh::new(0, b, [1024, 0, 1024])).unwrap();
        assert_eq!(
            remove_asset(&mut w, usize::from(b)),
            Err(MeshRefusal::InUse(1))
        );
        remove_asset(&mut w, usize::from(a)).unwrap();
        let (room, i) = w.find_mesh(id).unwrap();
        assert_eq!(w.rooms[room].meshes[i].asset, 0);
        assert_eq!(w.mesh_assets.len(), 1);
    }

    #[test]
    fn an_arch_span_stands_between_its_points_with_its_axis_along_them() {
        let from = [1024, 0, 2048];
        let to = [3072, 0, 2048];
        let placed = solve_arch_span(from, to, ArchSpec::default()).unwrap();
        assert_eq!(placed.spec.span, 2048);
        assert_eq!(placed.position, [2048, 0, 2048]);
        assert_eq!(placed.yaw, 0);
        // The same span along +z: +x of the asset turns to +z, which is yaw -90.
        let along_z =
            solve_arch_span([2048, 0, 1024], [2048, 0, 3072], ArchSpec::default()).unwrap();
        assert_eq!(along_z.yaw, -90);
        // And the endpoints land where asked after placing.
        let mut mesh = GridMesh::new(1, 0, along_z.position);
        mesh.rotation = [along_z.yaw, 0, 0];
        let left = mesh.place([-along_z.spec.span / 2, 0, 0]);
        let right = mesh.place([along_z.spec.span / 2, 0, 0]);
        assert_eq!((left, right), ([2048, 0, 1024], [2048, 0, 3072]));
        assert!(solve_arch_span(from, from, ArchSpec::default()).is_err());
        assert!(solve_arch_span([0, 0, 0], [100, 0, 0], ArchSpec::default()).is_err());
    }

    #[test]
    fn arch_span_places_an_asset_and_an_instance_in_one_step() {
        let mut w = world();
        let (asset, id, placed) = arch_span(
            &mut w,
            0,
            [1024, 0, 2048],
            [3072, 0, 2048],
            ArchSpec::default(),
            None,
        )
        .unwrap();
        assert_eq!(asset, 0);
        assert_eq!(placed.spec.span, 2048);
        assert_eq!(w.find_mesh(id), Some((0, 0)));
        assert!(w.mesh_assets[0].name.contains("arch"));
        // The same arch again reuses the asset.
        let (again, ..) = arch_span(
            &mut w,
            0,
            [1024, 0, 1024],
            [3072, 0, 1024],
            ArchSpec::default(),
            None,
        )
        .unwrap();
        assert_eq!(again, 0);
        assert_eq!(w.mesh_assets.len(), 1);
    }
}
