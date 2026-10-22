// SPDX-License-Identifier: GPL-2.0-or-later
//! Package a complete cold executable section and immutable tables from one ELF.
use psoxide_hazard::overlay_elf::Elf;
use psoxide_hazard::overlay_pack;
use psx_residency::arena::checksum;
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 6 {
        return Err("overlay-package ELF OUTPUT --id N --code SECTION [--data SECTION] [--resident SECTION]".into());
    }
    let mut id = None;
    let mut code = None;
    let mut data = Vec::new();
    let mut resident = Vec::new();
    let mut calls = Vec::new();
    let mut header = None;
    let mut descriptor = None;
    let mut sites = None;
    let mut trap = None;
    let mut final_guard = None;
    let mut fields = args[2..].chunks_exact(2);
    for field in fields.by_ref() {
        match field[0].as_str() {
            "--id" if id.is_none() => id = Some(field[1].parse::<u32>()?),
            "--code" if code.is_none() => code = Some(field[1].as_str()),
            "--data" => data.push(field[1].as_str()),
            "--resident" => resident.push(field[1].as_str()),
            "--call" => calls.push(
                field[1]
                    .split_once(':')
                    .ok_or("call must be WRAPPER:EXPORT")?,
            ),
            "--header" if header.is_none() => header = Some(field[1].as_str()),
            "--descriptor" if descriptor.is_none() => descriptor = Some(field[1].as_str()),
            "--sites" if sites.is_none() => sites = Some(field[1].as_str()),
            "--trap" if trap.is_none() => trap = Some(field[1].as_str()),
            "--final-guard" if final_guard.is_none() => final_guard = Some(field[1].as_str()),
            _ => return Err("unknown or repeated argument".into()),
        }
    }
    if !fields.remainder().is_empty() {
        return Err("argument missing value".into());
    }
    let bytes = std::fs::read(&args[0])?;
    let elf = Elf::parse(&bytes)?;
    let mut image = overlay_pack::pack(
        &elf,
        id.ok_or("missing id")?,
        code.ok_or("missing code")?,
        &data,
        &resident,
        &calls,
    )?;
    let mut executable =
        if header.is_some() || descriptor.is_some() || sites.is_some() || trap.is_some() {
            Some(overlay_pack::executable_images(
                &elf,
                &image,
                &resident,
                header.ok_or("missing header")?,
                descriptor.ok_or("missing descriptor")?,
                sites.ok_or("missing sites")?,
                trap.ok_or("missing trap")?,
            )?)
        } else {
            None
        };
    if let Some(path) = final_guard {
        let hardened = std::fs::read(path)?;
        let (final_image, final_executable) = overlay_pack::finalize_guard(
            &elf,
            &image,
            executable
                .as_ref()
                .ok_or("final-guard needs complete executable metadata options")?,
            &hardened,
            &resident,
            descriptor.ok_or("missing descriptor")?,
            sites.ok_or("missing sites")?,
            trap.ok_or("missing trap")?,
        )?;
        image = final_image;
        executable = Some(final_executable);
    }
    let bytes = image
        .words
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let mut metadata = format!(
        "linked={:#x}\ncode_end={}\nchecksum={:#x}\nimage_bytes={}\n",
        image.linked,
        image.code_end,
        checksum(&image.words),
        image.words[2] * 4
    );
    for export in &image.exports {
        metadata.push_str(&format!(
            "export {:#x} {} {:?}\n",
            export.address - image.linked,
            export.bytes,
            export.name
        ));
    }
    for call in &image.incoming {
        metadata.push_str(&format!(
            "call {} {:#x} {:#x} {:#x} {:?} {:?}\n",
            call.selector, call.site, call.offset, call.instruction, call.caller, call.export
        ));
    }
    if let Some(executable) = executable {
        std::fs::write(format!("{}.exe", args[1]), executable.resident)?;
        std::fs::write(format!("{}.guard.exe", args[1]), executable.guard)?;
        let args_path = format!("{}.guard-args", args[1]);
        let mut args = format!(
            "--module {:#x}..{:#x}@{:#x}",
            image.linked,
            image.linked + image.words[2] * 4,
            executable.module_at
        );
        for range in &image.code_ranges {
            args.push_str(&format!(" --code {:#x}..{:#x}", range.start, range.end));
        }
        std::fs::write(args_path, args)?;
    }
    for range in &image.code_ranges {
        metadata.push_str(&format!("code {:#x}..{:#x}\n", range.start, range.end));
    }
    std::fs::write(&args[1], bytes)?;
    std::fs::write(format!("{}.meta", args[1]), &metadata)?;
    let mut rust = format!("// Generated from the same linked ELF; do not edit.\npub const PACKAGE_WORDS: usize = {};\npub const DESCRIPTOR: psx_residency::arena::Descriptor = psx_residency::arena::Descriptor {{ id: {}, linked: {:#x}, checksum: {:#x} }};\npub const CALLS: &[(&str, &str, u32, u32)] = &[\n", image.words.len(), image.words[1], image.linked, checksum(&image.words));
    for call in &image.incoming {
        rust.push_str(&format!(
            "    ({:?}, {:?}, {:#x}, {:#x}),\n",
            call.caller, call.export, call.site, call.offset
        ));
    }
    rust.push_str("];\n");
    std::fs::write(format!("{}.rs", args[1]), rust)?;
    print!("{metadata}");
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
