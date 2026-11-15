//! The two image shapes beside psoxide.ld's: a headerless blob (`--raw`) and
//! a PS-EXE with a boot overlay its map declares.
//!
//! Every case builds a small image and an ld.lld-style map for it, plants a
//! load-delay hazard or none, and runs `hazard-scan` and `hazard-patch` on it
//! the way a build does.

mod common;

use std::path::PathBuf;

use common::*;
use psoxide_hazard::{patch, scan};

/// A row of an ld.lld map: VMA, LMA, size, then the name `depth` columns in.
fn row(vma: u32, lma: u32, size: u32, depth: usize, name: &str) -> String {
    format!(
        "{vma:8x} {lma:8x} {size:8x}     4 {}{name}\n",
        " ".repeat(depth)
    )
}

const MAP_HEAD: &str = "     VMA      LMA     Size Align Out     In      Symbol\n";

/// `jr ra` with a load in its slot: the consumer is the caller's first
/// instruction, which the image cannot show.
fn hazardous() -> Vec<u32> {
    vec![lui("at", 1), jr("ra"), lbu("v0", 0x10, "a0")]
}

/// A function the filler left alone: the load is followed by a nop.
fn clean() -> Vec<u32> {
    vec![lui("at", 1), lbu("v0", 0x10, "a0"), NOP, jr("ra"), NOP]
}

/// Data that decodes as a hazard: only a map that bounds the code keeps
/// the tools from reading it as code.
fn bait() -> Vec<u32> {
    vec![jr("ra"), lbu("v0", 0x10, "a0")]
}

fn run(main: fn(&[String], &mut dyn std::io::Write) -> i32, args: &[String]) -> (i32, String) {
    let mut out = Vec::new();
    let status = main(args, &mut out);
    (status, String::from_utf8(out).unwrap())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn bytes(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

// A raw blob: the chain loader's shape. Code at the start, then rodata.

const BLOB: u32 = 0x801c_8000;
/// Where the code ends and the data begins.
const BLOB_CODE: u32 = 0x100;
const BLOB_LEN: u32 = 0x200;

struct Blob {
    _dir: TempDir,
    bin: PathBuf,
    map: PathBuf,
}

/// A blob with `code` at its start and `bait` in its data, and a map that
/// names `.text` input sections around the code only.
fn blob_of(code: &[u32]) -> Blob {
    let dir = TempDir::new("blob");
    let mut words = vec![NOP; BLOB_LEN as usize / 4];
    words[..code.len()].copy_from_slice(code);
    let data = BLOB_CODE as usize / 4 + 8;
    words[data..data + 2].copy_from_slice(&bait());
    let bin = dir.0.join("loader.bin");
    std::fs::write(&bin, bytes(&words)).unwrap();
    let mut map = String::from(MAP_HEAD);
    map += &row(0, 0, 0, 0, "LOADER_BASE = 0x801C8000");
    map += &row(BLOB, BLOB, BLOB_LEN, 16, ".text");
    map += &row(BLOB, BLOB, 0x20, 8, "/loader.o:(.text.loader_entry)");
    map += &row(BLOB, BLOB, 0x20, 16, "loader_entry");
    map += &row(
        BLOB + 0x20,
        BLOB + 0x20,
        BLOB_CODE - 0x20,
        8,
        "/loader.o:(.text.work)",
    );
    map += &row(BLOB + 0x20, BLOB + 0x20, BLOB_CODE - 0x20, 16, "work");
    map += &row(
        BLOB + BLOB_CODE,
        BLOB + BLOB_CODE,
        0x20,
        8,
        "/loader.o:(.rodata.table)",
    );
    map += &row(BLOB + BLOB_LEN, BLOB + BLOB_LEN, 0, 8, "__bss_start = .");
    let map_path = dir.0.join("loader.map");
    std::fs::write(&map_path, map).unwrap();
    Blob {
        _dir: dir,
        bin,
        map: map_path,
    }
}

fn blob_args(blob: &Blob, extra: &[&str]) -> Vec<String> {
    let mut args = vec![blob.bin.to_str().unwrap().to_string()];
    args.extend(strings(&["--raw", "801c8000"]));
    args.extend(strings(extra));
    args
}

/// The blob's code with a function at its entry and one after it.
fn blob_code(function: &[u32]) -> Vec<u32> {
    let mut code = vec![NOP; 8];
    code[0] = jr("ra");
    code.extend_from_slice(function);
    code
}

#[test]
fn a_clean_raw_blob_scans_clean_with_and_without_a_map() {
    let blob = blob_of(&blob_code(&clean()));
    let map = blob.map.to_str().unwrap().to_string();
    // No map: the whole blob is scanned, bait data included, and the bait
    // is a hazard to the heuristic.
    let (status, out) = run(scan::main, &blob_args(&blob, &[]));
    // With the map only the code is read, and the bait is data.
    let (status_mapped, out_mapped) = run(scan::main, &blob_args(&blob, &["--map", &map]));
    assert_eq!(status_mapped, 0, "{out_mapped}");
    assert!(out_mapped.contains("0 hazards"), "{out_mapped}");
    assert_eq!(status, 1, "{out}");
    let at = BLOB + BLOB_CODE + 0x20;
    assert!(out.contains(&format!("{at:08x}: jr ra")), "{out}");
}

#[test]
fn a_planted_hazard_in_a_raw_blob_is_reported_at_its_load_address() {
    let blob = blob_of(&blob_code(&hazardous()));
    let map = blob.map.to_str().unwrap().to_string();
    let site = BLOB + 0x20 + 4;
    let (status, out) = run(scan::main, &blob_args(&blob, &["--map", &map]));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains(&format!("{site:08x}: jr ra")), "{out}");
    assert!(out.contains("1 hazards in"), "{out}");
    // The data bait is not counted: the map bounds the code.
    assert!(
        !out.contains(&format!("{:08x}", BLOB + BLOB_CODE + 0x20)),
        "{out}"
    );
}

#[test]
fn the_patcher_only_checks_a_raw_blob_and_writes_nothing() {
    let blob = blob_of(&blob_code(&hazardous()));
    let map = blob.map.to_str().unwrap().to_string();
    let before = std::fs::read(&blob.bin).unwrap();
    let (status, out) = run(patch::main, &blob_args(&blob, &["--map", &map]));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("1 hazards in"), "{out}");
    assert!(out.contains("raw blob"), "{out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert_eq!(std::fs::read(&blob.bin).unwrap(), before);
    assert_eq!(
        std::fs::read_dir(blob.bin.parent().unwrap())
            .unwrap()
            .count(),
        2
    );
    // A clean blob passes with exit 0, and is not padded or rewritten.
    let clean = blob_of(&blob_code(&clean()));
    let map = clean.map.to_str().unwrap().to_string();
    let before = std::fs::read(&clean.bin).unwrap();
    let (status, out) = run(patch::main, &blob_args(&clean, &["--map", &map]));
    assert_eq!(status, 0, "{out}");
    assert!(out.contains("0 hazards"), "{out}");
    assert_eq!(std::fs::read(&clean.bin).unwrap(), before);
}

#[test]
fn a_raw_blob_without_a_map_is_scanned_whole_and_never_patched() {
    let blob = blob_of(&blob_code(&hazardous()));
    let (status, out) = run(patch::main, &blob_args(&blob, &[]));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("raw blob"), "{out}");
}

