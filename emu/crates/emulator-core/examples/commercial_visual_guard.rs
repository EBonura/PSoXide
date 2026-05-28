//! Run local commercial-game visual guards without storing golden
//! copyrighted screenshots.
//!
//! Each guard defines the disc boot path, input pulse script, trace
//! window, and one or more structural GPU assertions. The Tekken guards
//! currently cover the mode-select/menu screen, the VS portrait
//! regression, plus early and late fight gameplay frames without storing
//! copyrighted golden screenshots.
//!
//! ```bash
//! PSOXIDE_DISC="/path/to/Tekken 3 (USA).cue" \
//! cargo run -p emulator-core --example commercial_visual_guard --release -- \
//!   --guard tekken3-vs-portrait
//! ```

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::gpu::GpuCmdLogEntry;
use emulator_core::{
    fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, Cpu, DISC_FAST_BOOT_WARMUP_STEPS,
};
use pad_support::{effective_mask, parse_pad_pulses};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

const DEFAULT_BIOS: &str = "bios/SCPH1001.BIN";
const DEFAULT_OUT_ROOT: &str = "/tmp/psoxide-commercial-guards";
const DEFAULT_TEKKEN3_DISC: &str =
    "discs/Tekken 3 (USA)/Tekken 3 (USA).cue";
const DEFAULT_GUARD: &str = "tekken3-vs-portrait";

const TEKKEN_MENU_PULSES: &str = "0x0008@100+30,0x0008@500+30,0x0008@850+30,\
     0x4000@950+10,0x4000@1100+10,0x4000@1300+10,0x4000@1500+10";

#[derive(Debug)]
struct Config {
    guard: String,
    bios: PathBuf,
    disc: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    list: bool,
    all: bool,
}

#[derive(Clone, Copy, Debug)]
struct GuardSpec {
    id: &'static str,
    title: &'static str,
    default_disc: &'static str,
    enable_at: u64,
    trace_cycles: u64,
    held_mask: u16,
    pad_pulses: &'static str,
    assertions: &'static [GuardAssertion],
}

#[derive(Clone, Copy, Debug)]
enum GuardAssertion {
    DisplaySize {
        width: u16,
        height: u16,
    },
    MirroredTexQuadOwner {
        label: &'static str,
        display_x: u16,
        display_y: u16,
    },
    MirroredTexQuadCoverage {
        label: &'static str,
        display_x: u16,
        display_y: u16,
        width: u16,
        height: u16,
        stride: u16,
        min_mirrored_samples: u32,
        min_distinct_commands: usize,
    },
    NonzeroCoverage {
        label: &'static str,
        display_x: u16,
        display_y: u16,
        width: u16,
        height: u16,
        stride: u16,
        min_nonzero_samples: u32,
    },
    TexturedCoverage {
        label: &'static str,
        display_x: u16,
        display_y: u16,
        width: u16,
        height: u16,
        stride: u16,
        min_textured_samples: u32,
        min_distinct_commands: usize,
    },
    ColorDiversity {
        label: &'static str,
        display_x: u16,
        display_y: u16,
        width: u16,
        height: u16,
        stride: u16,
        min_distinct_colors: usize,
    },
}

const TEKKEN3_MODE_SELECT_ASSERTIONS: &[GuardAssertion] = &[
    GuardAssertion::DisplaySize {
        width: 368,
        height: 480,
    },
    GuardAssertion::NonzeroCoverage {
        label: "mode-select title/logo",
        display_x: 0,
        display_y: 25,
        width: 368,
        height: 175,
        stride: 4,
        min_nonzero_samples: 900,
    },
    GuardAssertion::ColorDiversity {
        label: "mode-select title/logo colors",
        display_x: 0,
        display_y: 25,
        width: 368,
        height: 175,
        stride: 4,
        min_distinct_colors: 160,
    },
    GuardAssertion::NonzeroCoverage {
        label: "mode-select option list",
        display_x: 35,
        display_y: 250,
        width: 300,
        height: 150,
        stride: 4,
        min_nonzero_samples: 650,
    },
    GuardAssertion::ColorDiversity {
        label: "mode-select option list colors",
        display_x: 35,
        display_y: 250,
        width: 300,
        height: 150,
        stride: 4,
        min_distinct_colors: 55,
    },
];

