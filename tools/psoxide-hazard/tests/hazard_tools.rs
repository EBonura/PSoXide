//! Fixtures for `hazard-scan` and `hazard-patch`.
//!
//! Each case assembles a tiny PS-EXE around one load-delay shape, runs it on
//! a small R3000 interpreter that delivers a load one instruction late, and
//! checks three things: the scanner and `hazard-patch --check` both report
//! it, the unpatched program reads the stale register, and after patching
//! the rescan is clean and the program reads the loaded value. The last check
//! matters most: the scanner and patcher share one detector and so its blind
//! spots, and "0 hazards" alone would not have caught either past gap
//! (branch operands, 2026-09-04; `jr ra` returns, 2026-09-22). Every scan
//! also checks that the scanner and `hazard-patch --check` name the same
//! sites, so a filter added to one CLI cannot make them drift apart again.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use psoxide_hazard::detect::{looks_like_code, IsCode, TABLE_CEILING};
use psoxide_hazard::{patch, scan};

struct Fixture {
    _dir: TempDir,
    path: PathBuf,
}

/// `(branch, consumer)` from a scanner line: `B: op | slot .. | C: op`, with
/// no consumer address for a register jump that leaves the image.
fn scanner_site(line: &str) -> (String, Option<String>) {
    let branch = line[..8].to_string();
    let last = line.rsplit(" | ").next().unwrap();
    let consumer = (last.len() > 9
        && last.as_bytes()[8] == b':'
        && last[..8].bytes().all(|b| b.is_ascii_hexdigit()))
    .then(|| last[..8].to_string());
    (branch, consumer)
}

/// `(branch, consumer)` from `hazard-patch --check`: `hazard B: op | slot ..
/// | consumer C`.
fn patcher_site(line: &str) -> (String, Option<String>) {
    let branch = line["hazard ".len().."hazard ".len() + 8].to_string();
    let after = line.split(" | consumer ").nth(1).unwrap();
    let consumer = (after.len() >= 8
        && after[..8].bytes().all(|b| b.is_ascii_hexdigit())
        && after
            .as_bytes()
            .get(8)
            .is_none_or(|b| !b.is_ascii_alphanumeric()))
    .then(|| after[..8].to_string());
    (branch, consumer)
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new("hazard");
        let path = dir.0.join("fixture.exe");
        Self { _dir: dir, path }
    }

    fn p(&self) -> &str {
        self.path.to_str().unwrap()
    }

    fn scan_with(&self, is_code: IsCode<'_>, ceiling: usize) -> (Vec<String>, String) {
        let mut out = Vec::new();
        let hazards = match scan::scan_with_ceiling(&self.path, None, is_code, ceiling, &mut out) {
            Ok(hazards) => hazards,
            Err(_) => panic!("scan failed: {}", String::from_utf8_lossy(&out)),
        };
        (hazards, String::from_utf8(out).unwrap())
    }

    /// The scanner's report, after checking that `hazard-patch --check`
    /// names the same (branch, consumer) sites on this image.
    fn scan(&self) -> Vec<String> {
        let (hazards, _) = self.scan_with(&looks_like_code, TABLE_CEILING);
        let mut scanned: Vec<_> = hazards.iter().map(|l| scanner_site(l)).collect();
        scanned.sort();
        let (status, stdout) = self.patch(&["--check"]);
        assert_eq!(status, if hazards.is_empty() { 0 } else { 1 }, "{stdout}");
        let mut checked: Vec<_> = stdout
            .lines()
            .filter(|l| l.starts_with("hazard "))
            .map(patcher_site)
            .collect();
        checked.sort();
        assert_eq!(scanned, checked, "{stdout}");
        hazards
    }

    fn scan_output(&self) -> String {
        self.scan_with(&looks_like_code, TABLE_CEILING).1
    }

    /// These fixtures have no link map, so patching needs `--whole-image`
    /// (see `patching_needs_text_bounds`); `--check` does not.
    fn patch(&self, extra: &[&str]) -> (i32, String) {
        let mut args = vec![self.p()];
        args.extend_from_slice(extra);
        if !extra.contains(&"--check") {
            args.push("--whole-image");
        }
        call(patch::main, &args)
    }

    /// The shape is reported by both tools, reads the stale register
    /// unpatched, and reads VALUE once patched with nothing left over.
    fn assert_fixed(&self, image: &Image, r: &str, hazards: usize) {
        image.write(&self.path);
        assert_eq!(self.scan().len(), hazards, "{:?}", self.scan());
        assert_eq!(self.patch(&["--check"]).0, 1);
        assert_ne!(
            run(&self.path)[reg(r) as usize],
            VALUE,
            "fixture does not expose the hazard"
        );
        let (status, stdout) = self.patch(&[]);
        assert_eq!(status, 0, "{stdout}");
        assert_eq!(self.scan(), Vec::<String>::new());
        assert_eq!(run(&self.path)[reg(r) as usize], VALUE, "{stdout}");
    }
}

