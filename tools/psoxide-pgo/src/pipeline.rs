//! The shared PGO build: `collect`, `apply` and `choose`.
//!
//! Every game used to carry its own copy of these steps (hl-psx's
//! `hl-build pgo`, VoXide's `make pgo`). They differ only in how the guest
//! is built and packed, which tape drives it, and where the emulator is, so
//! those are the inputs and the rest lives here. See README.md.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs};

use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};
use object::{Object, ObjectSection};

use crate::Result;

/// The only guest target.
const TARGET: &str = "mipsel-sony-psx";

/// Line tables with discriminators, kept through the link. The flat image
/// drops them, so a build with these runs and measures like a plain one.
const COLLECT_FLAGS: [&str; 3] = [
    "-Cdebuginfo=1",
    "-Zdebug-info-for-profiling",
    "-Cstrip=none",
];

/// Set while linking the ELF twin. A guest whose build.rs adds
/// `--oformat=binary` must leave it out when this is set; flags given
/// through rustflags need nothing, because the twin's later
/// `--oformat=elf` wins.
const LINK_ELF_ENV: &str = "PSOXIDE_LINK_ELF";

/// A prime, so the sampler cannot fall into step with a loop.
const SAMPLE_INTERVAL: &str = "61";

/// Cap for tape replays, which stop on their own when the tape runs out.
const REPLAY_STEPS: &str = "40000000000";

pub const USAGE: &str = "\
       psoxide-pgo collect [GUEST] --frontend PATH [--tape PATH]... [--launch-arg ARG]...
                           [--pack CMD] --out PROFILE -- CARGO-ARGS...
       psoxide-pgo apply   [GUEST] [--profile PROFILE] [--variant V] -- CARGO-ARGS...
       psoxide-pgo choose  [GUEST] --profile PROFILE --gate CMD [--variant V]... [--pack CMD]
                           -- CARGO-ARGS...
  GUEST: [--crate DIR] [--work DIR] [--patcher PATH] [--scanner PATH]
  CARGO-ARGS: what follows `cargo` in the guest's own build, starting with `build`
  V: off | default | accurate | hot=N, joined with + (accurate+hot=1000)";

/// How to build one guest, shared by every mode.
struct Guest {
    crate_dir: PathBuf,
    cargo: Vec<String>,
    work: Option<PathBuf>,
    patcher: PathBuf,
    scanner: PathBuf,
}

#[derive(Default)]
struct Options {
    crate_dir: Option<PathBuf>,
    work: Option<PathBuf>,
    patcher: Option<PathBuf>,
    scanner: Option<PathBuf>,
    frontend: Option<PathBuf>,
    tapes: Vec<PathBuf>,
    launch_args: Vec<String>,
    pack: Option<String>,
    out: Option<PathBuf>,
    profile: Option<PathBuf>,
    variants: Vec<String>,
    gate: Option<String>,
    cargo: Vec<String>,
}

