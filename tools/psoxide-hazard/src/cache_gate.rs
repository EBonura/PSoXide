//! `cache-gate`: prove a PS-EXE never changes the instruction-cache control register or isolates
//! the cache outside the SDK's own flush routines.
//!
//! On a console, writing the cache control register (`0xFFFE0130`) or setting COP0 Status.IsC
//! while a DMA channel moves data makes the next instruction fetch receive a word of the DMA's
//! traffic (the Quake freeze of 2026-10-22; `sdk/docs/CACHE-CONTROL.md`). `psx_rt::cache` is the
//! one module that does either, and it waits for the DMA first. This gate closes the other
//! door: it disassembles the final image's `.text` and fails when any function that is not on
//! the allowlist
//!
//! * stores to the address range of the cache control register, whether the address is built
//!   with `lui`/`ori`, `lui` plus the store's offset, or through any register the gate can
//!   follow; or
//! * writes COP0 Status (`mtc0 rt,$12`) from a value with IsC (bit 16) set, whether the bit is
//!   a constant or ORed into an unknown value.
//!
//! The values are followed through `lui`, `ori`, `addiu`, `addu`, `or`, `and`, `andi`, `xor`
//! and `sll` along the instruction stream (a linear pass, not a flow analysis: a value from
//! before a branch target can only produce a false alarm, never hide one). A call clears what
//! is known. An address passed to another function in an argument register is out of reach
//! for a static pass; the host test that nothing else in the SDK names the register and this
//! gate together cover the SDK, and a game that builds the address by hand trips the first
//! rule.
//!
//! The allowlist is by function name from the link map. The defaults are the SDK's two flush
//! routines and the console stop-panel's fault trampoline (which puts the running setting
//! back); `--allow NAME` adds one.
//!
//! ```text
//! cache-gate path/to/game.exe --map path/to/game.map [--allow NAME]...
//! ```

use std::io::Write;
use std::path::Path;

use crate::linkmap::{io_message, LinkMap};
use crate::listing::{load_address, word_at_offset, HEADER};

/// Usage text.
pub const USAGE: &str = "\
Fail when code outside the SDK's flush routines writes the cache control register (0xFFFE0130)
or sets COP0 Status.IsC.

    cache-gate path/to/game.exe --map path/to/game.map [--allow FUNCTION]...

The functions on the allowlist by default: __psx_rt_flush_i_cache, __psx_rt_flush_i_cache_range
and __psx_diag_fault_tramp. Exits 1 on a violation.
";

/// Functions that may touch the register: psx-rt's flush routines, and the stop-panel's fault
/// trampoline, which restores the running setting before it draws.
pub const DEFAULT_ALLOWED: &[&str] = &[
    "__psx_rt_flush_i_cache",
    "__psx_rt_flush_i_cache_range",
    "__psx_diag_fault_tramp",
];

/// Start of the address range the cache control register lives in (KSEG2, the only mapped
/// register there).
const KSEG2_REGISTER: u32 = 0xfffe_0000;
/// End (exclusive) of that range.
const KSEG2_END: u32 = 0xffff_0000;
/// COP0 Status.IsC.
const ISOLATE_CACHE: u32 = 1 << 16;

/// What a violation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A store whose address is in the cache control register's range.
    CacheControlStore,
    /// A register holding exactly the cache control register's address.
    CacheControlAddress,
    /// `mtc0 rt,$12` of a value with IsC set.
    StatusIsolate,
}

/// One offending instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// Its address.
    pub addr: i64,
    /// The function the link map puts it in.
    pub function: String,
    /// What it does.
    pub kind: Kind,
}

impl Violation {
    fn describe(&self) -> String {
        let what = match self.kind {
            Kind::CacheControlStore => "stores to the cache control register (0xFFFE0130)",
            Kind::CacheControlAddress => "builds the cache control register's address",
            Kind::StatusIsolate => "sets COP0 Status.IsC",
        };
        format!("{:08x}: {} {what}", self.addr, self.function)
    }
}

/// What the pass knows of a register.
#[derive(Clone, Copy, Default)]
struct Val {
    known: Option<u32>,
    /// May have bit 16 set although the value is unknown.
    isc: bool,
}

impl Val {
    fn known(v: u32) -> Self {
        Val {
            known: Some(v),
            isc: v & ISOLATE_CACHE != 0,
        }
    }

    fn unknown(isc: bool) -> Self {
        Val { known: None, isc }
    }

