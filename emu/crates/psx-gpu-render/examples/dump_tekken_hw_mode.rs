//! Dump Tekken 3 mode-select through the live-style HW renderer path.
//!
//! This is a local diagnostic for commercial visual parity. It does
//! not store copyrighted output; it writes CPU and HW PPMs under
//! `/tmp` by default for manual inspection.

#[path = "../../emulator-core/examples/support/pad.rs"]
mod pad_support;

use emulator_core::{
    fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, Cpu, DISC_FAST_BOOT_WARMUP_STEPS,
};
use pad_support::{effective_mask, parse_pad_pulses};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const DEFAULT_BIOS: &str = "bios/SCPH1001.BIN";
const DEFAULT_DISC: &str = "discs/Tekken 3 (USA)/Tekken 3 (USA).cue";
const DEFAULT_OUT_DIR: &str = "/tmp/tekken-hw-mode";
const DEFAULT_STEPS: u64 = 220_000_000;
const TEKKEN_MENU_PULSES: &str = "0x0008@100+30,0x0008@500+30,0x0008@850+30,\
     0x4000@950+10,0x4000@1100+10,0x4000@1300+10,0x4000@1500+10";

fn main() -> Result<(), String> {
    let mut steps = DEFAULT_STEPS;
    let mut out_dir = PathBuf::from(DEFAULT_OUT_DIR);
    let mut scale = 1u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--steps" => {
                steps = take_arg(&mut args, "--steps")?
                    .parse()
                    .map_err(|_| format!("--steps must be an integer, got {}", steps))?
            }
            "--out-dir" => out_dir = PathBuf::from(take_arg(&mut args, "--out-dir")?),
            "--scale" => {
                scale = take_arg(&mut args, "--scale")?
                    .parse()
                    .map_err(|_| format!("--scale must be an integer, got {}", scale))?
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    let bios_path = std::env::var("PSOXIDE_BIOS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_BIOS));
    let disc_path = std::env::var("PSOXIDE_DISC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DISC));

    let bios = std::fs::read(&bios_path).map_err(|e| format!("read BIOS: {e}"))?;
    let mut bus = Bus::new(bios).map_err(|e| format!("bus init: {e}"))?;
    let mut cpu = Cpu::new();
    let disc = psoxide_settings::library::load_disc_from_cue(&disc_path)?;

    warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS)
        .map_err(|e| format!("BIOS warmup: {e}"))?;
    let info = fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false)
        .map_err(|e| format!("fast boot: {e:?}"))?;
    eprintln!(
        "[dump] fastboot {} entry=0x{:08x}",
        info.boot_path, info.initial_pc
    );
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    bus.gpu.enable_cmd_log();

    let mut hw = make_renderer()?;
    hw.set_internal_scale(scale, None);
    let mut frame_start_vram = bus.gpu.vram.words().to_vec();

    let pulses = parse_pad_pulses(TEKKEN_MENU_PULSES)?;
    let mut current_mask = u16::MAX;
    let mut last_vblank = bus.irq().raise_counts()[0];
    let mut hist = BTreeMap::<u8, u64>::new();
    let mut replayed_entries = 0u64;
    for i in 0..steps {
        if i & 0x1FFF == 0 {
            let vblank = bus.irq().raise_counts()[0];
            let mask = effective_mask(0, &pulses, vblank);
            if mask != current_mask {
                bus.set_port1_buttons(emulator_core::ButtonState::from_bits(mask));
                current_mask = mask;
            }
        }
        cpu.step(&mut bus)
            .map_err(|e| format!("CPU error at step {i}: {e}"))?;
        let vblank = bus.irq().raise_counts()[0];
        if vblank != last_vblank {
            replayed_entries +=
                render_drained_frame(&mut hw, &mut bus, &frame_start_vram, &mut hist);
            frame_start_vram.clear();
            frame_start_vram.extend_from_slice(bus.gpu.vram.words());
            last_vblank = vblank;
        }
    }
    replayed_entries += render_drained_frame(&mut hw, &mut bus, &frame_start_vram, &mut hist);

    dump_cpu_display(&bus, &out_dir.join("cpu.ppm"))?;
    dump_hw_display(&hw, &bus, &out_dir.join("hw.ppm"))?;
    dump_report(&bus, replayed_entries, &hist, &out_dir.join("report.txt"))?;
    eprintln!(
        "[dump] wrote {} and {}",
        out_dir.join("cpu.ppm").display(),
        out_dir.join("hw.ppm").display()
    );
    Ok(())
}

fn take_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn make_renderer() -> Result<psx_gpu_render::HwRenderer, String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok_or_else(|| "no compatible wgpu adapter".to_string())?;
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("tekken-hw-dump-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .map_err(|e| format!("request device: {e:?}"))?;
    Ok(psx_gpu_render::HwRenderer::new_headless(device, queue))
}

fn render_drained_frame(
    hw: &mut psx_gpu_render::HwRenderer,
    bus: &mut Bus,
    frame_start_vram: &[u16],
    hist: &mut BTreeMap<u8, u64>,
) -> u64 {
    let frame_log = bus.gpu.drain_completed_cmd_log();
    if frame_log.is_empty() {
        return 0;
    }
    for entry in &frame_log {
        *hist.entry(entry.opcode).or_default() += 1;
    }
    hw.render_frame(&bus.gpu, &frame_log, frame_start_vram);
    frame_log.len() as u64
}

fn dump_cpu_display(bus: &Bus, path: &Path) -> Result<(), String> {
    let (rgba, w, h) = bus.gpu.display_rgba8();
    write_ppm(path, w, h, &rgba)
}

fn dump_hw_display(hw: &psx_gpu_render::HwRenderer, bus: &Bus, path: &Path) -> Result<(), String> {
    let area = bus.gpu.display_area();
    let scale = hw.internal_scale();
    let (w, h, rgba) = hw.read_subrect_rgba8(
        area.x as u32 * scale,
        area.y as u32 * scale,
        area.width as u32 * scale,
        area.height as u32 * scale,
    );
    write_ppm(path, w, h, &rgba)
}

fn write_ppm(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let mut file =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    writeln!(file, "P6\n{w} {h}\n255").map_err(|e| format!("write {}: {e}", path.display()))?;
    for px in rgba.chunks_exact(4) {
        file.write_all(&px[..3])
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn dump_report(
    bus: &Bus,
    replayed_entries: u64,
    hist: &BTreeMap<u8, u64>,
    path: &Path,
) -> Result<(), String> {
    let area = bus.gpu.display_area();
    let mut report = String::new();
    report.push_str(&format!(
        "display: x={} y={} w={} h={} bpp24={}\n",
        area.x, area.y, area.width, area.height, area.bpp24
    ));
    report.push_str(&format!("replayed_entries: {replayed_entries}\n"));
    report.push_str("opcode histogram:\n");
    for (op, count) in hist {
        report.push_str(&format!("  0x{op:02X}: {count}\n"));
    }
    std::fs::write(path, report).map_err(|e| format!("write {}: {e}", path.display()))
}
