use std::path::PathBuf;

use psxed_project::ProjectDocument;
use psxed_ui::sync_builtin_sky_catalogue;

fn main() -> Result<(), String> {
    let mut roots = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        roots = psxed_project::list_projects().map_err(|error| error.to_string())?;
        roots.push(psxed_project::new_project_template_dir());
    }
    roots.sort();
    roots.dedup();

    for root in roots {
        let project_path = root.join("project.ron");
        let mut project = ProjectDocument::load_from_path(&project_path)
            .map_err(|error| format!("{}: {error}", project_path.display()))?;
        let report = sync_builtin_sky_catalogue(&mut project, &root)?;
        if report.changed() {
            project
                .save_to_path(&project_path)
                .map_err(|error| format!("{}: {error}", project_path.display()))?;
        }
        println!(
            "{}: {} added, {} updated, {} file(s) written",
            root.display(),
            report.resources_added,
            report.resources_updated,
            report.files_written
        );
    }
    Ok(())
}
