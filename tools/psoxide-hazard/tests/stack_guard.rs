//! Fixtures for `stack-guard`.
//!
//! Each case assembles a tiny PS-EXE plus the ld.lld map that would describe
//! it, with a psx-rt scratchpad stack entry at the root of a small call tree,
//! and checks the depth the guard computes or the reason it refuses.

mod common;

use std::path::PathBuf;

use common::*;
use psoxide_hazard::stack_guard::{self, GuardImage};
use psoxide_hazard::{patch, scan};

const BASE: u32 = 0x8001_0000;

fn entry(start: u32, end: u32) -> String {
    format!("<psx_rt::scratchpad::ScratchpadStack<{start}, {end}>>::stack_entry::<u32, t::f>")
}

fn prologue(frame: i64) -> Vec<u32> {
    if frame != 0 {
        vec![addiu("sp", "sp", -frame)]
    } else {
        vec![]
    }
}

fn epilogue(frame: i64) -> Vec<u32> {
    vec![
        jr("ra"),
        if frame != 0 {
            addiu("sp", "sp", frame)
        } else {
            NOP
        },
    ]
}

/// `sll ; addu ; lw ; nop ; jr ; nop`: a switch on `index` through the table
/// at `table`, whose %hi is already in `base`.
fn dispatch(index: &str, base: &str, table: u32) -> Vec<u32> {
    let rd = "at";
    vec![
        sll(rd, index, 2),
        addu(rd, rd, base),
        lw(rd, lo(table), rd),
        NOP,
        jr(rd),
        NOP,
    ]
}

enum Section {
    Named(&'static str),
    Owner(u32),
}

/// Functions laid out every 0x40 bytes from BASE, then a HAZARD_TRAMPOLINES
/// array at +0xC00; writes the exe and a matching map.
struct Fixture {
    image: Image,
    functions: Vec<(u32, u32, String)>,
    sections: Vec<(u32, u32, Section)>,
    trampoline_words: u32,
    dir: TempDir,
}

const SLOT: u32 = 0x40;

impl Fixture {
    fn new() -> Self {
        Self {
            image: Image::new(BASE),
            functions: vec![],
            sections: vec![],
            trampoline_words: 0,
            dir: TempDir::new("guard"),
        }
    }

    fn addr(&self, index: u32) -> u32 {
        BASE + index * SLOT
    }

    fn function(&mut self, index: u32, name: &str, words: &[u32]) -> u32 {
        self.function_slots(index, name, words, 1)
    }

    fn function_slots(&mut self, index: u32, name: &str, words: &[u32], slots: u32) -> u32 {
        assert!(words.len() as u32 * 4 <= SLOT * slots, "{name}");
        self.image.put(index * SLOT, words);
        self.functions
            .push((self.addr(index), words.len() as u32 * 4, name.to_string()));
        self.addr(index)
    }

    /// Words at `offset` past .text (0x800..0xC00), such as a jump table, as
    /// one input section of the map.
    fn data(&mut self, offset: u32, words: &[u32], section: Section) -> u32 {
        assert!(0x800 < offset && offset + 4 * words.len() as u32 <= Image::TRAMPOLINES);
        self.image.put(offset, words);
        self.sections
            .push((BASE + offset, 4 * words.len() as u32, section));
        BASE + offset
    }

    fn trampoline(&mut self, words: &[u32]) -> u32 {
        let offset = Image::TRAMPOLINES + 8 + self.trampoline_words * 4;
        self.image.put(offset, words);
        self.trampoline_words += words.len() as u32;
        BASE + offset
    }

    /// `payload_skew` misstates the map's payload size; `layout_skew` moves
    /// every function and the trampoline array in the map but keeps the
    /// payload, like a stale map of a relink.
    fn write_skewed(&self, payload_skew: u32, layout_skew: u32) -> (PathBuf, PathBuf) {
        let exe = self.dir.0.join("fixture.exe");
        self.image.write(&exe);
        let payload = self.image.words.len() as u32 * 4;
        let text_end = BASE + 0x20 * SLOT;
        let mut lines = vec!["     VMA      LMA     Size Align Out     In      Symbol".to_string()];
        let mut row = |address: u32, size: u32, depth: usize, name: &str, align: u32| {
            lines.push(format!(
                "{address:8x} {address:8x} {size:8x} {align:5} {}{name}",
                " ".repeat(depth)
            ));
        };
        row(BASE, 0, 8, "__text_start = .", 1);
        let mut functions = self.functions.clone();
        functions.sort();
        let mut keys = std::collections::HashMap::new();
        for (i, (address, size, name)) in functions.iter().enumerate() {
            keys.insert(*address, format!("f{i}"));
            row(
                address + layout_skew,
                *size,
                8,
                &format!("/fixture.o:(.text.f{i})"),
                4,
            );
            row(address + layout_skew, *size, 16, name, 1);
        }
        row(text_end, 0, 8, "__text_end = .", 1);
        let mut sections: Vec<_> = self.sections.iter().enumerate().collect();
        sections.sort_by_key(|(_, s)| s.0);
        for (i, (address, size, section)) in sections {
            let name = match section {
                Section::Named(kind) => format!("{kind}.d{i}"),
                Section::Owner(owner) => format!(".rodata.{}", keys[owner]),
            };
            row(*address, *size, 8, &format!("/fixture.o:({name})"), 4);
        }
        let tramp = BASE + Image::TRAMPOLINES + layout_skew;
        row(
            tramp,
            8 + 64 * 4,
            8,
            "/fixture.o:(.data.HAZARD_TRAMPOLINES)",
            4,
        );
        row(tramp, 8 + 64 * 4, 16, "HAZARD_TRAMPOLINES", 1);
        row(BASE + payload + payload_skew, 0, 8, "__bss_start = .", 1);
        let map = self.dir.0.join("fixture.map");
        std::fs::write(&map, lines.join("\n") + "\n").unwrap();
        (exe, map)
    }