#[test]
fn the_entry_of_a_raw_blob_is_stated_and_checked() {
    let blob = blob_of(&blob_code(&clean()));
    let map = blob.map.to_str().unwrap().to_string();
    let bin = blob.bin.to_str().unwrap().to_string();
    let ok = strings(&[
        &bin, "--raw", "801c8000", "--entry", "801c8020", "--map", &map,
    ]);
    assert_eq!(run(scan::main, &ok).0, 0);
    for args in [
        // Outside the blob.
        strings(&[&bin, "--raw", "801c8000", "--entry", "801c9000"]),
        // Not aligned.
        strings(&[&bin, "--raw", "801c8002"]),
        // Not in RAM.
        strings(&[&bin, "--raw", "00001000"]),
        // An entry needs a load.
        strings(&[&bin, "--entry", "801c8000"]),
        strings(&[&bin, "--raw"]),
    ] {
        let (status, out) = run(scan::main, &args);
        assert_ne!(status, 0, "{args:?}: {out}");
        assert_eq!(
            status,
            if out.contains("outside the blob") {
                1
            } else {
                2
            },
            "{out}"
        );
        let (status, _) = run(patch::main, &args);
        assert_ne!(status, 0, "{args:?}");
    }
}

// A PS-EXE with a boot overlay: the layer's CD builds.

const BASE: u32 = 0x8001_0000;
/// The resident load ends here (`__data_end`); the boot bytes follow.
const DATA_END: u32 = BASE + 0x1000;
/// The overlay's size, and where it runs: past a `.bss` of 0x400.
const BOOT_LEN: u32 = 0x100;
const BOOT_VMA: u32 = DATA_END + 0x400;
const BOOT_CODE: u32 = 0x40;
/// The entry stub, in resident `.text`.
const ENTRY: u32 = BASE + 0x700;

