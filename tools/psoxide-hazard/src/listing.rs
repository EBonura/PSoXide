//! The disassembly every check reads: one entry per word objdump would list.

use psx_disasm::objdump_listing;

/// Size of the PS-EXE header in front of the loaded bytes.
pub const HEADER: i64 = 0x800;
/// Where an image without a PS-EXE header loads.
pub const LOAD_ADDR: i64 = 0x8001_0000;

/// Why [`pad_to_payload`] left a file alone.
#[derive(Debug, PartialEq, Eq)]
pub enum Padding {
    /// No `PS-X EXE` header: an image without one has nothing to pad to.
    NotAnExe,
    /// The header claims `expected` payload bytes and the file has `actual`,
    /// a whole sector or more short.
    Truncated { expected: usize, actual: usize },
}

/// Zero-fill a linked executable to the payload size its header claims and
/// return how many bytes that added.
///
/// `psoxide.ld` starts `.bss` where `.data` ends and rounds the header's size
/// up to whole sectors, so the flat file the linker writes can end up to 2047
/// bytes short of it. The same rule as `psx_iso::pad_to_payload`, kept here
/// so this tool has no dependency that a downstream `Cargo.lock` would have
/// to learn about before it can take a new SDK.
pub fn pad_to_payload(bytes: &mut Vec<u8>) -> Result<usize, Padding> {
    const SECTOR: usize = 0x800;
    if bytes.len() < HEADER as usize || !bytes.starts_with(b"PS-X EXE") {
        return Err(Padding::NotAnExe);
    }
    let expected = u32::from_le_bytes(bytes[0x1C..0x20].try_into().unwrap()) as usize;
    let actual = bytes.len() - HEADER as usize;
    let Some(missing) = expected.checked_sub(actual) else {
        return Ok(0);
    };
    if missing >= SECTOR {
        return Err(Padding::Truncated { expected, actual });
    }
    bytes.resize(bytes.len() + missing, 0);
    Ok(missing)
}

/// One listed instruction: objdump's mnemonic (`.word` for a word it does
/// not decode) and its operand text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Mnemonic, aliases included.
    pub op: &'static str,
    /// Operands as objdump prints them, e.g. `a0,4(sp)`.
    pub args: String,
}

/// The listing of a whole file (header included), keyed by address. A word
/// inside a run of zeros objdump collapses to `...` has no entry.
#[derive(Clone, Debug)]
pub struct Listing {
    start: i64,
    entries: Vec<Option<Entry>>,
}

impl Listing {
    /// Disassemble `data` with its first byte at `base - HEADER`, as
    /// `objdump -D -b binary -m mips:3000 -EL --adjust-vma=base-0x800` does.
    pub fn new(data: &[u8], base: i64) -> Self {
        let start = base - HEADER;
        let mut entries = vec![None; data.len() / 4];
        objdump_listing(data, start as u32, |addr, word, insn| {
            let index = (addr.wrapping_sub(start as u32) / 4) as usize;
            entries[index] = Some(match insn {
                Some(insn) => Entry {
                    op: insn.mnemonic,
                    args: insn.to_string(),
                },
                None => Entry {
                    op: ".word",
                    args: format!("{word:#x}"),
                },
            });
        });
        Self { start, entries }
    }

    fn index(&self, addr: i64) -> Option<usize> {
        let offset = addr - self.start;
        if offset < 0 || offset % 4 != 0 {
            return None;
        }
        let index = (offset / 4) as usize;
        (index < self.entries.len()).then_some(index)
    }

    /// The entry at `addr`, if objdump lists one there.
    pub fn get(&self, addr: i64) -> Option<&Entry> {
        self.index(addr).and_then(|i| self.entries[i].as_ref())
    }

    /// True when objdump lists `addr`.
    pub fn contains(&self, addr: i64) -> bool {
        self.get(addr).is_some()
    }

    /// The mnemonic at `addr`, or `""` when nothing is listed there.
    pub fn op(&self, addr: i64) -> &'static str {
        self.get(addr).map_or("", |e| e.op)
    }

    /// The operands at `addr`, or `""` when nothing is listed there.
    pub fn args(&self, addr: i64) -> &str {
        self.get(addr).map_or("", |e| e.args.as_str())
    }

    /// Forget every entry outside `[lo, hi)`. A caller that has the link
    /// map's `.text` bounds lists only code: 4bpp texture bytes elsewhere in
    /// the load decode as plausible branches (0x11111111 is `beq t0,s1`).
    pub fn retain_text(&mut self, lo: i64, hi: i64) {
        let start = self.start;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            let addr = start + 4 * i as i64;
            if addr < lo || addr >= hi {
                *entry = None;
            }
        }
    }

    /// Every listed address with its entry, in address order.
    pub fn iter(&self) -> impl Iterator<Item = (i64, &Entry)> + '_ {
        self.entries
            .iter()
            .enumerate()
            .filter_map(move |(i, e)| e.as_ref().map(|e| (self.start + 4 * i as i64, e)))
    }
}

/// The header's load address (`t_addr`); an image without a PS-EXE header
/// loads at [`LOAD_ADDR`].
pub fn load_address(data: &[u8]) -> i64 {
    if data.starts_with(b"PS-X EXE") && data.len() >= 0x1C {
        i64::from(u32::from_le_bytes(data[0x18..0x1C].try_into().unwrap()))
    } else {
        LOAD_ADDR
    }
}

/// The little-endian word at file offset `offset`, or `None` outside it.
pub fn word_at_offset(data: &[u8], offset: i64) -> Option<u32> {
    let offset = usize::try_from(offset).ok()?;
    let bytes = data.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(bytes.try_into().unwrap()))
}
