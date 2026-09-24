//! The shared PGO build: `collect`, `order`, `apply` and `choose`.
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

use crate::{layout, Result};

/// The only guest target.
const TARGET: &str = "mipsel-sony-psx";

/// Line tables with discriminators, kept through the link. The flat image
/// drops them, so a build with these runs and measures like a plain one.
const COLLECT_FLAGS: [&str; 3] = [
    "-Cdebuginfo=1",
    "-Zdebug-info-for-profiling",
    "-Cstrip=none",
];

/// Set to an ordering file while relinking an `+order` variant. The guest's
/// build.rs passes it to the link as `--symbol-ordering-file` (in place of
/// any ordering of its own), and prints `cargo:rerun-if-env-changed` for it,
/// so only the final crate relinks. Its path names its contents.
const LINK_ORDER_ENV: &str = "PSOXIDE_LINK_ORDER";

/// Set while linking the ELF twin. A guest whose build.rs adds
/// `--oformat=binary` must leave it out when this is set; flags given
/// through rustflags need nothing, because the twin's later
/// `--oformat=elf` wins.
const LINK_ELF_ENV: &str = "PSOXIDE_LINK_ELF";

/// A prime, so the sampler cannot fall into step with a loop.
const SAMPLE_INTERVAL: &str = "61";

/// Cap for tape replays, which stop on their own when the tape runs out.
const REPLAY_STEPS: &str = "40000000000";

/// Route ticks per PC-sample window when training on a poll window. The
/// frontend cannot start sampling late, so it samples in windows and only
/// the ones wholly inside the gameplay polls are kept.
const SAMPLE_WINDOW_TICKS: usize = 30;

pub const USAGE: &str = "\
       psoxide-pgo collect [GUEST] --frontend PATH [--tape PATH [--polls A..B]]...
                           [--launch-arg ARG]... [--pack CMD] --out PROFILE -- CARGO-ARGS...
       psoxide-pgo order   [GUEST] --frontend PATH (--tape PATH --polls A..B)... [--launch-arg ARG]...
                           [--pack CMD] [--profile PROFILE] [--variant V] --out LAYOUT -- CARGO-ARGS...
       psoxide-pgo apply   [GUEST] [--profile PROFILE] [--layout LAYOUT] [--variant V] -- CARGO-ARGS...
       psoxide-pgo choose  [GUEST] --profile PROFILE [--layout LAYOUT] --gate CMD [--variant V]...
                           [--pack CMD] [--frame-budget VBLANKS] [--rank deadline|work] -- CARGO-ARGS...
       psoxide-pgo measure --frontend PATH --image PATH [--tape PATH] --polls A..B
                           [--launch-arg ARG]... [--name NAME] [--wait-range START..END]...
  GUEST: [--crate DIR] [--work DIR] [--patcher PATH] [--scanner PATH] [--stack-guard PATH]
         [--linker-script PATH]
  CARGO-ARGS: what follows `cargo` in the guest's own build, starting with `build`
  V: off | default | accurate | noreplay | nopgso | profi | hot=N | llvm=-FLAG, joined with +;
     add +order to link in the order the layout profile gives (off+order, hot=500+order)
  A..B: the gameplay window in port-1 polls, loads excluded
  START..END: guest addresses in hex (one build's layout) of a loop to count as waiting";

/// How to build one guest, shared by every mode.
struct Guest {
    crate_dir: PathBuf,
    cargo: Vec<String>,
    work: Option<PathBuf>,
    patcher: PathBuf,
    scanner: PathBuf,
    stack_guard: PathBuf,
    /// The guest's linker script, for the sections it places ahead of the
    /// catch-all `*(.text .text.*)`, which no ordering file can move.
    linker_script: PathBuf,
}

/// One link: the executable cargo reports and the link map the driver asked
/// the link for, when it was written.
struct Linked {
    exe: PathBuf,
    map: Option<PathBuf>,
}

/// One emulator run: a tape (or none) and the gameplay polls to keep.
#[derive(Default)]
struct Run {
    tape: Option<PathBuf>,
    polls: Option<(u64, u64)>,
}

#[derive(Default)]
struct Options {
    crate_dir: Option<PathBuf>,
    work: Option<PathBuf>,
    patcher: Option<PathBuf>,
    scanner: Option<PathBuf>,
    stack_guard: Option<PathBuf>,
    linker_script: Option<PathBuf>,
    frontend: Option<PathBuf>,
    runs: Vec<Run>,
    launch_args: Vec<String>,
    pack: Option<String>,
    out: Option<PathBuf>,
    profile: Option<PathBuf>,
    layout: Option<PathBuf>,
    variants: Vec<String>,
    gate: Option<String>,
    image: Option<PathBuf>,
    name: Option<String>,
    wait_ranges: Vec<(u32, u32)>,
    frame_budget: Option<u64>,
    objective: Objective,
    cargo: Vec<String>,
}

/// `START..END` as a half-open range of guest addresses, in hex.
fn parse_range(text: &str) -> Result<(u32, u32)> {
    let hex = |text: &str| u32::from_str_radix(text.trim_start_matches("0x"), 16).ok();
    let parsed = text
        .split_once("..")
        .and_then(|(start, end)| Some((hex(start)?, hex(end)?)));
    match parsed {
        Some((start, end)) if start < end && start % 4 == 0 && end % 4 == 0 => Ok((start, end)),
        _ => Err(format!(
            "--wait-range wants START..END in hex, word-aligned, START < END, not {text:?}"
        )
        .into()),
    }
}

/// `A..B` as a half-open poll range.
fn parse_polls(text: &str) -> Result<(u64, u64)> {
    let parsed = text
        .split_once("..")
        .and_then(|(from, to)| Some((from.parse().ok()?, to.parse().ok()?)));
    match parsed {
        Some((from, to)) if from < to => Ok((from, to)),
        _ => Err(format!("--polls wants FROM..TO with FROM < TO, not {text:?}").into()),
    }
}

fn parse(mode: &str, args: &[String]) -> Result<Options> {
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
            "--stack-guard" => options.stack_guard = Some(path(value()?)?),
            "--linker-script" => options.linker_script = Some(path(value()?)?),
            "--layout" => options.layout = Some(path(value()?)?),
            "--frontend" => options.frontend = Some(path(value()?)?),
            "--tape" => options.runs.push(Run {
                tape: Some(path(value()?)?),
                polls: None,
            }),
            // Belongs to the --tape before it, or to the one tapeless run.
            "--polls" => {
                let polls = parse_polls(&value()?)?;
                match options.runs.last_mut() {
                    Some(run) if run.polls.is_none() => run.polls = Some(polls),
                    Some(_) => return Err("one --polls per --tape".into()),
                    None => options.runs.push(Run {
                        tape: None,
                        polls: Some(polls),
                    }),
                }
            }
            "--image" => options.image = Some(path(value()?)?),
            "--name" => options.name = Some(value()?),
            "--wait-range" => options.wait_ranges.push(parse_range(&value()?)?),
            "--launch-arg" => options.launch_args.push(value()?),
            "--pack" => options.pack = Some(value()?),
            "--out" => options.out = Some(path(value()?)?),
            "--profile" => options.profile = Some(path(value()?)?),
            "--variant" => options.variants.push(value()?),
            "--gate" => options.gate = Some(value()?),
            "--frame-budget" => {
                let text = value()?;
                match text.parse() {
                    Ok(vblanks) if vblanks > 0 => options.frame_budget = Some(vblanks),
                    _ => {
                        return Err(format!(
                            "--frame-budget wants the vblanks a frame may take (1 for 60 fps, \
                             2 for 30), not {text:?}"
                        )
                        .into())
                    }
                }
            }
            "--rank" => {
                options.objective = match value()?.as_str() {
                    "deadline" => Objective::Deadline,
                    "work" => Objective::Work,
                    other => {
                        return Err(format!("--rank wants deadline or work, not {other:?}").into())
                    }
                }
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if mode != "measure" && options.cargo.first().map(String::as_str) != Some("build") {
        return Err("give the guest's cargo arguments after --, starting with `build`".into());
    }
    Ok(options)
}

/// Run `collect`, `order`, `apply`, `choose` or `measure`.
pub fn main(mode: &str, args: &[String]) -> Result<()> {
    let mut options = parse(mode, args)?;
    if mode == "measure" {
        return measure(&options);
    }
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
        stack_guard: options
            .stack_guard
            .take()
            .unwrap_or_else(|| tools.join("stack_guard.py")),
        linker_script: options
            .linker_script
            .take()
            .unwrap_or_else(|| tools.join("../sdk/psoxide.ld")),
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
        "order" => order(&guest, &options),
        "apply" => {
            let variant = one_variant(&options, "apply")?;
            let linked = apply(
                &guest,
                options.profile.as_deref(),
                options.layout.as_deref(),
                variant,
            )?;
            println!("psoxide-pgo: {variant} -> {}", linked.exe.display());
            Ok(())
        }
        "choose" => choose(&guest, &options),
        _ => unreachable!("main only dispatches pipeline modes"),
    }
}

/// The one `--variant` of `apply` or `order`, `default` if none.
fn one_variant<'a>(options: &'a Options, mode: &str) -> Result<&'a str> {
    match options.variants.as_slice() {
        [] => Ok("default"),
        [one] => Ok(one),
        _ => Err(format!("{mode} takes one --variant").into()),
    }
}

/// A variant's compile part, and whether it adds `+order` (the layout
/// profile's function order, which is a link and not a compile).
fn split_order(variant: &str) -> Result<(String, bool)> {
    let parts: Vec<&str> = variant.split('+').collect();
    let compile: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|&part| part != "order")
        .collect();
    if compile.is_empty() {
        return Err(
            "order joins a compile variant: off+order, default+order, hot=500+profi+order".into(),
        );
    }
    Ok((compile.join("+"), compile.len() < parts.len()))
}

/// Extra rustflags for one variant, or `None` for `off`.
fn variant_flags(variant: &str) -> Result<Option<Vec<String>>> {
    if variant == "off" {
        return Ok(None);
    }
    let mut flags = Vec::new();
    for part in variant.split('+') {
        let llvm = |flag: &str| format!("-Cllvm-args={flag}");
        match part {
            "default" => {}
            "accurate" => flags.push(llvm("-profile-sample-accurate")),
            // Do not replay the profiled build's inlining; the inlinees'
            // samples merge into their own functions instead.
            "noreplay" => flags.push(llvm("-disable-sample-loader-inlining")),
            // Do not optimise profile-cold code for size (which turns
            // struct copies into memcpy calls, among other things).
            "nopgso" => flags.push(llvm("-pgso=false")),
            // Infer block counts by min-cost flow where samples are missing
            // (line-0 code), instead of trusting sparse samples as they are.
            "profi" => flags.push(llvm("-sample-profile-use-profi")),
            _ => {
                if let Some(threshold) = part.strip_prefix("hot=") {
                    let threshold: u32 = threshold
                        .parse()
                        .map_err(|_| format!("hot= wants a number, not {threshold:?}"))?;
                    flags.push(llvm(&format!("-hot-callsite-threshold={threshold}")));
                } else if let Some(flag) = part.strip_prefix("llvm=").filter(|f| f.starts_with('-'))
                {
                    flags.push(llvm(flag));
                } else {
                    return Err(format!("unknown variant part {part:?}").into());
                }
            }
        }
    }
    Ok(Some(flags))
}

