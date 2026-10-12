//! Host encoding of checked code/data overlay packages.
//! The linker/hazard passes supply the final image and static relocation sites.
use psx_residency::overlay::{Error, Import, Package, IMPORT_MAGIC};

/// Target descriptor from the same final linked image.
#[derive(Clone, Copy, Debug)]
pub struct Dependency {
    /// Image size, including immutable data and hazard trampolines.
    pub bytes: u32,
    /// Exclusive executable prefix boundary for direct callable entries.
    pub code_end: u32,
}

/// Final module identity used to resolve static linked references.
#[derive(Clone, Copy, Debug)]
pub struct LinkedModule {
    /// Explicit linked address, not a runtime allocation.
    pub address: u32,
    /// Image byte size after hazard patching.
    pub bytes: u32,
    /// ELF section index before extraction.
    pub section: u16,
    /// Exclusive executable prefix boundary.
    pub code_end: u32,
}

fn target(modules: &[LinkedModule], address: u32) -> Result<Option<(usize, u32)>, Error> {
    let mut result = None;
    for (id, module) in modules.iter().enumerate() {
        let end = module
            .address
            .checked_add(module.bytes)
            .ok_or(Error::Bounds)?;
        if module.address <= address && address < end {
            if result.is_some() {
                return Err(Error::Bounds);
            }
            result = Some((id, address - module.address));
        }
    }
    Ok(result)
}

/// Derive self fixups and explicit cross-module imports from the final patched
/// image. All module identities come from the same linked ELF. Resident targets
/// remain unchanged; unsupported relocation forms fail packaging.
pub fn references(
    source: usize,
    image: &[u32],
    code_ranges: &[std::ops::Range<u32>],
    relocations: &[crate::overlay_elf::Relocation],
    modules: &[LinkedModule],
) -> Result<(Vec<u32>, Vec<Import>), Error> {
    let module = modules.get(source).ok_or(Error::Bounds)?;
    for candidate in modules {
        if candidate.bytes != 0 {
            psx_residency::overlay::placement(candidate.address, candidate.bytes, true)?;
        }
    }
    if image.len().checked_mul(4) != Some(module.bytes as usize) {
        return Err(Error::Bounds);
    }
    let mut self_sites = std::collections::BTreeMap::new();
    let mut imports = std::collections::BTreeMap::new();
    let mut add = |site: u32, kind: u8, address: u32| -> Result<(), Error> {
        let Some((id, offset)) = target(modules, address)? else {
            return Ok(());
        };
        if id == source {
            if imports.contains_key(&site) {
                return Err(Error::Bounds);
            }
            if self_sites
                .insert(site, (kind as u32) << 28 | site)
                .is_some()
            {
                return Err(Error::Bounds);
            }
        } else {
            if self_sites.contains_key(&site) {
                return Err(Error::Bounds);
            }
            let import = Import {
                site,
                kind,
                dependency: u16::try_from(id).map_err(|_| Error::Bounds)?,
                target: offset,
            };
            import.validate_code(module.bytes, modules[id].bytes, modules[id].code_end)?;
            if imports.insert(site, import).is_some() {
                return Err(Error::Bounds);
            }
        }
        Ok(())
    };
    for range in code_ranges {
        if range.start & 3 != 0
            || range.end & 3 != 0
            || range.start > range.end
            || range.end > module.bytes
        {
            return Err(Error::Bounds);
        }
        for site in (range.start..range.end).step_by(4) {
            let word = image[site as usize / 4];
            if word >> 26 == 2 || word >> 26 == 3 {
                let pc = module.address.checked_add(site).ok_or(Error::Bounds)?;
                add(
                    site,
                    1,
                    (pc.wrapping_add(4) & 0xf000_0000) | ((word & 0x03ff_ffff) << 2),
                )?;
            }
        }
    }
    for relocation in relocations {
        let site = relocation
            .address
            .checked_sub(module.address)
            .ok_or(Error::Bounds)?;
        let word = *image.get(site as usize / 4).ok_or(Error::Bounds)?;
        if site & 3 != 0 {
            return Err(Error::Bounds);
        }
        let mut destinations = modules
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.section == relocation.target_section);
        let Some((destination_id, destination)) = destinations.next() else {
            continue;
        };
        if destinations.next().is_some() {
            return Err(Error::Bounds);
        }
        match relocation.kind {
            2 => {
                if target(modules, word)?.map(|(id, _)| id) != Some(destination_id) {
                    return Err(Error::Bounds);
                }
                add(site, 4, word)?;
            }
            5 => {
                // All accepted module images occupy one signed-low-half window.
                // Retain each HI16's target window; the paired LO16 supplies the
                // per-word offset independently when binding.
                if word & 0xffff != psx_residency::overlay::window(destination.address) >> 16 {
                    return Err(Error::Window);
                }
                add(site, 2, destination.address)?;
            }
            6 => {
                let address = psx_residency::overlay::window(destination.address)
                    .wrapping_add((word as u16 as i16 as i32) as u32);
                if target(modules, address)?.map(|(id, _)| id) != Some(destination_id) {
                    return Err(Error::Bounds);
                }
                add(site, 3, address)?;
            }
            4 | 10 => {} // J26 was scanned; PC-relative branches are unchanged.
            _ => return Err(Error::Kind),
        }
    }
    Ok((
        self_sites.into_values().collect(),
        imports.into_values().collect(),
    ))
}

