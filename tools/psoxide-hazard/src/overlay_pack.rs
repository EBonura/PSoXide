// SPDX-License-Identifier: GPL-2.0-or-later
//! Centralized same-compilation cold code/data packing for existing arenas.
//! Resident targets stay fixed; references to other cold modules reject.
use crate::overlay_elf::{Elf, Relocation, Symbol};
use psx_residency::overlay;
use std::{collections::BTreeSet, ops::Range};

#[cfg(test)]
#[path = "overlay_pack_fixture.rs"]
mod full_fixture;

/// A complete checked image, including all selected helpers and immutable data.
#[derive(Clone, Debug)]
pub struct PackedImage<'a> {
    /// Original linked address.
    pub linked: u32,
    /// Exclusive executable prefix boundary.
    pub code_end: u32,
    /// Existing five-word package, with image and self fixups.
    pub words: Vec<u32>,
    /// Every same-compilation callable export in the executable prefix.
    pub exports: Vec<Symbol<'a>>,
    /// Linked ranges passed to the existing hazard/module/stack tools.
    pub code_ranges: Vec<Range<u32>>,
    /// Exact typed resident callsites to trap and bind under live admission.
    pub incoming: Vec<IncomingCall<'a>>,
    /// Original sections selected for exclusion from the resident executable.
    pub sections: Vec<u16>,
}

/// One direct import emitted by the same compilation as its cold Rust entry.
#[derive(Clone, Debug)]
pub struct IncomingCall<'a> {
    /// 1 direct J26; 2/3/4 are explicitly declared legacy pinned HI/LO/W32.
    pub kind: u32,
    /// Declared caller/export pair index.
    pub selector: u32,
    /// Stable caller symbol selected by the application.
    pub caller: &'a str,
    /// Stable cold export symbol selected by the application.
    pub export: &'a str,
    /// Resident instruction address.
    pub site: u32,
    /// Entry offset in the installed image.
    pub offset: u32,
    /// Original linked J/JAL used by the guard dependency closure.
    pub instruction: u32,
}

/// A pre-existing pinned binding, outside the ordinary leased callable API.
/// The consumer must retain its original loaded/pinned lifetime discipline.
pub struct PinnedImport<'a> {
    /// Exact initialized resident function/object symbol containing the site.
    pub owner: &'a str,
    /// Word-aligned site offset within that symbol.
    pub site_offset: u32,
    /// Existing compact import kind: HI16=2, LO16=3, W32 callback=4.
    pub kind: u32,
    /// Exact original linked destination offset within this image.
    pub target_offset: u32,
}

fn range(address: u32, bytes: u32) -> Result<Range<u32>, String> {
    Ok(address..address.checked_add(bytes).ok_or("section overflow")?)
}
fn executable(ranges: &[Range<u32>], target: u32) -> bool {
    target & 3 == 0 && ranges.iter().any(|r| r.contains(&target))
}

fn validate_incoming_owner(
    source: u16,
    resident: &[(u16, Range<u32>)],
    cold: &[(u16, Range<u32>)],
    relocations: &[Relocation],
) -> Result<(), String> {
    if !resident.iter().any(|(i, _)| *i == source)
        && relocations
            .iter()
            .any(|r| cold.iter().any(|(i, _)| *i == r.target_section))
    {
        return Err("undeclared allocated section references cold image".into());
    }
    Ok(())
}

/// Select one linked executable output section plus its cold immutable output
/// sections. All are from the same ELF/compiler. `resident` explicitly names
/// the allowed fixed service/data sections; an undeclared target rejects.
pub fn pack<'a>(
    elf: &Elf<'a>,
    id: u32,
    code: &str,
    data: &[&str],
    resident: &[&str],
    calls: &[(&str, &str)],
) -> Result<PackedImage<'a>, String> {
    pack_pinned(elf, id, code, data, resident, calls, &[])
}

