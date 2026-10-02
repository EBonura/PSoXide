//! `site player`: build the slim web player bundle from a pinned
//! PSoXide-emulator checkout, for the site to embed with `?embed=1`.
//!
//! This is the emulator's `tools/build-web-player.py` (at the revision the
//! site pins) run without Python: the same Trunk build, the same pruning,
//! notices and build record, so the bundle is byte-identical to that
//! script's. The notices keep naming that script as the source tree's own
//! build command, which is still how that tree documents its build.
//!
//! The output directory gets index.html, the JS glue and wasm, Trunk's
//! snippets/, the favicon, and `psoxide-player-build.json` (emulator
//! revision, tool versions and a SHA-256 per file). Needs rustup, Trunk
//! 0.21.14 and git; a cold run fetches the emulator's locked SDK sources
//! from GitHub.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use psoxide_link::sha256::sha256_hex;
use regex::Regex;

use super::{output, run, write, Args};
use crate::obj;
use crate::pyjson::{dumps, Json};

const TRUNK_VERSION: &str = "0.21.14";
/// Same flags as the public itch.io build. RUSTFLAGS replaces the simd128
/// flag from .cargo/config.toml, so it is repeated here.
const RUSTFLAGS: &str = "-C target-feature=+simd128 -C link-arg=-zstack-size=16777216";
const RECORD: &str = "psoxide-player-build.json";
/// Copied into Trunk's output for the full web page; the player does not
/// use it.
const NOT_SHIPPED: &[&str] = &["examples"];