fn parse(args: &[String]) -> Result<Options> {
    let mut options = Options::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            options.cargo = iter.by_ref().cloned().collect();
            break;
        }
        let mut value = || -> Result<String> {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value").into())
        };
        let path = |text: String| -> Result<PathBuf> { Ok(std::path::absolute(text)?) };
        match arg.as_str() {
            "--crate" => options.crate_dir = Some(path(value()?)?),
            "--work" => options.work = Some(path(value()?)?),
            "--patcher" => options.patcher = Some(path(value()?)?),
            "--scanner" => options.scanner = Some(path(value()?)?),
            "--frontend" => options.frontend = Some(path(value()?)?),
            "--tape" => options.tapes.push(path(value()?)?),
            "--launch-arg" => options.launch_args.push(value()?),
            "--pack" => options.pack = Some(value()?),
            "--out" => options.out = Some(path(value()?)?),
            "--profile" => options.profile = Some(path(value()?)?),
            "--variant" => options.variants.push(value()?),
            "--gate" => options.gate = Some(value()?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if options.cargo.first().map(String::as_str) != Some("build") {
        return Err("give the guest's cargo arguments after --, starting with `build`".into());
    }
    Ok(options)
}

/// Run `collect`, `apply` or `choose`.
pub fn main(mode: &str, args: &[String]) -> Result<()> {
    let mut options = parse(args)?;
    let tools = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let guest = Guest {
        crate_dir: match options.crate_dir.take() {
            Some(dir) => dir,
            None => env::current_dir()?,
        },
        cargo: std::mem::take(&mut options.cargo),
        work: options.work.take(),
        patcher: options
            .patcher
            .take()
            .unwrap_or_else(|| tools.join("hazard_patch.py")),
        scanner: options
            .scanner
            .take()
            .unwrap_or_else(|| tools.join("hazard_scan.py")),
    };
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        if env::var_os(name).is_some_and(|value| !value.is_empty()) {
            return Err(format!(
                "{name} is set. It replaces every config rustflags list, including the ones \
                 this passes with --config; put the guest's flags in \
                 [target.{TARGET}] rustflags or pass them with --config instead"
            )
            .into());
        }
    }
    match mode {
        "collect" => collect(&guest, &options),
        "apply" => {
            let variant = match options.variants.as_slice() {
                [] => "default",
                [one] => one.as_str(),
                _ => return Err("apply takes one --variant".into()),
            };
            let exe = apply(&guest, options.profile.as_deref(), variant)?;
            println!("psoxide-pgo: {variant} -> {}", exe.display());
            Ok(())
        }
        "choose" => choose(&guest, &options),
        _ => unreachable!("main only dispatches pipeline modes"),
    }
}

/// Extra rustflags for one variant, or `None` for `off`.
fn variant_flags(variant: &str) -> Result<Option<Vec<String>>> {
    if variant == "off" {
        return Ok(None);
    }
    let mut flags = Vec::new();
    for part in variant.split('+') {
        match part {
            "default" => {}
            "accurate" => flags.push("-Cllvm-args=-profile-sample-accurate".to_string()),
            _ => match part.strip_prefix("hot=").map(str::parse::<u32>) {
                Some(Ok(threshold)) => {
                    flags.push(format!("-Cllvm-args=-hot-callsite-threshold={threshold}"))
                }
                _ => return Err(format!("unknown variant part {part:?}").into()),
            },
        }
    }
    Ok(Some(flags))
}

/// A TOML basic string.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{what} failed ({status})").into());
    }
    Ok(())
}

impl Guest {
    /// Build with `flags` appended to the guest's own rustflags and return
    /// the executable cargo reports.
    fn build(&self, flags: &[String], elf: bool) -> Result<PathBuf> {
        let mut flags = flags.to_vec();
        let mut command = Command::new("cargo");
        command.current_dir(&self.crate_dir).args(&self.cargo);
        if elf {
            flags.push("-Clink-arg=--oformat=elf".to_string());
            command.env(LINK_ELF_ENV, "1");
        } else {
            command.env_remove(LINK_ELF_ENV);
        }
        if !flags.is_empty() {
            // `--config` appends to the rustflags in the guest's
            // .cargo/config.toml, where RUSTFLAGS would replace them.
            let list: Vec<String> = flags.iter().map(|flag| toml_string(flag)).collect();
            command
                .arg("--config")
                .arg(format!("target.{TARGET}.rustflags=[{}]", list.join(",")));
        }
        command
            .arg("--message-format=json-render-diagnostics")
            .stdout(Stdio::piped());
        let mut child = command.spawn()?;
        let mut executable = None;
        let stdout = child.stdout.take().expect("stdout is piped");
        for line in BufReader::new(stdout).lines() {
            let message: serde_json::Value = match serde_json::from_str(&line?) {
                Ok(message) => message,
                Err(_) => continue,
            };
            if message["reason"] == "compiler-artifact" {
                if let Some(path) = message["executable"].as_str() {
                    executable = Some(PathBuf::from(path));
                }
            }
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(format!("cargo {} failed ({status})", self.cargo.join(" ")).into());
        }
        let exe = executable.ok_or("cargo reported no executable")?;
        let is_elf = fs::read(&exe)?.starts_with(b"\x7fELF");
        if elf && !is_elf {
            return Err(format!(
                "{} is not an ELF. The guest's build.rs adds --oformat=binary; skip it when \
                 {LINK_ELF_ENV} is set (and print cargo:rerun-if-env-changed={LINK_ELF_ENV})",
                exe.display()
            )
            .into());
        }
        if !elf && is_elf {
            return Err(format!("{} is an ELF, not a flat PSX-EXE", exe.display()).into());
        }
        Ok(exe)
    }

