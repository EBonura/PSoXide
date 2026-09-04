use psxed_project::{playtest::cook_to_dir, ProjectDocument};
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap())
        .canonicalize()
        .unwrap();
    let p = ProjectDocument::load_from_path(root.join("project.ron")).unwrap();
    let report = cook_to_dir(&p, &root, &root.join("baked/generated")).unwrap();
    for w in &report.warnings {
        eprintln!("warning: {w}");
    }
    for e in &report.errors {
        eprintln!("error: {e}");
    }
    assert!(report.is_ok());
}