/// `Path.read_text(errors="replace")`.
fn read_lossy(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn files_below(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            files_below(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn show(command: &Command) {
    let mut line = vec![command.get_program().to_string_lossy().into_owned()];
    line.extend(command.get_args().map(|a| a.to_string_lossy().into_owned()));
    println!("+ {}", line.join(" "));
}

/// Build the bundle into `--out` (new or empty) from `--emulator`.
pub fn main(args: &[String]) -> Result<(), String> {
    let args = Args::parse(args, &[])?;
    let root = PathBuf::from(args.need("emulator")?);
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    let frontend = root.join("emu/crates/frontend");
    let out = PathBuf::from(args.need("out")?);
    if out.exists()
        && (!out.is_dir()
            || fs::read_dir(&out)
                .map_err(|e| e.to_string())?
                .next()
                .is_some())
    {
        return Err(format!(
            "{} must be a new or empty directory",
            out.display()
        ));
    }
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;

    let trunk = output(Command::new("trunk").arg("--version")).map_err(|_| {
        format!("trunk not found; install {TRUNK_VERSION}: cargo install trunk --version {TRUNK_VERSION} --locked")
    })?;
    if trunk.split_whitespace().last() != Some(TRUNK_VERSION) {
        return Err(format!(
            "found {trunk}, need trunk {TRUNK_VERSION}: cargo install trunk --version {TRUNK_VERSION} --locked"
        ));
    }

    // Run from the emulator root so rustup resolves its pinned toolchain.
    let mut target = Command::new("rustup");
    target
        .current_dir(&root)
        .args(["target", "add", "wasm32-unknown-unknown"]);
    show(&target);
    run(&mut target)?;
    psoxide_link::components::materialize(&root, &Default::default(), false, None)
        .map_err(|e| format!("emulator components: {e}"))?;
    let mut build = Command::new("trunk");
    // trunk rejects NO_COLOR=1 ("invalid value for --no-color").
    build
        .current_dir(&frontend)
        .env_remove("NO_COLOR")
        .env("RUSTFLAGS", RUSTFLAGS)
        .args([
            "build",
            "--release",
            "--locked",
            "--public-url",
            "./",
            "--dist",
        ])
        .arg(&out);
    show(&build);
    run(&mut build)?;

    for name in NOT_SHIPPED {
        let path = out.join(name);
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| e.to_string())?;
        }
    }
    // Ship the project licence, bundled-font notices and dependency licence
    // texts alongside the binary. Source and locked build instructions are
    // linked by exact revision in both the manifest and the notices.
    let revision = output(
        Command::new("git")
            .current_dir(&root)
            .args(["rev-parse", "HEAD"]),
    )?;
    let metadata: serde_json::Value =
        serde_json::from_str(&output(Command::new("cargo").current_dir(&root).args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            "wasm32-unknown-unknown",
        ]))?)
        .map_err(|e| e.to_string())?;
    let mut notices = vec![
        "PSoXide web player\nGPL-2.0-or-later\n".to_string(),
        format!("Source: https://github.com/EBonura/PSoXide-emulator/tree/{revision}\n"),
        "Build: python3 tools/build-web-player.py --out player\n".to_string(),
        "Locked SDK sources: see components.lock.json and tools/bootstrap-components.py in that source tree.\n".to_string(),
        read_lossy(&root.join("LICENSE"))?,
    ];
    let mut packages: Vec<&serde_json::Value> = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
        .iter()
        .collect();
    packages.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    for package in packages {
        if package["source"].is_null() {
            continue;
        }
        let field = |key: &str| package[key].as_str().filter(|s| !s.is_empty());
        let base = Path::new(field("manifest_path").unwrap_or_default())
            .parent()
            .ok_or("package without a manifest directory")?
            .to_path_buf();
        notices.push(format!(
            "\n===== {} {} ({}) =====\n",
            field("name").unwrap_or_default(),
            field("version").unwrap_or_default(),
            field("license").unwrap_or("see source")
        ));
        notices.push(format!(
            "Source: {}\n",
            field("repository")
                .or_else(|| field("source"))
                .unwrap_or("None")
        ));
        let mut candidates: BTreeSet<PathBuf> = fs::read_dir(&base)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                let upper = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_uppercase();
                ["LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"]
                    .iter()
                    .any(|prefix| upper.starts_with(prefix))
            })
            .collect();
        if let Some(file) = field("license_file") {
            candidates.insert(base.join(file));
        }
        for path in candidates {
            notices.push(format!(
                "{}\n{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                read_lossy(&path)?
            ));
        }
    }
    let mut fonts: Vec<PathBuf> = fs::read_dir(frontend.join("assets/fonts"))
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .collect();
    fonts.sort();
    for path in fonts {
        if path.is_file() && path.extension().is_none_or(|e| e != "ttf") {
            notices.push(format!(
                "\n===== Bundled font notice: {} =====\n{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                read_lossy(&path)?
            ));
        }
    }
    write(&out.join("THIRD-PARTY-NOTICES.txt"), notices.join("\n"))?;

    let mut files = Vec::new();
    files_below(&out, &mut files)?;
    files.sort();
    let names: Vec<String> = files
        .iter()
        .map(|p| {
            p.strip_prefix(&out)
                .expect("below out")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    if !names.iter().any(|n| n == "index.html") || !names.iter().any(|n| n.ends_with("_bg.wasm")) {
        return Err("trunk output is missing index.html or the wasm".into());
    }
    let stray: Vec<&String> = names
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            [".exe", ".bin", ".cue", ".flac", ".gz"]
                .iter()
                .any(|s| lower.ends_with(s))
                || n.contains("manifest")
        })
        .collect();
    if !stray.is_empty() {
        return Err(format!(
            "unexpected program or disc files in the player bundle: {stray:?}"
        ));
    }

    let wasm_opt = Regex::new(r#"(?m)^wasm_opt\s*=\s*"([^"]+)""#)
        .expect("valid regex")
        .captures(&read_lossy(&frontend.join("Trunk.toml"))?)
        .map(|c| c[1].to_string());
    let wasm_bindgen = Regex::new(r#"\[\[package\]\]\nname = "wasm-bindgen"\nversion = "([^"]+)""#)
        .expect("valid regex")
        .captures(&read_lossy(&root.join("Cargo.lock"))?)
        .map(|c| c[1].to_string());
    let dirty = output(Command::new("git").current_dir(&root).args([
        "status",
        "--porcelain",
        "--untracked-files=no",
    ]))?;
    let mut entries = Vec::new();
    let mut total = 0u64;
    for (name, path) in names.iter().zip(&files) {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        total += bytes.len() as u64;
        entries.push((
            name.clone(),
            obj! {"bytes" => bytes.len(), "sha256" => sha256_hex(&bytes)},
        ));
    }
    let lock = fs::read(root.join("components.lock.json")).map_err(|e| e.to_string())?;
    let record = obj! {
        "schema" => 1i64,
        "emulator_revision" => revision.clone(),
        "source_url" => format!("https://github.com/EBonura/PSoXide-emulator/tree/{revision}"),
        "emulator_dirty" => !dirty.is_empty(),
        "components_lock_sha256" => sha256_hex(&lock),
        "rustc" => output(Command::new("rustc").current_dir(&root).arg("-V"))?,
        "trunk" => trunk,
        "wasm_bindgen" => wasm_bindgen,
        "wasm_opt" => wasm_opt,
        "rustflags" => RUSTFLAGS,
        "files" => Json::Obj(entries),
        "total_bytes" => total,
    };
    write(&out.join(RECORD), dumps(&record, Some(2), true) + "\n")?;
    if !dirty.is_empty() {
        eprintln!(
            "warning: the emulator checkout has uncommitted changes (recorded as emulator_dirty)"
        );
    }
    println!(
        "Player bundle: {} ({} files, {total} bytes)",
        out.display(),
        files.len()
    );
    Ok(())
}