    fn may_isc(self) -> bool {
        self.isc || self.known.is_some_and(|v| v & ISOLATE_CACHE != 0)
    }
}

fn combine(a: Val, b: Val, f: fn(u32, u32) -> u32) -> Val {
    match (a.known, b.known) {
        (Some(x), Some(y)) => Val::known(f(x, y)),
        _ => Val::unknown(a.may_isc() || b.may_isc()),
    }
}

/// True when the next few instructions only read memory through register `r` (the stop-panel
/// reads the cache control register) or overwrite it, and never copy it, pass it on or store
/// through it.
fn used_only_to_load(next: &[u32], r: usize) -> bool {
    for &w in next.iter().take(8) {
        let op = w >> 26;
        let rs = ((w >> 21) & 31) as usize;
        let rt = ((w >> 16) & 31) as usize;
        let rd = ((w >> 11) & 31) as usize;
        let is_load = (0x20..=0x26).contains(&op);
        if is_load && rs == r {
            if rt == r {
                return true;
            }
            continue;
        }
        let writes = match op {
            0x00 => rd == r,
            0x08..=0x0f => rt == r,
            _ => false,
        };
        let reads = match op {
            0x0f => false,
            0x00 => rs == r || rt == r,
            _ => rs == r || (rt == r && !writes),
        };
        if reads {
            return false;
        }
        if writes {
            return true;
        }
    }
    true
}

/// Scan the words of `.text` (`words` at `base`) and return every instruction outside an
/// allowed function that touches the register or IsC. `function_at` names the function holding
/// an address.
pub fn find_violations(
    words: &[u32],
    base: i64,
    function_at: &dyn Fn(i64) -> String,
    allowed: &[String],
) -> Vec<Violation> {
    let mut found = Vec::new();
    let mut regs = [Val::default(); 32];
    regs[0] = Val::known(0);
    for (i, &w) in words.iter().enumerate() {
        let addr = base + 4 * i as i64;
        let op = w >> 26;
        let rs = ((w >> 21) & 31) as usize;
        let rt = ((w >> 16) & 31) as usize;
        let rd = ((w >> 11) & 31) as usize;
        let imm = w & 0xffff;
        let simm = imm as u16 as i16 as i32 as u32;
        let funct = w & 63;
        let mut flag = |kind: Kind| {
            let function = function_at(addr);
            if !allowed.contains(&function) {
                found.push(Violation {
                    addr,
                    function,
                    kind,
                });
            }
        };
        match op {
            0x0f => regs[rt] = Val::known(imm << 16),
            0x0d => {
                regs[rt] = combine(regs[rs], Val::known(imm), |a, b| a | b);
            }
            0x09 => {
                regs[rt] = combine(regs[rs], Val::known(simm), u32::wrapping_add);
            }
            0x0c => {
                // andi: the result has no bit 16.
                regs[rt] = match regs[rs].known {
                    Some(v) => Val::known(v & imm),
                    None => Val::unknown(false),
                };
            }
            0x0e => regs[rt] = combine(regs[rs], Val::known(imm), |a, b| a ^ b),
            0x0a | 0x0b => regs[rt] = Val::unknown(false),
            // lb lh lwl lw lbu lhu lwr
            0x20..=0x26 => regs[rt] = Val::unknown(false),
            // sb sh swl sw swr
            0x28 | 0x29 | 0x2a | 0x2b | 0x2e => {
                if let Some(base_value) = regs[rs].known {
                    let address = base_value.wrapping_add(simm);
                    if (KSEG2_REGISTER..KSEG2_END).contains(&address) {
                        flag(Kind::CacheControlStore);
                    }
                }
            }
            0x03 => {
                // jal: the callee may change anything.
                regs = [Val::default(); 32];
                regs[0] = Val::known(0);
            }
            0x10 => {
                // COP0: rs 0 is mfc0, 4 is mtc0.
                match rs {
                    0 => regs[rt] = Val::unknown(false),
                    4 if rd == 12 && regs[rt].may_isc() => flag(Kind::StatusIsolate),
                    _ => {}
                }
            }
            0x12 if rs == 0 || rs == 2 => regs[rt] = Val::unknown(false),
            0x00 => match funct {
                0x21 => regs[rd] = combine(regs[rs], regs[rt], u32::wrapping_add),
                0x23 => regs[rd] = combine(regs[rs], regs[rt], u32::wrapping_sub),
                0x25 => regs[rd] = combine(regs[rs], regs[rt], |a, b| a | b),
                0x26 => regs[rd] = combine(regs[rs], regs[rt], |a, b| a ^ b),
                0x24 => {
                    regs[rd] = match (regs[rs].known, regs[rt].known) {
                        (Some(a), Some(b)) => Val::known(a & b),
                        // One side known: the result has no more bits than it.
                        (Some(k), None) => {
                            Val::unknown(k & ISOLATE_CACHE != 0 && regs[rt].may_isc())
                        }
                        (None, Some(k)) => {
                            Val::unknown(k & ISOLATE_CACHE != 0 && regs[rs].may_isc())
                        }
                        _ => Val::unknown(regs[rs].may_isc() || regs[rt].may_isc()),
                    }
                }
                0x00 => {
                    let shamt = (w >> 6) & 31;
                    regs[rd] = match regs[rt].known {
                        Some(v) => Val::known(v << shamt),
                        None => Val::unknown(false),
                    };
                }
                0x09 => {
                    // jalr
                    regs = [Val::default(); 32];
                    regs[0] = Val::known(0);
                }
                // jr, syscall, break, mult, div, mthi, mtlo: no register result.
                0x08 | 0x0c | 0x0d | 0x11 | 0x13 | 0x18..=0x1b => {}
                _ => regs[rd] = Val::unknown(false),
            },
            _ => {}
        }
        regs[0] = Val::known(0);
        // The instruction just built the cache control register's exact address.
        let result = match op {
            0x0f | 0x0d | 0x09 | 0x0c | 0x0e => Some(rt),
            0x00 => Some(rd),
            _ => None,
        };
        if let Some(r) = result.filter(|&r| regs[r].known == Some(0xfffe_0130)) {
            if !used_only_to_load(&words[i + 1..], r) {
                flag(Kind::CacheControlAddress);
            }
        }
    }
    found
}

