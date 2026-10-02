//! `site sdk-docs`: publish searchable rustdoc for the PS1 target and the
//! host GTE backend under `static/api/`.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use regex::Regex;

use super::{pins, read_text, require_pinned, run, site_root, text, write, Args};

/// `html.escape(text)` with its default `quote=True`.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// `shutil.copytree`: files keep their modes and modification times.
pub fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|e| format!("{}: {e}", destination.display()))?;
    for entry in fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// `shutil.copy2`: contents, mode and modification time.
pub fn copy_file(source: &Path, target: &Path) -> Result<(), String> {
    fs::copy(source, target).map_err(|e| format!("{}: {e}", source.display()))?;
    let modified = fs::metadata(source)
        .and_then(|m| m.modified())
        .map_err(|e| e.to_string())?;
    fs::OpenOptions::new()
        .write(true)
        .open(target)
        .and_then(|f| f.set_times(fs::FileTimes::new().set_modified(modified)))
        .map_err(|e| format!("{}: {e}", target.display()))
}

/// Every `*.html` below `root`.
fn html_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            html_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

/// Lexically resolve `..` and `.` (the paths here never cross symlinks).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Build both rustdoc sets and stage them under `static/api/`.
pub fn main(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args, &[])?;
    let repo = PathBuf::from(args.need("sdk-root")?);
    let repo = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    let target = PathBuf::from(args.need("target-dir")?);
    fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    let target = target.canonicalize().map_err(|e| e.to_string())?;
    let base_url = args
        .get("base-url")
        .unwrap_or("https://ebonura.github.io/PSoXide");
    let pins = pins()?;
    let revision = text(&pins, "sdk_revision")?;
    require_pinned(&repo, revision, &["sdk", "crates"])?;
    for generated in [
        target.join("ps1/mipsel-sony-psx/doc"),
        target.join("host/doc"),
    ] {
        if generated.exists() {
            fs::remove_dir_all(&generated).map_err(|e| e.to_string())?;
        }
    }
    // Keep a route back to the guides above rustdoc's own navigation.
    let scratch = std::env::temp_dir().join(format!("sdk-rustdoc-{}", std::process::id()));
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let banner = scratch.join("navigation.html");
    write(
        &banner,
        format!(
            "<div style=\"padding:12px 24px;border-bottom:1px solid #888\"><a href=\"{}/docs/sdk/\">\u{2190} PSoXide SDK guides and examples</a> \u{b7} SDK {} \u{b7} All Cargo features enabled; check feature gates before use.</div>",
            html_escape(base_url.trim_end_matches('/')),
            &revision[..12.min(revision.len())]
        ),
    )?;
    let flags = format!("--html-before-content {}", banner.display());
    let cargo_doc = |extra: &[&str]| -> Result<(), String> {
        run(Command::new("cargo")
            .current_dir(&repo)
            .env("RUSTDOCFLAGS", &flags)
            .args(["doc", "--locked", "--no-deps"])
            .args(extra))
    };
    let ps1 = target.join("ps1");
    let host = target.join("host");
    let built = (|| {
        cargo_doc(&[
            "--manifest-path",
            "sdk/Cargo.toml",
            "--workspace",
            "--all-features",
            "--target",
            "mipsel-sony-psx",
            "-Z",
            "build-std=core,alloc",
            "--target-dir",
            &ps1.to_string_lossy(),
        ])?;
        cargo_doc(&[
            "-p",
            "psx-hw",
            "-p",
            "psxed-format",
            "--target",
            "mipsel-sony-psx",
            "-Z",
            "build-std=core,alloc",
            "--target-dir",
            &ps1.to_string_lossy(),
        ])?;
        cargo_doc(&[
            "--manifest-path",
            "sdk/Cargo.toml",
            "--all-features",
            "-p",
            "psx-gte",
            "-p",
            "psx-gte-core",
            "-p",
            "psx-math",
            "--target-dir",
            &host.to_string_lossy(),
        ])
    })();
    let _ = fs::remove_dir_all(&scratch);
    built?;

    let output = site_root().join("static/api");
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    let scripts = Regex::new(r#"<script src="([^"]*trait\.impl/[^"]+\.js)""#).expect("valid regex");
    for (name, source) in [
        ("ps1", ps1.join("mipsel-sony-psx/doc")),
        ("host", host.join("doc")),
    ] {
        let destination = output.join(name);
        if destination.exists() {
            // Only this task's generated rustdoc output.
            fs::remove_dir_all(&destination).map_err(|e| e.to_string())?;
        }
        copy_tree(&source, &destination)?;
        // Cargo does not create a root index without --enable-index-page.
        // Supply one so rustdoc's Help/Settings breadcrumb has a real
        // destination.
        let mut entries: Vec<String> = fs::read_dir(&destination)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| e.path().join("index.html").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("psx_") || n.starts_with("psxed_"))
            .collect();
        entries.sort();
        let list: String = entries
            .iter()
            .map(|c| format!("<li><a href=\"{c}/index.html\">{c}</a></li>"))
            .collect();
        write(
            &destination.join("index.html"),
            format!(
                "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>PSoXide API reference</title><h1>PSoXide API reference</h1><p><a href=\"../../docs/sdk/\">SDK guides and examples</a></p><ul>{list}</ul></html>"
            ),
        )?;
        // Correct two upstream documentation links without changing the
        // pinned SDK source. Ord links come from core's inherited iterator
        // docs.
        let mut documents = Vec::new();
        html_files(&destination, &mut documents);
        for document in &documents {
            let mut content = read_text(document)?.replace(
                "href=\"Ord#lexicographical-comparison\"",
                "href=\"https://doc.rust-lang.org/core/cmp/trait.Ord.html#lexicographical-comparison\"",
            );
            if document.strip_prefix(&destination).ok() == Some(Path::new("psx_mc/sio/index.html"))
            {
                content =
                    content.replace("href=\"../psx_pad\"", "href=\"../../psx_pad/index.html\"");
            }
            write(document, content)?;
        }
        // rustdoc emits an async implementor script reference even for a
        // trait with no emitted implementations. Supply its empty registry.
        for document in &documents {
            let content = read_text(document)?;
            for found in scripts.captures_iter(&content) {
                let script = lexical(
                    &document
                        .parent()
                        .expect("file has a directory")
                        .join(&found[1]),
                );
                if !script.starts_with(&destination) {
                    return Err("Implementor script escapes rustdoc output".into());
                }
                if !script.exists() {
                    fs::create_dir_all(script.parent().expect("script has a directory"))
                        .map_err(|e| e.to_string())?;
                    write(
                        &script,
                        "if(window.register_implementors){window.register_implementors({});}else{window.pending_implementors={};}\n",
                    )?;
                }
            }
        }
    }
    fs::copy(repo.join("LICENSE"), output.join("LICENSE.txt")).map_err(|e| e.to_string())?;
    println!("Staged PS1 API and host GTE API under static/api/");
    Ok(())
}