    fn write(&self) -> (PathBuf, PathBuf) {
        self.write_skewed(0, 0)
    }

    fn guard_skewed(
        &self,
        payload_skew: u32,
        layout_skew: u32,
        root: Option<(&str, i64)>,
    ) -> (usize, String) {
        let (exe, map) = self.write_skewed(payload_skew, layout_skew);
        let mut out = Vec::new();
        let failures = stack_guard::check(
            &exe,
            Some(&map),
            root.map(|r| r.0),
            root.map(|r| r.1),
            &mut out,
        );
        (failures, String::from_utf8(out).unwrap())
    }

    fn guard(&self) -> (usize, String) {
        self.guard_skewed(0, 0, None)
    }

    fn leaf(&mut self, index: u32, name: &str, frame: i64) -> u32 {
        let words = [prologue(frame), epilogue(frame)].concat();
        self.function(index, name, &words)
    }

    fn caller(&mut self, index: u32, name: &str, frame: i64, callees: &[u32]) -> u32 {
        let mut body = prologue(frame);
        for &callee in callees {
            body.extend([jal(callee), NOP]);
        }
        body.extend(epilogue(frame));
        self.function(index, name, &body)
    }

    /// A leaf that switches through `table` and returns from every case.
    fn switch(&mut self, index: u32, name: &str, frame: i64, table: u32) -> (u32, Vec<u32>) {
        let addr = self.addr(index);
        let mut body = prologue(frame);
        body.push(lui("t0", hi(table)));
        body.extend(dispatch("a0", "t0", table));
        let mut cases = vec![];
        for _ in 0..2 {
            cases.push(addr + 4 * body.len() as u32);
            body.extend(epilogue(frame));
        }
        self.function(index, name, &body);
        (addr, cases)
    }
}

#[test]
fn sums_frames_down_the_deepest_path() {
    let mut fx = Fixture::new();
    let b = fx.leaf(3, "t::b", 24);
    let c = fx.leaf(4, "t::c", 8);
    let a = fx.caller(2, "t::a", 40, &[b]);
    fx.caller(1, &entry(512, 1024), 16, &[a, c]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("80 of 492 bytes (region 512..1024"), "{out}");
    assert!(out.contains("t::a(40) > t::b(24)"), "{out}");
}

#[test]
fn a_tree_deeper_than_the_region_fails() {
    let mut fx = Fixture::new();
    let b = fx.leaf(3, "t::b", 24);
    let a = fx.caller(2, "t::a", 40, &[b]);
    fx.caller(1, &entry(960, 1024), 16, &[a]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("FAIL"));
    assert!(out.contains("80 of 44 bytes"), "{out}");
}

#[test]
fn recursion_is_refused() {
    let mut fx = Fixture::new();
    let a_addr = fx.addr(2);
    let b = fx.caller(3, "t::b", 8, &[a_addr]);
    fx.caller(2, "t::a", 8, &[b]);
    fx.caller(1, &entry(0, 1024), 8, &[a_addr]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("recurses"), "{out}");
}

#[test]
fn calls_through_a_register_are_refused() {
    let mut fx = Fixture::new();
    let body = [prologue(8), vec![jalr("t9"), NOP], epilogue(8)].concat();
    fx.function(2, "t::dyn_call", &body);
    let f = fx.addr(2);
    fx.caller(1, &entry(0, 1024), 8, &[f]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("calls through a register"), "{out}");
}

#[test]
fn bios_style_register_jumps_are_refused() {
    let mut fx = Fixture::new();
    let bios = fx.function(
        2,
        "__bios_putchar",
        &[
            addiu("t0", "zero", 0xA0),
            jr("t0"),
            addiu("t1", "zero", 0x3C),
        ],
    );
    fx.caller(1, &entry(0, 1024), 8, &[bios]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("not a jump table"), "{out}");
}

#[test]
fn other_stack_pointer_writes_are_refused() {
    let mut fx = Fixture::new();
    let body = [vec![addu("sp", "sp", "t0")], epilogue(0)].concat();
    fx.function(2, "t::alloca", &body);
    let f = fx.addr(2);
    fx.caller(1, &entry(0, 1024), 8, &[f]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("sets $sp"), "{out}");
}

#[test]
fn hazard_trampolines_are_followed() {
    let mut fx = Fixture::new();
    let b = fx.leaf(3, "t::b", 200);
    let tramp = fx.trampoline(&[NOP, j(b), NOP]);
    // `jal TRAMP` as the patcher leaves a patched call.
    let body = [prologue(16), vec![jal(tramp), NOP], epilogue(16)].concat();
    fx.function(2, "t::a", &body);
    let a = fx.addr(2);
    fx.caller(1, &entry(0, 1024), 8, &[a]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("224 of 1004 bytes"), "{out}");
}

#[test]
fn conditional_trampolines_count_both_exits() {
    let mut fx = Fixture::new();
    let b = fx.leaf(3, "t::b", 64);
    let a_addr = fx.addr(2);
    // bXX +3 ; nop ; j FALL ; nop ; j T ; nop, with FALL inside t::a.
    let tramp = fx.trampoline(&[beq("a0", "zero", 3), NOP, j(a_addr + 8), NOP, j(b), NOP]);
    let body = [prologue(16), vec![j(tramp), NOP], epilogue(16)].concat();
    fx.function(2, "t::a", &body);
    fx.caller(1, &entry(0, 1024), 8, &[a_addr]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("88 of 1004 bytes"), "{out}");
}

#[test]
fn switch_and_panic_handler_count_only_their_own_frames() {
    let mut fx = Fixture::new();
    let deep = fx.leaf(5, "t::deep_report", 900);
    // The real switch contains a jalr and moves $sp; neither may fail it.
    let switch = fx.function(
        3,
        "__psx_rt_call_on_stack",
        &[
            addiu("sp", "sp", -24),
            or("s0", "sp", "zero"),
            jalr("t9"),
            or("sp", "a2", "zero"),
            or("sp", "s0", "zero"),
            jr("ra"),
            addiu("sp", "sp", 24),
        ],
    );
    let handler = fx.caller(4, "__rustc::rust_begin_unwind", 32, &[deep]);
    fx.caller(1, &entry(0, 1024), 8, &[switch, handler]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("40 of 1004 bytes"), "{out}");
}

#[test]
fn a_map_from_another_link_is_refused() {
    let mut fx = Fixture::new();
    fx.caller(1, &entry(0, 1024), 8, &[]);
    let (failures, out) = fx.guard_skewed(0x800, 0, None);
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("map does not match this image"), "{out}");
}