struct Boot {
    _dir: TempDir,
    exe: PathBuf,
    map: PathBuf,
}

/// The resident image of `Image`, the overlay behind it (its code at the
/// start, bait in its data) padded to a sector, and the map of that link.
fn boot_of(code: &[u32], entry: u32) -> Boot {
    let dir = TempDir::new("boot");
    let exe = dir.0.join("game.exe");
    let mut image = Image::new(BASE);
    image.put(ENTRY - BASE, &[NOP, NOP]);
    image.write(&exe);
    let mut overlay = vec![NOP; BOOT_LEN as usize / 4];
    overlay[..code.len()].copy_from_slice(code);
    overlay[BOOT_CODE as usize / 4..BOOT_CODE as usize / 4 + 2].copy_from_slice(&bait());
    let mut file = std::fs::read(&exe).unwrap();
    file.extend(bytes(&overlay));
    // The header claims the whole stored image, a whole number of sectors.
    let payload = 0x1800u32;
    file.resize(0x800 + payload as usize, 0);
    file[0x10..0x14].copy_from_slice(&entry.to_le_bytes());
    file[0x1c..0x20].copy_from_slice(&payload.to_le_bytes());
    std::fs::write(&exe, file).unwrap();
    let tramp = BASE + Image::TRAMPOLINES;
    let mut map = String::from(MAP_HEAD);
    map += &row(BASE, BASE, 0, 8, "__text_start = .");
    map += &row(ENTRY, ENTRY, 8, 8, "/boot.o:(.text.__boot_entry)");
    map += &row(ENTRY, ENTRY, 8, 16, "__boot_entry");
    map += &row(BASE + 0x800, BASE + 0x800, 0, 8, "__text_end = .");
    map += &row(
        tramp,
        tramp,
        8 + 64 * 4,
        8,
        "/fixture.o:(.data.HAZARD_TRAMPOLINES)",
    );
    map += &row(tramp, tramp, 8 + 64 * 4, 16, "HAZARD_TRAMPOLINES");
    map += &row(DATA_END, DATA_END, 0, 8, "__data_end = .");
    map += &row(DATA_END + BOOT_LEN, DATA_END + BOOT_LEN, 0, 0, "__load_end = DEFINED(__boot_start) ? __boot_lma + (__boot_load_end - __boot_start) : __data_end");
    map += &row(BOOT_VMA, DATA_END, BOOT_LEN, 0, ".boot");
    map += &row(BOOT_VMA, DATA_END, 0, 8, "__boot_start = .");
    map += &row(BOOT_VMA, DATA_END, BOOT_CODE, 8, "/boot.o:(.text.FmvPlay)");
    map += &row(BOOT_VMA, DATA_END, BOOT_CODE, 16, "FmvPlay");
    map += &row(
        BOOT_VMA + BOOT_CODE,
        DATA_END + BOOT_CODE,
        8,
        8,
        "/boot.o:(.rodata.fmv_table)",
    );
    map += &row(
        BOOT_VMA + BOOT_LEN,
        DATA_END + BOOT_LEN,
        0,
        8,
        "__boot_load_end = .",
    );
    // ld.lld prints a --defsym'd value as 0; the `.boot` row above has the real one.
    map += &row(0, 0, 0, 0, "__boot_lma = BOOT_LMA");
    map += &row(
        BOOT_VMA + BOOT_LEN,
        BOOT_VMA + BOOT_LEN,
        0,
        8,
        "__bss_start = .",
    );
    let map_path = dir.0.join("game.map");
    std::fs::write(&map_path, map).unwrap();
    Boot {
        _dir: dir,
        exe,
        map: map_path,
    }
}

fn boot_args(boot: &Boot, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        boot.exe.to_str().unwrap().to_string(),
        "--map".to_string(),
        boot.map.to_str().unwrap().to_string(),
    ];
    args.extend(strings(extra));
    args
}

fn word(file: &[u8], at: u32) -> u32 {
    let at = (at - BASE) as usize + 0x800;
    u32::from_le_bytes(file[at..at + 4].try_into().unwrap())
}

#[test]
fn a_clean_boot_overlay_image_scans_and_checks_clean() {
    let boot = boot_of(&clean(), ENTRY);
    let before = std::fs::read(&boot.exe).unwrap();
    for main in [scan::main, patch::main] {
        let (status, out) = run(main, &boot_args(&boot, &[]));
        assert_eq!(status, 0, "{out}");
        assert!(out.contains("0 hazards"), "{out}");
    }
    assert_eq!(std::fs::read(&boot.exe).unwrap(), before);
}

