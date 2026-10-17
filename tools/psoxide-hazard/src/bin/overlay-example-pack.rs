//! Package the same-compilation borrowed-arena example from its final ELF.
//! Reuses the checked SDK ELF reader and existing legacy module ABI.
use psoxide_hazard::overlay_elf::{Elf, Symbol};
use psx_residency::{arena::checksum, overlay};
use std::{collections::BTreeSet, error::Error};

fn words(bytes: &[u8]) -> Result<Vec<u32>, Box<dyn Error>> {
    if bytes.len() & 3 != 0 {
        return Err("word-aligned section required".into());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .collect())
}
fn symbol<'a>(symbols: &[Symbol<'a>], name: &str) -> Result<Symbol<'a>, Box<dyn Error>> {
    let mut found = symbols.iter().filter(|s| s.name == name);
    let value = *found.next().ok_or_else(|| format!("missing {name}"))?;
    if found.next().is_some() {
        return Err("duplicate export".into());
    }
    Ok(value)
}
fn put(image: &mut [u8], address: u32, value: u32) -> Result<(), Box<dyn Error>> {
    let at = address.checked_sub(0x8000f800).ok_or("resident address")? as usize;
    image
        .get_mut(at..at.checked_add(4).ok_or("resident overflow")?)
        .ok_or("resident range")?
        .copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn run() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("overlay-example-pack LINKED.elf OUTPUT.exe".into());
    }
    let input = std::fs::read(&args[0])?;
    let elf = Elf::parse(&input)?;
    let symbols = elf.symbols()?;
    let (cold_index, cold) = elf.section(".overlay")?;
    let export = symbol(&symbols, "overlay_cold_update")?;
    if export.kind != 2
        || export.section as usize != cold_index
        || export.address != cold.address
        || export.bytes == 0
    {
        return Err("one exact executable cold export required".into());
    }
    let mut image = words(cold.bytes)?;
    let end = cold
        .address
        .checked_add(u32::try_from(cold.bytes.len())?)
        .ok_or("cold bounds")?;
    overlay::placement(cold.address, end - cold.address, true)
        .map_err(|e| format!("placement: {e:?}"))?;
    let mut fixups = BTreeSet::new();
    // The same checked reference contract as overlay_pack: scan direct jumps,
    // relocate self targets, leave fixed resident service targets unchanged.
    for offset in (0..export.bytes).step_by(4) {
        let word = *image
            .get(offset as usize / 4)
            .ok_or("cold executable bound")?;
        if matches!(word >> 26, 2 | 3) {
            let pc = cold
                .address
                .checked_add(offset)
                .and_then(|v| v.checked_add(4))
                .ok_or("cold PC")?;
            let target = (pc & 0xf0000000) | ((word & 0x03ffffff) << 2);
            if cold.address <= target && target < end {
                fixups.insert(0x10000000 | offset);
            } else if !symbols.iter().any(|s| {
                s.kind == 2
                    && s.bytes != 0
                    && s.address <= target
                    && target < s.address.saturating_add(s.bytes)
            }) {
                return Err("direct import has no executable symbol".into());
            }
        }
    }
    for relocation in elf.relocations(cold_index)? {
        let offset = relocation
            .address
            .checked_sub(cold.address)
            .ok_or("relocation site")?;
        if offset & 3 != 0 || offset >= end - cold.address {
            return Err("relocation bounds".into());
        }
        if relocation.kind == 10 && relocation.target_section as usize != cold_index {
            return Err("cross-section PC16 unsupported".into());
        }
        if relocation.target_section as usize != cold_index {
            if !matches!(relocation.kind, 2 | 4 | 5 | 6) {
                return Err("resident relocation unsupported".into());
            }
            continue;
        }
        let kind = match relocation.kind {
            2 => 4,
            5 => 2,
            6 => 3,
            4 | 10 => continue,
            _ => return Err("self relocation unsupported".into()),
        };
        fixups.insert((kind << 28) | offset);
    }
    // Preserve the existing module hardening reservation even when this tiny
    // module needs no hazard patch. No guard or trampoline policy is relaxed.
    image.extend([0x48415a54, psoxide_hazard::overlay_pack::TRAMPOLINE_WORDS]);
    image.extend([0; psoxide_hazard::overlay_pack::TRAMPOLINE_WORDS as usize]);
    let module_bytes = image.len() * 4;
    let mut package = vec![
        overlay::MAGIC,
        1,
        image.len() as u32,
        fixups.len() as u32,
        export.bytes,
    ];
    package.extend(image);
    package.extend(fixups);
    overlay::Package::parse(&package).map_err(|e| format!("package: {e:?}"))?;
    let header = elf.section(".ps_exe_head")?.1;
    let data = elf.section(".data")?.1;
    let resident_end = data
        .address
        .checked_add(data.bytes.len() as u32)
        .ok_or("resident end")?;
    let load_bytes = resident_end
        .checked_sub(0x80010000)
        .ok_or("resident load")?;
    let padded = load_bytes.checked_add(2047).ok_or("resident size")? & !2047;
    let mut exe = vec![0; 2048 + padded as usize];
    exe[..header.bytes.len()].copy_from_slice(header.bytes);
    for name in [".text", ".data"] {
        let section = elf.section(name)?.1;
        let at = section
            .address
            .checked_sub(0x8000f800)
            .ok_or("resident section")? as usize;
        exe.get_mut(at..at + section.bytes.len())
            .ok_or("resident section bounds")?
            .copy_from_slice(section.bytes);
    }
    exe[28..32].copy_from_slice(&padded.to_le_bytes());
    let caller = symbol(&symbols, "overlay_typed_call")?;
    let text = elf.section(".text")?.1;
    let mut sites = Vec::new();
    for offset in (0..text.bytes.len()).step_by(4) {
        let word = u32::from_le_bytes(text.bytes[offset..offset + 4].try_into().unwrap());
        let site = text.address + offset as u32;
        if matches!(word >> 26, 2 | 3) {
            let target = ((site + 4) & 0xf0000000) | ((word & 0x03ffffff) << 2);
            if cold.address <= target && target < end {
                if site < caller.address
                    || site >= caller.address + caller.bytes
                    || target != export.address
                {
                    return Err("unguarded cold call".into());
                }
                sites.push((site, word));
            }
        }
    }
    if sites.len() != 1 {
        return Err("exactly one typed cold callsite required".into());
    }
    let trap = symbol(&symbols, "overlay_unloaded_trap")?;
    let (site, instruction) = sites[0];
    put(
        &mut exe,
        site,
        overlay::direct(instruction, 1, trap.address, site)
            .map_err(|e| format!("import: {e:?}"))?,
    )?;
    let payload = symbol(&symbols, "OVERLAY_PAYLOAD")?;
    if package.len() * 4 > payload.bytes as usize {
        return Err("payload fixture capacity".into());
    }
    for (i, &word) in package.iter().enumerate() {
        put(&mut exe, payload.address + i as u32 * 4, word)?;
    }
    let descriptor = symbol(&symbols, "OVERLAY_DESCRIPTOR")?;
    for (i, word) in [package.len() as u32, checksum(&package), site, 0]
        .into_iter()
        .enumerate()
    {
        put(&mut exe, descriptor.address + i as u32 * 4, word)?;
    }
    // Existing guard tools model the linked module after the resident payload.
    // The guard image additionally restores the original typed direct call,
    // proving the actual caller/module/resident-service dependency closure.
    // Runtime bytes remain trapped until a live Admission binds them.
    let guard_at = 0x80010000u32.checked_add(padded).ok_or("guard placement")?;
    let mut guard = exe.clone();
    put(&mut guard, site, instruction)?;
    for &word in &package[5..5 + module_bytes / 4] {
        guard.extend_from_slice(&word.to_le_bytes());
    }
    std::fs::write(format!("{}.guard", args[1]), guard)?;
    std::fs::write(
        format!("{}.module-args", args[1]),
        format!(
            "--module {:#x}..{:#x}@{:#x}",
            cold.address,
            cold.address + module_bytes as u32,
            guard_at
        ),
    )?;
    std::fs::write(
        format!("{}.code-args", args[1]),
        format!(
            "--code {:#x}..{:#x} --code {:#x}..{:#x}",
            cold.address,
            cold.address + export.bytes,
            end + 8,
            cold.address + module_bytes as u32
        ),
    )?;
    std::fs::write(&args[1], exe)?;
    println!(
        "cold={:#x}..{:#x} payload={:#x} code_bytes={} package_bytes={} callsite={:#x} guard_at={:#x}",
        cold.address,
        cold.address + module_bytes as u32,
        payload.address + 20,
        export.bytes,
        package.len() * 4,
        site,
        guard_at
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
