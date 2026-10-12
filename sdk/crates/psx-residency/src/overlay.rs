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
/// Extended package magic with an explicit link base and static imports.
pub const IMPORT_MAGIC: u32 = 0x324d_4b48;

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
    /// Explicit link base for extended packages; legacy callers supply theirs.
    pub linked: Option<u32>,
    imports: &'a [u32],
}

impl<'a> Package<'a> {
    /// Parse one package and return the remaining words in a packed CD chunk.
    pub fn parse(words: &'a [u32]) -> Result<(Self, &'a [u32]), Error> {
        let header = words.get(..5).ok_or(Error::Bounds)?;
        if header[0] != MAGIC && header[0] != IMPORT_MAGIC {
            return Err(Error::Bounds);
        }
        let extended = header[0] == IMPORT_MAGIC;
        let header_words = if extended { 7usize } else { 5 };
        let extension = if extended {
            words.get(5..7).ok_or(Error::Bounds)?
        } else {
            &[]
        };
        let end_image = header_words
            .checked_add(header[2] as usize)
            .ok_or(Error::Bounds)?;
        let end_fixups = end_image
            .checked_add(header[3] as usize)
            .ok_or(Error::Bounds)?;
        let import_words = if extended {
            (extension[0] as usize)
                .checked_mul(3)
                .ok_or(Error::Bounds)?
        } else {
            0
        };
        let end = end_fixups.checked_add(import_words).ok_or(Error::Bounds)?;
        let image = words.get(header_words..end_image).ok_or(Error::Bounds)?;
        let fixups = words.get(end_image..end_fixups).ok_or(Error::Bounds)?;
        let imports = words.get(end_fixups..end).ok_or(Error::Bounds)?;
        let bytes = image.len().checked_mul(4).ok_or(Error::Bounds)?;
        if header[4] as usize > bytes
            || header[4] & 3 != 0
            || (bytes == 0 && (!fixups.is_empty() || !imports.is_empty()))
        {
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
        for import in imports.chunks_exact(3) {
            let offset = import[0] & 0x0fff_ffff;
            if offset & 3 != 0 || offset as usize >= bytes || import[1] > u16::MAX as u32 {
                return Err(Error::Bounds);
            }
            if !(1..=4).contains(&(import[0] >> 28)) {
                return Err(Error::Kind);
            }
        }
        if extended && extension[1] & 3 != 0 {
            return Err(Error::Bounds);
        }
        Ok((
            Self {
                id: header[1],
                image,
                fixups,
                code_end: header[4],
                linked: extended.then(|| extension[1]),
                imports,
            },
            &words[end..],
        ))
    }

    /// Ordered direct imports. Target bounds are checked against the admitted
    /// dependency descriptor before any patch is written.
    pub fn imports(&self) -> impl Iterator<Item = Import> + '_ {
        self.imports.chunks_exact(3).map(|record| Import {
            site: record[0] & 0x0fff_ffff,
            kind: (record[0] >> 28) as u8,
            dependency: record[1] as u16,
            target: record[2],
        })
    }
    /// Borrow raw import records for a bounded incremental installer.
    pub fn import_words(&self) -> &'a [u32] {
        self.imports
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
            let source_pc = linked.checked_add(offset).ok_or(Error::Bounds)?;
            let target = (source_pc.wrapping_add(4) & 0xf000_0000) | ((word & 0x03ff_ffff) << 2);
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

/// Static direct import emitted by the host linker. No function-pointer call
/// is introduced: the installed instruction retains a statically checked target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Import {
    /// Byte offset of the word to patch inside its source module.
    pub site: u32,
    /// Index into the application's admitted dependency table.
    pub dependency: u16,
    /// Byte offset inside that dependency's image.
    pub target: u32,
    /// J26=1, HI16=2, LO16=3, W32=4.
    pub kind: u8,
}

impl Import {
    /// Check a J26 against the linked executable prefix as well as image bounds.
    /// Data imports retain their full image range.
    pub fn validate_code(
        self,
        source_bytes: u32,
        target_bytes: u32,
        target_code_end: u32,
    ) -> Result<(), Error> {
        self.validate(source_bytes, target_bytes)?;
        if target_code_end > target_bytes || (self.kind == 1 && self.target >= target_code_end) {
            return Err(Error::Jump);
        }
        Ok(())
    }
    /// Check both image bounds before installation patches any word. Host
    /// hardening additionally verifies the target symbol ABI and direct-call tree.
    pub fn validate(self, source_bytes: u32, target_bytes: u32) -> Result<(), Error> {
        if self.site & 3 != 0
            || self
                .site
                .checked_add(4)
                .is_none_or(|end| end > source_bytes)
            || self.target >= target_bytes
        {
            return Err(Error::Bounds);
        }
        if !(1..=4).contains(&self.kind) {
            return Err(Error::Kind);
        }
        if self.kind == 1 && self.target & 3 != 0 {
            return Err(Error::Jump);
        }
        Ok(())
    }

