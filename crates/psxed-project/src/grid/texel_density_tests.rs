//! The world's texel density: how many texels one sector edge spans in the
//! default mapping. A new world spends 64 (one 64 by 64 texture per sector);
//! a stored world without the setting keeps the 32 it was made with, so no
//! existing face moves.

use crate::face_uv::{FaceUv, UvMode};
use crate::grid::faces::{all_faces, all_faces_with, GridFace, GridFaceKind, UvDefaults};
use crate::grid::plane::GridPlane;
use crate::grid::room::GridRoom;
use crate::grid::starter::starter_world;
use crate::grid::surface::GridSurface;
use crate::grid::units::{GridScale, SectorTexels};
use crate::grid::world::GridWorld;
use crate::grid::{GridWall, WallEdge};
use crate::ResourceId;

fn rid(n: u64) -> ResourceId {
    ron::from_str(&format!("({n})")).expect("a resource id")
}

/// A 4 by 4 room with one flat floor at sector (3, 2) carrying `uv`.
fn floor_world(texels: SectorTexels, uv: FaceUv) -> GridWorld {
    let mut room = GridRoom::new("r", 4, 4);
    let mut floor = GridSurface::flat(0, Some(rid(7)));
    floor.tri_a.uv = uv;
    room.sector_or_insert(3, 2)
        .unwrap()
        .set_ground_floor(Some(floor));
    GridWorld {
        scale: GridScale::DEFAULT,
        texels_per_sector: texels,
        rooms: vec![room],
        ..GridWorld::default()
    }
}

fn extent(face: &GridFace) -> [f64; 2] {
    let span = |axis: usize| {
        let lo = face.texels.iter().map(|t| t[axis]).fold(f64::MAX, f64::min);
        let hi = face.texels.iter().map(|t| t[axis]).fold(f64::MIN, f64::max);
        hi - lo
    };
    [span(0), span(1)]
}

fn centre(face: &GridFace) -> [f64; 2] {
    let n = face.texels.len() as f64;
    [
        face.texels.iter().map(|t| t[0]).sum::<f64>() / n,
        face.texels.iter().map(|t| t[1]).sum::<f64>() / n,
    ]
}

#[test]
fn new_worlds_and_the_starter_spend_64_texels_per_sector() {
    assert_eq!(GridWorld::new().texels_per_sector.get(), 64);
    assert_eq!(GridWorld::default().texels_per_sector, SectorTexels::DEFAULT);
    let starter = starter_world(Some(rid(1)), Some(rid(2)), Some(rid(3)));
    assert_eq!(starter.texels_per_sector.get(), 64);
}

#[test]
fn a_stored_world_without_the_setting_reads_as_32_and_writes_back_without_it() {
    let old: GridWorld = ron::from_str("(rooms: [])").expect("a world from before the setting");
    assert_eq!(old.texels_per_sector, SectorTexels::LEGACY);
    assert_eq!(old.texels_per_sector.get(), 32);
    let text = ron::to_string(&old).unwrap();
    assert!(
        !text.contains("texels_per_sector"),
        "a legacy world must not grow a field: {text}"
    );
    let new = GridWorld::new();
    let text = ron::to_string(&new).unwrap();
    assert!(text.contains("texels_per_sector:64"), "{text}");
    let back: GridWorld = ron::from_str(&text).unwrap();
    assert_eq!(back, new);
}

#[test]
fn every_tracked_reference_project_still_reads_as_32() {
    for name in [
        "default",
        "graybox-reach",
        "grid-cathedral",
        "grid-cave",
        "grid-corridor",
        "grid-dungeon",
        "grid-gate",
        "grid-level008",
        "grid-sewers",
        "grid-starter",
        "grid-tower",
        "grid-west",
    ] {
        let path = crate::projects_dir().join(name).join("project.ron");
        let project = crate::ProjectDocument::load_from_path(path).expect("loads");
        let Some(world) = project.active_scene().world.grid() else {
            continue;
        };
        assert_eq!(
            world.texels_per_sector,
            SectorTexels::LEGACY,
            "{name} must keep its 32 texels per sector"
        );
    }
}

#[test]
fn a_default_floor_spends_the_worlds_texels_per_sector() {
    for (texels, expect) in [
        (SectorTexels::DEFAULT, 64.0),
        (SectorTexels::LEGACY, 32.0),
    ] {
        let faces = all_faces(&floor_world(texels, FaceUv::default()));
        assert_eq!(extent(&faces[0]), [expect, expect], "{texels:?}");
    }
}

