//! Native side of the toolchain check: print the reference hash.
//!
//! ```sh
//! cargo run -p psx-grid --example toolchain_oracle -- <grid_level.pgl> <grid_rooms.pgr>
//! ```
//!
//! The value this prints is what a correctly compiled guest must also produce.
//! See [`psx_grid::toolchain_probe`] and `tools/toolchain_check.sh`.

/// A byte copy with the 4-byte alignment the in-place readers need.
fn aligned(bytes: &[u8]) -> Vec<u32> {
    let mut words = vec![0u32; bytes.len().div_ceil(4)];
    for (index, byte) in bytes.iter().enumerate() {
        words[index / 4] |= u32::from(*byte) << ((index % 4) * 8);
    }
    words
}

fn as_bytes(words: &[u32], len: usize) -> &[u8] {
    // SAFETY: `words` owns `len.div_ceil(4) * 4` initialised bytes.
    unsafe { core::slice::from_raw_parts(words.as_ptr().cast::<u8>(), len) }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = "engine/examples/editor-playtest/generated";
    let level_path = args
        .next()
        .unwrap_or_else(|| format!("{dir}/grid_level.pgl"));
    let rooms_path = args
        .next()
        .unwrap_or_else(|| format!("{dir}/grid_rooms.pgr"));
    let level = std::fs::read(&level_path).unwrap_or_else(|e| panic!("read {level_path}: {e}"));
    let rooms = std::fs::read(&rooms_path).unwrap_or_else(|e| panic!("read {rooms_path}: {e}"));
    let (level_words, rooms_words) = (aligned(&level), aligned(&rooms));
    println!(
        "0x{:08x}",
        psx_grid::toolchain_probe::compute_hash(
            as_bytes(&level_words, level.len()),
            as_bytes(&rooms_words, rooms.len())
        )
    );
}
