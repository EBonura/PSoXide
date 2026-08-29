//! Generate Cortex Ignition Tech Demo 0.2: Null Choir art plus combat actors.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use psxed_project::{
    CharacterControllerSettings, MaterialResource, NodeId, NodeKind, ProjectDocument, Resource,
    ResourceData, ResourceId, SkyMode, SkyVisibility, Transform3,
};
use psxed_tex::{convert, Config, CropMode, PsxtDepth, Resampler};

const PROJECT_NAME: &str = "Cortex Ignition Tech Demo 0.2";
const MATERIAL_SOURCE_PROJECT: &str = "null-choir";
const CHARACTER_SOURCE_PROJECT: &str = "tech-demo";
const DESTINATION_PROJECT: &str = "cortex-ignition-tech-demo-0.2";
const SKY_SOURCE_RELATIVE: &str = "source_assets/sky/null_choir_eclipse_equirect_v1.png";
const SKY_TEXTURE_RELATIVE: &str = "assets/textures/sky/null_choir_eclipse_cube_4bpp.psxt";

const MATERIALS: [(&str, &str, [u8; 3]); 20] = [
    (
        "Choir Bulkhead",
        "null_choir_bulkhead_v3",
        [0x98, 0xa0, 0xa8],
    ),
    (
        "Choir Bulkhead (V2 Draft)",
        "null_choir_bulkhead_v2",
        [0x98, 0xa0, 0xa8],
    ),
    (
        "Choir Bulkhead (V1 Draft)",
        "null_choir_bulkhead",
        [0x98, 0xa0, 0xa8],
    ),
    (
        "Choir Deck Grating",
        "null_choir_deck_v3",
        [0x98, 0x90, 0x88],
    ),
    (
        "Choir Deck Grating (V2 Draft)",
        "null_choir_deck_v2",
        [0x98, 0x90, 0x88],
    ),
    (
        "Choir Deck Grating (V1 Draft)",
        "null_choir_deck",
        [0x98, 0x90, 0x88],
    ),
    (
        "Choir Emergency Rib",
        "null_choir_rib_v3",
        [0xa0, 0x88, 0x88],
    ),
    (
        "Choir Emergency Rib (V2 Draft)",
        "null_choir_rib_v2",
        [0xa0, 0x88, 0x88],
    ),
    (
        "Choir Emergency Rib (V1 Draft)",
        "null_choir_rib",
        [0xa0, 0x88, 0x88],
    ),
    (
        "Choir Signal Core",
        "null_choir_core_v3",
        [0x88, 0xa8, 0xb8],
    ),
    (
        "Choir Signal Core (V2 Draft)",
        "null_choir_core_v2",
        [0x88, 0xa8, 0xb8],
    ),
    (
        "Choir Signal Core (V1 Draft)",
        "null_choir_core",
        [0x88, 0xa8, 0xb8],
    ),
    (
        "Choir Wall Plinth",
        "null_choir_wall_base_v3",
        [0x90, 0x90, 0x90],
    ),
    (
        "Choir Layered Beam",
        "null_choir_beam_face_v3",
        [0x88, 0x84, 0x84],
    ),
    (
        "Choir Beam Knee",
        "null_choir_beam_joint_v3",
        [0x90, 0x80, 0x7c],
    ),
    (
        "Choir Deck Edge",
        "null_choir_deck_edge_v3",
        [0x88, 0x90, 0x94],
    ),
    (
        "Choir Ceiling Vent",
        "null_choir_ceiling_vent_v3",
        [0x78, 0x78, 0x78],
    ),
    (
        "Choir Trench Liner",
        "null_choir_trench_liner_v3",
        [0x70, 0x90, 0x98],
    ),
    (
        "Choir Hazard Inset",
        "null_choir_hazard_inset_v3",
        [0xa0, 0x78, 0x74],
    ),
    (
        "Choir Service Panel",
        "null_choir_service_panel_v3",
        [0x80, 0x88, 0x8c],
    ),
];

fn main() {
    let projects_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("projects");
    let material_source = projects_root.join(MATERIAL_SOURCE_PROJECT);
    let character_source = projects_root.join(CHARACTER_SOURCE_PROJECT);
    let destination = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| projects_root.join(DESTINATION_PROJECT));

    generate(&material_source, &character_source, &destination);
}

