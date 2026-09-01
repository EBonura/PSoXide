//! Native side of the toolchain check: print the reference hash.
//!
//! ```sh
//! cargo run -p psx-bsp --example toolchain_oracle -- <map.pxbsp>
//! ```
//!
//! The value this prints is what a correctly compiled guest must also produce.
//! See [`psx_bsp::toolchain_probe`] and `tools/toolchain_check.sh`.

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "engine/examples/editor-playtest/generated/brush_world.pxbsp".to_string()
    });
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    println!("0x{:08x}", psx_bsp::toolchain_probe::compute_hash(&bytes));
}