/// Encode an extended package only after every import has a bounded target.
/// This does not replace hazard or stack analysis of the final linked code.
pub fn encode(
    id: u32,
    linked: u32,
    code_end: u32,
    image: &[u32],
    fixups: &[u32],
    imports: &[Import],
    dependencies: &[Dependency],
) -> Result<Vec<u32>, Error> {
    let image_words = u32::try_from(image.len()).map_err(|_| Error::Bounds)?;
    let image_bytes = image_words.checked_mul(4).ok_or(Error::Bounds)?;
    if image_bytes != 0 {
        psx_residency::overlay::placement(linked, image_bytes, code_end != 0)?;
    }
    let mut words = vec![
        IMPORT_MAGIC,
        id,
        image_words,
        u32::try_from(fixups.len()).map_err(|_| Error::Bounds)?,
        code_end,
        u32::try_from(imports.len()).map_err(|_| Error::Bounds)?,
        linked,
    ];
    words.extend_from_slice(image);
    words.extend_from_slice(fixups);
    let mut sites = std::collections::BTreeSet::new();
    for &fixup in fixups {
        if !sites.insert(fixup & 0x0fff_ffff) {
            return Err(Error::Bounds);
        }
    }
    for &import in imports {
        let dependency = dependencies
            .get(import.dependency as usize)
            .ok_or(Error::Bounds)?;
        import.validate_code(image_bytes, dependency.bytes, dependency.code_end)?;
        if import.site > 0x0fff_ffff || !sites.insert(import.site) {
            return Err(Error::Bounds);
        }
        words.extend_from_slice(&[
            (import.kind as u32) << 28 | import.site,
            import.dependency as u32,
            import.target,
        ]);
    }
    let (package, rest) = Package::parse(&words)?;
    if !rest.is_empty() || package.imports().count() != imports.len() {
        return Err(Error::Bounds);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_cross_module_references_remain_explicit() {
        let modules = [
            LinkedModule {
                address: 0x8040_8000,
                bytes: 12,
                section: 2,
                code_end: 4,
            },
            LinkedModule {
                address: 0x8041_8000,
                bytes: 8,
                section: 3,
                code_end: 8,
            },
        ];
        let image = [0x0c10_6001, 0x3c01_8042, 0x2421_8004];
        let relocations = [
            crate::overlay_elf::Relocation {
                address: 0x8040_8004,
                kind: 5,
                target_section: 3,
                target_address: 0x8041_8000,
            },
            crate::overlay_elf::Relocation {
                address: 0x8040_8008,
                kind: 6,
                target_section: 3,
                target_address: 0x8041_8000,
            },
        ];
        let (fixups, imports) = references(0, &image, &[0..4], &relocations, &modules).unwrap();
        assert!(fixups.is_empty());
        assert_eq!(imports.len(), 3);
        assert_eq!(
            imports[0],
            Import {
                site: 0,
                dependency: 1,
                target: 4,
                kind: 1
            }
        );
        assert_eq!(
            imports[2],
            Import {
                site: 8,
                dependency: 1,
                target: 4,
                kind: 3
            }
        );
        let mut malformed = image;
        malformed[2] = 0x2421_8010;
        assert!(references(0, &malformed, &[0..4], &relocations, &modules).is_err());
    }

    #[test]
    fn static_import_roundtrip_and_conflicting_sites() {
        let imports = [Import {
            site: 0,
            dependency: 0,
            target: 4,
            kind: 1,
        }];
        let targets = [Dependency {
            bytes: 8,
            code_end: 8,
        }];
        let words = encode(7, 0x8040_1000, 4, &[0x0c00_0000], &[], &imports, &targets).unwrap();
        let (package, _) = Package::parse(&words).unwrap();
        assert_eq!(package.linked, Some(0x8040_1000));
        assert_eq!(package.imports().collect::<Vec<_>>(), imports);
        assert!(encode(
            7,
            0x8040_1000,
            4,
            &[0x0c00_0000],
            &[0x1000_0000],
            &imports,
            &targets
        )
        .is_err());
        assert!(encode(
            7,
            0x8040_1000,
            4,
            &[0x0c00_0000],
            &[],
            &imports,
            &[Dependency {
                bytes: 4,
                code_end: 4
            }]
        )
        .is_err());
        assert!(encode(
            7,
            0x8040_1000,
            4,
            &[0x0c00_0000],
            &[],
            &imports,
            &[Dependency {
                bytes: 8,
                code_end: 4
            }]
        )
        .is_err());
        assert!(encode(7, 0x8040_1000, 4, &[0x0c00_0000], &[], &imports, &[]).is_err());
    }
}
