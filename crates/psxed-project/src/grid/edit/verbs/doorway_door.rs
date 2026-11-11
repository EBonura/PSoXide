//! A doorway with its door: one call that cuts the opening and places a
//! sliding logic Door and the Box Prop slab it moves, sized to the cut, and
//! optionally a lever Interactable that opens it.
//!
//! The grid edit and the scene edit are one logical step: the caller
//! snapshots the project once. The slab stands on the door plane (the edge
//! the doorway was cut in), `width` wide, `height` tall and a quarter
//! sector thick, and slides up by its own height. Names are the caller's:
//! the Door node is `name`, its slab `name box`, the lever `name lever`.

use crate::grid::world::GridWorld;
use crate::{
    DoorDirection, DoorLock, DoorSlide, InteractableKind, LogicNodeKind, NodeId, NodeKind, Scene,
    SwitchBehavior,
};
use crate::{ResourceId, BOX_PROP_FACE_COUNT};

use super::opening::{cut_doorway, DoorPlane, OpeningParams, OpeningRefusal};
use crate::grid::overrides::Axis;

/// A lever in a cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Lever {
    /// World sector the lever stands in.
    pub cell: [i32; 2],
    /// Prompt shown beside it.
    pub prompt: String,
    /// Press radius, editor units.
    pub radius: u16,
    /// Toggle or one shot.
    pub behavior: SwitchBehavior,
}

/// Parameters of a doorway with a door.
#[derive(Clone, Debug, PartialEq)]
pub struct DoorwayDoor {
    /// The opening to cut (its kind is forced to a door).
    pub opening: OpeningParams,
    /// Name of the Door node.
    pub name: String,
    /// Material of the slab.
    pub material: Option<ResourceId>,
    /// Which way the slab slides.
    pub direction: DoorDirection,
    /// Sliding time in 60 Hz ticks.
    pub ticks: u16,
    /// Lock rule.
    pub lock: DoorLock,
    /// Message shown when the door refuses.
    pub refusal: String,
    /// Survives death and saves.
    pub persistent: bool,
    /// A lever that opens it.
    pub lever: Option<Lever>,
}

/// What was made.
#[derive(Clone, Debug, PartialEq)]
pub struct DoorwayDoorOutcome {
    /// The doorway plane.
    pub plane: DoorPlane,
    /// The Door logic node.
    pub door: NodeId,
    /// The slab Box Prop.
    pub slab: NodeId,
    /// The lever entity, if one was asked for.
    pub lever: Option<NodeId>,
}

/// Cut the doorway and place the door (and lever). On a refusal of the cut
/// nothing is changed in either the grid or the scene.
pub fn doorway_with_door(
    world: &mut GridWorld,
    scene: &mut Scene,
    p: &DoorwayDoor,
) -> Result<DoorwayDoorOutcome, OpeningRefusal> {
    let plane = cut_doorway(world, &p.opening)?;
    let scale = world.scale;
    let ed = |gu: i32| scale.to_editor(gu);
    let (clear_w, clear_h) = (ed(plane.width), ed(plane.height));
    let half_w = (clear_w / 2).clamp(1, i32::from(i16::MAX)) as i16;
    let half_t = (ed(256) / 2).clamp(1, i32::from(i16::MAX)) as i16;
    let h = clear_h.clamp(1, i32::from(i16::MAX)) as i16;
    // Along the wall `half_w`, across it `half_t`; bottom ring first.
    let (tx, tz) = match plane.axis {
        Axis::X => (half_w, half_t),
        _ => (half_t, half_w),
    };
    let vertices = [
        [-tx, 0, -tz],
        [tx, 0, -tz],
        [tx, 0, tz],
        [-tx, 0, tz],
        [-tx, h, -tz],
        [tx, h, -tz],
        [tx, h, tz],
        [-tx, h, tz],
    ];
    let at = [
        ed(plane.center[0]) as f32,
        ed(plane.center[1]) as f32,
        ed(plane.center[2]) as f32,
    ];
    let slab_name = format!("{} box", p.name);
    let slab = scene.add_node(
        NodeId::ROOT,
        &slab_name,
        NodeKind::BoxProp {
            materials: [p.material; BOX_PROP_FACE_COUNT],
            uvs: crate::default_box_prop_uvs(),
            vertices,
            collision_enabled: true,
            break_flags: 0,
            erosion: Default::default(),
        },
    );
    if let Some(n) = scene.node_mut(slab) {
        n.transform.translation = at;
    }
    let distance = if matches!(p.direction, DoorDirection::Up | DoorDirection::Down) {
        h as u16
    } else {
        clear_w.clamp(1, i32::from(u16::MAX)) as u16
    };
    let door = scene.add_node(
        NodeId::ROOT,
        &p.name,
        NodeKind::Logic {
            kind: LogicNodeKind::Door {
                box_prop: slab_name,
                start_open: false,
                slide: Some(DoorSlide {
                    direction: p.direction,
                    distance,
                    ticks: p.ticks,
                }),
                lock: p.lock,
                refusal: p.refusal.clone(),
            },
            target: String::new(),
            killtarget: String::new(),
            master: String::new(),
            delay_ticks: 0,
            wait_ticks: 0,
            enabled: true,
            persistent: p.persistent,
        },
    );
    let lever = p.lever.as_ref().map(|l| {
        let id = scene.add_node(NodeId::ROOT, format!("{} lever", p.name), NodeKind::Entity);
        if let Some(n) = scene.node_mut(id) {
            n.transform.translation = [
                ed(l.cell[0] * 1024 + 512) as f32,
                ed(plane.center[1]) as f32,
                ed(l.cell[1] * 1024 + 512) as f32,
            ];
        }
        scene.add_node(
            id,
            "Interactable",
            NodeKind::Interactable {
                kind: InteractableKind::Switch {
                    target: p.name.clone(),
                    behavior: l.behavior,
                    unlocks: false,
                    persistent: p.persistent,
                    handle: None,
                    handle_slide: None,
                },
                prompt: l.prompt.clone(),
                radius: l.radius,
                enabled: true,
            },
        );
        id
    });
    Ok(DoorwayDoorOutcome {
        plane,
        door,
        slab,
        lever,
    })
}