const TEKKEN3_VS_PORTRAIT_ASSERTIONS: &[GuardAssertion] = &[
    GuardAssertion::DisplaySize {
        width: 368,
        height: 480,
    },
    GuardAssertion::MirroredTexQuadOwner {
        label: "right portrait upper band",
        display_x: 280,
        display_y: 240,
    },
    GuardAssertion::MirroredTexQuadOwner {
        label: "right portrait middle band",
        display_x: 280,
        display_y: 280,
    },
    GuardAssertion::MirroredTexQuadOwner {
        label: "right portrait lower band",
        display_x: 280,
        display_y: 360,
    },
    GuardAssertion::TexturedCoverage {
        label: "left portrait interior",
        display_x: 20,
        display_y: 110,
        width: 115,
        height: 200,
        stride: 8,
        min_textured_samples: 180,
        min_distinct_commands: 3,
    },
    GuardAssertion::ColorDiversity {
        label: "left portrait colors",
        display_x: 20,
        display_y: 110,
        width: 115,
        height: 200,
        stride: 8,
        min_distinct_colors: 80,
    },
    GuardAssertion::MirroredTexQuadCoverage {
        label: "right portrait interior",
        display_x: 236,
        display_y: 224,
        width: 110,
        height: 190,
        stride: 8,
        min_mirrored_samples: 100,
        min_distinct_commands: 3,
    },
    GuardAssertion::ColorDiversity {
        label: "right portrait colors",
        display_x: 236,
        display_y: 224,
        width: 110,
        height: 190,
        stride: 8,
        min_distinct_colors: 60,
    },
];

const TEKKEN3_EARLY_FIGHT_ASSERTIONS: &[GuardAssertion] = &[
    GuardAssertion::DisplaySize {
        width: 368,
        height: 480,
    },
    GuardAssertion::NonzeroCoverage {
        label: "fight HUD band",
        display_x: 10,
        display_y: 20,
        width: 348,
        height: 70,
        stride: 4,
        min_nonzero_samples: 350,
    },
    GuardAssertion::ColorDiversity {
        label: "fight HUD colors",
        display_x: 10,
        display_y: 20,
        width: 348,
        height: 70,
        stride: 4,
        min_distinct_colors: 50,
    },
    GuardAssertion::NonzeroCoverage {
        label: "stage background",
        display_x: 0,
        display_y: 100,
        width: 368,
        height: 340,
        stride: 8,
        min_nonzero_samples: 1_000,
    },
    GuardAssertion::ColorDiversity {
        label: "stage background colors",
        display_x: 0,
        display_y: 100,
        width: 368,
        height: 340,
        stride: 8,
        min_distinct_colors: 150,
    },
    GuardAssertion::TexturedCoverage {
        label: "left fighter",
        display_x: 85,
        display_y: 150,
        width: 90,
        height: 250,
        stride: 8,
        min_textured_samples: 140,
        min_distinct_commands: 20,
    },
    GuardAssertion::ColorDiversity {
        label: "left fighter colors",
        display_x: 85,
        display_y: 150,
        width: 90,
        height: 250,
        stride: 8,
        min_distinct_colors: 70,
    },
    GuardAssertion::TexturedCoverage {
        label: "right fighter",
        display_x: 180,
        display_y: 135,
        width: 120,
        height: 275,
        stride: 8,
        min_textured_samples: 170,
        min_distinct_commands: 20,
    },
    GuardAssertion::ColorDiversity {
        label: "right fighter colors",
        display_x: 180,
        display_y: 135,
        width: 120,
        height: 275,
        stride: 8,
        min_distinct_colors: 70,
    },
];

