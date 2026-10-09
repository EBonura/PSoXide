//! Compare cached chunk algorithms against frozen HL/CS at transport boundaries.

mod oracle_common;

use oracle_common::{cargo, here, read, root, skip_block, Scratch};
use regex::Regex;

fn remove_host_functions(mut s: String) -> String {
    let host = Regex::new(
        r#"#\[cfg\(not\(target_arch = "mips"\)\)\]\s*(?:#\[[^\n]+\]\s*)*(?:pub )?(?:unsafe )?fn \w+\([^\{]*\{"#,
    )
    .expect("static regex");
    loop {
        let Some(m) = host.find(&s) else {
            return s;
        };
        let end = skip_block(&s, m.end());
        s = format!("{}{}", &s[..m.start()], &s[end..]);
    }
}

#[test]
fn cached_chunk_algorithms_match_frozen_hl_cs() {
    let here = here();
    let root = root();
    for capacity in [16, 32] {
        let scratch = Scratch::new("chunk-oracle-");
        let old = remove_host_functions(read(&here.join("oracles/legacy-cdstream.rs")))
            .replace("#[cfg(target_arch = \"mips\")]", "")
            .replace(
                "use psx_pack::cd::{SectorReader, SECTOR_WORDS};",
                "use crate::fake::{Reader as SectorReader,SECTOR_WORDS};",
            )
            .replace("psx_pack::cd::find_entry", "crate::fake::find_entry")
            .replace(
                "use psx_io::cdrom::poll_data_sector as try_sector_ready;",
                "use crate::fake::ready as try_sector_ready;",
            )
            .replace(
                "const PERSIST_ENTRIES: usize = 16;",
                &format!("const PERSIST_ENTRIES: usize = {capacity};"),
            );
        // All state is reset between paired scenarios. Raw destination addresses
        // are not compared, only the bytes actually written and ordered events.
        let old = old
            + "\npub unsafe fn reset(){READER=SectorReader::new();SECTOR_BUF=[0;SECTOR_WORDS];PACK_CACHE_LEN=-1;PERSIST_CACHE=[CachedEntry{id:PERSIST_NONE,sector_offset:0,byte_size:0};PERSIST_ENTRIES];CHUNK_STREAM.active=false;CHUNK_STREAM.just_started=false;ON_SECTOR=None;}\n";
        scratch.write("src/old.rs", &old);
        scratch.write(
            "src/shared.rs",
            &read(&here.parent().unwrap().join("src/chunk_stream.rs")),
        );
        scratch.write(
            "src/main.rs",
            &read(&here.join("support/chunk_stream_harness.rs"))
                .replace("PERSIST_CAPACITY", &capacity.to_string()),
        );
        let deps: String = ["psx-pack", "psx-io"]
            .iter()
            .map(|n| format!("{n}={{path=\"{}/sdk/crates/{n}\"}}", root.display()))
            .collect::<Vec<_>>()
            .join("\n");
        scratch.write(
            "Cargo.toml",
            &format!(
                "[package]\nname=\"chunk-stream-oracle\"\nversion=\"0.0.0\"\nedition=\"2021\"\n[workspace]\n[dependencies]\n{deps}\n"
            ),
        );
        let output = cargo(
            &scratch.0,
            &[
                "run",
                "--release",
                "--quiet",
                "--manifest-path",
                "Cargo.toml",
            ],
        );
        println!("{output}");
    }
}
