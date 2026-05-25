//! CLI: import a Tomb Raider 4 `.tr4` level into a PSoXide project.
//!
//! Usage:
//!   import-tr-level <source.tr4> <output/project.ron> [project name]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use psxed_project::tr_level::{import_tr4_project, TrImportReport, TR_IMPORT_TEXTURE_PATH};

const IMPORT_TEXTURE_CANDIDATES: &[&str] = &[
    "projects/default/assets/textures/cobbles_1a.psxt",
    "editor/projects/default/assets/textures/cobbles_1a.psxt",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args.len() > 3 {
        eprintln!("Usage: import-tr-level <source.tr4> <output/project.ron> [project name]");
        return ExitCode::from(2);
    }

    let source = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    let project_name = args.get(2).cloned().unwrap_or_else(|| {
        source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("TR4 Level")
            .to_string()
    });

    let (project, report) = match import_tr4_project(&source, project_name) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("[import-tr-level] import failed: {error}");
            return ExitCode::from(1);
        }
    };

    if let Err(error) = copy_import_texture(&output) {
        eprintln!("[import-tr-level] material setup failed: {error}");
        return ExitCode::from(2);
    }

    if let Err(error) = project.save_to_path(&output) {
        eprintln!("[import-tr-level] save failed: {error}");
        return ExitCode::from(2);
    }

    print_report(&report, &output);
    ExitCode::SUCCESS
}

fn copy_import_texture(output: &Path) -> Result<(), String> {
    let project_root = output.parent().unwrap_or_else(|| Path::new("."));
    let destination = project_root.join(TR_IMPORT_TEXTURE_PATH);
    if destination.is_file() {
        return Ok(());
    }
    let Some(source) = IMPORT_TEXTURE_CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
    else {
        return Err(format!(
            "could not find {} in the default project assets",
            TR_IMPORT_TEXTURE_PATH
        ));
    };
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::copy(source, &destination).map_err(|error| {
        format!(
            "failed to copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn print_report(report: &TrImportReport, output: &PathBuf) {
    println!(
        "[import-tr-level] Rooms: {}  Portals: {}  Sectors: {}",
        report.rooms, report.portals, report.sectors
    );
    println!(
        "[import-tr-level] Vertical floor links: {}",
        report.vertical_floor_links
    );
    println!(
        "[import-tr-level] Room mesh: vertices={} rectangles={} triangles={}",
        report.mesh_vertices, report.mesh_rectangles, report.mesh_triangles
    );
    println!(
        "[import-tr-level] Textiles: room={} object={} bump={}",
        report.room_textiles, report.object_textiles, report.bump_textiles
    );
    if let Some(path) = &report.source_path {
        println!("[import-tr-level] Source: {}", path.display());
    }
    println!("[import-tr-level] Wrote: {}", output.display());
}