    /// Build the ELF twin with profiling line tables and keep a copy of it
    /// in the work directory.
    fn build_twin(&self) -> Result<PathBuf> {
        let flags: Vec<String> = COLLECT_FLAGS.iter().map(ToString::to_string).collect();
        let built = self.build(&flags, true)?;
        let data = fs::read(&built)?;
        let object = object::File::parse(&*data)?;
        if object.section_by_name(".debug_line").is_none() {
            return Err(format!(
                "{} has no line tables, so the profiling flags never reached rustc. Guest \
                 flags must live in [target.{TARGET}] rustflags (build.rustflags is \
                 replaced by it)",
                built.display()
            )
            .into());
        }
        let work = self.work_dir(&built)?;
        let stem = built.file_stem().ok_or("executable has no name")?;
        let elf = work.join(Path::new(stem).with_extension("elf"));
        fs::copy(&built, &elf)?;
        Ok(elf)
    }

    fn work_dir(&self, exe: &Path) -> Result<PathBuf> {
        let work = match &self.work {
            Some(work) => work.clone(),
            None => exe
                .parent()
                .ok_or("executable has no directory")?
                .join("psoxide-pgo"),
        };
        fs::create_dir_all(&work)?;
        Ok(work)
    }

    /// Reroute the load-delay hazards the delay-slot filler leaves, then
    /// prove the image clean. Never piped: a swallowed failure ships an
    /// unpatched exe.
    fn patch(&self, exe: &Path) -> Result<()> {
        run(
            Command::new("python3").arg(&self.patcher).arg(exe),
            "hazard patch",
        )?;
        run(
            Command::new("python3").arg(&self.scanner).arg(exe),
            "hazard scan",
        )
    }
}

/// The bytes `ld.lld --oformat=binary` would have written for this ELF:
/// every allocated section with contents, at its load address relative to
/// the lowest one.
fn flat_image(data: &[u8]) -> Result<Vec<u8>> {
    let elf = ElfFile32::<object::Endianness>::parse(data)?;
    let endian = elf.endian();
    let segments: Vec<(u64, u64, u64)> = elf
        .elf_header()
        .program_headers(endian, data)?
        .iter()
        .filter(|segment| segment.p_type(endian) == object::elf::PT_LOAD)
        .map(|segment| {
            (
                u64::from(segment.p_offset(endian)),
                u64::from(segment.p_filesz(endian)),
                u64::from(segment.p_paddr(endian)),
            )
        })
        .collect();
    let mut pieces = Vec::new();
    for section in elf.sections() {
        let object::SectionFlags::Elf { sh_flags, .. } = section.flags() else {
            continue;
        };
        if sh_flags.0 & object::elf::SHF_ALLOC.0 == 0 {
            continue;
        }
        let Some((offset, size)) = section.file_range() else {
            continue; // NOBITS
        };
        if size == 0 {
            continue;
        }
        let lma = segments
            .iter()
            .find(|&&(start, len, _)| start <= offset && offset < start + len)
            .map_or(section.address(), |&(start, _, paddr)| {
                paddr + offset - start
            });
        pieces.push((lma, section.data()?));
    }
    let base = pieces
        .iter()
        .map(|piece| piece.0)
        .min()
        .ok_or("the ELF has no loadable sections")?;
    let end = pieces
        .iter()
        .map(|piece| piece.0 + piece.1.len() as u64)
        .max()
        .unwrap_or(base);
    let mut image = vec![0u8; usize::try_from(end - base)?];
    for (lma, bytes) in pieces {
        let at = usize::try_from(lma - base)?;
        image[at..at + bytes.len()].copy_from_slice(bytes);
    }
    Ok(image)
}

