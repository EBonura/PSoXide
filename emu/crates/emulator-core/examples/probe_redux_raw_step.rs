//! Dump Redux's raw instructions inside one folded user-side step.
//!
//! Usage:
//! `cargo run -p emulator-core --release --example probe_redux_raw_step -- <completed_steps> <disc.cue|disc.bin>`

#[path = "support/pad.rs"]
mod pad_support;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use pad_support::{parse_pad_pulses, parse_u16_mask, PadPulse};
use parity_oracle::OracleError;
use parity_oracle::{OracleConfig, ReduxProcess};

const FAST_FORWARD_CHECKPOINT_INTERVAL: u64 = 1_000_000;

fn main() {
    absolutize_redux_trace_file_env();

    let start: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: probe_redux_raw_step <completed_steps> <disc>");
    let disc_path = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .expect("usage: probe_redux_raw_step <completed_steps> <disc>");

    let bios_path = PathBuf::from("bios/SCPH1001.BIN");
    let lua = OracleConfig::default_lua_dir().join("oracle.lua");
    let config = OracleConfig::new(bios_path, lua)
        .expect("Redux resolves")
        .with_disc(disc_path);
    let mut redux = ReduxProcess::launch(&config).expect("Redux launches");
    redux.handshake(Duration::from_secs(30)).expect("handshake");

    let pad = PadRoute::from_env();
    eprintln!("[redux] running {start} user-side steps...");
    let timeout = Duration::from_secs((start / 200_000).max(60));
    let tick = run_to_start(&mut redux, start, timeout, &pad).expect("run");
    eprintln!("[redux] at start tick={tick}; tracing one folded step...");

    let cap = std::env::var("PSOXIDE_REDUX_RAW_CAP")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(3000);
    let command = match std::env::var("PSOXIDE_REDUX_STOP_PC") {
        Ok(stop_pc) if std::env::var_os("PSOXIDE_REDUX_RAW_ONLY").is_some() => {
            format!("raw_trace_until {stop_pc} {cap}")
        }
        Ok(stop_pc) => format!("manual_trace_until {stop_pc} {cap}"),
        Err(_) => format!("trace_one_step {cap}"),
    };
    redux.send_command(&command).expect("trace command");
    let mut raw_count = 0u64;
    let mut pc_counts: HashMap<u32, u64> = HashMap::new();
    let mut first = Vec::new();
    let mut samples = Vec::new();
    let mut last: VecDeque<String> = VecDeque::new();
    loop {
        let line = redux
            .wait_for_response(Duration::from_secs(30))
            .expect("trace response");
        if line.starts_with("raw ") {
            raw_count += 1;
            if let Some(pc) = parse_raw_pc(&line) {
                *pc_counts.entry(pc).or_insert(0) += 1;
            }
            if first.len() < 12 {
                first.push(line.clone());
            }
            if raw_count % 1000 == 0 {
                samples.push(line.clone());
            }
            last.push_back(line.clone());
            if last.len() > 12 {
                last.pop_front();
            }
        } else {
            println!("{line}");
        }
        if line == "trace_one_step ok"
            || line == "manual_trace_until ok"
            || line == "raw_trace_until ok"
            || line.starts_with("err ")
        {
            break;
        }
    }
    println!("raw_count_seen={raw_count} cap={cap}");
    println!("first_raw:");
    for line in &first {
        println!("  {line}");
    }
    println!("sampled_raw:");
    for line in &samples {
        println!("  {line}");
    }
    println!("last_raw:");
    for line in &last {
        println!("  {line}");
    }
    let mut top_pcs = pc_counts.into_iter().collect::<Vec<_>>();
    top_pcs.sort_by_key(|&(pc, count)| (std::cmp::Reverse(count), pc));
    println!("top_pcs:");
    for (pc, count) in top_pcs.into_iter().take(24) {
        println!("  pc=0x{pc:08x} count={count}");
    }
    redux.send_command("quit").ok();
    let _ = redux.wait_for_response(Duration::from_secs(2));
    let _ = redux.terminate();
}

fn absolutize_redux_trace_file_env() {
    let Some(path) = std::env::var_os("PSOXIDE_REDUX_TRACE_FILE") else {
        return;
    };
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return;
    }
    let absolute = std::env::current_dir()
        .expect("current directory is readable")
        .join(path);
    // The Redux child inherits this environment variable. Set it before
    // launching any threads so relative paths do not land in Redux's
    // temporary portable run directory.
    unsafe {
        std::env::set_var("PSOXIDE_REDUX_TRACE_FILE", absolute);
    }
}

struct PadRoute {
    base_mask: u16,
    pulses: Vec<PadPulse>,
}

impl PadRoute {
    fn from_env() -> Self {
        let base_mask = std::env::var("PSOXIDE_PAD1")
            .ok()
            .and_then(|s| parse_u16_mask(&s))
            .unwrap_or(0);
        let pulses = std::env::var("PSOXIDE_PAD1_PULSES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| parse_pad_pulses(&s).expect("PSOXIDE_PAD1_PULSES parses"))
            .unwrap_or_default();
        Self { base_mask, pulses }
    }

    fn enabled(&self) -> bool {
        self.base_mask != 0 || !self.pulses.is_empty()
    }

    fn pulse_tuples(&self) -> Vec<(u16, u64, u64)> {
        self.pulses
            .iter()
            .map(|pulse| (pulse.mask, pulse.start_vblank, pulse.frames))
            .collect()
    }
}

fn run_to_start(
    redux: &mut ReduxProcess,
    start: u64,
    timeout: Duration,
    pad: &PadRoute,
) -> Result<u64, OracleError> {
    if start == 0 {
        return Ok(0);
    }
    let interval = start.min(FAST_FORWARD_CHECKPOINT_INTERVAL).max(1);
    let expected = start / interval;
    let stride = (expected / 10).max(1);
    let mut emitted = 0u64;
    if pad.enabled() {
        let pulses = pad.pulse_tuples();
        redux.run_checkpoint_pad(
            start,
            interval,
            1,
            pad.base_mask,
            &pulses,
            timeout,
            |step, _tick, _pc| {
                emitted += 1;
                if emitted % stride == 0 || step == start {
                    eprintln!("[redux] ff progress {step}/{start}");
                }
                Ok(())
            },
        )
    } else {
        redux.run_checkpoint(start, interval, timeout, |step, _tick, _pc| {
            emitted += 1;
            if emitted % stride == 0 || step == start {
                eprintln!("[redux] ff progress {step}/{start}");
            }
            Ok(())
        })
    }
}

fn parse_raw_pc(line: &str) -> Option<u32> {
    line.split_whitespace()
        .find_map(|part| part.strip_prefix("pc=")?.parse::<u32>().ok())
}