/// Legacy-only extraction with explicit pinned bindings. Ordinary `pack` still
/// rejects every address escape; this does not grant a new lifetime guarantee.
pub fn pack_pinned<'a>(
    elf: &Elf<'a>,
    id: u32,
    code: &str,
    data: &[&str],
    resident: &[&str],
    calls: &[(&str, &str)],
    pinned: &[PinnedImport<'_>],
) -> Result<PackedImage<'a>, String> {
    let (code_index, text) = elf.section(code)?;
    for (index, pair) in calls.iter().enumerate() {
        if calls[..index].contains(pair) {
            return Err("duplicate declared call pair".into());
        }
    }
    if text.kind != 1
        || text.flags & 6 != 6
        || text.size == 0
        || text.address & 3 != 0
        || text.size & 3 != 0
    {
        return Err("allocated word-aligned executable section required".into());
    }
    let code_range = range(text.address, text.size)?;
    let mut selected = vec![(code_index, text)];
    let mut image_end = code_range.end;
    for name in data {
        let (index, section) = elf.section(name)?;
        if selected.iter().any(|(i, _)| *i == index)
            || section.kind != 1
            || section.flags & 6 != 2
            || section.address < code_range.end
        {
            return Err("distinct allocated immutable cold data required after code".into());
        }
        let current = range(section.address, section.size)?;
        if selected.iter().any(|(_, s)| {
            s.address < current.end && current.start < s.address.saturating_add(s.size)
        }) {
            return Err("cold sections overlap".into());
        }
        image_end = image_end.max(current.end);
        selected.push((index, section));
    }
    let bytes = image_end
        .checked_sub(text.address)
        .and_then(|n| n.checked_add(3))
        .ok_or("image overflow")?
        & !3;
    let full_bytes = bytes.checked_add(136).ok_or("trampoline overflow")?;
    overlay::placement(text.address, full_bytes, true).map_err(|e| format!("placement: {e:?}"))?;
    let symbols = elf.symbols()?;
    let exports = symbols
        .iter()
        .copied()
        .filter(|s| s.section as usize == code_index && s.kind == 2 && s.bytes != 0)
        .collect::<Vec<_>>();
    if exports.is_empty()
        || exports.iter().any(|s| {
            s.address < code_range.start
                || s.address
                    .checked_add(s.bytes)
                    .is_none_or(|end| end > code_range.end)
        })
    {
        return Err("bounded cold function exports required".into());
    }
    let mut residents = Vec::new();
    for name in resident {
        let (index, section) = elf.section(name)?;
        if selected.iter().any(|(i, _)| *i == index)
            || residents.iter().any(|(i, _)| *i == index)
            || section.flags & 2 == 0
        {
            return Err("distinct allocated resident sections required".into());
        }
        residents.push((index, section));
    }
    let resident_code = residents
        .iter()
        .filter(|(_, section)| section.flags & 4 != 0)
        .map(|(_, section)| range(section.address, section.size))
        .collect::<Result<Vec<_>, _>>()?;
    // Cover the complete executable prefix, including linker thunks and
    // zero-sized assembly helpers, exactly as the module guards do.
    let cold_code = vec![code_range.clone()];
    let mut image = vec![0u32; full_bytes as usize / 4];
    let mut relocations = Vec::new();
    for &(index, section) in &selected {
        if section.bytes.len() != section.size as usize {
            return Err("cold section is not file-backed".into());
        }
        let offset = (section.address - text.address) as usize;
        for (i, &byte) in section.bytes.iter().enumerate() {
            let at = offset + i;
            image[at / 4] |= u32::from(byte) << ((at & 3) * 8);
        }
        relocations.extend(elf.relocations(index)?);
    }
    image[bytes as usize / 4] = 0x48415a54;
    image[bytes as usize / 4 + 1] = 32;
    let selected_ranges = selected
        .iter()
        .map(|(i, s)| Ok((*i as u16, range(s.address, s.size)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let resident_ranges = residents
        .iter()
        .map(|(i, s)| Ok((*i as u16, range(s.address, s.size)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let fixups = references(
        text.address,
        &image,
        &cold_code,
        &resident_code,
        &selected_ranges,
        &resident_ranges,
        &relocations,
    )?;
    let mut incoming = Vec::new();
    let mut incoming_sites = BTreeSet::new();
    for (index, section) in elf.sections().filter(|(_, s)| s.flags & 2 != 0) {
        if selected.iter().any(|(i, _)| *i == index) {
            continue;
        }
        let relocations = elf.relocations(index)?;
        validate_incoming_owner(
            index as u16,
            &resident_ranges,
            &selected_ranges,
            &relocations,
        )?;
        for relocation in &relocations {
            if !selected
                .iter()
                .any(|(i, _)| *i == relocation.target_section as usize)
            {
                continue;
            }
            if relocation.kind != 4 || section.flags & 4 == 0 {
                let mut matches = pinned.iter().filter(|p| {
                    symbols.iter().any(|s| {
                        s.name == p.owner
                            && s.section as usize == index
                            && s.bytes != 0
                            && s.address.checked_add(p.site_offset) == Some(relocation.address)
                            && p.site_offset
                                .checked_add(4)
                                .is_some_and(|end| end <= s.bytes)
                    })
                });
                let declaration = matches
                    .next()
                    .ok_or("cold address escapes a typed direct call")?;
                if matches.next().is_some() {
                    return Err("ambiguous pinned import".into());
                }
                let owner = unique_symbol(&symbols, declaration.owner)?;
                let offset = relocation
                    .address
                    .checked_sub(section.address)
                    .ok_or("pinned site offset")? as usize;
                let word = u32::from_le_bytes(
                    section
                        .bytes
                        .get(offset..offset.checked_add(4).ok_or("pinned site overflow")?)
                        .ok_or("pinned site bounds")?
                        .try_into()
                        .unwrap(),
                );
                let target = text
                    .address
                    .checked_add(declaration.target_offset)
                    .ok_or("pinned target overflow")?;
                let target_section = selected
                    .iter()
                    .find(|(i, _)| *i == relocation.target_section as usize)
                    .ok_or("pinned target owner")?
                    .1;
                let actual = match declaration.kind {
                    2 | 3
                        if owner.kind == 2
                            && section.flags & 4 != 0
                            && relocation.kind == if declaration.kind == 2 { 5 } else { 6 }
                            && target_section.flags & 4 == 0 =>
                    {
                        let resident_words = section
                            .bytes
                            .chunks_exact(4)
                            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                            .collect::<Vec<_>>();
                        hi_lo_address(section.address, &resident_words, relocation, &relocations)?
                    }
                    4 if owner.kind == 1
                        && section.flags & 4 == 0
                        && relocation.kind == 2
                        && executable(&cold_code, target) =>
                    {
                        word
                    }
                    _ => return Err("unsupported pinned import form".into()),
                };
                if actual != target
                    || target < target_section.address
                    || target >= target_section.address.saturating_add(target_section.size)
                    || !incoming_sites.insert(relocation.address)
                {
                    return Err("pinned target binding or duplicate site".into());
                }
                incoming.push(IncomingCall {
                    kind: declaration.kind,
                    selector: u32::MAX,
                    caller: owner.name,
                    export: target_section.name,
                    site: relocation.address,
                    offset: declaration.target_offset,
                    instruction: word,
                });
                continue;
            }
            let offset = relocation
                .address
                .checked_sub(section.address)
                .ok_or("incoming site")? as usize;
            let bytes = section
                .bytes
                .get(offset..offset.checked_add(4).ok_or("incoming overflow")?)
                .ok_or("incoming bounds")?;
            let word = u32::from_le_bytes(bytes.try_into().unwrap());
            if !matches!(word >> 26, 2 | 3) {
                return Err("incoming site is not J/JAL".into());
            }
            let target = (relocation.address.checked_add(4).ok_or("incoming PC")? & 0xf0000000)
                | ((word & 0x03ffffff) << 2);
            let caller = symbols
                .iter()
                .find(|s| {
                    s.kind == 2
                        && s.section as usize == index
                        && s.address <= relocation.address
                        && relocation.address < s.address.saturating_add(s.bytes)
                })
                .ok_or("incoming caller owner")?;
            let mut declarations = calls.iter().enumerate().filter(|(_, (c, e))| {
                *c == caller.name && exports.iter().any(|s| s.name == *e && s.address == target)
            });
            let (selector, (_, export_name)) =
                declarations.next().ok_or("undeclared typed cold call")?;
            if declarations.next().is_some() {
                return Err("ambiguous aliased entries in one caller".into());
            }
            let export = exports
                .iter()
                .find(|s| s.name == *export_name && s.address == target)
                .ok_or("incoming export")?;
            if !incoming_sites.insert(relocation.address) {
                return Err("duplicate incoming callsite".into());
            }
            incoming.push(IncomingCall {
                kind: 1,
                selector: u32::try_from(selector).map_err(|_| "call selector overflow")?,
                caller: caller.name,
                export: export.name,
                site: relocation.address,
                offset: target - text.address,
                instruction: word,
            });
        }
    }
    if pinned.len() != incoming.iter().filter(|c| c.kind != 1).count() {
        return Err("unused or duplicate pinned declaration".into());
    }
    for &(caller, export) in calls {
        if incoming
            .iter()
            .filter(|c| c.caller == caller && c.export == export)
            .count()
            == 0
        {
            return Err("at least one site per declared typed call required".into());
        }
    }
    let mut words = vec![
        overlay::MAGIC,
        id,
        image.len() as u32,
        fixups.len() as u32,
        text.size,
    ];
    words.extend(image);
    words.extend(fixups);
    overlay::Package::parse(&words).map_err(|e| format!("package: {e:?}"))?;
    Ok(PackedImage {
        linked: text.address,
        code_end: text.size,
        words,
        exports,
        code_ranges: vec![code_range, text.address + bytes..text.address + full_bytes],
        incoming,
        sections: selected.iter().map(|(i, _)| *i as u16).collect(),
    })
}

/// Resident runtime bytes plus a dependency-complete module guard image.
pub struct ExecutableImages {
    /// Cold sections are excluded; imports are trapped until live admission.
    pub resident: Vec<u8>,
    /// Identical payload appended with linked direct imports restored.
    pub guard: Vec<u8>,
    /// Appended module image address for the existing --module tools.
    pub module_at: u32,
}

fn unique_symbol<'a>(symbols: &[Symbol<'a>], name: &str) -> Result<Symbol<'a>, String> {
    let mut matches = symbols.iter().filter(|s| s.name == name);
    let value = *matches.next().ok_or("missing metadata symbol")?;
    if matches.next().is_some() {
        return Err("duplicate metadata symbol".into());
    }
    Ok(value)
}
fn put_word(image: &mut [u8], base: u32, address: u32, word: u32) -> Result<(), String> {
    let offset = address
        .checked_sub(base)
        .and_then(|n| n.checked_add(2048))
        .ok_or("resident patch address")? as usize;
    image
        .get_mut(offset..offset.checked_add(4).ok_or("resident patch overflow")?)
        .ok_or("resident patch bounds")?
        .copy_from_slice(&word.to_le_bytes());
    Ok(())
}

/// Extract the complete allocated resident image from the same ELF, patch its
/// fixed-layout metadata in place, and trap every declared cold import. No
/// generated constants are compiled in a second link. All allocated sections
/// must be explicitly selected cold, resident or the header.
pub fn executable_images(
    elf: &Elf<'_>,
    package: &PackedImage<'_>,
    resident: &[&str],
    header: &str,
    descriptor: &str,
    sites: &str,
    trap: &str,
) -> Result<ExecutableImages, String> {
    let image = resident_image(elf, &package.sections, resident, header)?;
    publish_image(elf, package, resident, descriptor, sites, trap, image)
}

/// Extract resident bytes once after excluding the complete cold ownership set.
pub(crate) fn resident_image(
    elf: &Elf<'_>,
    cold_sections: &[u16],
    resident: &[&str],
    header: &str,
) -> Result<Vec<u8>, String> {
    let (header_index, head) = elf.section(header)?;
    if head.bytes.len() != 2048 || !head.bytes.starts_with(b"PS-X EXE") {
        return Err("complete PS-X EXE header required".into());
    }
    let base = u32::from_le_bytes(head.bytes[24..28].try_into().unwrap());
    if base & 3 != 0 || !(0x80010000..0x80200000).contains(&base) {
        return Err("PS1 load base".into());
    }
    let selected = resident
        .iter()
        .map(|name| elf.section(name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut end = base;
    for (index, section) in elf
        .sections()
        .filter(|(_, s)| s.flags & 2 != 0 && s.size != 0)
    {
        if index == header_index || cold_sections.contains(&(index as u16)) {
            continue;
        }
        if !selected.iter().any(|(i, _)| *i == index) {
            return Err("allocated section omitted from resident/cold ownership".into());
        }
        let bounds = range(section.address, section.size)?;
        if bounds.start < base || bounds.end > 0x80200000 {
            return Err("resident PS1 bounds".into());
        }
        if section.kind != 8 {
            if section.kind != 1 || section.bytes.len() != section.size as usize {
                return Err("resident section file backing".into());
            }
            end = end.max(bounds.end);
        }
    }
    let load = end
        .checked_sub(base)
        .and_then(|n| n.checked_add(2047))
        .ok_or("resident size")?
        & !2047;
    let mut image = vec![0u8; 2048 + load as usize];
    image[..2048].copy_from_slice(head.bytes);
    image[28..32].copy_from_slice(&load.to_le_bytes());
    for (_, section) in &selected {
        if section.kind == 8 {
            continue;
        }
        let at = section
            .address
            .checked_sub(base)
            .and_then(|n| n.checked_add(2048))
            .ok_or("resident section offset")? as usize;
        image
            .get_mut(
                at..at
                    .checked_add(section.bytes.len())
                    .ok_or("resident copy overflow")?,
            )
            .ok_or("resident copy bounds")?
            .copy_from_slice(section.bytes);
    }
    Ok(image)
}

pub(crate) fn publish_image(
    elf: &Elf<'_>,
    package: &PackedImage<'_>,
    resident: &[&str],
    descriptor: &str,
    sites: &str,
    trap: &str,
    mut image: Vec<u8>,
) -> Result<ExecutableImages, String> {
    if package.incoming.iter().any(|call| call.kind != 1) {
        return Err("legacy pinned imports cannot use leased publication".into());
    }
    let base = u32::from_le_bytes(
        image
            .get(24..28)
            .ok_or("publish header")?
            .try_into()
            .unwrap(),
    );
    let load = u32::from_le_bytes(image.get(28..32).ok_or("publish load")?.try_into().unwrap());
    if image.len() != 2048 + load as usize {
        return Err("publish resident length".into());
    }
    let selected = resident
        .iter()
        .map(|name| elf.section(name))
        .collect::<Result<Vec<_>, _>>()?;
    let symbols = elf.symbols()?;
    let meta = unique_symbol(&symbols, descriptor)?;
    let table = unique_symbol(&symbols, sites)?;
    let trap = unique_symbol(&symbols, trap)?;
    let writable = |symbol: Symbol<'_>| {
        selected.iter().any(|(i, s)| {
            *i == symbol.section as usize
                && s.kind == 1
                && s.flags & 7 == 3
                && symbol.address >= s.address
                && symbol
                    .address
                    .checked_add(symbol.bytes)
                    .is_some_and(|end| end <= s.address.saturating_add(s.size))
        })
    };
    if !writable(meta)
        || !writable(table)
        || meta.bytes != 20
        || table.bytes % 12 != 0
        || table.bytes as usize / 12 < package.incoming.len()
    {
        return Err("initialized descriptor/site capacity or layout".into());
    }
    if meta.address < table.address.saturating_add(table.bytes)
        && table.address < meta.address.saturating_add(meta.bytes)
    {
        return Err("metadata tables overlap".into());
    }
    if trap.kind != 2
        || trap.bytes == 0
        || !selected
            .iter()
            .any(|(i, s)| *i == trap.section as usize && s.flags & 4 != 0)
    {
        return Err("resident executable trap required".into());
    }
    for (i, word) in [
        package.words.len() as u32,
        package.words[1],
        package.linked,
        psx_residency::arena::checksum(&package.words),
        package.incoming.len() as u32,
    ]
    .into_iter()
    .enumerate()
    {
        put_word(&mut image, base, meta.address + i as u32 * 4, word)?;
    }
    for i in (0..table.bytes).step_by(4) {
        put_word(&mut image, base, table.address + i, 0)?;
    }
    for (i, call) in package.incoming.iter().enumerate() {
        for (field, word) in [call.selector, call.site, call.offset]
            .into_iter()
            .enumerate()
        {
            put_word(
                &mut image,
                base,
                table.address + i as u32 * 12 + field as u32 * 4,
                word,
            )?;
        }
        let trapped = overlay::direct(call.instruction, 1, trap.address, call.site)
            .map_err(|e| format!("trap: {e:?}"))?;
        put_word(&mut image, base, call.site, trapped)?;
    }
    let mut guard = image.clone();
    for call in &package.incoming {
        put_word(&mut guard, base, call.site, call.instruction)?;
    }
    for word in &package.words[5..5 + package.words[2] as usize] {
        guard.extend_from_slice(&word.to_le_bytes());
    }
    Ok(ExecutableImages {
        resident: image,
        guard,
        module_at: base.checked_add(load).ok_or("module staging")?,
    })
}

/// Finalize bytes written by the existing module-aware hazard patcher. It
/// rescans all executable J26 sites, including new hazard trampolines, before
/// refreshing checksums and same-final-link tables. Relocation-bearing HI/LO
/// moves and rerouted typed import sites reject rather than use stale offsets.
pub fn finalize_guard<'a>(
    elf: &Elf<'a>,
    original: &PackedImage<'a>,
    initial: &ExecutableImages,
    hardened: &[u8],
    resident: &[&str],
    descriptor: &str,
    sites: &str,
    trap: &str,
) -> Result<(PackedImage<'a>, ExecutableImages), String> {
    let result = finalize_payload(elf, original, initial, hardened, resident)?;
    let published = publish_image(
        elf,
        &result,
        resident,
        descriptor,
        sites,
        trap,
        hardened[..initial.resident.len()].to_vec(),
    )?;
    Ok((result, published))
}

/// Shared hardened-payload validation independent of resident table layout.
pub(crate) fn finalize_payload<'a>(
    elf: &Elf<'a>,
    original: &PackedImage<'a>,
    initial: &ExecutableImages,
    hardened: &[u8],
    resident: &[&str],
) -> Result<PackedImage<'a>, String> {
    let (checked, rest) =
        overlay::Package::parse(&original.words).map_err(|e| format!("original package: {e:?}"))?;
    if !rest.is_empty()
        || checked.code_end != original.code_end
        || checked.image.len().checked_mul(4).is_none()
    {
        return Err("original package shape".into());
    }
    let initial_bytes = initial.guard.len();
    if hardened.len() < initial_bytes
        || hardened[initial_bytes..].iter().any(|&v| v != 0)
        || hardened.get(..2048) != initial.guard.get(..2048)
    {
        return Err("hardened guard layout/header changed".into());
    }
    let base = u32::from_le_bytes(hardened[24..28].try_into().unwrap());
    for call in &original.incoming {
        let at = call
            .site
            .checked_sub(base)
            .and_then(|v| v.checked_add(2048))
            .ok_or("hardened import site")? as usize;
        let word = u32::from_le_bytes(
            hardened
                .get(at..at + 4)
                .ok_or("hardened import bounds")?
                .try_into()
                .unwrap(),
        );
        if word != call.instruction {
            return Err("typed import rerouted by hardener; binding trace required".into());
        }
    }
    let at = initial.resident.len();
    let final_image = hardened[at..initial_bytes]
        .chunks_exact(4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .collect::<Vec<_>>();
    let old_image = &original.words[5..5 + original.words[2] as usize];
    let final_fixups = hardened_fixups(
        original,
        old_image,
        &final_image,
        resident
            .iter()
            .map(|name| elf.section(name))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .filter(|(_, s)| s.flags & 4 != 0)
            .map(|(_, s)| range(s.address, s.size))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
    let mut result = original.clone();
    result.words = vec![
        overlay::MAGIC,
        original.words[1],
        final_image.len() as u32,
        final_fixups.len() as u32,
        original.code_end,
    ];
    result.words.extend(final_image);
    result.words.extend(final_fixups);
    overlay::Package::parse(&result.words).map_err(|e| format!("final package: {e:?}"))?;
    Ok(result)
}

fn hardened_fixups(
    original: &PackedImage<'_>,
    before: &[u32],
    after: &[u32],
    resident_code: Vec<Range<u32>>,
) -> Result<Vec<u32>, String> {
    if before.len() != after.len() || before.len() < 34 {
        return Err("hardened image capacity changed".into());
    }
    let tramp = before.len() - 34;
    if after[tramp..tramp + 2] != [0x48415a54, 32] {
        return Err("hardened trampoline header changed".into());
    }
    let old_fixups = &original.words[5 + before.len()..];
    let mut fixups = BTreeSet::new();
    for &fixup in old_fixups {
        let kind = fixup >> 28;
        let site = (fixup & 0x0fffffff) as usize / 4;
        if kind == 1 {
            continue;
        }
        if before[site] != after[site] && kind != 4 {
            return Err("relocation-bearing instruction moved by hardener".into());
        }
        if kind == 4
            && before[site] != after[site]
            && !executable(&original.code_ranges, after[site])
        {
            return Err("patched table target is not cold code".into());
        }
        fixups.insert(fixup);
    }
    for (i, (&old, &new)) in before.iter().zip(after).enumerate() {
        let address = original.linked + i as u32 * 4;
        if old != new
            && !executable(&original.code_ranges, address)
            && !old_fixups.contains(&(0x40000000 | (i as u32 * 4)))
        {
            return Err("hardener changed immutable cold data".into());
        }
    }
    for code in &original.code_ranges {
        for pc in (code.start..code.end).step_by(4) {
            let site = pc - original.linked;
            let word = after[site as usize / 4];
            if matches!(word >> 26, 2 | 3) {
                let target = (pc.checked_add(4).ok_or("hardened PC")? & 0xf0000000)
                    | ((word & 0x03ffffff) << 2);
                if executable(&original.code_ranges, target) {
                    fixups.insert(0x10000000 | site);
                } else if !executable(&resident_code, target) {
                    return Err("hardened jump target undeclared".into());
                }
            }
        }
    }
    Ok(fixups.into_iter().collect())
}

fn references(
    linked: u32,
    image: &[u32],
    cold_code: &[Range<u32>],
    resident_code: &[Range<u32>],
    cold: &[(u16, Range<u32>)],
    resident: &[(u16, Range<u32>)],
    relocations: &[Relocation],
) -> Result<Vec<u32>, String> {
    let mut fixups = BTreeSet::new();
    for range in cold_code {
        for pc in (range.start..range.end).step_by(4) {
            let offset = pc.checked_sub(linked).ok_or("code offset")?;
            let word = *image.get(offset as usize / 4).ok_or("code bounds")?;
            if matches!(word >> 26, 2 | 3) {
                let next = pc.checked_add(4).ok_or("PC overflow")?;
                let target = (next & 0xf0000000) | ((word & 0x03ffffff) << 2);
                if executable(cold_code, target) {
                    fixups.insert(0x10000000 | offset);
                } else if !executable(resident_code, target) {
                    return Err("undeclared or non-executable direct target".into());
                }
            }
        }
    }
    for relocation in relocations {
        let offset = relocation
            .address
            .checked_sub(linked)
            .ok_or("relocation offset")?;
        if offset & 3 != 0 {
            return Err("unaligned relocation".into());
        }
        let word = *image.get(offset as usize / 4).ok_or("relocation bounds")?;
        let self_target = cold.iter().find(|(i, _)| *i == relocation.target_section);
        let target = self_target
            .or_else(|| {
                resident
                    .iter()
                    .find(|(i, _)| *i == relocation.target_section)
            })
            .ok_or("undeclared cold dependency")?;
        if relocation.target_address < target.1.start || relocation.target_address > target.1.end {
            return Err("relocation symbol outside declared owner".into());
        }
        let kind = match relocation.kind {
            2 => {
                if word < target.1.start || word > target.1.end {
                    return Err("word target bounds".into());
                }
                4
            }
            5 => {
                let address =
                    hi_lo_address(linked, image, relocation, relocations).map_err(|e| {
                        format!(
                            "{e} at {:#010x} target {:#010x}",
                            relocation.address, relocation.target_address
                        )
                    })?;
                if address < target.1.start || address > target.1.end {
                    return Err("HI16 target bounds".into());
                }
                if self_target.is_some() && word & 0xffff != overlay::window(linked) >> 16 {
                    return Err("self HI16 window".into());
                }
                2
            }
            6 => {
                let address =
                    hi_lo_address(linked, image, relocation, relocations).map_err(|e| {
                        format!(
                            "{e} at {:#010x} target {:#010x}",
                            relocation.address, relocation.target_address
                        )
                    })?;
                if address < target.1.start || address > target.1.end {
                    return Err("LO16 target bounds".into());
                }
                3
            }
            4 => continue,
            10 if self_target.is_some() => {
                let target = relocation
                    .address
                    .checked_add(4)
                    .and_then(|pc| pc.checked_add_signed((word as u16 as i16 as i32) << 2))
                    .ok_or("PC16 overflow")?;
                if !executable(cold_code, target) {
                    return Err("PC16 target is not cold code".into());
                }
                continue;
            }
            10 => return Err("cross-cold or resident PC16 unsupported".into()),
            _ => return Err("unsupported MIPS relocation".into()),
        };
        if self_target.is_some() {
            fixups.insert((kind << 28) | offset);
        }
    }
    Ok(fixups.into_iter().collect())
}

fn hi_lo_address(
    linked: u32,
    image: &[u32],
    relocation: &Relocation,
    all: &[Relocation],
) -> Result<u32, String> {
    let at = |r: &Relocation| {
        r.address
            .checked_sub(linked)
            .and_then(|offset| image.get(offset as usize / 4))
            .copied()
            .ok_or_else(|| "HI/LO site bounds".to_string())
    };
    let current = at(relocation)?;
    let compatible = |r: &&Relocation| {
        r.target_section == relocation.target_section
            && r.target_address == relocation.target_address
    };
    let (high, low) = if relocation.kind == 5 {
        if current >> 26 != 15 {
            return Err("HI16 is not LUI".into());
        }
        let low = all
            .iter()
            .filter(compatible)
            .filter(|r| r.kind == 6 && r.address > relocation.address)
            .filter(|r| base_reaches_low(linked, image, relocation, r))
            .min_by_key(|r| r.address)
            .ok_or("HI16 missing matching LO16 register")?;
        (current, at(low)?)
    } else {
        let high = all
            .iter()
            .filter(compatible)
            .filter(|r| r.kind == 5 && r.address < relocation.address)
            .filter(|r| {
                at(r).is_ok_and(|word| word >> 26 == 15)
                    && base_reaches_low(linked, image, r, relocation)
            })
            .max_by_key(|r| r.address)
            .ok_or("LO16 missing matching HI16 register")?;
        (at(high)?, current)
    };
    Ok(((high & 0xffff) << 16).wrapping_add(low as u16 as i16 as i32 as u32))
}

// A compiler jump-table address can add its runtime index before the LO16
// load. Prove the LUI contribution survives that straight-line affine path;
// never pair unrelated registers merely because their relocation symbols match.
fn base_reaches_low(linked: u32, image: &[u32], high: &Relocation, low: &Relocation) -> bool {
    let Some(start) = high.address.checked_sub(linked).map(|v| v as usize / 4) else {
        return false;
    };
    let Some(end) = low.address.checked_sub(linked).map(|v| v as usize / 4) else {
        return false;
    };
    let (Some(&lui), Some(&lo)) = (image.get(start), image.get(end)) else {
        return false;
    };
    if end <= start || lui >> 26 != 15 {
        return false;
    }
    let origin = ((lui >> 16) & 31) as usize;
    let target = ((lo >> 21) & 31) as usize;
    if origin == 0 || target == 0 {
        return false;
    }
    // Preserve the existing direct-base form; the additional indexed form is
    // bounded and rejects any control transfer or lost register contribution.
    if origin == target {
        return true;
    }
    if end - start > 16 {
        return false;
    }
    // 0 independent, 1 exactly one contribution, 2 unsupported/multiple.
    // Unsupported dependence must never become an independent index again.
    let mut carries = [0u8; 32];
    carries[origin] = 1;
    let mut special = [0u8; 2];
    for &instruction in &image[start + 1..end] {
        let op = instruction >> 26;
        let rs = ((instruction >> 21) & 31) as usize;
        let rt = ((instruction >> 16) & 31) as usize;
        let rd = ((instruction >> 11) & 31) as usize;
        if (1..=7).contains(&op) {
            return false;
        }
        if op == 0 {
            let function = instruction & 63;
            if function == 8 || function == 9 {
                return false;
            }
            let contribution = if function == 33 {
                if carries[rs] == 0 {
                    carries[rt]
                } else if carries[rt] == 0 {
                    carries[rs]
                } else {
                    2
                }
            } else if function == 37 && (rs == 0 || rt == 0) {
                carries[rs].max(carries[rt])
            } else if function == 16 || function == 18 {
                special[(function == 18) as usize]
            } else {
                let source = if function <= 7 {
                    carries[rt]
                } else {
                    carries[rs].max(carries[rt])
                };
                if source == 0 {
                    0
                } else {
                    2
                }
            };
            if function == 17 || function == 19 {
                special[(function == 19) as usize] = carries[rs];
            }
            if [24, 25, 26, 27].contains(&function) {
                special = [if carries[rs].max(carries[rt]) == 0 {
                    0
                } else {
                    2
                }; 2];
            }
            if ![17, 19, 24, 25, 26, 27].contains(&function) && rd != 0 {
                carries[rd] = contribution
            }
        } else if (8..=15).contains(&op) || (32..=38).contains(&op) {
            if rt != 0 {
                carries[rt] = if op == 15 || carries[rs] == 0 { 0 } else { 2 }
            }
        } else if !(40..=46).contains(&op) {
            return false;
        }
    }
    carries[target] == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_resident_jump_table_retains_its_proven_lui_base() {
        // Exact instruction shape from the CS title consumer's 0x8040a834
        // LUI through 0x8040a850 LO16 load. No retail content is involved.
        let base = 0x8040a834;
        let image = [
            0x3c02800a, 0x32560040, 0x32530010, 0x32512001, 0x325e4008, 0x00010880, 0x00220821,
            0x8c211cd0,
        ];
        let relocations = [
            Relocation {
                address: base,
                kind: 5,
                target_section: 3,
                target_address: 0x8009ac90,
            },
            Relocation {
                address: base + 28,
                kind: 6,
                target_section: 3,
                target_address: 0x8009ac90,
            },
        ];
        for relocation in &relocations {
            assert_eq!(
                hi_lo_address(base, &image, relocation, &relocations).unwrap(),
                0x800a1cd0
            );
        }
        assert!(references(
            base,
            &image,
            std::slice::from_ref(&(base..base + 32)),
            &[],
            &[(1, base..base + 32)],
            &[(3, 0x8009ac90..0x800a8728)],
            &relocations
        )
        .unwrap()
        .is_empty());
        for (index, word) in [(6, 0x00230821), (2, 0x24020009), (2, 0x10000000)] {
            let mut invalid = image;
            invalid[index] = word;
            assert!(hi_lo_address(base, &invalid, &relocations[0], &relocations).is_err());
        }
        let mut triple = image;
        triple[5] = 0x00421821; // addu r3,r2,r2
        triple[6] = 0x00620821; // addu r1,r3,r2
        assert!(hi_lo_address(base, &triple, &relocations[0], &relocations).is_err());
    }
    fn hardening_fixture() -> PackedImage<'static> {
        let base = 0x80408000;
        let jump = 0x08000000 | ((base >> 2) & 0x03ffffff);
        let mut image = vec![jump, 0, base, 0x48415a54, 32];
        image.extend([0; 32]);
        let mut words = vec![overlay::MAGIC, 1, image.len() as u32, 2, 8];
        words.extend(image);
        words.extend([0x10000000, 0x40000008]);
        PackedImage {
            linked: base,
            code_end: 8,
            words,
            exports: Vec::new(),
            code_ranges: vec![base..base + 8, base + 20..base + 148],
            incoming: Vec::new(),
            sections: vec![1, 2],
        }
    }
    #[test]
    fn hardened_trampoline_jumps_and_jump_tables_get_final_self_fixups() {
        let original = hardening_fixture();
        let before = &original.words[5..42];
        let mut after = before.to_vec();
        after[0] = 0x08000000 | (((original.linked + 20) >> 2) & 0x03ffffff);
        after[2] = original.linked + 20;
        after[5] = before[0];
        let fixups = hardened_fixups(&original, before, &after, Vec::new()).unwrap();
        assert_eq!(fixups, [0x10000000, 0x10000014, 0x40000008]);
    }
    #[test]
    fn moved_relocation_bearing_instruction_fails_publication() {
        let mut original = hardening_fixture();
        original.words[5] = 0x3c028041;
        original.words[6] = 0x24428000;
        original.words[3] = 2;
        original.words.truncate(42);
        original.words.extend([0x20000000, 0x30000004]);
        let before = &original.words[5..42];
        let mut after = before.to_vec();
        after[1] = 0;
        after[5] = before[1];
        assert!(hardened_fixups(&original, before, &after, Vec::new())
            .unwrap_err()
            .contains("relocation-bearing"));
    }
    #[test]
    fn resident_addend_crossing_symbol_signed_window_uses_actual_pair() {
        let base = 0x80408000;
        let image = [0x3c028014, 0x24428000];
        let relocations = [
            Relocation {
                address: base,
                kind: 5,
                target_section: 3,
                target_address: 0x80137ffc,
            },
            Relocation {
                address: base + 4,
                kind: 6,
                target_section: 3,
                target_address: 0x80137ffc,
            },
        ];
        assert!(references(
            base,
            &image,
            std::slice::from_ref(&(base..base + 8)),
            &[],
            &[(1, base..base + 8)],
            &[(3, 0x80010000..0x80180000)],
            &relocations
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn omitted_incoming_section_cannot_hide_a_cold_pointer() {
        let relocation = Relocation {
            address: 0x80020000,
            kind: 2,
            target_section: 1,
            target_address: 0x80408000,
        };
        assert!(validate_incoming_owner(
            7,
            &[(3, 0x80010000..0x80018000)],
            &[(1, 0x80408000..0x80408040)],
            &[relocation]
        )
        .is_err());
    }
    #[test]
    fn fixed_resident_symbol_beyond_initial_section_window_is_unchanged() {
        let base = 0x80408000;
        let image = [0x3c028013, 0x24421234];
        let relocations = [
            Relocation {
                address: base,
                kind: 5,
                target_section: 3,
                target_address: 0x80131234,
            },
            Relocation {
                address: base + 4,
                kind: 6,
                target_section: 3,
                target_address: 0x80131234,
            },
        ];
        assert!(references(
            base,
            &image,
            std::slice::from_ref(&(base..base + 8)),
            &[],
            &[(1, base..base + 8)],
            &[(3, 0x80010000..0x80180000)],
            &relocations
        )
        .unwrap()
        .is_empty());
    }
    #[test]
    fn all_helpers_and_jump_table_self_references_are_retained() {
        let base = 0x80408000;
        let image = [0x0c004000, 0, 0x03e00008, 0, base, base + 8];
        let code = [base..base + 8, base + 8..base + 16];
        let cold = [(1, base..base + 16), (2, base + 16..base + 24)];
        let relocations = [
            Relocation {
                address: base + 16,
                kind: 2,
                target_section: 1,
                target_address: base,
            },
            Relocation {
                address: base + 20,
                kind: 2,
                target_section: 1,
                target_address: base + 8,
            },
        ];
        let fixups = references(
            base,
            &image,
            &code,
            std::slice::from_ref(&(0x80010000..0x80010020)),
            &cold,
            &[(3, 0x80010000..0x80010020)],
            &relocations,
        )
        .unwrap();
        assert_eq!(fixups, [0x40000010, 0x40000014]);
        let moved = fixups
            .iter()
            .map(|&fixup| {
                overlay::relocate(
                    image[(fixup & 0x0fffffff) as usize / 4],
                    fixup >> 28,
                    base,
                    0x80108000,
                    fixup & 0x0fffffff,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(moved, [0x80108000, 0x80108008]);
    }
    #[test]
    fn cross_cold_and_non_executable_direct_targets_reject() {
        let base = 0x80408000;
        assert!(references(
            base,
            &[0x0c004000],
            std::slice::from_ref(&(base..base + 4)),
            &[],
            &[(1, base..base + 4)],
            &[],
            &[]
        )
        .is_err());
        let relocation = Relocation {
            address: base,
            kind: 2,
            target_section: 9,
            target_address: 0x80418000,
        };
        assert!(references(
            base,
            &[0x80418000],
            &[],
            &[],
            &[(1, base..base + 4)],
            &[],
            &[relocation]
        )
        .is_err());
        let relocation = Relocation {
            address: base,
            kind: 10,
            target_section: 3,
            target_address: 0x80010000,
        };
        assert!(references(
            base,
            &[0x10000000],
            std::slice::from_ref(&(base..base + 4)),
            &[],
            &[(1, base..base + 4)],
            &[(3, 0x80010000..0x80010020)],
            &[relocation]
        )
        .is_err());
    }
}