const TEKKEN3_LATE_FIGHT_ASSERTIONS: &[GuardAssertion] = &[
    GuardAssertion::DisplaySize {
        width: 368,
        height: 480,
    },
    GuardAssertion::NonzeroCoverage {
        label: "late fight HUD band",
        display_x: 10,
        display_y: 20,
        width: 348,
        height: 70,
        stride: 4,
        min_nonzero_samples: 350,
    },
    GuardAssertion::ColorDiversity {
        label: "late fight HUD colors",
        display_x: 10,
        display_y: 20,
        width: 348,
        height: 70,
        stride: 4,
        min_distinct_colors: 50,
    },
    GuardAssertion::NonzeroCoverage {
        label: "sky and far stage",
        display_x: 0,
        display_y: 80,
        width: 368,
        height: 190,
        stride: 8,
        min_nonzero_samples: 850,
    },
    GuardAssertion::ColorDiversity {
        label: "sky and far stage colors",
        display_x: 0,
        display_y: 80,
        width: 368,
        height: 190,
        stride: 8,
        min_distinct_colors: 120,
    },
    GuardAssertion::TexturedCoverage {
        label: "knocked-down left fighter",
        display_x: 20,
        display_y: 300,
        width: 130,
        height: 135,
        stride: 8,
        min_textured_samples: 80,
        min_distinct_commands: 12,
    },
    GuardAssertion::ColorDiversity {
        label: "knocked-down left fighter colors",
        display_x: 20,
        display_y: 300,
        width: 130,
        height: 135,
        stride: 8,
        min_distinct_colors: 55,
    },
    GuardAssertion::TexturedCoverage {
        label: "late right fighter",
        display_x: 185,
        display_y: 170,
        width: 130,
        height: 260,
        stride: 8,
        min_textured_samples: 160,
        min_distinct_commands: 20,
    },
    GuardAssertion::ColorDiversity {
        label: "late right fighter colors",
        display_x: 185,
        display_y: 170,
        width: 130,
        height: 260,
        stride: 8,
        min_distinct_colors: 70,
    },
];

const GUARDS: &[GuardSpec] = &[
    GuardSpec {
        id: "tekken3-mode-select",
        title: "Tekken 3 mode-select screen has intact title art and option list",
        default_disc: DEFAULT_TEKKEN3_DISC,
        enable_at: 200_000_000,
        trace_cycles: 20_000_000,
        held_mask: 0,
        pad_pulses: TEKKEN_MENU_PULSES,
        assertions: TEKKEN3_MODE_SELECT_ASSERTIONS,
    },
    GuardSpec {
        id: DEFAULT_GUARD,
        title: "Tekken 3 VS portraits have textured coverage and mirrored P2 TexQuads",
        default_disc: DEFAULT_TEKKEN3_DISC,
        enable_at: 300_000_000,
        trace_cycles: 60_000_000,
        held_mask: 0,
        pad_pulses: TEKKEN_MENU_PULSES,
        assertions: TEKKEN3_VS_PORTRAIT_ASSERTIONS,
    },
    GuardSpec {
        id: "tekken3-early-fight",
        title: "Tekken 3 early fight screen has HUD, stage, and textured fighters",
        default_disc: DEFAULT_TEKKEN3_DISC,
        enable_at: 490_000_000,
        trace_cycles: 30_000_000,
        held_mask: 0,
        pad_pulses: TEKKEN_MENU_PULSES,
        assertions: TEKKEN3_EARLY_FIGHT_ASSERTIONS,
    },
    GuardSpec {
        id: "tekken3-late-fight",
        title: "Tekken 3 late fight camera has HUD, sky, and textured fighters",
        default_disc: DEFAULT_TEKKEN3_DISC,
        enable_at: 690_000_000,
        trace_cycles: 30_000_000,
        held_mask: 0,
        pad_pulses: TEKKEN_MENU_PULSES,
        assertions: TEKKEN3_LATE_FIGHT_ASSERTIONS,
    },
];