#[test]
fn a_stale_map_with_the_same_payload_is_refused() {
    // A relink moved every function and the trampoline array by 0x20 bytes
    // but kept the 2 KiB aligned payload, so the old size check passed; a
    // stale Quake map cut 11 jump tables short this way.
    let mut fx = Fixture::new();
    let f1 = fx.addr(1);
    fx.function(0, "_start", &[jal(f1), NOP, j(BASE), NOP]);
    let leaf = fx.leaf(2, "t::leaf", 8);
    fx.caller(1, &entry(0, 1024), 8, &[leaf]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    let (failures, out) = fx.guard_skewed(0, 0x20, None);
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("map does not match this image"), "{out}");
    assert!(
        out.contains(&format!(
            "entry point {BASE:08x}, map's _start {:08x}",
            BASE + 0x20
        )),
        "{out}"
    );
    assert!(out.contains(&format!(
        "no HAZARD_TRAMPOLINES magic at the map's {:08x}",
        BASE + Image::TRAMPOLINES + 0x20
    )));
    assert!(out.contains(&format!(
        "2 calls to no function the map names, first jal {:08x} at {BASE:08x}",
        BASE + 0x40
    )));
    let (exe, map) = fx.write_skewed(0, 0x20);
    let (exe, map) = (exe.to_str().unwrap(), map.to_str().unwrap());
    for (main, args) in [
        (
            patch::main as fn(&[String], &mut dyn std::io::Write) -> i32,
            vec![exe, "--check", "--map", map],
        ),
        (patch::main, vec![exe, "--map", map]),
        (scan::main, vec![exe, "--map", map]),
    ] {
        let (status, out) = call(main, &args);
        assert_eq!(status, 1, "{out}");
        assert!(out.contains("map does not match this image"), "{out}");
    }
}

#[test]
fn a_functions_own_rodata_must_hold_its_jump_table() {
    // LLVM writes a function's jump tables to `.rodata.<its section>`, so
    // every word there lands in that function, or in a trampoline once
    // patched. A map that puts the section over other words (here one naming
    // another function) is from another link.
    let mut fx = Fixture::new();
    let (a, cases) = fx.switch(2, "t::a", 16, BASE + 0x900);
    let other = fx.leaf(4, "t::other", 8);
    fx.caller(1, &entry(0, 1024), 8, &[a, other]);
    let tramp = fx.trampoline(&[NOP, j(cases[1]), NOP]);
    fx.data(0x900, &[cases[0], tramp], Section::Owner(a));
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    fx.sections.clear();
    fx.data(0x900, &[cases[0], other], Section::Owner(a));
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains(&format!(
        "1 jump table words outside their function, first {:08x} holds {other:08x}, not in the function at {a:08x}",
        BASE + 0x904
    )), "{out}");
}

