//! Writes a deterministic benchmark file of N sectors, the byte pattern
//! `hello-cdstream` and `hello-cdstream-probe` verify:
//! `cargo run -p psx-iso --example cdtest_file -- OUT SECTORS`.
//! `mkisopsx --file OUT` puts it on a disc under its upper-cased name.

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().expect("usage: cdtest_file OUT SECTORS");
    let sectors: usize = args
        .next()
        .expect("usage: cdtest_file OUT SECTORS")
        .parse()
        .expect("SECTORS is a number");
    std::fs::write(&out, psx_iso::cd_stream_bench_payload(sectors)).expect("write the file");
    println!("{out}: {sectors} sectors");
}