fn generate(material_source: &Path, character_source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination.join("source_assets/textures"))
        .expect("create texture-kit source directory");
    std::fs::create_dir_all(destination.join("assets/textures"))
        .expect("create texture-kit cooked directory");

    let mut project = ProjectDocument::new(PROJECT_NAME);
    project.editor_camera.orbit_target = [0, 512, 1152];
    project.editor_camera.orbit_radius = 6000;

    for (name, stem, tint) in MATERIALS {
        copy(
            &material_source.join(format!("source_assets/textures/{stem}.png")),
            &destination.join(format!("source_assets/textures/{stem}.png")),
        );
        let relative_cooked = format!("assets/textures/{stem}.psxt");
        cook_64_square(
            &material_source.join(format!("source_assets/textures/{stem}.png")),
            &destination.join(&relative_cooked),
        );

        let mut material = MaterialResource::opaque(Some(relative_cooked));
        material.tint = tint;
        project.add_resource(name, ResourceData::Material(material));
    }

    copy(
        &material_source.join(SKY_SOURCE_RELATIVE),
        &destination.join(SKY_SOURCE_RELATIVE),
    );
    let sky_source = std::fs::read(material_source.join(SKY_SOURCE_RELATIVE))
        .expect("read Null Choir equirectangular sky");
    let cooked_sky = psxed_project::sky_texture::cook_equirectangular_cube_sky(&sky_source)
        .expect("cook Null Choir cube sky");
    let sky_destination = destination.join(SKY_TEXTURE_RELATIVE);
    std::fs::create_dir_all(sky_destination.parent().expect("cube-sky texture parent"))
        .expect("create cube-sky texture directory");
    std::fs::write(&sky_destination, cooked_sky).expect("write Null Choir cube sky");

    let mut sky_material = MaterialResource::opaque(Some(SKY_TEXTURE_RELATIVE.to_string()));
    sky_material.sky_aperture = true;
    let sky_id = project.add_resource(
        "Null Choir Eclipse Cube Sky",
        ResourceData::Material(sky_material),
    );
    let NodeKind::World { sky, .. } = &mut project
        .active_scene_mut()
        .node_mut(NodeId::ROOT)
        .expect("texture-kit world root")
        .kind
    else {
        panic!("texture-kit scene root must be a World");
    };
    sky.mode = SkyMode::Cube;
    sky.visibility = SkyVisibility::ThroughSkySurfaces;
    sky.texture = Some(sky_id);

    let characters = import_cortex_characters(character_source, destination, &mut project);
    place_character_lineup(&mut project, characters);

    copy(
        &material_source.join("TEXTURE_SET_V3.md"),
        &destination.join("TEXTURE_SET_V3.md"),
    );

    let project_path = destination.join("project.ron");
    project
        .save_to_path(&project_path)
        .expect("save Cortex Ignition Tech Demo 0.2 project");

    println!(
        "generated {} ({} resources, {} actors, {} brushes)",
        project_path.display(),
        project.resources.len(),
        project
            .active_scene()
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::CharacterController { .. }))
            .count(),
        project.active_scene().brushes.len()
    );
}

#[derive(Clone, Copy)]
struct ImportedCharacters {
    player: ResourceId,
    enemy: ResourceId,
    player_weapon: ResourceId,
    enemy_weapon: ResourceId,
}

