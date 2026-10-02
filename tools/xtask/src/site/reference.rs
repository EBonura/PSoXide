//! `site import-sdk-reference`: crate structure and complete example sources
//! from the player's SDK pin, as `data/sdk-reference.json`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

use super::{output, pins, read_text, read_toml, require_pinned, site_root, text, write, Args};
use crate::mfc0::splitlines;
use crate::obj;
use crate::pyjson::{dumps, Json};

/// `sorted(directory.glob(pattern_dir + '/**/*.rs'))`: every `.rs` file at
/// any depth below `directory`, sorted part by part.
fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(directory, &mut out);
    out.sort();
    out
}

fn posix(path: &Path, base: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(base)
        .map_err(|_| format!("{} is outside {}", path.display(), base.display()))?
        .to_string_lossy()
        .into_owned())
}

fn dependencies(manifest: &toml::Table, sdk_crates: &BTreeSet<String>) -> Vec<Json> {
    let empty = toml::Table::new();
    let mut tables: Vec<(String, &toml::Table)> = vec![(
        "all targets".into(),
        manifest
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .unwrap_or(&empty),
    )];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (target, value) in targets {
            tables.push((
                target.clone(),
                value
                    .get("dependencies")
                    .and_then(toml::Value::as_table)
                    .unwrap_or(&empty),
            ));
        }
    }
    let mut result = Vec::new();
    for (target, deps) in tables {
        let mut sorted: Vec<_> = deps.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(b.0));
        for (name, spec) in sorted {
            let field = |key: &str| spec.as_table().and_then(|t| t.get(key));
            result.push(obj! {
                "name" => name.as_str(),
                "target" => target.as_str(),
                "optional" => field("optional").map_or(Json::Bool(false), Json::from_toml),
                "features" => field("features").map_or(Json::Arr(vec![]), Json::from_toml),
                "sdk" => sdk_crates.contains(name),
            });
        }
    }
    result
}

fn package_field(manifest: &toml::Table, key: &str) -> Result<Json, String> {
    manifest
        .get("package")
        .and_then(|p| p.get(key))
        .map(Json::from_toml)
        .ok_or_else(|| format!("manifest has no package.{key}"))
}