/// main: jal callee ; nop ; addu s0, reg, zero ; break
fn caller(image: &mut Image, callee_offset: u32, r: &str) {
    let callee = image.addr(callee_offset);
    image.put(0, &[jal(callee), NOP, addu("s0", r, "zero"), BREAK]);
}

/// main: a0 = index ; jal f (at 0x100) with f's body `words`. The index
/// arrives in a0, which f's own block cannot see.
fn switch(image: &mut Image, index: i64, words: &[u32]) {
    let f = image.addr(0x100);
    image.put(0, &[addiu("a0", "zero", index), jal(f), NOP, BREAK]);
    image.put(0x100, words);
}

fn hex(addr: u32) -> String {
    format!("{addr:08x}")
}

#[test]
fn return_value_loaded_in_jr_ra_slot() {
    // The hl-psx/cs-psx settings getter: the caller's first instruction
    // reads v0 before the load lands.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    caller(&mut image, 0x100, "v0");
    let data = image.addr(Image::DATA);
    image.put(
        0x100,
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    fx.assert_fixed(&image, "s0", 1);
}

#[test]
fn jr_ra_at_a_non_default_load_address() {
    // The demo disc's chain loader is linked at 0x801F0000.
    let fx = Fixture::new();
    let mut image = Image::new(0x801F_0000);
    caller(&mut image, 0x100, "v0");
    let data = image.addr(Image::DATA);
    image.put(
        0x100,
        &[lui("at", hi(data)), jr("ra"), lw("v0", lo(data), "at")],
    );
    fx.assert_fixed(&image, "s0", 1);
}

#[test]
fn callee_reads_argument_as_branch_operand() {
    // 2026-09-04: a callee whose first instruction is `beq a2, t1, ...`
    // reads a2 as a branch's first operand. It returns 2 when a2 holds
    // VALUE and 1 otherwise.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    let f = image.addr(0x100);
    image.put(
        0,
        &[
            lui("t0", hi(data)),
            addiu("t1", "zero", VALUE.into()),
            jal(f),
            lw("a2", lo(data), "t0"),
            addu("s0", "v0", "zero"),
            BREAK,
        ],
    );
    image.put(
        0x100,
        &[
            beq("a2", "t1", 3),
            NOP,
            jr("ra"),
            addiu("v0", "zero", 1),
            jr("ra"),
            addiu("v0", "zero", 2),
        ],
    );
    image.write(&fx.path);
    assert_eq!(fx.scan().len(), 1);
    assert_eq!(run(&fx.path)[reg("s0") as usize], 1);
    assert_eq!(fx.patch(&[]).0, 0);
    assert_eq!(fx.scan(), Vec::<String>::new());
    assert_eq!(run(&fx.path)[reg("s0") as usize], 2);
}

#[test]
fn conditional_fall_through_consumer() {
    // bne not taken: the fall-through reads the slot load at once.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    image.put(
        0,
        &[
            lui("t0", hi(data)),
            bne("zero", "zero", 8),
            lw("a1", lo(data), "t0"),
            addu("s0", "a1", "zero"),
            BREAK,
        ],
    );
    fx.assert_fixed(&image, "s0", 1);
}

