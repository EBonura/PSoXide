//! Checked fixed-region MIPS overlays, independent of disc transport.
//!
//! A transport hands a complete, integrity-checked word buffer to [`Package::parse`].
//! The game reserves and pins the destination and every imported dependency,
//! copies the image, applies relocations, binds direct imports and flushes the
//! instruction cache before publishing entries. No callable pointer or static
//! data slice is returned here: consumers resolve data through their current
//! allocation handle and borrow it while the dependency set is pinned.

/// Existing HK module package magic. This format has five header words.
pub const MAGIC: u32 = 0x444d_4b48;

/// A malformed package or an unrepresentable relocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Header, image or fixup range is invalid.
    Bounds,
    /// An unsupported fixup kind was present.
    Kind,
    /// Code must occupy one signed-low-half 64 KiB window.
    Window,
    /// A direct jump cannot represent the requested target.
    Jump,
}

/// A checked package borrowing its completed transport buffer.
pub struct Package<'a> {
    /// Application-defined module identity.
    pub id: u32,
    /// Image words, including code and immutable data.
    pub image: &'a [u32],
    /// Encoded relocation records: kind in high four bits, byte offset below.
    pub fixups: &'a [u32],
    /// Exclusive code byte boundary inside the image.
    pub code_end: u32,
}

impl<'a> Package<'a> {
    /// Parse one package and return the remaining words in a packed CD chunk.
    pub fn parse(words: &'a [u32]) -> Result<(Self, &'a [u32]), Error> {
        let header = words.get(..5).ok_or(Error::Bounds)?;
        if header[0] != MAGIC {
            return Err(Error::Bounds);
        }
        let end_image = 5usize
            .checked_add(header[2] as usize)
            .ok_or(Error::Bounds)?;
        let end = end_image
            .checked_add(header[3] as usize)
            .ok_or(Error::Bounds)?;
        let image = words.get(5..end_image).ok_or(Error::Bounds)?;
        let fixups = words.get(end_image..end).ok_or(Error::Bounds)?;
        let bytes = image.len().checked_mul(4).ok_or(Error::Bounds)?;
        if bytes == 0 || header[4] as usize > bytes || header[4] & 3 != 0 {
            return Err(Error::Bounds);
        }
        for &fixup in fixups {
            let offset = fixup & 0x0fff_ffff;
            if offset & 3 != 0 || offset as usize >= bytes {
                return Err(Error::Bounds);
            }
            if !(1..=4).contains(&(fixup >> 28)) {
                return Err(Error::Kind);
            }
        }
        Ok((
            Self {
                id: header[1],
                image,
                fixups,
                code_end: header[4],
            },
            &words[end..],
        ))
    }
}

/// Centre used by signed MIPS low halves.
pub const fn window(address: u32) -> u32 {
    address.wrapping_add(0x8000) & 0xffff_0000
}

/// Validate a destination before copying code into its fixed region.
pub fn placement(address: u32, bytes: u32, code: bool) -> Result<(), Error> {
    let last = address
        .checked_add(bytes.checked_sub(1).ok_or(Error::Bounds)?)
        .ok_or(Error::Bounds)?;
    if address & 3 != 0 || bytes & 3 != 0 {
        return Err(Error::Bounds);
    }
    if code && window(address) != window(last) {
        return Err(Error::Window);
    }
    Ok(())
}

/// Relocate one checked image word, preserving its instruction opcode.
pub fn relocate(word: u32, kind: u32, linked: u32, loaded: u32, offset: u32) -> Result<u32, Error> {
    let delta = loaded.wrapping_sub(linked);
    Ok(match kind {
        1 => {
            let target = (linked & 0xf000_0000) | ((word & 0x03ff_ffff) << 2);
            direct(
                word,
                1,
                target.wrapping_add(delta),
                loaded.checked_add(offset).ok_or(Error::Bounds)?,
            )?
        }
        2 => (word & 0xffff_0000) | (window(loaded) >> 16),
        3 => {
            let offset =
                ((word & 0xffff) as i16 as i32).wrapping_sub((linked as u16 as i16) as i32) as u32;
            (word & 0xffff_0000)
                | (loaded.wrapping_add(offset).wrapping_sub(window(loaded)) & 0xffff)
        }
        4 => word.wrapping_add(delta),
        _ => return Err(Error::Kind),
    })
}

