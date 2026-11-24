// SPDX-License-Identifier: GPL-2.0-or-later
//! `psx-exe`: write a PS-X EXE from a psoxide.ld link's ELF, every section at its load address.
//!
//! ld.lld's `--oformat=binary` places every section at its run address, so a program with
//! sections stored in one place and run from another (psoxide-core.ld's core image,
//! psoxide-boot.ld's `.boot`) links an ELF and has its EXE written here instead. The bytes are the
//! ones `--oformat=binary` writes for a program without such sections: every allocated section
//! with contents, at its load address (its program header's `p_paddr`) relative to the
//! `.ps_exe_head` section's, which is the start of the file. As with `--oformat=binary` the file
//! ends with the last stored byte; `hazard-patch` pads it to the header's whole sectors.
//!
//! A program on the scene core with fixed-address overlays (psoxide-overlay.ld) has `.overlay.N`
//! sections: they are never in the EXE. `--overlay` writes one as its own image file, its bytes as
//! they run from its address, and prints the `load=` option of its scene manifest line.
//!
//! ```text
//! psx-exe path/to/game.elf path/to/game.exe
//! psx-exe --overlay .overlay.1 path/to/game.elf path/to/menu.ovl     (prints load=0x...)
//! ```

use crate::overlay_elf::Elf;

/// Usage text.
pub const USAGE: &str = "\
Write a PS-X EXE from a psoxide.ld link's ELF, every section at its load address, or one of its
fixed-address overlays (psoxide-overlay.ld) as its own image file.

    psx-exe path/to/game.elf path/to/game.exe
    psx-exe --overlay .overlay.N path/to/game.elf path/to/overlay.ovl

With --overlay it prints the overlay's scene manifest option, `load=0x........`.
";

/// Names of psoxide-overlay.ld's output sections: never stored in the EXE.
pub const OVERLAY_PREFIX: &str = ".overlay.";

/// The header section psoxide.ld writes first.
const HEADER: &str = ".ps_exe_head";
/// Bytes of the header.
const HEADER_BYTES: u32 = 0x800;
/// `SHF_ALLOC`.
const ALLOC: u32 = 2;
/// `SHT_NOBITS`.
const NOBITS: u32 = 8;

/// The EXE's bytes for an ELF linked with psoxide.ld.
///
/// Every allocated section with contents is stored, but the fixed-address overlays
/// ([`OVERLAY_PREFIX`], see [`overlay_from_elf`]). Refuses an ELF without the header section, a
/// section stored below the header, two sections stored over the same bytes, and a header whose
/// destination or stored size does not describe the bytes written (the size reaches the file's
/// end rounded up to whole 800h-byte sectors).
///
/// # Errors
///
/// Why the ELF cannot be written as a PS-X EXE (not a MIPS ELF, or segments the EXE cannot hold).
pub fn from_elf(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let elf = Elf::parse(bytes)?;
    let (_, head) = elf.section(HEADER)?;
    let base = head.load_address;
    let mut stored: Vec<(u32, &[u8], &str)> = elf
        .sections()
        .map(|(_, s)| s)
        .filter(|s| s.flags & ALLOC != 0 && s.kind != NOBITS && !s.bytes.is_empty())
        .filter(|s| !s.name.starts_with(OVERLAY_PREFIX))
        .map(|s| (s.load_address, s.bytes, s.name))
        .collect();
    stored.sort_by_key(|&(at, ..)| at);
    let mut end = base;
    for &(at, data, name) in &stored {
        if at < base {
            return Err(format!(
                "{name} is stored at {at:08x}, below the header at {base:08x}"
            ));
        }
        if at < end {
            return Err(format!(
                "{name} is stored at {at:08x}, over the bytes of the section before it (to {end:08x})"
            ));
        }
        end = at
            .checked_add(data.len() as u32)
            .ok_or_else(|| format!("{name} runs past the address space"))?;
    }
    let mut image = vec![0u8; (end - base) as usize];
    for &(at, data, _) in &stored {
        let from = (at - base) as usize;
        image[from..from + data.len()].copy_from_slice(data);
    }
    check_header(&image, base)?;
    Ok(image)
}