    /// Compute a patch only after both admitted allocation bounds are known.
    /// The application owns the actual store and subsequent I-cache flush.
    pub fn bind(
        self,
        word: u32,
        source: u32,
        source_bytes: u32,
        target: u32,
        target_bytes: u32,
        target_code_end: u32,
    ) -> Result<u32, Error> {
        self.validate_code(source_bytes, target_bytes, target_code_end)?;
        direct(
            word,
            self.kind as u32,
            target.checked_add(self.target).ok_or(Error::Bounds)?,
            source.checked_add(self.site).ok_or(Error::Bounds)?,
        )
    }
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

    /// Borrow current bytes for no longer than this executing admission and its
    /// backing buffer. No copyable run or static slice escapes the lease.
    pub fn bytes<'b, const P: usize>(
        &'b self,
        buffer: &'b crate::PageBuffer<P>,
        index: usize,
        len: usize,
    ) -> Option<&'b [u8]> {
        let dependency = self.dependencies.get(index)?;
        if self.residency.resident_handle(dependency.key) != Some(dependency.handle) {
            return None;
        }
        let run = self.residency.try_run(dependency.handle)?;
        if len > self.residency.payload_bytes(dependency.key) {
            return None;
        }
        buffer.bytes(run, len)
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

    struct Disc;
    impl crate::Host for Disc {
        fn extent(&self, _: crate::ResourceKey) -> Option<crate::Extent> {
            Some(crate::Extent {
                lba: 1,
                sectors: 1,
                pages: 1,
                bytes: 4,
            })
        }
        fn walk_distance(&self, _: crate::ResourceKey) -> u32 {
            0
        }
        fn submit(&mut self, _: &crate::ReadRequest) -> crate::Submit {
            crate::Submit::Accepted
        }
        fn cancel(&mut self, _: crate::RequestId) {}
    }

    #[test]
    fn admission_requires_entire_current_installed_dependency_set() {
        use crate::*;
        let mut config = Config::<1>::new();
        config.pools[0] = PoolConfig {
            capacity_pages: 4,
            placement: Placement::FirstFit,
        };
        config.categories[0] = CategoryConfig {
            pool: 0,
            max_pages: 4,
            max_entries: 4,
            movable: true,
            installs: true,
            keep_radius: 0,
        };
        let mut engine = Residency::<4, 4, 1>::new(config);
        let first = ResourceKey::new(Category::WORLD_REGION, 0).unwrap();
        let second = ResourceKey::new(Category::WORLD_REGION, 1).unwrap();
        let mut disc = Disc;
        let mut evictions = [];
        engine.step(
            &[
                Wanted::new(first, Class::Demand, 0),
                Wanted::new(second, Class::Demand, 0),
            ],
            &mut disc,
            &mut evictions,
        );
        engine.complete_read(engine.request_of(first).unwrap(), ReadOutcome::Done);
        engine.complete_read(engine.request_of(second).unwrap(), ReadOutcome::Done);
        let one = Dependency {
            key: first,
            handle: engine.begin_install(first).unwrap(),
        };
        let two = Dependency {
            key: second,
            handle: engine.begin_install(second).unwrap(),
        };
        assert!(engine.finish_install(first));
        let dependencies = [one, two];
        assert!(Admission::new(&mut engine, &dependencies).is_none());
        assert!(engine.finish_install(second));
        let buffer = PageBuffer::<4>::new();
        {
            let admitted = Admission::new(&mut engine, &dependencies).unwrap();
            assert_eq!(admitted.bytes(&buffer, 0, 4), Some(&[0u8; 4][..]));
            assert!(admitted.bytes(&buffer, 0, 5).is_none());
        }
        let mut evictions = [Eviction::NONE; 4];
        assert_eq!(engine.evict_unlisted(&[], &mut evictions), 2);
        assert!(Admission::new(&mut engine, &dependencies).is_none());
        engine.step(
            &[Wanted::new(first, Class::Demand, 0)],
            &mut disc,
            &mut evictions,
        );
        engine.complete_read(engine.request_of(first).unwrap(), ReadOutcome::Done);
        engine.begin_install(first).unwrap();
        assert!(engine.finish_install(first));
        assert_ne!(engine.resident_handle(first), Some(one.handle));
        assert!(Admission::new(&mut engine, &[one]).is_none());
    }

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
        assert_eq!(
            relocate(0x0c00_0000, 1, 0x8fff_fffc, 0x801d_8000, 0).unwrap(),
            direct(0x0c00_0000, 1, 0x801d_8004, 0x801d_8000).unwrap()
        );
        assert!(direct(0x0c00_0000, 1, 0x9000_0000, 0x8fff_fffc).is_ok());
        assert_eq!(
            direct(0x0c00_0000, 1, 0x9000_0000, 0x8fff_fff8),
            Err(Error::Jump)
        );
        assert_eq!(placement(0x801d_7ffc, 8, true), Err(Error::Window));
        assert!(placement(0x801d_8000, 0x10000, true).is_ok());
    }
}
