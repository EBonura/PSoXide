//! Differential check against GNU objdump, the tool this crate replaces.
//!
//! Ignored by default because it needs a MIPS objdump (`OBJDUMP`, or
//! `mipsel-none-elf-objdump` on PATH):
//!
//!     cargo test -p psx-disasm --test objdump -- --ignored
//!
//! It lists a field-by-field sweep of the opcode space plus random words
//! (`PSX_DISASM_RANDOM` of them, default 1 000 000, a tenth of them zero so
//! the `...` collapsing is exercised) with both, and compares every line.
//! `PSX_DISASM_FILES` adds images (colon separated) to compare whole, the
//! way the guest checks list them: `--adjust-vma` of the header's load
//! address minus 0x800.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use psx_disasm::objdump_listing;

fn objdump() -> String {
    env::var("OBJDUMP").unwrap_or_else(|_| "mipsel-none-elf-objdump".into())
}

/// `address -> (mnemonic, operands)` from objdump's text, parsed as the old
/// Python tools did: `\s*([0-9a-f]+):\s+[0-9a-f]{8}\s+(\S+)\s*(.*)`.
fn parse(text: &str) -> BTreeMap<u32, (String, String)> {
    let mut listing = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((addr, rest)) = line.split_once(':') else {
            continue;
        };
        let Ok(addr) = u32::from_str_radix(addr, 16) else {
            continue;
        };
        if addr.to_string().is_empty() || !line.starts_with(|c: char| c.is_ascii_hexdigit()) {
            continue;
        }
        let rest = rest.trim_start();
        if rest.len() < 8 || !rest[..8].chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let rest = rest[8..].trim_start();
        let (op, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        listing.insert(addr, (op.to_string(), args.trim_start().to_string()));
    }
    listing
}

fn ours(data: &[u8], vma: u32) -> BTreeMap<u32, (String, String)> {
    let mut listing = BTreeMap::new();
    objdump_listing(data, vma, |addr, word, insn| {
        let entry = match insn {
            Some(insn) => (insn.mnemonic.to_string(), insn.to_string()),
            None => (".word".to_string(), format!("{word:#x}")),
        };
        listing.insert(addr, entry);
    });
    listing
}

fn compare(data: &[u8], vma: u32, what: &str) {
    let dir = env::temp_dir().join(format!("psx-disasm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let name: String = what
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path: PathBuf = dir.join(format!("{name}.bin"));
    std::fs::write(&path, data).unwrap();
    let out = Command::new(objdump())
        .args(["-D", "-b", "binary", "-m", "mips:3000", "-EL"])
        .arg(format!("--adjust-vma={vma:#x}"))
        .arg(&path)
        .output()
        .expect("run objdump");
    std::fs::remove_file(&path).unwrap();
    let theirs = parse(&String::from_utf8_lossy(&out.stdout));
    let mine = ours(data, vma);
    let mut report = String::new();
    let mut bad = 0;
    for addr in theirs
        .keys()
        .chain(mine.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let (a, b) = (theirs.get(addr), mine.get(addr));
        if a != b {
            bad += 1;
            if bad <= 40 {
                let off = (addr.wrapping_sub(vma)) as usize;
                let word = data
                    .get(off..off + 4)
                    .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
                let _ = writeln!(report, "{addr:08x} {word:08x?}: objdump {a:?}, ours {b:?}");
            }
        }
    }
    assert_eq!(bad, 0, "{what}: {bad} lines differ\n{report}");
    eprintln!("{what}: {} lines identical", theirs.len());
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

fn words_bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
#[ignore = "needs a MIPS objdump"]
fn field_sweep_matches_objdump() {
    // Every opcode with each field at 0, 1 and 31 and the low half at
    // values that matter to the masks, each word followed by `li at,1` so
    // no zero run hides it.
    let fields = [0u32, 1, 2, 31];
    let lows = [
        0u32, 1, 0x3f, 0x40, 0x7ff, 0x800, 0x8000, 0xffff, 0x10, 0x30, 0x20,
    ];
    let mut words = Vec::new();
    for op in 0..64u32 {
        for rs in 0..32u32 {
            for rt in fields.iter().chain([16, 17].iter()) {
                for low in lows.iter().chain((0..64).collect::<Vec<_>>().iter()) {
                    for rd in [0u32, 1, 31] {
                        let low = if op == 0 || (16..20).contains(&op) {
                            (low & 0x7ff & !(31 << 11)) | rd << 11 | (low & 63)
                        } else {
                            *low
                        };
                        words.push(op << 26 | rs << 21 | rt << 16 | low);
                        words.push(0x2401_0001);
                    }
                }
            }
        }
    }
    compare(&words_bytes(&words), 0x8000_0000, "field sweep");
}

#[test]
#[ignore = "needs a MIPS objdump"]
fn random_words_match_objdump() {
    let count = env::var("PSX_DISASM_RANDOM")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(1_000_000);
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut words = Vec::with_capacity(count);
    for _ in 0..count {
        let roll = rng.next();
        words.push(match roll % 10 {
            0 => 0,
            // Bias toward the coprocessor and SPECIAL spaces, where the
            // masks are.
            1 => rng.next() & 0x03FF_FFFF,
            2 => (0x10 | (rng.next() & 3)) << 26 | (rng.next() & 0x03FF_FFFF),
            3 => 0x4400_0000 | (rng.next() & 0x03FF_FFFF) & 0x03E0_F83F | (rng.next() & 1) << 6,
            _ => rng.next(),
        });
    }
    compare(&words_bytes(&words), 0x8000_0000, "random words");
}

#[test]
#[ignore = "needs a MIPS objdump"]
fn images_match_objdump() {
    let Ok(files) = env::var("PSX_DISASM_FILES") else {
        return;
    };
    for file in files.split(':').filter(|f| !f.is_empty()) {
        let data = std::fs::read(file).unwrap();
        let base = if data.starts_with(b"PS-X EXE") {
            u32::from_le_bytes(data[0x18..0x1c].try_into().unwrap())
        } else {
            0x8001_0000
        };
        compare(&data, base.wrapping_sub(0x800), file);
    }
}