/// First line of a profile written by `collect`: the cargo features it was
/// trained with. LLVM skips `#` lines.
const FEATURES_TAG: &str = "# psoxide-pgo features: ";

/// The cargo features `args` select, normalised so equal sets compare equal.
fn features_of(args: &[String]) -> String {
    let mut features = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let list = match arg.as_str() {
            "--features" | "-F" => iter.next().map(String::as_str),
            "--all-features" | "--no-default-features" => {
                features.push(arg.trim_start_matches('-').to_string());
                None
            }
            _ => arg.strip_prefix("--features="),
        };
        if let Some(list) = list {
            features.extend(
                list.split([',', ' '])
                    .filter(|f| !f.is_empty())
                    .map(String::from),
            );
        }
    }
    features.sort();
    features.dedup();
    if features.is_empty() {
        "(default)".to_string()
    } else {
        features.join(",")
    }
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

/// 64-bit FNV-1a: a name for a set of build inputs that stays the same from
/// one driver build to the next (std's hasher promises no such thing).
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
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
    /// the executable cargo reports, with the link map when the link wrote
    /// one. `order` goes to the guest's build.rs in [`LINK_ORDER_ENV`]; it
    /// changes no rustflags, so only the final crate relinks.
    fn build(&self, flags: &[String], elf: bool, order: Option<&Path>) -> Result<Linked> {
        let mut flags = flags.to_vec();
        let mut command = Command::new("cargo");
        command.current_dir(&self.crate_dir).args(&self.cargo);
        match order {
            Some(order) => command.env(LINK_ORDER_ENV, order),
            None => command.env_remove(LINK_ORDER_ENV),
        };
        if elf {
            flags.push("-Clink-arg=--oformat=elf".to_string());
            command.env(LINK_ELF_ENV, "1");
        } else {
            command.env_remove(LINK_ELF_ENV);
        }
        // Link-only: the emitted bytes do not change. The path names every
        // input that selects the link, so a build cargo finds fresh (and so
        // does not relink) still has the map of the link that made it, and
        // stays fresh itself: a new path would be new rustflags. An ordered
        // relink shares its variant's path: a change of order reruns the
        // build script, so that link always writes the map.
        let map = self.map_path(&flags)?;
        flags.push(format!("-Clink-arg=-Map={}", map.display()));
        // `--config` appends to the rustflags in the guest's
        // .cargo/config.toml, where RUSTFLAGS would replace them.
        let list: Vec<String> = flags.iter().map(|flag| toml_string(flag)).collect();
        command
            .arg("--config")
            .arg(format!("target.{TARGET}.rustflags=[{}]", list.join(",")));
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
        let map = if map.is_file() {
            Some(map)
        } else {
            // A `-Map` the guest's build.rs adds comes later on the link
            // line and wins.
            eprintln!(
                "psoxide-pgo: warning: the link wrote no map to {}, so the hazard tools and \
                 the stack guard run without one",
                map.display()
            );
            None
        };
        Ok(Linked { exe, map })
    }

    /// Where the link for these rustflags writes its map:
    /// `<target>/mipsel-sony-psx/psoxide-pgo-maps/<hash>.map`, the hash of
    /// the crate, the guest's cargo arguments and `flags`.
    fn map_path(&self, flags: &[String]) -> Result<PathBuf> {
        let mut key = self.crate_dir.display().to_string();
        for part in self.cargo.iter().chain(flags) {
            key.push('\0');
            key.push_str(part);
        }
        let dir = self.target_dir()?.join(TARGET).join("psoxide-pgo-maps");
        fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{:016x}.map", fnv1a(key.as_bytes()))))
    }

    /// The guest's cargo target directory: its own `--target-dir`, or what
    /// `cargo metadata` resolves (CARGO_TARGET_DIR, config files, the
    /// guest's `--config`).
    fn target_dir(&self) -> Result<PathBuf> {
        let mut pass = Vec::new();
        let mut args = self.cargo.iter();
        while let Some(arg) = args.next() {
            if arg == "--target-dir" {
                let dir = args.next().ok_or("--target-dir needs a value")?;
                return Ok(self.crate_dir.join(dir));
            }
            if let Some(dir) = arg.strip_prefix("--target-dir=") {
                return Ok(self.crate_dir.join(dir));
            }
            if arg == "--config" || arg == "--manifest-path" {
                pass.push(arg.clone());
                pass.extend(args.next().cloned());
            } else if arg.starts_with("--config=") || arg.starts_with("--manifest-path=") {
                pass.push(arg.clone());
            }
        }
        let output = Command::new("cargo")
            .current_dir(&self.crate_dir)
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .args(&pass)
            .stderr(Stdio::inherit())
            .output()?;
        if !output.status.success() {
            return Err(format!("cargo metadata failed ({})", output.status).into());
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let dir = metadata["target_directory"]
            .as_str()
            .ok_or("cargo metadata names no target directory")?;
        Ok(PathBuf::from(dir))
    }

    /// Build the ELF twin with profiling line tables and keep a copy of it
    /// in the work directory.
    fn build_twin(&self) -> Result<Linked> {
        let flags: Vec<String> = COLLECT_FLAGS.iter().map(ToString::to_string).collect();
        let Linked { exe: built, map } = self.build(&flags, true, None)?;
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
        Ok(Linked { exe: elf, map })
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

    /// Reroute the load-delay hazards the delay-slot filler leaves, prove
    /// the image clean, and prove every scratchpad stack call tree fits its
    /// region. With the link's map every jump table is proven from it (and
    /// the stack guard needs it for an image that switches stacks). Never
    /// piped: a swallowed failure ships an unpatched exe.
    fn patch(&self, exe: &Path, map: Option<&Path>) -> Result<()> {
        let tool = |script: &Path, map_flag: bool| {
            let mut command = Command::new("python3");
            command.arg(script).arg(exe);
            if let Some(map) = map {
                if map_flag {
                    command.arg("--map");
                }
                command.arg(map);
            }
            command
        };
        run(&mut tool(&self.patcher, true), "hazard patch")?;
        run(&mut tool(&self.scanner, true), "hazard scan")?;
        run(&mut tool(&self.stack_guard, false), "stack guard")
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

/// `frontend launch` on `image` for one run: its tape, a stop at the end of
/// its poll window, then the caller's arguments.
fn launch(frontend: &Path, image: &Path, run: &Run, launch_args: &[String]) -> Command {
    let given = |flag: &str| launch_args.iter().any(|arg| arg == flag);
    let mut command = Command::new(frontend);
    command.arg("launch").arg("--path").arg(image);
    if let Some(tape) = &run.tape {
        command.arg("--input-tape").arg(tape);
    }
    if let Some((_, to)) = run.polls {
        if !given("--stop-at-poll") {
            command.arg("--stop-at-poll").arg(to.to_string());
        }
    }
    if (run.tape.is_some() || run.polls.is_some()) && !given("--steps") {
        command.args(["--steps", REPLAY_STEPS]);
    }
    command.args(launch_args);
    command
}

/// One `--route-log` row: the state at the end of a route tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Tick {
    polls: u64,
    cycles: u64,
    flipped: bool,
    icache: u64,
    /// Instructions retired since the start of the run.
    instructions_total: u64,
    /// Bus cycles since the start of the run.
    cycles_total: u64,
}

/// A route log, indexed by route tick.
fn read_route_log(path: &Path) -> Result<Vec<Tick>> {
    let text = fs::read_to_string(path)?;
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|field| *field == name)
            .ok_or_else(|| format!("route log has no {name} column"))
    };
    let (polls, cycles) = (column("port1_polls")?, column("bus_cycle_delta")?);
    let (flipped, icache) = (
        column("display_start_changed")?,
        column("icache_refill_stall_cycles_delta")?,
    );
    let (instructions_total, cycles_total) = (column("cpu_tick")?, column("bus_cycles")?);
    let mut ticks = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        let number = |index: usize| -> Result<u64> {
            Ok(fields.get(index).ok_or("short route log row")?.parse()?)
        };
        ticks.push(Tick {
            polls: number(polls)?,
            cycles: number(cycles)?,
            flipped: number(flipped)? != 0,
            icache: number(icache)?,
            instructions_total: number(instructions_total)?,
            cycles_total: number(cycles_total)?,
        });
    }
    Ok(ticks)
}

/// Whether ticks `first..=last` all ran inside the poll window: none
/// started before poll `from`, and none ran past poll `to`.
fn inside(ticks: &[Tick], first: usize, last: usize, (from, to): (u64, u64)) -> bool {
    let before = first.checked_sub(1).map_or(0, |index| ticks[index].polls);
    last < ticks.len() && before >= from && ticks[last].polls <= to
}

/// Sum a `--pc-sample-window-log` over the windows wholly inside `polls`
/// into a plain `pc,samples` histogram, and say how much was kept.
fn window_histogram(
    windows: &Path,
    ticks: &[Tick],
    polls: (u64, u64),
    out: &Path,
) -> Result<(usize, usize)> {
    let last_tick = ticks.len().saturating_sub(1);
    let mut kept: HashMap<u64, bool> = HashMap::new();
    let mut samples: Vec<(String, u64)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for line in fs::read_to_string(windows)?.lines().skip(1) {
        let mut fields = line.split(',');
        let (Some(start), Some(pc), Some(count)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(start), Ok(count)) = (start.parse::<u64>(), count.parse::<u64>()) else {
            continue;
        };
        // Samples taken while `start` ticks had completed land in route-log
        // rows start+1 ..= start+SAMPLE_WINDOW_TICKS.
        let keep = *kept.entry(start).or_insert_with(|| {
            let first = start as usize + 1;
            let last = (first + SAMPLE_WINDOW_TICKS - 1).min(last_tick);
            first <= last && inside(ticks, first, last, polls)
        });
        if keep {
            let slot = *index.entry(pc.to_string()).or_insert_with(|| {
                samples.push((pc.to_string(), 0));
                samples.len() - 1
            });
            samples[slot].1 += count;
        }
    }
    let mut text = String::from("pc,samples\n");
    for (pc, count) in &samples {
        let _ = writeln!(text, "{pc},{count}");
    }
    fs::write(out, text)?;
    Ok((kept.values().filter(|keep| **keep).count(), kept.len()))
}

/// Retired instructions between the measuring replay's PC samples, which
/// place its waiting in route ticks. A prime, like [`SAMPLE_INTERVAL`].
const TICK_SAMPLE_INTERVAL: u64 = 61;

/// Polls past the window's first one that the locating replay runs to, so
/// the route tick in which poll FROM lands is logged before it stops.
const LOCATE_LEAD_POLLS: u64 = 30;