#[test]
fn jump_table_target_consumer() {
    // A switch: `lui v1,%hi(T) ; addu v1,v1,a0 ; lw at,%lo(T)(v1) ; jr at ;
    // lw a1, X` with the table entry's target reading a1 first. The patcher
    // points that entry at a trampoline.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let (data, table) = (image.addr(Image::DATA), image.addr(Image::DATA + 0x40));
    let case = image.addr(0x180);
    image.put(Image::DATA + 0x40, &[case]);
    switch(
        &mut image,
        0,
        &[
            lui("v1", hi(table)),
            addu("v1", "v1", "a0"),
            lw("at", lo(table), "v1"),
            lui("t0", hi(data)),
            jr("at"),
            lw("a1", lo(data), "t0"),
        ],
    );
    image.put(0x180, &[addu("s0", "a1", "zero"), BREAK]);
    fx.assert_fixed(&image, "s0", 1);
    image.write(&fx.path);
    assert!(fx
        .patch(&[])
        .1
        .contains(&format!("patched table entry {}", hex(table))));
    assert!(fx
        .scan_output()
        .contains("warning: no --map: 1 of 1 register jumps resolve"));
}

#[test]
fn the_table_base_is_the_jump_registers_own() {
    // The old resolver took the nearest `lui` of ANY register before the
    // table load: here `lui t1` for the real `lui t0`. Its page holds another
    // table at the same offset whose case does not read a1, so it reported
    // nothing while the real case read a stale a1.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    image.words.extend(std::iter::repeat_n(NOP, 0x10000 / 4)); // a second 64 KiB page
    let (data, table) = (image.addr(Image::DATA), image.addr(0x10000 + 0x840));
    let decoy = table - 0x10000;
    assert_eq!(hi(decoy), hi(table) - 1);
    let (c1, c2) = (image.addr(0x180), image.addr(0x200));
    image.put(0x10840, &[c1]);
    image.put(0x840, &[c2]);
    switch(
        &mut image,
        0,
        &[
            lui("t0", hi(table)),
            lui("t1", hi(decoy)),
            addu("t0", "t0", "a0"),
            lw("at", lo(table), "t0"),
            lui("t2", hi(data)),
            jr("at"),
            lw("a1", lo(data), "t2"),
        ],
    );
    image.put(0x180, &[addu("s0", "a1", "zero"), BREAK]);
    image.put(0x200, &[addiu("s1", "zero", 1), BREAK]);
    image.write(&fx.path);
    let sites: Vec<_> = fx.scan().iter().map(|l| scanner_site(l)).collect();
    assert_eq!(
        sites,
        [(hex(image.addr(0x114)), Some(hex(image.addr(0x180))))]
    );
    fx.assert_fixed(&image, "s0", 1);
}

#[test]
fn a_table_longer_than_64_entries() {
    // Quake's TargetGraph::apply_command has 74 entries; the old resolver
    // stopped reading at 64 and never saw case 70.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let (data, table) = (image.addr(Image::DATA), image.addr(Image::DATA + 0x100));
    let mut entries = vec![image.addr(0x200); 74];
    entries[70] = image.addr(0x180);
    image.put(Image::DATA + 0x100, &entries);
    switch(
        &mut image,
        70 * 4,
        &[
            lui("v1", hi(table)),
            addu("v1", "v1", "a0"),
            lw("at", lo(table), "v1"),
            lui("t0", hi(data)),
            jr("at"),
            lw("a1", lo(data), "t0"),
        ],
    );
    image.put(0x180, &[addu("s0", "a1", "zero"), BREAK]);
    image.put(0x200, &[addiu("s1", "zero", 1), BREAK]);
    image.write(&fx.path);
    assert!(fx
        .patch(&["--check"])
        .1
        .contains(&format!("via table entry {}", hex(table + 70 * 4))));
    fx.assert_fixed(&image, "s0", 1);
    // A table that reaches the ceiling stops there and says so.
    assert!(fx
        .scan_with(&looks_like_code, 4)
        .1
        .contains("still names code after 4 entries"));
}

