//! Checked packages and relocation for fixed-region CD-loaded code/data.
//!
//! The consumer owns CD transport, allocation and scene pinning. A complete,
//! integrity-checked transport buffer is parsed before any image word is copied
//! or relocated. Bind resident imports and flush the instruction cache before
//! publication; trap incoming imports and flush before eviction reuses memory.
//! This first format has no cross-module imports. Its borrowed views must not
//! outlive the transport buffer. Execution/data leases remain consumer policy.

/// Five-word package magic used by the existing HK module transport.
pub const MAGIC: u32 = 0x444d_4b48;

/// Invalid package, placement or relocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A header, image or fixup lies outside its declared range.
    Bounds,
    /// An unsupported relocation kind was present.
    Kind,
    /// Code spans more than one signed-low-half 64 KiB window.
    Window,
    /// A direct jump cannot represent its destination.
    Jump,
}

/// A checked module borrowing the complete transport buffer.
pub struct Package<'a> {
    /// Consumer-defined identity.
    pub id: u32,
    /// Code and immutable data words.
    pub image: &'a [u32],
    /// Encoded kind/byte-offset fixups.
    pub fixups: &'a [u32],
    /// Exclusive executable prefix boundary.
    pub code_end: u32,
}
impl<'a> Package<'a> {
    /// Parse one checked package and return the next package's words.
    /// Empty inlined-away modules and data-only modules are supported.
    pub fn parse(words: &'a [u32]) -> Result<(Self, &'a [u32]), Error> {
        let header = words.get(..5).ok_or(Error::Bounds)?;
        if header[0] != MAGIC {
            return Err(Error::Bounds);
        }
        let image_end = 5usize
            .checked_add(header[2] as usize)
            .ok_or(Error::Bounds)?;
        let end = image_end
            .checked_add(header[3] as usize)
            .ok_or(Error::Bounds)?;
        let image = words.get(5..image_end).ok_or(Error::Bounds)?;
        let fixups = words.get(image_end..end).ok_or(Error::Bounds)?;
        let bytes = image.len().checked_mul(4).ok_or(Error::Bounds)?;
        if header[4] as usize > bytes || header[4] & 3 != 0 {
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

/// Centre of the signed-low-half address window.
pub const fn window(address: u32) -> u32 {
    address.wrapping_add(0x8000) & 0xffff_0000
}

/// Check alignment/range and the code window before copying into a fixed arena.
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

/// Rebind a statically checked import or trap instruction. This computes a
/// word; storing it and synchronizing I-cache remain the consumer's responsibility.
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

/// Relocate a checked self-reference, retaining its instruction opcode.
/// `offset` is the actual instruction offset, including for J26 PC+4 rules.
pub fn relocate(word: u32, kind: u32, linked: u32, loaded: u32, offset: u32) -> Result<u32, Error> {
    let delta = loaded.wrapping_sub(linked);
    Ok(match kind {
        1 => {
            let source = linked.checked_add(offset).ok_or(Error::Bounds)?;
            let target = (source.wrapping_add(4) & 0xf000_0000) | ((word & 0x03ff_ffff) << 2);
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
                ((word as u16 as i16) as i32).wrapping_sub((linked as u16 as i16) as i32) as u32;
            (word & 0xffff_0000)
                | (loaded.wrapping_add(offset).wrapping_sub(window(loaded)) & 0xffff)
        }
        4 => word.wrapping_add(delta),
        _ => return Err(Error::Kind),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_bounds_empty_data_and_concatenation() {
        let words = [
            MAGIC,
            3,
            1,
            1,
            4,
            0x0800_0000,
            0x1000_0000,
            MAGIC,
            4,
            0,
            0,
            0,
        ];
        let (p, next) = Package::parse(&words).unwrap();
        assert_eq!(p.id, 3);
        assert_eq!(p.code_end, 4);
        assert!(Package::parse(next).unwrap().0.image.is_empty());
        for bad in [0x1000_0004, 0x1000_0001, 0x5000_0000] {
            let mut invalid = words;
            invalid[6] = bad;
            assert!(Package::parse(&invalid).is_err());
        }
        assert!(Package::parse(&words[..6]).is_err());
        assert!(Package::parse(&[MAGIC, 2, 1, 0, 0, 42]).is_ok());
    }
    #[test]
    fn arbitrary_link_low_half_and_real_jump_pc() {
        assert_eq!(
            relocate(0x2421_1020, 3, 0x8040_1000, 0x801d_9000, 0).unwrap(),
            0x2421_9020
        );
        assert_eq!(
            relocate(0x3c01_8040, 2, 0x8040_1000, 0x801d_9000, 0).unwrap(),
            0x3c01_801e
        );
        assert_eq!(
            relocate(0x8040_1020, 4, 0x8040_1000, 0x801d_9000, 0).unwrap(),
            0x801d_9020
        );
        assert_eq!(
            relocate(0x0c00_0000, 1, 0x8fff_fffc, 0x801d_8000, 0).unwrap(),
            direct(0x0c00_0000, 1, 0x801d_8004, 0x801d_8000).unwrap()
        );
        assert_eq!(
            direct(0x0c00_0000, 1, 0x9000_0000, 0x8fff_fff8),
            Err(Error::Jump)
        );
    }
    #[test]
    fn destination_window_and_arithmetic_bounds() {
        assert_eq!(placement(0x801d_7ffc, 8, true), Err(Error::Window));
        assert!(placement(0x801d_8000, 0x10000, true).is_ok());
        assert!(placement(0xffff_fffc, 8, false).is_err());
        assert!(placement(0x801d_8000, 0, true).is_err());
    }
}
