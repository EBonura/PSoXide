//! Reproduce the CD-DA + data-read contention behavior of the CDROM
//! controller directly (no BIOS, no guest EXE).
//!
//! This is the exact situation cortex_ignition_v1 hits when gameplay
//! starts room streaming while the menu's CD-DA music is still playing:
//! the guest's `cd_stream.rs` reports `CD_ROOM_CHUNK_STATUS = 5`
//! (`STATUS_CD_ERROR`), which is the guest mapping of an INT5 (Error)
//! IRQ from the controller.
//!
//! The probe drives the controller through:
//!
//!   Init -> SetMode -> Play(track) [CD-DA, PLAYING latches]
//!        -> SetLoc(data LBA) -> ReadN
//!
//! and prints the full command + response (IRQ) log so the result can be
//! diffed byte-for-byte against DuckStation / PCSX-Redux / real hardware.
//!
//! North star: the emulator must match silicon. The open question is what
//! real hardware does for "ReadN issued while CD-DA is playing":
//!   - does it raise INT5 (Error), like the guest's STATUS_CD_ERROR? or
//!   - does it abort CD-DA and deliver data (Acknowledge + DataReady)?
//! PSoXide currently does the latter; this probe captures that so the
//! divergence (if any) is concrete.
//!
//! Run with the built-in synthetic data+audio disc (no disc build needed):
//!   cargo run -p emulator-core --example probe_cdda_read_contention
//! Or point it at a real disc and choose the track/LBA:
//!   PSOXIDE_DISC=path.cue PSOXIDE_PLAY_TRACK=2 PSOXIDE_READ_LBA=16 \
//!     cargo run -p emulator-core --example probe_cdda_read_contention

#[path = "support/disc.rs"]
mod disc_support;

use emulator_core::cdrom::IrqType;
use emulator_core::Bus;
use psx_iso::{Disc, Track, TrackType};
use std::path::Path;

const RAW_SECTOR: usize = 2352;

// MMIO ports (see cdrom_drive_test.rs for the index/port discipline).
const PORT_INDEX: u32 = 0x1F80_1800;
const PORT_CMD: u32 = 0x1F80_1801; // command (idx 0)
const PORT_PARAM: u32 = 0x1F80_1802; // param push (idx 0) / data FIFO (read)
const PORT_IRQ: u32 = 0x1F80_1803; // irq enable/flag (idx 1)

// Drive-status (stat) bits, mirrored from cdrom::drive_status_bit.
const STAT_ERROR: u8 = 1 << 0;
const STAT_READING: u8 = 1 << 5;
const STAT_PLAYING: u8 = 1 << 7;

