//! Editor-side resource-use collection for runtime-facing scene assets.

use std::collections::HashSet;

use crate::{
    GridDirection, NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, WorldGrid,
};

/// Referenced runtime-facing resources for a scene or room.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SceneResourceUse {
    pub materials: Vec<ResourceId>,
    pub textures: Vec<ResourceId>,
    pub models: Vec<ResourceId>,
    pub meshes: Vec<ResourceId>,
    pub characters: Vec<ResourceId>,
    pub model_instances: usize,
    pub character_controllers: usize,
    pub colliders: usize,
    pub image_props: usize,
    pub lights: usize,
    pub particle_emitters: usize,
    pub portals: usize,
}

/// Collect resources used by the active scene.
pub fn collect_scene_resource_use(project: &ProjectDocument) -> SceneResourceUse {
    collect_resource_use(project, None)
}

/// Collect resources used by one Room and its descendants.
pub fn collect_room_resource_use(project: &ProjectDocument, room_id: NodeId) -> SceneResourceUse {
    collect_resource_use(project, Some(room_id))
}

fn collect_resource_use(
    project: &ProjectDocument,
    room_filter: Option<NodeId>,
) -> SceneResourceUse {
    let scene = project.active_scene();
    let mut use_set = SceneResourceUse::default();
    let mut materials = HashSet::new();
    let mut textures = HashSet::new();
    let mut models = HashSet::new();
    let mut meshes = HashSet::new();
    let mut characters = HashSet::new();

    for node in scene.nodes() {
        if let Some(room_id) = room_filter {
            if !scene.is_descendant_of(node.id, room_id) {
                continue;
            }
        }

        match &node.kind {
            NodeKind::Section { grid } => {
                collect_grid_resources(grid, &mut use_set, &mut materials);
            }
            NodeKind::WaterVolume { material, .. } => {
                push_material(*material, &mut use_set, &mut materials);
            }
            NodeKind::MeshInstance { mesh, material, .. } => {
                push_material(*material, &mut use_set, &mut materials);
                if let Some(mesh_id) = mesh {
                    use_set.model_instances += 1;
                    match project.resource(*mesh_id).map(|resource| &resource.data) {
                        Some(ResourceData::Model(_)) => {
                            push_unique(*mesh_id, &mut use_set.models, &mut models)
                        }
                        Some(ResourceData::Mesh { .. }) => {
                            push_unique(*mesh_id, &mut use_set.meshes, &mut meshes)
                        }
                        _ => {}
                    }
                }
            }
            NodeKind::ModelRenderer {
                model, material, ..
            } => {
                push_material(*material, &mut use_set, &mut materials);
                if let Some(model_id) = model {
                    use_set.model_instances += 1;
                    push_unique(*model_id, &mut use_set.models, &mut models);
                }
            }
            NodeKind::ImageProp { material, .. } => {
                use_set.image_props += 1;
                push_material(*material, &mut use_set, &mut materials);
            }
            NodeKind::CharacterController { character, .. } => {
                use_set.character_controllers += 1;
                push_character_model(
                    project,
                    *character,
                    &mut use_set,
                    &mut characters,
                    &mut models,
                    &mut materials,
                );
            }
            NodeKind::SpawnPoint { character, .. } => {
                push_character_model(
                    project,
                    *character,
                    &mut use_set,
                    &mut characters,
                    &mut models,
                    &mut materials,
                );
            }
            NodeKind::Collider { .. } => use_set.colliders += 1,
            NodeKind::PointLight { .. } => use_set.lights += 1,
            NodeKind::ParticleEmitter { settings } => {
                use_set.particle_emitters += 1;
                if let Some(texture_id) = settings.texture {
                    push_unique(texture_id, &mut use_set.textures, &mut textures);
                }
            }
            NodeKind::Portal { .. } => use_set.portals += 1,
            _ => {}
        }
    }

    // Materials own their texture image now; the texture-carrying
    // resources for residency purposes are the textured materials
    // themselves (plus the direct image references collected above).
    for material_id in use_set.materials.clone() {
        let Some(resource) = project.resource(material_id) else {
            continue;
        };
        if let ResourceData::Material(material) = &resource.data {
            if material.psxt_path.is_some()
                || material.texture_mode == crate::MaterialTextureMode::Generated
                || material.texture_mode == crate::MaterialTextureMode::Transition
            {
                push_unique(material_id, &mut use_set.textures, &mut textures);
            }
        }
    }

    use_set
}

