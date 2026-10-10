//! Code modules: code linked at one address whose bytes sit at another.
//!
//! A streamed code module is linked at an address of its own, in the RAM
//! mirrors above the 2 MiB a load can occupy (nothing ever executes there),
//! and the game copies it to wherever its pool has room. Its `j`/`jal`
//! targets, switch tables and trampolines are all in link addresses. A
//! post-link check has to read the module the way it will run, at the link
//! address, while the bytes it checks sit somewhere inside the image the
//! file holds, usually appended after the resident load.
//!
//! `--module LINK_LO..LINK_HI@IMAGE_AT` declares that: the bytes
//! `IMAGE_AT..IMAGE_AT + (LINK_HI - LINK_LO)` of the load are the module's
//! code and data at link addresses `LINK_LO..LINK_HI`. The tools run over an
//! [`Overlay`], the load with each module moved to its link address, so a
//! branch target, a jump table entry or a trampoline address is a link
//! address throughout, and the module's bytes are copied back to where they
//! sit when a patch is written. `--code` ranges inside a module's link range
//! name the module's code (and its trampoline array) in link addresses.

use std::io::Write;

use crate::listing::HEADER;
use crate::text::int_hex;

/// The lowest link address of a module: RAM is 2 MiB, so everything from
/// here to the top of the 8 MiB KSEG0 window is a mirror of it.
pub const LINK_WINDOW_LO: i64 = 0x8020_0000;
/// One past the highest link address of a module.
pub const LINK_WINDOW_HI: i64 = 0x8080_0000;

/// One `--module`: the code module linked at `link` whose bytes start at
/// `at` in the load.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Module {
    /// `[lo, hi)` link addresses.
    pub link: (i64, i64),
    /// The load address at which the bytes of `link.0` sit.
    pub at: i64,
}

impl Module {
    /// Bytes in the module.
    pub fn len(&self) -> i64 {
        self.link.1 - self.link.0
    }

    /// True when the module holds no bytes (parsing refuses one).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True when `[lo, hi)` lies inside the module's link range.
    pub fn links(&self, lo: i64, hi: i64) -> bool {
        self.link.0 <= lo && lo < hi && hi <= self.link.1
    }

    /// `[lo, hi)` load addresses of the module's bytes.
    fn placed(&self) -> (i64, i64) {
        (self.at, self.at + self.len())
    }
}

