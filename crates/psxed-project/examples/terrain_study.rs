//! Reproduce the standalone terrain study and connected stress variant.
//! Generation overwrites study RONs from the preserved source snapshot.
//! Run from repository root: cargo run -p psxed-project --example terrain_study -- generate
//! Then: ... -- cook editor/projects/graybox-terrain/project.ron /absolute/generated
use psxed_project::{
    brush::Brush,
    terrain::{Terrain, TerrainShape},
    *,
};
use std::path::Path;
fn box_brush(min: [i32; 3], max: [i32; 3], material: ResourceId, group: NodeId) -> Brush {
    let mut b = Brush::cuboid(min, max);
    b.group = Some(group);
    for f in &mut b.faces {
        f.material = Some(material);
    }
    b
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "cheaper") {
        let step: [usize; 2] = [args[2].parse().unwrap(), args[3].parse().unwrap()];
        let destination = Path::new(&args[4]);
        let mut p =
            ProjectDocument::load_from_path("editor/projects/graybox-terrain/project.ron").unwrap();
        let scene = p.active_scene();
        let group = scene
            .nodes()
            .iter()
            .find(|n| n.name == "Terrain / southern rolling basin")
            .unwrap()
            .id;
        let source: Vec<_> = scene
            .brushes
            .iter()
            .filter(|b| b.group == Some(group))
            .cloned()
            .collect();
        let original = Terrain::from_brushes(&source).unwrap();
        assert!((0..2).all(|i| step[i] > 0 && original.cells[i].is_multiple_of(step[i])));
        let mut terrain = original.clone();
        for (i, &factor) in step.iter().enumerate() {
            terrain.cells[i] /= factor;
            terrain.spacing[i] *= factor as i32;
        }
        terrain.heights.clear();
        for z in 0..=terrain.cells[1] {
            for x in 0..=terrain.cells[0] {
                terrain
                    .heights
                    .push(original.heights[z * step[1] * (original.cells[0] + 1) + x * step[0]]);
            }
        }
        if let Some(scale) = args.get(5) {
            let scale: f64 = scale.parse().unwrap();
            assert!(scale > 0.0 && scale <= 1.0);
            for h in &mut terrain.heights {
                *h = (*h * scale / 16.0).round() * 16.0;
            }
        }
        terrain.validate().unwrap();
        let material = source[0].faces[0].material;
        let mut brushes = terrain.brushes(material).unwrap();
        // Keep the same palette, sampled at the corresponding original cell.
        for (i, b) in brushes.iter_mut().enumerate() {
            b.group = Some(group);
            let x = (i / 2) % terrain.cells[0];
            let z = (i / 2) / terrain.cells[0];
            let old = &source[(z * step[1] * original.cells[0] + x * step[0]) * 2 + i % 2];
            for (f, old) in b.faces.iter_mut().zip(&old.faces) {
                f.material = old.material;
                f.uv = old.uv;
            }
        }
        let scene = p.active_scene_mut();
        scene.brushes.retain(|b| b.group != Some(group));
        scene.brushes.extend(brushes);
        // Preserve X/Z positions and place each actor/hook on its actual new surface.
        let positions: Vec<_> = scene
            .nodes()
            .iter()
            .filter(|n| [2, 8, 204].contains(&n.id.raw()))
            .map(|n| (n.id, n.transform.translation))
            .collect();
        for (id, pos) in positions {
            let mut height = None;
            for t in terrain.triangles() {
                let [a, b, c] = t.map(|v| v.map(|n| n as f64));
                let [x, z] = [pos[0] as f64, pos[2] as f64];
                let den = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
                let u = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / den;
                let v = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / den;
                if u >= -1e-8 && v >= -1e-8 && u + v <= 1.0 + 1e-8 {
                    height = Some(u * a[1] + v * b[1] + (1.0 - u - v) * c[1]);
                    break;
                }
            }
            scene.node_mut(id).unwrap().transform.translation[1] =
                height.unwrap() as f32 + if id.raw() == 204 { 64.0 } else { 32.0 };
        }
        p.name = format!(
            "Graybox Terrain / {} x {}",
            terrain.cells[0], terrain.cells[1]
        );
        assert!(brush_world::diagnose_brush_world_leak(p.clone())
            .unwrap()
            .is_empty());
        p.save_to_path(destination).unwrap();
        println!(
            "Generated {} triangles, same footprint with resampled heights, sealed sky.",
            terrain.cells[0] * terrain.cells[1] * 2
        );
        return;
    }
    if args.get(1).is_some_and(|s| s == "cook") {
        let path = Path::new(&args[2]);
        let p = ProjectDocument::load_from_path(path).unwrap();
        let started = std::time::Instant::now();
        let (package, report) = playtest::build_package(&p, path.parent().unwrap());
        for w in &report.warnings {
            println!("WARNING {w}");
        }
        for e in &report.errors {
            println!("ERROR {e}");
        }
        assert!(report.is_ok(), "cook failed");
        let package = package.unwrap();
        println!("COOK_SECONDS {:.3}", started.elapsed().as_secs_f64());
        println!(
            "BUDGET {}",
            playtest::cooked_playtest_budgets(&p, &package).concise_summary()
        );
        println!("BSP_BYTES {}", package.world_geometry.bytes.len());
        if let Some(d) = playtest::analyze_pxbsp_draw_cost(&package).unwrap() {
            println!(
                "DRAW faces={} triangles={} leaves={} unreadable_pvs={}",
                d.world_face_count,
                d.world_base_triangle_count,
                d.non_solid_leaf_count,
                d.unreadable_pvs_leaf_count
            );
            for l in d.heaviest_leaves(5) {
                println!("HOT {l:?}");
            }
        }
        playtest::write_cook_result(Some(&package), Path::new(&args[3])).unwrap();
        return;
    }
    assert_eq!(args.get(1).map(String::as_str), Some("generate"));
    let mut p =
        ProjectDocument::load_from_path("editor/projects/graybox-terrain/source-project.ron")
            .unwrap();
    let original_scene = p.active_scene().clone();
    p.name = "Graybox Terrain".into();
    let sky = p
        .resources
        .iter()
        .find_map(|r| match &r.data {
            ResourceData::Material(m) if m.sky_aperture => Some(r.id),
            _ => None,
        })
        .unwrap();
    let colors = [
        [84, 111, 94],
        [100, 119, 94],
        [106, 112, 103],
        [119, 125, 113],
    ];
    let materials: Vec<_> = colors
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut m = MaterialResource::opaque(None);
            m.tint = c;
            p.add_resource(
                format!("Terrain / moss and stone {i}"),
                ResourceData::Material(m),
            )
        })
        .collect();
    let mut terrain = Terrain::generate(
        [8, 8],
        [4096, 4096],
        [-16384, 0, -49152],
        3072,
        42,
        TerrainShape::Hills,
        0.3,
    )
    .unwrap();
    // A broad, winding low route between two ridges; flatten the northern seam
    // exactly to the original courtyard floor. Outer vertices rise into banks.
    for z in 0..=8 {
        for x in 0..=8 {
            let xf = x as f64 - 4.0;
            let bend = (z as f64 * 0.7).sin() * 1.1;
            let bank = ((xf - bend).abs() / 2.5).clamp(0.0, 1.0);
            let seam = ((8 - z) as f64 / 2.0).min(1.0);
            let h = terrain.heights[z * 9 + x];
            terrain.heights[z * 9 + x] = (h * (0.2 + 0.8 * bank) + 1800.0 * bank) * seam;
        }
    }
    let ground = p.resources.iter().find(|r| r.id.raw() == 159).unwrap().id;
    let player_id = p
        .active_scene()
        .nodes()
        .iter()
        .find(|n| n.id.raw() == 2)
        .unwrap()
        .id;
    let s = p.active_scene_mut();
    // Open only the original southern wall, retaining its face finish and ownership.
    let wall = s
        .brushes
        .iter()
        .position(|b| {
            let q = b.solve();
            q.min[2].round() == -16640.0
                && q.max[2].round() == -16384.0
                && q.max[1] > 10000.0
                && q.min[0] < -8000.0
                && q.max[0] > 8000.0
        })
        .expect("southern seal wall");
    let original = s.brushes.remove(wall);
    let q = original.solve();
    let min = q.min.map(|v| v.round() as i32);
    let max = q.max.map(|v| v.round() as i32);
    for (a, b) in [
        (min, [-4096, max[1], max[2]]),
        ([4096, min[1], min[2]], max),
        ([-4096, 6144, min[2]], [4096, max[1], max[2]]),
    ] {
        let mut b = Brush::cuboid(a, b);
        b.group = original.group;
        for (f, old) in b.faces.iter_mut().zip(&original.faces) {
            f.material = old.material;
            f.uv = old.uv;
        }
        s.brushes.push(b);
    }
    let group = s.add_node(s.root, "Terrain / southern rolling basin", NodeKind::Group);
    let mut brushes = terrain.brushes(Some(materials[0])).unwrap();
    for (i, b) in brushes.iter_mut().enumerate() {
        b.group = Some(group);
        for f in &mut b.faces {
            let n = brush::Plane::from_points(f.points).unwrap().normal;
            if n[1] > 0 {
                f.material = Some(materials[(i / 2 + i / 16) % materials.len()]);
            }
        }
    }
    s.brushes.extend(brushes);
    let seal = s.add_node(s.root, "Sky / terrain enclosure", NodeKind::Group);
    let shell = terrain.sky_enclosure(12288, sky).unwrap();
    // North wall shares the original seal, including its entrance. Don't put a
    // second wall in that opening. The original slab already closes this seam.
    for (i, mut b) in shell.into_iter().enumerate() {
        if i == 3 {
            continue;
        }
        b.group = Some(seal);
        s.brushes.push(b);
    }
    // A sill below the doorway closes the 256-unit seam to the old floor.
    s.brushes.push(box_brush(
        [-4096, -512, -16640],
        [4096, 0, -16384],
        ground,
        seal,
    ));
    let light = s.add_node(
        s.root,
        "Terrain daylight",
        NodeKind::PointLight {
            color: [224, 236, 244],
            intensity: 0.3,
            radius: 48.0,
        },
    );
    s.node_mut(light).unwrap().transform.translation = [0.0, 10240.0, -32768.0];
    // Start above a lattice vertex with enough room for the existing camera.
    let spawn = terrain.vertex(4, 2);
    let player = s.node_mut(player_id).unwrap();
    player.transform.translation = [spawn[0] as f32, spawn[1] as f32 + 32.0, spawn[2] as f32];
    player.transform.rotation_degrees = [0.0, 0.0, 0.0];
    p.editor_camera.orbit_target = [0, 1536, -32768];
    p.editor_camera.orbit_radius = 42000;
    p.save_to_path("editor/projects/graybox-terrain/connected-stress.ron")
        .unwrap();
    // Preserve the complete source scene without making its geometry resident
    // in the terrain playtest. All character, audio, UI and camera resources stay.
    let s = p.active_scene_mut();
    let remove: Vec<_> = s
        .nodes()
        .iter()
        .filter(|n| {
            n.id != group
                && n.id != seal
                && n.id != light
                && matches!(n.kind, NodeKind::Group | NodeKind::PointLight { .. })
        })
        .map(|n| n.id)
        .collect();
    for id in remove {
        s.remove_node(id);
    }
    s.brushes.retain(|b| b.group == Some(group));
    for mut b in terrain.sky_enclosure(12288, sky).unwrap() {
        b.group = Some(seal);
        s.brushes.push(b);
    }
    let enemy_id = s.nodes().iter().find(|n| n.id.raw() == 8).unwrap().id;
    let enemy = terrain.vertex(4, 6);
    s.node_mut(enemy_id).unwrap().transform.translation = enemy.map(|v| v as f32);
    let hook_id = s.nodes().iter().find(|n| n.id.raw() == 204).unwrap().id;
    let hook = terrain.vertex(6, 5);
    s.node_mut(hook_id).unwrap().transform.translation =
        [hook[0] as f32, hook[1] as f32 + 64.0, hook[2] as f32];
    let mut original_scene = original_scene;
    original_scene.name = "Original Graybox Reach (preserved)".into();
    p.scenes.push(original_scene);
    p.save_to_path("editor/projects/graybox-terrain/project.ron")
        .unwrap();
    let mut flat = p.clone();
    flat.name = "Graybox Terrain / flat control".into();
    let s = flat.active_scene_mut();
    s.brushes.retain(|b| b.group != Some(group));
    s.brushes.push(box_brush(
        [-16384, -256, -49152],
        [16384, 0, -16384],
        materials[0],
        group,
    ));
    s.node_mut(player_id).unwrap().transform.translation[1] = 32.0;
    s.node_mut(enemy_id).unwrap().transform.translation[1] = 0.0;
    s.node_mut(hook_id).unwrap().transform.translation[1] = 64.0;
    flat.save_to_path("editor/projects/graybox-terrain/flat-control.ron")
        .unwrap();
    for (name, p) in [("terrain", p), ("flat", flat)] {
        let leak = brush_world::diagnose_brush_world_leak(p).unwrap();
        println!("{name}: leak points={}", leak.path.len());
        assert!(leak.is_empty(), "{name} leaks: {:?}", leak.path);
    }
    println!("Generated terrain: 128 wedges, 32768 square units; original scene preserved.");
}
