//! BIOS-boot commercial route matrix.
//!
//! This is the compatibility ratchet for local commercial media. It
//! discovers local disc sheets, runs each image from a real BIOS boot,
//! applies a route input script, records route evidence, and classifies
//! the earliest visible blocker by subsystem bucket.
//!
//! ```bash
//! cargo run --manifest-path emu/Cargo.toml -p emulator-core \
//!   --example commercial_route_matrix --release -- \
//!   --root "/Users/ebonura/Downloads/ps1 games" \
//!   --steps 300000000 \
//!   --report-dir target/commercial-route-matrix/local-300m
//! ```

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use emulator_core::{button, Bus, ButtonState, Cpu};
use pad_support::{effective_mask, PadPulse};

const DEFAULT_BIOS: &str = "/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN";
const DEFAULT_GAMES_ROOT: &str = "/Users/ebonura/Downloads/ps1 games";
const DEFAULT_STEPS: u64 = 300_000_000;
const DEFAULT_SOAK_STEPS: u64 = 30_000_000;
const DEFAULT_INTERVAL: u64 = 25_000_000;
const SPU_PUMP_CYCLES: u64 = 560_000;
const SPU_FRAME_SAMPLES: usize = 735;

const SONY_LOGO_HASH: u64 = 0xa3ac_6881_0443_33d0;
const EMPTY_DISPLAY_HASH: u64 = 0xcbf2_9ce4_8422_2325;
const CTR_SCEA_STUCK_HASH: u64 = 0xbfb9_bb04_fb70_42d8;
const METAL_SLUG_X_NO_DATA_HASH: u64 = 0x0936_9767_b12f_c5f2;

#[derive(Debug)]
struct Config {
    bios: PathBuf,
    root: PathBuf,
    discs: Vec<PathBuf>,
    report_dir: PathBuf,
    steps_override: Option<u64>,
    limit: Option<usize>,
    dump_visible: bool,
    wall_timeout: Option<Duration>,
}

