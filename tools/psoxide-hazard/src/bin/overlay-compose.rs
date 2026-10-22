// SPDX-License-Identifier: GPL-2.0-or-later
//! Batch same-final-link publication of independent cold images.
use psoxide_hazard::overlay_composition::{self, Binding};
use psoxide_hazard::overlay_elf::Elf;
use psoxide_hazard::overlay_pack;
use std::collections::BTreeSet;
use std::path::Path;
#[derive(Default)]
struct Spec<'a> {
    name: &'a str,
    id: Option<u32>,
    code: Option<&'a str>,
    data: Vec<&'a str>,
    pinned: Vec<overlay_pack::PinnedImport<'a>>,
    calls: Vec<(&'a str, &'a str)>,
    descriptor: Option<&'a str>,
    sites: Option<&'a str>,
    trap: Option<&'a str>,
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 8 {
        return Err("overlay-compose ELF NEW_DIRECTORY --header SECTION --resident SECTION --image NAME --id N --code SECTION --descriptor SYMBOL --sites SYMBOL --trap SYMBOL [--data SECTION] [--call WRAPPER:EXPORT] [--image ...] [--final-guard PATH]".into());
    }
    let mut header = None;
    let mut resident = Vec::new();
    let mut final_guard = None;
    let mut compact_sites = None;
    let mut compact_ranges = None;
    let mut compact_trap = None;
    let mut specs: Vec<Spec<'_>> = Vec::new();
    let mut names = BTreeSet::new();
    let mut fields = args[2..].chunks_exact(2);
    for field in fields.by_ref() {
        let flag = field[0].as_str();
        let value = field[1].as_str();
        match flag {
            "--header" if header.is_none() => header = Some(value),
            "--resident" => resident.push(value),
            "--final-guard" if final_guard.is_none() => final_guard = Some(value),
            "--compact-sites" if compact_sites.is_none() => compact_sites = Some(value),
            "--compact-ranges" if compact_ranges.is_none() => compact_ranges = Some(value),
            "--compact-trap" if compact_trap.is_none() => compact_trap = Some(value),
            "--image" => {
                if value.is_empty()
                    || !value
                        .bytes()
                        .all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'-')
                    || !names.insert(value)
                {
                    return Err("unique simple image name required".into());
                }
                specs.push(Spec {
                    name: value,
                    ..Spec::default()
                });
            }
            _ => {
                let spec = specs.last_mut().ok_or("image option before --image")?;
                match flag {
                    "--id" if spec.id.is_none() => spec.id = Some(value.parse()?),
                    "--code" if spec.code.is_none() => spec.code = Some(value),
                    "--data" => spec.data.push(value),
                    "--call" => spec
                        .calls
                        .push(value.split_once(':').ok_or("call must be WRAPPER:EXPORT")?),
                    "--pinned" => {
                        let parts = value.split(':').collect::<Vec<_>>();
                        if parts.len() != 4 {
                            return Err(
                                "pinned must be OWNER:SITE_OFFSET:KIND:TARGET_OFFSET".into()
                            );
                        }
                        let number = |v: &str| -> Result<u32, Box<dyn std::error::Error>> {
                            Ok(if let Some(hex) = v.strip_prefix("0x") {
                                u32::from_str_radix(hex, 16)?
                            } else {
                                v.parse()?
                            })
                        };
                        spec.pinned.push(overlay_pack::PinnedImport {
                            owner: parts[0],
                            site_offset: number(parts[1])?,
                            kind: number(parts[2])?,
                            target_offset: number(parts[3])?,
                        });
                    }
                    "--descriptor" if spec.descriptor.is_none() => spec.descriptor = Some(value),
                    "--sites" if spec.sites.is_none() => spec.sites = Some(value),
                    "--trap" if spec.trap.is_none() => spec.trap = Some(value),
                    _ => return Err("unknown or repeated image option".into()),
                }
            }
        }
    }
    if !fields.remainder().is_empty() {
        return Err("argument missing value".into());
    }
    let bytes = std::fs::read(&args[0])?;
    let elf = Elf::parse(&bytes)?;
    let mut images = specs
        .iter()
        .map(|s| {
            overlay_pack::pack_pinned(
                &elf,
                s.id.ok_or("missing image id")?,
                s.code.ok_or("missing image code")?,
                &s.data,
                &resident,
                &s.calls,
                &s.pinned,
            )
        })
        .collect::<Result<Vec<_>, String>>()?;
    let compact = if compact_sites.is_some() || compact_ranges.is_some() || compact_trap.is_some() {
        if specs
            .iter()
            .any(|s| s.descriptor.is_some() || s.sites.is_some() || s.trap.is_some())
        {
            return Err("compact and standard publication tables cannot be mixed".into());
        }
        Some(overlay_composition::CompactBinding {
            sites: compact_sites.ok_or("missing compact sites")?,
            ranges: compact_ranges.ok_or("missing compact ranges")?,
            trap: compact_trap.ok_or("missing compact trap")?,
        })
    } else {
        None
    };
    if compact.is_none() && specs.iter().any(|s| !s.pinned.is_empty()) {
        return Err("pinned bindings require legacy compact publication".into());
    }
    let bindings = if compact.is_some() {
        Vec::new()
    } else {
        specs
            .iter()
            .map(|s| {
                Ok(Binding {
                    descriptor: s.descriptor.ok_or("missing descriptor")?,
                    sites: s.sites.ok_or("missing sites")?,
                    trap: s.trap.ok_or("missing trap")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    let mut executable = if let Some(binding) = compact {
        overlay_composition::compose_compact(
            &elf,
            &images,
            binding,
            &resident,
            header.ok_or("missing header")?,
        )?
    } else {
        overlay_composition::compose(
            &elf,
            &images,
            &bindings,
            &resident,
            header.ok_or("missing header")?,
        )?
    };
    if let Some(path) = final_guard {
        let hardened = std::fs::read(path)?;
        (images, executable) = if let Some(binding) = compact {
            overlay_composition::finalize_compact(
                &elf,
                &images,
                binding,
                &executable,
                &hardened,
                &resident,
            )?
        } else {
            overlay_composition::finalize(
                &elf,
                &images,
                &bindings,
                &executable,
                &hardened,
                &resident,
            )?
        };
    }
    let output = Path::new(&args[1]);
    if output.exists() {
        return Err("output generation directory already exists".into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = output
        .file_name()
        .ok_or("output directory name")?
        .to_str()
        .ok_or("output directory UTF-8")?;
    let staging = parent.join(format!("{name}.next-{}", std::process::id()));
    // Publish one complete generation by directory rename after all validation.
    // On I/O failure the incomplete staging directory remains for diagnosis.
    std::fs::create_dir(&staging)?;
    std::fs::write(staging.join("runtime.exe"), &executable.resident)?;
    std::fs::write(staging.join("guard.exe"), &executable.guard)?;
    let mut guard_args = String::new();
    let mut summary = String::new();
    for ((spec, image), module) in specs.iter().zip(&images).zip(&executable.modules) {
        let bytes = image
            .words
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<_>>();
        std::fs::write(staging.join(format!("{}.pak", spec.name)), bytes)?;
        guard_args += &format!(
            " --module 0x{:08x}..0x{:08x}@0x{:08x}",
            module.linked,
            module.linked + module.bytes,
            module.stored
        );
        for range in &image.code_ranges {
            guard_args += &format!(" --code 0x{:08x}..0x{:08x}", range.start, range.end);
        }
        summary += &format!(
            "{} id={} words={} linked=0x{:08x} checksum=0x{:08x} image_bytes={} calls={}\n",
            spec.name,
            image.words[1],
            image.words.len(),
            image.linked,
            psx_residency::arena::checksum(&image.words),
            module.bytes,
            image.incoming.len()
        );
        for call in &image.incoming {
            summary += &format!(
                "  selector={} caller={} export={} site=0x{:08x} offset=0x{:08x}\n",
                call.selector, call.caller, call.export, call.site, call.offset
            );
        }
    }
    std::fs::write(staging.join("guard-args"), guard_args.trim_start())?;
    std::fs::write(staging.join("manifest.txt"), summary)?;
    std::fs::rename(&staging, output)?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
