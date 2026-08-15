//! Resolve a material's texture (any mode, including Generated) into a
//! `.psxt` file for preview and debug tooling.
//!
//! Usage:
//!   cargo run -p psxed-project --example dump_material_psxt -- \
//!       <project.ron> <material-name> <out.psxt>

use std::path::PathBuf;

use psxed_project::{ProjectDocument, ResourceData};

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: dump_material_psxt <project.ron> <material-name> <out.psxt>";
    let project_path = PathBuf::from(args.next().expect(usage));
    let name = args.next().expect(usage);
    let out = PathBuf::from(args.next().expect(usage));

    let project = ProjectDocument::load_from_path(&project_path).expect("load project.ron");
    let root = project_path.parent().expect("project root");
    let material = project
        .resources
        .iter()
        .find(|resource| {
            resource.name == name && matches!(resource.data, ResourceData::Material(_))
        })
        .unwrap_or_else(|| panic!("no Material resource named {name:?}"));
    let (key, bytes) = psxed_project::resolve_material_texture_psxt(&project, material.id, root)
        .expect("resolve material texture")
        .expect("material resolves to no texture");
    std::fs::write(&out, &bytes).expect("write psxt");
    println!(
        "{name} -> {} ({} bytes, key {key})",
        out.display(),
        bytes.len()
    );
}
