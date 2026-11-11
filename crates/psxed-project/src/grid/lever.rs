//! A visible lever in one call: a wall plate and a handle (two Box Props) and
//! the Interactable Switch that drops the handle when pulled.
//!
//! The handle is an ordinary Box Prop the Switch names (`handle`) and slides
//! by `handle_slide` with the door machinery, so it follows the lever's saved
//! state exactly as a door follows its own. Sizes are for the 1024-unit-tall
//! player: a 256 by 640 plate at hand height and a 96 by 96 by 320 handle that
//! drops 160 units.

use crate::{
    DoorDirection, DoorSlide, InteractableKind, NodeId, NodeKind, ResourceId, Scene,
    SwitchBehavior, BOX_PROP_FACE_COUNT,
};

/// Plate: depth off the wall, half width, bottom and top (editor units).
const PLATE: (i16, i16, i16, i16) = (32, 128, 256, 896);
/// Handle: depth range off the wall, half width, bottom and top.
const HANDLE: (i16, i16, i16, i16, i16) = (32, 128, 48, 448, 768);
/// How far the handle drops when the lever is pulled.
const HANDLE_DROP: u16 = 160;
/// Ticks (60 Hz) the handle takes to drop.
const HANDLE_TICKS: u16 = 20;
/// How far in front of the wall the prompt spot stands.
const PROMPT_OFFSET: f32 = 160.0;

/// Where and how to place a lever.
#[derive(Debug, Clone)]
pub struct LeverSpec<'a> {
    /// Name of the lever entity; the plate and handle are `<name> plate` and
    /// `<name> handle`.
    pub name: &'a str,
    /// The wall's face, editor units, at floor height (y is the floor).
    pub wall_at: [f32; 3],
    /// Which way the lever faces: the side of the wall the player stands on
    /// (East is +X, South is +Z). Up and Down are refused.
    pub facing: DoorDirection,
    /// Node name the lever fires (usually a Door).
    pub target: &'a str,
    /// One-shot or toggle.
    pub behavior: SwitchBehavior,
    /// Keep the lever's state in the save.
    pub persistent: bool,
    /// Prompt radius, editor units.
    pub radius: u16,
    /// Material of the plate.
    pub material: Option<ResourceId>,
    /// Material of the handle (a colour against the plate reads as a lever).
    pub handle_material: Option<ResourceId>,
}

/// Corners of the box spanning `min` to `max`, bottom ring first.
fn box_vertices(min: [i16; 3], max: [i16; 3]) -> [[i16; 3]; 8] {
    let [x0, y0, z0] = min;
    let [x1, y1, z1] = max;
    [
        [x0, y0, z0],
        [x1, y0, z0],
        [x1, y0, z1],
        [x0, y0, z1],
        [x0, y1, z0],
        [x1, y1, z0],
        [x1, y1, z1],
        [x0, y1, z1],
    ]
}

/// The box `depth` off the wall (range), `half` wide, `y` tall, for `facing`.
fn wall_box(
    facing: DoorDirection,
    depth: (i16, i16),
    half: i16,
    y: (i16, i16),
) -> Option<[[i16; 3]; 8]> {
    let (d0, d1) = depth;
    let (min, max) = match facing {
        DoorDirection::East => ([d0, y.0, -half], [d1, y.1, half]),
        DoorDirection::West => ([-d1, y.0, -half], [-d0, y.1, half]),
        DoorDirection::South => ([-half, y.0, d0], [half, y.1, d1]),
        DoorDirection::North => ([-half, y.0, -d1], [half, y.1, -d0]),
        DoorDirection::Up | DoorDirection::Down => return None,
    };
    Some(box_vertices(min, max))
}