fn main() {
    let play_track: u8 = env_u32("PSOXIDE_PLAY_TRACK").unwrap_or(2) as u8;
    let read_lba: u32 = env_u32("PSOXIDE_READ_LBA").unwrap_or(16);

    let (disc, source) = match std::env::var_os("PSOXIDE_DISC") {
        Some(p) if !p.is_empty() => {
            let path = Path::new(&p);
            let disc = disc_support::load_disc_path(path).expect("load PSOXIDE_DISC");
            (disc, format!("{}", path.display()))
        }
        _ => (
            synthetic_contention_disc(),
            "synthetic data+audio disc".to_string(),
        ),
    };

    println!("=== CD-DA + ReadN contention probe ===");
    println!("Disc:        {source}");
    println!(
        "Tracks:      {} (play track {play_track}, read LBA {read_lba})",
        disc.track_count()
    );
    for n in 1..=disc.track_count() as u8 {
        if let Some(t) = disc.track(n) {
            println!(
                "  track {n}: {:?} start_lba={} sectors={}",
                t.track_type, t.start_lba, t.sector_count
            );
        }
    }

    let mut bus = Bus::new_without_bios();
    bus.cdrom.insert_disc(Some(disc));
    bus.cdrom.enable_command_log(64);
    bus.cdrom.enable_response_log(128);

    // Enable all CDROM IRQ types and clear anything pending.
    bus.write8(PORT_INDEX, 1);
    bus.write8(PORT_PARAM, 0x1F);
    bus.write8(PORT_IRQ, 0x1F);

    // 1) Init: spin up, reset mode.
    run_cmd(&mut bus, "Init", 0x0A, &[], 2_000_000);
    // 2) SetMode: double-speed, 2048-byte sectors (what the room-stream
    //    reader uses). Play does not depend on this in PSoXide.
    run_cmd(&mut bus, "SetMode", 0x0E, &[0x80], 200_000);
    // 3) Play the audio track -> CD-DA, PLAYING latches.
    run_cmd(&mut bus, "Play", 0x03, &[bin_to_bcd(play_track)], 1_000_000);

    let stat_before = latest_stat(&bus);
    println!();
    println!(
        "After Play: stat=0x{stat_before:02x} [{}]",
        decode_stat(stat_before)
    );
    assert_state("CD-DA should be PLAYING after Play", stat_before & STAT_PLAYING != 0);

    // 4) The contention point: aim at a DATA sector and issue ReadN while
    //    CD-DA is still playing.
    run_cmd(&mut bus, "SetLoc(data)", 0x02, &lba_to_setloc_bcd(read_lba), 200_000);
    let resp_before_read = bus.cdrom.response_log().len();
    run_cmd(&mut bus, "ReadN", 0x06, &[], 1_500_000);

    let stat_after = latest_stat(&bus);
    let fifo = bus.cdrom.data_fifo_len();
    let read_responses = &bus.cdrom.response_log()[resp_before_read..];
    let saw_error = read_responses
        .iter()
        .any(|e| matches!(e.irq, IrqType::Error));
    let saw_data = read_responses
        .iter()
        .any(|e| matches!(e.irq, IrqType::DataReady));

    println!();
    println!("=== Full command / response (IRQ) log ===");
    for entry in bus.cdrom.command_log() {
        println!(
            "  CMD  cyc={:>10} op=0x{:02x} params=[{}]",
            entry.cycle,
            entry.command,
            fmt_bytes(&entry.params[..entry.param_len as usize]),
        );
    }
    for entry in bus.cdrom.response_log() {
        println!(
            "  IRQ  cyc={:>10} {:?} bytes=[{}]",
            entry.cycle,
            entry.irq,
            fmt_bytes(&entry.bytes[..entry.len as usize]),
        );
    }

    println!();
    println!("=== Verdict (PSoXide) ===");
    println!(
        "After ReadN: stat=0x{stat_after:02x} [{}] data_fifo_len={fifo}",
        decode_stat(stat_after)
    );
    if saw_error {
        println!("RESULT: ReadN-while-PLAYING -> INT5 Error (matches guest STATUS_CD_ERROR).");
    } else if saw_data {
        println!(
            "RESULT: ReadN-while-PLAYING -> Acknowledge + DataReady, {fifo} data bytes delivered."
        );
        println!("        PSoXide treats the contention as a clean CD-DA->data switch.");
        println!("        >>> If real hardware errors here, THIS is the accuracy gap. <<<");
    } else {
        println!("RESULT: ReadN-while-PLAYING -> no DataReady and no Error (unexpected).");
    }
}

/// Issue a CDROM command via MMIO and tick the bus, acking every IRQ as it
/// latches (the BIOS does this in software). Prints the responses produced.
fn run_cmd(bus: &mut Bus, label: &str, opcode: u8, params: &[u8], cycles: u64) {
    let before = bus.cdrom.response_log().len();
    bus.write8(PORT_INDEX, 0); // index 0 for command/param ports
    for &p in params {
        bus.write8(PORT_PARAM, p);
    }
    bus.write8(PORT_CMD, opcode);
    tick_and_ack(bus, cycles);
    let after = bus.cdrom.response_log().len();
    let new = &bus.cdrom.response_log()[before..after];
    let irqs: Vec<String> = new.iter().map(|e| format!("{:?}", e.irq)).collect();
    println!(
        "{label:<13} op=0x{opcode:02x} params=[{}] -> [{}]",
        fmt_bytes(params),
        irqs.join(", ")
    );
}

