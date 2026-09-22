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

/// Route ticks per PC-sample window when training on a poll window. The
/// frontend cannot start sampling late, so it samples in windows and only
/// the ones wholly inside the gameplay polls are kept.
const SAMPLE_WINDOW_TICKS: usize = 30;

pub const USAGE: &str = "\
       psoxide-pgo collect [GUEST] --frontend PATH [--tape PATH [--polls A..B]]...
                           [--launch-arg ARG]... [--pack CMD] --out PROFILE -- CARGO-ARGS...
       psoxide-pgo apply   [GUEST] [--profile PROFILE] [--variant V] -- CARGO-ARGS...
       psoxide-pgo choose  [GUEST] --profile PROFILE --gate CMD [--variant V]... [--pack CMD]
                           -- CARGO-ARGS...
       psoxide-pgo measure --frontend PATH --image PATH [--tape PATH] --polls A..B
                           [--launch-arg ARG]... [--name NAME]
  GUEST: [--crate DIR] [--work DIR] [--patcher PATH] [--scanner PATH]
  CARGO-ARGS: what follows `cargo` in the guest's own build, starting with `build`
  V: off | default | accurate | noreplay | nopgso | profi | hot=N | llvm=-FLAG, joined with +
  A..B: the gameplay window in port-1 polls, loads excluded";

