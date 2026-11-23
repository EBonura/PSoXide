//! Parser for the lld link map every PSoXide guest build writes
//! (`-Wl,-Map` / `--Map`), and the PS-X EXE reader that supplies the bytes.
//!
//! The map has seven columns: `VMA LMA Size Align`, then an output-section
//! name at indent 0, input sections at indent 8 (`object:(section)`), and
//! symbols at indent 16.

/// One symbol line under an input section.
#[derive(Debug, Clone)]
pub struct Sym {
    pub name: String,
    pub vma: u32,
    pub size: u32,
}

/// One input section as the linker placed it.
#[derive(Debug, Clone)]
pub struct Section {
    /// Output section it was placed in (`.text`, `.data`, `.bss`, ...).
    pub out: String,
    pub object: String,
    /// Raw (mangled) input section name, without the `object:(` wrapper.
    pub name: String,
    pub vma: u32,
    pub size: u32,
    pub align: u32,
    pub syms: Vec<Sym>,
}

/// An output section header.
#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub vma: u32,
    pub size: u32,
}

/// Everything the audit reads from a map.
#[derive(Debug, Default)]
pub struct LinkMap {
    pub outputs: Vec<Output>,
    pub sections: Vec<Section>,
    /// Linker-script symbols (`__heap_start`, `__image_end`, ...), by name.
    pub script: Vec<(String, u32)>,
    /// Raw `NAME = expression` assignments (the map prints their value as 0).
    pub exprs: Vec<(String, String)>,
}

impl LinkMap {
    pub fn script_value(&self, name: &str) -> Option<u32> {
        self.script
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
    }

    /// Evaluate a linker-script expression made of numbers, symbols, `+` and `-`.
    pub fn eval(&self, expr: &str, depth: u32) -> Option<u32> {
        if depth > 8 {
            return None;
        }
        // `DEFINED(NAME) ? A : B`, the form psoxide.ld gives STACK_RESERVE: A when
        // the map carries an assignment to NAME (a script passed before the SDK's),
        // else B. A `--defsym` is not in the map, so it reads as the default.
        if let Some(rest) = expr.trim().strip_prefix("DEFINED(") {
            let (name, rest) = rest.split_once(')')?;
            let (then, otherwise) = rest.trim().strip_prefix('?')?.split_once(':')?;
            let defined = self.exprs.iter().any(|(n, _)| n == name.trim());
            return self.eval(if defined { then } else { otherwise }, depth + 1);
        }
        let mut sign = 1i64;
        let mut acc = 0i64;
        for tok in expr.split_whitespace() {
            match tok {
                "+" => sign = 1,
                "-" => sign = -1,
                t => {
                    let v = if let Some(h) = t.strip_prefix("0x") {
                        i64::from_str_radix(h, 16).ok()?
                    } else if let Some(k) = t.strip_suffix('K') {
                        k.parse::<i64>().ok()? * 1024
                    } else if let Some(m) = t.strip_suffix('M') {
                        m.parse::<i64>().ok()? * 1024 * 1024
                    } else if let Ok(n) = t.parse::<i64>() {
                        n
                    } else {
                        let (_, e) = self.exprs.iter().rev().find(|(n, _)| n == t)?;
                        self.eval(e, depth + 1)? as i64
                    };
                    acc += sign * v;
                    sign = 1;
                }
            }
        }
        Some(acc as u32)
    }

    /// `STACK_RESERVE`, the bytes the link keeps for the stack below `STACK_INIT`.
    #[must_use]
    pub fn stack_reserve(&self) -> Option<u32> {
        self.eval("STACK_RESERVE", 0)
    }

    /// `STACK_INIT`, the initial stack pointer this link wrote into the PS-X header
    /// (`RAM_BASE + RAM_SIZE - 0x100` in psoxide.ld): 0x801FFF00 for a stock link,
    /// lower for one that sets `RAM_SIZE_OVERRIDE` (a program hosted under the scene
    /// core's window). Read from the map's own assignments, so it is the number the
    /// link used, not a constant.
    ///
    /// An override passed with `--defsym` is not in the map and reads as the default,
    /// exactly as [`LinkMap::stack_reserve`] does: psoxide.ld asks for overrides in a
    /// `-T` file for that reason, and the scene core's link rule does the same. (The
    /// map's address column on an assignment line is the location counter, not the
    /// value, so the map holds no second copy to check against.) A map whose
    /// `STACK_INIT` cannot be evaluated (no psoxide.ld link, or a script whose
    /// `RAM_SIZE` only a `--defsym` gives) is an error, never the stock value.
    pub fn stack_init(&self) -> Result<u32, String> {
        self.eval("STACK_INIT", 0).ok_or_else(|| {
            "the map gives no STACK_INIT this tool can evaluate (not a psoxide.ld link, or its \
             RAM size comes from --defsym, which the map does not record)"
                .to_string()
        })
    }

