//! `quake-chain-adapter RUN_DIR RESOLVED_INPUTS_JSON`: the semantic completion
//! adapter for the Quake E1M1-to-E1M2 chain case of the performance suite. It is
//! a separate executable so the suite can hash its bytes as an input.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = psoxide_perf::quake_chain_adapter::main_with(&args, &mut std::io::stdout().lock());
    std::process::exit(code);
}