/// How to build one guest, shared by every mode.
struct Guest {
    crate_dir: PathBuf,
    cargo: Vec<String>,
    work: Option<PathBuf>,
    patcher: PathBuf,
    scanner: PathBuf,
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
    frontend: Option<PathBuf>,
    runs: Vec<Run>,
    launch_args: Vec<String>,
    pack: Option<String>,
    out: Option<PathBuf>,
    profile: Option<PathBuf>,
    variants: Vec<String>,
    gate: Option<String>,
    image: Option<PathBuf>,
    name: Option<String>,
    cargo: Vec<String>,
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
            "--launch-arg" => options.launch_args.push(value()?),
            "--pack" => options.pack = Some(value()?),
            "--out" => options.out = Some(path(value()?)?),
            "--profile" => options.profile = Some(path(value()?)?),
            "--variant" => options.variants.push(value()?),
            "--gate" => options.gate = Some(value()?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if mode != "measure" && options.cargo.first().map(String::as_str) != Some("build") {
        return Err("give the guest's cargo arguments after --, starting with `build`".into());
    }
    Ok(options)
}

/// Run `collect`, `apply`, `choose` or `measure`.
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

/// Polls past the window's first one that the locating replay runs to, so
/// the route tick in which poll FROM lands is logged before it stops.
const LOCATE_LEAD_POLLS: u64 = 30;

/// A `line_pc,value,percent` log from `--pc-line-log` or one of the stall
/// attributions, as line address to value.
fn read_line_log(path: &Path) -> Result<HashMap<u32, u64>> {
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
}

/// Split the replay's span into work and wait (see work.rs), and list the
/// wait loops on stderr so a reader can check them.
fn split_work(
    logs: &MeasureLogs,
    ticks: &[Tick],
    start: usize,
    report: &Report,
    to: u64,
) -> Result<Work> {
    let lines = read_line_log(&logs.lines)?;
    let mmio = read_line_log(&logs.mmio)?;
    let ram_load = read_line_log(&logs.ram_load)?;
    let ram = fs::read(&logs.ram)?;
    let (Some(end_instructions), Some(end_cycles)) = (report.instructions, report.cycles) else {
        return Err("the frontend printed no final tick= and cycles=".into());
    };
    let span_instructions = end_instructions - ticks[start].instructions_total;
    let counted: u64 = lines.values().sum();
    if counted != span_instructions {
        return Err(format!(
            "the PC-line log holds {counted} instructions but the route log says \
             {span_instructions} ran after route tick {start}"
        )
        .into());
    }
    let executed = lines.keys().copied().collect();
    let loops = crate::work::wait_loops(&ram, &executed);
    let mut wait_lines: Vec<u32> = loops
        .iter()
        .flat_map(|found| found.body.iter().map(|pc| pc & !15))
        .collect();
    wait_lines.sort_unstable();
    wait_lines.dedup();
    let on = |log: &HashMap<u32, u64>, line: &u32| log.get(line).copied().unwrap_or(0);
    let wait_instructions: u64 = wait_lines.iter().map(|line| on(&lines, line)).sum();
    // Issue plus the stalls charged to the loop's own loads. A spin loop
    // stays in the I-cache and does no GTE or multiply work, so the other
    // stall kinds are negligible there.
    let wait_cycles = wait_instructions
        + wait_lines
            .iter()
            .map(|line| on(&mmio, line) + on(&ram_load, line))
            .sum::<u64>();
    let span_cycles = end_cycles - ticks[start].cycles_total;
    for found in &loops {
        let mut body_lines: Vec<u32> = found.body.iter().map(|pc| pc & !15).collect();
        body_lines.dedup();
        let count: u64 = body_lines.iter().map(|line| on(&lines, line)).sum();
        if count * 1000 >= span_instructions {
            eprintln!(
                "psoxide-pgo: wait loop {:#010x}..{:#010x}: {:.2}% of instructions",
                found.start,
                found.end,
                100.0 * count as f64 / span_instructions as f64
            );
        }
    }
    // Frames presented in the span: the flips in its whole ticks, and the
    // one the replay stops on once it has passed poll `to`.
    let tail = &ticks[start + 1..];
    let stop_flip = report.polls.is_some_and(|polls| polls >= to)
        && ticks
            .last()
            .is_some_and(|last| last.instructions_total < end_instructions);
    let frames = tail.iter().filter(|tick| tick.flipped).count() as u64 + u64::from(stop_flip);
    Ok(Work {
        instructions: span_instructions - wait_instructions,
        cycles: span_cycles.saturating_sub(wait_cycles),
        wait_cycles,
        span_cycles,
        frames,
    })
}

/// The files one measuring replay writes, removed afterwards.
struct MeasureLogs {
    locate: PathBuf,
    route: PathBuf,
    lines: PathBuf,
    mmio: PathBuf,
    ram_load: PathBuf,
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
            &self.ram,
        ] {
            let _ = fs::remove_file(path);
        }
    }
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

    // The per-line logs can only start at a route tick, so a short first
    // replay finds the tick in which poll FROM lands. The emulator is
    // deterministic, so the second replay reaches it in the same state.
    let locate = Run {
        tape: spec.tape.clone(),
        polls: Some((0, (polls.0 + LOCATE_LEAD_POLLS).min(polls.1))),
    };
    let mut command = launch(frontend, image, &locate, &options.launch_args);
    command.arg("--route-log").arg(&logs.locate);
    replay(command, "locating replay")?;
    let located = read_route_log(&logs.locate)?;
    // `start` is the last tick before the window: its row is the first to
    // reach poll FROM.
    let start = located
        .iter()
        .position(|tick| tick.polls >= polls.0)
        .ok_or_else(|| format!("the run never reached poll {}", polls.0))?;
    let start_tick = start.to_string();

    let mut command = launch(frontend, image, spec, &options.launch_args);
    if !options.launch_args.iter().any(|arg| arg == "--dump-hash") {
        command.arg("--dump-hash");
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
        .arg("--dump-ram")
        .arg(&logs.ram);
    let report = replay(command, "measuring replay")?;
    let ticks = read_route_log(&logs.route)?;
    if ticks.get(start) != located.get(start) {
        return Err(format!(
            "the two replays differ at route tick {start}; measure needs a deterministic run"
        )
        .into());
    }
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
    let work = split_work(logs, &ticks, start, &report, polls.1)?;
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

/// Whether a gate column holds work cycles (`work_cycles` or `NAME.work_cycles`).
fn is_work_column(key: &str) -> bool {
    key == "work_cycles" || key.ends_with(".work_cycles")
}

/// Order the passing rows by work cycles, fastest first, and give each a
/// `work` column: its work cycles against the `off` row (or the first
/// passing row with every work column), averaged over the gate's replays so
/// each tape counts the same. Returns that baseline's name, or `None` when
/// the gate reports no work cycles. Failed rows and rows missing a work
/// column keep their order at the end.
fn rank_by_work(rows: &mut Vec<GateRow>) -> Option<String> {
    let mut columns: Vec<String> = Vec::new();
    for row in rows.iter() {
        for (key, _) in &row.values {
            if is_work_column(key) && !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    if columns.is_empty() {
        return None;
    }
    let work = |row: &GateRow| -> Option<Vec<f64>> {
        if !row.passed {
            return None;
        }
        columns
            .iter()
            .map(|column| {
                row.values
                    .iter()
                    .find(|(key, _)| key == column)
                    .and_then(|(_, value)| value.parse::<f64>().ok())
            })
            .collect()
    };
    let baseline = rows
        .iter()
        .filter(|row| row.variant == "off")
        .chain(rows.iter())
        .find_map(|row| Some((row.variant.clone(), work(row)?)))?;
    let mut scored = Vec::new();
    let mut unscored = Vec::new();
    for mut row in rows.drain(..) {
        match work(&row) {
            Some(values) => {
                let score = values
                    .iter()
                    .zip(&baseline.1)
                    .map(|(value, base)| value / base - 1.0)
                    .sum::<f64>()
                    / values.len() as f64;
                row.values
                    .insert(0, ("work".to_string(), format!("{:+.2}%", 100.0 * score)));
                scored.push((score, row));
            }
            None => unscored.push(row),
        }
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.extend(scored.into_iter().map(|(_, row)| row));
    rows.extend(unscored);
    Some(baseline.0)
}

fn choose(guest: &Guest, options: &Options) -> Result<()> {
    let gate = options.gate.as_ref().ok_or("choose needs --gate CMD")?;
    let variants: Vec<String> = if options.variants.is_empty() {
        [
            "off",
            "default",
            "hot=500",
            "hot=500+profi",
            "accurate+nopgso+hot=1000",
            "accurate+nopgso+hot=1500",
        ]
        .map(String::from)
        .to_vec()
    } else {
        options.variants.clone()
    };
    for variant in &variants {
        variant_flags(variant)?;
    }
    let mut rows = Vec::new();
    for variant in &variants {
        println!("psoxide-pgo: building variant {variant}");
        let exe = match apply(guest, options.profile.as_deref(), variant) {
            Ok(exe) => exe,
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
    let ranked = rank_by_work(&mut rows);
    print!("\n{}", format_table(&rows));
    if let Some(baseline) = ranked {
        println!(
            "Ranked by work cycles (the `work` column is against {baseline}); a faster \
             variant that draws the same frames needs fewer."
        );
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
        assert_eq!(rank_by_work(&mut rows).as_deref(), Some("off"));
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
        assert_eq!(rank_by_work(&mut plain), None);
        assert_eq!(plain[0].values.len(), 1);
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
}