    /// The top of the RAM the program links for, `RAM_BASE + RAM_SIZE` (0x80200000 for a
    /// stock link): [`LinkMap::stack_init`] plus the 0x100 psoxide.ld leaves above it.
    pub fn ram_top(&self) -> Result<u32, String> {
        let init = self.stack_init()?;
        let top = self.eval("RAM_BASE + RAM_SIZE", 0).ok_or(
            "the map gives no RAM_BASE + RAM_SIZE this tool can evaluate (not a psoxide.ld link)",
        )?;
        if top.wrapping_sub(init) != STACK_INIT_GAP {
            return Err(format!(
                "STACK_INIT {init:#010x} is not {STACK_INIT_GAP:#x} below RAM_BASE + RAM_SIZE \
                 {top:#010x}: not the psoxide.ld layout this tool reads"
            ));
        }
        Ok(top)
    }

    /// Bytes between the heap start and the stack reserve, the figure every
    /// builder reports as `heap_free`.
    pub fn heap_free(&self) -> Option<u32> {
        let start = self.script_value("__heap_start")?;
        let end = self
            .eval("STACK_INIT - STACK_RESERVE", 0)
            .or_else(|| self.script_value("__heap_end"))?;
        end.checked_sub(start)
    }
}

fn hex(field: &str) -> Option<u32> {
    u32::from_str_radix(field.trim(), 16).ok()
}

/// psoxide.ld's gap between `STACK_INIT` and the top of RAM.
pub const STACK_INIT_GAP: u32 = 0x100;

/// Parse a map. Lines that are not part of the image (debug sections, the
/// header row, script assignments with no address) are skipped.
pub fn parse(text: &str) -> LinkMap {
    let mut map = LinkMap::default();
    let mut current_out: Option<usize> = None;
    for line in text.lines() {
        if line.len() < 34 || !line.is_char_boundary(33) {
            continue;
        }
        let (prefix, rest) = line.split_at(33);
        let (Some(vma), Some(size)) = (hex(&prefix[0..8]), hex(&prefix[18..26])) else {
            continue;
        };
        let align: u32 = prefix[26..32].trim().parse().unwrap_or(1);
        let indent = rest.len() - rest.trim_start().len();
        let body = rest.trim_start();
        match indent {
            0 => {
                if let Some((name, expr)) = body.split_once(" = ") {
                    map.exprs
                        .push((name.trim().to_string(), expr.trim().to_string()));
                    // A script assignment at output level (`__image_end = ...`).
                    if name.starts_with("__") {
                        map.script.push((name.trim().to_string(), vma));
                    }
                    continue;
                }
                if vma == 0 && size != 0 && body.starts_with(".debug") {
                    current_out = None;
                    continue;
                }
                if vma == 0 {
                    current_out = None;
                    continue;
                }
                map.outputs.push(Output {
                    name: body.to_string(),
                    vma,
                    size,
                });
                current_out = Some(map.outputs.len() - 1);
            }
            8 => {
                let Some(out) = current_out else { continue };
                if let Some((object, section)) = split_input(body) {
                    map.sections.push(Section {
                        out: map.outputs[out].name.clone(),
                        object: object.to_string(),
                        name: section.to_string(),
                        vma,
                        size,
                        align,
                        syms: Vec::new(),
                    });
                } else if let Some((name, _)) = body.split_once(" = ") {
                    if name.starts_with("__") {
                        map.script.push((name.trim().to_string(), vma));
                    }
                }
            }
            16 => {
                if let Some(section) = map.sections.last_mut() {
                    section.syms.push(Sym {
                        name: body.to_string(),
                        vma,
                        size,
                    });
                }
            }
            _ => {}
        }
    }
    map
}

/// Split `path/to/x.o:(.text.foo)` into the object and the section name.
/// Paths may contain spaces and colons, so split at the last `:(`.
fn split_input(body: &str) -> Option<(&str, &str)> {
    let at = body.rfind(":(")?;
    let section = body[at + 2..].strip_suffix(')')?;
    Some((&body[..at], section))
}

/// The bytes of a PS-X EXE: header-relative addressing by load address.
pub struct Image {
    load: u32,
    bytes: Vec<u8>,
}

