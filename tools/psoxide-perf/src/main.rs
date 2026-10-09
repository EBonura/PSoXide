//! `psoxide-perf <command> [args]`: performance-work host tools.

use std::io::Write;

use psoxide_perf::{instr_census, pc_line_attribution, pc_symbolize, text_census};

const HELP: &str = "usage: psoxide-perf <command> [args]\n\
commands:\n\
  pc-line-attribution PC_LINES LINKER_MAP [--limit N] [--compare BASE_LOG BASE_MAP]\n\
         [--icache-events CSV]\n\
      attribute exact PC-line or PC-word counts to linker-map symbols and cache sets\n\
  pc-symbolize --elf ELF --samples CSV [--top N] [--min-window-start TICK]\n\
      aggregate --pc-sample-log output into guest functions\n\
  instr-census EXE [--name LABEL] [--dis OUT.dis]\n\
      static instruction mix of a PSX-EXE via binutils\n\
  text-census [EXE]\n\
      static instruction census and modelled cycle split of a PSX-EXE";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout().lock();
    let result = match args.first().map(String::as_str) {
        Some("pc-line-attribution") => pc_line_attribution::run(&args[1..], &mut stdout),
        Some("pc-symbolize") => pc_symbolize::run(&args[1..], &mut stdout),
        Some("instr-census") => instr_census::run(&args[1..], &mut stdout),
        Some("text-census") => text_census::run(&args[1..], &mut stdout),
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            Ok(0)
        }
        Some(other) => {
            eprintln!("psoxide-perf: unknown command `{other}`\n\n{HELP}");
            Ok(2)
        }
        None => {
            eprintln!("{HELP}");
            Ok(2)
        }
    };
    let _ = stdout.flush();
    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("psoxide-perf: {error}");
            std::process::exit(1);
        }
    }
}