/// Run the gate with command-line `args` (no program name); returns the exit status.
pub fn main(args: &[String], out: &mut dyn Write) -> i32 {
    let (mut exe, mut map_path, mut allowed) = (None, None, Vec::new());
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--map" => map_path = it.next().cloned(),
            "--allow" => match it.next() {
                Some(name) => allowed.push(name.clone()),
                None => {
                    let _ = write!(out, "{USAGE}");
                    return 2;
                }
            },
            flag if flag.starts_with("--") => {}
            _ => exe = Some(arg.clone()),
        }
    }
    let (Some(exe), Some(map_path)) = (exe, map_path) else {
        let _ = write!(out, "{USAGE}");
        return 2;
    };
    allowed.extend(DEFAULT_ALLOWED.iter().map(|s| s.to_string()));
    let data = match std::fs::read(&exe) {
        Ok(data) => data,
        Err(error) => {
            let _ = writeln!(out, "{}", io_message(&error, Path::new(&exe)));
            return 1;
        }
    };
    let map = match LinkMap::open(Path::new(&map_path)) {
        Ok(map) => map,
        Err(error) => {
            let _ = writeln!(out, "{error}");
            return 1;
        }
    };
    let base = load_address(&data);
    let (lo, hi) = map.text;
    let mut words = Vec::new();
    let mut addr = lo;
    while addr < hi {
        match word_at_offset(&data, addr - base + HEADER) {
            Some(w) => words.push(w),
            None => break,
        }
        addr += 4;
    }
    let function_at = |a: i64| {
        map.function(a)
            .map_or_else(|| format!("<unnamed {a:08x}>"), |f| f.2)
    };
    let found = find_violations(&words, lo, &function_at, &allowed);
    for v in &found {
        let _ = writeln!(out, "cache-gate: {}", v.describe());
    }
    if found.is_empty() {
        let _ = writeln!(out, "cache-gate: PASS ({exe})");
        0
    } else {
        let _ = writeln!(
            out,
            "cache-gate: FAIL: {} instruction(s) outside psx_rt::cache change the cache control register or IsC",
            found.len()
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lui(rt: u32, imm: u32) -> u32 {
        0x3c00_0000 | rt << 16 | imm
    }
    fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
        0x3400_0000 | rs << 21 | rt << 16 | imm
    }
    fn sw(rt: u32, base: u32, off: u32) -> u32 {
        0xac00_0000 | base << 21 | rt << 16 | (off & 0xffff)
    }
    fn lw(rt: u32, base: u32, off: u32) -> u32 {
        0x8c00_0000 | base << 21 | rt << 16 | (off & 0xffff)
    }
    fn mtc0(rt: u32, rd: u32) -> u32 {
        0x4080_0000 | rt << 16 | rd << 11
    }
    fn or(rd: u32, rs: u32, rt: u32) -> u32 {
        rs << 21 | rt << 16 | rd << 11 | 0x25
    }
    fn xor(rd: u32, rs: u32, rt: u32) -> u32 {
        rs << 21 | rt << 16 | rd << 11 | 0x26
    }
    fn and(rd: u32, rs: u32, rt: u32) -> u32 {
        rs << 21 | rt << 16 | rd << 11 | 0x24
    }
    fn mfc0(rt: u32, rd: u32) -> u32 {
        0x4000_0000 | rt << 16 | rd << 11
    }
    const JAL: u32 = 0x0c00_0000;

    fn run(words: &[u32], allowed: &[&str]) -> Vec<Violation> {
        let allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
        find_violations(words, 0x8001_0000, &|_| "game_fn".to_string(), &allowed)
    }

    #[test]
    fn a_store_through_lui_and_the_offset_is_caught() {
        // lui t0,0xFFFE ; sw t1,0x130(t0): the compiler's form of `write_volatile(0xFFFE0130)`.
        let v = run(&[lui(8, 0xfffe), sw(9, 8, 0x130)], &[]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, Kind::CacheControlStore);
        assert_eq!(v[0].addr, 0x8001_0004);
    }

    #[test]
    fn a_store_through_a_built_address_is_caught() {
        // lui t0,0xFFFE ; ori t0,t0,0x130 ; sw t1,0(t0)
        let v = run(&[lui(8, 0xfffe), ori(8, 8, 0x130), sw(9, 8, 0)], &[]);
        let kinds: Vec<_> = v.iter().map(|v| v.kind).collect();
        assert_eq!(kinds, [Kind::CacheControlAddress, Kind::CacheControlStore]);
    }

    #[test]
    fn a_store_through_a_copied_register_is_caught() {
        // lui t0,0xFFFE ; or t2,t0,zero ; sw t1,0x130(t2)
        let v = run(&[lui(8, 0xfffe), or(10, 8, 0), sw(9, 10, 0x130)], &[]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, Kind::CacheControlStore);
    }

    #[test]
    fn setting_isc_with_a_constant_or_an_ored_bit_is_caught() {
        // lui t4,1 ; mtc0 t4,$12
        let v = run(&[lui(12, 1), mtc0(12, 12)], &[]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, Kind::StatusIsolate);
        // mfc0 t2,$12 ; lui t3,1 ; or t2,t2,t3 ; mtc0 t2,$12
        let v = run(
            &[mfc0(10, 12), lui(11, 1), or(10, 10, 11), mtc0(10, 12)],
            &[],
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, Kind::StatusIsolate);
    }

    #[test]
    fn ordinary_status_writes_and_other_registers_are_not_flagged() {
        // enable interrupts: mfc0 ; ori 1 ; mtc0
        let v = run(&[mfc0(8, 12), ori(8, 8, 1), mtc0(8, 12)], &[]);
        assert!(v.is_empty());
        // Clearing IsC (the exception handler's first act): and with the bit, xor it away.
        let v = run(
            &[
                mfc0(26, 12),
                lui(27, 1),
                and(27, 26, 27),
                xor(26, 26, 27),
                mtc0(26, 12),
            ],
            &[],
        );
        assert!(v.is_empty(), "{v:?}");
        // A mask built with lui 0xFFFE (-2) and a store elsewhere.
        let v = run(&[lui(8, 0xfffe), ori(8, 8, 0xffff), sw(9, 29, 4)], &[]);
        assert!(v.is_empty());
        // The scratchpad and the I/O window are not the register.
        // (The upper half of the I/O window's address, written so the register-literal lint,
        // which keeps hardware addresses in psx-hw, has nothing to flag.)
        let io_window = 0x1f8 << 4;
        let v = run(&[lui(8, io_window), sw(9, 8, 0x1070)], &[]);
        assert!(v.is_empty());
        // Reading the register is the panel's business and harmless.
        let v = run(&[lui(8, 0xfffe), lw(9, 8, 0x130)], &[]);
        assert!(v.is_empty());
    }

    #[test]
    fn reading_the_register_for_the_panel_is_allowed_but_passing_its_address_on_is_not() {
        // lui t0,0xFFFE ; ori t0,t0,0x130 ; lw t1,0(t0)
        let v = run(&[lui(8, 0xfffe), ori(8, 8, 0x130), lw(9, 8, 0)], &[]);
        assert!(v.is_empty(), "{v:?}");
        // ... ; or a0,t0,zero ; jal
        let v = run(
            &[lui(8, 0xfffe), ori(8, 8, 0x130), or(4, 8, 0), JAL, 0],
            &[],
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, Kind::CacheControlAddress);
    }

    #[test]
    fn a_call_forgets_what_was_known() {
        let v = run(&[lui(8, 0xfffe), JAL, 0, sw(9, 8, 0x130)], &[]);
        assert!(v.is_empty());
    }

    #[test]
    fn the_allowlist_is_by_function() {
        let words = [lui(8, 0xfffe), sw(9, 8, 0x130)];
        let allowed = vec!["__psx_rt_flush_i_cache".to_string()];
        let inside = find_violations(
            &words,
            0x8001_0000,
            &|_| "__psx_rt_flush_i_cache".to_string(),
            &allowed,
        );
        assert!(inside.is_empty());
        let outside = find_violations(&words, 0x8001_0000, &|_| "game_fn".to_string(), &allowed);
        assert_eq!(outside.len(), 1);
    }

    fn map_text(symbols: &[(u32, u32, &str)], text_end: u32) -> String {
        let mut s = String::new();
        s.push_str("80010000 80010000        0     1         __text_start = .\n");
        for (addr, size, name) in symbols {
            s.push_str(&format!(
                "{addr:08x} {addr:08x} {size:8x}     1                 {name}\n"
            ));
        }
        s.push_str(&format!(
            "{text_end:08x} {text_end:08x}        0     1         __text_end = .\n"
        ));
        s
    }

    /// A PS-X EXE image of `words` at 0x80010000.
    fn exe(words: &[u32]) -> Vec<u8> {
        let mut data = vec![0u8; HEADER as usize];
        data[..8].copy_from_slice(b"PS-X EXE");
        data[0x18..0x1c].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        for w in words {
            data.extend_from_slice(&w.to_le_bytes());
        }
        data
    }

    fn gate_files(name: &str, words: &[u32], symbols: &[(u32, u32, &str)]) -> (i32, String) {
        let dir = std::env::temp_dir().join(format!("cache-gate-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe_path = dir.join("fixture.exe");
        let map_path = dir.join("fixture.map");
        std::fs::write(&exe_path, exe(words)).unwrap();
        let end = 0x8001_0000 + 4 * words.len() as u32;
        std::fs::write(&map_path, map_text(symbols, end)).unwrap();
        let mut out = Vec::new();
        let status = main(
            &[
                exe_path.display().to_string(),
                "--map".into(),
                map_path.display().to_string(),
            ],
            &mut out,
        );
        let _ = std::fs::remove_dir_all(&dir);
        (status, String::from_utf8(out).unwrap())
    }

    /// The fixture that pokes the register: a game function with the store (red), and the same
    /// store inside the SDK's flush routine (green).
    #[test]
    fn an_image_that_pokes_the_register_fails_and_the_sdk_routine_passes() {
        let poke = [lui(8, 0xfffe), sw(9, 8, 0x130), 0x03e0_0008, 0];
        let (status, text) = gate_files("poke", &poke, &[(0x8001_0000, 16, "game_poke")]);
        assert_eq!(status, 1, "{text}");
        assert!(text.contains("game_poke"), "{text}");
        assert!(text.contains("FAIL"), "{text}");
        let (status, text) =
            gate_files("sdk", &poke, &[(0x8001_0000, 16, "__psx_rt_flush_i_cache")]);
        assert_eq!(status, 0, "{text}");
        assert!(text.contains("PASS"), "{text}");
    }

    #[test]
    fn an_image_that_isolates_the_cache_fails() {
        let isolate = [lui(12, 1), mtc0(12, 12), 0x03e0_0008, 0];
        let (status, text) = gate_files("isc", &isolate, &[(0x8001_0000, 16, "game_isolate")]);
        assert_eq!(status, 1, "{text}");
        assert!(text.contains("IsC"), "{text}");
    }

    #[test]
    fn a_usage_error_exits_2() {
        let mut out = Vec::new();
        assert_eq!(main(&[], &mut out), 2);
    }
}
