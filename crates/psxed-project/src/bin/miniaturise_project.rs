//! Copy a project, keeping only the assets its level actually reaches.
//!
//! A project's `assets/` directory is a library rather than a working set.
//! cortex_v5 carries 704 `.psxt` textures and its level reaches three of them,
//! so shipping the directory wholesale costs 7.3 MB to deliver a few kilobytes
//! of used art.
//!
//! Reachability comes from the cook itself -- `PlaytestPackage::used_texture_paths`
//! is built from the same map the cooker uses to dedupe texture references -- so
//! this cannot ship a project that fails to cook, and cannot disagree with what
//! the cook decided was needed.
//!
//! ```text
//! miniaturise-project projects/cortex_v5/project.ron /tmp/cortex_v5_slim
//! ```
//!
//! # INCOMPLETE -- do not ship its output yet
//!
//! Only textures reached through room materials, model meshes, model atlases
//! and animation clips are copied. A round-trip on cortex_v1 -- cook the
//! original, cook the copy, diff `generated/` -- showed three asset classes are
//! missed entirely:
//!
//! - **UI images.** The menu art cooks as PLACEHOLDERS, with warnings like
//!   `failed to read texture 'BONNIE_STUDIOS_LOGO' ... using placeholder`.
//! - **CD-DA tracks.** `track02.cdda` is absent from the copy.
//! - **UI SFX.** `ui_sfx_000.psau` is absent.
//!
//! A shipped copy would therefore boot to a broken menu with no audio. These
//! are project-global assets rather than per-room reachable ones, so they do
//! not need reachability analysis -- copying all of them is correct.
//!
//! All three live in `playtest/cook_ui.rs`, which this tool never traced:
//!
//! | Asset | Site | Note |
//! |---|---|---|
//! | CD-DA music | `cook_ui.rs:971` | `.wav` source via `resolve_path(trimmed, project_root)` |
//! | UI SFX | `cook_ui.rs:1114` | `.wav` source, same resolution |
//! | UI image textures | same module | reported by `"...is missing; using placeholder"` at `cook_ui.rs:770` |
//!
//! Collect those paths the way textures and models are collected -- from the
//! cook rather than a second traversal -- add them to the used set, and re-run
//! the round-trip until the diff is empty.
//!
//! The round-trip diff is the acceptance test for this tool. Until it comes
//! back clean, treat the output as a size experiment, not a shippable project.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(project_arg), Some(out_arg)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: miniaturise-project <project.ron> <out-dir>\n\
             \n\
             Copies the project and only the source assets its level reaches."
        );
        std::process::exit(2);
    };
    let project_path = PathBuf::from(&project_arg);
    let out_dir = PathBuf::from(&out_arg);

    let project_dir = match project_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };

    let text = match std::fs::read_to_string(&project_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("[miniaturise] {}: {error}", project_path.display());
            std::process::exit(2);
        }
    };
    let project = match psxed_project::ProjectDocument::from_ron_str(&text) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("[miniaturise] {}: parse failed: {error}", project_path.display());
            std::process::exit(2);
        }
    };

    // Cook purely to learn what the level reaches. A project that cannot cook
    // must not be shipped, so its errors are fatal here too.
    let (package, report) = psxed_project::playtest::build_package(&project, &project_dir);
    for error in &report.errors {
        eprintln!("[miniaturise] {error}");
    }
    let Some(package) = package else {
        eprintln!("[miniaturise] project does not cook; nothing written");
        std::process::exit(1);
    };

    let used: BTreeSet<&str> = package
        .used_texture_paths
        .iter()
        .chain(package.used_model_paths.iter())
        .map(String::as_str)
        .collect();

    if let Err(error) = std::fs::create_dir_all(&out_dir) {
        eprintln!("[miniaturise] mkdir {}: {error}", out_dir.display());
        std::process::exit(1);
    }

    // The project file keeps its asset paths, so the copied tree must preserve
    // the same relative layout for those paths to resolve.
    let mut copied = 0usize;
    let mut copied_bytes = 0u64;
    let mut generated = 0usize;
    for relative in &used {
        let source = project_dir.join(relative);
        // The cook's texture map is keyed by texture IDENTITY, not always a
        // path: a procedurally generated material's key is its descriptor, and
        // cortex_v5's art is mostly generated. Those have no source file -- the
        // cook rebuilds them from the descriptor in `project.ron` -- so they
        // need nothing copied and must not be reported as missing.
        if !source.is_file() {
            generated += 1;
            continue;
        }
        let destination = out_dir.join(relative);
        if let Some(parent) = destination.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!("[miniaturise] mkdir {}: {error}", parent.display());
                std::process::exit(1);
            }
        }
        match std::fs::copy(&source, &destination) {
            Ok(bytes) => {
                copied += 1;
                copied_bytes += bytes;
            }
            Err(error) => {
                eprintln!("[miniaturise] copy {}: {error}", source.display());
                std::process::exit(1);
            }
        }
    }

    let project_file_name = project_path
        .file_name()
        .map(Path::new)
        .unwrap_or_else(|| Path::new("project.ron"));
    if let Err(error) = std::fs::copy(&project_path, out_dir.join(project_file_name)) {
        eprintln!("[miniaturise] copy project file: {error}");
        std::process::exit(1);
    }

    let available = count_source_assets(&project_dir);
    println!(
        "[miniaturise] kept {copied} of {available} source assets ({} KiB)",
        copied_bytes / 1024
    );
    if generated > 0 {
        println!("[miniaturise] {generated} generated texture(s) need no source file");
    }
    println!("[miniaturise] wrote → {}", out_dir.display());
    if available > copied {
        println!(
            "[miniaturise] {} unused asset(s) left behind",
            available - copied
        );
    }
}

/// Count source assets under the project, for the kept-of-total report.
fn count_source_assets(project_dir: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, count);
            } else if path
                .extension()
                .is_some_and(|ext| ext == "psxt" || ext == "psxmdl" || ext == "psxanim")
            {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(&project_dir.join("assets"), &mut count);
    count
}
