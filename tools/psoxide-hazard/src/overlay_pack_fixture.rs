// SPDX-License-Identifier: GPL-2.0-or-later
//! End-to-end synthetic ELF fixture, with two typed imports and a cold table.
use super::*;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn jump(target: u32) -> u32 {
    0x0c000000 | ((target >> 2) & 0x03ffffff)
}
struct Part {
    name: &'static str,
    kind: u32,
    flags: u32,
    address: u32,
    bytes: Vec<u8>,
    link: u32,
    info: u32,
    stride: u32,
}
fn part(name: &'static str, kind: u32, flags: u32, address: u32, bytes: Vec<u8>) -> Part {
    Part {
        name,
        kind,
        flags,
        address,
        bytes,
        link: 0,
        info: 0,
        stride: 0,
    }
}

fn fixture(escaped: bool, call_capacity: usize) -> Vec<u8> {
    let cold = 0x80408000;
    let mut strings = vec![0];
    let mut symbols = vec![0; 16];
    for (name, address, size, section, kind) in [
        ("intro", cold, 16, 1, 2),
        ("run", cold + 16, 8, 1, 2),
        ("service", 0x80010000, 8, 3, 2),
        ("call_intro", 0x80010008, 8, 3, 2),
        ("call_run", 0x80010010, 8, 3, 2),
        ("trap", 0x80010018, 8, 3, 2),
        ("descriptor", 0x80010020, 20, 4, 1),
        ("sites", 0x80010034, call_capacity as u32 * 12, 4, 1),
    ] {
        let offset = strings.len() as u32;
        strings.extend(name.as_bytes());
        strings.push(0);
        let at = symbols.len();
        symbols.resize(at + 16, 0);
        put32(&mut symbols, at, offset);
        put32(&mut symbols, at + 4, address);
        put32(&mut symbols, at + 8, size);
        symbols[at + 12] = 0x10 | kind;
        put16(&mut symbols, at + 14, section);
    }
    let mut header = vec![0; 2048];
    header[..8].copy_from_slice(b"PS-X EXE");
    put32(&mut header, 16, 0x80010000);
    put32(&mut header, 24, 0x80010000);
    put32(&mut header, 28, 2048);
    put32(&mut header, 48, 0x801fff00);
    let mut data = vec![0; 20 + call_capacity * 12];
    if escaped {
        put32(&mut data, 0, cold);
    }
    let mut parts = vec![
        part("", 0, 0, 0, Vec::new()),
        part(
            ".cold.text",
            1,
            6,
            cold,
            words(&[jump(0x80010000), 0, 0x03e00008, 0, 0x03e00008, 0]),
        ),
        part(".cold.rodata", 1, 2, cold + 24, words(&[cold, cold + 16])),
        part(
            ".text",
            1,
            6,
            0x80010000,
            words(&[
                0x03e00008,
                0,
                jump(cold),
                0,
                jump(cold + 16),
                0,
                0x03e00008,
                0,
            ]),
        ),
        part(".data", 1, 3, 0x80010020, data),
        part(".ps_exe_head", 1, 2, 0x8000f800, header),
        part(
            ".rel.cold.rodata",
            9,
            0,
            0,
            words(&[cold + 24, 0x102, cold + 28, 0x202]),
        ),
        part(
            ".rel.text",
            9,
            0,
            0,
            words(&[0x80010008, 0x104, 0x80010010, 0x204]),
        ),
        part(".rel.cold.text", 9, 0, 0, words(&[cold, 0x304])),
        part(".symtab", 2, 0, 0, symbols),
        part(".strtab", 3, 0, 0, strings),
        part(".shstrtab", 3, 0, 0, Vec::new()),
    ];
    for (index, target) in [(6, 2), (7, 3), (8, 1)] {
        parts[index].link = 9;
        parts[index].info = target;
        parts[index].stride = 8;
    }
    parts[9].link = 10;
    parts[9].stride = 16;
    if escaped {
        let mut rel = part(".rel.data", 9, 0, 0, words(&[0x80010020, 0x102]));
        rel.link = 9;
        rel.info = 4;
        rel.stride = 8;
        parts.push(rel);
    }
    let mut names = vec![0];
    let mut name_offsets = Vec::new();
    for part in &parts {
        name_offsets.push(names.len() as u32);
        names.extend(part.name.as_bytes());
        names.push(0);
    }
    parts[11].bytes = names;
    let mut bytes = vec![0; 52];
    bytes[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
    put16(&mut bytes, 16, 2);
    put16(&mut bytes, 18, 8);
    put32(&mut bytes, 20, 1);
    put16(&mut bytes, 40, 52);
    put16(&mut bytes, 46, 40);
    put16(&mut bytes, 48, parts.len() as u16);
    put16(&mut bytes, 50, 11);
    let mut rows = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        while bytes.len() & 3 != 0 {
            bytes.push(0);
        }
        rows.push([
            name_offsets[index],
            part.kind,
            part.flags,
            part.address,
            bytes.len() as u32,
            part.bytes.len() as u32,
            part.link,
            part.info,
            4,
            part.stride,
        ]);
        bytes.extend(&part.bytes);
    }
    while bytes.len() & 3 != 0 {
        bytes.push(0);
    }
    let offset = bytes.len() as u32;
    put32(&mut bytes, 32, offset);
    for row in rows {
        bytes.extend(words(&row));
    }
    bytes
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

#[test]
fn final_link_header_metadata_payload_and_hardened_publication_stay_bound() {
    let source = fixture(false, 8);
    let elf = Elf::parse(&source).unwrap();
    let calls = [("call_intro", "intro"), ("call_run", "run")];
    let original = pack(
        &elf,
        3006,
        ".cold.text",
        &[".cold.rodata"],
        &[".text", ".data"],
        &calls,
    )
    .unwrap();
    assert_eq!(original.exports.len(), 2);
    assert_eq!(original.incoming.len(), 2);
    let initial = executable_images(
        &elf,
        &original,
        &[".text", ".data"],
        ".ps_exe_head",
        "descriptor",
        "sites",
        "trap",
    )
    .unwrap();
    assert_eq!(word(&initial.resident, 28), 2048);
    assert_eq!(
        word(&initial.resident, 2048 + 32),
        original.words.len() as u32
    );
    assert_eq!(word(&initial.resident, 2048 + 48), 2);
    assert_eq!(word(&initial.resident, 2048 + 52), 0);
    assert_eq!(word(&initial.resident, 2048 + 64), 1);
    assert_eq!(word(&initial.resident, 2048 + 8), jump(0x80010018));
    let mut hardened = initial.guard.clone();
    let at = initial.resident.len();
    put32(
        &mut hardened,
        at + 8,
        0x08000000 | (((original.linked + 40) >> 2) & 0x03ffffff),
    );
    put32(
        &mut hardened,
        at + 40,
        0x08000000 | (((original.linked + 16) >> 2) & 0x03ffffff),
    );
    let (final_package, final_images) = finalize_guard(
        &elf,
        &original,
        &initial,
        &hardened,
        &[".text", ".data"],
        "descriptor",
        "sites",
        "trap",
    )
    .unwrap();
    let final_checksum = psx_residency::arena::checksum(&final_package.words);
    assert_ne!(
        final_checksum,
        psx_residency::arena::checksum(&original.words)
    );
    assert_eq!(word(&final_images.resident, 2048 + 44), final_checksum);
    assert_eq!(word(&final_images.resident, 2048 + 8), jump(0x80010018));
    assert_eq!(word(&final_images.guard, 2048 + 8), jump(original.linked));
    assert_eq!(
        final_images.guard[at..],
        words(&final_package.words[5..5 + final_package.words[2] as usize])
    );
    assert!(final_package.words[5 + final_package.words[2] as usize..].contains(&0x10000028));
}

#[test]
fn omitted_pointer_owner_and_final_table_overflow_reject() {
    let source = fixture(true, 8);
    let elf = Elf::parse(&source).unwrap();
    assert!(pack(
        &elf,
        3006,
        ".cold.text",
        &[".cold.rodata"],
        &[".text"],
        &[("call_intro", "intro"), ("call_run", "run")]
    )
    .unwrap_err()
    .contains("undeclared allocated"));
    let source = fixture(false, 1);
    let elf = Elf::parse(&source).unwrap();
    let image = pack(
        &elf,
        3006,
        ".cold.text",
        &[".cold.rodata"],
        &[".text", ".data"],
        &[("call_intro", "intro"), ("call_run", "run")],
    )
    .unwrap();
    assert!(executable_images(
        &elf,
        &image,
        &[".text", ".data"],
        ".ps_exe_head",
        "descriptor",
        "sites",
        "trap"
    )
    .is_err());
}

#[test]
fn one_declared_pair_emits_every_distinct_callsite() {
    let mut source = fixture(false, 8);
    let headers = word(&source, 32) as usize;
    let text = word(&source, headers + 3 * 40 + 16) as usize;
    let rel = word(&source, headers + 7 * 40 + 16) as usize;
    let symbols = word(&source, headers + 9 * 40 + 16) as usize;
    put32(&mut source, text + 16, jump(0x80408000));
    put32(&mut source, rel + 12, 0x104);
    put32(&mut source, symbols + 4 * 16 + 8, 16);
    put32(&mut source, symbols + 5 * 16 + 8, 0);
    let elf = Elf::parse(&source).unwrap();
    let packed = pack(
        &elf,
        3006,
        ".cold.text",
        &[".cold.rodata"],
        &[".text", ".data"],
        &[("call_intro", "intro")],
    )
    .unwrap();
    assert_eq!(packed.incoming.len(), 2);
    assert_eq!(
        packed
            .incoming
            .iter()
            .map(|c| (c.selector, c.site))
            .collect::<Vec<_>>(),
        [(0, 0x80010008), (0, 0x80010010)]
    );
}
