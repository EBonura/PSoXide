//! Embedded-Play PXBSP constants and regression coverage.

pub const BRUSH_WORLD_FILENAME: &str = "brush_world.pxbsp";

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::brush_world::{compile_brush_world, BrushWorldCookMode, BrushWorldCookOptions};
    use crate::playtest::{PlaytestAssetKind, PlaytestWorldGeometry, StreamedClass};
    use crate::{
        ArchPropGeometry, BoxPropErosion, GridUvTransform, NodeKind, ProjectDocument, SkyMode,
        ARCH_PROP_MATERIAL_COUNT, BOX_PROP_FACE_COUNT,
    };
    use psx_bsp::collision::{Trace, TraceScratch, Q12_ONE};
    use psx_bsp::collision_provider::{select_body_hull, CookedBodyHull, PxbspCollisionProvider};
    use psx_bsp::mover::BrushDoorSet;
    use psx_bsp::pxbsp::{entity_class, entity_flags};
    use psx_bsp::pxbsp_resident::PxbspResidentMap;
    use psx_bsp::{SliceReader, Vec3I32};
    use psx_engine::{
        commit_body_step_with_trace_provider, trace_collision, CharacterBlockerTraceProvider,
        CharacterCollisionAabb, CollisionTraceQuery, CollisionTraceShape, RoomPoint,
    };

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
        // Lighting subdivision preserves surface order and this room's
        // faces are all under one lighting patch, so the packed world
        // is byte-identical to the pre-subdivision output.
        assert_eq!(world.bytes.len(), 13_008);
        assert_eq!(world.movers.len(), 1);
        assert_eq!(world.movers[0].model_index, 1);
        assert_eq!(package.rooms.len(), 1);
        assert_eq!(package.rooms[0].world_asset_index, None);
        assert!(package.chunks.is_empty());
        assert!(package.room_visibility.is_empty());
        assert_eq!(package.assets.len(), 2);
        assert_eq!(package.texture_asset_count(), 2);
        assert_eq!(world.texture_asset_indices, [0]);
        assert_eq!(package.rooms[0].sky.flags, psx_level::sky_flags::ENABLED);
        let sky_asset_index = package.rooms[0]
            .sky
            .cloud_layer
            .texture_asset_index
            .expect("PXBSP room has its authored sky panorama");
        assert_eq!(sky_asset_index, 1);
        let sky_asset = &package.assets[sky_asset_index];
        assert_eq!(sky_asset.kind, PlaytestAssetKind::Texture);
        assert_eq!(sky_asset.streamed_class, StreamedClass::Gameplay);
        assert_eq!(sky_asset.filename, "sky/sky_000.psxt");
        assert_eq!(sky_asset.source_label, "Cooked Sky Panorama 0");
        let sky_texture = psx_asset::Texture::from_bytes(&sky_asset.bytes)
            .expect("cooked PXBSP sky panorama parses as PSXT");
        assert_eq!((sky_texture.width(), sky_texture.height()), (512, 256));
        assert_eq!(
            world.body_hulls,
            [
                psx_bsp::collision_provider::CookedBodyHull::new(1, 16, 56),
                psx_bsp::collision_provider::CookedBodyHull::new(2, 32, 96),
            ]
        );
        assert_eq!(package.spawn.expect("player spawn").room, 0);
        assert_eq!(package.spawn.expect("player spawn").yaw, 3072);
        assert_eq!(package.logic.len(), 1);
        assert_eq!(package.logic[0].link, 0, "door links BSP mover ordinal");
        let envelope = crate::playtest::playtest_performance_envelope(&package)
            .expect("resident PXBSP performance envelope");
        assert!(envelope.room_surfaces > 0);
        assert!(envelope.authored_triangles > 0);
        assert_eq!(envelope.resident_stream_bytes, 0);

        let source = crate::playtest::render_manifest_source(&package);
        assert!(source.contains("pub const PLAYTEST_USES_PXBSP: bool = true;"));
        assert!(source.contains("pub static PXBSP_WORLD: &[u8]"));
        assert!(source.contains("PXBSP_MOVER_NODE_IDS: &[u32] = &[2]"));
        assert!(source.contains("CookedBodyHull::new(1, 16, 56)"));
        assert!(source.contains("CookedBodyHull::new(2, 32, 96)"));
        assert!(source.contains("world_asset: AssetId(65535)"));
        assert!(source.contains("ROOM_0_REQUIRED_VRAM: &[AssetId] = &[AssetId(1), AssetId(0)]"));
        assert!(source.contains("flags: asset_flags::STREAMED_GAMEPLAY_TRANSIENT"));
        assert!(source.contains("texture_asset: AssetId(1)"));
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
    fn pxbsp_sky_off_omits_the_panorama_and_preserves_brush_texture_ids() {
        let mut project = ProjectDocument::from_ron_str(include_str!(
            "../../../projects/brush-first-playable/project.ron"
        ))
        .expect("brush first-playable fixture");
        let world_id = project.active_scene().root;
        let world = project
            .active_scene_mut()
            .node_mut(world_id)
            .expect("brush world node");
        let NodeKind::World { sky, .. } = &mut world.kind else {
            panic!("scene root is not a World");
        };
        sky.mode = SkyMode::Off;

        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
        let (package, report) = crate::playtest::build_package(&project, &fixture_dir);
        assert!(report.is_ok(), "normal brush package: {:?}", report.errors);
        let package = package.expect("normal package");
        let crate::playtest::PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
            panic!("brush project selected the grid provider");
        };

        let enabled_world_bytes = {
            let enabled_project = ProjectDocument::from_ron_str(include_str!(
                "../../../projects/brush-first-playable/project.ron"
            ))
            .expect("enabled brush first-playable fixture");
            let enabled_package = crate::playtest::build_package(&enabled_project, &fixture_dir)
                .0
                .expect("enabled normal package");
            let crate::playtest::PlaytestWorldGeometry::Pxbsp(enabled_world) =
                enabled_package.world_geometry
            else {
                panic!("enabled brush project selected the grid provider");
            };
            enabled_world.bytes
        };

        assert_eq!(package.rooms[0].sky.flags, 0);
        assert_eq!(package.rooms[0].sky.cloud_layer.flags, 0);
        assert_eq!(package.rooms[0].sky.cloud_layer.texture_asset_index, None);
        assert_eq!(package.assets.len(), 1);
        assert_eq!(world.texture_asset_indices, [0]);
        assert_eq!(world.bytes, enabled_world_bytes);
        assert!(package
            .assets
            .iter()
            .all(|asset| !asset.filename.starts_with("sky/")));
        assert!(crate::playtest::render_manifest_source(&package)
            .contains("ROOM_0_REQUIRED_VRAM: &[AssetId] = &[AssetId(0)]"));
    }

    #[test]
    fn brush_combat_cooks_authored_player_and_enemy_body_hulls() {
        let project = ProjectDocument::from_ron_str(include_str!(
            "../../../projects/brush-combat-fixture/project.ron"
        ))
        .expect("brush combat fixture");
        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-combat-fixture");
        let (package, report) = crate::playtest::build_package(&project, &fixture_dir);
        assert!(report.is_ok(), "brush combat package: {:?}", report.errors);
        let package = package.expect("brush combat package");
        let PlaytestWorldGeometry::Pxbsp(world) = package.world_geometry else {
            panic!("brush combat selected the grid provider");
        };
        assert_eq!(
            world.body_hulls,
            [
                psx_bsp::collision_provider::CookedBodyHull::new(1, 188, 1024),
                psx_bsp::collision_provider::CookedBodyHull::new(2, 192, 1024),
            ]
        );
        let mut map = PxbspResidentMap::with_capacity(world.bytes.len());
        map.load(0, &mut SliceReader::new(&world.bytes))
            .expect("resident combat PXBSP");
        let door = map.brush_models().get(1).expect("combat door model");
        assert_eq!(
            door.origin,
            psx_bsp::Vec3I16 {
                x: 2048,
                y: 256,
                z: 1536,
            }
        );
        assert_eq!(
            door.mins,
            psx_bsp::Vec3I16 {
                x: -32,
                y: 0,
                z: -256,
            }
        );
        assert_eq!(
            door.maxs,
            psx_bsp::Vec3I16 {
                x: 32,
                y: 768,
                z: 256,
            }
        );
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

    fn pxbsp_body_step(
        map: &PxbspResidentMap,
        hulls: &[CookedBodyHull],
        blockers: &[CharacterCollisionAabb],
        start: RoomPoint,
        end: RoomPoint,
        radius: i32,
        height: i32,
    ) -> psx_engine::BodyStep {
        let shape = CollisionTraceShape::Body { radius, height };
        let hull_index = select_body_hull(hulls, radius, height).expect("authored body hull");
        let mut scratch = TraceScratch::new();
        let mut world = PxbspCollisionProvider::new(map, hull_index, &[], shape, &mut scratch)
            .expect("resident PXBSP body provider");
        let mut composed = CharacterBlockerTraceProvider::new_with_aabbs(&mut world, &[], blockers);
        commit_body_step_with_trace_provider(
            &mut composed,
            start,
            end.x.saturating_sub(start.x),
            end.z.saturating_sub(start.z),
            radius,
            height,
        )
        .expect("bounded trace succeeds")
    }

    #[test]
    fn real_pxbsp_player_and_npc_steps_obey_cooked_box_and_arch_props() {
        let mut project = ProjectDocument::from_ron_str(include_str!(
            "../../../projects/brush-first-playable/project.ron"
        ))
        .expect("brush first-playable fixture");
        let material = project.resources[0].id;
        let world_id = project
            .active_scene()
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::World { .. }))
            .map(|node| node.id)
            .expect("brush world node");
        if let NodeKind::World { sector_size, .. } = &mut project
            .active_scene_mut()
            .node_mut(world_id)
            .expect("brush world node")
            .kind
        {
            *sector_size = 128;
        }

        let box_vertices = [
            [-32, 0, -32],
            [32, 0, -32],
            [32, 64, -32],
            [-32, 64, -32],
            [-32, 0, 32],
            [32, 0, 32],
            [32, 64, 32],
            [-32, 64, 32],
        ];
        let collidable_box = project.active_scene_mut().add_node(
            world_id,
            "PXBSP blocking box",
            NodeKind::BoxProp {
                materials: [Some(material); BOX_PROP_FACE_COUNT],
                uvs: [GridUvTransform::IDENTITY; BOX_PROP_FACE_COUNT],
                vertices: box_vertices,
                collision_enabled: true,
                break_flags: 0,
                erosion: BoxPropErosion::default(),
            },
        );
        project
            .active_scene_mut()
            .node_mut(collidable_box)
            .expect("blocking box")
            .transform
            .translation = [352.0, 65.0, 192.0];
        let decorative_box = project.active_scene_mut().add_node(
            world_id,
            "PXBSP decorative box",
            NodeKind::BoxProp {
                materials: [Some(material); BOX_PROP_FACE_COUNT],
                uvs: [GridUvTransform::IDENTITY; BOX_PROP_FACE_COUNT],
                vertices: box_vertices,
                collision_enabled: false,
                break_flags: 0,
                erosion: BoxPropErosion::default(),
            },
        );
        project
            .active_scene_mut()
            .node_mut(decorative_box)
            .expect("decorative box")
            .transform
            .translation = [352.0, 65.0, 576.0];
        let arch = project.active_scene_mut().add_node(
            world_id,
            "PXBSP blocking arch",
            NodeKind::ArchProp {
                materials: [Some(material); ARCH_PROP_MATERIAL_COUNT],
                uvs: [GridUvTransform::IDENTITY; ARCH_PROP_MATERIAL_COUNT],
                geometry: ArchPropGeometry {
                    span_tiles: 2,
                    depth_tiles: 1,
                    rise_quanta: 2,
                    leg_height_quanta: 2,
                    band_thickness_quanta: 1,
                    ..ArchPropGeometry::default()
                },
                collision_enabled: true,
            },
        );
        project
            .active_scene_mut()
            .node_mut(arch)
            .expect("blocking arch")
            .transform
            .translation = [736.0, 65.0, 384.0];

        let fixture_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
        let (package, report) = crate::playtest::build_package(&project, &fixture_dir);
        assert!(report.is_ok(), "PXBSP prop package: {:?}", report.errors);
        let package = package.expect("PXBSP prop package");
        let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
            panic!("brush project selected the grid provider");
        };
        let mut map = PxbspResidentMap::with_capacity(world.bytes.len());
        map.load(0, &mut SliceReader::new(&world.bytes))
            .expect("resident prop PXBSP");

        assert_eq!(package.box_props.len(), 2);
        let blocking = package
            .box_props
            .iter()
            .find(|prop| prop.flags & psx_level::box_prop_flags::COLLISION_ENABLED != 0)
            .expect("collidable box");
        let decorative = package
            .box_props
            .iter()
            .find(|prop| prop.flags & psx_level::box_prop_flags::COLLISION_ENABLED == 0)
            .expect("non-collidable box");
        let box_blocker = CharacterCollisionAabb::new(
            RoomPoint::new(
                blocking.collision_min[0],
                blocking.collision_min[1],
                blocking.collision_min[2],
            ),
            RoomPoint::new(
                blocking.collision_max[0],
                blocking.collision_max[1],
                blocking.collision_max[2],
            ),
        );
        assert!(box_blocker.is_strictly_valid());
        assert_eq!(decorative.collision_min, [320, 65, 544]);
        assert_eq!(decorative.collision_max, [384, 129, 608]);

        let melee_from = RoomPoint::new(256, 96, 192);
        let melee_to = RoomPoint::new(440, 96, 192);
        let mut scratch = TraceScratch::new();
        let mut pxbsp =
            PxbspCollisionProvider::new(&map, 0, &[], CollisionTraceShape::Point, &mut scratch)
                .expect("resident PXBSP point provider");
        let melee_blockers = [box_blocker];
        let mut composed =
            CharacterBlockerTraceProvider::new_with_aabbs(&mut pxbsp, &[], &melee_blockers);
        let melee_trace = trace_collision(
            &mut composed,
            CollisionTraceQuery::point(melee_from, melee_to),
        )
        .expect("bounded PXBSP prop point trace");
        assert!(
            melee_trace.hit(),
            "the cooked collidable box occludes a gameplay point segment"
        );
        assert_eq!(melee_trace.normal_q12, [-4096, 0, 0]);

        for (radius, height) in [(16, 56), (8, 32)] {
            let start = RoomPoint::new(256, 65, 192);
            let end = RoomPoint::new(440, 65, 192);
            let open = pxbsp_body_step(&map, &world.body_hulls, &[], start, end, radius, height);
            assert_eq!(
                open.position,
                RoomPoint::new(end.x, 64, end.z),
                "world-only path is clear"
            );
            let blocked = pxbsp_body_step(
                &map,
                &world.body_hulls,
                &[box_blocker],
                start,
                end,
                radius,
                height,
            );
            assert!(blocked.blocked, "player/NPC body is blocked by cooked box");
            assert_ne!(blocked.position, end);

            let decorative_start = RoomPoint::new(256, 65, 576);
            let decorative_end = RoomPoint::new(440, 65, 576);
            let decorative_step = pxbsp_body_step(
                &map,
                &world.body_hulls,
                &[],
                decorative_start,
                decorative_end,
                radius,
                height,
            );
            assert_eq!(
                decorative_step.position,
                RoomPoint::new(decorative_end.x, 64, decorative_end.z),
                "non-collidable box stays decorative"
            );
        }

        let arch_record = package.arch_props.first().expect("collidable arch");
        let arch_collisions = &package.arch_prop_collisions[usize::from(arch_record.collision_first)
            ..usize::from(arch_record.collision_first) + usize::from(arch_record.collision_count)];
        let arch_collision = arch_collisions
            .iter()
            .find(|collision| {
                collision.min[0] > 128
                    && collision.max[0] < 896
                    && collision.min[1] <= 65
                    && collision.max[1] >= 97
            })
            .expect("low arch collision inside the room");
        let arch_blocker = CharacterCollisionAabb::new(
            RoomPoint::new(
                arch_collision.min[0],
                arch_collision.min[1],
                arch_collision.min[2],
            ),
            RoomPoint::new(
                arch_collision.max[0],
                arch_collision.max[1],
                arch_collision.max[2],
            ),
        );
        let arch_z = (arch_collision.min[2] + arch_collision.max[2]) / 2;
        let arch_start = RoomPoint::new(arch_collision.min[0] - 48, 65, arch_z);
        let arch_end = RoomPoint::new(arch_collision.max[0] + 48, 65, arch_z);
        let open = pxbsp_body_step(&map, &world.body_hulls, &[], arch_start, arch_end, 16, 56);
        assert_eq!(
            open.position,
            RoomPoint::new(arch_end.x, 64, arch_end.z),
            "world-only arch path is clear"
        );
        let blocked = pxbsp_body_step(
            &map,
            &world.body_hulls,
            &[arch_blocker],
            arch_start,
            arch_end,
            16,
            56,
        );
        assert!(blocked.blocked, "cooked arch segment blocks BSP body");
        assert_ne!(blocked.position, arch_end);
    }
}
