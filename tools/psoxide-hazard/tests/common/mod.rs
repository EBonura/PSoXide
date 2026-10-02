//! Fixture encoders, a PS-EXE writer and a small R3000 interpreter that
//! delivers a load one instruction late, shared by the tool tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub const NAMES: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp",
    "ra",
];
pub const MAGIC: u32 = 0x4841_5A54;
/// Every register starts as STALE + its number.
pub const STALE: u32 = 0x5A1E_0000;
/// What the slot load fetches.
pub const VALUE: u32 = 0x2A;
pub const NOP: u32 = 0;
pub const BREAK: u32 = 0x0D;

pub fn reg(name: &str) -> u32 {
    NAMES
        .iter()
        .position(|n| *n == name)
        .unwrap_or_else(|| panic!("register {name}")) as u32
}

pub fn r_type(funct: u32, rs: &str, rt: &str, rd: &str) -> u32 {
    reg(rs) << 21 | reg(rt) << 16 | reg(rd) << 11 | funct
}
pub fn i_type(op: u32, rs: &str, rt: &str, imm: i64) -> u32 {
    op << 26 | reg(rs) << 21 | reg(rt) << 16 | (imm as u32 & 0xFFFF)
}
pub fn addu(rd: &str, rs: &str, rt: &str) -> u32 {
    r_type(0x21, rs, rt, rd)
}
pub fn or(rd: &str, rs: &str, rt: &str) -> u32 {
    r_type(0x25, rs, rt, rd)
}
pub fn sll(rd: &str, rt: &str, sa: u32) -> u32 {
    reg(rt) << 16 | reg(rd) << 11 | sa << 6
}
pub fn jr(rs: &str) -> u32 {
    r_type(0x08, rs, "zero", "zero")
}
pub fn jalr(rs: &str) -> u32 {
    r_type(0x09, rs, "zero", "ra")
}
pub fn addiu(rt: &str, rs: &str, imm: i64) -> u32 {
    i_type(0x09, rs, rt, imm)
}
pub fn lui(rt: &str, imm: i64) -> u32 {
    i_type(0x0F, "zero", rt, imm)
}
pub fn ori(rt: &str, rs: &str, imm: i64) -> u32 {
    i_type(0x0D, rs, rt, imm)
}
pub fn lw(rt: &str, off: i64, rs: &str) -> u32 {
    i_type(0x23, rs, rt, off)
}
pub fn lbu(rt: &str, off: i64, rs: &str) -> u32 {
    i_type(0x24, rs, rt, off)
}
pub fn sw(rt: &str, off: i64, rs: &str) -> u32 {
    i_type(0x2B, rs, rt, off)
}
pub fn beq(rs: &str, rt: &str, offset: i64) -> u32 {
    i_type(0x04, rs, rt, offset)
}
pub fn bne(rs: &str, rt: &str, offset: i64) -> u32 {
    i_type(0x05, rs, rt, offset)
}
pub fn b(offset: i64) -> u32 {
    beq("zero", "zero", offset)
}
pub fn j(target: u32) -> u32 {
    2 << 26 | (target >> 2) & 0x03FF_FFFF
}
pub fn jal(target: u32) -> u32 {
    3 << 26 | (target >> 2) & 0x03FF_FFFF
}
pub fn hi(addr: u32) -> i64 {
    i64::from((addr.wrapping_add(0x8000) >> 16) & 0xFFFF)
}
pub fn lo(addr: u32) -> i64 {
    i64::from(addr & 0xFFFF)
}

/// A PS-EXE with code at `base`, functions every 0x100 bytes, a data page at
/// +0x800 and the trampoline array at +0xC00, far enough apart that the
/// tools' 16-word data guard never sees data next to code.
#[derive(Clone)]
pub struct Image {
    pub base: u32,
    pub words: Vec<u32>,
}

impl Image {
    pub const CODE: u32 = 0x000;
    pub const DATA: u32 = 0x800;
    pub const TRAMPOLINES: u32 = 0xC00;

    pub fn new(base: u32) -> Self {
        let mut image = Self {
            base,
            words: vec![NOP; 0x400],
        };
        image.put(Self::DATA, &[VALUE]);
        image.put(Self::TRAMPOLINES, &[MAGIC, 64]);
        image
    }