#[test]
fn an_authored_wall_spends_the_same_density() {
    for (texels, expect) in [
        (SectorTexels::DEFAULT, 64.0),
        (SectorTexels::LEGACY, 32.0),
    ] {
        let mut world = floor_world(texels, FaceUv::default());
        world.rooms[0]
            .sector_or_insert(3, 2)
            .unwrap()
            .walls_mut(WallEdge::North)
            .push(GridWall::span(0, 1024, Some(rid(8))));
        let faces = all_faces(&world);
        let wall = faces
            .iter()
            .find(|f| matches!(f.kind, GridFaceKind::Wall { .. }))
            .expect("the wall");
        assert_eq!(extent(wall), [expect, expect], "{texels:?}");
    }
}

#[test]
fn a_derived_closure_wall_spends_the_same_density() {
    for (texels, expect) in [
        (SectorTexels::DEFAULT, 64.0),
        (SectorTexels::LEGACY, 32.0),
    ] {
        let mut world = starter_world(Some(rid(1)), Some(rid(2)), Some(rid(3)));
        world.texels_per_sector = texels;
        let faces = all_faces(&world);
        let wall = faces
            .iter()
            .find(|f| matches!(f.kind, GridFaceKind::Derived { .. }))
            .expect("a closure wall");
        // One sector long; two sectors tall.
        assert_eq!(extent(wall)[0], expect, "{texels:?}");
        assert_eq!(extent(wall)[1], expect * 2.0, "{texels:?}");
    }
}

#[test]
fn a_plane_in_tile_mode_spends_the_same_density() {
    for (texels, expect) in [
        (SectorTexels::DEFAULT, 64.0),
        (SectorTexels::LEGACY, 32.0),
    ] {
        let mut world = floor_world(texels, FaceUv::default());
        let mut plane = GridPlane::new([512, 512, 512], [1024, 1024]);
        plane.material = Some(rid(9));
        world.rooms[0].planes.push(plane);
        let faces = all_faces(&world);
        let face = faces
            .iter()
            .find(|f| matches!(f.kind, GridFaceKind::Plane { .. }))
            .expect("the plane");
        assert_eq!(extent(face), [expect, expect], "{texels:?}");
    }
}

#[test]
fn fine_rotation_and_scale_turn_about_the_face_centre_at_any_density() {
    for (texels, per_sector) in [(SectorTexels::DEFAULT, 64.0), (SectorTexels::LEGACY, 32.0)] {
        let uv = FaceUv {
            rotation_deg: 90,
            scale_q8: [512, 512],
            ..FaceUv::default()
        };
        let faces = all_faces(&floor_world(texels, uv));
        let c = centre(&faces[0]);
        // Sector (3, 2): its centre is at (3.5, 2.5) sectors.
        assert!((c[0] - 3.5 * per_sector).abs() < 1e-9, "{texels:?} {c:?}");
        assert!((c[1] - 2.5 * per_sector).abs() < 1e-9, "{texels:?} {c:?}");
    }
}

#[test]
fn a_materials_own_tiling_beats_the_worlds_density() {
    struct Sixteen;
    impl UvDefaults for Sixteen {
        fn default_uv(&self, _material: ResourceId) -> Option<UvMode> {
            Some(UvMode::World {
                texels_per_sector: [16, 16],
            })
        }
    }
    let world = floor_world(SectorTexels::DEFAULT, FaceUv::default());
    let faces = all_faces_with(&world, &Sixteen);
    assert_eq!(extent(&faces[0]), [16.0, 16.0]);
}

#[test]
fn the_inspector_report_counts_tiles_at_the_worlds_density() {
    use crate::grid::edit::FaceRef;
    for (texels, tiles) in [(SectorTexels::DEFAULT, 1.0), (SectorTexels::LEGACY, 0.5)] {
        let world = floor_world(texels, FaceUv::default());
        let face = FaceRef::floor(0, [3, 2]);
        let report = crate::grid::uv::report_for(&world, &(), face, None).expect("a report");
        assert_eq!(report.tiles, [tiles, tiles], "{texels:?}");
    }
}

#[test]
fn a_generated_mesh_follows_the_world_density() {
    use crate::grid::mesh::{ArchSpec, GridMesh, MeshAsset, MeshKind};
    let at = |texels: SectorTexels| {
        let mut room = GridRoom::new("r", 6, 6);
        room.meshes.push(GridMesh::new(1, 0, [3072, 0, 3072]));
        let world = GridWorld {
            scale: GridScale { num: 1, den: 1 },
            texels_per_sector: texels,
            rooms: vec![room],
            mesh_assets: vec![MeshAsset::new("arch", MeshKind::Arch(ArchSpec::default()))],
            ..GridWorld::default()
        };
        let mut out = Vec::new();
        crate::grid::mesh_faces::mesh_faces_with(&world, &(), 0, 0, &mut out);
        out.iter().map(|f| extent(f)[0]).fold(0.0, f64::max)
    };
    let (legacy, new) = (at(SectorTexels::LEGACY), at(SectorTexels::DEFAULT));
    assert!(legacy > 0.0);
    assert_eq!(new, legacy * 2.0, "an arch lays its texture on the world lattice");
}

