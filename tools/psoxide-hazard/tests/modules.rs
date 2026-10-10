//! `--module`: a code module linked in the RAM mirrors whose bytes sit after
//! the resident load, checked and patched at its link address.
//!
//! Each case writes a small PS-EXE (the resident load: nops and a
//! trampoline array of its own), appends a 0x100-byte module after the
//! payload its header claims, and a map that describes the resident link.

mod common;

use std::path::PathBuf;

use common::*;
use psoxide_hazard::{patch, scan};

const BASE: u32 = 0x8001_0000;
/// Where the module is linked.
const LINK: u32 = 0x8041_0000;
const SIZE: u32 = 0x100;
/// Where its bytes sit: just past the resident payload the header claims.
const AT: u32 = BASE + 0x1000;
/// The module's code, its trampoline array, and the array's end.
const CODE: u32 = 0x00;
const ARRAY: u32 = 0x80;
const END: u32 = 0xC8;

struct Setup {
    _dir: TempDir,
    exe: PathBuf,
    map: PathBuf,
}

/// The resident image, the module's words appended after it, and the map.
fn setup(module: &[u32]) -> Setup {
    let dir = TempDir::new("module");
    let exe = dir.0.join("game.exe");
    Image::new(BASE).write(&exe);
    let mut words = vec![NOP; SIZE as usize / 4];
    words[..module.len()].copy_from_slice(module);
    // The module's own trampoline array: magic, 16 words.
    words[ARRAY as usize / 4] = MAGIC;
    words[ARRAY as usize / 4 + 1] = 16;
    let mut file = std::fs::read(&exe).unwrap();
    for w in words {
        file.extend_from_slice(&w.to_le_bytes());
    }
    std::fs::write(&exe, file).unwrap();
    let payload = 0x1000;
    let row = |address: u32, size: u32, depth: usize, name: &str| {
        format!(
            "{address:8x} {address:8x} {size:8x}     4 {}{name}\n",
            " ".repeat(depth)
        )
    };
    let mut map = String::from("     VMA      LMA     Size Align Out     In      Symbol\n");
    map += &row(BASE, 0, 8, "__text_start = .");
    map += &row(BASE + 0x800, 0, 8, "__text_end = .");
    let tramp = BASE + Image::TRAMPOLINES;
    map += &row(
        tramp,
        8 + 64 * 4,
        8,
        "/fixture.o:(.data.HAZARD_TRAMPOLINES)",
    );
    map += &row(tramp, 8 + 64 * 4, 16, "HAZARD_TRAMPOLINES");
    map += &row(BASE + payload, 0, 8, "__data_end = .");
    map += &row(BASE + payload, 0, 8, "__bss_start = .");
    let map_path = dir.0.join("game.map");
    std::fs::write(&map_path, map).unwrap();
    Setup {
        _dir: dir,
        exe,
        map: map_path,
    }
}

/// A hazard in the module: `jr ra` with a load in its slot, whose consumer
/// is the caller's first instruction.
fn hazardous() -> Vec<u32> {
    vec![lui("at", 1), jr("ra"), lbu("v0", 0x10, "a0")]
}

fn module_flag(link: u32, size: u32, at: u32) -> String {
    format!("{:x}..{:x}@{at:x}", link, link + size)
}

fn args(setup: &Setup, extra: &[&str]) -> Vec<String> {
    [
        setup.exe.to_str().unwrap(),
        "--map",
        setup.map.to_str().unwrap(),
    ]
    .iter()
    .chain(extra)
    .map(|a| a.to_string())
    .collect()
}

/// The flags that declare the module and its code: the code up to the
/// array, and the array.
fn declared(at: u32) -> Vec<String> {
    let code = format!("{:x}..{:x}", LINK + CODE, LINK + 0x40);
    let array = format!("{:x}..{:x}", LINK + ARRAY, LINK + END);
    [
        "--module",
        &module_flag(LINK, SIZE, at),
        "--code",
        &code,
        "--code",
        &array,
    ]
    .iter()
    .map(|a| a.to_string())
    .collect()
}

fn run(
    main: fn(&[String], &mut dyn std::io::Write) -> i32,
    setup: &Setup,
    extra: &[String],
) -> (i32, String) {
    let mut all = args(setup, &[]);
    all.extend(extra.iter().cloned());
    let mut out = Vec::new();
    let status = main(&all, &mut out);
    (status, String::from_utf8(out).unwrap())
}

