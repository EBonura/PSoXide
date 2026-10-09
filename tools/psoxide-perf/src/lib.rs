//! Host tools for performance work. One library, one binary (`psoxide-perf`)
//! with a subcommand per job; the library is what the unit tests call.

#[macro_use]
pub mod util;
pub mod instr_census;
pub mod pc_line_attribution;
pub mod pc_symbolize;
pub mod text_census;
