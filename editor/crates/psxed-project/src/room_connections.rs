//! Derived paired-room connection view for editor workflows.
//!
//! The saved/runtime source of truth stays Tomb Raider-like: portal
//! records live under their source room and point at a target room.
//! This module folds those directed records into editor-facing
//! connections so UI can present `Room A <-> Room B` without creating
//! a second persisted graph.

use std::collections::HashSet;

use crate::portal_rooms::{portal_edge_for_node, PortalEdge};
use crate::{GridDirection, NodeId, NodeKind, PortalGeometry, Scene, SceneNode, WorldGrid};

/// Broad portal surface class used by the connection UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomConnectionKind {
    /// Cardinal wall seam.
    Wall,
    /// Horizontal floor or ceiling opening.
    FloorCeiling,
    /// Not enough information to classify yet.
    Unknown,
}

impl RoomConnectionKind {
    /// Short human-readable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Wall => "wall portal",
            Self::FloorCeiling => "floor/ceiling portal",
            Self::Unknown => "portal",
        }
    }
}

/// Pairing/repair state for a derived room connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomConnectionStatus {
    /// Source and target both have reciprocal directed portals.
    Paired,
    /// Source portal has no target assigned yet.
    Unassigned,
    /// Source points at a room, but that room has no reciprocal portal.
    Unpaired,
    /// Source points at a missing or non-room node.
    MissingTarget,
    /// Multiple reciprocal candidates exist; the editor needs user intent.
    Ambiguous,
}

impl RoomConnectionStatus {
    /// Short human-readable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Paired => "paired",
            Self::Unassigned => "unassigned",
            Self::Unpaired => "needs repair",
            Self::MissingTarget => "missing target",
            Self::Ambiguous => "ambiguous",
        }
    }

    /// Whether the connection needs author attention before it is clean.
    pub const fn needs_repair(self) -> bool {
        !matches!(self, Self::Paired)
    }
}

/// One directed portal endpoint in its source room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomConnectionEndpoint {
    /// Room that owns the portal node.
    pub room: NodeId,
    /// Directed portal node id.
    pub portal: NodeId,
    /// Target room, when wired.
    pub target_room: Option<NodeId>,
    /// Snapped sector edge for authored seam portals.
    pub edge: Option<PortalEdge>,
    /// Imported TR levels can carry an exact 3D rectangle.
    pub geometry: Option<PortalGeometry>,
    /// Broad surface type.
    pub kind: RoomConnectionKind,
}

/// Editor-facing connection derived from one or two directed portals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomConnection {
    /// Primary endpoint, always present.
    pub a: RoomConnectionEndpoint,
    /// Reciprocal endpoint when one could be matched.
    pub b: Option<RoomConnectionEndpoint>,
    /// Additional reciprocal candidates for ambiguous cases.
    pub alternatives: Vec<NodeId>,
    /// Pairing/repair state.
    pub status: RoomConnectionStatus,
    /// Surface class to show in summaries.
    pub kind: RoomConnectionKind,
}

impl RoomConnection {
    /// Node id that can represent this connection in node-selection based UI.
    pub const fn primary_portal(&self) -> NodeId {
        self.a.portal
    }

    /// Returns true when `portal` is one of this connection's directed endpoints.
    pub fn contains_portal(&self, portal: NodeId) -> bool {
        self.a.portal == portal || self.b.as_ref().is_some_and(|b| b.portal == portal)
    }
}

/// Build the paired editor connection view from the scene's directed portal nodes.
pub fn derive_room_connections(scene: &Scene) -> Vec<RoomConnection> {
    let endpoints = collect_portal_endpoints(scene);
    let mut used = HashSet::new();
    let mut connections = Vec::new();

    for endpoint in &endpoints {
        if used.contains(&endpoint.portal) {
            continue;
        }
        used.insert(endpoint.portal);

        let Some(target_room) = endpoint.target_room else {
            connections.push(RoomConnection {
                a: endpoint.clone(),
                b: None,
                alternatives: Vec::new(),
                status: RoomConnectionStatus::Unassigned,
                kind: endpoint.kind,
            });
            continue;
        };
        if !is_room(scene, target_room) {
            connections.push(RoomConnection {
                a: endpoint.clone(),
                b: None,
                alternatives: Vec::new(),
                status: RoomConnectionStatus::MissingTarget,
                kind: endpoint.kind,
            });
            continue;
        }

        let reciprocal = endpoints
            .iter()
            .filter(|candidate| {
                candidate.portal != endpoint.portal
                    && candidate.room == target_room
                    && candidate.target_room == Some(endpoint.room)
            })
            .cloned()
            .collect::<Vec<_>>();

        let (paired, alternatives, status) = match reciprocal.as_slice() {
            [] => (None, Vec::new(), RoomConnectionStatus::Unpaired),
            [only] => (Some(only.clone()), Vec::new(), RoomConnectionStatus::Paired),
            [first, rest @ ..] => (
                Some(first.clone()),
                rest.iter().map(|endpoint| endpoint.portal).collect(),
                RoomConnectionStatus::Ambiguous,
            ),
        };
        if let Some(pair) = &paired {
            used.insert(pair.portal);
        }
        for alternative in &alternatives {
            used.insert(*alternative);
        }
        connections.push(RoomConnection {
            a: endpoint.clone(),
            b: paired,
            alternatives,
            status,
            kind: merge_kind(endpoint.kind, reciprocal.first().map(|e| e.kind)),
        });
    }

    connections.sort_by_key(|connection| {
        (
            connection.a.room.raw(),
            connection
                .a
                .target_room
                .map(NodeId::raw)
                .unwrap_or(u64::MAX),
            connection.a.portal.raw(),
        )
    });
    connections
}