fn word(file: &[u8], address: u32) -> u32 {
    let at = (address - BASE) as usize + 0x800;
    u32::from_le_bytes(file[at..at + 4].try_into().unwrap())
}

#[test]
fn a_module_is_checked_at_its_link_address() {
    let setup = setup(&hazardous());
    // The module's bytes are not code to the resident ranges: no module, no
    // hazard.
    assert_eq!(run(patch::main, &setup, &["--check".into()]).0, 0);
    let mut flags = declared(AT);
    flags.push("--check".into());
    let (status, out) = run(patch::main, &setup, &flags);
    assert_eq!(status, 1, "{out}");
    // The address is the link address, not where the bytes sit.
    assert!(
        out.contains(&format!("hazard {:08x}: jr ra", LINK + 4)),
        "{out}"
    );
    assert!(!out.contains(&format!("{:08x}", AT + 4)), "{out}");
    let (status, out) = run(scan::main, &setup, &declared(AT));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains(&format!("{:08x}: jr ra", LINK + 4)), "{out}");
}

#[test]
fn a_hazard_in_a_module_is_patched_through_its_own_array() {
    let setup = setup(&hazardous());
    let before = std::fs::read(&setup.exe).unwrap();
    let (status, out) = run(patch::main, &setup, &declared(AT));
    assert_eq!(status, 0, "{out}");
    let tramp = LINK + ARRAY + 8;
    assert!(
        out.contains(&format!(
            "patched jr ra at {:08x} -> trampoline {tramp:08x}",
            LINK + 4
        )),
        "{out}"
    );
    assert!(
        out.contains("1 patched, 0 remaining, 3/16 trampoline words"),
        "{out}"
    );
    let after = std::fs::read(&setup.exe).unwrap();
    assert_eq!(after.len(), before.len());
    // In the file the module is still where it sat; the rerouted jump is a
    // link address, and the trampoline carries the load and the return.
    assert_eq!(word(&after, AT + 4), j(tramp));
    assert_eq!(word(&after, AT + 8), NOP);
    assert_eq!(word(&after, AT + ARRAY + 8), lbu("v0", 0x10, "a0"));
    assert_eq!(word(&after, AT + ARRAY + 12), jr("ra"));
    // The resident image, its array included, is as it was.
    let resident = 0..(AT - BASE) as usize + 0x800;
    assert_eq!(after[resident.clone()], before[resident]);
    // The scan agrees, and a second run finds nothing to change.
    let (status, out) = run(scan::main, &setup, &declared(AT));
    assert_eq!(status, 0, "{out}");
    assert!(out.contains("0 hazards"), "{out}");
    let (status, _) = run(patch::main, &setup, &declared(AT));
    assert_eq!(status, 0);
    assert_eq!(std::fs::read(&setup.exe).unwrap(), after);
}

#[test]
fn a_branch_in_a_module_is_patched_to_link_address_targets() {
    // `bne a1,zero,T ; lbu v0,0x10(a0)` with a consumer of v0 on the
    // fall-through (+8) and at the target (+0xC): the trampoline has both exits.
    let module = vec![
        bne("a1", "zero", 2),
        lbu("v0", 0x10, "a0"),
        addu("v1", "v0", "v0"),
        addu("v1", "v0", "v0"),
    ];
    let setup = setup(&module);
    let (status, out) = run(patch::main, &setup, &declared(AT));
    assert_eq!(status, 0, "{out}");
    let tramp = LINK + ARRAY + 8;
    assert!(
        out.contains(&format!(
            "patched {:08x} -> trampoline {tramp:08x} (6 words)",
            LINK
        )),
        "{out}"
    );
    let file = std::fs::read(&setup.exe).unwrap();
    assert_eq!(word(&file, AT), j(tramp));
    // The fall-through and the branch target, both in link addresses.
    assert_eq!(word(&file, AT + ARRAY + 8 + 8), j(LINK + 8));
    assert_eq!(word(&file, AT + ARRAY + 8 + 16), j(LINK + 0xC));
    assert_eq!(run(scan::main, &setup, &declared(AT)).0, 0);
}

