//! Fixtures for `window-gate`.
//!
//! Each case assembles a tiny PS-EXE plus the ld.lld map that would describe it: psx-rt's three shared window
//! routines, a few stubs, and a call tree behind them, then checks the proof or the reason it fails.

mod common;

use common::*;
use psoxide_hazard::stack_guard::GuardImage;
use psoxide_hazard::window_gate::{self, Report};

const BASE: u32 = 0x8001_0000;
const SLOT: u32 = 0x40;
const L1: u32 = 0;
const GUARD: u32 = 1;
const HOP: u32 = 2;

fn addr(index: u32) -> u32 {
    BASE + index * SLOT
}

fn frame_in(frame: i64, body: &[u32]) -> Vec<u32> {
    let mut w = vec![addiu("sp", "sp", -frame)];
    w.extend_from_slice(body);
    w.extend([jr("ra"), addiu("sp", "sp", frame)]);
    w
}

/// A level-1 or guard stub for `target`, carrying `floor` (and `room` for a guard).
fn stub(target: u32, guard: bool, floor: u32, room: u32) -> Vec<u32> {
    vec![
        lui("t9", hi(target)),
        addiu("t9", "t9", lo(target)),
        j(addr(if guard { GUARD } else { L1 })),
        if guard {
            addiu("t8", "zero", i64::from(room))
        } else {
            NOP
        },
        0x2400_0000 | floor,
    ]
}

struct Scene {
    image: Image,
    functions: Vec<(u32, u32, String)>,
    dir: TempDir,
}

impl Scene {
    fn new() -> Self {
        let mut s = Self {
            image: Image::new(BASE),
            functions: vec![],
            dir: TempDir::new("window"),
        };
        for (i, name) in [
            (L1, "__psx_rt_window_l1"),
            (GUARD, "__psx_rt_window_guard"),
            (HOP, "__psx_rt_window_hop"),
        ] {
            s.put(i, name, &[jr("ra"), NOP]);
        }
        s
    }

    fn put(&mut self, index: u32, name: &str, words: &[u32]) -> u32 {
        assert!(words.len() as u32 * 4 <= SLOT, "{name}");
        self.image.put(index * SLOT, words);
        self.functions
            .push((addr(index), words.len() as u32 * 4, name.to_string()));
        addr(index)
    }

    fn check(&self, floor: Option<u32>, top: u32) -> Report {
        let exe = self.dir.0.join("w.exe");
        self.image.write(&exe);
        let map = self.dir.0.join("w.map");
        let payload = self.image.words.len() as u32 * 4;
        let mut lines = vec!["     VMA      LMA     Size Align Out     In      Symbol".to_string()];
        let mut row = |a: u32, size: u32, depth: usize, name: &str, align: u32| {
            lines.push(format!(
                "{a:8x} {a:8x} {size:8x} {align:5} {}{name}",
                " ".repeat(depth)
            ));
        };
        row(BASE, 0, 8, "__text_start = .", 1);
        let mut functions = self.functions.clone();
        functions.sort();
        for (i, (a, size, name)) in functions.iter().enumerate() {
            row(*a, *size, 8, &format!("/w.o:(.text.f{i})"), 4);
            row(*a, *size, 16, name, 1);
        }
        row(BASE + 0x20 * SLOT, 0, 8, "__text_end = .", 1);
        let tramp = BASE + Image::TRAMPOLINES;
        row(tramp, 8 + 64 * 4, 8, "/w.o:(.data.HAZARD_TRAMPOLINES)", 4);
        row(tramp, 8 + 64 * 4, 16, "HAZARD_TRAMPOLINES", 1);
        row(BASE + payload, 0, 8, "__data_end = .", 1);
        row(BASE + payload, 0, 8, "__bss_start = .", 1);
        std::fs::write(&map, lines.join("\n") + "\n").unwrap();
        let image = GuardImage::open(&exe, &map).unwrap();
        window_gate::analyse(&image, floor, top)
    }
}