impl Image {
    pub fn parse(file: &[u8]) -> Result<Image, String> {
        if file.len() < 0x800 || &file[0..8] != b"PS-X EXE" {
            return Err("not a PS-X EXE".into());
        }
        let word = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        let load = word(0x18);
        let size = word(0x1c) as usize;
        let end = (0x800 + size).min(file.len());
        Ok(Image {
            load,
            bytes: file[0x800..end].to_vec(),
        })
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The bytes at `[vma, vma+size)` when they are inside the file image.
    pub fn at(&self, vma: u32, size: u32) -> Option<&[u8]> {
        let start = vma.checked_sub(self.load)? as usize;
        self.bytes.get(start..start + size as usize)
    }

    pub fn word(&self, vma: u32) -> Option<u32> {
        let b = self.at(vma, 4)?;
        Some(u32::from_le_bytes(b.try_into().unwrap()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
     VMA      LMA     Size Align Out     In      Symbol
       0        0        0     1 RAM_BASE = 0x80000000
80010000 80010000     1000     4 .text
80010000 80010000        0     1         __text_start = .
80010000 80010000       78     4         /a b/deps/g-1.o:(.text._start)
80010000 80010000       78     1                 _start
80010080 80010080       20     4         /a b/deps/g-1.o:(.rodata..Lanon.1.0)
800bd240 800bd240     d7c8    16 .data
800bd240 800bd240        6     1         g-1.o:(.data._RNvCs1_1g3FOO)
800bd240 800bd240        6     1                 g::FOO
800caa08 800caa08        0     1 __image_end = x
800caa08 800caa08     1000     8 .bss
801f9158 801f9158        0     1 __heap_start = .
801f9158 801f9158        0     1 __heap_end = STACK_INIT - STACK_RESERVE
       0        0     d5a0     4 .pdr
";

    #[test]
    fn parses_sections_symbols_and_script_values() {
        let m = parse(SAMPLE);
        assert_eq!(m.outputs.len(), 3);
        assert_eq!(m.sections.len(), 3);
        assert_eq!(m.sections[0].object, "/a b/deps/g-1.o");
        assert_eq!(m.sections[0].syms[0].name, "_start");
        assert_eq!(m.sections[2].name, ".data._RNvCs1_1g3FOO");
        assert_eq!(m.script_value("__image_end"), Some(0x800caa08));
        assert_eq!(m.heap_free(), Some(0));
    }

    #[test]
    fn stack_reserve_follows_the_sdk_scripts_override_hook() {
        let ld = "       0        0        0     1 STACK_RESERVE = DEFINED(STACK_RESERVE_OVERRIDE) ? STACK_RESERVE_OVERRIDE : 0x8000\n";
        let ov = "       0        0        0     1 STACK_RESERVE_OVERRIDE = 0xD000\n";
        // Nothing sets it: the SDK default.
        assert_eq!(parse(ld).stack_reserve(), Some(0x8000));
        // A game script before the SDK's, or after it in the map: the override.
        assert_eq!(parse(&format!("{ov}{ld}")).stack_reserve(), Some(0xd000));
        assert_eq!(parse(&format!("{ld}{ov}")).stack_reserve(), Some(0xd000));
    }

    /// The four psoxide.ld lines that set the RAM top, as lld prints them.
    const RAM_LINES: &str = "\
     VMA      LMA     Size Align Out     In      Symbol
       0        0        0     1 RAM_BASE = 0x80000000
       0        0        0     1 RAM_SIZE = DEFINED(RAM_SIZE_OVERRIDE) ? RAM_SIZE_OVERRIDE : 2M
       0        0        0     1 STACK_INIT = RAM_BASE + RAM_SIZE - 0x100
";

    #[test]
    fn the_stack_top_is_read_from_the_link_not_a_constant() {
        // A stock link: the SDK's 2 MB.
        let stock = parse(RAM_LINES);
        assert_eq!(stock.stack_init(), Ok(0x801f_ff00));
        assert_eq!(stock.ram_top(), Ok(0x8020_0000));
        // A program hosted under the scene core's window (CORE-ABI: RAM_SIZE_OVERRIDE =
        // 0x1FD800 in a -T file before psoxide.ld), in either order in the map.
        let ov = "       0        0        0     1 RAM_SIZE_OVERRIDE = 0x1FD800\n";
        for text in [format!("{ov}{RAM_LINES}"), format!("{RAM_LINES}{ov}")] {
            let hosted = parse(&text);
            assert_eq!(hosted.stack_init(), Ok(0x801f_d700));
            assert_eq!(hosted.ram_top(), Ok(0x801f_d800));
        }
    }

    #[test]
    fn a_stack_top_the_map_cannot_give_is_an_error_not_the_stock_value() {
        // No psoxide.ld assignments at all.
        assert!(parse(SAMPLE).stack_init().is_err());
        // A Psy-Q style script whose RAM size only a --defsym gives: never 0x801FFF00.
        let psyq = "\
     VMA      LMA     Size Align Out     In      Symbol
       0        0        0     1 RAM_BASE = 0x80000000
       0        0        0     1 RAM_SIZE = DEFINED(RAM_SIZE) ? RAM_SIZE : 0x200000
       0        0        0     1 STACK_INIT = RAM_BASE + RAM_SIZE - 0x100
";
        let err = parse(psyq).stack_init().unwrap_err();
        assert!(err.contains("--defsym"), "{err}");
        assert!(parse(psyq).ram_top().is_err());
    }
}
