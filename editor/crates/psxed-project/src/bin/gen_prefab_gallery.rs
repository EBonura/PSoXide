//! Build a gallery project: every prefab in the library, one Room each, laid
//! out on a grid so the whole kit can be walked and inspected in one place.
//!
//! One room per piece rather than one room holding everything, because the
//! whole kit in a single grid is ~106 cells wide and ~2000 triangles, and the
//! runtime caps a room at 32 cells and 2048 triangles. Separate Room nodes also
//! give the scene tree a legible index: each room is named after its prefab.
//!
//! Re-runnable. Regenerate whenever the kit changes.

use psxed_project::*;

/// Cells between adjacent pieces. Wide enough that neighbouring rooms read as
/// separate objects rather than one continuous level.
const GAP: i32 = 5;
/// Pieces per row.
const COLS: usize = 5;

fn main() {
    // Optional name filter, so a focused gallery of just the stairs (or just
    // the connectors) can be cooked and looked at without the others in the way.
    let filter = std::env::args().nth(1);
    let mut paths = list_prefabs().expect("prefab directory readable");
    paths.sort();
    if let Some(needle) = filter.as_deref() {
        let needle = needle.to_lowercase();
        paths.retain(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }
    if paths.is_empty() {
        eprintln!("no prefabs in {}", prefabs_dir().display());
        std::process::exit(1);
    }

    let mut project = ProjectDocument::starter();
    project.name = match filter.as_deref() {
        Some(f) => format!("Prefab Gallery {f}"),
        None => "Prefab Gallery".to_string(),
    };

    // The starter ships one room with a small pad in it. Empty it and reuse it
    // as the first bay, so the player spawn still has floor under it.
    let starter_room = project
        .active_scene()
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id);
    if let Some(id) = starter_room {
        // Drop the starter's own preview light, or the first bay ends up with
        // two when the rule is one per piece. Only lights: the starter's other
        // children include the player, and removing those leaves the cook with
        // no player source at all.
        let lights: Vec<NodeId> = project
            .active_scene()
            .node(id)
            .map(|n| n.children.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|child| {
                matches!(
                    project.active_scene().node(*child).map(|n| &n.kind),
                    Some(NodeKind::PointLight { .. })
                )
            })
            .collect();
        for light in lights {
            project.active_scene_mut().remove_node(light);
        }
        if let Some(NodeKind::Section { grid }) =
            project.active_scene_mut().node_mut(id).map(|n| &mut n.kind)
        {
            for sector in grid.sectors.iter_mut() {
                *sector = None;
            }
            grid.floors_above.clear();
        }
    }

    // Column pitch is set by the widest piece so rows line up.
    let loaded: Vec<Prefab> = paths
        .iter()
        .map(|p| Prefab::load_from_path(p).expect("prefab loads"))
        .collect();
    let pitch_x = loaded.iter().map(|p| p.width).max().unwrap_or(1) + GAP;
    let pitch_z = loaded.iter().map(|p| p.height).max().unwrap_or(1) + GAP;

    let mut placed = 0usize;
    for (index, prefab) in loaded.iter().enumerate() {
        let col = (index % COLS) as i32;
        let row = (index / COLS) as i32;

        // Reuse the starter room for the first bay so the player spawns on it.
        let room = if index == 0 {
            let id = starter_room.expect("starter has a room");
            if let Some(NodeKind::Section { grid }) =
                project.active_scene_mut().node_mut(id).map(|n| &mut n.kind)
            {
                *grid = WorldGrid::empty(
                    prefab.width as u16,
                    prefab.height as u16,
                    prefab.sector_size,
                );
            }
            id
        } else {
            project.active_scene_mut().add_node(
                NodeId::ROOT,
                &prefab.name,
                NodeKind::Section {
                    grid: WorldGrid::empty(
                        prefab.width as u16,
                        prefab.height as u16,
                        prefab.sector_size,
                    ),
                },
            )
        };
        // Position via the grid's world-cell origin, NOT the node transform.
        // The cook places section geometry from `grid.origin` alone and never
        // reads a section's own X/Z translation, so laying the gallery out with
        // transforms produced twenty sections stacked on one spot that merely
        // looked separated in the editor.
        if let Some(node) = project.active_scene_mut().node_mut(room) {
            node.name = prefab.name.clone();
            node.transform.translation = [0.0, 0.0, 0.0];
        }
        if let Some(NodeKind::Section { grid }) = project
            .active_scene_mut()
            .node_mut(room)
            .map(|n| &mut n.kind)
        {
            grid.origin = [col * pitch_x, -(row * pitch_z)];
            for floor in grid.floors_above.iter_mut() {
                floor.origin = [col * pitch_x, -(row * pitch_z)];
            }
        }

        // Materials rebind by name against this project, links resolve onto
        // this room. Same path the editor stamp takes.
        let (floors, unbound) = prefab.bound_floors(&project, room, 0);
        if unbound > 0 {
            eprintln!(
                "warning: {} lost {unbound} material references",
                prefab.name
            );
        }

        let Some(NodeKind::Section { grid }) = project
            .active_scene_mut()
            .node_mut(room)
            .map(|n| &mut n.kind)
        else {
            continue;
        };
        for (floor_index, floor) in floors.iter().enumerate() {
            while grid.floor_count() <= floor_index {
                let base = grid.elevation;
                let created = grid.push_floor();
                if let Some(g) = grid.floor_mut(created) {
                    g.elevation = base.saturating_add(floor.relative_elevation);
                }
            }
            let Some(target) = grid.floor_mut(floor_index) else {
                continue;
            };
            for cell in &floor.cells {
                let (Some(sector), Ok(x), Ok(z)) = (
                    cell.sector.clone(),
                    u16::try_from(cell.offset[0]),
                    u16::try_from(cell.offset[1]),
                ) else {
                    continue;
                };
                if let Some(slot) = target.sector_index(x, z).map(|i| &mut target.sectors[i]) {
                    *slot = Some(sector);
                }
            }
        }
        // The gallery writes cells directly rather than going through the
        // editor stamp, so it has to place the piece's lights itself.
        let lights: Vec<(NodeKind, [f32; 3])> = {
            let Some(NodeKind::Section { grid }) =
                project.active_scene().node(room).map(|n| &n.kind)
            else {
                continue;
            };
            prefab
                .lights
                .iter()
                .map(|light| {
                    let editor = grid.world_cells_to_editor([
                        light.cell[0] as f32 + 0.5,
                        light.cell[1] as f32 + 0.5,
                    ]);
                    (
                        NodeKind::PointLight {
                            color: light.color,
                            intensity: light.intensity,
                            radius: light.radius,
                        },
                        [editor[0], light.height_sectors, editor[1]],
                    )
                })
                .collect()
        };
        for (kind, translation) in lights {
            let id = project
                .active_scene_mut()
                .add_node(room, "Prefab Light", kind);
            if let Some(node) = project.active_scene_mut().node_mut(id) {
                node.transform.translation = translation;
            }
        }

        placed += 1;
    }

    let dir = projects_dir().join(project_file_stem(&project.name));
    std::fs::create_dir_all(&dir).expect("project directory");
    // Assets are shared with the starter, so copy them across rather than
    // leaving the gallery with dangling texture references.
    let starter_assets = projects_dir().join("default").join("assets");
    if starter_assets.is_dir() {
        copy_tree(&starter_assets, &dir.join("assets")).expect("assets copy");
    }
    let ron = project.to_ron_string().expect("project serialises");
    std::fs::write(dir.join("project.ron"), ron).expect("project writes");
    println!(
        "{placed} pieces, {} rooms, laid out {COLS} per row -> {}",
        placed,
        dir.display()
    );
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), dst)?;
        }
    }
    Ok(())
}