fn collect_grid_resources(
    grid: &WorldGrid,
    use_set: &mut SceneResourceUse,
    materials: &mut HashSet<ResourceId>,
) {
    // A Room owns every authored floor through its base grid. Resource
    // residency therefore has to walk the complete stack, not just floor
    // zero; otherwise upper-floor materials lose their native preview slots
    // even though the runtime cooker (which walks every floor) renders them.
    for floor_index in 0..grid.floor_count() {
        let Some(floor_grid) = grid.floor(floor_index) else {
            continue;
        };
        for sector in floor_grid.sectors.iter().flatten() {
            if let Some(face) = &sector.floor {
                push_material(face.triangle_material(0), use_set, materials);
                push_material(face.triangle_material(1), use_set, materials);
            }
            if let Some(face) = &sector.ceiling {
                push_material(face.triangle_material(0), use_set, materials);
                push_material(face.triangle_material(1), use_set, materials);
            }
            for direction in GridDirection::ALL {
                for wall in sector.walls.get(direction) {
                    push_material(wall.material, use_set, materials);
                }
            }
        }
    }
}

fn push_material(
    id: Option<ResourceId>,
    use_set: &mut SceneResourceUse,
    materials: &mut HashSet<ResourceId>,
) {
    if let Some(id) = id {
        push_unique(id, &mut use_set.materials, materials);
    }
}

fn push_character_model(
    project: &ProjectDocument,
    character: Option<ResourceId>,
    use_set: &mut SceneResourceUse,
    characters: &mut HashSet<ResourceId>,
    models: &mut HashSet<ResourceId>,
    materials: &mut HashSet<ResourceId>,
) {
    let Some(character_id) = character else {
        return;
    };
    push_unique(character_id, &mut use_set.characters, characters);
    let Some(resource) = project.resource(character_id) else {
        return;
    };
    let ResourceData::Character(character) = &resource.data else {
        return;
    };
    if let Some(model_id) = character.model {
        push_unique(model_id, &mut use_set.models, models);
    }
    push_material(character.material, use_set, materials);
}

