//! Repository tasks for the PSoXide SDK and its website, in Rust so the
//! repository needs no other language to build, test or publish:
//!
//! ```text
//! cargo run --locked -p xtask -- <task> [args]
//!
//! check-mfc0 [PATHS...]                 MFC0/MFC2 load-delay hazards in guest asm
//! material-audit [--repo P] [--history] firmware artifacts and vendor EXE text
//! fmv-test-movie --psxavenc P --out F   hello-fmv's synthetic STR movie
//! ```

mod fmv_movie;
mod material_audit;
mod mfc0;
pub mod pyjson;
#[cfg(test)]
mod readme_tables;

use std::process::ExitCode;

const USAGE: &str = "usage: xtask <check-mfc0|material-audit|fmv-test-movie> [args]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((task, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let code = match task.as_str() {
        "check-mfc0" => mfc0::main(rest),
        "material-audit" => material_audit::main(rest),
        "fmv-test-movie" => report(fmv_movie::main(rest)),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    ExitCode::from(code as u8)
}

/// Print a task's error and turn it into exit status 1.
fn report(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("xtask: {error}");
            1
        }
    }
}