#[test]
fn a_base_from_outside_the_block_is_reported() {
    // The table base s1 is set before the label L that a branch elsewhere
    // reaches, so the dispatch's block cannot show it. The old resolver took
    // `lui t0` (the slot load's page) instead, found a "table" at its page
    // plus the load offset, and reported nothing. Now the site is
    // unresolved: reported, and patched by moving the load out of the slot.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let (data, table) = (image.addr(Image::DATA), image.addr(Image::DATA + 0x40));
    let (c1, c2) = (image.addr(0x180), image.addr(0x200));
    image.put(Image::DATA + 0x40, &[c1]);
    image.put(0x40, &[c2]); // what `lui t0` + 0x40 names
    assert_eq!((hi(data) as u32) << 16 | 0x40, image.addr(0x40));
    let body = [
        lui("s1", hi(table - 0x40)),
        addiu("s1", "s1", lo(table - 0x40)),
        lui("t0", hi(data)),
        addu("t1", "s1", "a0"),
        lw("at", 0x40, "t1"),
        NOP, // L at 0x108
        jr("at"),
        lw("a1", lo(data), "t0"),
    ];
    switch(&mut image, 0, &body);
    image.put(0x180, &[addu("s0", "a1", "zero"), BREAK]);
    image.put(0x200, &[addiu("s1", "zero", 1), BREAK]);
    image.put(0x300, &[beq("zero", "zero", (0x108 - 0x304) / 4), NOP]);
    image.write(&fx.path);
    let tails: Vec<_> = fx
        .scan()
        .iter()
        .map(|l| l.rsplit(" | ").next().unwrap().to_string())
        .collect();
    assert_eq!(tails, ["jump table not resolved, target unknown"]);
    assert!(fx
        .scan_output()
        .contains("warning: no --map: 0 of 1 register jumps resolve"));
    fx.assert_fixed(&image, "s0", 1);
    // Without the branch to L the block reaches s1's lui and proves it.
    image.put(0x300, &[NOP]);
    image.write(&fx.path);
    let sites: Vec<_> = fx.scan().iter().map(|l| scanner_site(l)).collect();
    assert_eq!(
        sites,
        [(hex(image.addr(0x118)), Some(hex(image.addr(0x180))))]
    );
}

#[test]
fn register_jump_with_unresolved_target() {
    // `jr t9` built from lui/ori, no table load to resolve: the target is
    // unknown, so the slot load must be moved out of the slot.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let (data, target) = (image.addr(Image::DATA), image.addr(0x100));
    image.put(
        0,
        &[
            lui("t9", (target >> 16).into()),
            ori("t9", "t9", (target & 0xFFFF).into()),
            lui("t0", hi(data)),
            jr("t9"),
            lw("a0", lo(data), "t0"),
        ],
    );
    image.put(0x100, &[addu("s0", "a0", "zero"), BREAK]);
    fx.assert_fixed(&image, "s0", 1);
}

#[test]
fn register_call_reads_argument_first() {
    // jalr: the callee (a leaf reading a0 at once) is unknown to the tools.
    // The patched call must still return to the original site.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let (data, callee) = (image.addr(Image::DATA), image.addr(0x100));
    image.put(
        0,
        &[
            lui("t9", (callee >> 16).into()),
            ori("t9", "t9", (callee & 0xFFFF).into()),
            lui("t0", hi(data)),
            jalr("t9"),
            lw("a0", lo(data), "t0"),
            addiu("s1", "zero", 7),
            BREAK,
        ],
    );
    image.put(0x100, &[addu("s0", "a0", "zero"), jr("ra"), NOP]);
    fx.assert_fixed(&image, "s0", 1);
    assert_eq!(run(&fx.path)[reg("s1") as usize], 7);
}

#[test]
fn register_call_slot_load_reading_the_jump_register() {
    // jalr reads t9 and writes only ra, so a slot load based on t9 reads the
    // same address whether it runs in the slot or ahead of the jalr.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let callee = image.addr(0x100);
    image.put(
        0,
        &[
            lui("t9", (callee >> 16).into()),
            ori("t9", "t9", (callee & 0xFFFF).into()),
            jalr("t9"),
            lw("a0", i64::from(Image::DATA) - 0x100, "t9"),
            addiu("s1", "zero", 7),
            BREAK,
        ],
    );
    image.put(0x100, &[addu("s0", "a0", "zero"), jr("ra"), NOP]);
    fx.assert_fixed(&image, "s0", 1);
    assert_eq!(run(&fx.path)[reg("s1") as usize], 7);
}

