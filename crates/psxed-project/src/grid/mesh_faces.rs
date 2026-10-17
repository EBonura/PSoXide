//! The drawable faces of placed meshes.
//!
//! An instance becomes ordinary [`GridFace`]s, so the editor preview, the light
//! bake and the cook read meshes exactly as they read free planes: the same
//! corner rounding (each corner is [`GridMesh::place`], rounded to grid units
//! once, so corners of one instance weld), the same texel rebasing and the same
//! per-corner bake. The faces of one instance are one run in the room's plane
//! list at cook time.

use super::faces::{editor_point, Builder, Corner, GridFace, GridFaceKind, UvDefaults};
use super::mesh::{BuiltMesh, GridMesh};
use super::uv::Frame;
use super::world::GridWorld;

/// An instance with its asset built, ready to be placed.
pub struct Placed<'a> {
    /// The instance.
    pub mesh: &'a GridMesh,
    /// What its asset builds.
    pub built: BuiltMesh,
}

/// The instance `index` of `room`, with its asset built. `None` for a
/// missing, hidden or unbuildable instance (validation reports the last).
pub fn placed(world: &GridWorld, room: usize, index: usize) -> Option<Placed<'_>> {
    let mesh = world
        .rooms
        .get(room)?
        .meshes
        .get(index)
        .filter(|m| m.enabled)?;
    let built = world
        .mesh_assets
        .get(usize::from(mesh.asset))?
        .build()
        .ok()?;
    Some(Placed { mesh, built })
}

/// Append the faces of instance `index` of `room`; returns how many.
pub fn mesh_faces_with(
    world: &GridWorld,
    defaults: &dyn UvDefaults,
    room: usize,
    index: usize,
    out: &mut Vec<GridFace>,
) -> usize {
    let Some(Placed { mesh, built }) = placed(world, room, index) else {
        return 0;
    };
    let Some(r) = world.rooms.get(room) else {
        return 0;
    };
    let first = out.len();
    let mut builder = Builder {
        scale: world.scale,
        room,
        cell: mesh.center_cell(),
        y_offset: r.y_offset,
        defaults,
        legacy_uv: false,
        room_ref: None,
        frame: Frame {
            min: [0.0, 0.0],
            max: [64.0, 64.0],
        },
        jitter: None,
        tint: None,
        out,
    };
    let (scale, y_offset) = (builder.scale, builder.y_offset);
    for face in &built.faces {
        let placed: Vec<[i32; 3]> = face.ring().iter().map(|p| mesh.place(*p)).collect();
        let ring: Vec<Corner> = placed
            .iter()
            .zip(&face.texels)
            .map(|(g, t)| Corner {
                p: editor_point(scale, y_offset, g[0], g[1], g[2]),
                g: [g[0], y_offset + g[1], g[2]],
                uv: [f64::from(t[0]), f64::from(t[1])],
            })
            .collect();
        // The asset ring is counter-clockwise seen from the front, so its
        // right-hand normal is the visible side.
        let toward = ring_normal(&placed);
        builder.push(
            GridFaceKind::Mesh { index },
            ring,
            toward,
            mesh.material(face.slot),
            face.double_sided,
        );
    }
    let faces = builder.out.len() - first;
    for face in &mut out[first..] {
        face.unlit = mesh.fullbright;
    }
    faces
}

/// Unit right-hand normal of a ring of grid points (Newell), zero when it
/// collapses.
fn ring_normal(ring: &[[i32; 3]]) -> [f64; 3] {
    let mut n = [0.0f64; 3];
    for i in 0..ring.len() {
        let a = ring[i].map(f64::from);
        let b = ring[(i + 1) % ring.len()].map(f64::from);
        n[0] += a[1] * b[2] - a[2] * b[1];
        n[1] += a[2] * b[0] - a[0] * b[2];
        n[2] += a[0] * b[1] - a[1] * b[0];
    }
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 {
        return [0.0, 1.0, 0.0];
    }
    n.map(|v| v / len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::mesh::{ArchProfile, ArchSpec, MeshAsset, MeshKind};
    use crate::grid::room::GridRoom;
    use crate::grid::units::GridScale;

    fn world_with(spec: ArchSpec, mesh: GridMesh) -> GridWorld {
        let mut room = GridRoom::new("r", 6, 6);
        room.meshes.push(mesh);
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            mesh_assets: vec![MeshAsset::new("arch", MeshKind::Arch(spec))],
            ..GridWorld::default()
        }
    }

    fn faces_of(world: &GridWorld) -> Vec<GridFace> {
        let mut out = Vec::new();
        mesh_faces_with(world, &(), 0, 0, &mut out);
        out
    }

    #[test]
    fn an_instance_becomes_the_faces_of_its_asset_at_its_position() {
        let spec = ArchSpec::default();
        let mesh = GridMesh::new(1, 0, [3072, 0, 3072]);
        let world = world_with(spec, mesh);
        let built = world.mesh_assets[0].build().unwrap();
        let faces = faces_of(&world);
        assert_eq!(faces.len(), built.faces.len());
        assert!(faces
            .iter()
            .all(|f| matches!(f.kind, GridFaceKind::Mesh { index: 0 })));
        // The base of the piers stands on the floor at the instance position.
        let low = faces
            .iter()
            .flat_map(|f| f.grid.iter().map(|g| g[1]))
            .min()
            .unwrap();
        assert_eq!(low, 0);
        let xs: Vec<i32> = faces
            .iter()
            .flat_map(|f| f.grid.iter().map(|g| g[0]))
            .collect();
        let mid = (xs.iter().min().unwrap() + xs.iter().max().unwrap()) / 2;
        assert_eq!(mid, 3072);
    }

    #[test]
    fn the_preview_normal_is_the_visible_side_whatever_the_turn() {
        for yaw in [0, 30, 90, 180, -90] {
            let mut mesh = GridMesh::new(1, 0, [3072, 0, 3072]);
            mesh.rotation = [yaw, 0, 0];
            let world = world_with(ArchSpec::default(), mesh);
            let faces = faces_of(&world);
            // The extrados faces up and out whatever the yaw; the soffit down.
            let up = faces.iter().filter(|f| f.normal[1] > 0.5).count();
            let down = faces.iter().filter(|f| f.normal[1] < -0.5).count();
            assert!(up >= 3 && down >= 3, "yaw {yaw}: up {up} down {down}");
        }
    }

    #[test]
    fn a_hidden_or_unbuildable_instance_draws_nothing() {
        let mut mesh = GridMesh::new(1, 0, [0; 3]);
        mesh.enabled = false;
        assert!(faces_of(&world_with(ArchSpec::default(), mesh)).is_empty());
        let bad = ArchSpec {
            profile: ArchProfile::Pointed,
            rise: 100,
            ..ArchSpec::default()
        };
        assert!(faces_of(&world_with(bad, GridMesh::new(1, 0, [0; 3]))).is_empty());
        let mut missing = GridMesh::new(1, 7, [0; 3]);
        missing.enabled = true;
        assert!(faces_of(&world_with(ArchSpec::default(), missing)).is_empty());
    }

    #[test]
    fn fullbright_and_materials_follow_the_instance() {
        let mut mesh = GridMesh::new(1, 0, [0; 3]);
        mesh.fullbright = true;
        mesh.set_material(1, Some(crate::ResourceId(5)));
        let world = world_with(ArchSpec::default(), mesh);
        let faces = faces_of(&world);
        assert!(faces.iter().all(|f| f.unlit));
        assert!(faces
            .iter()
            .any(|f| f.material == Some(crate::ResourceId(5))));
        assert!(faces.iter().any(|f| f.material.is_none()));
    }
}
