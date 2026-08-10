//! Embedded-Play PXBSP constants and regression coverage.

pub const BRUSH_WORLD_FILENAME: &str = "brush_world.pxbsp";

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::brush_world::{compile_brush_world, BrushWorldCookMode, BrushWorldCookOptions};
    use crate::playtest::PlaytestWorldGeometry;
    use crate::ProjectDocument;
    use psx_bsp::collision::{Trace, TraceScratch, Q12_ONE};
    use psx_bsp::mover::BrushDoorSet;
    use psx_bsp::pxbsp::{entity_class, entity_flags};
    use psx_bsp::pxbsp_resident::PxbspResidentMap;
    use psx_bsp::{SliceReader, Vec3I32};

    #[test]
    fn first_playable_brush_world_uses_the_normal_gameplay_package() {
        let project = ProjectDocument::from_ron_str(include_str!(
            "../../../projects/brush-first-playable/project.ron"
        ))
        .expect("brush first-playable fixture");
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
        let (package, report) = crate::playtest::build_package(&project, &fixture_dir);
        assert!(report.is_ok(), "normal brush package: {:?}", report.errors);
        let package = package.expect("normal package");
        let crate::playtest::PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
            panic!("brush project selected the grid provider");
        };
        assert_eq!(world.bytes.len(), 13_008);
        assert_eq!(world.movers.len(), 1);
        assert_eq!(world.movers[0].model_index, 1);
        assert_eq!(package.rooms.len(), 1);
        assert_eq!(package.assets.len(), 2);
        assert_eq!(package.texture_asset_count(), 1);
        assert_eq!(world.texture_asset_indices, [1]);
        assert_eq!(package.spawn.expect("player spawn").room, 0);
        assert_eq!(package.spawn.expect("player spawn").yaw, 3072);
        assert_eq!(package.logic.len(), 1);
        assert_eq!(package.logic[0].link, 0, "door links BSP mover ordinal");

        let source = crate::playtest::render_manifest_source(&package);
        assert!(source.contains("pub const PLAYTEST_USES_PXBSP: bool = true;"));
        assert!(source.contains("pub static PXBSP_WORLD: &[u8]"));
        assert!(source.contains("PXBSP_MOVER_NODE_IDS: &[u32] = &[2]"));
        assert!(source.contains("ROOM_0_REQUIRED_VRAM: &[AssetId] = &[AssetId(1)]"));
        assert!(!source.contains("BRUSH_TEXTURES"));

        let runtime_main = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../engine/examples/editor-playtest/src/main.rs"),
        )
        .expect("normal editor-playtest runtime source");
        assert!(runtime_main.contains("mod bsp_runtime;"));
        assert!(!runtime_main.contains("brush_playtest::run()"));
    }

    #[test]
    fn project_cook_choice_reaches_the_normal_package_and_compiler() {
        let mut project = ProjectDocument::from_ron_str(include_str!(
            "../../../projects/brush-first-playable/project.ron"
        ))
        .expect("brush first-playable fixture");
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");

        assert_eq!(project.bsp_cook_mode, BrushWorldCookMode::Draft);
        let draft = crate::playtest::build_package(&project, &fixture_dir)
            .0
            .expect("Draft package");
        project.bsp_cook_mode = BrushWorldCookMode::Release;
        let release = crate::playtest::build_package(&project, &fixture_dir)
            .0
            .expect("Release package");

        assert_eq!(draft.bsp_cook_mode, BrushWorldCookMode::Draft);
        assert_eq!(release.bsp_cook_mode, BrushWorldCookMode::Release);
        let PlaytestWorldGeometry::Pxbsp(draft_world) = &draft.world_geometry else {
            panic!("Draft package is not PXBSP");
        };
        let PlaytestWorldGeometry::Pxbsp(release_world) = &release.world_geometry else {
            panic!("Release package is not PXBSP");
        };
        assert_ne!(draft_world.bytes, release_world.bytes);
        assert!(crate::playtest::render_manifest_source(&draft)
            .contains("pub const BSP_COOK_IS_RELEASE: bool = false;"));
        assert!(crate::playtest::render_manifest_source(&release)
            .contains("pub const BSP_COOK_IS_RELEASE: bool = true;"));

        let restored = ProjectDocument::from_ron_str(&project.to_ron_string().expect("serialize"))
            .expect("reload");
        assert_eq!(restored.bsp_cook_mode, BrushWorldCookMode::Release);
    }

    #[test]
    fn first_playable_fixture_opens_a_player_hull_route_through_its_door() {
        let project = ProjectDocument::from_ron_str(include_str!(
            "../../../projects/brush-first-playable/project.ron"
        ))
        .expect("brush first-playable fixture");
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
        let compiled = compile_brush_world(
            &project,
            BrushWorldCookOptions {
                project_root: &fixture_dir,
                mode: BrushWorldCookMode::Draft,
                ambient: [32; 3],
                texture_asset_base: 0,
            },
        )
        .expect("compile brush first-playable fixture");
        let mut map = PxbspResidentMap::with_capacity(compiled.pxbsp.bytes.len());
        map.load(0, &mut SliceReader::new(&compiled.pxbsp.bytes))
            .expect("load brush first-playable fixture");

        let entities = map.entities();
        let spawn = (0..entities.len())
            .filter_map(|index| entities.get(index))
            .find(|entity| {
                entity.class_id == entity_class::PLAYER_SPAWN
                    && entity.flags & entity_flags::ENABLED != 0
            })
            .expect("enabled player spawn");
        let destination = Vec3I32 {
            x: 768 * 4096,
            ..spawn.origin
        };
        let mut scratch = TraceScratch::new();
        let mut camera_trace = Trace::default();
        let camera_destination = Vec3I32 {
            x: 2_909 * 4096,
            ..spawn.origin
        };
        assert!(map
            .model_collision_hull(0, 0)
            .expect("world point hull")
            .trace_into(
                &spawn.origin,
                &camera_destination,
                &mut scratch,
                &mut camera_trace,
            ));
        assert!(
            camera_trace.fraction < Q12_ONE,
            "third-person camera cannot leave the brush world"
        );
        let mut world_trace = Trace::default();
        assert!(map
            .model_collision_hull(0, 1)
            .expect("world player hull")
            .trace_into(&spawn.origin, &destination, &mut scratch, &mut world_trace,));
        assert_eq!(world_trace.fraction, Q12_ONE, "static doorway is clear");

        let mut doors = BrushDoorSet::<1>::default();
        doors.init_from_map(&map).expect("one runtime door");
        assert_eq!(doors.len(), 1);
        let mut closed_trace = Trace::default();
        assert!(map
            .model_collision_hull(1, 1)
            .expect("door player hull")
            .transformed(doors.get(0).expect("door").transform())
            .trace_into(&spawn.origin, &destination, &mut scratch, &mut closed_trace,));
        assert!(
            closed_trace.fraction < Q12_ONE,
            "closed door blocks the route"
        );

        doors.get_mut(0).expect("door").set_open(true);
        for _ in 0..60 {
            doors.tick();
        }
        let mut open_trace = Trace::default();
        assert!(map
            .model_collision_hull(1, 1)
            .expect("door player hull")
            .transformed(doors.get(0).expect("door").transform())
            .trace_into(&spawn.origin, &destination, &mut scratch, &mut open_trace,));
        assert_eq!(open_trace.fraction, Q12_ONE, "open door clears the route");

        let mut tape_doors = BrushDoorSet::<1>::default();
        tape_doors.init_from_map(&map).expect("one tape door");
        let mut tape_origin = spawn.origin;
        for tick in 0..150 {
            if tick == 24 {
                tape_doors.get_mut(0).expect("tape door").toggle();
            }
            tape_doors.tick();
            let candidate = Vec3I32 {
                x: tape_origin.x + 4 * 4096,
                ..tape_origin
            };
            let mut trace = Trace::default();
            assert!(map
                .model_collision_hull(0, 1)
                .expect("world player hull")
                .trace_into(&tape_origin, &candidate, &mut scratch, &mut trace));
            let mut door_trace = Trace::default();
            assert!(map
                .model_collision_hull(1, 1)
                .expect("door player hull")
                .transformed(tape_doors.get(0).expect("tape door").transform())
                .trace_into(&tape_origin, &candidate, &mut scratch, &mut door_trace,));
            if door_trace.fraction < trace.fraction {
                trace = door_trace;
            }
            tape_origin = trace.end;
        }
        assert!(
            tape_origin.x >= 768 * 4096,
            "walkthrough tape reaches the second room: x={}",
            tape_origin.x >> 12
        );
    }
}