/// A `line_pc,value,percent` log from `--pc-line-log` or one of the stall
/// attributions, as line address to value.
pub(crate) fn read_line_log(path: &Path) -> Result<HashMap<u32, u64>> {
    let mut lines = HashMap::new();
    for row in fs::read_to_string(path)?.lines().skip(1) {
        let mut fields = row.split(',');
        let (Some(line), Some(value)) = (fields.next(), fields.next()) else {
            continue;
        };
        let line = u32::from_str_radix(line.trim_start_matches("0x"), 16)?;
        lines.insert(line, value.parse()?);
    }
    Ok(lines)
}

/// The `key=value` lines of a frontend's final report.
#[derive(Default)]
struct Report {
    hashes: Vec<(&'static str, String)>,
    instructions: Option<u64>,
    cycles: Option<u64>,
    polls: Option<u64>,
}

impl Report {
    fn read(&mut self, line: &str) {
        for (tag, key) in [("vram_fnv1a_64=", "vram"), ("display_fnv1a_64=", "display")] {
            if let Some(value) = line.strip_prefix(tag) {
                let value = value.split_whitespace().next().unwrap_or_default();
                self.hashes.push((key, value.to_string()));
            }
        }
        // `tick=... cycles=... pc=...` and `route-ticks=... port1-polls=...`
        // close the run; other lines may carry their own cycles= fields.
        if !line.starts_with("tick=") && !line.starts_with("route-ticks=") {
            return;
        }
        for field in line.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let value = value.parse().ok();
            match key {
                "tick" => self.instructions = value,
                "cycles" => self.cycles = value,
                "port1-polls" => self.polls = value,
                _ => {}
            }
        }
    }
}

/// Run `command`, echoing its stdout to stderr (out of the gate's table)
/// and keeping the final report.
fn replay(mut command: Command, what: &str) -> Result<Report> {
    let mut child = command.stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let mut report = Report::default();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        eprintln!("{line}");
        report.read(&line);
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(format!("{what} failed ({status})").into());
    }
    Ok(report)
}