fn push_unique(id: ResourceId, out: &mut Vec<ResourceId>, seen: &mut HashSet<ResourceId>) {
    if seen.insert(id) {
        out.push(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CharacterResource, GridTriangleMaterialOverride, MaterialResource, NodeKind,
        ParticleEmitterSettings, ResourceData, WaterVolumeCell, WaterVolumeSettings,
    };

    #[test]
    fn budget_for_rect_counts_only_requested_area() {
        let floor = ResourceId(1);
        let mut grid = WorldGrid::empty(4, 4, 1024);
        grid.set_floor(0, 0, 0, Some(floor));
        grid.set_floor(3, 3, 0, Some(floor));

        let left = grid.budget_for_rect(0, 0, 2, 4).unwrap();
        let right = grid.budget_for_rect(2, 0, 2, 4).unwrap();

        assert_eq!(left.total_cells, 8);
        assert_eq!(right.total_cells, 8);
        assert_eq!(left.floors, 1);
        assert_eq!(right.floors, 1);
    }

    #[test]
    fn scene_resource_use_follows_components_and_material_textures() {
        let mut project = ProjectDocument::new("test");
        let material = project.add_resource(
            "mat",
            ResourceData::Material(MaterialResource::opaque(Some("atlas.psxt".to_string()))),
        );
        let particle_texture = project.add_resource(
            "particle_mask",
            ResourceData::Material(MaterialResource::opaque(Some(
                "particle_mask.psxt".to_string(),
            ))),
        );
        let model = project.add_resource(
            "model",
            ResourceData::Model(crate::ModelResource {
                model_path: "model.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: None,
                world_height: 1024,
                collision_radius: crate::default_model_collision_radius_for_height(1024),
                scale_q8: [crate::MODEL_SCALE_ONE_Q8; 3],
                default_visual_yaw_q12: 0,
                attachments: Vec::new(),
            }),
        );
        let character = project.add_resource(
            "character",
            ResourceData::Character(CharacterResource {
                model: Some(model),
                ..CharacterResource::default()
            }),
        );

        let scene = project.active_scene_mut();
        let room = scene.add_node(
            scene.root,
            "Room",
            NodeKind::Section {
                grid: WorldGrid::stone_room(2, 2, 1024, Some(material), None),
            },
        );
        scene.add_node(
            room,
            "Emitter",
            NodeKind::ParticleEmitter {
                settings: ParticleEmitterSettings {
                    texture: Some(particle_texture),
                    ..ParticleEmitterSettings::default()
                },
            },
        );
        scene.add_node(
            room,
            "Water",
            NodeKind::WaterVolume {
                material: Some(material),
                cells: vec![WaterVolumeCell::new(0, 0)],
                settings: WaterVolumeSettings::default(),
            },
        );
        let entity = scene.add_node(room, "Entity", NodeKind::Entity);
        scene.add_node(
            entity,
            "Controller",
            NodeKind::CharacterController {
                character: Some(character),
                settings: crate::CharacterControllerSettings::default(),
                player: true,
            },
        );
        scene.add_node(
            entity,
            "Renderer",
            NodeKind::ModelRenderer {
                model: Some(model),
                material: None,
                visual_offset: [0; 3],
                visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
            },
        );

        let use_set = collect_scene_resource_use(&project);

        assert_eq!(use_set.materials, vec![material]);
        assert!(use_set.textures.contains(&material));
        assert!(use_set.textures.contains(&particle_texture));
        assert_eq!(use_set.textures.len(), 2);
        assert_eq!(use_set.models, vec![model]);
        assert_eq!(use_set.characters, vec![character]);
        assert_eq!(use_set.model_instances, 1);
        assert_eq!(use_set.character_controllers, 1);
        assert_eq!(use_set.particle_emitters, 1);
    }

    #[test]
    fn scene_resource_use_includes_stacked_floor_and_triangle_materials() {
        let mut project = ProjectDocument::new("stacked-floor-resources");
        let base_material = project.add_resource(
            "base",
            ResourceData::Material(MaterialResource::opaque(Some("base.psxt".to_string()))),
        );
        let upper_material = project.add_resource(
            "upper",
            ResourceData::Material(MaterialResource::opaque(Some("upper.psxt".to_string()))),
        );
        let triangle_material = project.add_resource(
            "triangle",
            ResourceData::Material(MaterialResource::opaque(Some("triangle.psxt".to_string()))),
        );

        let mut grid = WorldGrid::empty(1, 1, 1024);
        grid.set_floor(0, 0, 0, Some(base_material));
        let upper = grid.push_floor();
        let upper_grid = grid.floor_mut(upper).expect("new upper floor");
        upper_grid.set_floor(0, 0, 0, Some(upper_material));
        upper_grid
            .sector_mut(0, 0)
            .expect("upper sector")
            .floor
            .as_mut()
            .expect("upper floor face")
            .triangle_override_mut(1)
            .material = Some(GridTriangleMaterialOverride::Resource(triangle_material));

        let scene = project.active_scene_mut();
        scene.add_node(scene.root, "Room", NodeKind::Section { grid });

        let use_set = collect_scene_resource_use(&project);
        assert_eq!(
            use_set.materials,
            vec![base_material, upper_material, triangle_material]
        );
        assert!(use_set.textures.contains(&base_material));
        assert!(use_set.textures.contains(&upper_material));
        assert!(use_set.textures.contains(&triangle_material));
    }
}