fn main() {
    let cfg = parse_args();
    if cfg.list {
        list_guards();
        return;
    }

    let selected = select_guards(&cfg);
    let multiple = cfg.all || selected.len() > 1;
    let mut failures = Vec::new();
    for spec in selected {
        if let Err(err) = run_guard(spec, &cfg, multiple) {
            eprintln!("[guard/{}] FAIL: {err}", spec.id);
            failures.push(spec.id);
        }
    }

    if !failures.is_empty() {
        eprintln!("commercial visual guards failed: {}", failures.join(", "));
        std::process::exit(1);
    }
}

fn parse_args() -> Config {
    let mut cfg = Config {
        guard: std::env::var("PSOXIDE_VISUAL_GUARD").unwrap_or_else(|_| DEFAULT_GUARD.to_string()),
        bios: std::env::var("PSOXIDE_BIOS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_BIOS)),
        disc: std::env::var("PSOXIDE_DISC").ok().map(PathBuf::from),
        out_dir: std::env::var("PSOXIDE_VISUAL_GUARD_OUT")
            .ok()
            .map(PathBuf::from),
        list: false,
        all: false,
    };

    let mut positional_guard = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--guard" => cfg.guard = take_string(&mut args, "--guard"),
            "--bios" => cfg.bios = take_path(&mut args, "--bios"),
            "--disc" => cfg.disc = Some(take_path(&mut args, "--disc")),
            "--out-dir" => cfg.out_dir = Some(take_path(&mut args, "--out-dir")),
            "--list" => cfg.list = true,
            "--all" => cfg.all = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other if !other.starts_with('-') && !positional_guard => {
                cfg.guard = other.to_string();
                positional_guard = true;
            }
            other => panic!("unknown argument: {other}; pass --help"),
        }
    }

    cfg
}

fn take_string(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
}

fn take_path(args: &mut impl Iterator<Item = String>, flag: &str) -> PathBuf {
    PathBuf::from(take_string(args, flag))
}

fn print_help() {
    println!(
        "\
commercial_visual_guard

Options:
  --guard ID       guard id to run (default: {DEFAULT_GUARD})
  --bios PATH      BIOS image (default: PSOXIDE_BIOS or {DEFAULT_BIOS})
  --disc PATH      disc image/CUE (default: PSOXIDE_DISC or guard default)
  --out-dir PATH   output directory (default: {DEFAULT_OUT_ROOT}/<guard>)
  --list           list available guards
  --all            run every available guard
"
    );
}

fn list_guards() {
    println!("available commercial visual guards:");
    for guard in GUARDS {
        println!(
            "  {:<24} {:>2} checks  {}",
            guard.id,
            guard.assertions.len(),
            guard.title
        );
    }
}

fn select_guards(cfg: &Config) -> Vec<&'static GuardSpec> {
    if cfg.all {
        return GUARDS.iter().collect();
    }
    let Some(spec) = GUARDS.iter().find(|g| g.id == cfg.guard) else {
        eprintln!("unknown guard `{}`", cfg.guard);
        list_guards();
        std::process::exit(2);
    };
    vec![spec]
}