/// Find the derived connection containing a directed portal node.
pub fn connection_for_portal(scene: &Scene, portal: NodeId) -> Option<RoomConnection> {
    derive_room_connections(scene)
        .into_iter()
        .find(|connection| connection.contains_portal(portal))
}

fn collect_portal_endpoints(scene: &Scene) -> Vec<RoomConnectionEndpoint> {
    scene
        .nodes()
        .iter()
        .filter_map(|node| endpoint_for_node(scene, node))
        .collect()
}

fn endpoint_for_node(scene: &Scene, node: &SceneNode) -> Option<RoomConnectionEndpoint> {
    let NodeKind::Portal {
        target_room,
        geometry,
        ..
    } = &node.kind
    else {
        return None;
    };
    let room = room_ancestor(scene, node.id)?;
    let edge = room_grid(scene, room).and_then(|grid| portal_edge_for_node(grid, node));
    let kind = geometry
        .as_ref()
        .map(classify_geometry)
        .or_else(|| edge.map(classify_edge))
        .unwrap_or(RoomConnectionKind::Unknown);
    Some(RoomConnectionEndpoint {
        room,
        portal: node.id,
        target_room: *target_room,
        edge,
        geometry: geometry.clone(),
        kind,
    })
}

fn room_ancestor(scene: &Scene, id: NodeId) -> Option<NodeId> {
    let mut current = Some(id);
    while let Some(node_id) = current {
        let node = scene.node(node_id)?;
        if matches!(node.kind, NodeKind::Room { .. }) {
            return Some(node_id);
        }
        current = node.parent;
    }
    None
}

fn room_grid(scene: &Scene, room: NodeId) -> Option<&WorldGrid> {
    scene.node(room).and_then(|node| match &node.kind {
        NodeKind::Room { grid } => Some(grid),
        _ => None,
    })
}

fn is_room(scene: &Scene, id: NodeId) -> bool {
    scene
        .node(id)
        .is_some_and(|node| matches!(node.kind, NodeKind::Room { .. }))
}

fn classify_edge(edge: PortalEdge) -> RoomConnectionKind {
    match edge.direction {
        GridDirection::North | GridDirection::East | GridDirection::South | GridDirection::West => {
            RoomConnectionKind::Wall
        }
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => {
            RoomConnectionKind::Unknown
        }
    }
}

fn classify_geometry(geometry: &PortalGeometry) -> RoomConnectionKind {
    let [nx, ny, nz] = geometry.normal;
    let ax = nx.abs();
    let ay = ny.abs();
    let az = nz.abs();
    if ay > ax.max(az) {
        RoomConnectionKind::FloorCeiling
    } else if ax.max(az) > 0 {
        RoomConnectionKind::Wall
    } else {
        RoomConnectionKind::Unknown
    }
}

fn merge_kind(a: RoomConnectionKind, b: Option<RoomConnectionKind>) -> RoomConnectionKind {
    match (a, b) {
        (RoomConnectionKind::FloorCeiling, _) | (_, Some(RoomConnectionKind::FloorCeiling)) => {
            RoomConnectionKind::FloorCeiling
        }
        (RoomConnectionKind::Wall, _) | (_, Some(RoomConnectionKind::Wall)) => {
            RoomConnectionKind::Wall
        }
        _ => RoomConnectionKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeKind, ProjectDocument, Transform3, WorldGrid};

    fn add_room(scene: &mut Scene, name: &str) -> NodeId {
        scene.add_node(
            scene.root,
            name,
            NodeKind::Room {
                grid: WorldGrid::empty(2, 2, 1024),
            },
        )
    }

    fn add_portal(scene: &mut Scene, room: NodeId, name: &str, target: Option<NodeId>) -> NodeId {
        let id = scene.add_node(
            room,
            name,
            NodeKind::Portal {
                target_room: target,
                target_entry: String::new(),
                entry_name: String::new(),
                geometry: None,
            },
        );
        if let Some(node) = scene.node_mut(id) {
            node.transform = Transform3 {
                translation: [0.5, 0.0, 1.0],
                rotation_degrees: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
            };
        }
        id
    }

    #[test]
    fn reciprocal_portals_fold_into_one_paired_connection() {
        let mut project = ProjectDocument::new("test");
        let scene = project.active_scene_mut();
        let a = add_room(scene, "Room A");
        let b = add_room(scene, "Room B");
        let ab = add_portal(scene, a, "A to B", Some(b));
        let ba = add_portal(scene, b, "B to A", Some(a));

        let connections = derive_room_connections(scene);

        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].status, RoomConnectionStatus::Paired);
        assert_eq!(connections[0].a.portal, ab);
        assert_eq!(
            connections[0].b.as_ref().map(|endpoint| endpoint.portal),
            Some(ba)
        );
    }

    #[test]
    fn one_way_portal_is_marked_for_repair() {
        let mut project = ProjectDocument::new("test");
        let scene = project.active_scene_mut();
        let a = add_room(scene, "Room A");
        let b = add_room(scene, "Room B");
        let ab = add_portal(scene, a, "A to B", Some(b));

        let connections = derive_room_connections(scene);

        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].status, RoomConnectionStatus::Unpaired);
        assert_eq!(connections[0].primary_portal(), ab);
    }
}
