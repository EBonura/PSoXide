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
    pub audio: Vec<ResourceId>,
    pub model_instances: usize,
    pub character_controllers: usize,
    pub colliders: usize,
    pub interactables: usize,
    pub image_props: usize,
    pub lights: usize,
    pub triggers: usize,
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
    let mut audio = HashSet::new();

    for node in scene.nodes() {
        if let Some(room_id) = room_filter {
            if !scene.is_descendant_of(node.id, room_id) {
                continue;
            }
        }

        match &node.kind {
            NodeKind::Room { grid } => {
                collect_grid_resources(grid, &mut use_set, &mut materials);
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
                );
            }
            NodeKind::SpawnPoint { character, .. } => {
                push_character_model(
                    project,
                    *character,
                    &mut use_set,
                    &mut characters,
                    &mut models,
                );
            }
            NodeKind::AudioSource { sound, .. } => {
                if let Some(audio_id) = sound {
                    push_unique(*audio_id, &mut use_set.audio, &mut audio);
                }
            }
            NodeKind::Collider { .. } => use_set.colliders += 1,
            NodeKind::Interactable { .. } => use_set.interactables += 1,
            NodeKind::PointLight { .. } => use_set.lights += 1,
            NodeKind::Trigger { .. } => use_set.triggers += 1,
            NodeKind::Portal { .. } => use_set.portals += 1,
            _ => {}
        }
    }

    for material_id in use_set.materials.clone() {
        let Some(resource) = project.resource(material_id) else {
            continue;
        };
        if let ResourceData::Material(material) = &resource.data {
            if let Some(texture_id) = material.texture {
                push_unique(texture_id, &mut use_set.textures, &mut textures);
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
    for sector in grid.sectors.iter().flatten() {
        if let Some(face) = &sector.floor {
            push_material(face.material, use_set, materials);
        }
        if let Some(face) = &sector.ceiling {
            push_material(face.material, use_set, materials);
        }
        for direction in GridDirection::ALL {
            for wall in sector.walls.get(direction) {
                push_material(wall.material, use_set, materials);
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
}

fn push_unique(id: ResourceId, out: &mut Vec<ResourceId>, seen: &mut HashSet<ResourceId>) {
    if seen.insert(id) {
        out.push(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CharacterResource, MaterialResource, NodeKind, ResourceData};

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
        let texture = project.add_resource(
            "atlas",
            ResourceData::Texture {
                psxt_path: "atlas.psxt".to_string(),
            },
        );
        let material = project.add_resource(
            "mat",
            ResourceData::Material(MaterialResource::opaque(Some(texture))),
        );
        let model = project.add_resource(
            "model",
            ResourceData::Model(crate::ModelResource {
                model_path: "model.psxmdl".to_string(),
                source_path: None,
                texture_path: None,
                skeleton: None,
                clips: Vec::new(),
                default_clip: None,
                preview_clip: None,
                world_height: 1024,
                collision_radius: crate::default_model_collision_radius_for_height(1024),
                scale_q8: [crate::MODEL_SCALE_ONE_Q8; 3],
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
            NodeKind::Room {
                grid: WorldGrid::stone_room(2, 2, 1024, Some(material), None),
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
        assert_eq!(use_set.textures, vec![texture]);
        assert_eq!(use_set.models, vec![model]);
        assert_eq!(use_set.characters, vec![character]);
        assert_eq!(use_set.model_instances, 1);
        assert_eq!(use_set.character_controllers, 1);
    }
}