fn run_guard(spec: &GuardSpec, cfg: &Config, multiple: bool) -> Result<(), String> {
    let disc_path = cfg
        .disc
        .clone()
        .unwrap_or_else(|| PathBuf::from(spec.default_disc));
    let out_dir = guard_out_dir(spec, cfg, multiple);
    if !cfg.bios.is_file() {
        return Err(format!("BIOS not found: {}", cfg.bios.display()));
    }
    if !disc_path.is_file() {
        return Err(format!("disc not found: {}", disc_path.display()));
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;

    eprintln!(
        "[guard/{}] {} | disc={} out={}",
        spec.id,
        spec.title,
        disc_path.display(),
        out_dir.display()
    );

    let bios = std::fs::read(&cfg.bios).map_err(|e| format!("read BIOS: {e}"))?;
    let mut bus = Bus::new(bios).map_err(|e| format!("bus init: {e}"))?;
    let mut cpu = Cpu::new();
    let disc = disc_support::load_disc_path(&disc_path)?;

    warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS)
        .map_err(|e| format!("BIOS warmup: {e}"))?;
    let info = fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false)
        .map_err(|e| format!("fast boot: {e:?}"))?;
    eprintln!(
        "[guard/{}] fastboot {} entry=0x{:08x}",
        spec.id, info.boot_path, info.initial_pc
    );
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();

    let pulses = parse_pad_pulses(spec.pad_pulses)?;
    let total = spec
        .enable_at
        .checked_add(spec.trace_cycles)
        .ok_or("trace window overflows u64")?;
    let mut current_mask = u16::MAX;
    let mut tracer_enabled = false;
    for i in 0..total {
        if i & 0x1FFF == 0 {
            let vblank = bus.irq().raise_counts()[0];
            let mask = effective_mask(spec.held_mask, &pulses, vblank);
            if mask != current_mask {
                bus.set_port1_buttons(emulator_core::ButtonState::from_bits(mask));
                current_mask = mask;
            }
        }

        if !tracer_enabled && i >= spec.enable_at {
            eprintln!(
                "[guard/{}] enabling pixel-owner tracer at step {i}, vblank {}",
                spec.id,
                bus.irq().raise_counts()[0]
            );
            bus.gpu.enable_pixel_tracer();
            tracer_enabled = true;
        }

        cpu.step(&mut bus)
            .map_err(|e| format!("CPU error at step {i}: {e}"))?;
    }

    let final_ppm = out_dir.join("final.ppm");
    dump_display_ppm(&bus, &final_ppm)?;
    eprintln!(
        "[guard/{}] wrote {} ({} commands traced)",
        spec.id,
        final_ppm.display(),
        bus.gpu.cmd_log.len()
    );

    assert_guards(&bus, spec.assertions, spec.id)
}

fn guard_out_dir(spec: &GuardSpec, cfg: &Config, multiple: bool) -> PathBuf {
    match (&cfg.out_dir, multiple) {
        (Some(base), true) => base.join(spec.id),
        (Some(dir), false) => dir.clone(),
        (None, _) => PathBuf::from(DEFAULT_OUT_ROOT).join(spec.id),
    }
}