/// root (frame 64) behind a level-1 stub, calling a guard stub for `callee` (frame `callee_frame`).
fn scene(room: u32, callee_frame: i64, floor: u32) -> Scene {
    let mut s = Scene::new();
    let (s_root, root, s_callee, callee) = (3, 4, 5, 6);
    s.put(s_root, "__ws_root", &stub(addr(root), false, floor, 0));
    s.put(root, "root", &frame_in(64, &[jal(addr(s_callee))]));
    s.put(
        s_callee,
        "__ws_callee",
        &stub(addr(callee), true, floor, room),
    );
    s.put(callee, "callee", &frame_in(callee_frame, &[]));
    s
}

fn failed(r: &Report, what: &str) -> bool {
    !r.ok
        && r.lines
            .iter()
            .any(|l| l.starts_with("FAIL") && l.contains(what))
}

#[test]
fn a_guard_with_enough_room_passes() {
    let r = scene(8 + 128 + 20, 128, 8).check(None, 976);
    assert!(r.ok, "{:?}", r.lines);
    assert_eq!(r.stubs, 2);
}

#[test]
fn a_guard_below_its_bound_fails() {
    let r = scene(8 + 64, 128, 8).check(None, 976);
    assert!(failed(&r, "guard callee"), "{:?}", r.lines);
}

#[test]
fn the_floor_the_stubs_carry_is_what_is_proved_against() {
    // Room 156 covers floor 8 but not floor 256.
    assert!(scene(156, 128, 8).check(None, 976).ok);
    let r = scene(156, 128, 256).check(None, 976);
    assert!(failed(&r, "guard callee"), "{:?}", r.lines);
    let r = scene(156, 128, 8).check(Some(256), 976);
    assert!(failed(&r, "floor"), "{:?}", r.lines);
}

#[test]
fn a_root_deeper_than_the_window_fails() {
    let r = scene(8 + 128 + 20, 128, 8).check(None, 60);
    assert!(failed(&r, "level1 root"), "{:?}", r.lines);
}

#[test]
fn a_direct_call_that_skips_the_stub_fails() {
    let mut s = scene(8 + 128 + 20, 128, 8);
    s.put(4, "root", &frame_in(64, &[jal(addr(6))]));
    let r = s.check(None, 976);
    assert!(failed(&r, "skips the stub"), "{:?}", r.lines);
}

#[test]
fn an_indirect_call_in_the_tree_fails() {
    let mut s = scene(8 + 128 + 20, 128, 8);
    s.put(6, "callee", &frame_in(128, &[jalr("v0")]));
    let r = s.check(None, 976);
    assert!(failed(&r, "guard callee"), "{:?}", r.lines);
}

#[test]
fn recursion_through_a_self_call_fails() {
    let mut s = scene(8 + 128 + 20, 128, 8);
    s.put(6, "callee", &frame_in(128, &[jal(addr(6))]));
    let r = s.check(None, 976);
    assert!(failed(&r, "calls itself"), "{:?}", r.lines);
}

#[test]
fn an_sp_realignment_fails() {
    let mut s = scene(8 + 128 + 20, 128, 8);
    // and sp,sp,-256
    s.put(
        6,
        "callee",
        &[
            addiu("sp", "sp", -128),
            i_type(0x0c, "sp", "sp", -256),
            jr("ra"),
            NOP,
        ],
    );
    let r = s.check(None, 976);
    assert!(failed(&r, "guard callee"), "{:?}", r.lines);
}

#[test]
fn recursion_through_a_guard_is_bounded_by_the_guard() {
    let mut s = scene(8 + 152 + 20, 128, 8);
    // callee calls its own guard stub again: the stub is a 24-byte leaf, the room check protects each level.
    s.put(6, "callee", &frame_in(128, &[jal(addr(5))]));
    let r = s.check(None, 976);
    assert!(r.ok, "{:?}", r.lines);
}

#[test]
fn a_program_without_stubs_passes_silently() {
    let r = Scene::new().check(None, 976);
    assert!(r.ok);
    assert_eq!(r.stubs, 0);
}

#[test]
fn a_jump_to_a_window_routine_outside_a_stub_fails() {
    let mut s = scene(8 + 128 + 20, 128, 8);
    // A hand-written 3-word stub (size under 20) is not recognised, so its `j` is found instead.
    s.put(
        7,
        "handmade",
        &[lui("t9", hi(addr(6))), j(addr(GUARD)), NOP],
    );
    let r = s.check(None, 976);
    assert!(failed(&r, "not a recognised stub"), "{:?}", r.lines);
}
