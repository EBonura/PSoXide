//! `check-mfc0`: see [`psoxide_hazard::mfc0`].
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(psoxide_hazard::mfc0::main(&args));
}