/// A fixed-address overlay of an ELF linked with psoxide-overlay.ld: its run address and its
/// bytes as they run there (the image file the core loads to that address).
///
/// Refuses a name without the [`OVERLAY_PREFIX`], a section that is missing, empty, not
/// allocated or without contents, and one whose address is not word aligned (the core's
/// checksum runs over words; the disc writer pads the file to whole sectors).
///
/// # Errors
///
/// Why the ELF holds no overlay of that name, or one the overlay file cannot carry.
pub fn overlay_from_elf(bytes: &[u8], name: &str) -> Result<(u32, Vec<u8>), String> {
    if !name.starts_with(OVERLAY_PREFIX) {
        return Err(format!(
            "{name} is not an overlay section ({OVERLAY_PREFIX}N)"
        ));
    }
    let elf = Elf::parse(bytes)?;
    let (_, section) = elf.section(name)?;
    if section.flags & ALLOC == 0 || section.kind == NOBITS || section.bytes.is_empty() {
        return Err(format!("{name} holds no code or data"));
    }
    if section.address % 4 != 0 {
        return Err(format!(
            "{name} runs at {:08x}, not word aligned",
            section.address
        ));
    }
    Ok((section.address, section.bytes.to_vec()))
}

fn word(image: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(image[at..at + 4].try_into().expect("a 4-byte slice"))
}

/// The header describes the image: magic, destination right after it, stored size reaching the
/// end of the bytes by under one sector.
fn check_header(image: &[u8], base: u32) -> Result<(), String> {
    if image.len() < HEADER_BYTES as usize || !image.starts_with(b"PS-X EXE") {
        return Err(format!("{HEADER} is not a PS-X EXE header"));
    }
    let destination = word(image, 0x18);
    if destination != base + HEADER_BYTES {
        return Err(format!(
            "the header's destination {destination:08x} is not the first byte after it ({:08x})",
            base + HEADER_BYTES
        ));
    }
    let size = word(image, 0x1c) as usize;
    let stored = image.len() - HEADER_BYTES as usize;
    if size < stored
        || size - stored >= HEADER_BYTES as usize
        || !size.is_multiple_of(HEADER_BYTES as usize)
    {
        return Err(format!(
            "the header's stored size {size:#x} does not cover the {stored:#x} bytes after it in \
             whole sectors"
        ));
    }
    Ok(())
}

