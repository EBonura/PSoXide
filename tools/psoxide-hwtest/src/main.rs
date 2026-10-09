//! `psoxide-hwtest <command> [args]`: the hardware-test disc's host tools.

use std::io::Write;

use psoxide_hwtest::report::{parse_capture, payloads_from_paths, print_report, ReportOptions};
use psoxide_hwtest::util::{Error, Result};
use psoxide_hwtest::{audio_chain, audio_decode, audio_report, machine_code, sb4};

const HELP: &str = "usage: psoxide-hwtest <command> [args]\n\
commands:\n\
  report [--baseline FILE] [--allow-suite-mismatch] [--fail-on-change]\n\
         [--layout-immune-timing-only] [PAYLOAD_OR_FILE ...]\n\
      validate and assemble PX7/PX8 hardware-test photo payloads\n\
  sb4-report CAPTURE [--baseline FILE] [--fail-on-change]\n\
      decode and diff SB4 capture-ring payloads\n\
  verify-machine-code EXE [--baseline FILE] [--fail-on-change]\n\
      audit the measured instruction blocks of the linked hardware-tests EXE\n\
  audio-report PAYLOAD_OR_FILE\n\
      decode PA1..PA5 audio-probe QR payloads\n\
  audio-decode WAV [--out FILE] [--emit-pages FILE] [--max-frames N]\n\
      recover the capture payload from a recording of the audio link\n\
  audio-chaintest WAV [--workdir DIR]\n\
      decode the recording again after simulated capture-chain damage";

/// `psoxide-hwtest report`
fn report_command(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32> {
    let mut baseline_path = None;
    let mut allow_suite_mismatch = false;
    let mut options = ReportOptions::default();
    let mut inputs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--baseline" => {
                i += 1;
                baseline_path = Some(args.get(i).ok_or("--baseline needs a value")?.clone());
            }
            "--allow-suite-mismatch" => allow_suite_mismatch = true,
            "--fail-on-change" => options.fail_on_change = true,
            "--layout-immune-timing-only" => options.layout_immune_only = true,
            flag if flag.starts_with("--") => {
                return Err(Error(format!("unrecognized arguments: {flag}")))
            }
            input => inputs.push(input.to_string()),
        }
        i += 1;
    }
    let capture = parse_capture(&payloads_from_paths(&inputs)?)?;
    let baseline = match &baseline_path {
        Some(path) => Some(parse_capture(&payloads_from_paths(std::slice::from_ref(
            path,
        ))?)?),
        None => None,
    };
    if let Some(baseline) = &baseline {
        if baseline.schema != capture.schema {
            return Err(Error("baseline and capture schemas differ".into()));
        }
        if !allow_suite_mismatch {
            let here = (capture.suite_major, capture.suite_minor);
            let there = (baseline.suite_major, baseline.suite_minor);
            // A MAJOR difference means a record id can have been redefined, so
            // the diff would compare two different measurements under one name.
            // That is worse than no diff, so it fails rather than warns.
            if here.0 != there.0 {
                return Err(Error(format!(
                    "suite version mismatch: capture v{}.{} vs baseline v{}.{}. Record meanings may differ across a MAJOR bump; re-baseline, or pass --allow-suite-mismatch if you know they are comparable.",
                    here.0, here.1, there.0, there.1
                )));
            }
            if here != there {
                writeln!(
                    err,
                    "# note: capture v{}.{} vs baseline v{}.{}; shared records remain comparable across a MINOR bump",
                    here.0, here.1, there.0, there.1
                )?;
            }
        }
    }
    print_report(&capture, baseline.as_ref(), options, out, err)
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = stderr.lock();
    let Some(command) = args.first() else {
        eprintln!("{HELP}");
        return 2;
    };
    let rest = &args[1..];
    // Exit codes follow the scripts: the report and the audio reader turn a
    // malformed input into status 2, the SB4 reader let it surface as an
    // uncaught error (status 1).
    let (result, failure_code) = match command.as_str() {
        "report" => (report_command(rest, &mut out, &mut err), 2),
        "sb4-report" => (sb4::run(rest, &mut out), 1),
        "verify-machine-code" => (machine_code::run(rest, &mut out, &mut err), 1),
        "audio-report" => (audio_report::run(rest, &mut out), 2),
        "audio-decode" => (audio_decode::run(rest, &mut out, &mut err), 1),
        "audio-chaintest" => (audio_chain::run(rest, &mut out), 1),
        "-h" | "--help" | "help" => {
            println!("{HELP}");
            return 0;
        }
        other => {
            eprintln!("psoxide-hwtest: unknown command {other:?}\n{HELP}");
            return 2;
        }
    };
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            let _ = out.flush();
            let name = match command.as_str() {
                "report" => "hwtest-report",
                "audio-report" => "hwtest-audio-report",
                other => other,
            };
            let _ = writeln!(err, "{name}: {error}");
            failure_code
        }
    };
    let _ = out.flush();
    code
}

fn main() {
    std::process::exit(run());
}
