//! Headless "Starter Characters" sync: bring one project's starter catalogue
//! (Aletha, Light Enemy, Heavy Enemy, weapons, clips, sets) up to date with the embedded
//! default project, exactly as the resources-panel button does.
//!   cargo run -p psxed-ui --example sync_starter -- <project_dir>

use std::path::PathBuf;

use psxed_project::ProjectDocument;
use psxed_ui::sync_starter_character_catalogue;

fn main() -> Result<(), String> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: sync_starter <project_dir>")?,
    );
    let file = dir.join("project.ron");
    let mut project =
        ProjectDocument::load_from_path(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let report = sync_starter_character_catalogue(&mut project, &dir)?;
    if report.changed() {
        project
            .save_to_path(&file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    println!("{}: {report:?}", dir.display());
    Ok(())
}