#[test]
fn register_call_slot_load_reading_the_link_register_is_refused() {
    // jalr writes ra before its delay slot runs, so `lw a0, X(ra)` there
    // reads through the NEW return address. Hoisted into a trampoline ahead
    // of the jalr it would read through the old ra instead.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let (callee, old_ra) = (image.addr(0x100), image.addr(0x40));
    let offset = i64::from(Image::DATA) - 0x18; // the jalr is at 0x10, so the new ra is +0x18
    image.put(
        0,
        &[
            lui("t9", (callee >> 16).into()),
            ori("t9", "t9", (callee & 0xFFFF).into()),
            lui("ra", (old_ra >> 16).into()),
            ori("ra", "ra", (old_ra & 0xFFFF).into()),
            jalr("t9"),
            lw("a0", offset, "ra"),
            addu("s1", "a0", "zero"),
            BREAK,
        ],
    );
    image.put(0x100, &[addu("s0", "a0", "zero"), jr("ra"), NOP]);
    image.put((0x40 + offset) as u32, &[0x0BAD]); // what the old ra would lead to
    image.write(&fx.path);
    assert_eq!(fx.scan().len(), 1);
    let regs = run(&fx.path);
    assert_ne!(
        regs[reg("s0") as usize],
        VALUE,
        "fixture does not expose the hazard"
    );
    assert_eq!(regs[reg("s1") as usize], VALUE);
    let before = std::fs::read(&fx.path).unwrap();
    let (status, stdout) = fx.patch(&[]);
    assert_eq!(status, 1, "{stdout}");
    assert!(stdout.contains("reads the link register (ra)"));
    assert_eq!(std::fs::read(&fx.path).unwrap(), before);
    // Why: the trampoline the patcher would otherwise build fetches the
    // other word, because the load now runs before the jalr links.
    let tramp = image.addr(Image::TRAMPOLINES + 8);
    let back = image.addr(0x18);
    image.put(0x10, &[j(tramp), NOP]);
    image.put(
        Image::TRAMPOLINES + 8,
        &[lw("a0", offset, "ra"), jalr("t9"), NOP, j(back), NOP],
    );
    image.write(&fx.path);
    assert_eq!(run(&fx.path)[reg("s1") as usize], 0x0BAD);
}

#[test]
fn clean_shapes_are_not_reported() {
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    caller(&mut image, 0x100, "v0");
    // Epilogue arithmetic in the slot, a slot load into ra, and a load whose
    // value has an instruction to land in before the return.
    image.put(0x100, &[jr("ra"), addiu("sp", "sp", 8)]);
    image.put(
        0x200,
        &[lui("at", hi(data)), jr("ra"), lw("ra", lo(data), "at")],
    );
    image.put(
        0x300,
        &[
            lui("at", hi(data)),
            lw("v0", lo(data), "at"),
            NOP,
            jr("ra"),
            NOP,
        ],
    );
    image.write(&fx.path);
    assert_eq!(fx.scan(), Vec::<String>::new());
    assert_eq!(fx.patch(&["--check"]).0, 0);
}

#[test]
fn gte_command_in_a_delay_slot_is_a_warning() {
    // psx-rt's handler cannot step over a GTE command in a delay slot (EPC
    // names the branch), so the scanner points at it, without failing the
    // image, and ignores GTE commands outside delay slots.
    let rtps = 0x4A18_0001;
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    image.put(0, &[beq("zero", "zero", 2), rtps, NOP, rtps, NOP, BREAK]);
    image.write(&fx.path);
    let (hazards, out) = fx.scan_with(&looks_like_code, TABLE_CEILING);
    assert!(hazards.is_empty());
    assert!(out.contains("warning: 1 GTE commands in branch delay slots"));
    assert!(out.contains(&hex(image.addr(0))));
    image.put(4, &[NOP]);
    image.write(&fx.path);
    assert!(!fx.scan_output().contains("GTE"));
}

