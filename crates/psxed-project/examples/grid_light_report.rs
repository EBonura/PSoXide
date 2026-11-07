//! Cook a grid project's world with the global light off and on and print
//! what the bake costs and what it changed.
//!
//! ```text
//! cargo run --release -p psxed-project --example grid_light_report -- <project dir> [out.ron]
//! ```
//!
//! For each setting it builds the playtest package (the same cook a Play
//! build runs) and prints the cooked sizes, so the quad and vertex counts of
//! the two can be put side by side, then times the pieces of the global light
//! on their own: the shadow scene, the face caster and the gather. With an
//! `out.ron` it also writes the project with the global light on, to render
//! with the editor preview.

use std::path::PathBuf;
use std::time::Instant;

use psxed_project::grid::faces::all_faces_with;
use psxed_project::grid::lighting::gather::{solve, GatherInput};
use psxed_project::grid::lighting::{
    scene_grid_lights, AlbedoTable, FaceCaster, GlobalLight, LightScene,
};
use psxed_project::ProjectDocument;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("a project directory"));
    let out = args.next().map(PathBuf::from);
    let (root, file) = if dir.is_dir() {
        (dir.clone(), dir.join("project.ron"))
    } else {
        (dir.parent().unwrap().to_path_buf(), dir.clone())
    };
    let project = ProjectDocument::load_from_path(&file).expect("the project loads");
    let world = project
        .active_scene()
        .world
        .grid()
        .expect("a grid project")
        .clone();
    if let Some(b) = world.editor_bounds() {
        println!("editor bounds {:?} .. {:?}", b.min, b.max);
    }
    for (i, room) in world.rooms.iter().enumerate() {
        println!(
            "room {i} {:?}: {}x{} sectors at {:?}, y_offset {}, ambient {:?}",
            room.name, room.width, room.depth, room.origin, room.y_offset, room.ambient
        );
    }
    for node in project.active_scene().nodes() {
        if let psxed_project::NodeKind::PointLight {
            color,
            intensity,
            radius,
        } = &node.kind
        {
            println!(
                "light {:?} at {:?} colour {color:?} intensity {intensity} radius {radius}",
                node.name, node.transform.translation
            );
        }
    }
    let faces = all_faces_with(&world, &());
    let corners: usize = faces.iter().map(|f| f.grid.len()).sum();
    println!(
        "{}: {} rooms, {} faces, {} face corners, global light {:?}",
        file.display(),
        world.rooms.len(),
        faces.len(),
        corners,
        world.global_light.enabled
    );

    for (name, light) in [
        ("off", GlobalLight::DEFAULT),
        ("on ", GlobalLight::NEW_WORLD),
    ] {
        let mut p = project.clone();
        p.active_scene_mut().world.grid_mut().unwrap().global_light = light;
        let t = Instant::now();
        let (package, report) = psxed_project::playtest::build_package(&p, &root);
        let took = t.elapsed();
        let Some(package) = package else {
            println!(
                "global light {name}: no package: {:?}",
                report.error_messages()
            );
            continue;
        };
        let world_pkg = &package.world_geometry;
        println!(
            "global light {name}: build_package {:.0} ms, {} surfaces, {} bytes of room chunks\n{}",
            took.as_secs_f64() * 1000.0,
            world_pkg.faces,
            world_pkg.rooms.len(),
            world_pkg.summary
        );
    }

    let on = {
        let mut w = world.clone();
        w.global_light = GlobalLight::NEW_WORLD;
        w
    };
    let t = Instant::now();
    let scene = LightScene::new(&on).expect("scene");
    println!("shadow scene (collision tables): {:.0} ms", ms(t));
    let t = Instant::now();
    let caster = FaceCaster::build(&faces, &scene.units(), &|_| true);
    println!(
        "face caster: {:.0} ms, {} faces",
        ms(t),
        caster.face_count()
    );
    let t = Instant::now();
    let calls = 2000;
    for _ in 0..calls {
        scene
            .with_rig(&[], on.shading, None, |_| ())
            .expect("a built scene parses");
    }
    println!(
        "opening the shadow query (parse of the collision): {:.1} us per call",
        t.elapsed().as_secs_f64() * 1e6 / f64::from(calls)
    );
    let lights = scene_grid_lights(project.active_scene(), 16.0);
    let albedo = AlbedoTable::from_project(&project, &root, faces.iter().map(|f| f.material));
    for rays in [16u8, 32, 64] {
        let mut w = on.clone();
        w.global_light.rays = rays;
        let t = Instant::now();
        let field = solve(&GatherInput {
            world: &w,
            units: scene.units(),
            lights: &lights,
            probe: scene.probe(),
            materials: &albedo,
            cancel: None,
        })
        .expect("solved");
        println!(
            "gather, {rays} rays, {} bounces: {:.0} ms, {} samples, {} rays, {} threads",
            w.global_light.bounces,
            ms(t),
            field.stats.samples,
            field.stats.rays,
            field.stats.threads
        );
    }

    if let Some(out) = out {
        let mut p = project.clone();
        p.active_scene_mut().world.grid_mut().unwrap().global_light = GlobalLight::NEW_WORLD;
        p.save_to_path(&out).expect("write");
        println!("wrote {}", out.display());
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