fn import_cortex_characters(
    source_dir: &Path,
    destination: &Path,
    project: &mut ProjectDocument,
) -> ImportedCharacters {
    let source = ProjectDocument::load_from_path(source_dir.join("project.ron"))
        .expect("load canonical Cortex Ignition character project");
    let player = named_resource(&source, "Aletha", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let enemy = named_resource(&source, "Light Enemy", |data| {
        matches!(data, ResourceData::Character(_))
    });
    let player_weapon = named_resource(&source, "Sword1 Light", |data| {
        matches!(data, ResourceData::Weapon(_))
    });
    let enemy_weapon = named_resource(&source, "Sword1 Heavy", |data| {
        matches!(data, ResourceData::Weapon(_))
    });

    let selected =
        resource_dependency_closure(&source, &[player, enemy, player_weapon, enemy_weapon]);
    copy_runtime_assets(source_dir, destination, &selected);

    let mut remap = HashMap::new();
    for resource in &selected {
        let new_id = project.add_resource(resource.name.clone(), resource.data.clone());
        remap.insert(resource.id.raw(), new_id);
    }
    for resource in &selected {
        let new_id = mapped_resource(&remap, resource.id);
        let remapped = remap_resource_data(resource.data.clone(), &remap);
        project
            .resource_mut(new_id)
            .expect("newly imported character dependency")
            .data = remapped;
    }

    ImportedCharacters {
        player: mapped_resource(&remap, player),
        enemy: mapped_resource(&remap, enemy),
        player_weapon: mapped_resource(&remap, player_weapon),
        enemy_weapon: mapped_resource(&remap, enemy_weapon),
    }
}

fn named_resource(
    project: &ProjectDocument,
    name: &str,
    matches_kind: impl Fn(&ResourceData) -> bool,
) -> ResourceId {
    project
        .resources
        .iter()
        .find(|resource| resource.name == name && matches_kind(&resource.data))
        .unwrap_or_else(|| panic!("canonical resource {name:?} not found"))
        .id
}

fn resource_dependency_closure(project: &ProjectDocument, roots: &[ResourceId]) -> Vec<Resource> {
    let mut pending = roots.to_vec();
    let mut selected = HashSet::new();
    while let Some(id) = pending.pop() {
        if !selected.insert(id.raw()) {
            continue;
        }
        let resource = project
            .resource(id)
            .unwrap_or_else(|| panic!("missing dependency resource #{}", id.raw()));
        pending.extend(resource_dependencies(&resource.data));
    }
    project
        .resources
        .iter()
        .filter(|resource| selected.contains(&resource.id.raw()))
        .cloned()
        .collect()
}

fn resource_dependencies(data: &ResourceData) -> Vec<ResourceId> {
    let mut ids = Vec::new();
    let mut option = |id: Option<ResourceId>| ids.extend(id);
    match data {
        ResourceData::Model(model) => option(model.skeleton),
        ResourceData::AnimationSource(source) => {
            option(source.skeleton);
            option(source.target_model);
        }
        ResourceData::AnimationClip(clip) => {
            option(clip.skeleton);
            option(clip.target_model);
            option(clip.source);
        }
        ResourceData::AnimationSet(set) => {
            for id in [
                set.skeleton,
                set.idle_clip,
                set.walk_clip,
                set.run_clip,
                set.turn_clip,
                set.roll_clip,
                set.backstep_clip,
            ] {
                option(id);
            }
            ids.extend(set.action_clips.iter().map(|binding| binding.clip));
            ids.extend(
                set.weapon_appearance_tracks
                    .iter()
                    .map(|track| track.weapon),
            );
            ids.extend(set.clips.iter().copied());
        }
        ResourceData::Character(character) => {
            option(character.model);
            option(character.material);
            option(character.animation_set);
            ids.extend(
                character
                    .combat_capsules
                    .iter()
                    .filter_map(|volume| match volume.role {
                        psxed_project::CombatCapsuleRole::ProjectileEmitter {
                            projectile, ..
                        } => projectile,
                        _ => None,
                    }),
            );
        }
        ResourceData::Weapon(weapon) => option(weapon.model),
        ResourceData::Texture { .. }
        | ResourceData::Material(_)
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::Projectile(_)
        | ResourceData::BoostModule(_) => {}
    }
    ids
}

fn remap_resource_data(mut data: ResourceData, remap: &HashMap<u64, ResourceId>) -> ResourceData {
    let map_option = |id: &mut Option<ResourceId>| {
        if let Some(old) = *id {
            *id = Some(mapped_resource(remap, old));
        }
    };
    match &mut data {
        ResourceData::Model(model) => {
            map_option(&mut model.skeleton);
            model.source_path = None;
        }
        ResourceData::AnimationSource(source) => {
            map_option(&mut source.skeleton);
            map_option(&mut source.target_model);
        }
        ResourceData::AnimationClip(clip) => {
            map_option(&mut clip.skeleton);
            map_option(&mut clip.target_model);
            map_option(&mut clip.source);
        }
        ResourceData::AnimationSet(set) => {
            for id in [
                &mut set.skeleton,
                &mut set.idle_clip,
                &mut set.walk_clip,
                &mut set.run_clip,
                &mut set.turn_clip,
                &mut set.roll_clip,
                &mut set.backstep_clip,
            ] {
                map_option(id);
            }
            for binding in &mut set.action_clips {
                binding.clip = mapped_resource(remap, binding.clip);
            }
            for track in &mut set.weapon_appearance_tracks {
                track.weapon = mapped_resource(remap, track.weapon);
            }
            for clip in &mut set.clips {
                *clip = mapped_resource(remap, *clip);
            }
        }
        ResourceData::Character(character) => {
            map_option(&mut character.model);
            map_option(&mut character.material);
            map_option(&mut character.animation_set);
            for volume in &mut character.combat_capsules {
                if let psxed_project::CombatCapsuleRole::ProjectileEmitter { projectile, .. } =
                    &mut volume.role
                {
                    map_option(projectile);
                }
            }
        }
        ResourceData::Weapon(weapon) => map_option(&mut weapon.model),
        ResourceData::Texture { .. }
        | ResourceData::Material(_)
        | ResourceData::Skeleton(_)
        | ResourceData::Mesh { .. }
        | ResourceData::Scene { .. }
        | ResourceData::Script { .. }
        | ResourceData::Audio { .. }
        | ResourceData::Projectile(_)
        | ResourceData::BoostModule(_) => {}
    }
    data
}

fn mapped_resource(remap: &HashMap<u64, ResourceId>, old: ResourceId) -> ResourceId {
    *remap
        .get(&old.raw())
        .unwrap_or_else(|| panic!("resource #{} escaped dependency closure", old.raw()))
}

fn copy_runtime_assets(source: &Path, destination: &Path, resources: &[Resource]) {
    for resource in resources {
        match &resource.data {
            ResourceData::Model(model) => {
                copy(
                    &source.join(&model.model_path),
                    &destination.join(&model.model_path),
                );
                if let Some(texture) = &model.texture_path {
                    copy(&source.join(texture), &destination.join(texture));
                }
            }
            ResourceData::AnimationClip(clip) => copy(
                &source.join(&clip.psxanim_path),
                &destination.join(&clip.psxanim_path),
            ),
            ResourceData::Material(material) => {
                if let Some(texture) = &material.psxt_path {
                    copy(&source.join(texture), &destination.join(texture));
                }
            }
            _ => {}
        }
    }
}

fn place_character_lineup(project: &mut ProjectDocument, imported: ImportedCharacters) {
    place_character(
        project,
        imported.player,
        imported.player_weapon,
        "Aletha (Player)",
        [0.0, 1.0, 0.0],
        0.0,
        360,
        Some(10),
        true,
    );
    place_character(
        project,
        imported.enemy,
        imported.enemy_weapon,
        "Intake Custodian",
        [-1792.0, 1.0, 2304.0],
        180.0,
        448,
        Some(0),
        false,
    );
    place_character(
        project,
        imported.enemy,
        imported.enemy_weapon,
        "Gallery Custodian",
        [1792.0, 1.0, 2304.0],
        180.0,
        448,
        Some(0),
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn place_character(
    project: &mut ProjectDocument,
    character_id: ResourceId,
    weapon_id: ResourceId,
    name: &str,
    translation: [f32; 3],
    yaw_degrees: f32,
    visual_scale_q8: u16,
    idle_clip: Option<u16>,
    player: bool,
) {
    let character = project
        .resource(character_id)
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => Some(character.clone()),
            _ => None,
        })
        .expect("imported Character profile");
    let controller = CharacterControllerSettings::from_character(&character);
    let camera = character.camera_settings();

    let scene = project.active_scene_mut();
    let entity = scene.add_node(NodeId::ROOT, name, NodeKind::Entity);
    scene
        .node_mut(entity)
        .expect("new character entity")
        .transform = Transform3 {
        translation,
        rotation_degrees: [0.0, yaw_degrees, 0.0],
        ..Transform3::default()
    };
    scene.add_node(
        entity,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: character.model,
            material: character.material,
            visual_offset: [0, 1, 0],
            visual_scale_q8,
        },
    );
    scene.add_node(
        entity,
        "Animator",
        NodeKind::Animator {
            clip: idle_clip,
            action_clips: Vec::new(),
            autoplay: true,
            pose_frame: 0,
        },
    );
    scene.add_node(
        entity,
        "Character Controller",
        NodeKind::CharacterController {
            character: Some(character_id),
            settings: Some(controller),
            player,
        },
    );
    if player {
        scene.add_node(entity, "Camera", NodeKind::Camera { settings: camera });
    }
    scene.add_node(
        entity,
        "Equipment",
        NodeKind::Equipment {
            weapon: Some(weapon_id),
            character_socket: "right_hand_grip".to_string(),
            weapon_grip: "grip".to_string(),
        },
    );
}