#[test]
fn a_module_outside_the_load_is_refused() {
    let setup = setup(&hazardous());
    let before = std::fs::read(&setup.exe).unwrap();
    // Past the end of the file, straddling it, and below the load address.
    for at in [AT + SIZE, AT + 4, BASE - 0x100] {
        let (status, out) = run(patch::main, &setup, &declared(at));
        assert_eq!(status, 2, "{at:x}: {out}");
        assert!(out.contains("not inside the load"), "{out}");
        let (status, out) = run(scan::main, &setup, &declared(at));
        assert_eq!(status, 2, "{at:x}: {out}");
    }
    assert_eq!(std::fs::read(&setup.exe).unwrap(), before);
}

#[test]
fn modules_may_not_overlap() {
    let setup = setup(&hazardous());
    let other = |link: u32, at: u32| {
        let mut flags = declared(AT);
        flags.extend(["--module".into(), module_flag(link, SIZE, at)]);
        flags
    };
    // Link ranges.
    let (status, out) = run(patch::main, &setup, &other(LINK + 0x80, AT + 0x400));
    assert_eq!(status, 2, "{out}");
    assert!(out.contains("link range overlaps"), "{out}");
    // Bytes.
    let (status, out) = run(patch::main, &setup, &other(LINK + 0x1000, AT + 0x80));
    assert_eq!(status, 2, "{out}");
    assert!(out.contains("bytes overlap"), "{out}");
}

#[test]
fn a_module_may_not_sit_on_resident_code() {
    let setup = setup(&hazardous());
    let (status, out) = run(patch::main, &setup, &declared(BASE + 0x100));
    assert_eq!(status, 2, "{out}");
    assert!(out.contains("overlap resident load bytes"), "{out}");
}

#[test]
fn a_module_may_not_overwrite_resident_data() {
    let setup = setup(&hazardous());
    let (status, out) = run(patch::main, &setup, &declared(AT - SIZE));
    assert_eq!(status, 2, "{out}");
    assert!(out.contains("overlap resident load bytes"), "{out}");
    assert_eq!(run(scan::main, &setup, &declared(AT - SIZE)).0, 2);
}

#[test]
fn two_modules_patch_through_their_own_arrays() {
    let setup = setup(&hazardous());
    let second_link = LINK + 0x1000;
    let second_at = AT + SIZE;
    let mut file = std::fs::read(&setup.exe).unwrap();
    let mut words = vec![NOP; SIZE as usize / 4];
    words[..hazardous().len()].copy_from_slice(&hazardous());
    words[ARRAY as usize / 4] = MAGIC;
    words[ARRAY as usize / 4 + 1] = 16;
    for word in words {
        file.extend_from_slice(&word.to_le_bytes());
    }
    std::fs::write(&setup.exe, file).unwrap();
    let mut flags = declared(AT);
    flags.extend([
        "--module".into(),
        module_flag(second_link, SIZE, second_at),
        "--code".into(),
        format!("{:x}..{:x}", second_link, second_link + 0x40),
        "--code".into(),
        format!("{:x}..{:x}", second_link + ARRAY, second_link + END),
    ]);
    let (status, out) = run(patch::main, &setup, &flags);
    assert_eq!(status, 0, "{out}");
    assert!(
        out.contains(&format!("trampoline {:08x}", LINK + ARRAY + 8)),
        "{out}"
    );
    assert!(
        out.contains(&format!("trampoline {:08x}", second_link + ARRAY + 8)),
        "{out}"
    );
    let after = std::fs::read(&setup.exe).unwrap();
    assert_eq!(word(&after, AT + 4), j(LINK + ARRAY + 8));
    assert_eq!(word(&after, second_at + 4), j(second_link + ARRAY + 8));
    assert_eq!(run(scan::main, &setup, &flags).0, 0);
}

