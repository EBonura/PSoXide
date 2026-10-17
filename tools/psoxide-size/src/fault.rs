//! The CPU exception record psx-rt keeps for the debugger and gates.
//!
//! A guest built with `-Cpanic=immediate-abort` turns every panic into a
//! `break` at the faulting instruction. psx-rt's exception handler stores the
//! COP0 Cause, EPC and BadVAddr in `__psx_rt_fault_*` and counts the
//! exception in `__psx_rt_fault_count`, so a gate that dumps RAM after a run
//! can fail on a non-zero count and name the function around EPC.

use crate::map::{LinkMap, Section};

/// What `psx-rt` recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub count: u32,
    pub cause: u32,
    pub epc: u32,
    pub badvaddr: u32,
}

impl Fault {
    /// Cause.ExcCode (bits 2..6): 9 is `break`, 4/5 address errors, 6/7 bus errors.
    pub fn exc_code(&self) -> u32 {
        (self.cause >> 2) & 0x1f
    }
}

/// Address of a symbol by exact name, from any input-section symbol line.
pub fn symbol_address(map: &LinkMap, name: &str) -> Option<u32> {
    map.sections
        .iter()
        .flat_map(|s: &Section| s.syms.iter())
        .find(|s| s.name == name)
        .map(|s| s.vma)
}

/// Name of the function that contains `address`.
pub fn symbolize(map: &LinkMap, address: u32) -> Option<&str> {
    map.sections
        .iter()
        .flat_map(|s| s.syms.iter())
        .filter(|s| s.size > 0 && s.vma <= address && address < s.vma + s.size)
        .map(|s| s.name.as_str())
        .next()
}

fn ram_word(ram: &[u8], address: u32) -> Result<u32, String> {
    // KUSEG, KSEG0 and KSEG1 alias the same 2 MiB.
    let offset = (address & 0x001f_ffff) as usize;
    let word = ram
        .get(offset..offset + 4)
        .ok_or_else(|| format!("RAM dump is too short to hold 0x{address:08x}"))?;
    Ok(u32::from_le_bytes(word.try_into().unwrap()))
}

/// Read the fault record out of a `--dump-ram` image.
pub fn read(map: &LinkMap, ram: &[u8]) -> Result<Fault, String> {
    let word = |name: &str| -> Result<u32, String> {
        let at =
            symbol_address(map, name).ok_or_else(|| format!("link map has no symbol {name}"))?;
        ram_word(ram, at)
    };
    Ok(Fault {
        count: word("__psx_rt_fault_count")?,
        cause: word("__psx_rt_fault_cause")?,
        epc: word("__psx_rt_fault_epc")?,
        badvaddr: word("__psx_rt_fault_badvaddr")?,
    })
}

/// One line for a gate log: empty when the guest took no exception.
pub fn describe(map: &LinkMap, fault: &Fault) -> Option<String> {
    if fault.count == 0 {
        return None;
    }
    let near = symbolize(map, fault.epc).unwrap_or("?");
    Some(format!(
        "the guest took {} CPU exceptions (a panic is a BREAK): Cause 0x{:08x} (ExcCode {}), EPC 0x{:08x} in {near}, BadVAddr 0x{:08x}",
        fault.count,
        fault.cause,
        fault.exc_code(),
        fault.epc,
        fault.badvaddr
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::parse;

    const MAP: &str = "\
     VMA      LMA     Size Align Out     In      Symbol
80010000 80010000     1000     4 .text
80010000 80010000       40     4         x.o:(.text.f)
80010000 80010000       40     1                 game::f
800bd240 800bd240      100    16 .data
800bd240 800bd240        4     4         x.o:(.bss.__psx_rt_fault_count)
800bd240 800bd240        4     1                 __psx_rt_fault_count
800bd244 800bd244        4     4         x.o:(.bss.__psx_rt_fault_cause)
800bd244 800bd244        4     1                 __psx_rt_fault_cause
800bd248 800bd248        4     4         x.o:(.bss.__psx_rt_fault_epc)
800bd248 800bd248        4     1                 __psx_rt_fault_epc
800bd24c 800bd24c        4     4         x.o:(.bss.__psx_rt_fault_badvaddr)
800bd24c 800bd24c        4     1                 __psx_rt_fault_badvaddr
";

    #[test]
    fn reads_and_describes_a_break() {
        let map = parse(MAP);
        let mut ram = vec![0u8; 0x0020_0000];
        let put = |ram: &mut Vec<u8>, at: usize, v: u32| {
            ram[at..at + 4].copy_from_slice(&v.to_le_bytes())
        };
        put(&mut ram, 0xbd240, 1);
        put(&mut ram, 0xbd244, 9 << 2);
        put(&mut ram, 0xbd248, 0x8001_0010);
        let fault = read(&map, &ram).unwrap();
        assert_eq!(fault.exc_code(), 9);
        let text = describe(&map, &fault).unwrap();
        assert!(text.contains("EPC 0x80010010 in game::f"), "{text}");
        put(&mut ram, 0xbd240, 0);
        assert!(describe(&map, &read(&map, &ram).unwrap()).is_none());
    }

    #[test]
    fn missing_symbols_are_reported() {
        let map = parse(MAP.lines().take(4).collect::<Vec<_>>().join("\n").as_str());
        assert!(read(&map, &[0; 16]).unwrap_err().contains("no symbol"));
    }
}