/// Place the lever: a plate and a handle Box Prop at the wall, and the entity
/// with its Switch. Returns the lever entity's node, or `None` when `facing`
/// is not a horizontal direction.
pub fn place_lever(scene: &mut Scene, spec: &LeverSpec<'_>) -> Option<NodeId> {
    let plate_vertices = wall_box(spec.facing, (0, PLATE.0), PLATE.1, (PLATE.2, PLATE.3))?;
    let handle_vertices = wall_box(
        spec.facing,
        (HANDLE.0, HANDLE.1),
        HANDLE.2,
        (HANDLE.3, HANDLE.4),
    )?;
    let mut prop = |name: String, vertices: [[i16; 3]; 8], material: Option<ResourceId>| {
        let id = scene.add_node(
            NodeId::ROOT,
            name.clone(),
            NodeKind::BoxProp {
                materials: [material; BOX_PROP_FACE_COUNT],
                uvs: crate::default_box_prop_uvs(),
                vertices,
                collision_enabled: false,
                break_flags: 0,
                erosion: Default::default(),
            },
        );
        if let Some(node) = scene.node_mut(id) {
            node.transform.translation = spec.wall_at;
        }
        name
    };
    prop(
        format!("{} plate", spec.name),
        plate_vertices,
        spec.material,
    );
    let handle = prop(
        format!("{} handle", spec.name),
        handle_vertices,
        spec.handle_material,
    );

    let entity = scene.add_node(NodeId::ROOT, spec.name, NodeKind::Entity);
    let (nx, nz) = match spec.facing {
        DoorDirection::East => (1.0, 0.0),
        DoorDirection::West => (-1.0, 0.0),
        DoorDirection::South => (0.0, 1.0),
        _ => (0.0, -1.0),
    };
    if let Some(node) = scene.node_mut(entity) {
        node.transform.translation = [
            spec.wall_at[0] + nx * PROMPT_OFFSET,
            spec.wall_at[1],
            spec.wall_at[2] + nz * PROMPT_OFFSET,
        ];
    }
    scene.add_node(
        entity,
        "Interactable",
        NodeKind::Interactable {
            kind: InteractableKind::Switch {
                target: spec.target.to_string(),
                behavior: spec.behavior,
                unlocks: false,
                persistent: spec.persistent,
                handle: Some(handle),
                handle_slide: Some(DoorSlide {
                    direction: DoorDirection::Down,
                    distance: HANDLE_DROP,
                    ticks: HANDLE_TICKS,
                }),
            },
            prompt: "PULL LEVER".to_string(),
            radius: spec.radius,
            enabled: true,
        },
    );
    Some(entity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectDocument;

    fn spec(facing: DoorDirection) -> LeverSpec<'static> {
        LeverSpec {
            name: "lever",
            wall_at: [1000.0, 0.0, 500.0],
            facing,
            target: "gate",
            behavior: SwitchBehavior::OneShot,
            persistent: true,
            radius: 800,
            material: None,
            handle_material: None,
        }
    }

    #[test]
    fn a_lever_is_a_plate_a_handle_and_a_switch_naming_the_handle() {
        let mut project = ProjectDocument::new("lever");
        let scene = project.active_scene_mut();
        let before = scene.nodes().len();
        let entity = place_lever(scene, &spec(DoorDirection::West)).expect("placed");
        assert_eq!(
            scene.nodes().len(),
            before + 4,
            "plate, handle, entity, switch"
        );
        let handle = scene
            .nodes()
            .iter()
            .find(|n| n.name == "lever handle")
            .expect("a handle");
        let NodeKind::BoxProp { vertices, .. } = &handle.kind else {
            panic!("a box prop");
        };
        // 96 wide, 96 deep, 320 tall, at hand height, on the west face.
        assert_eq!(vertices[0], [-128, 448, -48]);
        assert_eq!(vertices[6], [-32, 768, 48]);
        let prompt = scene.node(entity).expect("entity").transform.translation;
        assert_eq!(prompt, [840.0, 0.0, 500.0], "in front of the wall");
    }

    #[test]
    fn up_and_down_are_not_wall_faces() {
        let mut project = ProjectDocument::new("lever");
        let scene = project.active_scene_mut();
        assert!(place_lever(scene, &spec(DoorDirection::Up)).is_none());
        assert!(scene.nodes().len() == 1, "nothing was placed");
    }
}