/// A dependency identity uses the residency engine's existing generation handle.
#[derive(Clone, Copy, Debug)]
pub struct Dependency {
    /// Transport resource identity.
    pub key: crate::ResourceKey,
    /// Generation captured when installation completed.
    pub handle: crate::Handle,
}

/// Borrow of a complete installed dependency set. The engine cannot evict,
/// compact or reload allocations while this exclusive borrow is held.
pub struct Admission<'a, const E: usize, const S: usize, const C: usize> {
    residency: &'a mut crate::Residency<E, S, C>,
    dependencies: &'a [Dependency],
}

impl<'a, const E: usize, const S: usize, const C: usize> Admission<'a, E, S, C> {
    /// Resolve every dependency as installed at the recorded generation before
    /// any entry is used. Publication is the existing finish_install boundary:
    /// callers bind direct imports and flush I-cache before finishing install.
    pub fn new(
        residency: &'a mut crate::Residency<E, S, C>,
        dependencies: &'a [Dependency],
    ) -> Option<Self> {
        for dependency in dependencies {
            if residency.resident_handle(dependency.key) != Some(dependency.handle) {
                return None;
            }
        }
        Some(Self {
            residency,
            dependencies,
        })
    }

    /// Resolve current pages, never a static slice or an unguarded cached entry.
    pub fn run(&self, index: usize) -> Option<crate::PageRun> {
        let dependency = self.dependencies.get(index)?;
        if self.residency.resident_handle(dependency.key) != Some(dependency.handle) {
            return None;
        }
        self.residency.try_run(dependency.handle)
    }
}

/// Bind a static import to a dependency entry or to the application's trap.
/// `site` is the real instruction address, needed for J26 region validation.
pub fn direct(word: u32, kind: u32, target: u32, site: u32) -> Result<u32, Error> {
    Ok(match kind {
        1 => {
            if target & 3 != 0 || target & 0xf000_0000 != site.wrapping_add(4) & 0xf000_0000 {
                return Err(Error::Jump);
            }
            (word & 0xfc00_0000) | ((target >> 2) & 0x03ff_ffff)
        }
        2 => (word & 0xffff_0000) | (window(target) >> 16),
        3 => (word & 0xffff_0000) | (target.wrapping_sub(window(target)) & 0xffff),
        4 => target,
        _ => return Err(Error::Kind),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_reject_outside_and_unknown_fixups() {
        let good = [MAGIC, 2, 1, 1, 4, 0x0800_0000, 0x1000_0000];
        let (package, rest) = Package::parse(&good).unwrap();
        assert_eq!(package.id, 2);
        assert!(rest.is_empty());
        let mut bad = good;
        bad[6] = 0x1000_0004;
        assert!(matches!(Package::parse(&bad), Err(Error::Bounds)));
        bad[6] = 0x5000_0000;
        assert!(matches!(Package::parse(&bad), Err(Error::Kind)));
        assert!(matches!(Package::parse(&good[..6]), Err(Error::Bounds)));
    }

    #[test]
    fn low_half_uses_actual_link_base() {
        // A target at linked+0x20 encoded against a base ending 0x1000.
        let low = relocate(0x2421_1020, 3, 0x8040_1000, 0x801d_9000, 0).unwrap();
        let high = relocate(0x3c01_8040, 2, 0x8040_1000, 0x801d_9000, 0).unwrap();
        assert_eq!(high, 0x3c01_801e);
        assert_eq!(low, 0x2421_9020);
        assert_eq!(
            relocate(0x8040_1020, 4, 0x8040_1000, 0x801d_9000, 0).unwrap(),
            0x801d_9020
        );
    }

    #[test]
    fn jump_checks_real_instruction_pc() {
        assert!(direct(0x0c00_0000, 1, 0x9000_0000, 0x8fff_fffc).is_ok());
        assert_eq!(
            direct(0x0c00_0000, 1, 0x9000_0000, 0x8fff_fff8),
            Err(Error::Jump)
        );
        assert_eq!(placement(0x801d_7ffc, 8, true), Err(Error::Window));
        assert!(placement(0x801d_8000, 0x10000, true).is_ok());
    }
}
