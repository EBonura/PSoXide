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
    fixture_mode(escaped, call_capacity, false)
}
fn fixture_mode(escaped: bool, call_capacity: usize, second: bool) -> Vec<u8> {
    fixture_modes(escaped, call_capacity, second, false)
}
fn fixture_modes(escaped: bool, call_capacity: usize, second: bool, pinned: bool) -> Vec<u8> {
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
    if second {
        let other = 0x80508000;
        parts[3]
            .bytes
            .extend(words(&[jump(other), 0, jump(other + 16), 0]));
        parts[4].address = 0x80010080;
        let metadata_bytes = parts[4].bytes.len() as u32;
        parts[4].bytes.extend(vec![0; 20 + 8 * 12]);
        put32(&mut parts[9].bytes, 7 * 16 + 4, 0x80010080);
        put32(&mut parts[9].bytes, 8 * 16 + 4, 0x80010094);
        for (name, address, size, section, kind) in [
            ("other_intro", other, 16, 12, 2),
            ("other_run", other + 16, 8, 12, 2),
            ("call_other_intro", 0x80010020, 8, 3, 2),
            ("call_other_run", 0x80010028, 8, 3, 2),
            ("other_descriptor", 0x80010080 + metadata_bytes, 20, 4, 1),
            ("other_sites", 0x80010080 + metadata_bytes + 20, 96, 4, 1),
        ] {
            let name_at = parts[10].bytes.len() as u32;
            parts[10].bytes.extend(name.as_bytes());
            parts[10].bytes.push(0);
            let symbol_at = parts[9].bytes.len();
            parts[9].bytes.resize(symbol_at + 16, 0);
            put32(&mut parts[9].bytes, symbol_at, name_at);
            put32(&mut parts[9].bytes, symbol_at + 4, address);
            put32(&mut parts[9].bytes, symbol_at + 8, size);
            parts[9].bytes[symbol_at + 12] = 0x10 | kind;
            put16(&mut parts[9].bytes, symbol_at + 14, section);
        }
        parts[7]
            .bytes
            .extend(words(&[0x80010020, 0x904, 0x80010028, 0xa04]));
        parts.push(part(
            ".other.text",
            1,
            6,
            other,
            words(&[jump(0x80010000), 0, 0x03e00008, 0, 0x03e00008, 0]),
        ));
        parts.push(part(
            ".other.rodata",
            1,
            2,
            other + 24,
            words(&[other, other + 16]),
        ));
        let mut rel = part(
            ".rel.other.rodata",
            9,
            0,
            0,
            words(&[other + 24, 0x902, other + 28, 0xa02]),
        );
        rel.link = 9;
        rel.info = 13;
        rel.stride = 8;
        parts.push(rel);
        let mut rel = part(".rel.other.text", 9, 0, 0, words(&[other, 0x304]));
        rel.link = 9;
        rel.info = 12;
        rel.stride = 8;
        parts.push(rel);
    }
    if escaped {
        let mut rel = part(".rel.data", 9, 0, 0, words(&[parts[4].address, 0x102]));
        rel.link = 9;
        rel.info = 4;
        rel.stride = 8;
        parts.push(rel);
    }
    if pinned {
        put32(&mut parts[3].bytes, 0, 0x3c028041);
        put32(&mut parts[3].bytes, 4, 0x24428018);
        let symbol = parts[9].bytes.len() as u32 / 16;
        let name = parts[10].bytes.len() as u32;
        parts[10].bytes.extend(b"pinned_data\0");
        let mut row = vec![0; 16];
        put32(&mut row, 0, name);
        put32(&mut row, 4, cold + 24);
        put32(&mut row, 8, 8);
        row[12] = 0x11;
        put16(&mut row, 14, 2);
        parts[9].bytes.extend(row);
        parts[7].bytes.extend(words(&[
            0x80010000,
            (symbol << 8) | 5,
            0x80010004,
            (symbol << 8) | 6,
        ]));
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

#[test]
fn composition_two_link_addresses_publish_all_tables_and_hardened_images() {
    use crate::overlay_composition::{compose, finalize, Binding};
    let source = fixture_mode(false, 8, true);
    let elf = Elf::parse(&source).unwrap();
    let resident = [".text", ".data"];
    let first = pack(
        &elf,
        3006,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &[("call_intro", "intro"), ("call_run", "run")],
    )
    .unwrap();
    let second = pack(
        &elf,
        3007,
        ".other.text",
        &[".other.rodata"],
        &resident,
        &[
            ("call_other_intro", "other_intro"),
            ("call_other_run", "other_run"),
        ],
    )
    .unwrap();
    let images = [first, second];
    let bindings = [
        Binding {
            descriptor: "descriptor",
            sites: "sites",
            trap: "trap",
        },
        Binding {
            descriptor: "other_descriptor",
            sites: "other_sites",
            trap: "trap",
        },
    ];
    let initial = compose(&elf, &images, &bindings, &resident, ".ps_exe_head").unwrap();
    assert_eq!(initial.resident.len(), 4096);
    assert_eq!(initial.modules.len(), 2);
    assert_ne!(initial.modules[0].linked, initial.modules[1].linked);
    for site in [8, 16, 32, 40] {
        assert_eq!(word(&initial.resident, 2048 + site), jump(0x80010018));
    }
    let mut second_only = initial.guard.clone();
    let second_module = initial.modules[1];
    let second_at = (second_module.stored - 0x80010000) as usize + 2048;
    put32(
        &mut second_only,
        second_at + 8,
        0x08000000 | (((second_module.linked + 40) >> 2) & 0x03ffffff),
    );
    put32(
        &mut second_only,
        second_at + 40,
        0x08000000 | (((second_module.linked + 16) >> 2) & 0x03ffffff),
    );
    let (one_changed, one_published) =
        finalize(&elf, &images, &bindings, &initial, &second_only, &resident).unwrap();
    assert_eq!(one_changed[0].words, images[0].words);
    assert_ne!(one_changed[1].words, images[1].words);
    assert_eq!(
        word(&one_published.resident, 2048 + 0x80 + 12),
        psx_residency::arena::checksum(&images[0].words)
    );
    let mut hardened = initial.guard.clone();
    for module in &initial.modules {
        let at = (module.stored - 0x80010000) as usize + 2048;
        put32(
            &mut hardened,
            at + 8,
            0x08000000 | (((module.linked + 40) >> 2) & 0x03ffffff),
        );
        put32(
            &mut hardened,
            at + 40,
            0x08000000 | (((module.linked + 16) >> 2) & 0x03ffffff),
        );
    }
    let (packages, published) =
        finalize(&elf, &images, &bindings, &initial, &hardened, &resident).unwrap();
    for (index, package) in packages.iter().enumerate() {
        let descriptor = if index == 0 { 0x80 } else { 0x80 + 116 };
        assert_eq!(
            word(&published.resident, 2048 + descriptor + 12),
            psx_residency::arena::checksum(&package.words)
        );
        assert_eq!(word(&published.resident, 2048 + descriptor + 16), 2);
        assert_ne!(package.words, images[index].words);
        let module = published.modules[index];
        let at = (module.stored - 0x80010000) as usize + 2048;
        assert_eq!(
            &published.guard[at..at + module.bytes as usize],
            words(&package.words[5..5 + package.words[2] as usize])
        );
        assert!(package.words[5 + package.words[2] as usize..].contains(&0x10000028));
        for call in &package.incoming {
            let at = (call.site - 0x80010000) as usize + 2048;
            assert_eq!(word(&published.resident, at), jump(0x80010018));
            assert_eq!(word(&published.guard, at), call.instruction);
        }
    }
    // Every payload is checked before any composed publication is returned.
    let second_at = (initial.modules[1].stored - 0x80010000) as usize + 2048;
    put32(&mut hardened, second_at + 24, 0xdeadbeef);
    assert!(finalize(&elf, &images, &bindings, &initial, &hardened, &resident).is_err());
}

#[test]
fn composition_rejects_shared_metadata_and_duplicate_module_identity() {
    use crate::overlay_composition::{compose, Binding};
    let source = fixture_mode(false, 8, true);
    let elf = Elf::parse(&source).unwrap();
    let resident = [".text", ".data"];
    let first = pack(
        &elf,
        1,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &[("call_intro", "intro"), ("call_run", "run")],
    )
    .unwrap();
    let mut second = pack(
        &elf,
        2,
        ".other.text",
        &[".other.rodata"],
        &resident,
        &[
            ("call_other_intro", "other_intro"),
            ("call_other_run", "other_run"),
        ],
    )
    .unwrap();
    let one = Binding {
        descriptor: "descriptor",
        sites: "sites",
        trap: "trap",
    };
    let two = Binding {
        descriptor: "other_descriptor",
        sites: "other_sites",
        trap: "trap",
    };
    assert!(compose(
        &elf,
        &[first.clone(), second.clone()],
        &[one, one],
        &resident,
        ".ps_exe_head"
    )
    .err()
    .unwrap()
    .contains("tables overlap"));
    second.words[1] = 1;
    assert!(compose(
        &elf,
        &[first, second],
        &[one, two],
        &resident,
        ".ps_exe_head"
    )
    .err()
    .unwrap()
    .contains("identity"));
}

#[test]
fn composition_rejects_cross_cold_dependency_before_publication() {
    let mut source = fixture_mode(false, 8, true);
    let parsed = Elf::parse(&source).unwrap();
    let code_at =
        parsed.section(".other.text").unwrap().1.bytes.as_ptr() as usize - source.as_ptr() as usize;
    let relocation_at = parsed.section(".rel.other.text").unwrap().1.bytes.as_ptr() as usize
        - source.as_ptr() as usize;
    put32(&mut source, code_at, jump(0x80408000));
    put32(&mut source, relocation_at + 4, 0x104);
    let elf = Elf::parse(&source).unwrap();
    assert!(pack(
        &elf,
        2,
        ".other.text",
        &[".other.rodata"],
        &[".text", ".data"],
        &[
            ("call_other_intro", "other_intro"),
            ("call_other_run", "other_run")
        ]
    )
    .is_err());
}

#[test]
fn compact_composition_preserves_eight_byte_capacity_and_final_binding() {
    use crate::overlay_composition::{compose_compact, finalize_compact, CompactBinding};
    let source = fixture_mode(false, 8, true);
    let elf = Elf::parse(&source).unwrap();
    let resident = [".text", ".data"];
    let images = [
        pack(
            &elf,
            0,
            ".cold.text",
            &[".cold.rodata"],
            &resident,
            &[("call_intro", "intro"), ("call_run", "run")],
        )
        .unwrap(),
        pack(
            &elf,
            1,
            ".other.text",
            &[".other.rodata"],
            &resident,
            &[
                ("call_other_intro", "other_intro"),
                ("call_other_run", "other_run"),
            ],
        )
        .unwrap(),
    ];
    let binding = CompactBinding {
        sites: "sites",
        ranges: "descriptor",
        trap: "trap",
    };
    let initial = compose_compact(&elf, &images, binding, &resident, ".ps_exe_head").unwrap();
    assert_eq!(initial.resident.len(), 4096);
    assert_eq!(word(&initial.resident, 2048 + 0x80), 2 << 16);
    assert_eq!(word(&initial.resident, 2048 + 0x84), (2 << 16) | 2);
    for (index, (site, identity, offset)) in [
        (0x80010008, 0, 0),
        (0x80010010, 0, 16),
        (0x80010020, 1, 0),
        (0x80010028, 1, 16),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(word(&initial.resident, 2048 + 0x94 + index * 8), site);
        assert_eq!(
            word(&initial.resident, 2048 + 0x98 + index * 8),
            (1 << 28) | (identity << 20) | offset
        );
        assert_eq!(
            word(&initial.resident, 2048 + (site - 0x80010000) as usize),
            jump(0x80010018)
        );
    }
    let mut hardened = initial.guard.clone();
    let module = initial.modules[1];
    let at = (module.stored - 0x80010000) as usize + 2048;
    put32(
        &mut hardened,
        at + 8,
        0x08000000 | (((module.linked + 40) >> 2) & 0x03ffffff),
    );
    put32(
        &mut hardened,
        at + 40,
        0x08000000 | (((module.linked + 16) >> 2) & 0x03ffffff),
    );
    let (final_images, published) =
        finalize_compact(&elf, &images, binding, &initial, &hardened, &resident).unwrap();
    assert_eq!(final_images[0].words, images[0].words);
    assert_ne!(final_images[1].words, images[1].words);
    assert_eq!(published.resident.len(), initial.resident.len());
    for image in &final_images {
        for call in &image.incoming {
            let at = 2048 + (call.site - 0x80010000) as usize;
            assert_eq!(word(&published.resident, at), jump(0x80010018));
            assert_eq!(word(&published.guard, at), call.instruction);
        }
    }
    let mut invalid = images.clone();
    invalid[1].words[1] = 256;
    assert!(compose_compact(&elf, &invalid, binding, &resident, ".ps_exe_head").is_err());
}

#[test]
fn legacy_pinned_callback_requires_exact_declaration_and_cannot_use_leased_tables() {
    use super::{pack_pinned, PinnedImport};
    let source = fixture(true, 8);
    let elf = Elf::parse(&source).unwrap();
    let resident = [".text", ".data"];
    let calls = [("call_intro", "intro"), ("call_run", "run")];
    assert!(pack(&elf, 14, ".cold.text", &[".cold.rodata"], &resident, &calls).is_err());
    let declaration = PinnedImport {
        owner: "descriptor",
        site_offset: 0,
        kind: 4,
        target_offset: 0,
    };
    let image = pack_pinned(
        &elf,
        14,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &calls,
        &[declaration],
    )
    .unwrap();
    assert_eq!(image.incoming.iter().filter(|c| c.kind == 4).count(), 1);
    assert!(super::publish_image(
        &elf,
        &image,
        &resident,
        "descriptor",
        "sites",
        "trap",
        vec![0; 4096]
    )
    .is_err());
    let wrong = PinnedImport {
        owner: "descriptor",
        site_offset: 0,
        kind: 4,
        target_offset: 4,
    };
    assert!(pack_pinned(
        &elf,
        14,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &calls,
        &[wrong]
    )
    .is_err());
}

#[test]
fn legacy_pinned_pair_checks_exact_original_data_destination() {
    use super::{pack_pinned, PinnedImport};
    let source = fixture_modes(false, 8, false, true);
    let elf = Elf::parse(&source).unwrap();
    let resident = [".text", ".data"];
    let calls = [("call_intro", "intro"), ("call_run", "run")];
    assert!(pack(&elf, 16, ".cold.text", &[".cold.rodata"], &resident, &calls).is_err());
    let declarations = [
        PinnedImport {
            owner: "service",
            site_offset: 0,
            kind: 2,
            target_offset: 24,
        },
        PinnedImport {
            owner: "service",
            site_offset: 4,
            kind: 3,
            target_offset: 24,
        },
    ];
    let image = pack_pinned(
        &elf,
        16,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &calls,
        &declarations,
    )
    .unwrap();
    assert_eq!(image.incoming.iter().filter(|c| c.kind != 1).count(), 2);
    let wrong = [
        PinnedImport {
            owner: "service",
            site_offset: 0,
            kind: 2,
            target_offset: 28,
        },
        PinnedImport {
            owner: "service",
            site_offset: 4,
            kind: 3,
            target_offset: 28,
        },
    ];
    assert!(pack_pinned(
        &elf,
        16,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &calls,
        &wrong
    )
    .is_err());
}

#[test]
fn legacy_compact_finalization_preserves_pinned_pair_and_callback() {
    use super::{pack_pinned, PinnedImport};
    use crate::overlay_composition::{compose_compact, finalize_compact, CompactBinding};
    let source = fixture_modes(true, 8, true, true);
    let elf = Elf::parse(&source).unwrap();
    let resident = [".text", ".data"];
    let declarations = [
        PinnedImport {
            owner: "service",
            site_offset: 0,
            kind: 2,
            target_offset: 24,
        },
        PinnedImport {
            owner: "service",
            site_offset: 4,
            kind: 3,
            target_offset: 24,
        },
        PinnedImport {
            owner: "descriptor",
            site_offset: 0,
            kind: 4,
            target_offset: 0,
        },
    ];
    let image = pack_pinned(
        &elf,
        0,
        ".cold.text",
        &[".cold.rodata"],
        &resident,
        &[("call_intro", "intro"), ("call_run", "run")],
        &declarations,
    )
    .unwrap();
    let binding = CompactBinding {
        sites: "other_sites",
        ranges: "other_descriptor",
        trap: "trap",
    };
    // The other image owns resident wrappers too and must be included in the union.
    let other = pack(
        &elf,
        1,
        ".other.text",
        &[".other.rodata"],
        &resident,
        &[
            ("call_other_intro", "other_intro"),
            ("call_other_run", "other_run"),
        ],
    )
    .unwrap();
    let images = [image, other];
    let initial = compose_compact(&elf, &images, binding, &resident, ".ps_exe_head").unwrap();
    assert_eq!(word(&initial.resident, 2048), 0x3c028001);
    assert_eq!(word(&initial.resident, 2052), 0x24420018);
    assert_eq!(word(&initial.resident, 2048 + 0x80), 0x80010018);
    assert_eq!(word(&initial.guard, 2048), 0x3c028041);
    assert_eq!(word(&initial.guard, 2052), 0x24428018);
    assert_eq!(word(&initial.guard, 2048 + 0x80), 0x80408000);
    let (final_images, published) =
        finalize_compact(&elf, &images, binding, &initial, &initial.guard, &resident).unwrap();
    assert_eq!(published.resident, initial.resident);
    assert_eq!(final_images[0].words, images[0].words);
    assert_eq!(published.guard, initial.guard);
}
