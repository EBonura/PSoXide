//! Build the lever-and-doors test project: three rooms in a row, a lever that
//! opens a door, and a one-way shortcut door (see `grid::logic_demo`).
//!
//! ```text
//! cargo run -p psxed-project --example lever_doors_demo -- <out-dir> [base-dir]
//! ```
//!
//! `base-dir` is the project the player, camera, sky and UI come from; it
//! defaults to `projects/graybox-reach` under `PSOXIDE_PROJECTS_DIR` (or the
//! checkout's `projects/`), which the editor repository carries.

use std::path::PathBuf;

use psxed_project::grid::logic_demo::{base_dir, create_project};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(out) = args.next().map(PathBuf::from) else {
        eprintln!("usage: lever_doors_demo <out-dir> [base-dir]");
        std::process::exit(2);
    };
    let base = args
        .next()
        .map_or_else(|| base_dir(&psxed_project::projects_dir()), PathBuf::from);
    match create_project(&base, &out, "Lever doors") {
        Ok(path) => println!("{}", path.display()),
        Err(error) => {
            eprintln!("lever_doors_demo: {error}");
            std::process::exit(1);
        }
    }
}
