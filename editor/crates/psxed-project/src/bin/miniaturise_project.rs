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
//! # Acceptance test
//!
//! Cook the original, cook the copy, diff `generated/`. The copy is only
//! shippable when that diff is empty and the cook logs no placeholder or
//! missing-source warnings. cortex_v1 passes: 566 source assets down to 44,
//! 295 MB down to 1.0 MB, byte-identical cooked output.
//!
//! Getting there needed three asset classes a room-driven walk never reaches --
//! UI image textures, UI SFX and CD-DA, all cooked in `playtest/cook_ui.rs` --
//! and two traps worth remembering. A CD-DA key is an asset IDENTITY, not a
//! path: it may be absolute and carries a `#<loop-point>` fragment. And a
//! source that fails to resolve must be reported, never folded in with
//! procedurally generated materials, or the counters read healthy while the
//! copy quietly loses files.
//!

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
            eprintln!(
                "[miniaturise] {}: parse failed: {error}",
                project_path.display()
            );
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
        .chain(package.used_ui_paths.iter())
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
    let mut missing = 0usize;
    for key in &used {
        // A CD-DA key carries a `#<loop-point>` fragment and may be absolute,
        // so the map key is an asset IDENTITY rather than a path. Recover the
        // file, then re-derive a project-relative destination so the copy keeps
        // the layout `project.ron` expects.
        // A generated material's key is its descriptor, not a path. The cook
        // rebuilds it from `project.ron`, so there is nothing to copy.
        if key.starts_with('@') {
            generated += 1;
            continue;
        }
        let path_part = key.split('#').next().unwrap_or(key);
        let source = if Path::new(path_part).is_absolute() {
            PathBuf::from(path_part)
        } else {
            project_dir.join(path_part)
        };
        // The destination MUST stay inside `out_dir`. `Path::join` silently
        // discards its base when given an absolute path, so a failed
        // strip_prefix here would write over the source tree instead of the
        // copy. Compare canonical forms, and refuse rather than escape.
        let relative_owned = match (source.canonicalize(), project_dir.canonicalize()) {
            (Ok(abs_source), Ok(abs_root)) => abs_source
                .strip_prefix(&abs_root)
                .ok()
                .map(|rel| rel.to_path_buf()),
            _ => None,
        };
        let Some(relative) = relative_owned else {
            eprintln!("[miniaturise] REFUSED '{path_part}': outside the project directory");
            missing += 1;
            continue;
        };
        let relative = relative.as_path();
        // The cook's texture map is keyed by texture IDENTITY, not always a
        // path: a procedurally generated material's key is its descriptor, and
        // cortex_v5's art is mostly generated. Those have no source file -- the
        // cook rebuilds them from the descriptor in `project.ron` -- so they
        // need nothing copied and must not be reported as missing.
        if !source.is_file() {
            // A procedurally generated material's key is its descriptor, not a
            // path, and the cook rebuilds it from `project.ron`, so it needs no
            // file. Anything that LOOKS like a path but does not resolve is a
            // real miss and must be loud: silently folding the two together is
            // how CD-DA and SFX `.wav`s went missing while the counter still
            // read healthy.
            if path_part.contains('.') && !path_part.starts_with('@') {
                eprintln!("[miniaturise] MISSING source for '{path_part}'");
                missing += 1;
            } else {
                generated += 1;
            }
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
    if missing > 0 {
        eprintln!(
            "[miniaturise] {missing} source file(s) could not be resolved; \
             the copy is INCOMPLETE"
        );
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
            } else if path.extension().is_some_and(|ext| {
                ext == "psxt" || ext == "psxmdl" || ext == "psxanim" || ext == "wav"
            }) {
                *count += 1;
            }
        }
    }
    let mut count = 0;
    walk(&project_dir.join("assets"), &mut count);
    count
}
