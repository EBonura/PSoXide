//! `psoxide-perf <command> [args]`: performance-work host tools.

use std::io::Write;

use psoxide_perf::{
    cortex_30fps_report, cortex_bench_report, engine_stress, instr_census, mcp_client,
    pc_line_attribution, pc_symbolize, performance_suite, text_census,
};

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
      static instruction census and modelled cycle split of a PSX-EXE\n\
  performance-suite run MANIFEST --bindings FILE --store DIR [--jobs N] [--case ID]...\n\
         [--report FILE] | get RESULT_DIR | compare BASELINE CANDIDATE\n\
      content-verified local replay store\n\
  cortex-bench-report OUT_DIR [--baseline DIR]\n\
      summarise a tools/cortex_bench.sh output directory\n\
  cortex-30fps-report RUN_DIR... [--compare-lockstep]\n\
      summarise cortex replay captures and compare lockstep visual hashes\n\
  engine-stress run SCENARIO... | analyse | measure-route RUN_DIR FIRST LAST\n\
         | calibrate-areas [PILLARS...]\n\
      the benchmarks/engine-stress runner, analyser and area calibration sweep\n\
  mcp-client --project DIR (--tools | CALLS_JSON|- [--out DIR])\n\
      run a batch of psxed-mcp tool calls in one server process";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout();
    let result = match args.first().map(String::as_str) {
        Some("pc-line-attribution") => pc_line_attribution::run(&args[1..], &mut stdout),
        Some("pc-symbolize") => pc_symbolize::run(&args[1..], &mut stdout),
        Some("instr-census") => instr_census::run(&args[1..], &mut stdout),
        Some("text-census") => text_census::run(&args[1..], &mut stdout),
        Some("performance-suite") => performance_suite::run(&args[1..], &mut stdout),
        Some("cortex-bench-report") => cortex_bench_report::run(&args[1..], &mut stdout),
        Some("cortex-30fps-report") => cortex_30fps_report::run(&args[1..], &mut stdout),
        Some("engine-stress") => engine_stress::run(&args[1..], &mut stdout),
        Some("mcp-client") => mcp_client::run(&args[1..], &mut stdout),
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
