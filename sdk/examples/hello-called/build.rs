//! The caller embeds the called program's EXE, built first (build.sh), named by
//! `HELLO_CALLED_BLOB`. The blob build itself needs nothing.

fn main() {
    println!("cargo:rerun-if-env-changed=HELLO_CALLED_BLOB");
    if std::env::var_os("CARGO_FEATURE_BLOB").is_none() {
        let blob = std::env::var("HELLO_CALLED_BLOB")
            .expect("HELLO_CALLED_BLOB names the called program's EXE (build.sh builds it first)");
        println!("cargo:rerun-if-changed={blob}");
        println!("cargo:rustc-env=HELLO_CALLED_BLOB_EXE={blob}");
    }
}
