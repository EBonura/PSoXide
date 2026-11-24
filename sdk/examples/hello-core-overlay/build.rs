//! The link order a `psoxide-pgo apply --variant ...+order` relink names in `PSOXIDE_LINK_ORDER`
//! (tools/pgo_search_gate.sh relinks this program with a searched layout). Without it the link is
//! unchanged.

fn main() {
    println!("cargo:rerun-if-env-changed=PSOXIDE_LINK_ORDER");
    if let Some(order) = std::env::var_os("PSOXIDE_LINK_ORDER") {
        println!(
            "cargo:rustc-link-arg=--symbol-ordering-file={}",
            order.to_string_lossy()
        );
    }
}
