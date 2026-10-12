//! Bounded ELF32 little-endian module inputs, following HK's existing linker
//! contract. No object disassembler or executable function pointers are used.
use std::ops::Range;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at.checked_add(2).ok_or("ELF offset")?)
            .ok_or("ELF bounds")?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at.checked_add(4).ok_or("ELF offset")?)
            .ok_or("ELF bounds")?
            .try_into()
            .unwrap(),
    ))
}
fn range(offset: u32, size: u32, len: usize) -> Result<Range<usize>, String> {
    let end = offset.checked_add(size).ok_or("ELF section overflow")? as usize;
    if end > len {
        return Err("ELF section outside file".into());
    }
    Ok(offset as usize..end)
}
fn string(bytes: &[u8], at: u32) -> Result<&str, String> {
    let bytes = bytes.get(at as usize..).ok_or("ELF string offset")?;
    let end = bytes
        .iter()
        .position(|&b| b == 0)
        .ok_or("ELF unterminated string")?;
    std::str::from_utf8(&bytes[..end]).map_err(|_| "ELF string encoding".into())
}

/// One bounded linked section.
#[derive(Clone, Debug)]
pub struct Section<'a> {
    /// Unique ELF section name.
    pub name: &'a str,
    /// ELF section type.
    pub kind: u32,
    /// Linked virtual address.
    pub address: u32,
    /// Associated table index.
    pub link: u32,
    /// Relocation target section index.
    pub info: u32,
    /// Entry size of a table.
    pub stride: u32,
    /// File-backed bytes; NOBITS sections have an empty view.
    pub bytes: &'a [u8],
}

/// One direct ELF relocation, retaining its source and target identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// Source instruction/data address.
    pub address: u32,
    /// R_MIPS relocation type.
    pub kind: u8,
    /// Target symbol's section index.
    pub target_section: u16,
    /// Target symbol's linked address.
    pub target_address: u32,
}

/// Borrowed linked inputs. All table bounds are checked before iteration.
pub struct Elf<'a> {
    sections: Vec<Section<'a>>,
}

/// A bounded ELF symbol used for code ranges and resident import tables.
#[derive(Clone, Copy, Debug)]
pub struct Symbol<'a> {
    /// Original mangled symbol name.
    pub name: &'a str,
    /// Linked symbol address.
    pub address: u32,
    /// Linked symbol byte size.
    pub bytes: u32,
    /// ELF symbol type (function=2).
    pub kind: u8,
    /// Owning section index.
    pub section: u16,
}

impl<'a> Elf<'a> {
    /// Read only ELF32 little-endian MIPS executables with ELF REL tables.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.get(..7) != Some(&b"\x7fELF\x01\x01\x01"[..]) || u16_at(bytes, 18)? != 8 {
            return Err("expected ELF32 little-endian MIPS".into());
        }
        let start = u32_at(bytes, 32)? as usize;
        let stride = u16_at(bytes, 46)? as usize;
        let count = u16_at(bytes, 48)? as usize;
        let names_index = u16_at(bytes, 50)? as usize;
        if stride < 40 || count == 0 || names_index >= count {
            return Err("ELF section table".into());
        }
        let end = start
            .checked_add(count.checked_mul(stride).ok_or("ELF section count")?)
            .ok_or("ELF section table overflow")?;
        if end > bytes.len() {
            return Err("ELF truncated section table".into());
        }
        let names_at = start + names_index * stride;
        let names = &bytes[range(
            u32_at(bytes, names_at + 16)?,
            u32_at(bytes, names_at + 20)?,
            bytes.len(),
        )?];
        let mut sections = Vec::with_capacity(count);
        for i in 0..count {
            let at = start + i * stride;
            let kind = u32_at(bytes, at + 4)?;
            let section_bytes = if kind == 8 {
                &[]
            } else {
                &bytes[range(
                    u32_at(bytes, at + 16)?,
                    u32_at(bytes, at + 20)?,
                    bytes.len(),
                )?]
            };
            sections.push(Section {
                name: string(names, u32_at(bytes, at)?)?,
                kind,
                address: u32_at(bytes, at + 12)?,
                link: u32_at(bytes, at + 24)?,
                info: u32_at(bytes, at + 28)?,
                stride: u32_at(bytes, at + 36)?,
                bytes: section_bytes,
            });
        }
        Ok(Self { sections })
    }

    /// Unique named module section, refusing ambiguous layouts.
    pub fn section(&self, name: &str) -> Result<(usize, &Section<'a>), String> {
        let mut matches = self
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| s.name == name);
        let result = matches
            .next()
            .ok_or_else(|| format!("missing section {name}"))?;
        if matches.next().is_some() {
            return Err(format!("duplicate section {name}"));
        }
        Ok(result)
    }

    /// Checked relocations targeting one section.
    pub fn relocations(&self, target: usize) -> Result<Vec<Relocation>, String> {
        if target >= self.sections.len() {
            return Err("ELF target section".into());
        }
        let mut result = Vec::new();
        for section in &self.sections {
            if section.kind != 9 || section.info as usize != target {
                continue;
            }
            if section.stride != 8 || section.bytes.len() % 8 != 0 {
                return Err("ELF REL stride".into());
            }
            let symbols = self
                .sections
                .get(section.link as usize)
                .ok_or("ELF symbol link")?;
            if symbols.kind != 2 || symbols.stride != 16 || symbols.bytes.len() % 16 != 0 {
                return Err("ELF symbol table".into());
            }
            for at in (0..section.bytes.len()).step_by(8) {
                let info = u32_at(section.bytes, at + 4)?;
                let symbol = (info >> 8) as usize;
                let offset = symbol.checked_mul(16).ok_or("ELF symbol overflow")?;
                let target_section = u16_at(symbols.bytes, offset + 14)?;
                if target_section as usize >= self.sections.len() && target_section < 0xff00 {
                    return Err("ELF symbol section".into());
                }
                result.push(Relocation {
                    address: u32_at(section.bytes, at)?,
                    kind: info as u8,
                    target_section,
                    target_address: u32_at(symbols.bytes, offset + 4)?,
                });
            }
        }
        Ok(result)
    }

    /// All checked symbols, including exact owning section identities.
    pub fn symbols(&self) -> Result<Vec<Symbol<'a>>, String> {
        let mut result = Vec::new();
        for section in &self.sections {
            if section.kind != 2 {
                continue;
            }
            if section.stride != 16 || section.bytes.len() % 16 != 0 {
                return Err("ELF symbol stride".into());
            }
            let strings = self
                .sections
                .get(section.link as usize)
                .ok_or("ELF string link")?;
            if strings.kind != 3 {
                return Err("ELF string table".into());
            }
            for at in (0..section.bytes.len()).step_by(16) {
                result.push(Symbol {
                    name: string(strings.bytes, u32_at(section.bytes, at)?)?,
                    address: u32_at(section.bytes, at + 4)?,
                    bytes: u32_at(section.bytes, at + 8)?,
                    kind: section.bytes[at + 12] & 15,
                    section: u16_at(section.bytes, at + 14)?,
                });
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncated_and_wrong_machine_are_rejected() {
        for length in 0..64 {
            assert!(Elf::parse(&vec![0; length]).is_err());
        }
        let mut bytes = vec![0; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x01\x01\x01");
        assert!(Elf::parse(&bytes).is_err());
        bytes[18] = 8;
        bytes[46] = 40;
        bytes[48] = 1;
        bytes[32..36].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Elf::parse(&bytes).is_err());
    }
}
