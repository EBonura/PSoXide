//! Host inspector for cooked XBSP maps.
//!
//! Accepts raw `.xbsp` files or `PSOXWPAK` bundles (quake-psx `WORLD.PAK`)
//! and prints a validated lump table per map. This is the P0 gate driver in
//! docs/quake-bsp-migration-plan.md: every map must index, size-check and
//! record-walk cleanly.
//!
//! ```sh
//! cargo run -p psx-bsp --example xbspinfo -- WORLD.PAK
//! ```

use psx_bsp::{
    CookedRecord, LumpKind, Node, PsbIndex, SliceReader, LUMP_HEADER_BYTES, PSB3_MAGIC,
    PSB_HEADER_BYTES, PSB_MAGIC,
};

const PAK_MAGIC: &[u8; 8] = b"PSOXWPAK";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: xbspinfo <map.xbsp | WORLD.PAK> ...");
        std::process::exit(2);
    }

    let mut failures = 0usize;
    for path in &args {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("{path}: {err}");
                failures += 1;
                continue;
            }
        };

        if bytes.len() >= PAK_MAGIC.len() && &bytes[..PAK_MAGIC.len()] == PAK_MAGIC {
            match inspect_pack(path, &bytes) {
                Ok(maps) => println!("{path}: {maps} map chunk(s) parsed"),
                Err(err) => {
                    eprintln!("{path}: {err}");
                    failures += 1;
                }
            }
        } else {
            match inspect(&bytes) {
                Ok(summary) => {
                    println!("{path}:");
                    print!("{summary}");
                }
                Err(err) => {
                    eprintln!("{path}: {err}");
                    failures += 1;
                }
            }
        }
    }

    if failures > 0 {
        eprintln!("{failures} input(s) failed");
        std::process::exit(1);
    }
}

/// Walk a PSOXWPAK world pack: verify every chunk's checksum, decompress the
/// HLZC payloads, and inspect each chunk that carries an XBSP map. Errors if
/// no chunk parses as a map.
fn inspect_pack(path: &str, bytes: &[u8]) -> Result<usize, String> {
    let header = psx_pack::parse_header(bytes).ok_or("bad PSOXWPAK header")?;
    let mut maps = 0usize;
    for index in 0..header.chunk_count as usize {
        let entry = psx_pack::parse_entry(bytes, index).ok_or("chunk table truncated")?;
        let start = entry.sector_offset as usize * psx_pack::SECTOR_BYTES;
        let payload = bytes
            .get(start..start + entry.byte_size as usize)
            .ok_or_else(|| format!("chunk {}: payload out of range", entry.chunk_id))?;
        if psx_pack::fnv1a32(payload) != entry.checksum {
            return Err(format!("chunk {}: checksum mismatch", entry.chunk_id));
        }

        // In-place HLZC decode: output [0, raw_len) never reaches the input
        // staged at cap - comp_len when cap = raw_len + payload len.
        let raw_len = if payload.get(..4) == Some(&psx_pack::HLZC_MAGIC[..]) {
            u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]) as usize
        } else {
            payload.len()
        };
        let mut buf = vec![0u8; raw_len + payload.len()];
        buf[..payload.len()].copy_from_slice(payload);
        let decoded = psx_pack::decompress_hlzc_in_place(&mut buf, payload.len())
            .ok_or_else(|| format!("chunk {}: HLZC decode failed", entry.chunk_id))?;

        let map = &buf[..decoded];
        if map.get(..4) == Some(&PSB_MAGIC.to_le_bytes()[..])
            || map.get(..4) == Some(&PSB3_MAGIC.to_le_bytes()[..])
        {
            match inspect(map) {
                Ok(summary) => {
                    maps += 1;
                    println!(
                        "{path} chunk {} ({} -> {} bytes):",
                        entry.chunk_id, entry.byte_size, decoded
                    );
                    print!("{summary}");
                }
                Err(err) => return Err(format!("chunk {}: {err}", entry.chunk_id)),
            }
        } else {
            let prefix: Vec<String> = map.iter().take(8).map(|b| format!("{b:02x}")).collect();
            println!(
                "{path} chunk {}: {} bytes, not an XBSP map (starts {})",
                entry.chunk_id,
                decoded,
                prefix.join(" ")
            );
        }
    }
    if maps == 0 {
        return Err("no XBSP map chunks found".into());
    }
    Ok(maps)
}

