//! Package one already hazard-patched module using checked ELF identities.
use psoxide_hazard::{
    overlay_elf::Elf,
    overlay_pack::{self, Dependency, LinkedModule},
};
use std::{io::Read, path::PathBuf};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--extract") {
        use std::io::Write;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        if bytes.len() % 4 != 0 {
            return Err("unaligned package".into());
        }
        let words: Vec<_> = bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let (package, rest) = psx_residency::overlay::Package::parse(&words)
            .map_err(|e| format!("package: {e:?}"))?;
        if !rest.is_empty() {
            return Err("multiple packages in image extraction".into());
        }
        for word in package.image {
            std::io::stdout().write_all(&word.to_le_bytes())?;
        }
        return Ok(());
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let request: serde_json::Value = serde_json::from_str(&input)?;
    let field = |key| {
        request[key]
            .as_str()
            .ok_or_else(|| format!("missing {key}"))
    };
    let elf_bytes = std::fs::read(field("elf")?)?;
    let elf = Elf::parse(&elf_bytes)?;
    let names = request["modules"].as_array().ok_or("module identities")?;
    let symbols = elf.symbols()?;
    let mut modules = Vec::new();
    for (id, name) in names.iter().enumerate() {
        let name = name.as_str().ok_or("module name")?;
        let section = format!(".mod_{name}");
        match elf.section(&section) {
            Ok((index, section)) => {
                let code_end = symbols
                    .iter()
                    .filter(|s| s.section as usize == index && s.kind == 2 && s.bytes != 0)
                    .map(|s| s.address.checked_add(s.bytes).ok_or("symbol overflow"))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .max()
                    .ok_or("module has no code")?
                    .checked_sub(section.address)
                    .ok_or("module code range")?;
                // Same bounded trampoline reservation as the existing module
                // hardener: eight header bytes plus 32 words, never resized.
                let bytes = u32::try_from(section.bytes.len())?
                    .checked_add(3)
                    .ok_or("image overflow")?
                    & !3;
                modules.push(LinkedModule {
                    address: section.address,
                    bytes: bytes.checked_add(136).ok_or("trampoline overflow")?,
                    section: u16::try_from(index)?,
                    code_end,
                });
            }
            Err(error) if !symbols.iter().any(|s| s.name == section) => {
                if !error.starts_with("missing section ") {
                    return Err(error.into());
                }
                modules.push(LinkedModule {
                    address: 0x8040_8000 + u32::try_from(id)? * 0x10000,
                    bytes: 0,
                    section: u16::MAX
                        .checked_sub(u16::try_from(id)?)
                        .ok_or("module count")?,
                    code_end: 0,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    let source = request["source"].as_u64().ok_or("source module")? as usize;
    let descriptor = *modules.get(source).ok_or("source identity")?;
    let image_bytes = std::fs::read(field("image")?)?;
    if image_bytes.len() % 4 != 0 {
        return Err("unaligned image".into());
    }
    let image: Vec<u32> = image_bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    let (section, _) = elf.section(&format!(".mod_{}", names[source].as_str().unwrap()))?;
    let relocations = elf.relocations(section)?;
    let tramp = descriptor
        .bytes
        .checked_sub(136)
        .ok_or("trampoline range")?;
    let (fixups, imports) = overlay_pack::references(
        source,
        &image,
        &[0..descriptor.code_end, tramp + 8..descriptor.bytes],
        &relocations,
        &modules,
    )
    .map_err(|e| format!("module references: {e:?}"))?;
    let allowed = request["allowed_imports"]
        .as_array()
        .ok_or("allowed import identities")?;
    for import in &imports {
        if !allowed
            .iter()
            .any(|id| id.as_u64() == Some(import.dependency as u64))
        {
            return Err(format!(
                "unadmitted dependency {} from module {source}",
                import.dependency
            )
            .into());
        }
    }
    let dependencies: Vec<_> = modules
        .iter()
        .map(|m| Dependency {
            bytes: m.bytes,
            code_end: m.code_end,
        })
        .collect();
    let package = overlay_pack::encode(
        u32::try_from(source)?,
        descriptor.address,
        descriptor.code_end,
        &image,
        &fixups,
        &imports,
        &dependencies,
    )
    .map_err(|e| format!("module package: {e:?}"))?;
    let output = PathBuf::from(field("output")?);
    let next = output.with_extension("next");
    std::fs::write(
        &next,
        package
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>(),
    )?;
    std::fs::rename(next, output)?;
    println!(
        "{}",
        serde_json::json!({"fixups":fixups.len(),"imports":imports.len(),
        "image_bytes":descriptor.bytes,"resident_bytes":descriptor.bytes+u32::try_from(imports.len())?*12,
        "code_bytes":descriptor.code_end})
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("overlay-package: {error}");
        std::process::exit(1);
    }
}
