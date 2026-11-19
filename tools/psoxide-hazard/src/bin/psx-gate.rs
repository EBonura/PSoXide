//! `psx-gate`: see [`psoxide_hazard::gate`].
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(psoxide_hazard::gate::main(&args));
}