/// Parse one map that starts at the head of `bytes`. The map's true length
/// is discovered from the index's trailing-data check: a first pass over the
/// oversized slice reports where the lump walk ended, the second pass
/// validates exactly that many bytes.
fn inspect(bytes: &[u8]) -> Result<String, String> {
    let index = match PsbIndex::read(&mut SliceReader::new(bytes)) {
        Ok(index) => index,
        Err(psx_bsp::PsbError::TrailingData { parsed, .. }) => {
            let exact = &bytes[..parsed as usize];
            PsbIndex::read(&mut SliceReader::new(exact)).map_err(|err| format!("{err:?}"))?
        }
        Err(err) => return Err(format!("{err:?}")),
    };

    let mut out = String::new();
    out.push_str(&format!(
        "  map bytes: {} (header {} + {} lump headers)\n",
        index.file_len(),
        PSB_HEADER_BYTES,
        LUMP_HEADER_BYTES * psx_bsp::LUMP_COUNT as u32,
    ));
    for kind in LumpKind::ALL {
        let range = index.lump(kind);
        match kind.record_size(index.version()) {
            Some(size) if size > 0 => {
                if range.len % size != 0 {
                    return Err(format!(
                        "{kind:?}: {} bytes not a multiple of record size {size}",
                        range.len
                    ));
                }
                out.push_str(&format!(
                    "  {kind:?}: {} records ({} bytes)\n",
                    range.len / size,
                    range.len
                ));
            }
            _ => out.push_str(&format!("  {kind:?}: {} bytes\n", range.len)),
        }
    }

    walk(bytes, &index)?;
    out.push_str("  integrity walk: ok\n");
    Ok(out)
}

/// Cross-lump index checks: every node and face must reference in-range
/// planes, and node children must reference in-range nodes or leaves.
fn walk(bytes: &[u8], index: &PsbIndex) -> Result<(), String> {
    let lump = |kind: LumpKind| {
        let range = index.lump(kind);
        &bytes[range.offset as usize..(range.offset + range.len) as usize]
    };
    let planes =
        index.lump(LumpKind::Planes).len / LumpKind::Planes.record_size(index.version()).unwrap();
    let leaves =
        index.lump(LumpKind::Leaves).len / LumpKind::Leaves.record_size(index.version()).unwrap();

    let node_size = LumpKind::Nodes.record_size(index.version()).unwrap() as usize;
    let node_bytes = lump(LumpKind::Nodes);
    let node_count = node_bytes.len() / node_size;
    for (i, record) in node_bytes.chunks_exact(node_size).enumerate() {
        let node = Node::decode(&record[..Node::SIZE]);
        if u32::from(node.plane) >= planes {
            return Err(format!("node {i}: plane {} out of range", node.plane));
        }
        for child in node.children {
            let ok = if child < 0 {
                u32::from((-child - 1) as u16) < leaves
            } else {
                (child as usize) < node_count
            };
            if !ok {
                return Err(format!("node {i}: child {child} out of range"));
            }
        }
    }

    let face_size = LumpKind::Faces.record_size(index.version()).unwrap() as usize;
    for (i, record) in lump(LumpKind::Faces).chunks_exact(face_size).enumerate() {
        let plane = i16::from_le_bytes(record[0..2].try_into().unwrap());
        if plane < 0 || plane as u32 >= planes {
            return Err(format!("face {i}: plane {plane} out of range"));
        }
    }
    Ok(())
}
