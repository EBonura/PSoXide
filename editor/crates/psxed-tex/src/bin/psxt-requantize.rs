//! Requantise an existing cooked `.psxt` texture to the project-standard 4bpp.
//!
//! Usage: `psxt-requantize <src.psxt> <out.psxt>`

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        eprintln!("usage: {} <src.psxt> <out.psxt>", args[0]);
        std::process::exit(2);
    }
    let source = std::fs::read(&args[1]).expect("read source PSXT");
    let output = psxed_tex::requantize_psxt_to_4bpp(&source).expect("requantize PSXT");
    std::fs::write(&args[2], &output).expect("write output PSXT");
    println!(
        "wrote {} ({} -> {} bytes, 4bpp)",
        args[2],
        source.len(),
        output.len()
    );
}
