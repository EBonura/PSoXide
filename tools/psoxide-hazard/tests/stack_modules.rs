//! Scratchpad stack proof through compact streamed code modules.
mod common;

use common::*;
use psoxide_hazard::stack_guard;

const BASE: u32 = 0x8001_0000;
const LINK: u32 = 0x8041_0000;
const AT: u32 = BASE + 0x1000;
const SIZE: u32 = 0x100;
const ROOT: u32 = BASE;
const FLUSH: u32 = BASE + 0x80;
const MODULE_FN: u32 = LINK;
const MODULE_TRAMP: u32 = LINK + 0x88;

fn row(address: u32, size: u32, depth: usize, name: &str) -> String {
    format!(
        "{address:8x} {address:8x} {size:8x}     4 {}{name}\n",
        " ".repeat(depth)
    )
}

struct Fixture {
    _dir: TempDir,
    exe: std::path::PathBuf,
    map: std::path::PathBuf,
}

impl Fixture {
    fn new(via_trampoline: bool, module_size: u32, mapped_size: u32, calls_flush: bool) -> Self {
        let dir = TempDir::new("stack-module");
        let exe = dir.0.join("game.exe");
        let map = dir.0.join("game.map");
        let mut image = Image::new(BASE);
        image.put(
            0,
            &[
                jal(if via_trampoline {
                    MODULE_TRAMP
                } else {
                    MODULE_FN
                }),
                NOP,
                jr("ra"),
                NOP,
            ],
        );
        image.put(0x80, &[jr("ra"), NOP]);
        image.write(&exe);
        let mut module = vec![NOP; SIZE as usize / 4];
        module[..6].copy_from_slice(&[
            addiu("sp", "sp", -16),
            if calls_flush { jal(FLUSH) } else { NOP },
            NOP,
            jr("ra"),
            addiu("sp", "sp", 16),
            NOP,
        ]);
        module[0x80 / 4] = MAGIC;
        module[0x80 / 4 + 1] = 16;
        module[0x88 / 4..0x94 / 4].copy_from_slice(&[NOP, j(MODULE_FN), NOP]);
        let mut file = std::fs::read(&exe).unwrap();
        for word in module {
            file.extend_from_slice(&word.to_le_bytes());
        }
        std::fs::write(&exe, file).unwrap();
        let mut text = String::from("     VMA      LMA     Size Align Out     In      Symbol\n");
        text += &row(BASE, 0, 8, "__text_start = .");
        text += &row(ROOT, 16, 8, "/fixture.o:(.text.root)");
        text += &row(
            ROOT,
            16,
            16,
            "<psx_rt::scratchpad::ScratchpadStack<0, 1024>>::stack_entry::<u32, t::f>",
        );
        text += &row(FLUSH, 8, 8, "/fixture.o:(.text.flush)");
        text += &row(FLUSH, 8, 16, "__psx_rt_flush_i_cache");
        text += &row(BASE + 0x800, 0, 8, "__text_end = .");
        text += &row(
            BASE + Image::TRAMPOLINES,
            8 + 64 * 4,
            8,
            "/fixture.o:(.data.HAZARD_TRAMPOLINES)",
        );
        text += &row(
            BASE + Image::TRAMPOLINES,
            8 + 64 * 4,
            16,
            "HAZARD_TRAMPOLINES",
        );
        text += &row(BASE + 0x1000, 0, 8, "__data_end = .");
        text += &row(LINK, module_size, 0, ".mod_test");
        text += &row(LINK, mapped_size, 8, "/fixture.o:(.text.module)");
        text += &row(LINK, mapped_size, 16, "module_callee");
        std::fs::write(&map, text).unwrap();
        Self {
            _dir: dir,
            exe,
            map,
        }
    }

    fn guard(&self, span: u32) -> (i32, String) {
        let args = vec![
            self.exe.display().to_string(),
            self.map.display().to_string(),
            "--module".into(),
            format!("{LINK:x}..{:x}@{AT:x}", LINK + span),
            "--forbid".into(),
            "__psx_rt_flush_i_cache".into(),
        ];
        let mut out = Vec::new();
        let status = stack_guard::main(&args, &mut out);
        (status, String::from_utf8(out).unwrap())
    }
}

#[test]
fn resident_root_reaching_module_and_forbidden_flush_fails() {
    let fixture = Fixture::new(false, SIZE, 24, true);
    let (status, output) = fixture.guard(SIZE);
    assert_eq!(status, 1, "{output}");
    assert!(
        output.contains("reaches __psx_rt_flush_i_cache"),
        "{output}"
    );
}

#[test]
fn module_trampoline_still_reaches_forbidden_flush() {
    let fixture = Fixture::new(true, SIZE, 24, true);
    let (status, output) = fixture.guard(SIZE);
    assert_eq!(status, 1, "{output}");
    assert!(
        output.contains("reaches __psx_rt_flush_i_cache"),
        "{output}"
    );
}

#[test]
fn mapped_function_outside_readable_module_fails_closed() {
    let fixture = Fixture::new(false, SIZE, SIZE + 4, true);
    let (status, output) = fixture.guard(SIZE);
    assert_eq!(status, 1, "{output}");
    assert!(output.contains("not wholly readable"), "{output}");
}

#[test]
fn truncated_module_placement_and_missing_flag_fail() {
    let fixture = Fixture::new(false, SIZE, 24, true);
    let (status, output) = fixture.guard(SIZE + 4);
    assert_eq!(status, 1, "{output}");
    assert!(output.contains("not inside the load"), "{output}");
    let args = vec![
        fixture.exe.display().to_string(),
        fixture.map.display().to_string(),
        "--forbid".into(),
        "__psx_rt_flush_i_cache".into(),
    ];
    let mut out = Vec::new();
    assert_eq!(stack_guard::main(&args, &mut out), 1);
    assert!(String::from_utf8(out).unwrap().contains("outside .text"));
}

#[test]
fn resident_root_calls_a_bounded_module_function() {
    let fixture = Fixture::new(false, SIZE, 24, false);
    let (status, output) = fixture.guard(SIZE);
    assert_eq!(status, 0, "{output}");
    assert!(output.contains("ok"), "{output}");
    assert!(output.contains("module_callee"), "{output}");
}

#[test]
fn a_module_stack_root_cannot_disappear_without_a_module_declaration() {
    let fixture = Fixture::new(false, SIZE, 24, true);
    let mut map = std::fs::read_to_string(&fixture.map).unwrap();
    map += &row(
        LINK,
        24,
        16,
        "<psx_rt::scratchpad::ScratchpadStack<0, 1024>>::stack_entry::<u32, t::module>",
    );
    std::fs::write(&fixture.map, map).unwrap();
    let args = vec![
        fixture.exe.display().to_string(),
        fixture.map.display().to_string(),
        "--forbid".into(),
        "__psx_rt_flush_i_cache".into(),
    ];
    let mut out = Vec::new();
    assert_eq!(stack_guard::main(&args, &mut out), 1);
    let output = String::from_utf8(out).unwrap();
    assert!(output.contains("outside declared module code"), "{output}");
    let (status, output) = fixture.guard(SIZE);
    assert_eq!(status, 1, "{output}");
    assert!(
        output.contains("reaches __psx_rt_flush_i_cache"),
        "{output}"
    );
}