#[test]
fn a_hazard_in_the_overlay_is_reported_at_its_run_address() {
    let boot = boot_of(&hazardous(), ENTRY);
    let (status, out) = run(scan::main, &boot_args(&boot, &[]));
    assert_eq!(status, 1, "{out}");
    // Where it runs, not where it is stored; and not the bait in its data.
    assert!(
        out.contains(&format!("{:08x}: jr ra", BOOT_VMA + 4)),
        "{out}"
    );
    assert!(!out.contains(&format!("{:08x}", DATA_END + 4)), "{out}");
    assert!(
        !out.contains(&format!("{:08x}", BOOT_VMA + BOOT_CODE)),
        "{out}"
    );
    let (status, out) = run(patch::main, &boot_args(&boot, &["--check"]));
    assert_eq!(status, 1, "{out}");
    assert!(
        out.contains(&format!("hazard {:08x}: jr ra", BOOT_VMA + 4)),
        "{out}"
    );
}

#[test]
fn a_hazard_in_the_overlay_is_patched_through_the_resident_array() {
    let boot = boot_of(&hazardous(), ENTRY);
    let before = std::fs::read(&boot.exe).unwrap();
    let (status, out) = run(patch::main, &boot_args(&boot, &[]));
    assert_eq!(status, 0, "{out}");
    let tramp = BASE + Image::TRAMPOLINES + 8;
    assert!(
        out.contains(&format!(
            "patched jr ra at {:08x} -> trampoline {tramp:08x}",
            BOOT_VMA + 4
        )),
        "{out}"
    );
    let after = std::fs::read(&boot.exe).unwrap();
    assert_eq!(after.len(), before.len());
    // Stored where it was: the rerouted jump, in the overlay's bytes.
    assert_eq!(word(&after, DATA_END + 4), j(tramp));
    assert_eq!(word(&after, DATA_END + 8), NOP);
    // The trampoline, in the resident array, carries the load and the return.
    assert_eq!(word(&after, tramp), lbu("v0", 0x10, "a0"));
    assert_eq!(word(&after, tramp + 4), jr("ra"));
    // Nothing else moved: the bait in the overlay's data, the padding behind
    // it (the overlay runs over it) and the resident code.
    let changed: Vec<usize> = (0..after.len())
        .filter(|&i| after[i] != before[i])
        .map(|i| i & !3)
        .collect();
    let allowed = [
        (DATA_END + 4 - BASE) as usize + 0x800,
        (DATA_END + 8 - BASE) as usize + 0x800,
        (tramp - BASE) as usize + 0x800,
        (tramp + 4 - BASE) as usize + 0x800,
        (tramp + 8 - BASE) as usize + 0x800,
        (tramp + 12 - BASE) as usize + 0x800,
    ];
    for at in changed {
        assert!(
            allowed.contains(&at),
            "unexpected change at file offset {at:#x}"
        );
    }
    // The scan agrees and a second run changes nothing.
    let (status, out) = run(scan::main, &boot_args(&boot, &[]));
    assert_eq!(status, 0, "{out}");
    let (status, _) = run(patch::main, &boot_args(&boot, &[]));
    assert_eq!(status, 0);
    assert_eq!(std::fs::read(&boot.exe).unwrap(), after);
}

#[test]
fn the_map_entry_and_stored_size_of_a_boot_image_are_checked() {
    // The initial PC must be the overlay mover, not `_start` or anything else.
    let wrong = boot_of(&clean(), ENTRY + 4);
    let (status, out) = run(scan::main, &boot_args(&wrong, &[]));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("entry point"), "{out}");
    // A file with another stored size than the overlay's end implies.
    let short = boot_of(&clean(), ENTRY);
    let mut file = std::fs::read(&short.exe).unwrap();
    file[0x1c..0x20].copy_from_slice(&0x1000u32.to_le_bytes());
    std::fs::write(&short.exe, file).unwrap();
    let (status, out) = run(scan::main, &boot_args(&short, &[]));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("map does not match"), "{out}");
}

#[test]
fn an_overlay_may_not_sit_over_resident_bytes() {
    // A map whose overlay runs inside the resident payload is refused rather
    // than expanded over its own code.
    let boot = boot_of(&hazardous(), ENTRY);
    let map = std::fs::read_to_string(&boot.map).unwrap();
    let bad = map.replace(&format!("{BOOT_VMA:8x}"), &format!("{:8x}", BASE + 0x400));
    std::fs::write(&boot.map, bad).unwrap();
    let (status, out) = run(scan::main, &boot_args(&boot, &[]));
    assert_ne!(status, 0, "{out}");
    assert!(out.contains("bad --module"), "{out}");
}