#[test]
fn custom_roots_take_a_budget() {
    let mut fx = Fixture::new();
    let b = fx.leaf(3, "t::b", 100);
    fx.caller(1, "game::projection_entry", 16, &[b]);
    let (failures, out) = fx.guard_skewed(0, 0, Some(("^game::projection_entry$", 100)));
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("116 of 100 bytes"), "{out}");
}

#[test]
fn a_table_stops_at_the_next_functions_table() {
    // The tables sit back to back, as in .rodata. Read on, t::a's table
    // names t::b's cases, and t::b's 600-byte frame lands in t::a's tree
    // (hk-psx: presentation::service's table ran into menu::run's). The
    // second word of t::b's table is a trampoline into t::b, as an earlier
    // patch leaves it: still not t::a's.
    let mut fx = Fixture::new();
    let (table_a, table_b) = (BASE + 0x900, BASE + 0x908);
    let (a, cases_a) = fx.switch(2, "t::a", 16, table_a);
    let (_, cases_b) = fx.switch(4, "t::b", 600, table_b);
    let tramp = fx.trampoline(&[NOP, j(cases_b[1]), NOP]);
    fx.data(
        0x900,
        &[cases_a[0], cases_a[1], tramp, cases_b[1]],
        Section::Named(".rodata"),
    );
    fx.caller(1, &entry(0, 1024), 8, &[a]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("24 of 1004 bytes"), "{out}");
    assert!(!out.contains("t::b"), "{out}");
    let (exe, map) = fx.write();
    let image = GuardImage::open(&exe, &map).unwrap();
    let entries = stack_guard::jump_table(&image, i64::from(a + 24));
    let want = vec![
        (i64::from(table_a), i64::from(cases_a[0])),
        (i64::from(table_a + 4), i64::from(cases_a[1])),
    ];
    assert_eq!(entries, Some(want));
}

/// t::far loads its table's %hi at the top, calls a leaf, branches, and
/// dispatches more than the old 11-instruction window later.
fn far_base(base_reg: &str) -> (usize, String) {
    let mut fx = Fixture::new();
    let leaf = fx.leaf(6, "t::leaf", 40);
    let far = fx.addr(2);
    let table = BASE + 0x900;
    let filler = 12;
    let mut body = prologue(24);
    body.extend([
        lui(base_reg, hi(table)),
        sw("ra", 20, "sp"),
        jal(leaf),
        NOP,
        beq("a1", "zero", filler + 1),
        NOP,
    ]);
    body.extend(std::iter::repeat_n(addiu("v0", "v0", 1), filler as usize));
    body.extend(dispatch("a0", base_reg, table));
    let mut cases = vec![];
    for _ in 0..2 {
        cases.push(far + 4 * body.len() as u32);
        body.extend([lw("ra", 20, "sp"), jr("ra"), addiu("sp", "sp", 24)]);
    }
    fx.function_slots(2, "t::far", &body, 3);
    fx.data(0x900, &cases, Section::Named(".rodata"));
    fx.caller(1, &entry(0, 1024), 8, &[far]);
    fx.guard()
}

#[test]
fn a_table_base_loaded_far_away_is_followed_back() {
    // State::apply keeps its table's %hi in s8 for the whole loop.
    let (failures, out) = far_base("s0");
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("72 of 1004 bytes"), "{out}");
    assert!(out.contains("t::far(24) > t::leaf(40)"), "{out}");
}

#[test]
fn a_caller_saved_base_across_a_call_stays_unresolved() {
    // t0 does not survive the call, so no write of it reaches the use.
    let (failures, out) = far_base("t0");
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("not a jump table it can prove"), "{out}");
}

#[test]
fn a_base_from_the_caller_stays_unresolved() {
    let mut fx = Fixture::new();
    let table = BASE + 0x900;
    let addr = fx.addr(2);
    let body = [dispatch("a0", "a1", table), epilogue(0), epilogue(0)].concat();
    fx.function(2, "t::from_arg", &body);
    fx.data(0x900, &[addr + 24, addr + 32], Section::Named(".rodata"));
    fx.caller(1, &entry(0, 1024), 8, &[addr]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("not a jump table it can prove"), "{out}");
}

#[test]
fn a_table_of_function_pointers_is_not_a_switch() {
    // A tail call through a constant table of functions leaves t::tail.
    let mut fx = Fixture::new();
    let other = fx.leaf(4, "t::other", 200);
    let table = BASE + 0x900;
    let body = [vec![lui("t0", hi(table))], dispatch("a0", "t0", table)].concat();
    let tail = fx.function(2, "t::tail", &body);
    fx.data(0x900, &[other, other], Section::Named(".rodata"));
    fx.caller(1, &entry(0, 1024), 8, &[tail]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("not a jump table it can prove"), "{out}");
}