/// Run the caller's pack command for `exe` and return the image to launch.
fn pack(command: &str, exe: &Path, disc: &Path) -> Result<PathBuf> {
    run(
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("PSOXIDE_PGO_EXE", exe)
            .env("PSOXIDE_PGO_DISC", disc),
        "pack",
    )?;
    let cue = disc.with_extension("cue");
    if cue.is_file() {
        Ok(cue)
    } else if disc.is_file() {
        Ok(disc.to_path_buf())
    } else {
        Err(format!("the pack command did not write {}", disc.display()).into())
    }
}

fn remove_disc(disc: &Path) {
    let _ = fs::remove_file(disc);
    let _ = fs::remove_file(disc.with_extension("cue"));
}

fn collect(guest: &Guest, options: &Options) -> Result<()> {
    let frontend = options
        .frontend
        .as_ref()
        .ok_or("collect needs --frontend")?;
    let out = options.out.as_ref().ok_or("collect needs --out PROFILE")?;
    if options.tapes.is_empty() && options.launch_args.is_empty() {
        return Err("collect needs a --tape, or --launch-arg for a run without one".into());
    }

    let elf = guest.build_twin()?;
    let work = elf.parent().expect("the twin lives in the work directory");
    // The replayed image is cut from the twin itself, so every sampled PC
    // is an address in the ELF by construction.
    let exe = work.join("collect.exe");
    fs::write(&exe, flat_image(&fs::read(&elf)?)?)?;
    guest.patch(&exe)?;
    let disc = work.join("collect.bin");
    let image = match &options.pack {
        Some(command) => pack(command, &exe, &disc)?,
        None => exe.clone(),
    };

    let runs: Vec<Option<&PathBuf>> = if options.tapes.is_empty() {
        vec![None]
    } else {
        options.tapes.iter().map(Some).collect()
    };
    let mut logs = Vec::new();
    let replayed = (|| -> Result<()> {
        for (index, tape) in runs.iter().enumerate() {
            let log = work.join(format!("pc-{index}.csv"));
            let mut command = Command::new(frontend);
            command.arg("launch").arg("--path").arg(&image);
            if let Some(tape) = tape {
                command.arg("--input-tape").arg(tape);
                if !options.launch_args.iter().any(|arg| arg == "--steps") {
                    command.args(["--steps", REPLAY_STEPS]);
                }
            }
            command
                .arg("--pc-sample-log")
                .arg(&log)
                .args(["--pc-sample-instructions", SAMPLE_INTERVAL])
                .args(&options.launch_args);
            logs.push(log);
            run(&mut command, "profiling replay")?;
        }
        Ok(())
    })();
    remove_disc(&disc);
    let _ = fs::remove_file(&exe);
    let converted = replayed.and_then(|()| {
        let raw = work.join("raw.prof");
        crate::convert(&elf, &logs, &raw)?;
        crate::portable(&raw, out)
    });
    // The logs are only an intermediate; the profile carries what matters.
    for log in &logs {
        let _ = fs::remove_file(log);
    }
    converted?;
    println!("psoxide-pgo: portable profile -> {}", out.display());
    Ok(())
}

/// Build one variant and return the patched executable.
fn apply(guest: &Guest, profile: Option<&Path>, variant: &str) -> Result<PathBuf> {
    let Some(extra) = variant_flags(variant)? else {
        let exe = guest.build(&[], false)?;
        guest.patch(&exe)?;
        return Ok(exe);
    };
    let profile = profile.ok_or("a profiled variant needs --profile")?;
    if !profile.is_file() {
        return Err(format!("no profile at {}", profile.display()).into());
    }
    let elf = guest.build_twin()?;
    let rebound = elf.with_extension("rebound.prof");
    crate::rebind(profile, &elf, &rebound)?;
    let mut flags: Vec<String> = COLLECT_FLAGS.iter().map(ToString::to_string).collect();
    flags.push(format!("-Zprofile-sample-use={}", rebound.display()));
    flags.extend(extra);
    let exe = guest.build(&flags, false)?;
    guest.patch(&exe)?;
    Ok(exe)
}

