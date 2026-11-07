//! Re-cook a Model resource's mesh from a replacement GLB (for example a
//! decimated export) without saving the project, and report how the cooked
//! `.psxmdl` compares with the resource's current one.
//!
//! ```text
//! reimport-model-probe project.ron "Light Enemy / Artigli" decimated.glb [extra animation sources...]
//! ```
//!
//! The replacement is cooked into `assets/models/<name>_probe/`; the project
//! file is left untouched. Joint count, parts, vertex and face counts, the
//! world scale and the joint table are printed side by side so a mesh swap
//! that would desync the resource's existing clips is visible before any
//! file is copied over the original.

use std::path::{Path, PathBuf};

use psxed_project::model_import::{import_model_with_animation_sources, resolve_path};
use psxed_project::{ProjectDocument, ResourceData};

fn fail(message: String) -> ! {
    eprintln!("reimport-model-probe: {message}");
    std::process::exit(2);
}

fn describe(label: &str, bytes: &[u8]) -> Vec<u8> {
    let model = psx_asset::Model::from_bytes(bytes)
        .unwrap_or_else(|e| fail(format!("{label}: not a cooked model: {e:?}")));
    println!(
        "{label}: joints {} parts {} vertices {} faces {} flags {:#06x} local_to_world_q12 {} floor_lift {}",
        model.joint_count(),
        model.part_count(),
        model.vertex_count(),
        model.face_count(),
        model.flags(),
        model.local_to_world_q12(),
        model.bind_pose_floor_lift(),
    );
    let mut parents = Vec::new();
    for joint in 0..model.joint_count() {
        let record = model
            .joint(joint)
            .unwrap_or_else(|| fail(format!("{label}: joint {joint}")));
        parents.push(record.parent().map_or(0xff, |p| p as u8));
    }
    parents
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        fail("usage: reimport-model-probe project.ron <model name> <replacement.glb> [animation sources...]".into());
    }
    let project_path = PathBuf::from(&args[0]);
    let model_name = &args[1];
    let replacement = PathBuf::from(&args[2]);
    let extra: Vec<PathBuf> = args[3..].iter().map(PathBuf::from).collect();
    let project_root = match project_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let text = std::fs::read_to_string(&project_path)
        .unwrap_or_else(|e| fail(format!("{}: {e}", project_path.display())));
    let mut project = ProjectDocument::from_ron_str(&text)
        .unwrap_or_else(|e| fail(format!("{}: parse failed: {e}", project_path.display())));

    let (current_path, world_height) = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Model(model) if resource.name == *model_name => Some((
                resolve_path(&model.model_path, Some(&project_root)),
                model.world_height,
            )),
            _ => None,
        })
        .unwrap_or_else(|| fail(format!("no Model resource named {model_name:?}")));
    let current = std::fs::read(&current_path)
        .unwrap_or_else(|e| fail(format!("{}: {e}", current_path.display())));

    let config = psxed_project::model_import::RigidModelConfig {
        texture_width: 256,
        texture_height: 256,
        texture_depth: psxed_project::model_import::TextureDepth::Bit8,
        animation_fps: 12,
        world_height,
        normalize_root_translation: false,
        strip_animation_scale: true,
        prune_detached_face_islands: 0,
        extra_animations_affect_bounds: true,
        force_single_bind: std::env::var("PROBE_SINGLE_BIND").is_ok(),
        double_sided: false,
        ignore_embedded_animations: false,
        collapse_bone_patterns: if std::env::var("PROBE_COLLAPSE").is_ok() {
            psxed_project::model_import::default_collapse_bone_patterns()
        } else {
            Vec::new()
        },
    };
    let output_name = format!(
        "{}_probe",
        Path::new(&args[2]).file_stem().unwrap().to_string_lossy()
    );
    let id = import_model_with_animation_sources(
        &mut project,
        &replacement,
        &extra,
        &output_name,
        &project_root,
        config,
    )
    .unwrap_or_else(|e| fail(format!("import failed: {e}")));
    let probe_path = project
        .resource(id)
        .and_then(|resource| match &resource.data {
            ResourceData::Model(model) => {
                Some(resolve_path(&model.model_path, Some(&project_root)))
            }
            _ => None,
        })
        .unwrap_or_else(|| fail("probe resource missing".into()));
    let probe = std::fs::read(&probe_path)
        .unwrap_or_else(|e| fail(format!("{}: {e}", probe_path.display())));

    let a = describe("current", &current);
    let b = describe("probe  ", &probe);
    println!("joint parents equal: {}", a == b);
    println!("probe written to {}", probe_path.display());
}