/// The guide page each crate and example needs: (`crates`|`examples`, name).
type Guides = Vec<(&'static str, String)>;

/// Build the reference from the SDK checkout at `repo`.
fn generate(repo: &Path) -> Result<(Json, Guides), String> {
    let pins = pins()?;
    let revision = text(&pins, "sdk_revision")?.to_string();
    // Do not label a different source tree with the published example revision.
    require_pinned(repo, &revision, &["sdk", "crates"])?;
    let tracked: BTreeSet<String> = output(Command::new("git").current_dir(repo).args([
        "ls-tree",
        "-r",
        "--name-only",
        &revision,
        "sdk",
    ]))?
    .lines()
    .map(str::to_string)
    .collect();
    let pinned = |path: &Path, what: &str| -> Result<(), String> {
        let relative = posix(path, repo)?;
        if tracked.contains(&relative) {
            Ok(())
        } else {
            Err(format!("Unpinned {what}: {relative}"))
        }
    };
    let workspace = read_toml(&repo.join("sdk/Cargo.toml"))?;
    let members: Vec<String> = workspace
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or("sdk/Cargo.toml has no workspace.members")?
        .iter()
        .filter_map(|m| m.as_str().map(str::to_string))
        .collect();
    let sdk_crates: BTreeSet<String> = members
        .iter()
        .filter_map(|m| {
            Path::new(m)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .collect();
    let docs = Regex::new(r"(?m)^//! ?(.*)$").expect("valid regex");
    let rustdoc_link = Regex::new(r"\[(`[^`]+`)\]").expect("valid regex");

    let mut guides = Vec::new();
    let mut crates = Vec::new();
    for member in &members {
        let directory = repo.join("sdk").join(member);
        let manifest = read_toml(&directory.join("Cargo.toml"))?;
        let mut layout = vec![directory.join("Cargo.toml")];
        if directory.join("README.md").exists() {
            layout.push(directory.join("README.md"));
        }
        layout.extend(rust_sources(&directory.join("src")));
        layout.extend(rust_sources(&directory.join("tests")));
        let mut files = Vec::new();
        for path in layout {
            pinned(&path, "source")?;
            let source = read_text(&path)?;
            let summary = docs
                .captures_iter(&source)
                .map(|c| c[1].to_string())
                .find(|line| !crate::mfc0::py_strip(line).is_empty())
                .unwrap_or_default();
            // Crate overviews link to the real API reference, not unresolved
            // rustdoc shortcuts.
            let summary = rustdoc_link.replace_all(&summary, "$1").into_owned();
            files.push(obj! {"path" => posix(&path, &directory)?, "summary" => summary});
        }
        let name = package_field(&manifest, "name")?;
        if let Json::Str(name) = &name {
            guides.push(("crates", name.clone()));
        }
        crates.push(obj! {
            "name" => name,
            "description" => package_field(&manifest, "description")?,
            "features" => manifest.get("features").map_or(Json::Obj(vec![]), Json::from_toml),
            "dependencies" => dependencies(&manifest, &sdk_crates),
            "files" => Json::Arr(files),
        });
    }

    let mut manifests: Vec<PathBuf> = fs::read_dir(repo.join("sdk/examples"))
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path().join("Cargo.toml"))
        .filter(|p| p.is_file())
        .collect();
    manifests.sort();
    let mut examples = Vec::new();
    for path in manifests {
        if !tracked.contains(&posix(&path, repo)?) {
            return Err(format!("Unpinned manifest: {}", path.display()));
        }
        let manifest = read_toml(&path)?;
        let directory = path
            .parent()
            .expect("manifest has a directory")
            .to_path_buf();
        let mut sources = vec![path.clone()];
        sources.extend(rust_sources(&directory.join("src")));
        let mut files = Vec::new();
        for source in &sources {
            if !tracked.contains(&posix(source, repo)?) {
                return Err(format!("Unpinned example source: {}", source.display()));
            }
            let code = read_text(source)?;
            files.push(obj! {
                "path" => posix(source, &directory)?,
                "language" => if *source == path { "toml" } else { "rust" },
                "lines" => splitlines(&code).len(),
                "code" => code,
            });
        }
        let name = package_field(&manifest, "name")?;
        if let Json::Str(name) = &name {
            guides.push(("examples", name.clone()));
        }
        examples.push(obj! {
            "name" => name,
            "dependencies" => dependencies(&manifest, &sdk_crates),
            "files" => Json::Arr(files),
        });
    }
    Ok((
        obj! {"revision" => revision, "crates" => Json::Arr(crates), "examples" => Json::Arr(examples)},
        guides,
    ))
}

/// Write (or with `--check`, verify) `data/sdk-reference.json`, then require
/// a guide page for every crate and example.
pub fn main(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args, &["check"])?;
    let site = site_root();
    let repo = args
        .get("sdk-root")
        .map_or_else(|| site.join(".."), PathBuf::from);
    let repo = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    let (data, guides) = generate(&repo)?;
    let text = dumps(&data, Some(2), false) + "\n";
    let destination = site.join("data/sdk-reference.json");
    if args.flag("check") {
        if !destination.exists() || read_text(&destination)? != text {
            return Err(
                "SDK reference is stale; run `cargo run -p xtask -- site import-sdk-reference`"
                    .into(),
            );
        }
    } else {
        write(&destination, &text)?;
    }
    for (kind, name) in &guides {
        if !site
            .join("content/docs")
            .join(kind)
            .join(format!("{name}.md"))
            .is_file()
        {
            return Err(format!("Missing {kind} guide: {name}"));
        }
    }
    let count = |key| match data.get(key) {
        Some(Json::Arr(items)) => items.len(),
        _ => 0,
    };
    println!(
        "{} crate structures; {} complete examples",
        count("crates"),
        count("examples")
    );
    Ok(())
}
