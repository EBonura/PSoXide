// SPDX-License-Identifier: GPL-2.0-or-later
//! Borrowed code/data arenas with generation-bound execution admission.
//!
//! Transport completes in caller-owned staging bytes before `install`. No
//! arena storage is allocated here. Allocation uses the existing PagePool
//! generations with one allocation unit per byte, not sector rounding.
//! CPU entry calls borrow the registry and backing arena for their full scope.
//! DMA, renderer and other external users must be quiescent before unload.

use crate::{overlay, Handle, PagePool, Placement};

/// Admission or installation failure; no callable entry is published on failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Invalid package or relocation.
    Format(overlay::Error),
    /// The supplied arena has a different range or alignment.
    Arena,
    /// No legal fixed-window range is free.
    Space,
    /// An allocation was evicted or reused.
    Stale,
    /// A dependency has not completed installation.
    NotReady,
    /// Immutable package identity or checksum differs.
    Integrity,
}
impl From<overlay::Error> for Error {
    fn from(error: overlay::Error) -> Self {
        Self::Format(error)
    }
}

/// A view of an existing word-aligned arena, including its MIPS address.
/// Host tests may supply a model address; executing code requires its actual
/// physical address, checked on MIPS at construction.
pub struct CallerArena<'a> {
    words: &'a mut [u32],
    address: u32,
}
impl<'a> CallerArena<'a> {
    /// Borrow existing storage. This creates no owned buffer or reservation.
    pub fn new(words: &'a mut [u32], address: u32) -> Result<Self, Error> {
        let bytes = u32::try_from(words.len().checked_mul(4).ok_or(Error::Arena)?)
            .map_err(|_| Error::Arena)?;
        if address & 3 != 0 || bytes == 0 || address.checked_add(bytes).is_none() {
            return Err(Error::Arena);
        }
        #[cfg(target_arch = "mips")]
        if words.as_ptr() as usize as u32 != address {
            return Err(Error::Arena);
        }
        Ok(Self { words, address })
    }
}

/// Trusted descriptor emitted alongside the same-compilation package.
#[derive(Clone, Copy)]
pub struct Descriptor {
    /// Package identity.
    pub id: u32,
    /// Original linked address of its image.
    pub linked: u32,
    /// Checksum over all package words, before relocation.
    pub checksum: u32,
}

/// Final-link-patched resident metadata; keep initialized storage in `.data`.
/// This fixed layout avoids a second compilation moving typed callsites.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PackedDescriptor {
    /// Complete package word count, excluding CD sector padding.
    pub words: u32,
    /// Module identity.
    pub id: u32,
    /// Original linked image address.
    pub linked: u32,
    /// Complete package checksum before relocation.
    pub checksum: u32,
    /// Number of live records in the fixed-capacity resident call table.
    pub calls: u32,
}

/// Final-link-patched typed callsite; selector is the declared call-pair index.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PackedCall {
    /// Stable caller/export pair index from packaging arguments.
    pub selector: u32,
    /// Resident J/JAL instruction address.
    pub site: u32,
    /// Cold export byte offset.
    pub offset: u32,
}
/// Word checksum matching the existing HK module transport.
pub fn checksum(words: &[u32]) -> u32 {
    words.iter().fold(0x811c9dc5, |hash, word| {
        (hash ^ word).wrapping_mul(16777619)
    })
}
#[derive(Clone, Copy)]
struct Installed {
    id: u32,
    code_end: u32,
    ready: bool,
}
const EMPTY: Installed = Installed {
    id: 0,
    code_end: 0,
    ready: false,
};

