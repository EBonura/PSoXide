//! Diff PSoXide vs PCSX-Redux on the CD-DA + data-read contention.
//!
//! Runs the `cdda-read-contention` guest (which issues the engine's real
//! read path while CD-DA is playing) in both emulators and compares the
//! CD-ROM IRQ each one produces for the `ReadN`. The guest writes its
//! result to RAM at `RESULT_BASE`; PSoXide reads it with `read32`, Redux
//! with `peek32`.
//!
//! Env:
//!   PSOXIDE_EXE   guest .exe   (default: build/.../cdda-read-contention.exe)
//!   PSOXIDE_DISC  mixed disc   (default: build/.../cdda-read-contention.cue)
//!   PSOXIDE_BIOS  PS1 BIOS     (Redux only; PSoXide uses HLE)
//!   PSOXIDE_REDUX_BIN  Redux binary (else Redux step is skipped)

use std::path::{Path, PathBuf};
use std::time::Duration;

use emulator_core::{Bus, Cpu};
use parity_oracle::{OracleConfig, ReduxProcess};
use psx_iso::Exe;

const RESULT_BASE: u32 = 0x8010_0000;
const DONE_MAGIC: u32 = 0x0C0D_0001;

#[derive(Default, Debug)]
struct Outcome {
    magic: u32,
    irq: u32,
    play_stat: u32,
    read_stat: u32,
    data: u32,
    stage: u32,
}

impl Outcome {
    fn verdict(&self) -> &'static str {
        if self.magic != DONE_MAGIC {
            "DID-NOT-COMPLETE"
        } else if self.irq & (1 << 5) != 0 {
            "INT5_ERROR (matches guest STATUS_CD_ERROR)"
        } else if self.irq & (1 << 1) != 0 {
            "DATA_DELIVERED (no error; CD-DA dropped, data returned)"
        } else {
            "UNKNOWN (no DataReady, no Error)"
        }
    }

    fn print(&self, label: &str) {
        println!("--- {label} ---");
        println!("  magic     = 0x{:08x} (want 0x{DONE_MAGIC:08x})", self.magic);
        println!("  irq mask  = 0x{:08x}  [INT1 data={} INT2 cmpl={} INT3 ack={} INT5 err={}]",
            self.irq,
            (self.irq >> 1) & 1, (self.irq >> 2) & 1, (self.irq >> 3) & 1, (self.irq >> 5) & 1);
        println!("  play_stat = 0x{:02x}  [PLAYING={}]", self.play_stat, (self.play_stat >> 7) & 1);
        println!("  read_stat = 0x{:02x}  [READING={}]", self.read_stat, (self.read_stat >> 5) & 1);
        println!("  data fifo = {}", self.data);
        println!("  stage     = {} (4 = ran to completion)", self.stage);
        println!("  VERDICT   = {}", self.verdict());
    }
}

fn main() {
    let exe = env_path("PSOXIDE_EXE").unwrap_or_else(|| {
        repo_root().join("build/examples/mipsel-sony-psx/release/cdda-read-contention.exe")
    });
    let disc = env_path("PSOXIDE_DISC").unwrap_or_else(|| {
        repo_root().join("build/examples/mipsel-sony-psx/release/cdda-read-contention.cue")
    });

    println!("exe : {}", exe.display());
    println!("disc: {}", disc.display());
    println!();

    let psoxide = run_psoxide(&exe, &disc);
    psoxide.print("PSoXide (emulator-core, HLE BIOS)");
    println!();

    match run_redux(&exe, &disc) {
        Ok(redux) => {
            redux.print("PCSX-Redux");
            println!();
            println!("=== DIFF ===");
            if psoxide.irq == redux.irq {
                println!("PSoXide and Redux AGREE: irq=0x{:08x} -> {}", psoxide.irq, psoxide.verdict());
                println!("If they agree but hardware differs, BOTH emulators miss the behavior.");
            } else {
                println!("PSoXide and Redux DISAGREE:");
                println!("  PSoXide irq=0x{:08x} -> {}", psoxide.irq, psoxide.verdict());
                println!("  Redux   irq=0x{:08x} -> {}", redux.irq, redux.verdict());
                println!("Per the accuracy hierarchy (hw > DuckStation > Redux), Redux is the");
                println!("stronger proxy: treat its result as the lead and confirm on silicon.");
            }
        }
        Err(reason) => {
            println!("PCSX-Redux step SKIPPED: {reason}");
            println!("Set PSOXIDE_REDUX_BIN and PSOXIDE_BIOS to enable the proxy diff.");
        }
    }
}