fn dump_display_ppm(bus: &Bus, path: &Path) -> Result<(), String> {
    let (rgba, w, h) = bus.gpu.display_rgba8();
    let mut f =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    writeln!(f, "P6\n{w} {h}\n255").map_err(|e| format!("write {}: {e}", path.display()))?;
    for px in rgba.chunks_exact(4) {
        f.write_all(&px[..3])
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn assert_guards(bus: &Bus, assertions: &[GuardAssertion], guard_id: &str) -> Result<(), String> {
    for assertion in assertions {
        match *assertion {
            GuardAssertion::DisplaySize { width, height } => {
                assert_display_size(bus, guard_id, width, height)?
            }
            GuardAssertion::MirroredTexQuadOwner {
                label,
                display_x,
                display_y,
            } => assert_mirrored_texquad_owner(bus, guard_id, label, display_x, display_y)?,
            GuardAssertion::MirroredTexQuadCoverage {
                label,
                display_x,
                display_y,
                width,
                height,
                stride,
                min_mirrored_samples,
                min_distinct_commands,
            } => assert_mirrored_texquad_coverage(
                bus,
                guard_id,
                Region {
                    label,
                    display_x,
                    display_y,
                    width,
                    height,
                    stride,
                },
                min_mirrored_samples,
                min_distinct_commands,
            )?,
            GuardAssertion::NonzeroCoverage {
                label,
                display_x,
                display_y,
                width,
                height,
                stride,
                min_nonzero_samples,
            } => assert_nonzero_coverage(
                bus,
                guard_id,
                Region {
                    label,
                    display_x,
                    display_y,
                    width,
                    height,
                    stride,
                },
                min_nonzero_samples,
            )?,
            GuardAssertion::TexturedCoverage {
                label,
                display_x,
                display_y,
                width,
                height,
                stride,
                min_textured_samples,
                min_distinct_commands,
            } => assert_textured_coverage(
                bus,
                guard_id,
                Region {
                    label,
                    display_x,
                    display_y,
                    width,
                    height,
                    stride,
                },
                min_textured_samples,
                min_distinct_commands,
            )?,
            GuardAssertion::ColorDiversity {
                label,
                display_x,
                display_y,
                width,
                height,
                stride,
                min_distinct_colors,
            } => assert_color_diversity(
                bus,
                guard_id,
                Region {
                    label,
                    display_x,
                    display_y,
                    width,
                    height,
                    stride,
                },
                min_distinct_colors,
            )?,
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Region {
    label: &'static str,
    display_x: u16,
    display_y: u16,
    width: u16,
    height: u16,
    stride: u16,
}

fn assert_display_size(
    bus: &Bus,
    guard_id: &str,
    expected_w: u16,
    expected_h: u16,
) -> Result<(), String> {
    let da = bus.gpu.display_area();
    if da.width != expected_w || da.height != expected_h {
        return Err(format!(
            "display size was {}x{}, expected {}x{}",
            da.width, da.height, expected_w, expected_h
        ));
    }
    eprintln!(
        "[guard/{guard_id}] ok: display size is {}x{}",
        da.width, da.height
    );
    Ok(())
}

fn assert_mirrored_texquad_owner(
    bus: &Bus,
    guard_id: &str,
    label: &str,
    display_x: u16,
    display_y: u16,
) -> Result<(), String> {
    let da = bus.gpu.display_area();
    let vx = da.x.wrapping_add(display_x);
    let vy = da.y.wrapping_add(display_y);
    let entry = bus
        .gpu
        .pixel_owner_at(vx, vy)
        .ok_or_else(|| format!("{label} display=({display_x},{display_y}) has no pixel owner"))?;

    if !matches!(entry.opcode, 0x2C..=0x2F) {
        return Err(format!(
            "{label} display=({display_x},{display_y}) owner was opcode 0x{:02x}, expected TexQuad: {}",
            entry.opcode,
            entry_summary(entry)
        ));
    }

    let v = texquad_vertices(entry).ok_or_else(|| {
        format!(
            "{label} owner cmd #{} is TexQuad but has malformed FIFO length {}",
            entry.index,
            entry.fifo.len()
        )
    })?;
    if !texquad_is_axis_aligned(v) {
        return Err(format!(
            "{label} owner cmd #{} is not axis-aligned: {}",
            entry.index,
            entry_summary(entry)
        ));
    }
    if !texquad_is_mirrored_x(v) {
        return Err(format!(
            "{label} owner cmd #{} is not mirrored on X: {}",
            entry.index,
            entry_summary(entry)
        ));
    }

    eprintln!(
        "[guard/{guard_id}] ok: {label} display=({display_x},{display_y}) \
         vram=({vx},{vy}) owner cmd #{} is mirrored TexQuad",
        entry.index
    );
    Ok(())
}

fn assert_mirrored_texquad_coverage(
    bus: &Bus,
    guard_id: &str,
    region: Region,
    min_mirrored_samples: u32,
    min_distinct_commands: usize,
) -> Result<(), String> {
    validate_region(bus, region)?;
    let da = bus.gpu.display_area();
    let mut samples = 0u32;
    let mut mirrored = 0u32;
    let mut owners = BTreeSet::new();
    for display_y in stepped(region.display_y, region.height, region.stride) {
        for display_x in stepped(region.display_x, region.width, region.stride) {
            samples += 1;
            let vx = da.x.wrapping_add(display_x);
            let vy = da.y.wrapping_add(display_y);
            let Some(entry) = bus.gpu.pixel_owner_at(vx, vy) else {
                continue;
            };
            if !matches!(entry.opcode, 0x2C..=0x2F) {
                continue;
            }
            let Some(v) = texquad_vertices(entry) else {
                continue;
            };
            if texquad_is_axis_aligned(v) && texquad_is_mirrored_x(v) {
                mirrored += 1;
                owners.insert(entry.index);
            }
        }
    }

    if mirrored < min_mirrored_samples {
        return Err(format!(
            "{} had {mirrored}/{samples} mirrored TexQuad samples, expected at least {min_mirrored_samples}",
            region.label
        ));
    }
    if owners.len() < min_distinct_commands {
        return Err(format!(
            "{} had {} distinct mirrored TexQuad owner commands, expected at least {min_distinct_commands}",
            region.label,
            owners.len()
        ));
    }
    eprintln!(
        "[guard/{guard_id}] ok: {} has {mirrored}/{samples} mirrored TexQuad samples across {} commands",
        region.label,
        owners.len()
    );
    Ok(())
}

fn assert_nonzero_coverage(
    bus: &Bus,
    guard_id: &str,
    region: Region,
    min_nonzero_samples: u32,
) -> Result<(), String> {
    validate_region(bus, region)?;
    let da = bus.gpu.display_area();
    let mut samples = 0u32;
    let mut nonzero = 0u32;
    for display_y in stepped(region.display_y, region.height, region.stride) {
        for display_x in stepped(region.display_x, region.width, region.stride) {
            samples += 1;
            let vx = da.x.wrapping_add(display_x);
            let vy = da.y.wrapping_add(display_y);
            if bus.gpu.vram.get_pixel(vx, vy) != 0 {
                nonzero += 1;
            }
        }
    }
    if nonzero < min_nonzero_samples {
        return Err(format!(
            "{} had {nonzero}/{samples} nonzero samples, expected at least {min_nonzero_samples}",
            region.label
        ));
    }
    eprintln!(
        "[guard/{guard_id}] ok: {} has {nonzero}/{samples} nonzero samples",
        region.label
    );
    Ok(())
}

fn assert_textured_coverage(
    bus: &Bus,
    guard_id: &str,
    region: Region,
    min_textured_samples: u32,
    min_distinct_commands: usize,
) -> Result<(), String> {
    validate_region(bus, region)?;
    let da = bus.gpu.display_area();
    let mut samples = 0u32;
    let mut textured = 0u32;
    let mut owners = BTreeSet::new();
    for display_y in stepped(region.display_y, region.height, region.stride) {
        for display_x in stepped(region.display_x, region.width, region.stride) {
            samples += 1;
            let vx = da.x.wrapping_add(display_x);
            let vy = da.y.wrapping_add(display_y);
            if let Some(entry) = bus.gpu.pixel_owner_at(vx, vy) {
                if opcode_is_textured(entry.opcode) {
                    textured += 1;
                    owners.insert(entry.index);
                }
            }
        }
    }

    if textured < min_textured_samples {
        return Err(format!(
            "{} had {textured}/{samples} textured samples, expected at least {min_textured_samples}",
            region.label
        ));
    }
    if owners.len() < min_distinct_commands {
        return Err(format!(
            "{} had {} distinct textured owner commands, expected at least {min_distinct_commands}",
            region.label,
            owners.len()
        ));
    }
    eprintln!(
        "[guard/{guard_id}] ok: {} has {textured}/{samples} textured samples across {} commands",
        region.label,
        owners.len()
    );
    Ok(())
}

fn assert_color_diversity(
    bus: &Bus,
    guard_id: &str,
    region: Region,
    min_distinct_colors: usize,
) -> Result<(), String> {
    validate_region(bus, region)?;
    let da = bus.gpu.display_area();
    let mut colors = BTreeSet::new();
    for display_y in stepped(region.display_y, region.height, region.stride) {
        for display_x in stepped(region.display_x, region.width, region.stride) {
            let vx = da.x.wrapping_add(display_x);
            let vy = da.y.wrapping_add(display_y);
            colors.insert(bus.gpu.vram.get_pixel(vx, vy));
        }
    }

    if colors.len() < min_distinct_colors {
        return Err(format!(
            "{} had {} distinct sampled colors, expected at least {min_distinct_colors}",
            region.label,
            colors.len()
        ));
    }
    eprintln!(
        "[guard/{guard_id}] ok: {} has {} distinct sampled colors",
        region.label,
        colors.len()
    );
    Ok(())
}

fn validate_region(bus: &Bus, region: Region) -> Result<(), String> {
    let da = bus.gpu.display_area();
    if region.stride == 0 {
        return Err(format!("{} has zero stride", region.label));
    }
    let x_end = region
        .display_x
        .checked_add(region.width)
        .ok_or_else(|| format!("{} x range overflows", region.label))?;
    let y_end = region
        .display_y
        .checked_add(region.height)
        .ok_or_else(|| format!("{} y range overflows", region.label))?;
    if x_end > da.width || y_end > da.height {
        return Err(format!(
            "{} region ({},{}) {}x{} exceeds display {}x{}",
            region.label,
            region.display_x,
            region.display_y,
            region.width,
            region.height,
            da.width,
            da.height
        ));
    }
    Ok(())
}

fn stepped(start: u16, len: u16, stride: u16) -> impl Iterator<Item = u16> {
    (start..start + len).step_by(stride as usize)
}

fn opcode_is_textured(opcode: u8) -> bool {
    matches!(
        opcode,
        0x24..=0x27
            | 0x2C..=0x2F
            | 0x34..=0x37
            | 0x3C..=0x3F
            | 0x64..=0x67
            | 0x6C..=0x6F
            | 0x74..=0x77
            | 0x7C..=0x7F
    )
}

fn texquad_vertices(entry: &GpuCmdLogEntry) -> Option<[(i32, i32); 4]> {
    if entry.fifo.len() < 8 {
        return None;
    }
    Some([
        decode_vertex(entry.fifo[1]),
        decode_vertex(entry.fifo[3]),
        decode_vertex(entry.fifo[5]),
        decode_vertex(entry.fifo[7]),
    ])
}

fn texquad_is_axis_aligned(v: [(i32, i32); 4]) -> bool {
    v[0].1 == v[1].1 && v[2].1 == v[3].1 && v[0].0 == v[2].0 && v[1].0 == v[3].0
}

fn texquad_is_mirrored_x(v: [(i32, i32); 4]) -> bool {
    v[0].0 > v[1].0
}

fn decode_vertex(word: u32) -> (i32, i32) {
    let x = ((word & 0x7FF) as i32) << 21 >> 21;
    let y = (((word >> 16) & 0x7FF) as i32) << 21 >> 21;
    (x, y)
}

fn entry_summary(entry: &GpuCmdLogEntry) -> String {
    let words = entry
        .fifo
        .iter()
        .map(|w| format!("{w:08x}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "cmd #{} op=0x{:02x} fifo=[{}]",
        entry.index, entry.opcode, words
    )
}

#[cfg(test)]
mod tests {
    use super::{opcode_is_textured, stepped};

    #[test]
    fn stepped_samples_region_without_including_end() {
        let samples: Vec<u16> = stepped(10, 13, 4).collect();
        assert_eq!(samples, vec![10, 14, 18, 22]);
    }

    #[test]
    fn textured_opcode_classifier_covers_polys_and_rects() {
        for opcode in [0x24, 0x2c, 0x34, 0x3c, 0x64, 0x6c, 0x74, 0x7c] {
            assert!(opcode_is_textured(opcode), "0x{opcode:02x}");
            assert!(opcode_is_textured(opcode | 0x03), "0x{:02x}", opcode | 0x03);
        }

        for opcode in [0x20, 0x28, 0x30, 0x38, 0x60, 0x68, 0x70, 0x78] {
            assert!(!opcode_is_textured(opcode), "0x{opcode:02x}");
        }
    }

    #[test]
    fn guard_check_counts_include_color_diversity_assertions() {
        assert_eq!(super::TEKKEN3_VS_PORTRAIT_ASSERTIONS.len(), 8);
        assert_eq!(super::TEKKEN3_EARLY_FIGHT_ASSERTIONS.len(), 9);
        assert_eq!(super::TEKKEN3_LATE_FIGHT_ASSERTIONS.len(), 9);
    }
}