/// Parse `key=value` lines from a gate's output.
fn gate_values(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let valid = !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            valid.then(|| (key.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// One variant's gate result.
struct GateRow {
    variant: String,
    passed: bool,
    values: Vec<(String, String)>,
}

fn format_table(rows: &[GateRow]) -> String {
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        for (key, _) in &row.values {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    let mut table: Vec<Vec<String>> = vec![["variant", "gate"]
        .into_iter()
        .map(String::from)
        .chain(columns.iter().cloned())
        .collect()];
    for result in rows {
        let lookup: HashMap<&str, &str> = result
            .values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let mut row = vec![
            result.variant.clone(),
            if result.passed { "pass" } else { "FAIL" }.to_string(),
        ];
        row.extend(
            columns
                .iter()
                .map(|key| lookup.get(key.as_str()).unwrap_or(&"-").to_string()),
        );
        table.push(row);
    }
    let widths: Vec<usize> = (0..table[0].len())
        .map(|column| table.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &table {
        let cells: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out
}

fn choose(guest: &Guest, options: &Options) -> Result<()> {
    let gate = options.gate.as_ref().ok_or("choose needs --gate CMD")?;
    let variants: Vec<String> = if options.variants.is_empty() {
        ["off", "default", "accurate"].map(String::from).to_vec()
    } else {
        options.variants.clone()
    };
    for variant in &variants {
        variant_flags(variant)?;
    }
    let mut rows = Vec::new();
    for variant in &variants {
        println!("psoxide-pgo: building variant {variant}");
        let exe = apply(guest, options.profile.as_deref(), variant)?;
        let disc = guest.work_dir(&exe)?.join("choose.bin");
        let image = match &options.pack {
            Some(command) => Some(pack(command, &exe, &disc)?),
            None => None,
        };
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(gate)
            .env("PSOXIDE_PGO_VARIANT", variant)
            .env("PSOXIDE_PGO_EXE", &exe)
            .stdout(Stdio::piped());
        if let Some(image) = &image {
            command.env("PSOXIDE_PGO_IMAGE", image);
        }
        let mut child = command.spawn()?;
        let mut output = String::new();
        for line in BufReader::new(child.stdout.take().expect("stdout is piped")).lines() {
            let line = line?;
            println!("  {line}");
            output.push_str(&line);
            output.push('\n');
        }
        let passed = child.wait()?.success();
        remove_disc(&disc);
        rows.push(GateRow {
            variant: variant.clone(),
            passed,
            values: gate_values(&output),
        });
    }
    print!("\n{}", format_table(&rows));
    println!(
        "The last variant built is {}; build the winner with `apply --variant`.",
        variants.last().expect("at least one variant")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_map_to_llvm_flags() {
        assert_eq!(variant_flags("off").unwrap(), None);
        assert_eq!(variant_flags("default").unwrap(), Some(vec![]));
        assert_eq!(
            variant_flags("accurate+hot=1000").unwrap(),
            Some(vec![
                "-Cllvm-args=-profile-sample-accurate".to_string(),
                "-Cllvm-args=-hot-callsite-threshold=1000".to_string(),
            ])
        );
        assert!(variant_flags("hot=lots").is_err());
        assert!(variant_flags("fast").is_err());
    }

    #[test]
    fn toml_strings_keep_spaces_and_escape_quotes() {
        assert_eq!(
            toml_string("-Zprofile-sample-use=/a b/\"c\"\\d"),
            "\"-Zprofile-sample-use=/a b/\\\"c\\\"\\\\d\""
        );
    }

    #[test]
    fn gates_report_key_value_lines() {
        let values = gate_values("replaying\ncycles=123\n unseen.cycles = 456 \nnot a pair\n=x\n");
        assert_eq!(
            values,
            vec![
                ("cycles".to_string(), "123".to_string()),
                ("unseen.cycles".to_string(), "456".to_string()),
            ]
        );
    }

    #[test]
    fn the_table_lines_up_and_marks_failures() {
        let rows = vec![
            GateRow {
                variant: "off".to_string(),
                passed: true,
                values: vec![("cycles".to_string(), "100".to_string())],
            },
            GateRow {
                variant: "default".to_string(),
                passed: false,
                values: vec![],
            },
        ];
        assert_eq!(
            format_table(&rows),
            "variant  gate  cycles\noff      pass  100\ndefault  FAIL  -\n"
        );
    }
}