fn run_psoxide(exe_path: &Path, disc_path: &Path) -> Outcome {
    let exe_bytes = std::fs::read(exe_path).expect("read guest exe");
    let exe = Exe::parse(&exe_bytes).expect("parse PSX-EXE");
    let disc = psoxide_settings::library::load_disc_from_cue(disc_path).expect("load disc cue");

    let mut bus = Bus::new_without_bios();
    bus.load_exe_payload(exe.load_addr, &exe.payload);
    bus.clear_exe_bss(exe.bss_addr, exe.bss_size);
    bus.enable_hle_bios();
    bus.cdrom.insert_disc(Some(disc));

    let mut cpu = Cpu::new();
    cpu.seed_from_exe(exe.initial_pc, exe.initial_gp, exe.initial_sp());

    let mut steps = 0u64;
    while steps < 200_000_000 {
        if cpu.step(&mut bus).is_err() {
            break;
        }
        steps += 1;
        if steps % 65_536 == 0 && bus.read32(RESULT_BASE) == DONE_MAGIC {
            break;
        }
    }
    println!("[psoxide] ran {steps} steps");
    Outcome {
        magic: bus.read32(RESULT_BASE),
        irq: bus.read32(RESULT_BASE + 0x04),
        play_stat: bus.read32(RESULT_BASE + 0x08),
        read_stat: bus.read32(RESULT_BASE + 0x0C),
        data: bus.read32(RESULT_BASE + 0x10),
        stage: bus.read32(RESULT_BASE + 0x14),
    }
}

fn run_redux(exe_path: &Path, disc_path: &Path) -> Result<Outcome, String> {
    let bios = env_path("PSOXIDE_BIOS").ok_or("PSOXIDE_BIOS not set")?;
    if !bios.is_file() {
        return Err(format!("BIOS not found at {}", bios.display()));
    }
    let lua = OracleConfig::default_lua_dir().join("oracle.lua");
    let config = OracleConfig::new(bios, lua)
        .map_err(|e| format!("Redux binary not resolved: {e:?}"))?
        .with_disc(disc_path.to_path_buf());

    let mut redux = ReduxProcess::launch(&config).map_err(|e| format!("launch: {e:?}"))?;
    redux
        .handshake(Duration::from_secs(30))
        .map_err(|e| format!("handshake: {e:?}"))?;
    // BIOS warmup so the CD/SPU are initialized before we sideload.
    redux
        .run(3_000_000, Duration::from_secs(90))
        .map_err(|e| format!("warmup run: {e:?}"))?;
    redux
        .load_exe(exe_path, Duration::from_secs(30))
        .map_err(|e| format!("load_exe: {e:?}"))?;
    redux
        .run(30_000_000, Duration::from_secs(180))
        .map_err(|e| format!("run: {e:?}"))?;

    let peek = |addr: u32, redux: &mut ReduxProcess| -> Result<u32, String> {
        redux
            .peek32(addr, Duration::from_secs(5))
            .map_err(|e| format!("peek32(0x{addr:08x}): {e:?}"))
    };
    let outcome = Outcome {
        magic: peek(RESULT_BASE, &mut redux)?,
        irq: peek(RESULT_BASE + 0x04, &mut redux)?,
        play_stat: peek(RESULT_BASE + 0x08, &mut redux)?,
        read_stat: peek(RESULT_BASE + 0x0C, &mut redux)?,
        data: peek(RESULT_BASE + 0x10, &mut redux)?,
        stage: peek(RESULT_BASE + 0x14, &mut redux)?,
    };

    redux.send_command("quit").ok();
    let _ = redux.wait_for_response(Duration::from_secs(2));
    let _ = redux.terminate();
    Ok(outcome)
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
