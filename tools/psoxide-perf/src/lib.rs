//! Host tools for performance work. One library, one binary (`psoxide-perf`)
//! with a subcommand per job; the library is what the unit tests call.

#[macro_use]
pub mod util;
pub mod cortex_30fps_report;
pub mod cortex_bench_report;
pub mod instr_census;
pub mod pc_line_attribution;
pub mod pc_symbolize;
pub mod performance_suite;
pub mod pyjson;
pub mod quake_chain_adapter;
pub mod text_census;