mod switching {
    use super::*;
    use crate::grid::density::{set_texels_per_sector, KeepLook};
    use crate::grid::uv::ProjectUvDefaults;
    use crate::grid::world::SceneWorld;
    use crate::{MaterialResource, ProjectDocument, ResourceData};

    /// A project whose one room has a floor in a 32 wide material, a floor
    /// in a 64 wide one, a tiled plane, and a closure-walled derived room.
    fn project() -> (ProjectDocument, ResourceId, ResourceId) {
        let mut project = ProjectDocument::new("t");
        let mut add = |name: &str| {
            project.add_resource(name, ResourceData::Material(MaterialResource::opaque(None)))
        };
        let small = add("small");
        let big = add("big");
        let mut room = GridRoom::new("r", 4, 4);
        for (x, material) in [(0usize, small), (1, big)] {
            room.sector_or_insert(x, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, Some(material))));
        }
        let mut plane = GridPlane::new([512, 512, 512], [1024, 1024]);
        plane.material = Some(small);
        room.planes.push(plane);
        let mut world = GridWorld {
            scale: GridScale::DEFAULT,
            rooms: vec![room],
            ..GridWorld::default()
        };
        world.texels_per_sector = SectorTexels::LEGACY;
        project.active_scene_mut().world = SceneWorld::Grid(world);
        (project, small, big)
    }

    fn width(small: ResourceId, big: ResourceId) -> impl Fn(&ProjectDocument, ResourceId) -> Option<u16> {
        move |_, id| (id == small).then_some(32).or((id == big).then_some(64))
    }

    /// Every face as `(material, texels)`, in order.
    fn look(project: &ProjectDocument) -> Vec<(Option<ResourceId>, Vec<[f64; 2]>)> {
        let world = project.active_scene().world.grid().unwrap();
        all_faces_with(world, &ProjectUvDefaults(project))
            .into_iter()
            .map(|f| (f.material, f.texels))
            .collect()
    }

    #[test]
    fn keeping_other_texture_sizes_leaves_a_32_wide_texture_as_it_was() {
        let (mut project, small, big) = project();
        let before = look(&project);
        let change = set_texels_per_sector(
            &mut project,
            SectorTexels::DEFAULT,
            KeepLook::OtherTextureSizes,
            &width(small, big),
        )
        .unwrap();
        assert_eq!(change.from, Some(SectorTexels::LEGACY));
        assert_eq!((change.materials_pinned, change.planes_pinned), (1, 1));
        let after = look(&project);
        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(&after) {
            if b.0 == Some(small) {
                assert_eq!(b, a, "the 32 wide texture keeps its size");
            } else {
                // The 64 wide texture now spans a whole texture per sector.
                let scaled: Vec<[f64; 2]> = b.1.iter().map(|t| [t[0] * 2.0, t[1] * 2.0]).collect();
                assert_eq!(a.1, scaled);
            }
        }
        assert_eq!(
            project.active_scene().world.grid().unwrap().texels_per_sector,
            SectorTexels::DEFAULT
        );
    }

    #[test]
    fn keeping_everything_changes_no_face() {
        let (mut project, small, big) = project();
        let before = look(&project);
        set_texels_per_sector(
            &mut project,
            SectorTexels::DEFAULT,
            KeepLook::Everything,
            &width(small, big),
        )
        .unwrap();
        assert_eq!(look(&project), before);
    }

    #[test]
    fn keeping_nothing_moves_every_face_to_the_new_density() {
        let (mut project, small, big) = project();
        let before = look(&project);
        let change = set_texels_per_sector(
            &mut project,
            SectorTexels::DEFAULT,
            KeepLook::Nothing,
            &width(small, big),
        )
        .unwrap();
        assert_eq!((change.materials_pinned, change.planes_pinned), (0, 0));
        for (b, a) in before.iter().zip(&look(&project)) {
            let scaled: Vec<[f64; 2]> = b.1.iter().map(|t| [t[0] * 2.0, t[1] * 2.0]).collect();
            assert_eq!(a.1, scaled);
        }
    }

    #[test]
    fn the_same_density_is_no_change() {
        let (mut project, small, big) = project();
        let before = project.clone();
        let change = set_texels_per_sector(
            &mut project,
            SectorTexels::LEGACY,
            KeepLook::Everything,
            &width(small, big),
        )
        .unwrap();
        assert_eq!((change.materials_pinned, change.planes_pinned), (0, 0));
        assert_eq!(project, before);
    }
}