#[derive(Clone, Debug)]
struct RouteSpec {
    id: String,
    title: String,
    probe_steps: u64,
    soak_steps: u64,
    pulses: Vec<PadPulse>,
    goal: RouteGoal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteGoal {
    ReachGameplay,
    ReachMenuOrGameplay,
    PassDataDetection,
    TriageOnly,
}

impl RouteGoal {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReachGameplay => "reach-gameplay",
            Self::ReachMenuOrGameplay => "reach-menu-or-gameplay",
            Self::PassDataDetection => "pass-data-detection",
            Self::TriageOnly => "triage-only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
enum Bucket {
    PlayableCandidate,
    RouteProgress,
    BootLicense,
    DataDetection,
    FmvMdec,
    MenuInput,
    RenderGpu,
    SpuHang,
    MemoryCardSio,
    Stalled,
    Loader,
    Unknown,
}

impl Bucket {
    fn as_str(self) -> &'static str {
        match self {
            Self::PlayableCandidate => "playable-candidate",
            Self::RouteProgress => "route-progress",
            Self::BootLicense => "boot/license",
            Self::DataDetection => "data-detection",
            Self::FmvMdec => "fmv/mdec",
            Self::MenuInput => "menu-input",
            Self::RenderGpu => "render/gpu",
            Self::SpuHang => "spu-hang",
            Self::MemoryCardSio => "memory-card/sio",
            Self::Stalled => "stalled",
            Self::Loader => "loader",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug)]
struct RouteResult {
    game: String,
    disc: PathBuf,
    route: RouteSpec,
    steps: u64,
    elapsed_secs: f64,
    error: Option<String>,
    bucket: Bucket,
    reason: String,
    pc: u32,
    cycles: u64,
    vblank: u64,
    display_hash: u64,
    display_size: (u32, u32),
    display_stats: DisplayStats,
    gp0_uploads: u32,
    gp0_rects: u32,
    gp0_polys: u32,
    dma_starts: [u64; 7],
    cdrom_cmds: Vec<(u8, u32)>,
    cdrom_irq_counts: [u64; 6],
    cdrom_sector_events: u64,
    cdrom_fifo_pops: u64,
    cdrom_fifo_len: usize,
    mdec_commands: u64,
    mdec_macroblocks: u64,
    pad_polls: u32,
    memcard_cmds: u32,
    applied_input_vblanks: u64,
    parity_command: String,
    screenshot: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default)]
struct DisplayStats {
    nonzero_pixels: usize,
    distinct_sampled_colors: usize,
    dominant_sample_count: usize,
    sampled_pixels: usize,
}

fn main() {
    let cfg = parse_args();
    if !cfg.bios.is_file() {
        eprintln!("BIOS not found: {}", cfg.bios.display());
        std::process::exit(2);
    }
    fs::create_dir_all(&cfg.report_dir).expect("create report dir");

    let discs = discover_discs(&cfg);
    if discs.is_empty() {
        eprintln!("No CUE/CCD sheets found. Pass --disc PATH or --root PATH.");
        std::process::exit(2);
    }

    println!(
        "commercial route matrix: games={} bios={} report={}",
        discs.len(),
        cfg.bios.display(),
        cfg.report_dir.display()
    );
    println!(
        "{:<42} {:<22} {:<19} {:>12} {:>10} {:>8} {:>8}  reason",
        "game", "route", "bucket", "hash", "cdread", "pad", "mdec"
    );
    println!("{}", "-".repeat(150));

    let bios = fs::read(&cfg.bios).expect("BIOS readable");
    let mut results = Vec::with_capacity(discs.len());
    for disc in discs {
        let route = route_for_disc(&disc);
        let result = run_route(&cfg, &bios, &disc, route);
        println!(
            "{:<42} {:<22} {:<19} 0x{:016x} {:>10} {:>8} {:>8}  {}",
            truncate(&result.game, 42),
            truncate(&result.route.id, 22),
            result.bucket.as_str(),
            result.display_hash,
            result.cdrom_irq_counts[1],
            result.pad_polls,
            result.mdec_macroblocks,
            result.reason
        );
        results.push(result);
    }

    write_reports(&cfg, &results).expect("write route matrix reports");
    print_summary(&results);
}

fn parse_args() -> Config {
    let mut cfg = Config {
        bios: std::env::var("PSOXIDE_BIOS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_BIOS)),
        root: std::env::var("PSOXIDE_GAMES_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_GAMES_ROOT)),
        discs: Vec::new(),
        report_dir: default_report_dir(),
        steps_override: None,
        limit: None,
        dump_visible: false,
        wall_timeout: Some(Duration::from_secs(300)),
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bios" => cfg.bios = take_path(&mut args, "--bios"),
            "--root" => cfg.root = take_path(&mut args, "--root"),
            "--disc" => cfg.discs.push(take_path(&mut args, "--disc")),
            "--steps" => cfg.steps_override = Some(take_u64(&mut args, "--steps")),
            "--limit" => cfg.limit = Some(take_usize(&mut args, "--limit")),
            "--report-dir" => cfg.report_dir = take_path(&mut args, "--report-dir"),
            "--dump-visible" => cfg.dump_visible = true,
            "--wall-timeout-secs" => {
                let secs = take_u64(&mut args, "--wall-timeout-secs");
                cfg.wall_timeout = (secs > 0).then_some(Duration::from_secs(secs));
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => panic!("unknown arg {other}; pass --help"),
        }
    }
    if !cfg.report_dir.is_absolute() {
        cfg.report_dir = std::env::current_dir()
            .expect("current dir")
            .join(&cfg.report_dir);
    }
    cfg
}

fn print_help() {
    println!(
        "\
commercial_route_matrix

Options:
  --bios PATH          BIOS image (default: PSOXIDE_BIOS or {DEFAULT_BIOS})
  --root PATH          game-library root (default: PSOXIDE_GAMES_ROOT or {DEFAULT_GAMES_ROOT})
  --disc PATH          add a specific CUE/CCD; can be repeated
  --steps N            override per-route probe+soak step budget
  --limit N            run only the first N discovered games
  --report-dir PATH    output dir (default: emu/target/commercial-route-matrix/latest)
  --dump-visible       write final visible framebuffer PPMs
  --wall-timeout-secs N
                       abort an individual route after N wall seconds; 0 disables (default: 300)
"
    );
}

fn take_path(args: &mut impl Iterator<Item = String>, flag: &str) -> PathBuf {
    PathBuf::from(take_string(args, flag))
}

fn take_string(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

fn take_u64(args: &mut impl Iterator<Item = String>, flag: &str) -> u64 {
    take_string(args, flag)
        .parse()
        .unwrap_or_else(|_| panic!("{flag} requires an integer"))
}

fn take_usize(args: &mut impl Iterator<Item = String>, flag: &str) -> usize {
    take_string(args, flag)
        .parse()
        .unwrap_or_else(|_| panic!("{flag} requires an integer"))
}

fn discover_discs(cfg: &Config) -> Vec<PathBuf> {
    let mut discs = if cfg.discs.is_empty() {
        disc_support::discover_cue_files(&cfg.root)
            .unwrap_or_else(|e| panic!("discover {}: {e}", cfg.root.display()))
    } else {
        cfg.discs.clone()
    };
    discs.sort();
    discs.dedup();
    if let Some(limit) = cfg.limit {
        discs.truncate(limit);
    }
    discs
}

fn route_for_disc(disc: &Path) -> RouteSpec {
    let name = game_name(disc).to_ascii_lowercase();
    if name.contains("tekken 3") {
        return RouteSpec {
            id: "tekken3-fight-smoke".to_string(),
            title: "Repeated START/CROSS route toward arcade fight".to_string(),
            probe_steps: 720_000_000,
            soak_steps: DEFAULT_SOAK_STEPS,
            pulses: menu_mash_pulses(650, 28, 24),
            goal: RouteGoal::ReachGameplay,
        };
    }
    if name.contains("resident evil 2") {
        return RouteSpec {
            id: "re2-original-game".to_string(),
            title: "Original Game menu route toward first controllable room".to_string(),
            probe_steps: 2_200_000_000,
            soak_steps: DEFAULT_SOAK_STEPS,
            pulses: menu_mash_pulses(650, 40, 18),
            goal: RouteGoal::ReachGameplay,
        };
    }
    if name.contains("ctr - crash team racing") {
        return RouteSpec {
            id: "ctr-race-start".to_string(),
            title: "Pass SCEA splash and reach race/menu".to_string(),
            probe_steps: 300_000_000,
            soak_steps: 0,
            pulses: menu_mash_pulses(650, 30, 18),
            goal: RouteGoal::ReachMenuOrGameplay,
        };
    }
    if name.contains("metal slug x") {
        return RouteSpec {
            id: "metal-slug-x-data".to_string(),
            title: "Pass game-data detection".to_string(),
            probe_steps: 300_000_000,
            soak_steps: 0,
            pulses: menu_mash_pulses(650, 30, 18),
            goal: RouteGoal::PassDataDetection,
        };
    }
    if name.contains("marvel vs. capcom") || name.contains("street fighter") {
        return RouteSpec {
            id: "fighter-menu-mash".to_string(),
            title: "Generic Capcom fighter route toward character select/fight".to_string(),
            probe_steps: 420_000_000,
            soak_steps: DEFAULT_SOAK_STEPS,
            pulses: menu_mash_pulses(650, 34, 18),
            goal: RouteGoal::ReachGameplay,
        };
    }
    if name.contains("crash bandicoot") {
        return RouteSpec {
            id: "crash-start-route".to_string(),
            title: "Generic Crash route toward map/level control".to_string(),
            probe_steps: 500_000_000,
            soak_steps: DEFAULT_SOAK_STEPS,
            pulses: menu_mash_pulses(650, 34, 18),
            goal: RouteGoal::ReachGameplay,
        };
    }
    RouteSpec {
        id: "generic-menu-mash".to_string(),
        title: "Generic repeated START/CROSS route for triage".to_string(),
        probe_steps: DEFAULT_STEPS,
        soak_steps: 0,
        pulses: menu_mash_pulses(650, 30, 18),
        goal: RouteGoal::TriageOnly,
    }
}

fn menu_mash_pulses(first_vblank: u64, count: u64, spacing: u64) -> Vec<PadPulse> {
    let mut pulses = Vec::new();
    for i in 0..count {
        let base = first_vblank + i * spacing;
        let mask = match i % 6 {
            0 => button::START,
            1 | 2 => button::CROSS,
            3 => button::DOWN,
            4 => button::CROSS,
            _ => button::START,
        };
        pulses.push(PadPulse {
            mask,
            start_vblank: base,
            frames: 8,
        });
    }
    pulses
}

fn run_route(cfg: &Config, bios: &[u8], disc_path: &Path, route: RouteSpec) -> RouteResult {
    let game = game_name(disc_path);
    let steps = cfg
        .steps_override
        .unwrap_or(route.probe_steps.saturating_add(route.soak_steps));
    let parity_command = parity_command(disc_path, steps, &route);
    let started = Instant::now();

    let mut bus = match Bus::new(bios.to_vec()) {
        Ok(bus) => bus,
        Err(e) => {
            return errored_result(
                game,
                disc_path,
                route,
                steps,
                parity_command,
                format!("{e:?}"),
            );
        }
    };
    let disc = match disc_support::load_disc_path(disc_path) {
        Ok(disc) => disc,
        Err(e) => return errored_result(game, disc_path, route, steps, parity_command, e),
    };
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    bus.attach_memcard_port1(Vec::new());
    let mut cpu = Cpu::new();

    let mut cycles_at_last_pump = 0u64;
    let mut current_mask = u16::MAX;
    let mut applied_input_vblanks = BTreeSet::new();
    let mut error = None;

    for step in 0..steps {
        if step > 0 && step % DEFAULT_INTERVAL == 0 {
            eprintln!(
                "[route/{}] step {step}/{steps} vblank={} pc=0x{:08x}",
                game,
                bus.irq().raise_counts()[0],
                cpu.pc()
            );
        }
        if cfg
            .wall_timeout
            .is_some_and(|timeout| started.elapsed() > timeout)
        {
            error = Some(format!(
                "wall timeout after {:.1}s at step {step}/{steps}",
                started.elapsed().as_secs_f64()
            ));
            break;
        }
        if step % DEFAULT_INTERVAL == 0 {
            apply_route_input(
                &mut bus,
                &route,
                &mut current_mask,
                &mut applied_input_vblanks,
            );
        } else {
            apply_route_input(
                &mut bus,
                &route,
                &mut current_mask,
                &mut applied_input_vblanks,
            );
        }
        if let Err(e) = cpu.step(&mut bus) {
            error = Some(format!("CPU step {step}: {e:?}"));
            break;
        }
        if bus.cycles().saturating_sub(cycles_at_last_pump) > SPU_PUMP_CYCLES {
            cycles_at_last_pump = bus.cycles();
            bus.run_spu_samples(SPU_FRAME_SAMPLES);
            let _ = bus.spu.drain_audio();
        }
    }

    let screenshot = if cfg.dump_visible {
        let dir = cfg.report_dir.join(sanitize_filename(&game));
        fs::create_dir_all(&dir).expect("create screenshot dir");
        let path = dir.join(format!("{}.ppm", route.id));
        dump_visible_ppm(&bus, &path).expect("dump visible PPM");
        Some(path)
    } else {
        None
    };

    let snapshot = snapshot(&bus, &cpu);
    let (bucket, reason) = classify(&route, &snapshot, error.as_deref());
    RouteResult {
        game,
        disc: disc_path.to_path_buf(),
        route,
        steps,
        elapsed_secs: started.elapsed().as_secs_f64(),
        error,
        bucket,
        reason,
        pc: snapshot.pc,
        cycles: snapshot.cycles,
        vblank: snapshot.vblank,
        display_hash: snapshot.display_hash,
        display_size: snapshot.display_size,
        display_stats: snapshot.display_stats,
        gp0_uploads: snapshot.gp0_uploads,
        gp0_rects: snapshot.gp0_rects,
        gp0_polys: snapshot.gp0_polys,
        dma_starts: snapshot.dma_starts,
        cdrom_cmds: snapshot.cdrom_cmds,
        cdrom_irq_counts: snapshot.cdrom_irq_counts,
        cdrom_sector_events: snapshot.cdrom_sector_events,
        cdrom_fifo_pops: snapshot.cdrom_fifo_pops,
        cdrom_fifo_len: snapshot.cdrom_fifo_len,
        mdec_commands: snapshot.mdec_commands,
        mdec_macroblocks: snapshot.mdec_macroblocks,
        pad_polls: snapshot.pad_polls,
        memcard_cmds: snapshot.memcard_cmds,
        applied_input_vblanks: applied_input_vblanks.len() as u64,
        parity_command,
        screenshot,
    }
}

fn apply_route_input(
    bus: &mut Bus,
    route: &RouteSpec,
    current_mask: &mut u16,
    applied_input_vblanks: &mut BTreeSet<u64>,
) {
    let vblank = bus.irq().raise_counts()[0];
    let mask = effective_mask(0, &route.pulses, vblank);
    if mask != 0 {
        applied_input_vblanks.insert(vblank);
    }
    if mask != *current_mask {
        bus.set_port1_buttons(ButtonState::from_bits(mask));
        *current_mask = mask;
    }
}

#[derive(Debug)]
struct Snapshot {
    pc: u32,
    cycles: u64,
    vblank: u64,
    display_hash: u64,
    display_size: (u32, u32),
    display_stats: DisplayStats,
    gp0_uploads: u32,
    gp0_rects: u32,
    gp0_polys: u32,
    dma_starts: [u64; 7],
    cdrom_cmds: Vec<(u8, u32)>,
    cdrom_irq_counts: [u64; 6],
    cdrom_sector_events: u64,
    cdrom_fifo_pops: u64,
    cdrom_fifo_len: usize,
    mdec_commands: u64,
    mdec_macroblocks: u64,
    pad_polls: u32,
    memcard_cmds: u32,
}

fn snapshot(bus: &Bus, cpu: &Cpu) -> Snapshot {
    let (display_hash, display_width, display_height, _) = bus.gpu.display_hash();
    let gp0 = bus.gpu.gp0_opcode_histogram();
    let cdrom_cmds = bus
        .cdrom
        .command_histogram()
        .iter()
        .enumerate()
        .filter_map(|(op, &count)| (count > 0).then_some((op as u8, count)))
        .collect();
    Snapshot {
        pc: cpu.pc(),
        cycles: bus.cycles(),
        vblank: bus.irq().raise_counts()[0],
        display_hash,
        display_size: (display_width, display_height),
        display_stats: display_stats(bus),
        gp0_uploads: gp0[0xA0],
        gp0_rects: gp0[0x60]
            .saturating_add(gp0[0x64])
            .saturating_add(gp0[0x68]),
        gp0_polys: gp0[0x20]
            .saturating_add(gp0[0x24])
            .saturating_add(gp0[0x28])
            .saturating_add(gp0[0x2C])
            .saturating_add(gp0[0x30])
            .saturating_add(gp0[0x34])
            .saturating_add(gp0[0x38])
            .saturating_add(gp0[0x3C]),
        dma_starts: bus.dma_start_triggers(),
        cdrom_cmds,
        cdrom_irq_counts: bus.cdrom.irq_type_counts,
        cdrom_sector_events: bus.cdrom.sector_events_scheduled,
        cdrom_fifo_pops: bus.cdrom.data_fifo_pops(),
        cdrom_fifo_len: bus.cdrom.data_fifo_len(),
        mdec_commands: bus.mdec.commands_seen(),
        mdec_macroblocks: bus.mdec.macroblocks_decoded(),
        pad_polls: bus
            .port1_pad_command_histogram()
            .map(|hist| hist.iter().sum())
            .unwrap_or(0),
        memcard_cmds: bus
            .port1_memcard_command_histogram()
            .map(|hist| hist.iter().sum())
            .unwrap_or(0),
    }
}

fn display_stats(bus: &Bus) -> DisplayStats {
    let (rgba, width, height) = bus.gpu.display_rgba8();
    if width == 0 || height == 0 {
        return DisplayStats::default();
    }
    let stride = ((width as usize * height as usize) / 4096).max(1);
    let mut nonzero_pixels = 0usize;
    let mut sampled_pixels = 0usize;
    let mut colors = BTreeMap::<u32, usize>::new();
    for (idx, px) in rgba.chunks_exact(4).enumerate() {
        let rgb = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32;
        if rgb != 0 {
            nonzero_pixels += 1;
        }
        if idx % stride == 0 {
            sampled_pixels += 1;
            *colors.entry(rgb).or_default() += 1;
        }
    }
    let dominant_sample_count = colors.values().copied().max().unwrap_or(0);
    DisplayStats {
        nonzero_pixels,
        distinct_sampled_colors: colors.len(),
        dominant_sample_count,
        sampled_pixels,
    }
}

fn classify(route: &RouteSpec, s: &Snapshot, error: Option<&str>) -> (Bucket, String) {
    if let Some(error) = error {
        return (Bucket::Stalled, error.to_string());
    }
    if s.display_hash == EMPTY_DISPLAY_HASH {
        return (
            Bucket::BootLicense,
            "no visible display produced".to_string(),
        );
    }
    if s.display_hash == SONY_LOGO_HASH {
        return (
            Bucket::BootLicense,
            "still on BIOS Sony-logo display".to_string(),
        );
    }
    if s.display_hash == CTR_SCEA_STUCK_HASH {
        return (
            Bucket::BootLicense,
            "stuck on post-license SCEA splash".to_string(),
        );
    }
    if s.display_hash == METAL_SLUG_X_NO_DATA_HASH {
        return (
            Bucket::DataDetection,
            "game reports no data detected".to_string(),
        );
    }
    if s.memcard_cmds > 10_000 && s.cdrom_irq_counts[1] == 0 {
        return (
            Bucket::MemoryCardSio,
            "heavy memory-card traffic before data reads".to_string(),
        );
    }
    if s.mdec_commands > 0 && s.mdec_macroblocks == 0 {
        return (
            Bucket::FmvMdec,
            "MDEC commands were submitted but no macroblocks decoded".to_string(),
        );
    }
    if s.display_stats.nonzero_pixels < 256 {
        return (
            Bucket::RenderGpu,
            "visible display has too little nonzero coverage".to_string(),
        );
    }
    if s.pad_polls == 0 && route.goal != RouteGoal::PassDataDetection {
        return (
            Bucket::MenuInput,
            "route input was scheduled but the game never polled port 1".to_string(),
        );
    }
    if s.cdrom_irq_counts[1] == 0
        && s.cdrom_sector_events == 0
        && matches!(
            route.goal,
            RouteGoal::PassDataDetection | RouteGoal::ReachGameplay
        )
    {
        return (
            Bucket::DataDetection,
            "no CD data sectors delivered during route".to_string(),
        );
    }
    if s.display_stats.sampled_pixels > 0
        && s.display_stats.dominant_sample_count * 100 / s.display_stats.sampled_pixels > 97
        && s.display_stats.distinct_sampled_colors < 8
    {
        return (
            Bucket::RenderGpu,
            "display is nearly uniform after route".to_string(),
        );
    }
    if looks_like_playable_candidate(route, s) {
        return (
            Bucket::PlayableCandidate,
            "route passed structural gameplay-candidate guards; needs Redux parity pin".to_string(),
        );
    }
    if route.goal == RouteGoal::PassDataDetection {
        return (
            Bucket::RouteProgress,
            "passed data-detection guard but gameplay is not confirmed".to_string(),
        );
    }
    if s.mdec_commands > 0 || s.mdec_macroblocks > 0 {
        return (
            Bucket::FmvMdec,
            "route is active in movie/MDEC path but gameplay is not confirmed".to_string(),
        );
    }
    (
        Bucket::Unknown,
        "route made progress but no gameplay guard matched".to_string(),
    )
}

fn looks_like_playable_candidate(route: &RouteSpec, s: &Snapshot) -> bool {
    let _ = (route, s);
    false
}

fn errored_result(
    game: String,
    disc: &Path,
    route: RouteSpec,
    steps: u64,
    parity_command: String,
    error: String,
) -> RouteResult {
    RouteResult {
        game,
        disc: disc.to_path_buf(),
        route,
        steps,
        elapsed_secs: 0.0,
        error: Some(error.clone()),
        bucket: Bucket::Loader,
        reason: error,
        pc: 0,
        cycles: 0,
        vblank: 0,
        display_hash: 0,
        display_size: (0, 0),
        display_stats: DisplayStats::default(),
        gp0_uploads: 0,
        gp0_rects: 0,
        gp0_polys: 0,
        dma_starts: [0; 7],
        cdrom_cmds: Vec::new(),
        cdrom_irq_counts: [0; 6],
        cdrom_sector_events: 0,
        cdrom_fifo_pops: 0,
        cdrom_fifo_len: 0,
        mdec_commands: 0,
        mdec_macroblocks: 0,
        pad_polls: 0,
        memcard_cmds: 0,
        applied_input_vblanks: 0,
        parity_command,
        screenshot: None,
    }
}

fn write_reports(cfg: &Config, results: &[RouteResult]) -> std::io::Result<()> {
    let summary = cfg.report_dir.join("SUMMARY.md");
    let csv_path = cfg.report_dir.join("matrix.csv");
    let mut summary_file = fs::File::create(summary)?;
    let mut csv_file = fs::File::create(csv_path)?;

    writeln!(summary_file, "# Commercial Route Matrix")?;
    writeln!(summary_file)?;
    writeln!(summary_file, "BIOS: `{}`", cfg.bios.display())?;
    writeln!(summary_file)?;
    writeln!(
        summary_file,
        "| Game | Route | Title | Goal | Bucket | Reason | Steps | Hash | Display | CD data IRQs | Pad polls | MDEC MB | Next parity command |"
    )?;
    writeln!(
        summary_file,
        "|---|---|---|---|---|---|---:|---|---|---:|---:|---:|---|"
    )?;
    writeln!(
        csv_file,
        "game,disc,route,route_title,goal,bucket,reason,error,steps,elapsed_secs,pc,cycles,vblank,display_hash,width,height,nonzero_pixels,distinct_sampled_colors,dominant_sample_count,sampled_pixels,gp0_uploads,gp0_rects,gp0_polys,dma0,dma1,dma2,dma3,dma4,dma5,dma6,cdrom_cmds,cdrom_data_irqs,cdrom_sector_events,cdrom_fifo_pops,cdrom_fifo_len,mdec_commands,mdec_macroblocks,pad_polls,memcard_cmds,applied_input_vblanks,screenshot,parity_command"
    )?;

    for r in results {
        writeln!(
            summary_file,
            "| {} | `{}` | {} | `{}` | `{}` | {} | {} | `0x{:016x}` | {}x{} | {} | {} | {} | `{}` |",
            escape_md(&r.game),
            r.route.id,
            escape_md(&r.route.title),
            r.route.goal.as_str(),
            r.bucket.as_str(),
            escape_md(&r.reason),
            r.steps,
            r.display_hash,
            r.display_size.0,
            r.display_size.1,
            r.cdrom_irq_counts[1],
            r.pad_polls,
            r.mdec_macroblocks,
            r.parity_command.replace('`', "'"),
        )?;
        let row = vec![
            csv(&r.game),
            csv(&r.disc.display().to_string()),
            csv(&r.route.id),
            csv(&r.route.title),
            csv(r.route.goal.as_str()),
            csv(r.bucket.as_str()),
            csv(&r.reason),
            csv(r.error.as_deref().unwrap_or("")),
            r.steps.to_string(),
            format!("{:.3}", r.elapsed_secs),
            format!("0x{:08x}", r.pc),
            r.cycles.to_string(),
            r.vblank.to_string(),
            format!("0x{:016x}", r.display_hash),
            r.display_size.0.to_string(),
            r.display_size.1.to_string(),
            r.display_stats.nonzero_pixels.to_string(),
            r.display_stats.distinct_sampled_colors.to_string(),
            r.display_stats.dominant_sample_count.to_string(),
            r.display_stats.sampled_pixels.to_string(),
            r.gp0_uploads.to_string(),
            r.gp0_rects.to_string(),
            r.gp0_polys.to_string(),
            r.dma_starts[0].to_string(),
            r.dma_starts[1].to_string(),
            r.dma_starts[2].to_string(),
            r.dma_starts[3].to_string(),
            r.dma_starts[4].to_string(),
            r.dma_starts[5].to_string(),
            r.dma_starts[6].to_string(),
            csv(&summarize_cdrom_cmds(&r.cdrom_cmds)),
            r.cdrom_irq_counts[1].to_string(),
            r.cdrom_sector_events.to_string(),
            r.cdrom_fifo_pops.to_string(),
            r.cdrom_fifo_len.to_string(),
            r.mdec_commands.to_string(),
            r.mdec_macroblocks.to_string(),
            r.pad_polls.to_string(),
            r.memcard_cmds.to_string(),
            r.applied_input_vblanks.to_string(),
            csv(&r
                .screenshot
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()),
            csv(&r.parity_command),
        ];
        writeln!(csv_file, "{}", row.join(","))?;
    }
    Ok(())
}

fn print_summary(results: &[RouteResult]) {
    let mut counts = BTreeMap::<Bucket, usize>::new();
    for r in results {
        *counts.entry(r.bucket).or_default() += 1;
    }
    println!("{}", "-".repeat(150));
    println!("summary:");
    for (bucket, count) in counts {
        println!("  {:<19} {}", bucket.as_str(), count);
    }
}

fn parity_command(disc: &Path, steps: u64, route: &RouteSpec) -> String {
    let pulses = format_pad_pulses(&route.pulses);
    let mut cmd = format!(
        "cargo run --manifest-path emu/Cargo.toml -p emulator-core --example local_lockstep_sweep --release -- --disc \"{}\" --steps {} --interval 1000000 --no-visual",
        disc.display(),
        steps
    );
    if !pulses.is_empty() {
        cmd.push_str(&format!(" --pad-pulses \"{pulses}\""));
    }
    cmd
}

fn format_pad_pulses(pulses: &[PadPulse]) -> String {
    pulses
        .iter()
        .map(|p| format!("0x{:04x}@{}+{}", p.mask, p.start_vblank, p.frames))
        .collect::<Vec<_>>()
        .join(",")
}

fn dump_visible_ppm(bus: &Bus, path: &Path) -> std::io::Result<()> {
    let (rgba, width, height) = bus.gpu.display_rgba8();
    let mut file = fs::File::create(path)?;
    writeln!(file, "P6\n{width} {height}\n255")?;
    for px in rgba.chunks_exact(4) {
        file.write_all(&px[..3])?;
    }
    Ok(())
}

fn game_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string())
}

fn summarize_cdrom_cmds(cmds: &[(u8, u32)]) -> String {
    cmds.iter()
        .map(|(op, count)| format!("0x{op:02x}:{count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_report_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("target")
        .join("commercial-route-matrix")
        .join("latest")
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn truncate(s: &str, width: usize) -> String {
    if s.len() <= width {
        s.to_string()
    } else {
        let keep = width.saturating_sub(3);
        format!("{}...", &s[..keep])
    }
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|")
}

fn csv(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
