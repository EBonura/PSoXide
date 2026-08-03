use super::*;
use crate::{WaterVolumeCell, WaterVolumeSettings};

#[test]
fn water_volume_cooks_sparse_cells_and_transparent_surface() {
    let mut project = project_with_one_room();
    let material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id)
        .expect("fixture material");
    if let ResourceData::Material(resource) = &mut project.resource_mut(material).unwrap().data {
        resource.blend_mode = PsxBlendMode::Average;
        resource.animation.mode = crate::MaterialAnimationMode::UvScroll;
        resource.animation.uv_scroll.enabled = true;
        resource.animation.uv_scroll.speed_u_q8 = 8 * 256;
        resource.animation.uv_scroll.speed_v_q8 = 1203;
    }
    let room = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("fixture room");
    let (world_x, world_z) = {
        let NodeKind::Section { grid } =
            &mut project.active_scene_mut().node_mut(room).unwrap().kind
        else {
            unreachable!()
        };
        let grid = grid.floor_mut(0).unwrap();
        let sector = grid.sector_mut(0, 0).expect("fixture floor cell");
        let floor_material = sector.floor.as_ref().expect("floor").material;
        sector.floor.as_mut().expect("floor").heights = [-512; 4];
        let second_world_x = grid.origin[0] + 1;
        let second_world_z = grid.origin[1];
        let (second_x, second_z) = grid.extend_to_include(second_world_x, second_world_z);
        grid.set_floor(second_x, second_z, -256, floor_material);
        let raised = grid
            .sector_mut(second_x, second_z)
            .expect("second fixture floor cell");
        raised.floor.as_mut().expect("floor").heights = [-256; 4];
        (grid.origin[0], grid.origin[1])
    };
    project.active_scene_mut().add_node(
        room,
        "Deep water",
        NodeKind::WaterVolume {
            material: Some(material),
            cells: vec![
                WaterVolumeCell::new(world_x, world_z),
                WaterVolumeCell::new(world_x + 1, world_z),
            ],
            settings: WaterVolumeSettings {
                height_above_floor: 512,
                lethal_depth: 384,
                movement_percent: 65,
                death_delay_ticks: 30,
                death_submerge_depth: 64,
            },
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("water project cooks");
    assert_eq!(package.water_cells.len(), 2);
    let water = package.water_cells[0];
    let raised_water = package.water_cells[1];
    assert_eq!(water.depth, 512);
    assert_eq!(raised_water.depth, 512);
    assert_eq!(water.surface_y, 0);
    assert_eq!(raised_water.surface_y, 256);
    assert_eq!(water.lethal_depth, 384);
    assert_eq!(water.movement_percent, 65);
    assert_eq!(water.blend_mode, psx_level::model_override_blend::AVERAGE);
    assert_eq!(water.animation.mode, crate::MaterialAnimationMode::UvScroll);
    assert_eq!(water.animation.uv_scroll.speed_u_q8, 8 * 256);
    assert_eq!(water.animation.uv_scroll.speed_v_q8, 1203);
    assert!(water.texture_asset_index.is_some());

    let manifest = render_manifest_source(&package);
    assert!(manifest.contains("pub static WATER_CELLS"));
    assert!(manifest.contains("death_delay_ticks: 30"));
    assert!(manifest.contains(
        "animation: LevelMaterialAnimation::UvScroll(LevelMaterialUvMotion { enabled: true, speed_u_q8: 2048, speed_v_q8: 1203"
    ));
}

#[test]
fn water_surface_anchors_to_lowest_point_of_sloped_floor() {
    let mut project = project_with_one_room();
    let room = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("fixture room");
    let (world_x, world_z) = {
        let NodeKind::Section { grid } =
            &mut project.active_scene_mut().node_mut(room).unwrap().kind
        else {
            unreachable!()
        };
        let grid = grid.floor_mut(0).expect("base floor");
        let sector = grid.sector_mut(0, 0).expect("fixture sector");
        sector.floor.as_mut().expect("floor").heights = [256, -128, 512, 64];
        (grid.origin[0], grid.origin[1])
    };
    project.active_scene_mut().add_node(
        room,
        "Slope water",
        NodeKind::WaterVolume {
            material: None,
            cells: vec![WaterVolumeCell::new(world_x, world_z)],
            settings: WaterVolumeSettings {
                height_above_floor: 128,
                ..WaterVolumeSettings::default()
            },
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let water = package
        .expect("water project cooks")
        .water_cells
        .into_iter()
        .next()
        .expect("water cell");
    assert_eq!(water.surface_y, 0, "-128 low point + 128 water height");
    assert_eq!(water.depth, 128);
}