/// Small persistent allocation/publication registry. Keep it across title,
/// gameplay and title re-admission so old handles cannot become current again.
pub struct Registry<const SLOTS: usize> {
    pool: PagePool<SLOTS>,
    installed: [Installed; SLOTS],
    address: Option<u32>,
}
impl<const SLOTS: usize> Registry<SLOTS> {
    /// Byte capacity of the existing arena, with at most `SLOTS` allocations.
    pub const fn new(pool: u8, capacity_bytes: u32) -> Self {
        Self {
            pool: PagePool::new(pool, capacity_bytes, Placement::FirstFit),
            installed: [EMPTY; SLOTS],
            address: None,
        }
    }
    fn check_arena(&self, arena: &CallerArena<'_>) -> Result<(), Error> {
        if arena.words.len() * 4 != self.pool.capacity_pages() as usize
            || self.address.is_some_and(|address| address != arena.address)
        {
            return Err(Error::Arena);
        }
        Ok(())
    }
    fn place(&mut self, arena: &CallerArena<'_>, bytes: u32, code: bool) -> Result<Handle, Error> {
        if bytes == 0 || bytes & 3 != 0 || (code && bytes > 0x10000) {
            return Err(Error::Space);
        }
        let mut at = 0u32;
        loop {
            let end = at
                .checked_add(bytes)
                .filter(|end| *end <= self.pool.capacity_pages())
                .ok_or(Error::Space)?;
            let address = arena.address.checked_add(at).ok_or(Error::Space)?;
            if overlay::placement(address, bytes, code).is_err() {
                if !code {
                    return Err(Error::Space);
                }
                at = overlay::window(address)
                    .checked_add(0x8000)
                    .and_then(|next| next.checked_sub(arena.address))
                    .ok_or(Error::Space)?;
                continue;
            }
            let overlap = self
                .pool
                .iter_runs()
                .find(|(_, run, _)| at < run.end_page() && run.first_page < end);
            if let Some((_, run, _)) = overlap {
                at = run.end_page();
                continue;
            }
            return self
                .pool
                .allocate_at(at, bytes)
                .map(|(handle, _)| handle)
                .ok_or(Error::Space);
        }
    }
    /// Install a completed, checked payload. Copy, relocation and I-cache
    /// synchronization all finish before the handle becomes admissible.
    pub fn install(
        &mut self,
        arena: &mut CallerArena<'_>,
        words: &[u32],
        descriptor: Descriptor,
    ) -> Result<Handle, Error> {
        self.check_arena(arena)?;
        let (package, rest) = overlay::Package::parse(words)?;
        if !rest.is_empty() || package.id != descriptor.id || checksum(words) != descriptor.checksum
        {
            return Err(Error::Integrity);
        }
        if package.image.is_empty() {
            return Err(Error::Space);
        }
        let bytes = u32::try_from(package.image.len() * 4).map_err(|_| Error::Space)?;
        let handle = self.place(arena, bytes, package.code_end != 0)?;
        let run = self.pool.try_run(handle).ok_or(Error::Stale)?;
        let start = run.first_page as usize / 4;
        let image = &mut arena.words[start..start + package.image.len()];
        image.copy_from_slice(package.image);
        let address = arena.address + run.first_page;
        for &fixup in package.fixups {
            let offset = fixup & 0x0fff_ffff;
            let index = offset as usize / 4;
            match overlay::relocate(
                image[index],
                fixup >> 28,
                descriptor.linked,
                address,
                offset,
            ) {
                Ok(word) => image[index] = word,
                Err(error) => {
                    self.pool.free(handle);
                    return Err(error.into());
                }
            }
        }
        flush();
        self.installed[handle.slot() as usize] = Installed {
            id: descriptor.id,
            code_end: package.code_end,
            ready: true,
        };
        self.address = Some(arena.address);
        Ok(handle)
    }
    /// Borrow the arena and registry until every CPU call/data view completes.
    /// All dependencies are checked before any admission is returned.
    pub fn admit<'a, 'buffer>(
        &'a mut self,
        arena: &'a mut CallerArena<'buffer>,
        dependencies: &'a [Handle],
    ) -> Result<Admission<'a, 'buffer, SLOTS>, Error> {
        self.check_arena(arena)?;
        for &handle in dependencies {
            if !self.pool.is_current(handle) {
                return Err(Error::Stale);
            }
            if !self.installed[handle.slot() as usize].ready {
                return Err(Error::NotReady);
            }
        }
        Ok(Admission {
            registry: self,
            arena,
            dependencies,
        })
    }
    /// Remove an allocation after external DMA/render users have completed.
    /// A live admission prevents this mutable registry access in safe Rust.
    pub fn unload(&mut self, handle: Handle) -> bool {
        if !self.pool.free(handle) {
            return false;
        }
        self.installed[handle.slot() as usize] = EMPTY;
        true
    }
    /// Quiescent phase handoff before gameplay overwrites title code storage.
    /// Persistent PagePool generations survive the next title re-admission.
    pub fn invalidate_all(&mut self) {
        for slot in 0..SLOTS {
            let handle = self
                .pool
                .iter_runs()
                .find(|(candidate, _, _)| *candidate as usize == slot)
                .and_then(|(_, _, _)| self.pool.handle_for_slot(slot as u16));
            if let Some(handle) = handle {
                self.unload(handle);
            }
        }
    }
}