/// `psx-exe ELF EXE`, or `psx-exe --overlay SECTION ELF FILE`.
pub fn main(args: &[String], out: &mut dyn std::io::Write) -> i32 {
    if let [flag, section, elf, file] = args {
        if flag == "--overlay" {
            let result = std::fs::read(elf)
                .map_err(|e| format!("{elf}: {e}"))
                .and_then(|bytes| {
                    overlay_from_elf(&bytes, section).map_err(|e| format!("{elf}: {e}"))
                })
                .and_then(|(address, image)| {
                    std::fs::write(file, image)
                        .map(|()| address)
                        .map_err(|e| format!("{file}: {e}"))
                });
            return match result {
                Ok(address) => {
                    let _ = writeln!(out, "load={address:#010x}");
                    0
                }
                Err(message) => {
                    eprintln!("psx-exe: {message}");
                    1
                }
            };
        }
    }
    let [elf, exe] = args else {
        eprint!("{USAGE}");
        return 2;
    };
    let result = std::fs::read(elf)
        .map_err(|e| format!("{elf}: {e}"))
        .and_then(|bytes| from_elf(&bytes).map_err(|e| format!("{elf}: {e}")))
        .and_then(|image| std::fs::write(exe, image).map_err(|e| format!("{exe}: {e}")));
    match result {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("psx-exe: {message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One section of a fixture ELF, with its own program header when it has contents.
    struct Part {
        name: &'static str,
        kind: u32,
        address: u32,
        load: u32,
        size: u32,
        bytes: Vec<u8>,
    }

    fn progbits(name: &'static str, address: u32, load: u32, bytes: Vec<u8>) -> Part {
        Part {
            name,
            kind: 1,
            address,
            load,
            size: bytes.len() as u32,
            bytes,
        }
    }

    fn nobits(name: &'static str, address: u32, size: u32) -> Part {
        Part {
            name,
            kind: NOBITS,
            address,
            load: address,
            size,
            bytes: Vec::new(),
        }
    }

    fn header(pc: u32, destination: u32, size: u32) -> Vec<u8> {
        let mut head = vec![0u8; HEADER_BYTES as usize];
        head[..8].copy_from_slice(b"PS-X EXE");
        head[0x10..0x14].copy_from_slice(&pc.to_le_bytes());
        head[0x18..0x1c].copy_from_slice(&destination.to_le_bytes());
        head[0x1c..0x20].copy_from_slice(&size.to_le_bytes());
        head
    }

    fn fill(word: u32, words: usize) -> Vec<u8> {
        std::iter::repeat_n(word.to_le_bytes(), words)
            .flatten()
            .collect()
    }

    /// An ELF32 little-endian MIPS executable laid out as ld.lld writes one: a section header
    /// table, and one `PT_LOAD` per section with contents, `p_vaddr` its run address and
    /// `p_paddr` its load address.
    fn elf(parts: &[Part]) -> Vec<u8> {
        let mut names = vec![0u8];
        let mut name_at = Vec::new();
        for part in parts {
            name_at.push(names.len() as u32);
            names.extend_from_slice(part.name.as_bytes());
            names.push(0);
        }
        let shstrtab_name = names.len() as u32;
        names.extend_from_slice(b".shstrtab\0");
        let loads: Vec<&Part> = parts.iter().filter(|p| p.kind != NOBITS).collect();
        let phoff = 52usize;
        let mut offset = phoff + 32 * loads.len();
        let mut data_at = Vec::new();
        for part in parts {
            data_at.push(offset as u32);
            offset += part.bytes.len();
        }
        let names_at = offset;
        offset += names.len();
        let shoff = offset.next_multiple_of(4);
        let count = parts.len() + 2;
        let mut out = vec![0u8; shoff + 40 * count];
        out[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        let put16 = |out: &mut Vec<u8>, at: usize, v: u16| {
            out[at..at + 2].copy_from_slice(&v.to_le_bytes())
        };
        let put32 = |out: &mut Vec<u8>, at: usize, v: u32| {
            out[at..at + 4].copy_from_slice(&v.to_le_bytes())
        };
        put16(&mut out, 16, 2); // ET_EXEC
        put16(&mut out, 18, 8); // EM_MIPS
        put32(&mut out, 28, phoff as u32);
        put32(&mut out, 32, shoff as u32);
        put16(&mut out, 42, 32);
        put16(&mut out, 44, loads.len() as u16);
        put16(&mut out, 46, 40);
        put16(&mut out, 48, count as u16);
        put16(&mut out, 50, (count - 1) as u16);
        let mut ph = phoff;
        for (i, part) in parts.iter().enumerate() {
            out[data_at[i] as usize..data_at[i] as usize + part.bytes.len()]
                .copy_from_slice(&part.bytes);
            if part.kind != NOBITS {
                put32(&mut out, ph, 1);
                put32(&mut out, ph + 4, data_at[i]);
                put32(&mut out, ph + 8, part.address);
                put32(&mut out, ph + 12, part.load);
                put32(&mut out, ph + 16, part.size);
                put32(&mut out, ph + 20, part.size);
                ph += 32;
            }
            let sh = shoff + 40 * (i + 1);
            put32(&mut out, sh, name_at[i]);
            put32(&mut out, sh + 4, part.kind);
            put32(&mut out, sh + 8, ALLOC);
            put32(&mut out, sh + 12, part.address);
            put32(&mut out, sh + 16, data_at[i]);
            put32(&mut out, sh + 20, part.size);
        }
        out[names_at..names_at + names.len()].copy_from_slice(&names);
        let sh = shoff + 40 * (count - 1);
        put32(&mut out, sh, shstrtab_name);
        put32(&mut out, sh + 4, 3);
        put32(&mut out, sh + 16, names_at as u32);
        put32(&mut out, sh + 20, names.len() as u32);
        out
    }

    const HEAD: u32 = 0x8000_f800;
    const TEXT: u32 = 0x8001_0000;
    const DATA: u32 = 0x8001_0010;
    const DATA_END: u32 = 0x8001_0028;
    const BOOT_RUN: u32 = 0x8001_0070;

    /// .text, .data, .bss over the stored bytes, and .boot run above .bss but stored after .data.
    fn boot_program(size: u32) -> Vec<Part> {
        vec![
            progbits(HEADER, HEAD, HEAD, header(TEXT + 8, TEXT, size)),
            progbits(".text", TEXT, TEXT, fill(0x1111_1111, 4)),
            progbits(".data", DATA, DATA, fill(0x2222_2222, 6)),
            nobits(".bss", DATA_END, 0x40),
            progbits(".boot", BOOT_RUN, DATA_END, fill(0x3333_3333, 3)),
        ]
    }

    #[test]
    fn boot_is_written_after_data_at_its_load_address() {
        let image = from_elf(&elf(&boot_program(0x800))).unwrap();
        let at = |address: u32| (address - HEAD) as usize;
        assert_eq!(
            image.len(),
            at(DATA_END) + 12,
            "the file ends with .boot's last byte"
        );
        assert_eq!(&image[at(TEXT)..at(TEXT) + 16], &fill(0x1111_1111, 4)[..]);
        assert_eq!(&image[at(DATA)..at(DATA_END)], &fill(0x2222_2222, 6)[..]);
        assert_eq!(&image[at(DATA_END)..], &fill(0x3333_3333, 3)[..]);
        assert_eq!(word(&image, 0x18), TEXT);
        assert_eq!(
            word(&image, 0x1c),
            0x800,
            "one sector covers the stored bytes"
        );
    }

    /// psoxide-overlay.ld's overlays run above .bss and are stored nowhere in the EXE; each one is
    /// written on its own, at its run address.
    #[test]
    fn an_overlay_is_left_out_of_the_exe_and_written_on_its_own() {
        const OVERLAY: u32 = 0x8010_0000;
        let mut parts = boot_program(0x800);
        parts.push(progbits(
            ".overlay.1",
            OVERLAY,
            OVERLAY & 0x1fff_ffff,
            fill(0x4444_4444, 5),
        ));
        parts.push(progbits(
            ".overlay.2",
            OVERLAY,
            (OVERLAY & 0x1fff_ffff) + 20,
            fill(0x5555_5555, 2),
        ));
        let bytes = elf(&parts);
        let without = {
            let mut plain = boot_program(0x800);
            plain.truncate(5);
            from_elf(&elf(&plain)).unwrap()
        };
        assert_eq!(
            from_elf(&bytes).unwrap(),
            without,
            "the EXE holds no overlay byte"
        );
        assert_eq!(
            overlay_from_elf(&bytes, ".overlay.1").unwrap(),
            (OVERLAY, fill(0x4444_4444, 5))
        );
        assert_eq!(
            overlay_from_elf(&bytes, ".overlay.2").unwrap(),
            (OVERLAY, fill(0x5555_5555, 2))
        );
        assert!(overlay_from_elf(&bytes, ".overlay.3")
            .unwrap_err()
            .contains(".overlay.3"));
        assert!(overlay_from_elf(&bytes, ".boot")
            .unwrap_err()
            .contains("not an overlay"));
        // The CLI prints the manifest option.
        let dir = std::env::temp_dir().join(format!("psx-exe-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (elf_path, file) = (dir.join("game.elf"), dir.join("menu.ovl"));
        std::fs::write(&elf_path, &bytes).unwrap();
        let args: Vec<String> = ["--overlay", ".overlay.1"]
            .iter()
            .map(|s| s.to_string())
            .chain(
                [elf_path, file.clone()]
                    .iter()
                    .map(|p| p.display().to_string()),
            )
            .collect();
        let mut printed = Vec::new();
        assert_eq!(main(&args, &mut printed), 0);
        assert_eq!(String::from_utf8(printed).unwrap(), "load=0x80100000\n");
        assert_eq!(std::fs::read(&file).unwrap(), fill(0x4444_4444, 5));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_program_without_relocated_sections_matches_its_run_layout() {
        let mut parts = boot_program(0x800);
        parts.pop();
        let image = from_elf(&elf(&parts)).unwrap();
        assert_eq!(image.len(), (DATA_END - HEAD) as usize);
    }

    #[test]
    fn a_header_size_that_does_not_cover_the_bytes_is_refused() {
        let mut parts = boot_program(0x800);
        // 0x800 + 0x40 + 12 stored bytes after a header claiming none.
        parts[0] = progbits(HEADER, HEAD, HEAD, header(TEXT, TEXT, 0));
        assert!(from_elf(&elf(&parts)).unwrap_err().contains("stored size"));
        parts[0] = progbits(HEADER, HEAD, HEAD, header(TEXT, TEXT, 0x1000));
        assert!(from_elf(&elf(&parts)).unwrap_err().contains("stored size"));
    }

    #[test]
    fn two_sections_stored_over_the_same_bytes_are_refused() {
        let mut parts = boot_program(0x800);
        parts[4].load = DATA_END - 4;
        assert!(from_elf(&elf(&parts))
            .unwrap_err()
            .contains("over the bytes"));
    }

    #[test]
    fn a_wrong_destination_is_refused() {
        let mut parts = boot_program(0x800);
        parts[0] = progbits(HEADER, HEAD, HEAD, header(TEXT, TEXT + 0x800, 0x800));
        assert!(from_elf(&elf(&parts)).unwrap_err().contains("destination"));
    }

    /// overlay-compose's resident image stores .boot where psx-exe does and sizes the header
    /// from the stored bytes.
    #[test]
    fn overlay_compose_stores_boot_at_its_load_address() {
        let bytes = elf(&boot_program(0x800));
        let parsed = Elf::parse(&bytes).unwrap();
        let resident = [".text", ".data", ".bss", ".boot"];
        let image = crate::overlay_pack::resident_image(&parsed, &[], &resident, HEADER).unwrap();
        let written = from_elf(&bytes).unwrap();
        assert_eq!(&image[..written.len()], &written[..]);
        assert_eq!(
            image.len(),
            0x1000,
            "one sector of stored bytes after the header"
        );
        assert_eq!(word(&image, 0x1c), 0x800);
    }
}