fn copy(source: &Path, destination: &Path) {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).expect("create copied texture parent");
    }
    std::fs::copy(source, destination).unwrap_or_else(|error| {
        panic!(
            "copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    });
}

fn cook_64_square(source: &Path, destination: &Path) {
    let source_bytes =
        std::fs::read(source).unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
    let cooked = convert(
        &source_bytes,
        &Config {
            width: 64,
            height: 64,
            depth: PsxtDepth::Bit4,
            crop: CropMode::CentreSquare,
            resampler: Resampler::Lanczos3,
            transparent_index_zero: false,
            clut_rows: 1,
        },
    )
    .unwrap_or_else(|error| panic!("cook {}: {error}", source.display()));
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).expect("create cooked texture parent");
    }
    std::fs::write(destination, cooked)
        .unwrap_or_else(|error| panic!("write {}: {error}", destination.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_tech_demo_has_materials_cube_sky_player_and_two_enemies() {
        let project_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("projects")
            .join(DESTINATION_PROJECT);
        let project = ProjectDocument::load_from_path(project_dir.join("project.ron"))
            .expect("load tracked Cortex Ignition Tech Demo 0.2");

        assert_eq!(project.name, PROJECT_NAME);
        assert!(project.resources.len() >= 65);
        assert!(project.active_scene().nodes().len() >= 17);

        let controllers = project
            .active_scene()
            .nodes()
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::CharacterController {
                    character,
                    settings,
                    player,
                } => Some((*character, *settings, *player)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(controllers.len(), 3);
        assert_eq!(controllers.iter().filter(|row| row.2).count(), 1);
        assert_eq!(controllers.iter().filter(|row| !row.2).count(), 2);
        assert!(controllers
            .iter()
            .filter(|row| !row.2)
            .all(|row| row.1.is_some_and(|settings| settings.enemy.is_some())));
        for (character, _, _) in controllers {
            assert!(matches!(
                character.and_then(|id| project.resource(id).map(|resource| &resource.data)),
                Some(ResourceData::Character(_))
            ));
        }

        for (_, stem, _) in MATERIALS {
            let source = project_dir.join(format!("source_assets/textures/{stem}.png"));
            let source_dimensions =
                image::image_dimensions(&source).expect("read source PNG dimensions");
            assert!(
                source_dimensions.0 > 0 && source_dimensions.1 > 0,
                "{} dimensions",
                source.display()
            );

            let cooked = project_dir.join(format!("assets/textures/{stem}.psxt"));
            let bytes = std::fs::read(&cooked).expect("read cooked PSXT");
            assert_eq!(&bytes[..4], b"PSXT", "{} magic", cooked.display());
            assert_eq!(
                [
                    u16::from_le_bytes([bytes[14], bytes[15]]),
                    u16::from_le_bytes([bytes[16], bytes[17]]),
                ],
                [64, 64],
                "{} dimensions",
                cooked.display()
            );
        }

        let sky = project_dir.join(SKY_TEXTURE_RELATIVE);
        let bytes = std::fs::read(&sky).expect("read cooked cube-sky PSXT");
        let texture = psx_asset::Texture::from_bytes(&bytes).expect("parse cooked cube-sky PSXT");
        assert_eq!(
            [texture.width(), texture.height()],
            psx_bsp::sky::CUBE_SKY_ATLAS_SIZE
        );
        assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit4);
        assert_eq!(texture.clut_entries(), psx_bsp::sky::CUBE_SKY_CLUT_ENTRIES);

        for relative in [
            "assets/models/aletha_delivered/aletha_delivered.psxmdl",
            "assets/models/rust_mantis/rust_mantis.psxmdl",
            "assets/animations/aletha_delivered/aletha_idle.psxanim",
            "assets/animations/rust_mantis_starter/idle.psxanim",
        ] {
            assert!(project_dir.join(relative).is_file(), "missing {relative}");
        }
    }
}