/// The value at quantile `q` of `sorted` (nearest rank).
fn percentile(sorted: &[u64], q: f64) -> u64 {
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// Presentation over the window's ticks: bus cycles between consecutive
/// display flips (p50, p95) and how many route ticks (vblanks) each frame
/// was on screen, as `vblanks:frames` pairs.
fn frame_pacing(ticks: &[Tick], window: &[usize]) -> Option<(u64, u64, String)> {
    let flips: Vec<usize> = window
        .iter()
        .copied()
        .filter(|&tick| ticks[tick].flipped)
        .collect();
    if flips.len() < 2 {
        return None;
    }
    let mut cycles: Vec<u64> = flips
        .windows(2)
        .map(|pair| ticks[pair[1]].cycles_total - ticks[pair[0]].cycles_total)
        .collect();
    cycles.sort_unstable();
    let mut vblanks: std::collections::BTreeMap<usize, usize> = Default::default();
    for pair in flips.windows(2) {
        *vblanks.entry(pair[1] - pair[0]).or_default() += 1;
    }
    let histogram: Vec<String> = vblanks
        .iter()
        .map(|(vblanks, frames)| format!("{vblanks}:{frames}"))
        .collect();
    Some((
        percentile(&cycles, 0.5),
        percentile(&cycles, 0.95),
        histogram.join(","),
    ))
}

/// Work and waiting from the start of route tick `start + 1` to the end of
/// the replay.
struct Work {
    instructions: u64,
    cycles: u64,
    wait_cycles: u64,
    span_cycles: u64,
    frames: u64,
    /// Work cycles each whole route tick of the span spent, by route-log
    /// index (see [`tick_wait`]).
    tick_work: HashMap<usize, u64>,
}

/// Cycles spent waiting: the waiting instructions' issue plus every stall
/// charged to them (`stalls`: I-cache refills, RAM loads, MMIO), in
/// proportion where only part of a unit's count waited. A layout can put a
/// wait loop in the same I-cache set as its hazard trampoline, and the
/// refills that costs on every spin wait for the same event as the spin.
fn wait_cycles(
    wait: &HashMap<u32, u64>,
    counts: &HashMap<u32, u64>,
    stalls: &[&HashMap<u32, u64>],
) -> u64 {
    let on = |log: &HashMap<u32, u64>, at: u32| log.get(&at).copied().unwrap_or(0);
    wait.iter()
        .map(|(&at, &count)| {
            let stalled: u128 = stalls.iter().map(|log| u128::from(on(log, at))).sum();
            count + (stalled * u128::from(count) / u128::from(on(counts, at).max(1))) as u64
        })
        .sum()
}

/// Wait cycles per retired instruction at each unit that waited, on the
/// same terms as [`wait_cycles`].
fn wait_per_instruction(
    wait: &HashMap<u32, u64>,
    counts: &HashMap<u32, u64>,
    stalls: &[&HashMap<u32, u64>],
) -> HashMap<u32, f64> {
    let on = |log: &HashMap<u32, u64>, at: u32| log.get(&at).copied().unwrap_or(0);
    wait.iter()
        .filter_map(|(&at, &count)| {
            let total = on(counts, at);
            if total == 0 {
                return None;
            }
            let stalled: u64 = stalls.iter().map(|log| on(log, at)).sum();
            let cycles = count as f64 * (total + stalled) as f64 / total as f64;
            Some((at, cycles / total as f64))
        })
        .collect()
}

/// A `--pc-sample-window-log` with one route tick per window, as
/// (route-log index, PC, samples). Samples taken while `start` ticks had
/// completed belong to route-log row `start + 1`.
fn read_tick_samples(path: &Path) -> Result<Vec<(usize, u32, u64)>> {
    let mut samples = Vec::new();
    for row in fs::read_to_string(path)?.lines().skip(1) {
        let mut fields = row.split(',');
        let (Some(start), Some(pc), Some(count)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        samples.push((
            start.parse::<usize>()? + 1,
            u32::from_str_radix(pc.trim_start_matches("0x"), 16)?,
            count.parse()?,
        ));
    }
    Ok(samples)
}

/// Cycles each route tick spent waiting, estimated from PC samples taken
/// every `interval` retired instructions: a sample stands for `interval`
/// instructions at its unit, and each of those waits what the exact counts
/// say an instruction there waits on average. A spell of waiting is one run
/// of instructions, so a tick's estimate is off by at most `interval`
/// instructions for each spell in it.
fn tick_wait(
    samples: &[(usize, u32, u64)],
    interval: u64,
    unit: u32,
    per_instruction: &HashMap<u32, f64>,
) -> HashMap<usize, u64> {
    let mut wait: HashMap<usize, f64> = HashMap::new();
    for &(tick, pc, count) in samples {
        if let Some(cycles) = per_instruction.get(&(pc & !(unit - 1))) {
            *wait.entry(tick).or_default() += (count * interval) as f64 * cycles;
        }
    }
    wait.into_iter()
        .map(|(tick, cycles)| (tick, cycles.round() as u64))
        .collect()
}

/// Work cycles of each frame the window presented, sorted: the route ticks
/// after one flip up to and including the tick of the next.
fn frame_work(ticks: &[Tick], window: &[usize], tick_work: &HashMap<usize, u64>) -> Vec<u64> {
    let flips: Vec<usize> = window
        .iter()
        .copied()
        .filter(|&tick| ticks[tick].flipped)
        .collect();
    let mut frames: Vec<u64> = flips
        .windows(2)
        .map(|pair| {
            (pair[0] + 1..=pair[1])
                .map(|tick| tick_work.get(&tick).copied().unwrap_or(0))
                .sum()
        })
        .collect();
    frames.sort_unstable();
    frames
}

/// Split the replay's span into work and wait (see work.rs), and list the
/// wait loops on stderr so a reader can check them. `unit` is how finely
/// the logs count: 4 bytes (one word) or 16 (one I-cache line).
fn split_work(
    logs: &MeasureLogs,
    ticks: &[Tick],
    start: usize,
    report: &Report,
    to: u64,
    unit: u32,
    ranges: &[(u32, u32)],
    icache_lines: bool,
) -> Result<Work> {
    let counts = read_line_log(&logs.lines)?;
    let mut stall_logs = vec![read_line_log(&logs.mmio)?, read_line_log(&logs.ram_load)?];
    if icache_lines {
        stall_logs.push(read_line_log(&logs.icache)?);
    }
    let stalls: Vec<&HashMap<u32, u64>> = stall_logs.iter().collect();
    let ram = fs::read(&logs.ram)?;
    let (Some(end_instructions), Some(end_cycles)) = (report.instructions, report.cycles) else {
        return Err("the frontend printed no final tick= and cycles=".into());
    };
    let span_instructions = end_instructions - ticks[start].instructions_total;
    let counted: u64 = counts.values().sum();
    if counted != span_instructions {
        return Err(format!(
            "the PC-line log holds {counted} instructions but the route log says \
             {span_instructions} ran after route tick {start}"
        )
        .into());
    }
    let percent = |count: u64| 100.0 * count as f64 / span_instructions as f64;
    let split = crate::work::split(&ram, &counts, unit, ranges)?;
    for (found, count) in &split.loops {
        if count * 1000 >= span_instructions {
            eprintln!(
                "psoxide-pgo: wait loop {:#010x}..{:#010x}: {:.2}% of instructions",
                found.start,
                found.end,
                percent(*count)
            );
        }
    }
    for named in &split.named {
        let calls = match named.called {
            _ if named.calls == 0 => String::new(),
            Some(called) => format!(
                ", and its {} calls {:.2}% more",
                named.calls,
                percent(called)
            ),
            None => format!(
                ", and its {} calls count as work: the frontend has no --pc-log-words",
                named.calls
            ),
        };
        eprintln!(
            "psoxide-pgo: wait range {:#010x}..{:#010x}: {:.2}% of instructions{calls}",
            named.start,
            named.end,
            percent(named.own)
        );
    }
    let wait = split.wait;
    let wait_instructions: u64 = wait.values().sum();
    let wait_cycles = wait_cycles(&wait, &counts, &stalls);
    let span_cycles = end_cycles - ticks[start].cycles_total;
    // Frames presented in the span: the flips in its whole ticks, and the
    // one the replay stops on once it has passed poll `to`.
    let tail = &ticks[start + 1..];
    let stop_flip = report.polls.is_some_and(|polls| polls >= to)
        && ticks
            .last()
            .is_some_and(|last| last.instructions_total < end_instructions);
    let frames = tail.iter().filter(|tick| tick.flipped).count() as u64 + u64::from(stop_flip);
    let samples: Vec<(usize, u32, u64)> = read_tick_samples(&logs.windows)?
        .into_iter()
        .filter(|&(tick, _, _)| tick > start && tick < ticks.len())
        .collect();
    let waits = tick_wait(
        &samples,
        TICK_SAMPLE_INTERVAL,
        unit,
        &wait_per_instruction(&wait, &counts, &stalls),
    );
    let estimated: u64 = waits.values().sum();
    let in_ticks: u64 = tail.iter().map(|tick| tick.cycles).sum();
    eprintln!(
        "psoxide-pgo: waiting per route tick, from 1-in-{TICK_SAMPLE_INTERVAL} PC samples: \
         {estimated} cycles in the span's whole ticks; exactly {wait_cycles} in the span, \
         which runs {} cycles past its last whole tick",
        span_cycles - in_ticks
    );
    let tick_work = (start + 1..ticks.len())
        .map(|tick| {
            let waited = waits.get(&tick).copied().unwrap_or(0);
            (tick, ticks[tick].cycles.saturating_sub(waited))
        })
        .collect();
    Ok(Work {
        instructions: span_instructions - wait_instructions,
        cycles: span_cycles.saturating_sub(wait_cycles),
        wait_cycles,
        span_cycles,
        frames,
        tick_work,
    })
}

/// The files one measuring replay writes, removed afterwards.
struct MeasureLogs {
    locate: PathBuf,
    route: PathBuf,
    lines: PathBuf,
    mmio: PathBuf,
    ram_load: PathBuf,
    icache: PathBuf,
    windows: PathBuf,
    ram: PathBuf,
}

impl MeasureLogs {
    fn new() -> Self {
        let file =
            |name: &str| env::temp_dir().join(format!("psoxide-pgo-{}-{name}", std::process::id()));
        Self {
            locate: file("locate.csv"),
            route: file("route.csv"),
            lines: file("lines.csv"),
            mmio: file("mmio.csv"),
            ram_load: file("ram-load.csv"),
            icache: file("icache.csv"),
            windows: file("windows.csv"),
            ram: file("ram.bin"),
        }
    }

    fn remove(&self) {
        for path in [
            &self.locate,
            &self.route,
            &self.lines,
            &self.mmio,
            &self.ram_load,
            &self.icache,
            &self.windows,
            &self.ram,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

/// The per-line logs can only start at a route tick, so a short replay
/// finds the tick in which poll FROM of `spec`'s window lands: the last
/// tick before the window, whose route-log row is the first to reach it.
/// Returns it and its row, which the full replay must match (the emulator
/// is deterministic, so it reaches the tick in the same state).
fn locate(
    frontend: &Path,
    image: &Path,
    spec: &Run,
    launch_args: &[String],
    log: &Path,
) -> Result<(usize, Tick)> {
    let polls = spec.polls.ok_or("a replay window needs --polls FROM..TO")?;
    let locate = Run {
        tape: spec.tape.clone(),
        polls: Some((0, (polls.0 + LOCATE_LEAD_POLLS).min(polls.1))),
    };
    let mut command = launch(frontend, image, &locate, launch_args);
    command.arg("--route-log").arg(log);
    replay(command, "locating replay")?;
    let located = read_route_log(log)?;
    let start = located
        .iter()
        .position(|tick| tick.polls >= polls.0)
        .ok_or_else(|| format!("the run never reached poll {}", polls.0))?;
    Ok((start, located[start]))
}

/// Refuse a full replay that does not match its locating replay at `start`.
fn same_at(ticks: &[Tick], start: usize, located: Tick) -> Result<()> {
    if ticks.get(start) != Some(&located) {
        return Err(format!(
            "the two replays differ at route tick {start}; this needs a deterministic run"
        )
        .into());
    }
    Ok(())
}

/// Whether `frontend launch` lists `flag`: `--pc-log-words` counts every
/// word instead of every 16-byte line, and `--icache-stall-line-log`
/// charges I-cache refills to the fetch that missed.
fn lists(frontend: &Path, flag: &str) -> Result<bool> {
    let help = Command::new(frontend)
        .args(["launch", "--help"])
        .stderr(Stdio::null())
        .output()?;
    let help = String::from_utf8_lossy(&help.stdout);
    Ok(help
        .split(|c: char| c.is_whitespace() || c == ',')
        .any(|word| word == flag))
}

/// Run one replay and print its gameplay-window totals as `key=value` lines
/// for a `choose` gate.
fn measure(options: &Options) -> Result<()> {
    let logs = MeasureLogs::new();
    let measured = measure_with(options, &logs);
    logs.remove();
    measured
}

fn measure_with(options: &Options, logs: &MeasureLogs) -> Result<()> {
    let frontend = options
        .frontend
        .as_ref()
        .ok_or("measure needs --frontend")?;
    let image = options.image.as_ref().ok_or("measure needs --image")?;
    let spec = match options.runs.as_slice() {
        [spec] if spec.polls.is_some() => spec,
        _ => {
            return Err("measure needs one --polls FROM..TO window (and at most one --tape)".into())
        }
    };
    let polls = spec.polls.expect("checked above");
    let (start, located) = locate(frontend, image, spec, &options.launch_args, &logs.locate)?;
    let start_tick = start.to_string();

    // Per-word counts, where the frontend has them, make the split exact
    // and let a wait loop's calls count (see work::attribute).
    let words = lists(frontend, "--pc-log-words")?;
    let icache_lines = lists(frontend, "--icache-stall-line-log")?;
    let mut command = launch(frontend, image, spec, &options.launch_args);
    if !options.launch_args.iter().any(|arg| arg == "--dump-hash") {
        command.arg("--dump-hash");
    }
    if words {
        command.arg("--pc-log-words");
    }
    command
        .arg("--route-log")
        .arg(&logs.route)
        .arg("--pc-line-log")
        .arg(&logs.lines)
        .args(["--pc-line-start-route-tick", &start_tick])
        .arg("--mmio-stall-line-log")
        .arg(&logs.mmio)
        .args(["--mmio-stall-line-start-route-tick", &start_tick])
        .arg("--ram-load-stall-line-log")
        .arg(&logs.ram_load)
        .args(["--ram-load-stall-line-start-route-tick", &start_tick])
        .arg("--pc-sample-window-log")
        .arg(&logs.windows)
        .args(["--pc-sample-window-ticks", "1"])
        .args([
            "--pc-sample-instructions",
            &TICK_SAMPLE_INTERVAL.to_string(),
        ])
        .arg("--dump-ram")
        .arg(&logs.ram);
    if icache_lines {
        command
            .arg("--icache-stall-line-log")
            .arg(&logs.icache)
            .args(["--icache-stall-line-start-route-tick", &start_tick]);
    } else {
        eprintln!(
            "psoxide-pgo: {} has no --icache-stall-line-log, so the wait loops' I-cache \
             refills count as work",
            frontend.display()
        );
    }
    let report = replay(command, "measuring replay")?;
    let ticks = read_route_log(&logs.route)?;
    same_at(&ticks, start, located)?;
    let window: Vec<usize> = (1..ticks.len())
        .filter(|&tick| inside(&ticks, tick, tick, polls))
        .collect();
    if window.is_empty() {
        return Err(format!("the run never reached polls {}..{}", polls.0, polls.1).into());
    }
    let name = options.name.as_deref().unwrap_or("run");
    let sum = |value: fn(&Tick) -> u64| window.iter().map(|&tick| value(&ticks[tick])).sum::<u64>();
    println!("{name}.ticks={}", window.len());
    println!("{name}.flips={}", sum(|tick| u64::from(tick.flipped)));
    println!("{name}.cycles={}", sum(|tick| tick.cycles));
    println!("{name}.icache={}", sum(|tick| tick.icache));
    if let Some((p50, p95, vblanks)) = frame_pacing(&ticks, &window) {
        println!("{name}.frame_p50={p50}");
        println!("{name}.frame_p95={p95}");
        println!("{name}.vblanks={vblanks}");
    }
    let unit = if words { 4 } else { 16 };
    let work = split_work(
        logs,
        &ticks,
        start,
        &report,
        polls.1,
        unit,
        &options.wait_ranges,
        icache_lines,
    )?;
    println!("{name}.work_instr={}", work.instructions);
    println!("{name}.work_cycles={}", work.cycles);
    println!("{name}.wait_cycles={}", work.wait_cycles);
    println!(
        "{name}.wait_share={:.2}%",
        100.0 * work.wait_cycles as f64 / work.span_cycles.max(1) as f64
    );
    if let Some(per_frame) = work.cycles.checked_div(work.frames) {
        println!("{name}.work_per_frame={per_frame}");
    }
    let frames = frame_work(&ticks, &window, &work.tick_work);
    if !frames.is_empty() {
        for (key, q) in [("p50", 0.5), ("p95", 0.95), ("p99", 0.99)] {
            println!("{name}.frame_work_{key}={}", percentile(&frames, q));
        }
    }
    // Final-state hashes at the stop poll: equal across builds only for a
    // guest whose simulation does not depend on its own speed.
    for (key, value) in report.hashes {
        println!("{name}.{key}={value}");
    }
    Ok(())
}

fn collect(guest: &Guest, options: &Options) -> Result<()> {
    let frontend = options
        .frontend
        .as_ref()
        .ok_or("collect needs --frontend")?;
    let out = options.out.as_ref().ok_or("collect needs --out PROFILE")?;
    if options.runs.is_empty() && options.launch_args.is_empty() {
        return Err("collect needs a --tape, or --launch-arg for a run without one".into());
    }

    let Linked { exe: elf, map } = guest.build_twin()?;
    let work = elf.parent().expect("the twin lives in the work directory");
    // The replayed image is cut from the twin itself, so every sampled PC
    // is an address in the ELF by construction, and the twin's map
    // describes it.
    let exe = work.join("collect.exe");
    fs::write(&exe, flat_image(&fs::read(&elf)?)?)?;
    guest.patch(&exe, map.as_deref())?;
    let disc = work.join("collect.bin");
    let image = match &options.pack {
        Some(command) => pack(command, &exe, &disc)?,
        None => exe.clone(),
    };

    let tapeless = [Run::default()];
    let runs = if options.runs.is_empty() {
        &tapeless[..]
    } else {
        &options.runs[..]
    };
    let mut logs = Vec::new();
    let mut scratch = Vec::new();
    let replayed = (|| -> Result<()> {
        for (index, run_spec) in runs.iter().enumerate() {
            let log = work.join(format!("pc-{index}.csv"));
            logs.push(log.clone());
            let mut command = launch(frontend, &image, run_spec, &options.launch_args);
            command.args(["--pc-sample-instructions", SAMPLE_INTERVAL]);
            let Some(polls) = run_spec.polls else {
                command.arg("--pc-sample-log").arg(&log);
                run(&mut command, "profiling replay")?;
                continue;
            };
            // Loading and menus would skew the profile towards CD polling,
            // so only samples from the gameplay polls are kept.
            let windows = work.join(format!("pc-windows-{index}.csv"));
            let route = work.join(format!("route-{index}.csv"));
            scratch.extend([windows.clone(), route.clone()]);
            command
                .arg("--pc-sample-window-log")
                .arg(&windows)
                .args(["--pc-sample-window-ticks", &SAMPLE_WINDOW_TICKS.to_string()])
                .arg("--route-log")
                .arg(&route);
            run(&mut command, "profiling replay")?;
            let (kept, total) = window_histogram(&windows, &read_route_log(&route)?, polls, &log)?;
            println!(
                "psoxide-pgo: polls {}..{} kept {kept} of {total} sample windows",
                polls.0, polls.1
            );
            if kept == 0 {
                return Err("no sample window fell inside the gameplay polls".into());
            }
        }
        Ok(())
    })();
    for file in &scratch {
        let _ = fs::remove_file(file);
    }
    remove_disc(&disc);
    let _ = fs::remove_file(&exe);
    let converted = replayed.and_then(|()| {
        let raw = work.join("raw.prof");
        crate::convert(&elf, &logs, &raw)?;
        crate::portable(&raw, out)?;
        // Symbol names hold no features once portable, but the code they
        // describe does: record them so `apply` can warn on a mismatch.
        let profile = fs::read_to_string(out)?;
        fs::write(
            out,
            format!("{FEATURES_TAG}{}\n{profile}", features_of(&guest.cargo)),
        )?;
        Ok(())
    });
    // The logs are only an intermediate; the profile carries what matters.
    for log in &logs {
        let _ = fs::remove_file(log);
    }
    converted?;
    println!("psoxide-pgo: portable profile -> {}", out.display());
    Ok(())
}

/// The rustflags of a variant's compile part, none for `off`. A profiled
/// variant builds the ELF twin here to rebind the profile.
fn compile_flags(guest: &Guest, profile: Option<&Path>, variant: &str) -> Result<Vec<String>> {
    let Some(extra) = variant_flags(variant)? else {
        return Ok(Vec::new());
    };
    let profile = profile.ok_or("a profiled variant needs --profile")?;
    if !profile.is_file() {
        return Err(format!("no profile at {}", profile.display()).into());
    }
    let trained = fs::read_to_string(profile)?
        .lines()
        .next()
        .and_then(|line| line.strip_prefix(FEATURES_TAG))
        .map(str::to_string);
    let building = features_of(&guest.cargo);
    if let Some(trained) = trained.filter(|trained| *trained != building) {
        eprintln!(
            "psoxide-pgo: warning: {} was trained with features {trained} and this build \
             uses {building}. Names still bind, but code that differs between the two gets \
             the other build's counts or none; keep one profile per shipped feature set.",
            profile.display()
        );
    }
    let elf = guest.build_twin()?.exe;
    let rebound = elf.with_extension("rebound.prof");
    crate::rebind(profile, &elf, &rebound)?;
    let mut flags: Vec<String> = COLLECT_FLAGS.iter().map(ToString::to_string).collect();
    flags.push(format!("-Zprofile-sample-use={}", rebound.display()));
    flags.extend(extra);
    Ok(flags)
}

/// Build one variant and return the patched executable and its map.
fn apply(
    guest: &Guest,
    profile: Option<&Path>,
    layout: Option<&Path>,
    variant: &str,
) -> Result<Linked> {
    let (compile, ordered) = split_order(variant)?;
    let flags = compile_flags(guest, profile, &compile)?;
    let linked = guest.build(&flags, false, None)?;
    guest.patch(&linked.exe, linked.map.as_deref())?;
    if !ordered {
        return Ok(linked);
    }
    let layout = layout.ok_or("an +order variant needs --layout")?;
    let order = place(guest, &linked, layout, &compile)?;
    let relinked = guest.build(&flags, false, Some(&order.path))?;
    let map = relinked
        .map
        .as_deref()
        .ok_or("the ordered relink wrote no map to check its order against")?;
    let followed = layout::check_order(
        &order.text,
        &order.placed,
        &layout::read_map(&fs::read_to_string(map)?),
    )?;
    println!(
        "psoxide-pgo: the relink put all {} ordered functions in order; {} of the {} placed \
         ones start where the model put them",
        followed.listed,
        order.placed.len() - followed.drifted,
        order.placed.len()
    );
    guest.patch(&relinked.exe, relinked.map.as_deref())?;
    Ok(relinked)
}

/// An ordering file written for one link.
struct Order {
    path: PathBuf,
    text: String,
    /// The placed functions' symbols and modelled addresses.
    placed: Vec<(String, u32)>,
}

/// Bind the layout profile onto `linked` (patched, as `order` replayed it),
/// refuse it below [`layout::MIN_BOUND`], and write the ordering file the
/// placement gives, named by its contents.
fn place(guest: &Guest, linked: &Linked, layout: &Path, compile: &str) -> Result<Order> {
    let map = linked
        .map
        .as_deref()
        .ok_or("an +order variant needs the link map, and the link wrote none")?;
    let sections = layout::read_map(&fs::read_to_string(map)?);
    let image = layout::Image::parse(&fs::read(&linked.exe)?)?;
    let profile = layout::Layout::parse(&fs::read_to_string(layout)?)
        .map_err(|error| format!("{}: {error}", layout.display()))?;
    let building = features_of(&guest.cargo);
    if profile.features != building || profile.variant != compile {
        eprintln!(
            "psoxide-pgo: warning: {} was collected on features {} and variant {}, and this \
             build is {building} and {compile}. Only functions whose code is the same bind.",
            layout.display(),
            profile.features,
            profile.variant
        );
    }
    let bound = layout::bind(&profile, &sections, &image);
    println!(
        "psoxide-pgo: layout {}: {}",
        layout.display(),
        bound.coverage
    );
    bound.coverage.check(layout::MIN_BOUND)?;
    let fixed = layout::fixed_patterns(
        &fs::read_to_string(&guest.linker_script)
            .map_err(|error| format!("{}: {error}", guest.linker_script.display()))?,
    );
    let placement = layout::place(&sections, &image, &bound, &fixed);
    println!(
        "psoxide-pgo: placed {} functions ({} cold bytes moved into gaps); predicted \
         I-cache conflict refills {:.0} -> {:.0}",
        placement.placed.len(),
        placement.gap,
        placement.before,
        placement.after
    );
    let text = layout::order_file(&sections, &placement);
    let path = guest
        .work_dir(&linked.exe)?
        .join(format!("order-{:016x}.txt", fnv1a(text.as_bytes())));
    fs::write(&path, &text)?;
    let placed = placement
        .placed
        .iter()
        .filter_map(|&(k, at)| Some((sections[k].symbol.clone()?, at)))
        .collect();
    Ok(Order { path, text, placed })
}

/// Build a variant, replay it with a count for every word over each run's
/// gameplay polls, and write the layout profile `apply ...+order` places
/// from.
fn order(guest: &Guest, options: &Options) -> Result<()> {
    let frontend = options.frontend.as_ref().ok_or("order needs --frontend")?;
    let out = options.out.as_ref().ok_or("order needs --out LAYOUT")?;
    // Collected on the compile part, which is what `+order` relinks.
    let (compile, _) = split_order(one_variant(options, "order")?)?;
    if options.runs.is_empty() || options.runs.iter().any(|run| run.polls.is_none()) {
        return Err("order needs --polls FROM..TO for every --tape (or one without a tape)".into());
    }
    if !lists(frontend, "--pc-log-words")? {
        return Err(format!("{} has no --pc-log-words", frontend.display()).into());
    }
    let linked = apply(guest, options.profile.as_deref(), None, &compile)?;
    let map = linked
        .map
        .as_deref()
        .ok_or("order needs the link map, and the link wrote none")?;
    let sections = layout::read_map(&fs::read_to_string(map)?);
    let image_bytes = fs::read(&linked.exe)?;
    let work = guest.work_dir(&linked.exe)?;
    let disc = work.join("order.bin");
    let image = match &options.pack {
        Some(command) => pack(command, &linked.exe, &disc)?,
        None => linked.exe.clone(),
    };
    let mut counts: HashMap<u32, u64> = HashMap::new();
    let mut scratch = Vec::new();
    let replayed = (|| -> Result<()> {
        for (index, spec) in options.runs.iter().enumerate() {
            let [locate_log, route, words] = ["locate", "route", "words"]
                .map(|name| work.join(format!("order-{name}-{index}.csv")));
            scratch.extend([locate_log.clone(), route.clone(), words.clone()]);
            let (start, located) =
                locate(frontend, &image, spec, &options.launch_args, &locate_log)?;
            let mut command = launch(frontend, &image, spec, &options.launch_args);
            command
                .arg("--pc-log-words")
                .arg("--route-log")
                .arg(&route)
                .arg("--pc-line-log")
                .arg(&words)
                .args(["--pc-line-start-route-tick", &start.to_string()]);
            replay(command, "layout replay")?;
            same_at(&read_route_log(&route)?, start, located)?;
            for (pc, count) in read_line_log(&words)? {
                *counts.entry(pc).or_default() += count;
            }
        }
        Ok(())
    })();
    for file in &scratch {
        let _ = fs::remove_file(file);
    }
    remove_disc(&disc);
    replayed?;
    let mut collected = layout::collect(&sections, &layout::Image::parse(&image_bytes)?, &counts);
    collected.layout.features = features_of(&guest.cargo);
    collected.layout.variant = compile;
    fs::write(out, collected.layout.to_text())?;
    let share = |part: u64| 100.0 * part as f64 / collected.instructions.max(1) as f64;
    println!(
        "psoxide-pgo: layout profile -> {}: {} functions ran; {:.2}% of instructions outside \
         them (BIOS, trampolines), {} indirect calls (jalr) not in the call graph",
        out.display(),
        collected.layout.functions.len(),
        share(collected.outside),
        collected.indirect
    );
    Ok(())
}

/// Parse `key=value` lines from a gate's output.
fn gate_values(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let (key, value) = (key.trim(), value.trim());
            let valid = !key.is_empty()
                && !value.contains(char::is_whitespace)
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            valid.then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

/// One variant's gate result.
#[derive(Clone)]
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

/// What `choose` ranks the passing variants by (`--rank`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Objective {
    /// Frames that miss their deadline, then p95 and p99 frame work, then
    /// average work. A game locked to the display shows every frame for a
    /// whole number of vblanks, so what a player sees is how many frames
    /// overrun the budget, and those are the heavy ones.
    #[default]
    Deadline,
    /// Average work cycles alone.
    Work,
}

/// The value of gate column `key` in `row`, as a number.
fn number(row: &GateRow, key: &str) -> Option<f64> {
    row.values
        .iter()
        .find(|(column, _)| column == key)
        .and_then(|(_, value)| value.parse().ok())
}

/// A `vblanks` column (`1:748,2:27`) as (vblanks, frames) pairs.
fn parse_vblanks(text: &str) -> Option<Vec<(u64, u64)>> {
    text.split(',')
        .map(|pair| {
            let (vblanks, frames) = pair.split_once(':')?;
            Some((vblanks.parse().ok()?, frames.parse().ok()?))
        })
        .collect()
}

/// The pacing most frames kept, the fewer vblanks on a tie.
fn modal_budget(histogram: &[(u64, u64)]) -> Option<u64> {
    histogram
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map(|&(vblanks, _)| vblanks)
}

/// Frames on screen for more than `budget` vblanks, and all frames.
fn missed(histogram: &[(u64, u64)], budget: u64) -> (u64, u64) {
    let over = histogram
        .iter()
        .filter(|(vblanks, _)| *vblanks > budget)
        .map(|(_, frames)| frames)
        .sum();
    (over, histogram.iter().map(|(_, frames)| frames).sum())
}

/// The gate columns named `key` or `NAME.key`, in the order they appear.
fn columns_named(rows: &[GateRow], key: &str) -> Vec<String> {
    let dotted = format!(".{key}");
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        for (column, _) in &row.values {
            if (column == key || column.ends_with(&dotted)) && !columns.contains(column) {
                columns.push(column.clone());
            }
        }
    }
    columns
}

/// One passing row's scores. Relative ones are against the baseline row,
/// averaged over the gate's replays so every tape counts the same.
#[derive(Clone, Debug, Default)]
struct Scores {
    /// Missed frames as a share of the frames presented, averaged.
    missed: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    work: Option<f64>,
}

impl Scores {
    fn deadline_order(&self, other: &Self) -> std::cmp::Ordering {
        let by = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            _ => std::cmp::Ordering::Equal,
        };
        by(self.missed, other.missed)
            .then(by(self.p95, other.p95))
            .then(by(self.p99, other.p99))
            .then(by(self.work, other.work))
    }

    fn work_order(&self, other: &Self) -> std::cmp::Ordering {
        match (self.work, other.work) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            _ => std::cmp::Ordering::Equal,
        }
    }

    /// The columns these scores add to the table, in order.
    fn columns(&self) -> Vec<(String, String)> {
        let relative = |key: &str, value: Option<f64>| {
            value.map(|value| (key.to_string(), format!("{:+.2}%", 100.0 * value)))
        };
        [
            self.missed
                .map(|value| ("missed".to_string(), format!("{:.2}%", 100.0 * value))),
            relative("p95", self.p95),
            relative("p99", self.p99),
            relative("work", self.work),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// How `choose` ranked its rows.
#[derive(Debug)]
struct Ranking {
    /// The row the relative columns are against.
    baseline: String,
    /// Each `vblanks` column, its frame budget and whether that came from
    /// the baseline's pacing rather than `--frame-budget`.
    budgets: Vec<(String, u64, bool)>,
    /// The objective the table is ordered by.
    objective: Objective,
    /// Variants best first by each objective, with their added columns.
    by_deadline: Vec<(String, Vec<(String, String)>)>,
    by_work: Vec<(String, Vec<(String, String)>)>,
}

/// Score the passing rows against `off` (or the first passing row with
/// every ranked column) and order them by `objective`, adding the `missed`,
/// `p95`, `p99` and `work` columns in front of each. `missed` counts the
/// frames each `vblanks` column shows on screen for longer than `budget`
/// vblanks, or than the baseline's most common pacing with no budget
/// given. `p95` and `p99` are the gate's `frame_work_p95` and
/// `frame_work_p99`, and `work` its `work_cycles`, against the baseline.
/// Returns `None` when the gate reports neither pacing nor work. Failed
/// rows and rows missing a ranked column keep their order at the end.
fn rank(rows: &mut Vec<GateRow>, budget: Option<u64>, objective: Objective) -> Option<Ranking> {
    let work_columns = columns_named(rows, "work_cycles");
    let pacing_columns = columns_named(rows, "vblanks");
    if work_columns.is_empty() && pacing_columns.is_empty() {
        return None;
    }
    let pacing = |row: &GateRow| -> Option<Vec<Vec<(u64, u64)>>> {
        pacing_columns
            .iter()
            .map(|column| {
                let (_, text) = row.values.iter().find(|(key, _)| key == column)?;
                parse_vblanks(text)
            })
            .collect()
    };
    let complete = |row: &GateRow| {
        row.passed
            && pacing(row).is_some()
            && work_columns
                .iter()
                .all(|column| number(row, column).is_some())
    };
    let baseline = rows
        .iter()
        .filter(|row| row.variant == "off")
        .chain(rows.iter())
        .find(|row| complete(row))?
        .clone();
    let baseline = &baseline;
    let base_pacing = pacing(baseline).expect("the baseline is complete");
    let budgets: Vec<(String, u64, bool)> = pacing_columns
        .iter()
        .zip(&base_pacing)
        .map(|(column, histogram)| match budget {
            Some(budget) => (column.clone(), budget, false),
            None => (column.clone(), modal_budget(histogram).unwrap_or(1), true),
        })
        .collect();
    // Frame work of each replay, where the baseline has it.
    let frame_columns = |key: &str| -> Vec<String> {
        columns_named(rows, key)
            .into_iter()
            .filter(|column| number(baseline, column).is_some())
            .collect()
    };
    let (p95_columns, p99_columns) = (
        frame_columns("frame_work_p95"),
        frame_columns("frame_work_p99"),
    );
    let relative = |row: &GateRow, columns: &[String]| -> Option<f64> {
        if columns.is_empty() {
            return None;
        }
        let mut sum = 0.0;
        for column in columns {
            sum += number(row, column)? / number(baseline, column)? - 1.0;
        }
        Some(sum / columns.len() as f64)
    };
    let score = |row: &GateRow| -> Option<Scores> {
        if !complete(row) {
            return None;
        }
        let missed = (!pacing_columns.is_empty()).then(|| {
            let shares: f64 = pacing(row)
                .expect("the row is complete")
                .iter()
                .zip(&budgets)
                .map(|(histogram, (_, budget, _))| {
                    let (over, frames) = missed(histogram, *budget);
                    over as f64 / frames.max(1) as f64
                })
                .sum();
            shares / pacing_columns.len() as f64
        });
        Some(Scores {
            missed,
            p95: relative(row, &p95_columns),
            p99: relative(row, &p99_columns),
            work: relative(row, &work_columns),
        })
    };
    let baseline_name = baseline.variant.clone();
    let mut scored: Vec<(Scores, GateRow)> = Vec::new();
    let mut unscored = Vec::new();
    for row in rows.drain(..) {
        match score(&row) {
            Some(scores) => scored.push((scores, row)),
            None => unscored.push(row),
        }
    }
    // Without pacing there is no deadline, and without work cycles no
    // average: rank by what the gate gave.
    let objective = match objective {
        Objective::Deadline if pacing_columns.is_empty() => Objective::Work,
        Objective::Work if work_columns.is_empty() => Objective::Deadline,
        objective => objective,
    };
    let listed = |order: &dyn Fn(&Scores, &Scores) -> std::cmp::Ordering| {
        let mut sorted: Vec<&(Scores, GateRow)> = scored.iter().collect();
        sorted.sort_by(|a, b| order(&a.0, &b.0));
        sorted
            .into_iter()
            .map(|(scores, row)| (row.variant.clone(), scores.columns()))
            .collect::<Vec<_>>()
    };
    let by_deadline = if pacing_columns.is_empty() {
        Vec::new()
    } else {
        listed(&Scores::deadline_order)
    };
    let by_work = if work_columns.is_empty() {
        Vec::new()
    } else {
        listed(&Scores::work_order)
    };
    scored.sort_by(|a, b| match objective {
        Objective::Deadline => a.0.deadline_order(&b.0),
        Objective::Work => a.0.work_order(&b.0),
    });
    for (scores, mut row) in scored {
        for (at, column) in scores.columns().into_iter().enumerate() {
            row.values.insert(at, column);
        }
        rows.push(row);
    }
    rows.extend(unscored);
    Some(Ranking {
        baseline: baseline_name,
        budgets,
        objective,
        by_deadline,
        by_work,
    })
}

/// The rankings under the table: the budgets, then both orders, best first.
fn format_ranking(ranking: &Ranking, rows: &[GateRow]) -> String {
    let mut out = String::new();
    let base = &ranking.baseline;
    for (column, budget, derived) in &ranking.budgets {
        let plural = if *budget == 1 { "" } else { "s" };
        let source = if *derived {
            let pacing = rows
                .iter()
                .find(|row| &row.variant == base)
                .and_then(|row| row.values.iter().find(|(key, _)| key == column))
                .map_or("-", |(_, value)| value.as_str());
            format!("{base}'s most common pacing, {pacing}; --frame-budget N sets it")
        } else {
            "--frame-budget".to_string()
        };
        let _ = writeln!(
            out,
            "Frame budget for {column}: {budget} vblank{plural} a frame ({source})."
        );
    }
    let list = |out: &mut String, ranked: &[(String, Vec<(String, String)>)]| {
        let width = ranked
            .iter()
            .map(|(variant, _)| variant.len())
            .max()
            .unwrap_or(0);
        for (place, (variant, columns)) in ranked.iter().enumerate() {
            let columns: Vec<String> = columns
                .iter()
                .map(|(key, value)| format!("{key} {value}"))
                .collect();
            let _ = writeln!(
                out,
                "  {}. {variant:<width$}  {}",
                place + 1,
                columns.join("  ")
            );
        }
    };
    if !ranking.by_deadline.is_empty() {
        let _ = writeln!(
            out,
            "By deadline: frames shown longer than the budget (`missed`), then frame work at \
             p95 and p99 (`p95`, `p99`), then average work (`work`), each against {base}:"
        );
        list(&mut out, &ranking.by_deadline);
    }
    if !ranking.by_work.is_empty() {
        let _ = writeln!(out, "By average work (`work`, against {base}):");
        let work_only: Vec<(String, Vec<(String, String)>)> = ranking
            .by_work
            .iter()
            .map(|(variant, columns)| {
                let work = columns.iter().filter(|(key, _)| key == "work").cloned();
                (variant.clone(), work.collect())
            })
            .collect();
        list(&mut out, &work_only);
    }
    let _ = writeln!(
        out,
        "The table follows the {} ranking{}.",
        match ranking.objective {
            Objective::Deadline => "deadline",
            Objective::Work => "average-work",
        },
        match (
            ranking.objective,
            ranking.by_deadline.is_empty(),
            ranking.by_work.is_empty()
        ) {
            (Objective::Deadline, _, false) => "; --rank work orders it by average work",
            (Objective::Work, false, _) => "; --rank deadline orders it by missed frames",
            _ => "",
        }
    );
    out
}

fn choose(guest: &Guest, options: &Options) -> Result<()> {
    let gate = options.gate.as_ref().ok_or("choose needs --gate CMD")?;
    let variants: Vec<String> = if options.variants.is_empty() {
        let mut variants = [
            "off",
            "default",
            "hot=500",
            "hot=500+profi",
            "accurate+nopgso+hot=1000",
            "accurate+nopgso+hot=1500",
        ]
        .map(String::from)
        .to_vec();
        // A layout profile binds onto the variant it was collected on, so
        // that one is the candidate for its order.
        if let Some(layout) = &options.layout {
            let collected_on = layout::Layout::parse(&fs::read_to_string(layout)?)
                .map_err(|error| format!("{}: {error}", layout.display()))?
                .variant;
            if !variants.contains(&collected_on) {
                variants.push(collected_on.clone());
            }
            variants.push(format!("{collected_on}+order"));
        }
        variants
    } else {
        options.variants.clone()
    };
    for variant in &variants {
        variant_flags(&split_order(variant)?.0)?;
    }
    let mut rows = Vec::new();
    for variant in &variants {
        println!("psoxide-pgo: building variant {variant}");
        let built = apply(
            guest,
            options.profile.as_deref(),
            options.layout.as_deref(),
            variant,
        );
        let exe = match built {
            Ok(linked) => linked.exe,
            Err(error) => {
                eprintln!("psoxide-pgo: variant {variant} did not build: {error}");
                rows.push(GateRow {
                    variant: variant.clone(),
                    passed: false,
                    values: vec![("build".to_string(), "failed".to_string())],
                });
                continue;
            }
        };
        let disc = guest.work_dir(&exe)?.join("choose.bin");
        let image = match &options.pack {
            Some(command) => Some(pack(command, &exe, &disc)?),
            None => None,
        };
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(gate)
            .env("PSOXIDE_PGO", env::current_exe()?)
            .env("PSOXIDE_PGO_VARIANT", variant)
            .env("PSOXIDE_PGO_EXE", &exe)
            .env("PSOXIDE_PGO_IMAGE", image.as_ref().unwrap_or(&exe))
            .stdout(Stdio::piped());
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
    let ranking = rank(&mut rows, options.frame_budget, options.objective);
    print!("\n{}", format_table(&rows));
    if let Some(ranking) = ranking {
        print!("{}", format_ranking(&ranking, &rows));
    }
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
    fn link_maps_are_named_by_what_selects_the_link() {
        let target = env::temp_dir().join(format!("psoxide-pgo-maps-{}", std::process::id()));
        let guest = |cargo: &[&str]| Guest {
            crate_dir: PathBuf::from("/game"),
            cargo: cargo.iter().map(|arg| arg.to_string()).collect(),
            work: None,
            patcher: PathBuf::new(),
            scanner: PathBuf::new(),
            stack_guard: PathBuf::new(),
            linker_script: PathBuf::new(),
        };
        let spaced = guest(&["build", "--target-dir", target.to_str().unwrap()]);
        let joined = guest(&["build", &format!("--target-dir={}", target.display())]);
        let flags = |list: &[&str]| list.iter().map(|flag| flag.to_string()).collect::<Vec<_>>();
        let plain = spaced.map_path(&[]).unwrap();
        assert_eq!(
            plain.parent().unwrap(),
            target.join(TARGET).join("psoxide-pgo-maps")
        );
        assert_eq!(spaced.map_path(&[]).unwrap(), plain);
        assert_ne!(spaced.map_path(&flags(&["-Cdebuginfo=1"])).unwrap(), plain);
        assert_ne!(joined.map_path(&[]).unwrap(), plain);
        assert_eq!(
            joined.map_path(&[]).unwrap().parent(),
            plain.parent(),
            "both spellings of --target-dir name the same directory"
        );
        fs::remove_dir_all(&target).unwrap();
    }

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
        assert_eq!(
            variant_flags("noreplay+nopgso+llvm=-sample-profile-inline-size").unwrap(),
            Some(vec![
                "-Cllvm-args=-disable-sample-loader-inlining".to_string(),
                "-Cllvm-args=-pgso=false".to_string(),
                "-Cllvm-args=-sample-profile-inline-size".to_string(),
            ])
        );
        assert!(variant_flags("hot=lots").is_err());
        assert!(variant_flags("llvm=pgso").is_err());
        assert!(variant_flags("fast").is_err());
    }

    #[test]
    fn order_is_a_link_part_of_any_variant() {
        assert_eq!(
            split_order("hot=500+profi").unwrap(),
            ("hot=500+profi".into(), false)
        );
        assert_eq!(
            split_order("hot=500+order+profi").unwrap(),
            ("hot=500+profi".into(), true)
        );
        assert_eq!(split_order("off+order").unwrap(), ("off".into(), true));
        assert!(split_order("order").is_err());
    }

    #[test]
    fn features_normalise_across_spellings() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(features_of(&args(&["build", "--release"])), "(default)");
        assert_eq!(
            features_of(&args(&[
                "build",
                "--features",
                "b a",
                "-F",
                "c",
                "--features=a"
            ])),
            "a,b,c"
        );
        assert_eq!(
            features_of(&args(&[
                "build",
                "--no-default-features",
                "--features",
                "x,y"
            ])),
            "no-default-features,x,y"
        );
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
        let values = gate_values(
            "replaying\ncycles=123\n unseen.cycles = 456 \nnot a pair\n=x\n\
             tick=1  cycles=2  pc=0x80010ca8\n",
        );
        assert_eq!(
            values,
            vec![
                ("cycles".to_string(), "123".to_string()),
                ("unseen.cycles".to_string(), "456".to_string()),
            ]
        );
    }

    #[test]
    fn poll_windows_parse_as_half_open_ranges() {
        assert_eq!(parse_polls("300..1400").unwrap(), (300, 1400));
        assert!(parse_polls("1400..300").is_err());
        assert!(parse_polls("300").is_err());
    }

    #[test]
    fn wait_ranges_parse_as_word_aligned_hex() {
        assert_eq!(
            parse_range("0x80090d88..0x80090e2c").unwrap(),
            (0x8009_0d88, 0x8009_0e2c)
        );
        assert_eq!(
            parse_range("800b1c90..800b1ca8").unwrap(),
            (0x800b_1c90, 0x800b_1ca8)
        );
        assert!(parse_range("0x80090e2c..0x80090d88").is_err());
        assert!(parse_range("0x80090d89..0x80090e2c").is_err());
        assert!(parse_range("0x80090d88").is_err());
    }

    fn ticks(polls: &[u64]) -> Vec<Tick> {
        polls
            .iter()
            .map(|&polls| Tick {
                polls,
                cycles: 10,
                ..Tick::default()
            })
            .collect()
    }

    #[test]
    fn a_tick_is_inside_only_when_it_starts_and_ends_in_the_window() {
        // Row 0 is the start of the run; poll 2 lands during tick 2.
        let route = ticks(&[0, 1, 2, 3, 4, 5]);
        let window = (2, 4);
        let inside_ticks: Vec<usize> = (1..route.len())
            .filter(|&tick| inside(&route, tick, tick, window))
            .collect();
        assert_eq!(inside_ticks, vec![3, 4]);
        assert!(!inside(&route, 3, 9, window));
    }

    #[test]
    fn sample_windows_outside_the_gameplay_polls_are_dropped() {
        let dir = env::temp_dir().join(format!("psoxide-pgo-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (windows, out) = (dir.join("windows.csv"), dir.join("pc.csv"));
        // One poll per tick; windows of SAMPLE_WINDOW_TICKS ticks.
        let route = ticks(&(0..=3 * SAMPLE_WINDOW_TICKS as u64).collect::<Vec<_>>());
        let w = SAMPLE_WINDOW_TICKS;
        fs::write(
            &windows,
            format!(
                "window_start_tick,pc,samples,percent_window\n\
                 0,0x80010000,5,1\n{w},0x80010004,7,1\n{w},0x80010000,1,1\n{},0x80010004,9,1\n",
                2 * w
            ),
        )
        .unwrap();
        // Polls w..2w cover exactly the middle window.
        let kept = window_histogram(&windows, &route, (w as u64, 2 * w as u64), &out).unwrap();
        assert_eq!(kept, (1, 3));
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "pc,samples\n0x80010004,7\n0x80010000,1\n"
        );
        fs::remove_dir_all(&dir).unwrap();
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

    fn row(variant: &str, passed: bool, values: &[(&str, &str)]) -> GateRow {
        GateRow {
            variant: variant.to_string(),
            passed,
            values: values
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        }
    }

    #[test]
    fn choose_ranks_passing_rows_by_work_against_off() {
        let mut rows = vec![
            row(
                "default",
                true,
                &[("a.work_cycles", "110"), ("b.work_cycles", "220")],
            ),
            row(
                "off",
                true,
                &[("a.work_cycles", "100"), ("b.work_cycles", "200")],
            ),
            row(
                "hot=500",
                false,
                &[("a.work_cycles", "50"), ("b.work_cycles", "50")],
            ),
            row(
                "profi",
                true,
                &[("a.work_cycles", "98"), ("b.work_cycles", "194")],
            ),
        ];
        let ranking = rank(&mut rows, None, Objective::Work).unwrap();
        assert_eq!(ranking.baseline, "off");
        let order: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| {
                let work = row.values.iter().find(|(key, _)| key == "work");
                (
                    row.variant.as_str(),
                    work.map_or("-", |(_, value)| value.as_str()),
                )
            })
            .collect();
        // profi: (-2% + -3%) / 2. The failed row is last and unranked.
        assert_eq!(
            order,
            vec![
                ("profi", "-2.50%"),
                ("off", "+0.00%"),
                ("default", "+10.00%"),
                ("hot=500", "-"),
            ]
        );
        let mut plain = vec![row("off", true, &[("cycles", "1")])];
        assert!(rank(&mut plain, None, Objective::Deadline).is_none());
        assert_eq!(plain[0].values.len(), 1);
    }

    #[test]
    fn choose_ranks_a_losing_order_below_its_variant() {
        // Quake's I-cache study: an order trained on E1M1 alone, gated on
        // E1M1 and E1M2 (prototype builds, 2026-09-22 frontend).
        let mut rows = vec![
            row(
                "base",
                true,
                &[
                    ("e1m1.work_cycles", "1581957371"),
                    ("e1m2.work_cycles", "2130733102"),
                ],
            ),
            row(
                "base+order",
                true,
                &[
                    ("e1m1.work_cycles", "1585393788"),
                    ("e1m2.work_cycles", "2147913376"),
                ],
            ),
        ];
        let ranking = rank(&mut rows, None, Objective::Deadline).unwrap();
        assert_eq!(ranking.baseline, "base");
        assert_eq!(
            ranking.objective,
            Objective::Work,
            "a gate without pacing ranks by work"
        );
        let order: Vec<&str> = rows.iter().map(|row| row.variant.as_str()).collect();
        assert_eq!(order, vec!["base", "base+order"]);
        assert_eq!(
            rows[1].values[0],
            ("work".to_string(), "+0.51%".to_string())
        );
    }

    #[test]
    fn frames_are_paced_between_flips() {
        // One tick per vblank of 10 cycles; flips at ticks 2, 4, 7 and 9.
        let route: Vec<Tick> = (0..10u64)
            .map(|tick| Tick {
                cycles_total: 10 * tick,
                flipped: matches!(tick, 2 | 4 | 7 | 9),
                ..Tick::default()
            })
            .collect();
        let window: Vec<usize> = (1..10).collect();
        assert_eq!(
            frame_pacing(&route, &window),
            Some((20, 30, "2:2,3:1".to_string()))
        );
        assert_eq!(frame_pacing(&route, &[1, 2, 3]), None);
    }

    #[test]
    fn percentiles_take_the_nearest_rank() {
        let sorted: Vec<u64> = (1..=20).collect();
        assert_eq!(percentile(&sorted, 0.5), 10);
        assert_eq!(percentile(&sorted, 0.95), 19);
        assert_eq!(percentile(&[7], 0.95), 7);
    }

    #[test]
    fn the_final_report_gives_totals_and_hashes() {
        let mut report = Report::default();
        for line in [
            "voxide: boot",
            "[guest f1 c2] frame cycles=5",
            "tick=709920489  cycles=1460035978  pc=0x80033504  stopped-at=709920489",
            "route-ticks=2555  port1-polls=1200",
            "vram_fnv1a_64=0xb3051a3293c03c2a",
            "display_fnv1a_64=0x67d276946ed10327  w=320  h=240",
        ] {
            report.read(line);
        }
        assert_eq!(report.instructions, Some(709_920_489));
        assert_eq!(report.cycles, Some(1_460_035_978));
        assert_eq!(report.polls, Some(1200));
        assert_eq!(
            report.hashes,
            vec![
                ("vram", "0xb3051a3293c03c2a".to_string()),
                ("display", "0x67d276946ed10327".to_string()),
            ]
        );
    }

    #[test]
    fn line_logs_parse_by_address() {
        let path = env::temp_dir().join(format!("psoxide-pgo-lines-{}.csv", std::process::id()));
        fs::write(
            &path,
            "line_pc,instructions,percent\n0x80033490,57217069,10.28\n0xbfc00180,3,0.00\n",
        )
        .unwrap();
        let lines = read_line_log(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(lines.get(&0x8003_3490), Some(&57_217_069));
        assert_eq!(lines.get(&0xbfc0_0180), Some(&3));
    }

    fn variants(rows: &[GateRow]) -> Vec<&str> {
        rows.iter().map(|row| row.variant.as_str()).collect()
    }

    fn value<'a>(row: &'a GateRow, key: &str) -> &'a str {
        row.values
            .iter()
            .find(|(column, _)| column == key)
            .map_or("-", |(_, value)| value.as_str())
    }

    /// A 60 fps game where the profiled build does less work on average
    /// but more on its heavy frames, which are the ones that miss a vblank.
    fn sixty_fps_rows() -> Vec<GateRow> {
        vec![
            row(
                "hot=500+profi",
                true,
                &[
                    ("train.vblanks", "1:700,2:50"),
                    ("train.work_cycles", "990"),
                    ("train.frame_work_p95", "580"),
                    ("train.frame_work_p99", "640"),
                ],
            ),
            row(
                "off",
                true,
                &[
                    ("train.vblanks", "1:750,2:25"),
                    ("train.work_cycles", "1000"),
                    ("train.frame_work_p95", "550"),
                    ("train.frame_work_p99", "620"),
                ],
            ),
        ]
    }

    #[test]
    fn choose_ranks_missed_frames_before_average_work() {
        let mut rows = sixty_fps_rows();
        let ranking = rank(&mut rows, None, Objective::Deadline).unwrap();
        assert_eq!(
            ranking.budgets,
            vec![("train.vblanks".to_string(), 1, true)]
        );
        assert_eq!(variants(&rows), vec!["off", "hot=500+profi"]);
        assert_eq!(value(&rows[0], "missed"), "3.23%");
        assert_eq!(value(&rows[1], "missed"), "6.67%");
        assert_eq!(value(&rows[1], "p95"), "+5.45%");
        assert_eq!(value(&rows[1], "work"), "-1.00%");
        let deadline: Vec<&str> = ranking
            .by_deadline
            .iter()
            .map(|(v, _)| v.as_str())
            .collect();
        let work: Vec<&str> = ranking.by_work.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(deadline, vec!["off", "hot=500+profi"]);
        assert_eq!(work, vec!["hot=500+profi", "off"]);
        let text = format_ranking(&ranking, &rows);
        assert!(
            text.contains("train.vblanks: 1 vblank a frame (off's most common pacing, 1:750,2:25")
        );
        assert!(text.contains("--rank work orders it by average work"));

        // The old order stays one flag away.
        let mut rows = sixty_fps_rows();
        let ranking = rank(&mut rows, None, Objective::Work).unwrap();
        assert_eq!(ranking.objective, Objective::Work);
        assert_eq!(variants(&rows), vec!["hot=500+profi", "off"]);
    }

    #[test]
    fn equal_misses_fall_to_frame_work_then_average_work() {
        // Locked at 30 fps: no frame misses two vblanks, so the heavy
        // frames decide, and the average only once those tie too.
        let locked = |p95: &str, p99: &str, work: &str| {
            [
                ("a.vblanks".to_string(), "2:900".to_string()),
                ("a.frame_work_p95".to_string(), p95.to_string()),
                ("a.frame_work_p99".to_string(), p99.to_string()),
                ("a.work_cycles".to_string(), work.to_string()),
            ]
        };
        let row_of = |variant: &str, values: [(String, String); 4]| GateRow {
            variant: variant.to_string(),
            passed: true,
            values: values.to_vec(),
        };
        let mut rows = vec![
            row_of("off", locked("900", "990", "1000")),
            row_of("cheap-average", locked("910", "990", "900")),
            row_of("cheap-p99", locked("900", "980", "1010")),
            row_of("same-heavy", locked("900", "990", "990")),
        ];
        let ranking = rank(&mut rows, None, Objective::Deadline).unwrap();
        assert_eq!(ranking.budgets, vec![("a.vblanks".to_string(), 2, true)]);
        assert_eq!(
            variants(&rows),
            vec!["cheap-p99", "same-heavy", "off", "cheap-average"]
        );
        assert!(rows.iter().all(|row| value(row, "missed") == "0.00%"));
    }

    #[test]
    fn a_frame_budget_overrides_the_observed_pacing() {
        let mut rows = sixty_fps_rows();
        let ranking = rank(&mut rows, Some(2), Objective::Deadline).unwrap();
        assert_eq!(
            ranking.budgets,
            vec![("train.vblanks".to_string(), 2, false)]
        );
        // Nothing takes three vblanks, so frame work decides.
        assert_eq!(variants(&rows), vec!["off", "hot=500+profi"]);
        assert_eq!(value(&rows[1], "missed"), "0.00%");
        assert!(format_ranking(&ranking, &rows).contains("2 vblanks a frame (--frame-budget)"));
    }

    #[test]
    fn missed_frames_average_over_replays_and_skip_failures() {
        let mut rows = vec![
            row(
                "off",
                true,
                &[("a.vblanks", "1:90,2:10"), ("b.vblanks", "1:100")],
            ),
            row(
                "default",
                true,
                &[("a.vblanks", "1:100"), ("b.vblanks", "1:80,3:20")],
            ),
            row(
                "hot=500",
                false,
                &[("a.vblanks", "1:100"), ("b.vblanks", "1:100")],
            ),
        ];
        let ranking = rank(&mut rows, None, Objective::Deadline).unwrap();
        assert_eq!(variants(&rows), vec!["off", "default", "hot=500"]);
        assert_eq!(value(&rows[0], "missed"), "5.00%");
        assert_eq!(value(&rows[1], "missed"), "10.00%");
        assert_eq!(value(&rows[2], "missed"), "-");
        assert!(ranking.by_work.is_empty());
    }

    #[test]
    fn pacing_histograms_parse_and_count_misses() {
        let histogram = parse_vblanks("1:748,2:27,3:1").unwrap();
        assert_eq!(histogram, vec![(1, 748), (2, 27), (3, 1)]);
        assert_eq!(missed(&histogram, 1), (28, 776));
        assert_eq!(missed(&histogram, 2), (1, 776));
        assert_eq!(modal_budget(&histogram), Some(1));
        assert_eq!(modal_budget(&[(2, 5), (3, 5)]), Some(2));
        assert!(parse_vblanks("1:2,x").is_none());
    }

    #[test]
    fn frame_budget_and_rank_flags_parse() {
        let args = |list: &[&str]| list.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        let options = parse(
            "choose",
            &args(&["--frame-budget", "2", "--rank", "work", "--", "build"]),
        )
        .unwrap();
        assert_eq!(options.frame_budget, Some(2));
        assert_eq!(options.objective, Objective::Work);
        assert_eq!(
            parse("choose", &args(&["--", "build"])).unwrap().objective,
            Objective::Deadline
        );
        assert!(parse("choose", &args(&["--frame-budget", "0", "--", "build"])).is_err());
        assert!(parse("choose", &args(&["--rank", "fps", "--", "build"])).is_err());
    }

    #[test]
    fn a_wait_loops_stalls_are_waiting() {
        // A flip wait whose line an I-cache set shares with its hazard
        // trampoline refills on every spin (NitroXide, 0x80017d18 against
        // 0x8008fd14). Those refills wait for the same vblank.
        let (spin, work) = (0x8001_7d18, 0x8003_0000);
        let counts = HashMap::from([(spin, 1000), (work, 2000)]);
        let mmio = HashMap::from([(spin, 30)]);
        let ram_load = HashMap::from([(work, 400)]);
        let icache = HashMap::from([(spin, 5000), (work, 100)]);
        let wait = HashMap::from([(spin, 1000)]);
        assert_eq!(wait_cycles(&wait, &counts, &[&mmio, &ram_load]), 1030);
        assert_eq!(
            wait_cycles(&wait, &counts, &[&mmio, &ram_load, &icache]),
            6030
        );
        // Where a quarter of a word's count waited, so do a quarter of its
        // stalls.
        let quarter = HashMap::from([(spin, 250)]);
        assert_eq!(
            wait_cycles(&quarter, &counts, &[&mmio, &ram_load, &icache]),
            250 + 5030 / 4
        );
        let per = wait_per_instruction(&quarter, &counts, &[&mmio, &ram_load, &icache]);
        assert!((per[&spin] - 0.25 * 6030.0 / 1000.0).abs() < 1e-9);
        assert!(!per.contains_key(&work));
    }

    #[test]
    fn frames_carry_the_work_of_their_ticks() {
        // Ticks of 100 cycles; a spin at 0x100 waits 2 cycles an
        // instruction; samples every 10 instructions.
        let per_instruction = HashMap::from([(0x100, 2.0)]);
        let samples = vec![
            (1, 0x100, 3),
            (1, 0x200, 4),
            (2, 0x100, 1),
            (3, 0x104, 5),
            (4, 0x100, 0),
        ];
        let waits = tick_wait(&samples, 10, 16, &per_instruction);
        assert_eq!(waits.get(&1), Some(&60));
        assert_eq!(waits.get(&2), Some(&20));
        assert_eq!(waits.get(&3), Some(&100), "0x104 shares 0x100's line");
        let route: Vec<Tick> = (0..6u64)
            .map(|tick| Tick {
                cycles: 100,
                flipped: matches!(tick, 1 | 3 | 4),
                ..Tick::default()
            })
            .collect();
        let work: HashMap<usize, u64> = (1..6)
            .map(|tick| (tick, 100 - waits.get(&tick).copied().unwrap_or(0)))
            .collect();
        // Frames end at the flips in ticks 3 and 4: ticks 2..=3 and 4.
        assert_eq!(frame_work(&route, &[1, 2, 3, 4, 5], &work), vec![80, 100]);
    }

    #[test]
    fn tick_samples_belong_to_the_next_route_row() {
        let path = env::temp_dir().join(format!("psoxide-pgo-ticks-{}.csv", std::process::id()));
        fs::write(
            &path,
            "window_start_tick,pc,samples,percent_window\n0,0x80010000,5,50.0\n7,0x8001fffc,2,1.0\n",
        )
        .unwrap();
        let samples = read_tick_samples(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(samples, vec![(1, 0x8001_0000, 5), (8, 0x8001_fffc, 2)]);
    }
}