fn tick_and_ack(bus: &mut Bus, cycles: u64) {
    let target = bus.cycles() + cycles;
    while bus.cycles() < target {
        bus.tick(128);
        // `Bus::tick` advances the clock and drains VBlank/SIO/DMA slots
        // but NOT the CDROM, which is serviced only on the per-instruction
        // branch-boundary drain. Call it here so response deadlines fire.
        bus.drain_scheduler_events_post_op();
        bus.write8(PORT_INDEX, 1); // idx 1 for IRQ flag
        let irq = bus.read8(PORT_IRQ);
        if irq & 0x1F != 0 {
            // Drain the response FIFO (status bit 0x20 = not empty).
            while bus.read8(PORT_INDEX) & 0x20 != 0 {
                let _ = bus.read8(PORT_CMD);
            }
            bus.write8(PORT_IRQ, irq & 0x1F); // ack (1-to-clear)
        }
        bus.write8(PORT_INDEX, 0);
    }
}

/// The most recent response's stat byte (first byte of every response).
fn latest_stat(bus: &Bus) -> u8 {
    bus.cdrom
        .response_log()
        .iter()
        .rev()
        .find_map(|e| (e.len > 0).then(|| e.bytes[0]))
        .unwrap_or(0)
}

fn decode_stat(stat: u8) -> String {
    let mut parts = Vec::new();
    if stat & STAT_PLAYING != 0 {
        parts.push("PLAYING");
    }
    if stat & STAT_READING != 0 {
        parts.push("READING");
    }
    if stat & STAT_ERROR != 0 {
        parts.push("ERROR");
    }
    if parts.is_empty() {
        "idle".to_string()
    } else {
        parts.join("|")
    }
}

fn assert_state(msg: &str, ok: bool) {
    if !ok {
        eprintln!("[contention-probe] precondition failed: {msg}");
        std::process::exit(2);
    }
}

/// Build a minimal disc with a data track 1 and an audio track 2 so that
/// `Play(track 2)` latches CD-DA and a later `ReadN` targets track 1 data.
fn synthetic_contention_disc() -> Disc {
    const DATA_SECTORS: u32 = 32;
    const AUDIO_SECTORS: u32 = 75;

    let mut data = vec![0u8; DATA_SECTORS as usize * RAW_SECTOR];
    for s in 0..DATA_SECTORS as usize {
        let base = s * RAW_SECTOR;
        // Mode-2 sync pattern so it reads like a real data sector.
        data[base + 1..base + 11].fill(0xFF);
        data[base + 15] = 0x02; // mode 2
        // Recognizable, non-zero user data so a successful read is visible.
        for b in 24..RAW_SECTOR {
            data[base + b] = (0xA0 + s as u8) ^ (b as u8);
        }
    }

    let mut audio = vec![0u8; AUDIO_SECTORS as usize * RAW_SECTOR];
    for (i, b) in audio.iter_mut().enumerate() {
        // Simple non-zero waveform so CD-DA has content.
        *b = ((i as u32).wrapping_mul(53) >> 3) as u8;
    }

    Disc::from_tracks(vec![
        Track {
            number: 1,
            track_type: TrackType::Data,
            start_lba: 0,
            sector_count: DATA_SECTORS,
            pregap: 0,
            file_pregap: 0,
            bytes: data,
        },
        Track {
            number: 2,
            track_type: TrackType::Audio,
            start_lba: DATA_SECTORS,
            sector_count: AUDIO_SECTORS,
            pregap: 0,
            file_pregap: 0,
            bytes: audio,
        },
    ])
}

/// Convert an absolute LBA to SetLoc BCD MSF params (LBA 0 = 00:02:00).
fn lba_to_setloc_bcd(lba: u32) -> [u8; 3] {
    let total = lba + 150;
    let m = (total / (75 * 60)) as u8;
    let s = ((total / 75) % 60) as u8;
    let f = (total % 75) as u8;
    [bin_to_bcd(m), bin_to_bcd(s), bin_to_bcd(f)]
}

fn bin_to_bcd(v: u8) -> u8 {
    ((v / 10) << 4) | (v % 10)
}

fn fmt_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn env_u32(name: &str) -> Option<u32> {
    std::env::var(name).ok()?.trim().parse().ok()
}
