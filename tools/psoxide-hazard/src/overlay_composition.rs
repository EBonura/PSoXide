// SPDX-License-Identifier: GPL-2.0-or-later
//! One final resident image and guard closure for independent cold packages.
use crate::overlay_elf::Elf;
use crate::overlay_pack::{self, ExecutableImages, PackedImage};
use std::collections::BTreeSet;

/// Fixed resident publication symbols for one separately admitted image.
#[derive(Clone, Copy)]
pub struct Binding<'a> {
    pub descriptor: &'a str,
    pub sites: &'a str,
    pub trap: &'a str,
}
/// Where the unchanged module-aware guards find each linked cold image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardModule {
    pub linked: u32,
    pub stored: u32,
    pub bytes: u32,
}
pub struct ComposedExecutable {
    pub resident: Vec<u8>,
    pub guard: Vec<u8>,
    pub modules: Vec<GuardModule>,
}
fn word(image: &mut [u8], base: u32, site: u32, value: u32) -> Result<(), String> {
    let at = site
        .checked_sub(base)
        .and_then(|v| v.checked_add(2048))
        .ok_or("composed site")? as usize;
    image
        .get_mut(at..at.checked_add(4).ok_or("composed site overflow")?)
        .ok_or("composed site bounds")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn validate(
    elf: &Elf<'_>,
    images: &[PackedImage<'_>],
    bindings: &[Binding<'_>],
) -> Result<(), String> {
    if images.is_empty() || images.len() != bindings.len() {
        return Err("composition image/binding count".into());
    }
    validate_images(images)?;
    let symbols = elf.symbols()?;
    let mut tables = Vec::new();
    for binding in bindings {
        for name in [binding.descriptor, binding.sites] {
            let mut found = symbols.iter().filter(|s| s.name == name);
            let symbol = found.next().ok_or("composition metadata symbol")?;
            if found.next().is_some() {
                return Err("ambiguous composition metadata symbol".into());
            }
            let end = symbol
                .address
                .checked_add(symbol.bytes)
                .ok_or("composition metadata range")?;
            if symbol.address & 3 != 0
                || tables
                    .iter()
                    .any(|&(start, stop)| symbol.address < stop && start < end)
            {
                return Err("composition metadata tables overlap or misalign".into());
            }
            tables.push((symbol.address, end));
        }
    }
    Ok(())
}
fn validate_images(images: &[PackedImage<'_>]) -> Result<(), String> {
    if images.is_empty() {
        return Err("composition needs images".into());
    }
    let mut ids = BTreeSet::new();
    let mut sections = BTreeSet::new();
    let mut sites = BTreeSet::new();
    let mut code = Vec::new();
    for image in images {
        let (checked, rest) = psx_residency::overlay::Package::parse(&image.words)
            .map_err(|e| format!("composition package: {e:?}"))?;
        if !rest.is_empty() || checked.code_end != image.code_end {
            return Err("composition package shape".into());
        }
        if !ids.insert(image.words[1]) {
            return Err("duplicate composition identity".into());
        }
        let end = image
            .linked
            .checked_add(image.words[2].checked_mul(4).ok_or("composition size")?)
            .ok_or("composition address")?;
        if code
            .iter()
            .any(|&(start, stop)| image.linked < stop && start < end)
        {
            return Err("overlapping linked cold images".into());
        }
        code.push((image.linked, end));
        for &section in &image.sections {
            if !sections.insert(section) {
                return Err("cold section owned by multiple images".into());
            }
        }
        for call in &image.incoming {
            if !sites.insert(call.site) {
                return Err("typed call owned by multiple images".into());
            }
        }
    }
    Ok(())
}
fn append_guard(
    images: &[PackedImage<'_>],
    resident: Vec<u8>,
) -> Result<ComposedExecutable, String> {
    let base = u32::from_le_bytes(
        resident
            .get(24..28)
            .ok_or("composition header")?
            .try_into()
            .unwrap(),
    );
    let mut guard = resident.clone();
    for image in images {
        for call in &image.incoming {
            word(&mut guard, base, call.site, call.instruction)?
        }
    }
    let mut modules = Vec::new();
    for image in images {
        let stored = base
            .checked_add(
                u32::try_from(guard.len().checked_sub(2048).ok_or("composition length")?)
                    .map_err(|_| "composition length")?,
            )
            .ok_or("composition staging")?;
        let bytes = image.words[2]
            .checked_mul(4)
            .ok_or("composition image length")?;
        modules.push(GuardModule {
            linked: image.linked,
            stored,
            bytes,
        });
        for value in &image.words[5..5 + image.words[2] as usize] {
            guard.extend_from_slice(&value.to_le_bytes())
        }
    }
    Ok(ComposedExecutable {
        resident,
        guard,
        modules,
    })
}
/// Exclude the union of cold sections and publish every fixed table in one EXE.
/// Each package must already pass the ordinary single-image admission checks.
pub fn compose(
    elf: &Elf<'_>,
    images: &[PackedImage<'_>],
    bindings: &[Binding<'_>],
    resident: &[&str],
    header: &str,
) -> Result<ComposedExecutable, String> {
    validate(elf, images, bindings)?;
    let mut ownership = images[0].clone();
    ownership.sections = images
        .iter()
        .flat_map(|i| i.sections.iter().copied())
        .collect();
    let first = bindings[0];
    let mut output = overlay_pack::executable_images(
        elf,
        &ownership,
        resident,
        header,
        first.descriptor,
        first.sites,
        first.trap,
    )?
    .resident;
    for (image, binding) in images.iter().zip(bindings).skip(1) {
        output = overlay_pack::publish_image(
            elf,
            image,
            resident,
            binding.descriptor,
            binding.sites,
            binding.trap,
            output,
        )?
        .resident;
    }
    append_guard(images, output)
}
/// Finalize all images against one hardened composite, then publish together.
/// No partial output is returned when any image/import/fixup fails validation.
pub fn finalize<'a>(
    elf: &Elf<'a>,
    images: &[PackedImage<'a>],
    bindings: &[Binding<'_>],
    initial: &ComposedExecutable,
    hardened: &[u8],
    resident: &[&str],
) -> Result<(Vec<PackedImage<'a>>, ComposedExecutable), String> {
    validate(elf, images, bindings)?;
    let expected = append_guard(images, initial.resident.clone())?;
    if initial.guard != expected.guard || initial.modules != expected.modules {
        return Err("composition initial layout binding".into());
    }
    if initial.modules.len() != images.len()
        || hardened.len() < initial.guard.len()
        || hardened[initial.guard.len()..].iter().any(|&b| b != 0)
        || hardened.get(..2048) != initial.guard.get(..2048)
    {
        return Err("composed hardened layout/header changed".into());
    }
    let mut final_images = Vec::new();
    let resident_len = initial.resident.len();
    let base = u32::from_le_bytes(initial.resident[24..28].try_into().unwrap());
    for ((image, binding), module) in images.iter().zip(bindings).zip(&initial.modules) {
        if module.linked != image.linked
            || module.bytes
                != image.words[2]
                    .checked_mul(4)
                    .ok_or("composition image bytes")?
        {
            return Err("composition module binding".into());
        }
        let at = module
            .stored
            .checked_sub(base)
            .and_then(|v| v.checked_add(2048))
            .ok_or("composition module offset")? as usize;
        let end = at
            .checked_add(module.bytes as usize)
            .ok_or("composition module end")?;
        let source = initial
            .guard
            .get(at..end)
            .ok_or("composition source bounds")?;
        let final_bytes = hardened.get(at..end).ok_or("composition hardened bounds")?;
        let mut single_guard = initial.guard[..resident_len].to_vec();
        single_guard.extend_from_slice(source);
        let one = ExecutableImages {
            resident: initial.resident.clone(),
            guard: single_guard,
            module_at: base
                .checked_add(
                    u32::try_from(resident_len - 2048)
                        .map_err(|_| "composition resident length")?,
                )
                .ok_or("composition load")?,
        };
        let mut single_hardened = hardened[..resident_len].to_vec();
        single_hardened.extend_from_slice(final_bytes);
        let (result, _) = overlay_pack::finalize_guard(
            elf,
            image,
            &one,
            &single_hardened,
            resident,
            binding.descriptor,
            binding.sites,
            binding.trap,
        )?;
        final_images.push(result);
    }
    let mut output = hardened[..resident_len].to_vec();
    for (image, binding) in final_images.iter().zip(bindings) {
        output = overlay_pack::publish_image(
            elf,
            image,
            resident,
            binding.descriptor,
            binding.sites,
            binding.trap,
            output,
        )?
        .resident;
    }
    Ok((final_images.clone(), append_guard(&final_images, output)?))
}

/// Existing fixed import layout: (site, kind:4 | identity:8 | offset:20),
/// with one (first:16 | count:16) range per identity. Only direct J26 sites are
/// supported; selectors remain bound to their exact compiled sites/exports.
#[derive(Clone, Copy)]
pub struct CompactBinding<'a> {
    pub sites: &'a str,
    pub ranges: &'a str,
    pub trap: &'a str,
}
fn compact_publish(
    elf: &Elf<'_>,
    images: &[PackedImage<'_>],
    binding: CompactBinding<'_>,
    resident: &[&str],
    mut output: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let symbols = elf.symbols()?;
    let lookup = |name: &str| -> Result<crate::overlay_elf::Symbol<'_>, String> {
        let mut candidates = symbols.iter().filter(|s| s.name == name);
        let symbol = *candidates.next().ok_or("compact symbol")?;
        if candidates.next().is_some() {
            return Err("ambiguous compact symbol".into());
        }
        Ok(symbol)
    };
    let sites = lookup(binding.sites)?;
    let ranges = lookup(binding.ranges)?;
    let trap = lookup(binding.trap)?;
    let selected = resident
        .iter()
        .map(|name| elf.section(name))
        .collect::<Result<Vec<_>, _>>()?;
    let writable = |symbol: crate::overlay_elf::Symbol<'_>| {
        selected.iter().any(|(index, s)| {
            *index == symbol.section as usize
                && s.kind == 1
                && s.flags & 7 == 3
                && symbol.address & 3 == 0
                && symbol.address >= s.address
                && symbol
                    .address
                    .checked_add(symbol.bytes)
                    .is_some_and(|end| end <= s.address.saturating_add(s.size))
        })
    };
    let count = images.iter().map(|i| i.incoming.len()).sum::<usize>();
    if !writable(sites)
        || !writable(ranges)
        || sites.bytes % 8 != 0
        || ranges.bytes % 4 != 0
        || count > sites.bytes as usize / 8
        || count > u16::MAX as usize
    {
        return Err("compact initialized table capacity/layout".into());
    }
    if sites.address < ranges.address.saturating_add(ranges.bytes)
        && ranges.address < sites.address.saturating_add(sites.bytes)
    {
        return Err("compact tables overlap".into());
    }
    if trap.kind != 2
        || trap.bytes == 0
        || !selected
            .iter()
            .any(|(index, s)| *index == trap.section as usize && s.flags & 4 != 0)
    {
        return Err("compact resident trap".into());
    }
    let base = u32::from_le_bytes(
        output
            .get(24..28)
            .ok_or("compact header")?
            .try_into()
            .unwrap(),
    );
    for offset in (0..sites.bytes).step_by(4) {
        word(&mut output, base, sites.address + offset, u32::MAX)?
    }
    for offset in (0..ranges.bytes).step_by(4) {
        word(&mut output, base, ranges.address + offset, 0)?
    }
    let mut cursor = 0u32;
    let mut order = images.iter().collect::<Vec<_>>();
    order.sort_by_key(|i| i.words[1]);
    for image in order {
        let identity = image.words[1];
        if identity >= 256
            || identity >= ranges.bytes / 4
            || image.incoming.len() > u16::MAX as usize
        {
            return Err("compact identity/range cannot represent image".into());
        }
        for call in &image.incoming {
            if call.site >= sites.address && call.site < sites.address.saturating_add(sites.bytes)
                || call.site >= ranges.address
                    && call.site < ranges.address.saturating_add(ranges.bytes)
            {
                return Err("compact import overlaps publication table".into());
            }
            if call.offset >= 1 << 20
                || !(1..=4).contains(&call.kind)
                || call.kind == 1 && call.instruction >> 26 != 2 && call.instruction >> 26 != 3
            {
                return Err("compact site cannot represent import".into());
            }
            word(&mut output, base, sites.address + cursor * 8, call.site)?;
            word(
                &mut output,
                base,
                sites.address + cursor * 8 + 4,
                (call.kind << 28) | (identity << 20) | call.offset,
            )?;
            let trapped = match call.kind {
                1 => psx_residency::overlay::direct(call.instruction, 1, trap.address, call.site)
                    .map_err(|e| format!("compact trap: {e:?}"))?,
                2 => {
                    (call.instruction & 0xffff0000)
                        | (psx_residency::overlay::window(trap.address) >> 16)
                }
                3 => {
                    (call.instruction & 0xffff0000)
                        | (trap
                            .address
                            .wrapping_sub(psx_residency::overlay::window(trap.address))
                            & 0xffff)
                }
                _ => trap.address,
            };
            word(&mut output, base, call.site, trapped)?;
            cursor += 1;
        }
        let first = cursor - image.incoming.len() as u32;
        word(
            &mut output,
            base,
            ranges.address + identity * 4,
            first | ((image.incoming.len() as u32) << 16),
        )?;
    }
    Ok(output)
}
/// Reuse the consumer's existing compact tables without new resident descriptors.
/// Existing chunk checksum rows remain transport-owned and use `arena::checksum`.
pub fn compose_compact(
    elf: &Elf<'_>,
    images: &[PackedImage<'_>],
    binding: CompactBinding<'_>,
    resident: &[&str],
    header: &str,
) -> Result<ComposedExecutable, String> {
    validate_images(images)?;
    let sections = images
        .iter()
        .flat_map(|i| i.sections.iter().copied())
        .collect::<Vec<_>>();
    let bytes = overlay_pack::resident_image(elf, &sections, resident, header)?;
    append_guard(
        images,
        compact_publish(elf, images, binding, resident, bytes)?,
    )
}
/// Refresh all compact imports and hardened packages as one final generation.
pub fn finalize_compact<'a>(
    elf: &Elf<'a>,
    images: &[PackedImage<'a>],
    binding: CompactBinding<'_>,
    initial: &ComposedExecutable,
    hardened: &[u8],
    resident: &[&str],
) -> Result<(Vec<PackedImage<'a>>, ComposedExecutable), String> {
    validate_images(images)?;
    let expected = append_guard(images, initial.resident.clone())?;
    if initial.guard != expected.guard
        || initial.modules != expected.modules
        || hardened.len() < initial.guard.len()
        || hardened[initial.guard.len()..].iter().any(|&v| v != 0)
        || hardened.get(..2048) != initial.guard.get(..2048)
    {
        return Err("compact final layout binding".into());
    }
    let resident_len = initial.resident.len();
    let base = u32::from_le_bytes(initial.resident[24..28].try_into().unwrap());
    let mut final_images = Vec::new();
    for (image, module) in images.iter().zip(&initial.modules) {
        let at = module
            .stored
            .checked_sub(base)
            .and_then(|v| v.checked_add(2048))
            .ok_or("compact module offset")? as usize;
        let end = at
            .checked_add(module.bytes as usize)
            .ok_or("compact module end")?;
        let mut source = initial.guard[..resident_len].to_vec();
        source.extend_from_slice(initial.guard.get(at..end).ok_or("compact source bounds")?);
        let one = ExecutableImages {
            resident: initial.resident.clone(),
            guard: source,
            module_at: base
                .checked_add(
                    u32::try_from(resident_len - 2048).map_err(|_| "compact resident length")?,
                )
                .ok_or("compact module load")?,
        };
        let mut patched = hardened[..resident_len].to_vec();
        patched.extend_from_slice(hardened.get(at..end).ok_or("compact patched bounds")?);
        final_images.push(overlay_pack::finalize_payload(
            elf, image, &one, &patched, resident,
        )?);
    }
    let published = compact_publish(
        elf,
        &final_images,
        binding,
        resident,
        hardened[..resident_len].to_vec(),
    )?;
    Ok((
        final_images.clone(),
        append_guard(&final_images, published)?,
    ))
}