    pub fn addr(&self, offset: u32) -> u32 {
        self.base + offset
    }

    pub fn put(&mut self, offset: u32, words: &[u32]) {
        for (i, w) in words.iter().enumerate() {
            self.words[offset as usize / 4 + i] = *w;
        }
    }

    pub fn write(&self, path: &Path) {
        let mut data = vec![0u8; 0x800];
        data[..8].copy_from_slice(b"PS-X EXE");
        for (i, v) in [self.base, 0, self.base, self.words.len() as u32 * 4]
            .iter()
            .enumerate()
        {
            data[0x10 + 4 * i..0x14 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        for w in &self.words {
            data.extend_from_slice(&w.to_le_bytes());
        }
        std::fs::write(path, data).unwrap();
    }
}

/// Execute from the header's entry until `break`; loads land one
/// instruction late, as on the R3000. Returns the register file.
pub fn run(path: &Path) -> [u32; 32] {
    let data = std::fs::read(path).unwrap();
    let word = |mem: &[u8], at: u32| {
        u32::from_le_bytes(mem[at as usize..at as usize + 4].try_into().unwrap())
    };
    let base = word(&data, 0x18);
    let mem = &data[0x800..];
    let mut regs = [0u32; 32];
    for (i, r) in regs.iter_mut().enumerate() {
        *r = STALE + i as u32;
    }
    regs[0] = 0;
    let (mut pc, mut npc, mut pending): (u32, u32, Option<(usize, u32)>) = (base, base + 4, None);
    for _ in 0..500 {
        let w = word(mem, pc - base);
        let (op, rs, rt) = (w >> 26, (w >> 21 & 31) as usize, (w >> 16 & 31) as usize);
        let (rd, funct, imm) = ((w >> 11 & 31) as usize, w & 63, w & 0xFFFF);
        let simm = imm as u16 as i16 as i32;
        let landing = pending.take();
        let (nxt, mut nnxt) = (npc, npc + 4);
        let mut write = None;
        if op == 0 && funct == BREAK {
            return regs;
        }
        if op == 0 && funct == 0x21 {
            write = Some((rd, regs[rs].wrapping_add(regs[rt])));
        } else if op == 0 && funct == 0x08 {
            nnxt = regs[rs];
        } else if op == 0 && funct == 0x09 {
            write = Some((rd, pc + 8));
            nnxt = regs[rs];
        } else if op == 0 && w == NOP {
        } else if op == 2 || op == 3 {
            nnxt = (pc & 0xF000_0000) | (w & 0x03FF_FFFF) << 2;
            if op == 3 {
                write = Some((31, pc + 8));
            }
        } else if op == 4 || op == 5 {
            if (regs[rs] == regs[rt]) == (op == 4) {
                nnxt = npc.wrapping_add((simm << 2) as u32);
            }
        } else if op == 0x09 {
            write = Some((rt, regs[rs].wrapping_add(simm as u32)));
        } else if op == 0x0D {
            write = Some((rt, regs[rs] | imm));
        } else if op == 0x0F {
            write = Some((rt, imm << 16));
        } else if op == 0x23 || op == 0x24 {
            let addr = regs[rs].wrapping_add(simm as u32);
            let value = if op == 0x23 {
                word(mem, addr - base)
            } else {
                u32::from(mem[(addr - base) as usize])
            };
            pending = Some((rt, value));
        } else {
            panic!("fixture interpreter: unsupported word {w:08x} at {pc:08x}");
        }
        if let Some((r, v)) = write {
            if r != 0 {
                regs[r] = v;
            }
        }
        if let Some((r, v)) = landing {
            if r != 0 {
                regs[r] = v;
            }
        }
        pc = nxt;
        npc = nnxt;
    }
    panic!("fixture program did not reach break");
}

/// A temporary directory removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("psoxide-hazard-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run a library `main` with `args`, returning (status, stdout).
pub fn call(main: fn(&[String], &mut dyn std::io::Write) -> i32, args: &[&str]) -> (i32, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let status = main(&args, &mut out);
    (status, String::from_utf8(out).unwrap())
}