/// t::a dispatches through `lw` from BASE + 0x900 with `lw v0` in the jr's
/// slot; its second case reads v0 at once. `sections` lays the words from
/// 0x900 out as (section, case indexes); returns the fixture, the jr
/// address and the cases.
fn code_pointer_dispatch(sections: &[(&'static str, &[usize])]) -> (Fixture, u32, Vec<u32>) {
    let mut fx = Fixture::new();
    let a = fx.addr(2);
    let table = BASE + 0x900;
    let mut body = [vec![lui("t0", hi(table))], dispatch("a0", "t0", table)].concat();
    let last = body.len() - 1;
    body[last] = lw("v0", 0, "a1");
    let cases = vec![a + 4 * body.len() as u32, a + 4 * body.len() as u32 + 8];
    let jr_at = a + 4 * (body.len() as u32 - 2);
    let full = [
        body,
        epilogue(0),
        vec![addu("v1", "v0", "zero")],
        epilogue(0),
    ]
    .concat();
    fx.function(2, "t::a", &full);
    fx.caller(1, &entry(0, 1024), 8, &[a]);
    let mut offset = 0x900;
    for (section, indexes) in sections {
        let words: Vec<u32> = indexes.iter().map(|&i| cases[i]).collect();
        fx.data(offset, &words, Section::Named(section));
        offset += 4 * indexes.len() as u32;
    }
    (fx, jr_at, cases)
}

fn patch_with_map(fx: &Fixture, extra: &[&str]) -> String {
    let (exe, map) = fx.write();
    let mut args = vec![
        exe.to_str().unwrap().to_string(),
        "--map".into(),
        map.to_str().unwrap().to_string(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let mut out = Vec::new();
    patch::main(&args, &mut out);
    String::from_utf8(out).unwrap()
}

#[test]
fn a_mutable_array_of_code_pointers_is_not_a_switch() {
    // A `static mut` array in .data indexed by `lw` reads like a switch table
    // whose entries land inside t::a, but the image only holds its initial
    // words. In .rodata the second entry is proven and patched to a
    // trampoline; in .data the dispatch stays unresolved, so the slot load
    // moves out of the slot and the array is left as it was.
    let (fx, _, _) = code_pointer_dispatch(&[(".rodata", &[0, 1])]);
    assert!(patch_with_map(&fx, &["--check"])
        .contains(&format!("via table entry {:08x}", BASE + 0x904)));
    let (fx, jr_at, cases) = code_pointer_dispatch(&[(".data", &[0, 1])]);
    let (exe, map) = fx.write();
    let image = GuardImage::open(&exe, &map).unwrap();
    assert_eq!(stack_guard::jump_table(&image, i64::from(jr_at)), None);
    let out = patch_with_map(&fx, &["--check"]);
    assert!(out.contains(&format!(
        "hazard {jr_at:08x}: jr at | slot lw v0,0(a1) | consumer the jump target (table not resolved)"
    )), "{out}");
    assert!(!out.contains("via table entry"));
    // Patch the written image in place (patch_with_map would rewrite it).
    let args = vec![
        exe.to_str().unwrap().to_string(),
        "--map".into(),
        map.to_str().unwrap().to_string(),
    ];
    let mut out = Vec::new();
    patch::main(&args, &mut out);
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(&format!("patched jr at at {jr_at:08x}")),
        "{out}"
    );
    assert!(out.contains("0 remaining"), "{out}");
    let image = GuardImage::open(&exe, &map).unwrap();
    let words: Vec<i64> = (0..2)
        .map(|i| image.word_at(i64::from(BASE + 0x900 + 4 * i)))
        .collect();
    assert_eq!(
        words,
        cases.iter().map(|&c| i64::from(c)).collect::<Vec<_>>()
    );
    let (failures, out) = fx.guard();
    assert_eq!(failures, 1, "{out}");
    assert!(out.contains("not a jump table it can prove"), "{out}");
}

#[test]
fn a_table_ends_with_its_rodata_section() {
    // The words right after the table name t::a's case too, but they are a
    // .data array: the table stops at its section's end and the array word
    // is neither an entry nor patched.
    let (fx, jr_at, cases) = code_pointer_dispatch(&[(".rodata", &[0, 0]), (".data", &[1])]);
    let (exe, map) = fx.write();
    let image = GuardImage::open(&exe, &map).unwrap();
    let entries = stack_guard::jump_table(&image, i64::from(jr_at));
    let c0 = i64::from(cases[0]);
    assert_eq!(
        entries,
        Some(vec![
            (i64::from(BASE + 0x900), c0),
            (i64::from(BASE + 0x904), c0)
        ])
    );
    assert!(patch_with_map(&fx, &["--check"]).contains("0 hazards"));
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
}

#[test]
fn a_switch_inside_another_switchs_case() {
    // The second dispatch's block is only entered through the first table,
    // so it resolves once that table is proven (apply_arena).
    let mut fx = Fixture::new();
    let nested = fx.addr(2);
    let (table_1, table_2) = (BASE + 0x900, BASE + 0x908);
    let mut body = prologue(16);
    body.push(lui("t2", hi(table_1)));
    body.extend(std::iter::repeat_n(addiu("v0", "v0", 1), 12));
    body.extend(dispatch("a0", "t2", table_1));
    let inner = nested + 4 * body.len() as u32;
    body.extend(dispatch("a1", "t2", table_2));
    let done = nested + 4 * body.len() as u32;
    body.extend(epilogue(16));
    fx.function_slots(2, "t::nested", &body, 2);
    fx.data(0x900, &[inner, done, done, done], Section::Named(".rodata"));
    fx.caller(1, &entry(0, 1024), 8, &[nested]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("24 of 1004 bytes"), "{out}");
}

/// t::after_panic sets `base` to its table's %hi, but on one path moves an
/// argument into it and calls t::panic, which never returns; the word after
/// that call is a case block that loops back to the dispatch. That
/// fall-through is not an edge: following it would find `move base, a0` and
/// refuse the switch.
fn after_noreturn(base: &str) {
    let mut fx = Fixture::new();
    let panic = fx.function(6, "t::panic", &[prologue(8), vec![b(-1), NOP]].concat());
    let f = fx.addr(2);
    let table = BASE + 0x900;
    let mut body = prologue(24);
    body.extend([sw("ra", 20, "sp"), lui(base, hi(table))]);
    let branch = body.len();
    body.extend([
        beq("a1", "zero", 0),
        NOP,
        or(base, "a0", "zero"),
        jal(panic),
        NOP,
    ]);
    let case_loop = f + 4 * body.len() as u32;
    body.extend([addiu("a1", "a1", -1), b(0), NOP]);
    let back = body.len() - 2;
    let case_exit = f + 4 * body.len() as u32;
    body.extend([lw("ra", 20, "sp"), jr("ra"), addiu("sp", "sp", 24)]);
    let top = body.len();
    body.extend(dispatch("a1", base, table));
    body[branch] = beq("a1", "zero", (top - branch - 1) as i64);
    body[back] = b((top - back - 1) as i64);
    fx.function_slots(2, "t::after_panic", &body, 2);
    fx.data(0x900, &[case_loop, case_exit], Section::Named(".rodata"));
    fx.caller(1, &entry(0, 1024), 8, &[f]);
    let (failures, out) = fx.guard();
    assert_eq!(failures, 0, "{out}");
    assert!(out.contains("40 of 1004 bytes"), "{out}");
}

#[test]
fn the_word_after_a_noreturn_call_is_not_its_return() {
    // A callee-saved base survives calls that return, so only knowing
    // t::panic does not return drops the path.
    after_noreturn("s0");
}

#[test]
fn no_path_carries_a_caller_saved_base_across_a_call() {
    // The callee may change t3, so no value of it reaches back past the
    // call, returning or not (hk-psx State::apply keeps a base in ra).
    after_noreturn("t3");
}

/// The patcher's `--check` and the scanner's output for t::a and t::b, with
/// and without the map.
fn two_switches(across_branch: bool) -> std::collections::HashMap<(&'static str, bool), String> {
    let mut fx = Fixture::new();
    let (table_a, table_b) = (BASE + 0x900, BASE + 0x908);
    let a = fx.addr(2);
    let mut body = [vec![lui("t0", hi(table_a))], dispatch("a0", "t0", table_a)].concat();
    let last = body.len() - 1;
    body[last] = lw("v0", 0, "a1");
    let cases_a = [a + 4 * body.len() as u32, a + 4 * body.len() as u32 + 8];
    fx.function(2, "t::a", &[body, epilogue(0), epilogue(0)].concat());
    let b_addr = fx.addr(4);
    let mut body = vec![lui("t0", hi(table_b))];
    if across_branch {
        body.extend([beq("a1", "zero", 1), NOP]); // to the dispatch, which is then a label
    }
    body.extend(dispatch("a0", "t0", table_b));
    let case_b = b_addr + 4 * body.len() as u32;
    fx.function(
        4,
        "t::b",
        &[body, vec![addu("v1", "v0", "zero")], epilogue(0)].concat(),
    );
    fx.data(
        0x900,
        &[cases_a[0], cases_a[1], case_b, case_b],
        Section::Named(".rodata"),
    );
    let (exe, map) = fx.write();
    let (exe, map) = (exe.to_str().unwrap(), map.to_str().unwrap());
    let mut runs = std::collections::HashMap::new();
    for (tool, main) in [
        (
            "patch",
            patch::main as fn(&[String], &mut dyn std::io::Write) -> i32,
        ),
        ("scan", scan::main),
    ] {
        for with_map in [false, true] {
            let mut args = if tool == "patch" {
                vec![exe, "--check"]
            } else {
                vec![exe]
            };
            if with_map {
                args.extend(["--map", map]);
            }
            runs.insert((tool, with_map), call(main, &args).1);
        }
    }
    runs
}

#[test]
fn the_patcher_bounds_tables_with_a_map() {
    // t::a's switch loads v0 in its delay slot. t::b's first case reads v0 at
    // once, so a table that reads on names that case as a consumer of t::a's
    // load: a harmless extra trampoline. Without a map a table stops where
    // another dispatch's table starts, which bounds t::a's while t::b's
    // dispatch shows its own base; once t::b keeps that base across a
    // branch, only the map proves t::b and so bounds t::a. The scanner
    // agrees with the patcher every time.
    let spurious = format!("via table entry {:08x}", BASE + 0x908);
    let count = |out: &str| {
        let at = out.find(" hazards in").unwrap();
        out[..at]
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .to_string()
    };
    for across_branch in [false, true] {
        let runs = two_switches(across_branch);
        assert_eq!(
            runs[&("patch", false)].contains(&spurious),
            across_branch,
            "{runs:?}"
        );
        assert!(!runs[&("patch", true)].contains(&spurious));
        assert!(runs[&("patch", true)].contains("0 hazards"));
        for with_map in [false, true] {
            assert_eq!(
                count(&runs[&("patch", with_map)]),
                count(&runs[&("scan", with_map)]),
                "{runs:?}"
            );
        }
    }
}

#[test]
fn without_a_map_only_the_switch_is_looked_for() {
    let mut fx = Fixture::new();
    fx.caller(1, "t::main", 8, &[]);
    let (exe, _) = fx.write();
    let mut out = Vec::new();
    assert_eq!(
        stack_guard::check(&exe, None, None, None, &mut out),
        0,
        "{}",
        String::from_utf8_lossy(&out)
    );
    let body = [vec![jalr("t9"), or("sp", "a2", "zero")], epilogue(0)].concat();
    fx.function(3, "__psx_rt_call_on_stack", &body);
    let (exe, _) = fx.write();
    let mut out = Vec::new();
    assert_eq!(stack_guard::check(&exe, None, None, None, &mut out), 1);
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("pass its link map"));
}

#[test]
fn the_binary_takes_the_same_command_line() {
    let mut fx = Fixture::new();
    let b = fx.leaf(3, "t::b", 24);
    fx.caller(1, &entry(512, 1024), 16, &[b]);
    let (exe, map) = fx.write();
    let bin = env!("CARGO_BIN_EXE_stack-guard");
    let out = std::process::Command::new(bin)
        .arg(&exe)
        .arg(&map)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout)
        .starts_with("ok   <psx_rt::scratchpad::ScratchpadStack<512, 1024>>"));
    let out = std::process::Command::new(bin)
        .arg(&exe)
        .arg(&map)
        .args(["--root", "t::b"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = std::process::Command::new(bin)
        .arg(&exe)
        .arg(&map)
        .args(["--root", "^t::b$", "--budget", "0x10"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL t::b: 24 of 16 bytes"));
}

#[test]
fn text_only_takes_the_bounds_from_the_map() {
    let mut fx = Fixture::new();
    let f1 = fx.addr(1);
    fx.function(0, "_start", &[jal(f1), NOP, j(BASE), NOP]);
    let leaf = fx.leaf(2, "t::leaf", 8);
    fx.caller(1, &entry(0, 1024), 8, &[leaf]);
    let (exe, map) = fx.write();
    let text = psoxide_hazard::linkmap::LinkMap::open(&map).unwrap().text;
    let (exe, map) = (exe.to_str().unwrap(), map.to_str().unwrap());
    let args = |extra: &[&str]| -> Vec<String> {
        [exe, "--map", map]
            .iter()
            .chain(extra)
            .map(|a| a.to_string())
            .collect()
    };
    let run = |main: &dyn Fn(&[String], &mut dyn std::io::Write) -> i32, args: &[String]| {
        let mut out = Vec::new();
        let status = main(args, &mut out);
        (status, String::from_utf8(out).unwrap())
    };
    let flagged = run(&scan::main, &args(&["--text-only"]));
    assert_eq!(
        flagged,
        run(&|a, o| scan::main_in(a, &[text], o), &args(&[]))
    );
    assert_eq!(flagged.0, 0, "{}", flagged.1);
    let flagged = run(&patch::main, &args(&["--check", "--text-only"]));
    assert_eq!(
        flagged,
        run(
            &|a, o| patch::main_in(a, Some(&[text]), o),
            &args(&["--check"])
        )
    );
    // Without a map there are no bounds to read.
    let bare: Vec<String> = [exe, "--text-only"].iter().map(|a| a.to_string()).collect();
    assert_eq!(run(&scan::main, &bare).0, 2);
    assert_eq!(run(&patch::main, &bare).0, 2);
}

/// A static slice of `{ label: &'static [u8], .. }` records in `.rodata`:
/// each record is a pointer into the load (`lb at,..` read as code) and a
/// length of 8 (`jr zero`). Twenty records, so the heuristic data guard sees
/// nothing undecodable near any of them.
fn setting_table() -> Vec<u32> {
    (0..20).flat_map(|i| [0x8001_0A40 + 4 * i, 8]).collect()
}

/// `t::g` returns a byte loaded in its `jr ra` slot (a real hazard, in
/// `.text`) while `.rodata` at BASE + 0x900 holds the table above.
fn rodata_table_fixture() -> (Fixture, Vec<u32>) {
    let mut fx = Fixture::new();
    let data = BASE + Image::DATA;
    fx.function(
        1,
        "t::g",
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    fx.caller(0, "t::main", 0, &[fx.addr(1)]);
    let table = setting_table();
    fx.data(0x900, &table, Section::Named(".rodata"));
    (fx, table)
}

#[test]
fn patching_with_a_map_never_touches_rodata() {
    // hl/wipeout options screen, 2026-10-08: `psoxide-pgo apply` ran the
    // patcher with a map but without `--text-only`, so the heuristic data
    // guard read this table as code, saw `jr zero` with a load in its slot,
    // and rewrote table words into jumps.
    let (fx, table) = rodata_table_fixture();
    let (exe, map) = fx.write();
    let (exe_arg, map_arg) = (exe.to_str().unwrap(), map.to_str().unwrap());
    let rodata = |exe: &std::path::Path| {
        let data = std::fs::read(exe).unwrap();
        let start = 0x800 + 0x900;
        data[start..start + 4 * table.len()].to_vec()
    };
    let before_exe = std::fs::read(&exe).unwrap();
    let before = rodata(&exe);
    assert_eq!(
        before,
        table
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>()
    );

    // Without the map the whole-image heuristic does read the table as code:
    // this is the failure the map bound exists to prevent.
    let mut out = Vec::new();
    assert_eq!(
        patch::main(&[exe_arg.into(), "--check".into()], &mut out),
        1
    );
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains(&format!("hazard {:08x}: jr zero", BASE + 0x900 + 4)),
        "{out}"
    );
    assert_eq!(std::fs::read(&exe).unwrap(), before_exe);

    // With the map (and no other flag) only the real hazard is found and
    // fixed, and every byte outside .text and the trampoline array is kept.
    let args = |extra: &[&str]| {
        let mut args = vec![exe_arg.to_string(), "--map".into(), map_arg.into()];
        args.extend(extra.iter().map(|s| s.to_string()));
        args
    };
    let mut out = Vec::new();
    assert_eq!(patch::main(&args(&["--check"]), &mut out), 1);
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out.matches("hazard ").count(), 1, "{out}");
    assert!(out.contains("jr ra | slot lbu v0"), "{out}");
    let mut out = Vec::new();
    assert_eq!(patch::main(&args(&[]), &mut out), 0);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("1 patched, 0 remaining"), "{out}");
    let after_exe = std::fs::read(&exe).unwrap();
    assert_eq!(rodata(&exe), before, "patching rewrote .rodata");
    let changed: Vec<usize> = (0..before_exe.len())
        .filter(|&i| before_exe[i] != after_exe[i])
        .collect();
    let (text_end, tramp) = (0x800 + 0x800, (0x800 + Image::TRAMPOLINES) as usize);
    assert!(
        changed
            .iter()
            .all(|&i| i < text_end || (tramp..tramp + 8 + 64 * 4).contains(&i)),
        "bytes changed outside .text and the trampoline array: {changed:?}"
    );
    assert!(!changed.is_empty());

    // The scanner agrees about what is code.
    let mut out = Vec::new();
    assert_eq!(scan::main(&args(&[]), &mut out), 0);
    assert!(String::from_utf8(out)
        .unwrap()
        .ends_with(&format!("0 hazards in {exe_arg}\n")));
}

#[test]
fn patching_without_text_bounds_is_refused() {
    let (fx, _) = rodata_table_fixture();
    let (exe, _) = fx.write();
    let before = std::fs::read(&exe).unwrap();
    let mut out = Vec::new();
    let status = patch::main(&[exe.to_str().unwrap().to_string()], &mut out);
    assert_eq!(status, 2);
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("without .text bounds"));
    assert_eq!(std::fs::read(&exe).unwrap(), before);
}

#[test]
fn code_ranges_extend_the_map_bounds() {
    // A real hazard in a function the map's .text does not span (a code
    // module linked elsewhere) is invisible to the bounded tools until
    // `--code` names it.
    let mut fx = Fixture::new();
    let data = BASE + Image::DATA;
    let module = BASE + 0x900;
    fx.image.put(
        0x900,
        &[lui("at", hi(data)), jr("ra"), lbu("v0", lo(data), "at")],
    );
    fx.caller(0, "t::main", 0, &[module]);
    let (exe, map) = fx.write();
    let (exe, map) = (exe.to_str().unwrap(), map.to_str().unwrap());
    let run = |extra: &[&str]| {
        let mut args = vec![
            exe.to_string(),
            "--map".into(),
            map.into(),
            "--check".into(),
        ];
        args.extend(extra.iter().map(|a| a.to_string()));
        let mut out = Vec::new();
        let status = patch::main(&args, &mut out);
        (status, String::from_utf8(out).unwrap())
    };
    assert_eq!(run(&[]).0, 0);
    let range = format!("{:x}..{:x}", module, module + 0x40);
    let (status, out) = run(&["--code", &range]);
    assert_eq!(status, 1, "{out}");
    assert!(
        out.contains(&format!("hazard {:08x}: jr ra", module + 4)),
        "{out}"
    );
    assert_eq!(run(&["--code", "nonsense"]).0, 2);
}
