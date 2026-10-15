use psxed_project::{brush::Brush, *};
use serde::Deserialize;
use std::{collections::HashMap, path::Path};
#[derive(Deserialize)]
struct Spec {
    brushes: Vec<Solid>,
    spawn: Vec<i32>,
}
#[derive(Deserialize)]
struct Solid {
    name: String,
    vertices: Vec<Vec<i32>>,
    faces: Vec<Vec<usize>>,
    material: String,
    group: String,
}
fn main() {
    let root = Path::new("editor/projects/buried-grid-blockout");
    let spec: Spec =
        ron::from_str(&std::fs::read_to_string(root.join("source_assets/layout.ron")).unwrap())
            .unwrap();
    let mut p =
        ProjectDocument::load_from_path(root.join("source_assets/player-template.ron")).unwrap();
    p.name = "Cortex Ignition / Buried Grid / Organic Blockout".into();
    let skybytes = sky_texture::cook_equirectangular_cube_sky(
        &std::fs::read(root.join("source_assets/desert-sky.png")).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("assets/textures/desert-sky.psxt"), skybytes).unwrap();
    let mut mats = HashMap::new();
    for (name, tint, path) in [
        ("sand", [128, 128, 128], Some("sand.psxt")),
        ("rock", [128, 128, 128], Some("rock.psxt")),
        ("steel", [62, 64, 66], None),
        ("rust", [109, 72, 47], None),
        ("floor", [103, 94, 80], None),
        ("teal", [70, 183, 168], None),
        ("sky", [128, 128, 128], Some("desert-sky.psxt")),
    ] {
        let mut m = MaterialResource::opaque(path.map(|s| format!("assets/textures/{s}")));
        m.tint = tint;
        m.sky_aperture = name == "sky";
        mats.insert(
            name.to_string(),
            p.add_resource(format!("Buried Grid / {name}"), ResourceData::Material(m)),
        );
    }
    let scene = p.active_scene_mut();
    let remove: Vec<_> = scene
        .node(NodeId::ROOT)
        .unwrap()
        .children
        .iter()
        .copied()
        .filter(|id| id.raw() != 2)
        .collect();
    for id in remove {
        scene.remove_node(id);
    }
    scene.brushes.clear();
    scene.name = "Buried Grid - six-area winding loop".into();
    if let NodeKind::World {
        sky,
        culling,
        world_message,
        ..
    } = &mut scene.node_mut(NodeId::ROOT).unwrap().kind
    {
        sky.mode = SkyMode::Cube;
        sky.visibility = SkyVisibility::ThroughSkySurfaces;
        sky.texture = Some(mats["sky"]);
        culling.draw_distance = 60000;
        *world_message = None;
    }
    let id = scene.nodes().iter().find(|n| n.id.raw() == 2).unwrap().id;
    scene.node_mut(id).unwrap().transform.translation = spec
        .spawn
        .clone()
        .try_into()
        .unwrap_or([0; 3])
        .map(|x| x as f32);
    let mut groups = HashMap::new();
    for s in spec.brushes {
        let vertices: Vec<[i32; 3]> = s
            .vertices
            .iter()
            .map(|v| v.clone().try_into().unwrap())
            .collect();
        let mut b = Brush::from_convex_polyhedron(&vertices, &s.faces)
            .unwrap_or_else(|e| panic!("{}: {e}", s.name));
        let g = *groups
            .entry(s.group.clone())
            .or_insert_with(|| scene.add_node(NodeId::ROOT, s.group, NodeKind::Group));
        b.group = Some(g);
        for f in &mut b.faces {
            f.material = Some(mats[&s.material]);
            f.uv.scale_q8 = [2048, 2048];
        }
        scene.brushes.push(b);
    }
    p.editor_camera.orbit_target = [0, 2048, 0];
    p.editor_camera.orbit_radius = 80000;
    p.editor_camera.orbit_yaw_q12 = 640;
    p.editor_camera.orbit_pitch_q12 = 3500;
    p.save_to_path(root.join("project.ron")).unwrap();
    println!(
        "SAVED {} brushes, {} groups",
        p.active_scene().brushes.len(),
        groups.len()
    );
}