#[test]
fn scanner_and_patcher_name_the_same_sites() {
    // Several shapes in one image, including a conditional whose load is read
    // on both paths (two sites at one branch). scan() fails if the two CLIs
    // disagree on any of them.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    image.put(
        0x100,
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    image.put(
        0x200,
        &[
            lui("t0", hi(data)),
            beq("zero", "zero", 2),
            lw("a1", lo(data), "t0"),
            addu("s0", "a1", "zero"),
            addu("s1", "a1", "zero"),
            jr("ra"),
            NOP,
        ],
    );
    image.put(
        0x300,
        &[
            lui("t0", hi(data)),
            jalr("t9"),
            lw("a0", lo(data), "t0"),
            jr("ra"),
            NOP,
        ],
    );
    image.write(&fx.path);
    let mut sites: Vec<_> = fx.scan().iter().map(|l| scanner_site(l)).collect();
    sites.sort();
    let mut want = vec![
        (hex(image.addr(0x104)), None),
        (hex(image.addr(0x204)), Some(hex(image.addr(0x20C)))),
        (hex(image.addr(0x204)), Some(hex(image.addr(0x210)))),
        (hex(image.addr(0x304)), None),
    ];
    want.sort();
    assert_eq!(sites, want);
}

#[test]
fn callers_can_replace_the_data_guard() {
    // Games replace the data guard so proven .text is never skipped as data
    // (alttp-psx, hk-psx). The replacement has to reach the shared detector.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    caller(&mut image, 0x100, "v0");
    image.put(
        0x100,
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    image.put(0x120, &[0xFFFF_FFFF]); // decodes as `.word`, inside the 16-word guard
    image.write(&fx.path);
    assert_eq!(fx.scan(), Vec::<String>::new());
    assert_eq!(fx.scan_with(&|_, _| true, TABLE_CEILING).0.len(), 1);
    let args = vec![fx.p().to_string()];
    let mut out = Vec::new();
    assert_eq!(scan::main_with(&args, &|_, _| true, &mut out), 1);
}

#[test]
fn second_pass_keeps_earlier_trampolines() {
    // Every trampoline contains nops, so a second pass must not take the
    // first zero word after the magic for free space.
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    caller(&mut image, 0x100, "v0");
    image.put(
        0x100,
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    image.put(
        0x200,
        &[lui("at", hi(data)), jr("ra"), lbu("v1", lo(data), "at")],
    );
    image.write(&fx.path);
    // The diagnostic variable is process-wide, so this pass runs the binary.
    let first = Command::new(env!("CARGO_BIN_EXE_hazard-patch"))
        .arg(&fx.path)
        .arg("--whole-image")
        .env("HAZARD_PATCH_ONLY", hex(image.addr(0x104)))
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    let before = std::fs::read(&fx.path).unwrap();
    assert_eq!(fx.patch(&[]).0, 0);
    let after = std::fs::read(&fx.path).unwrap();
    let start = (0x800 + Image::TRAMPOLINES + 8) as usize;
    assert_eq!(after[start..start + 12], before[start..start + 12]);
    assert_eq!(fx.scan(), Vec::<String>::new());
    assert_eq!(run(&fx.path)[reg("s0") as usize], VALUE);
}

#[test]
fn the_binaries_take_the_same_command_lines() {
    let fx = Fixture::new();
    let mut image = Image::new(0x8001_0000);
    let data = image.addr(Image::DATA);
    caller(&mut image, 0x100, "v0");
    image.put(
        0x100,
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    image.write(&fx.path);
    let tool = |name: &str, args: &[&str]| {
        let out = Command::new(name).args(args).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
        )
    };
    let scan_bin = env!("CARGO_BIN_EXE_hazard-scan");
    let patch_bin = env!("CARGO_BIN_EXE_hazard-patch");
    let (status, out) = tool(scan_bin, &[fx.p()]);
    assert_eq!(status, 1);
    assert!(
        out.ends_with(&format!("1 hazards in {}\n", fx.p())),
        "{out}"
    );
    assert_eq!(tool(patch_bin, &[fx.p(), "--check"]).0, 1);
    assert_eq!(tool(patch_bin, &[]).0, 2);
    assert_eq!(tool(scan_bin, &[]).0, 2);
    assert_eq!(tool(patch_bin, &[fx.p()]).0, 2, "no map, no --whole-image");
    assert_eq!(tool(patch_bin, &[fx.p(), "--whole-image"]).0, 0);
    assert_eq!(
        tool(scan_bin, &[fx.p()]),
        (0, format!("0 hazards in {}\n", fx.p()))
    );
    let _ = Path::new(fx.p());
}