fn overlap(a: (i64, i64), b: (i64, i64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// One `--module` value, `LINK_LO..LINK_HI@IMAGE_AT` in hex (`0x` optional).
fn parse(value: &str) -> Result<Module, String> {
    let bad = || format!("bad --module {value}: want LINK_LO..LINK_HI@IMAGE_AT, in hex");
    let (range, at) = value.split_once('@').ok_or_else(bad)?;
    let (lo, hi) = range.split_once("..").ok_or_else(bad)?;
    let hex = |s: &str| {
        if s.starts_with(['-', '+']) {
            None
        } else {
            int_hex(s)
        }
    };
    let (Some(lo), Some(hi), Some(at)) = (hex(lo), hex(hi), hex(at)) else {
        return Err(bad());
    };
    if lo >= hi {
        return Err(format!(
            "bad --module {value}: the link range {lo:#x}..{hi:#x} is empty"
        ));
    }
    if lo % 4 != 0 || hi % 4 != 0 || at % 4 != 0 {
        return Err(format!(
            "bad --module {value}: the link range and the placement must be word aligned"
        ));
    }
    if !(LINK_WINDOW_LO <= lo && hi <= LINK_WINDOW_HI) {
        return Err(format!(
            "bad --module {value}: the link range {lo:#x}..{hi:#x} is not in the RAM mirrors \
             {LINK_WINDOW_LO:#x}..{LINK_WINDOW_HI:#x}, where no load has bytes of its own"
        ));
    }
    Ok(Module { link: (lo, hi), at })
}

/// The modules of every `--module LINK_LO..LINK_HI@IMAGE_AT` in `args`.
/// `Err` names a bad value, or two modules whose link ranges or placements
/// overlap. Where the placement lies in the file is [`Overlay::new`]'s check.
pub fn modules(args: &[String]) -> Result<Vec<Module>, String> {
    let mut found: Vec<Module> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg != "--module" {
            continue;
        }
        let value = it
            .next()
            .ok_or("--module needs LINK_LO..LINK_HI@IMAGE_AT")?;
        let module = parse(value)?;
        for other in &found {
            if overlap(module.link, other.link) {
                return Err(format!(
                    "bad --module {value}: its link range overlaps the module linked at {:#x}..{:#x}",
                    other.link.0, other.link.1
                ));
            }
            if overlap(module.placed(), other.placed()) {
                return Err(format!(
                    "bad --module {value}: its bytes overlap those of the module linked at {:#x}..{:#x}",
                    other.link.0, other.link.1
                ));
            }
        }
        found.push(module);
    }
    Ok(found)
}

/// A load with its modules at their link addresses.
///
/// [`Overlay::expand`] gives the tools the image to work on: the file's
/// bytes, zero-filled up to the end of the highest module, with each
/// module's bytes moved from where they sit to their link address (the
/// placement is zeroed, so no copy of a module is left to be found twice).
/// [`Overlay::collapse`] puts the result back in the file's layout.
#[derive(Debug)]
pub struct Overlay {
    modules: Vec<Module>,
}

impl Overlay {
    /// The overlay of `modules` over `file`, the bytes of a PS-EXE loaded at
    /// `base`. `code` is the image's executable ranges, `--code` ranges
    /// inside a module's link range included. Reports to `out` and returns
    /// `Err(2)` when a module's bytes are not wholly inside the load, or
    /// cover code that is not the module's own.
    pub fn new(
        modules: Vec<Module>,
        file: &[u8],
        base: i64,
        code: &[(i64, i64)],
        out: &mut dyn Write,
    ) -> Result<Self, i32> {
        let load_end = base + file.len() as i64 - HEADER;
        for module in &modules {
            let (lo, hi) = module.placed();
            let name = format!(
                "--module {:#x}..{:#x}@{:#x}",
                module.link.0, module.link.1, module.at
            );
            if lo < base || load_end < hi {
                let _ = writeln!(
                    out,
                    "bad {name}: its bytes {lo:#x}..{hi:#x} are not inside the load {base:#x}..{load_end:#x}"
                );
                return Err(2);
            }
            if overlap(module.link, (base, load_end)) {
                let _ = writeln!(
                    out,
                    "bad {name}: its link range lies inside the load {base:#x}..{load_end:#x}"
                );
                return Err(2);
            }
            // Code is read and written only inside its ranges; the bytes a
            // module sits on must not be the resident code's.
            let resident = |&(clo, chi): &(i64, i64)| {
                !modules.iter().any(|m| m.links(clo, chi)) && overlap((clo, chi), (lo, hi))
            };
            if let Some(&(clo, chi)) = code.iter().find(|&r| resident(r)) {
                let _ = writeln!(
                    out,
                    "bad {name}: its bytes {lo:#x}..{hi:#x} cover the code {clo:#x}..{chi:#x}"
                );
                return Err(2);
            }
        }
        Ok(Self { modules })
    }

    /// True when `address` may hold the trampoline array a patch uses: any
    /// address without modules, else one inside a module's link range.
    pub fn holds(&self, address: i64) -> bool {
        self.modules.is_empty() || self.modules.iter().any(|m| m.links(address, address + 1))
    }

    fn offset(address: i64, base: i64) -> usize {
        (address - base + HEADER) as usize
    }

    /// The image the checks read and patch, addressed from `base` like the
    /// file.
    pub fn expand(&self, file: &[u8], base: i64) -> Vec<u8> {
        let mut image = file.to_vec();
        let end = self
            .modules
            .iter()
            .map(|m| Self::offset(m.link.1, base))
            .max()
            .unwrap_or(0);
        if image.len() < end {
            image.resize(end, 0);
        }
        for module in &self.modules {
            let from = Self::offset(module.at, base);
            let to = Self::offset(module.link.0, base);
            let len = module.len() as usize;
            let bytes = file[from..from + len].to_vec();
            image[from..from + len].fill(0);
            image[to..to + len].copy_from_slice(&bytes);
        }
        image
    }

    /// The file with the checks' result in it: `image`'s bytes up to the
    /// file's `length`, each module back where it sits.
    pub fn collapse(&self, image: &[u8], length: usize, base: i64) -> Vec<u8> {
        let mut file = image[..length].to_vec();
        for module in &self.modules {
            let from = Self::offset(module.at, base);
            let to = Self::offset(module.link.0, base);
            let len = module.len() as usize;
            file[from..from + len].copy_from_slice(&image[to..to + len]);
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn values_parse_in_hex_with_or_without_the_prefix() {
        let found = modules(&args(&["--module", "0x80408000..8040d480@801c0000"])).unwrap();
        assert_eq!(
            found,
            [Module {
                link: (0x8040_8000, 0x8040_d480),
                at: 0x801c_0000
            }]
        );
        assert_eq!(found[0].len(), 0x5480);
        assert!(!found[0].is_empty());
        assert!(modules(&[]).unwrap().is_empty());
    }

    #[test]
    fn bad_values_are_named() {
        for bad in [
            "80408000..8040d480",
            "80408000@801c0000",
            "-80408000..8040d480@801c0000",
            "80408000..zz@801c0000",
            "8040d480..80408000@801c0000",
            "80408002..8040d480@801c0000",
            "80408000..8040d480@801c0002",
            "801f0000..80208000@801c0000",
            "80800000..80800100@801c0000",
        ] {
            let error = modules(&args(&["--module", bad])).unwrap_err();
            assert!(error.contains(bad), "{error}");
        }
        assert!(modules(&args(&["--module"])).is_err());
    }

    #[test]
    fn expand_and_collapse_round_trip() {
        let base = 0x8001_0000;
        // A 0x20-byte payload, a 0x10-byte module after it.
        let mut file = vec![0u8; HEADER as usize + 0x30];
        for (i, b) in file[HEADER as usize..].iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        let module = Module {
            link: (0x8041_0000, 0x8041_0010),
            at: base + 0x20,
        };
        let overlay = Overlay::new(
            vec![module],
            &file,
            base,
            &[(base, base + 0x20)],
            &mut Vec::new(),
        )
        .unwrap();
        let image = overlay.expand(&file, base);
        let link = (0x8041_0000 - base + HEADER) as usize;
        assert_eq!(image.len(), link + 0x10);
        // Moved, not copied.
        assert_eq!(image[link..link + 0x10], file[HEADER as usize + 0x20..]);
        assert!(image[HEADER as usize + 0x20..HEADER as usize + 0x30]
            .iter()
            .all(|&b| b == 0));
        assert_eq!(overlay.collapse(&image, file.len(), base), file);
        // A change at the link address lands where the bytes sit.
        let mut changed = image.clone();
        changed[link + 4] = 0xEE;
        let back = overlay.collapse(&changed, file.len(), base);
        assert_eq!(back[HEADER as usize + 0x24], 0xEE);
        assert_eq!(back.len(), file.len());
        assert!(overlay.holds(0x8041_0004) && !overlay.holds(base));
    }
}
