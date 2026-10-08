//! Dump every cooked model mesh and animation clip of a project, plus an
//! index, so host tools can study exactly what ships.
//!
//! Usage: anim_dump <project.ron> <out-dir>

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psxed_project::{playtest::build_package, ProjectDocument};

fn safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_path = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    let text = std::fs::read_to_string(&project_path).expect("read project");
    let project = ProjectDocument::from_ron_str(&text).expect("parse project");
    let root = Path::new(&project_path).parent().unwrap().to_path_buf();
    let (package, report) = build_package(&project, &root);
    let package = package.unwrap_or_else(|| panic!("cook failed: {:?}", report.errors));
    let mut index = String::new();
    for (mi, model) in package.models.iter().enumerate() {
        let dir = out.join(format!("{mi}_{}", safe(&model.name)));
        std::fs::create_dir_all(&dir).unwrap();
        let mesh = &package.assets[model.mesh_asset_index];
        std::fs::write(dir.join("model.psxmdl"), &mesh.bytes).unwrap();
        writeln!(
            index,
            "model\t{mi}\t{}\tmesh_bytes={}",
            model.name,
            mesh.bytes.len()
        )
        .unwrap();
        for ci in 0..model.clip_count as usize {
            let clip = &package.model_clips[model.clip_first as usize + ci];
            let asset = &package.assets[clip.animation_asset_index];
            let file = format!("{ci:02}_{}.psxanim", safe(&clip.name));
            std::fs::write(dir.join(&file), &asset.bytes).unwrap();
            writeln!(
                index,
                "clip\t{mi}\t{ci}\t{}\t{}\tbytes={}\tasset={}\tclass={:?}",
                clip.name,
                file,
                asset.bytes.len(),
                clip.animation_asset_index,
                asset.streamed_class
            )
            .unwrap();
        }
    }
    std::fs::write(out.join("index.tsv"), index).unwrap();
}