#[test]
fn resident_and_module_hazards_use_separate_arrays() {
    let setup = setup(&hazardous());
    let mut file = std::fs::read(&setup.exe).unwrap();
    let resident = BASE + 0x40;
    let offset = (resident - BASE) as usize + 0x800;
    for (index, word) in hazardous().iter().enumerate() {
        file[offset + index * 4..offset + index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    std::fs::write(&setup.exe, file).unwrap();
    let flags = declared(AT);
    let (status, out) = run(patch::main, &setup, &flags);
    assert_eq!(status, 0, "{out}");
    let after = std::fs::read(&setup.exe).unwrap();
    assert_eq!(word(&after, resident + 4), j(BASE + Image::TRAMPOLINES + 8));
    assert_eq!(word(&after, AT + 4), j(LINK + ARRAY + 8));
    assert_eq!(run(scan::main, &setup, &flags).0, 0);
}

#[test]
fn module_data_that_looks_like_an_array_is_not_used() {
    let setup = setup(&hazardous());
    let mut file = std::fs::read(&setup.exe).unwrap();
    let offset = (AT + 0x50 - BASE) as usize + 0x800;
    file[offset..offset + 4].copy_from_slice(&MAGIC.to_le_bytes());
    file[offset + 4..offset + 8].copy_from_slice(&4u32.to_le_bytes());
    std::fs::write(&setup.exe, file).unwrap();
    let (status, out) = run(patch::main, &setup, &declared(AT));
    assert_eq!(status, 0, "{out}");
    let after = std::fs::read(&setup.exe).unwrap();
    assert_eq!(word(&after, AT + 4), j(LINK + ARRAY + 8));
    assert_eq!(word(&after, AT + 0x50), MAGIC);
}

#[test]
fn an_array_crossing_the_module_end_is_refused() {
    let setup = setup(&hazardous());
    let mut file = std::fs::read(&setup.exe).unwrap();
    let capacity_at = (AT + ARRAY + 4 - BASE) as usize + 0x800;
    file[capacity_at..capacity_at + 4].copy_from_slice(&64u32.to_le_bytes());
    let before = file.clone();
    std::fs::write(&setup.exe, file).unwrap();
    let (status, out) = run(patch::main, &setup, &declared(AT));
    assert_eq!(status, 1, "{out}");
    assert!(
        out.contains("no HAZARD_TRAMPOLINES array for hazard"),
        "{out}"
    );
    assert_eq!(std::fs::read(&setup.exe).unwrap(), before);
}

#[test]
fn a_module_link_range_must_be_in_the_ram_mirrors() {
    let setup = setup(&hazardous());
    for (link, size) in [
        (0x8010_0000, SIZE),
        (0x801F_FF00, 0x200),
        (0x8080_0000 - 0x80, 0x100),
        (0x0041_0000, SIZE),
        (0xA041_0000, SIZE),
    ] {
        let flags = vec!["--module".to_string(), module_flag(link, size, AT)];
        let (status, out) = run(patch::main, &setup, &flags);
        assert_eq!(status, 2, "{link:x}: {out}");
        assert!(out.contains("not in the RAM mirrors"), "{out}");
    }
    for bad in [
        "nonsense",
        "80410000..80410100",
        "80410100..80410000@80011000",
    ] {
        let flags = vec!["--module".to_string(), bad.to_string()];
        assert_eq!(run(patch::main, &setup, &flags).0, 2, "{bad}");
    }
    // Word alignment.
    let flags = vec!["--module".to_string(), module_flag(LINK + 2, SIZE, AT)];
    let (status, out) = run(patch::main, &setup, &flags);
    assert_eq!(status, 2, "{out}");
    assert!(out.contains("word aligned"), "{out}");
}

#[test]
fn code_ranges_are_in_ram_or_in_a_module() {
    let setup = setup(&hazardous());
    // In link space but in no module.
    let outside = format!("{:x}..{:x}", LINK + 0x200, LINK + 0x240);
    let mut flags = declared(AT);
    flags.extend(["--code".into(), outside]);
    let (status, out) = run(patch::main, &setup, &flags);
    assert_eq!(status, 2, "{out}");
    assert!(out.contains("bad --code range"), "{out}");
    // Straddling the end of the module.
    let straddling = format!("{:x}..{:x}", LINK + 0xF0, LINK + 0x110);
    let mut flags = declared(AT);
    flags.extend(["--code".into(), straddling]);
    assert_eq!(run(patch::main, &setup, &flags).0, 2);
    // And without a module the link range stays out of reach.
    let range = format!("{:x}..{:x}", LINK, LINK + 0x40);
    let (status, out) = run(patch::main, &setup, &["--code".into(), range]);
    assert_eq!(status, 2, "{out}");
}

#[test]
fn a_module_needs_a_map() {
    let setup = setup(&hazardous());
    let mut bare = vec![setup.exe.to_str().unwrap().to_string()];
    bare.extend(declared(AT));
    let mut out = Vec::new();
    assert_eq!(patch::main(&bare, &mut out), 2);
    assert!(String::from_utf8(out)
        .unwrap()
        .contains("--module needs --map"));
    let mut out = Vec::new();
    assert_eq!(scan::main(&bare, &mut out), 2);
}