/// A known resident direct-call instruction, not an escaped function pointer.
pub struct StaticImport<'a> {
    word: &'a mut u32,
    site: u32,
    module: Handle,
    offset: u32,
}
impl<'a> StaticImport<'a> {
    /// Bind only a J/JAL emitted by the same compilation as the packaged Rust
    /// entry. The typed caller and callee must share their exact ABI/types.
    ///
    /// # Safety
    /// `word` must name the actual writable resident instruction at `site`.
    /// Its typed callee must be the packaged export at `offset`; no other CPU
    /// or DMA user may read/write the instruction during rebinding.
    pub unsafe fn new(word: &'a mut u32, site: u32, module: Handle, offset: u32) -> Self {
        Self {
            word,
            site,
            module,
            offset,
        }
    }
}

/// Execution/data lease. Its fields deliberately do not expose raw code
/// addresses or an owned run that could escape the admission borrow.
pub struct Admission<'a, 'buffer, const SLOTS: usize> {
    registry: &'a mut Registry<SLOTS>,
    arena: &'a mut CallerArena<'buffer>,
    dependencies: &'a [Handle],
}
impl<const SLOTS: usize> Admission<'_, '_, SLOTS> {
    /// Read only through this live admission, never through a cached static view.
    pub fn words(&self, handle: Handle) -> Result<&[u32], Error> {
        if !self.dependencies.contains(&handle) {
            return Err(Error::NotReady);
        }
        let run = self.registry.pool.try_run(handle).ok_or(Error::Stale)?;
        Ok(&self.arena.words[run.first_page as usize / 4..run.end_page() as usize / 4])
    }
    /// Installed logical identity, under the same generation-bound lease.
    pub fn id(&self, handle: Handle) -> Result<u32, Error> {
        self.words(handle)?;
        Ok(self.registry.installed[handle.slot() as usize].id)
    }
    /// Run a statically typed same-compilation caller while its imports are
    /// bound. Trap all sites again and flush I-cache on normal/unwind exit.
    ///
    /// # Safety
    /// The closure may call only the exact packaged Rust entries matching the
    /// supplied sites/offsets. It must not retain module callbacks or pointers,
    /// nor start asynchronous users of module bytes beyond this scope.
    pub unsafe fn invoke<R>(
        &self,
        imports: &mut [StaticImport<'_>],
        trap: u32,
        caller: impl FnOnce() -> R,
    ) -> Result<R, Error> {
        for import in imports.iter() {
            self.words(import.module)?;
            let info = self.registry.installed[import.module.slot() as usize];
            if import.site & 3 != 0
                || import.offset & 3 != 0
                || import.offset >= info.code_end
                || !matches!(*import.word >> 26, 2 | 3)
            {
                return Err(Error::Integrity);
            }
            let run = self
                .registry
                .pool
                .try_run(import.module)
                .ok_or(Error::Stale)?;
            overlay::direct(
                *import.word,
                1,
                self.arena.address + run.first_page + import.offset,
                import.site,
            )?;
            overlay::direct(*import.word, 1, trap, import.site)?;
        }
        for import in imports.iter_mut() {
            let run = self
                .registry
                .pool
                .try_run(import.module)
                .ok_or(Error::Stale)?;
            *import.word = overlay::direct(
                *import.word,
                1,
                self.arena.address + run.first_page + import.offset,
                import.site,
            )?;
        }
        flush_sites(imports);
        let guard = CallScope { imports, trap };
        let result = caller();
        drop(guard);
        Ok(result)
    }
}
struct CallScope<'a, 'instruction> {
    imports: &'a mut [StaticImport<'instruction>],
    trap: u32,
}
impl Drop for CallScope<'_, '_> {
    fn drop(&mut self) {
        for import in self.imports.iter_mut() {
            // Both target and trap were checked before any site was published.
            *import.word =
                overlay::direct(*import.word, 1, self.trap, import.site).expect("validated trap");
        }
        flush_sites(self.imports);
    }
}
/// Make the patched call-site words visible to instruction fetch: only the cache lines that hold
/// them are invalidated (the rest of the cache keeps its working set; a full flush after every
/// patch and every restore cost a cold cache twice per lease).
fn flush_sites(imports: &[StaticImport<'_>]) {
    for import in imports {
        #[cfg(target_arch = "mips")]
        psx_rt::cache::flush_instruction_cache_range(import.site, import.site + 4);
        #[cfg(not(target_arch = "mips"))]
        let _ = import;
    }
    #[cfg(target_arch = "mips")]
    // SAFETY: empty asm only constrains compiler memory ordering.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "mips"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}
fn flush() {
    #[cfg(target_arch = "mips")]
    psx_rt::cache::flush_instruction_cache();
    // The pinned MIPS-I backend lowers compiler_fence to unsupported SYNC.
    // Match psx-rt's empty-asm memory barrier instead.
    #[cfg(target_arch = "mips")]
    // SAFETY: empty asm only constrains compiler memory ordering.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "mips"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_link_metadata_has_the_fixed_patch_layout() {
        assert_eq!(core::mem::size_of::<PackedDescriptor>(), 20);
        assert_eq!(core::mem::offset_of!(PackedDescriptor, checksum), 12);
        assert_eq!(core::mem::size_of::<PackedCall>(), 12);
        assert_eq!(core::mem::offset_of!(PackedCall, offset), 8);
    }
    fn package(id: u32) -> [u32; 7] {
        [overlay::MAGIC, id, 2, 0, 8, 0x03e00008, 0]
    }
    fn descriptor(words: &[u32]) -> Descriptor {
        Descriptor {
            id: words[1],
            linked: 0x80408000,
            checksum: checksum(words),
        }
    }
    #[test]
    fn byte_allocations_do_not_round_to_sectors_and_stale_handles_stay_stale() {
        let mut backing = [0u32; 8];
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let mut registry = Registry::<4>::new(0, 32);
        let words = package(7);
        let first = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        let second = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        assert_eq!(registry.pool.used_pages(), 16);
        assert_eq!(registry.pool.try_run(second).unwrap().first_page, 8);
        assert!(registry.unload(first));
        let replacement = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        assert_eq!(replacement.slot(), first.slot());
        assert_ne!(replacement.generation(), first.generation());
        assert!(matches!(
            registry.admit(&mut arena, &[first]),
            Err(Error::Stale)
        ));
    }
    #[test]
    fn failure_cannot_publish_and_missing_dependency_admission_is_all_or_none() {
        let mut backing = [0u32; 8];
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let mut registry = Registry::<4>::new(0, 32);
        let words = package(7);
        let mut bad = descriptor(&words);
        bad.checksum ^= 1;
        assert_eq!(
            registry.install(&mut arena, &words, bad),
            Err(Error::Integrity)
        );
        assert_eq!(registry.pool.used_pages(), 0);
        let good = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        assert!(matches!(
            registry.admit(&mut arena, &[good, Handle::NONE]),
            Err(Error::Stale)
        ));
        assert!(registry.unload(good));
        let invalid = [overlay::MAGIC, 8, 2, 1, 8, 0, 0x0c000000, 0x10000004];
        let mut desc = descriptor(&invalid);
        desc.linked = 0xfffffffc;
        assert_eq!(
            registry.install(&mut arena, &invalid, desc),
            Err(Error::Format(overlay::Error::Bounds))
        );
        assert_eq!(registry.pool.used_pages(), 0);
        assert!(registry.installed.iter().all(|slot| !slot.ready));
    }
    #[test]
    fn fixed_window_placement_and_oversized_code_are_bounded() {
        let mut backing = [0u32; 16];
        let mut arena = CallerArena::new(&mut backing, 0x801d7ff0).unwrap();
        let mut registry = Registry::<2>::new(0, 64);
        let words = [overlay::MAGIC, 1, 6, 0, 24, 0, 0, 0, 0, 0, 0];
        let handle = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        assert_eq!(registry.pool.try_run(handle).unwrap().first_page, 16);
        assert_eq!(registry.place(&arena, 0x10004, true), Err(Error::Space));
        assert_eq!(
            registry.place(&arena, 0xffff_fffc, false),
            Err(Error::Space)
        );
    }
    #[test]
    fn title_gameplay_title_reuse_keeps_generation_invalidation() {
        let mut backing = [0u32; 8];
        let mut registry = Registry::<1>::new(0, 32);
        let words = package(7);
        let first = {
            let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
            let handle = registry
                .install(&mut arena, &words, descriptor(&words))
                .unwrap();
            registry.invalidate_all();
            handle
        };
        backing.fill(0x12345678); // Normal mutable gameplay arena access.
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let second = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        assert_ne!(first.generation(), second.generation());
        assert!(matches!(
            registry.admit(&mut arena, &[first]),
            Err(Error::Stale)
        ));
        let dependencies = [second];
        let admission = registry.admit(&mut arena, &dependencies).unwrap();
        assert_eq!(admission.id(second).unwrap(), 7);
        assert_eq!(admission.words(second).unwrap(), &words[5..]);
    }
    #[test]
    fn callable_scope_traps_on_normal_and_unwind_exit_and_rejects_data_entries() {
        let mut backing = [0u32; 8];
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let mut registry = Registry::<2>::new(0, 32);
        let words = package(7);
        let handle = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        let dependencies = [handle];
        let admission = registry.admit(&mut arena, &dependencies).unwrap();
        let mut instruction = 0x0c000000;
        let trap = 0x80010080;
        let expected = overlay::direct(instruction, 1, trap, 0x80010000).unwrap();
        {
            // SAFETY: model instruction storage is exclusive and is never executed.
            let mut imports =
                [unsafe { StaticImport::new(&mut instruction, 0x80010000, handle, 0) }];
            assert_eq!(
                // SAFETY: the closure executes no model instruction or async user.
                unsafe { admission.invoke(&mut imports, trap, || 42) },
                Ok(42)
            );
        }
        assert_eq!(instruction, expected);
        {
            // SAFETY: exclusive model storage; no instruction is executed.
            let mut imports =
                [unsafe { StaticImport::new(&mut instruction, 0x80010000, handle, 0) }];
            // SAFETY: the closure only unwinds, with no asynchronous user.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                admission.invoke(&mut imports, trap, || panic!("caller unwind"))
            }));
            assert!(result.is_err());
        }
        assert_eq!(instruction, expected);
        // SAFETY: invalid entry is tested in exclusive model storage without execution.
        let mut imports = [unsafe { StaticImport::new(&mut instruction, 0x80010000, handle, 8) }];
        assert_eq!(
            // SAFETY: validation rejects this before any instruction is executed.
            unsafe { admission.invoke(&mut imports, trap, || ()) },
            Err(Error::Integrity)
        );
    }
}
