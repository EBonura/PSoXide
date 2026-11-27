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
/// A resident jump veneer for one typed call site, built by the final-link packer
/// (`overlay-compose --veneers SYMBOL`). The site's J/JAL always targets the veneer;
/// the veneer loads its own `target` word and jumps there. A lease writes the
/// overlay's entry into `target` and the trap back when it ends, so entering and
/// leaving a lease are two data stores: the instruction cache is flushed only when
/// an image is installed or replaced ([`Registry::install`]), never per call.
///
/// The five code words never change after the final link and are never written at
/// run time, so the cache can hold them for good. The data load of `target`
/// is not cached (the console's data cache is the scratchpad).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedVeneer {
    /// `lui t0, hi; lw t0, lo(t0); nop; jr t0; nop`, addressing `target` absolutely.
    pub code: [u32; 5],
    /// Where the veneer jumps: the resident trap while no lease is live.
    pub target: u32,
}
impl PackedVeneer {
    /// Size of one veneer, and the stride of a veneer table.
    pub const BYTES: u32 = 24;
    /// Initial contents of a veneer table in resident `.data`; the packer overwrites
    /// every record it uses. Keep the table in `.data` (`link_section`), never `.bss`.
    pub const PLACEHOLDER: Self = Self {
        code: [0; 5],
        target: 0,
    };
    /// The code words of the veneer stored at `address`. The pair `lui`/`lw`
    /// addresses the veneer's own `target` word at `address + 20`; `$t0` is a
    /// caller-saved temporary that no argument register or return value uses.
    pub const fn code_for(address: u32) -> [u32; 5] {
        let slot = address.wrapping_add(20);
        [
            0x3c08_0000 | (slot.wrapping_add(0x8000) >> 16),
            0x8d08_0000 | (slot & 0xffff),
            0,
            0x0100_0008,
            0,
        ]
    }
    /// The veneer the packer publishes at `address`, trapping until a lease binds it.
    pub const fn published(address: u32, trap: u32) -> Self {
        Self {
            code: Self::code_for(address),
            target: trap,
        }
    }
}

/// One veneered typed call: the resident veneer, the module that owns the export and
/// the export's byte offset in that module's image.
pub struct VeneerImport<'a> {
    veneer: &'a mut PackedVeneer,
    address: u32,
    module: Handle,
    offset: u32,
}
impl<'a> VeneerImport<'a> {
    /// Bind only a veneer the final-link packer published for the packaged Rust
    /// entry at `offset`; its call site's typed caller and callee must share their
    /// exact ABI/types, as for [`StaticImport`].
    ///
    /// # Safety
    /// `veneer` must be the actual resident record at `address`, and no other CPU or
    /// DMA user may write it while the import exists.
    pub unsafe fn new(
        veneer: &'a mut PackedVeneer,
        address: u32,
        module: Handle,
        offset: u32,
    ) -> Self {
        #[cfg(target_arch = "mips")]
        debug_assert_eq!(veneer as *mut PackedVeneer as usize as u32, address);
        Self {
            veneer,
            address,
            module,
            offset,
        }
    }
}

/// Word checksum matching the existing HK module transport
/// ([`psx_pack::WordFnv32`], the one kernel for it).
#[inline(never)]
pub fn checksum(words: &[u32]) -> u32 {
    psx_pack::WordFnv32::of(words)
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

/// A package being installed from its words as they arrive ([`Registry::begin_stream`]). It
/// holds a placement, not an admission: finish it ([`Registry::finish_stream`]) or abandon it
/// ([`Registry::abandon_stream`]), else the placement stays taken until the registry is
/// invalidated.
pub struct Streaming {
    handle: Handle,
    descriptor: Descriptor,
    /// Arena word index of the image's first word.
    start: usize,
    /// Its address.
    address: u32,
    /// Image words.
    image: usize,
    /// Package words (header, image and fixups).
    words: usize,
    /// Package words arrived so far.
    seen: usize,
    code_end: u32,
    /// Word FNV of the words arrived so far.
    hash: u32,
}
impl Streaming {
    /// Package words arrived so far, the five header words included.
    #[must_use]
    pub const fn seen(&self) -> usize {
        self.seen
    }
    /// The package's complete word count.
    #[must_use]
    pub const fn words(&self) -> usize {
        self.words
    }
}

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
    /// Begin installing a package whose words arrive in order (a CD read sector by sector), so
    /// the image is read straight into its place in the arena: no staging copy of the package.
    /// `header` is the package's first five words, `words` its complete word count (header,
    /// image and fixups). The image is placed as [`Registry::install`] places it; nothing is
    /// admissible until [`Registry::finish_stream`] checks the whole package.
    ///
    /// Then, in package order: [`Registry::stream_image`] is where the image words that have not
    /// arrived yet go (a reader may write them there itself and report them with
    /// [`Registry::stream_landed`]), and [`Registry::stream_feed`] takes words that arrived
    /// elsewhere (image words are stored in place, fixup words relocate the image in place).
    /// After an error, or to give up, [`Registry::abandon_stream`] frees the placement.
    ///
    /// # Errors
    /// An arena that is not this registry's, a header that is short or not a package's, or no
    /// room for the image.
    pub fn begin_stream(
        &mut self,
        arena: &CallerArena<'_>,
        header: &[u32],
        words: usize,
        descriptor: Descriptor,
    ) -> Result<Streaming, Error> {
        self.check_arena(arena)?;
        let header = header
            .get(..5)
            .ok_or(Error::Format(overlay::Error::Bounds))?;
        if header[0] != overlay::MAGIC {
            return Err(Error::Format(overlay::Error::Bounds));
        }
        let image = header[2] as usize;
        let end = 5usize
            .checked_add(image)
            .and_then(|end| end.checked_add(header[3] as usize));
        if end != Some(words) || header[4] & 3 != 0 || header[4] as usize > image * 4 {
            return Err(Error::Format(overlay::Error::Bounds));
        }
        if header[1] != descriptor.id {
            return Err(Error::Integrity);
        }
        if image == 0 {
            return Err(Error::Space);
        }
        let bytes = u32::try_from(image * 4).map_err(|_| Error::Space)?;
        let handle = self.place(arena, bytes, header[4] != 0)?;
        let run = self.pool.try_run(handle).ok_or(Error::Stale)?;
        let mut hash = psx_pack::WordFnv32::new();
        hash.update(header);
        Ok(Streaming {
            handle,
            descriptor,
            start: run.first_page as usize / 4,
            address: arena.address + run.first_page,
            image,
            words,
            seen: 5,
            code_end: header[4],
            hash: hash.finish(),
        })
    }
    /// The image words of `stream` that have not arrived yet, in their final place: a reader may
    /// write the next words of the package here itself, then report them with
    /// [`Registry::stream_landed`]. Empty once the image is complete.
    ///
    /// # Errors
    /// An arena or stream that is not this registry's.
    pub fn stream_image<'b>(
        &self,
        arena: &'b mut CallerArena<'_>,
        stream: &Streaming,
    ) -> Result<&'b mut [u32], Error> {
        self.check_stream(arena, stream)?;
        let done = (stream.seen - 5).min(stream.image);
        Ok(&mut arena.words[stream.start + done..stream.start + stream.image])
    }
    /// The reader wrote the next `count` image words of `stream` into
    /// [`Registry::stream_image`] itself.
    ///
    /// # Errors
    /// An arena or stream that is not this registry's, or more words than the image has left.
    pub fn stream_landed(
        &self,
        arena: &CallerArena<'_>,
        stream: &mut Streaming,
        count: usize,
    ) -> Result<(), Error> {
        self.check_stream(arena, stream)?;
        let done = stream.seen - 5;
        if done.checked_add(count).is_none_or(|end| end > stream.image) {
            return Err(Error::Format(overlay::Error::Bounds));
        }
        let at = stream.start + done;
        let mut hash = psx_pack::WordFnv32::from_state(stream.hash);
        hash.update(&arena.words[at..at + count]);
        stream.hash = hash.finish();
        stream.seen += count;
        Ok(())
    }
    /// The next `words` of `stream`'s package, read somewhere else: image words are stored in
    /// place, fixup words relocate the image in place (every image word precedes them, so the
    /// checksum covers each word as it was read).
    ///
    /// # Errors
    /// An arena or stream that is not this registry's, more words than the package has left,
    /// or a fixup word that does not relocate.
    pub fn stream_feed(
        &self,
        arena: &mut CallerArena<'_>,
        stream: &mut Streaming,
        words: &[u32],
    ) -> Result<(), Error> {
        self.check_stream(arena, stream)?;
        if stream
            .seen
            .checked_add(words.len())
            .is_none_or(|end| end > stream.words)
        {
            return Err(Error::Format(overlay::Error::Bounds));
        }
        let mut hash = psx_pack::WordFnv32::from_state(stream.hash);
        hash.update(words);
        stream.hash = hash.finish();
        let image_end = 5 + stream.image;
        let split = image_end.saturating_sub(stream.seen).min(words.len());
        let (image_words, fixups) = words.split_at(split);
        let at = stream.start + stream.seen - 5;
        arena.words[at..at + image_words.len()].copy_from_slice(image_words);
        let image = &mut arena.words[stream.start..stream.start + stream.image];
        for &fixup in fixups {
            let offset = fixup & 0x0fff_ffff;
            let kind = fixup >> 28;
            if offset & 3 != 0 || offset as usize >= stream.image * 4 {
                return Err(Error::Format(overlay::Error::Bounds));
            }
            if !(1..=4).contains(&kind) {
                return Err(Error::Format(overlay::Error::Kind));
            }
            let index = offset as usize / 4;
            image[index] = overlay::relocate(
                image[index],
                kind,
                stream.descriptor.linked,
                stream.address,
                offset,
            )?;
        }
        stream.seen += words.len();
        Ok(())
    }
    /// Publish `stream` once every word of its package has arrived and the package checksum is
    /// the descriptor's: the I-cache is synchronized before the handle becomes admissible. On
    /// any failure the placement is freed and nothing is published.
    ///
    /// # Errors
    /// An arena or stream that is not this registry's, a package not complete, or a checksum
    /// that is not the descriptor's.
    pub fn finish_stream(
        &mut self,
        arena: &CallerArena<'_>,
        stream: Streaming,
    ) -> Result<Handle, Error> {
        if let Err(error) = self.check_stream(arena, &stream) {
            self.abandon_stream(stream);
            return Err(error);
        }
        if stream.seen != stream.words || stream.hash != stream.descriptor.checksum {
            self.abandon_stream(stream);
            return Err(Error::Integrity);
        }
        flush();
        self.installed[stream.handle.slot() as usize] = Installed {
            id: stream.descriptor.id,
            code_end: stream.code_end,
            ready: true,
        };
        self.address = Some(arena.address);
        Ok(stream.handle)
    }
    /// Give up `stream`: its placement is freed, nothing was published.
    pub fn abandon_stream(&mut self, stream: Streaming) {
        self.pool.free(stream.handle);
    }
    fn check_stream(&self, arena: &CallerArena<'_>, stream: &Streaming) -> Result<(), Error> {
        self.check_arena(arena)?;
        let run = self.pool.try_run(stream.handle).ok_or(Error::Stale)?;
        if run.first_page as usize / 4 != stream.start
            || arena.address + run.first_page != stream.address
            || self.installed[stream.handle.slot() as usize].ready
        {
            return Err(Error::Stale);
        }
        Ok(())
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

    /// Like [`Admission::invoke`], for call sites the packer routed through
    /// resident jump veneers ([`PackedVeneer`]). Entering the lease stores each
    /// module entry into its veneer's `target` word; leaving it (normal or unwind)
    /// stores the trap back. Nothing is patched and nothing is flushed: the
    /// instruction cache sees only the image install, so a lease costs two data
    /// stores per import instead of a gated cache flush on entry and another on exit.
    ///
    /// Every import is validated before any veneer is touched: the module must be
    /// admitted, the export inside its executable prefix, the veneer exactly the
    /// published code for its address, and its target the trap (a leaked earlier
    /// lease is an integrity failure, not a silent rebind).
    ///
    /// # Safety
    /// As [`Admission::invoke`]: the closure may call only the exact packaged
    /// entries through their typed callers, must not retain module callbacks or
    /// pointers, and must not start asynchronous users of module bytes beyond the scope.
    pub unsafe fn invoke_veneered<R>(
        &self,
        imports: &mut [VeneerImport<'_>],
        trap: u32,
        caller: impl FnOnce() -> R,
    ) -> Result<R, Error> {
        if trap & 3 != 0 {
            return Err(Error::Integrity);
        }
        for import in imports.iter() {
            self.words(import.module)?;
            let info = self.registry.installed[import.module.slot() as usize];
            if import.address & 3 != 0
                || import.offset & 3 != 0
                || import.offset >= info.code_end
                || import.veneer.code != PackedVeneer::code_for(import.address)
                || import.veneer.target != trap
            {
                return Err(Error::Integrity);
            }
        }
        for import in imports.iter_mut() {
            let run = self
                .registry
                .pool
                .try_run(import.module)
                .ok_or(Error::Stale)?;
            let entry = self.arena.address + run.first_page + import.offset;
            // SAFETY: the record is exclusive (`VeneerImport::new`); a volatile store
            // is the one write the veneer's load must observe.
            unsafe { core::ptr::write_volatile(&mut import.veneer.target, entry) };
        }
        order_stores();
        let guard = VeneerScope { imports, trap };
        let result = caller();
        drop(guard);
        Ok(result)
    }
}
struct VeneerScope<'a, 'veneer> {
    imports: &'a mut [VeneerImport<'veneer>],
    trap: u32,
}
impl Drop for VeneerScope<'_, '_> {
    fn drop(&mut self) {
        for import in self.imports.iter_mut() {
            // SAFETY: as in `invoke_veneered`; the trap was validated before binding.
            unsafe { core::ptr::write_volatile(&mut import.veneer.target, self.trap) };
        }
        order_stores();
    }
}
/// Keep the compiler from moving the veneer stores across the lease's calls.
fn order_stores() {
    #[cfg(target_arch = "mips")]
    // SAFETY: empty asm only constrains compiler memory ordering.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "mips"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
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
        #[cfg(test)]
        count_flush();
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
    #[cfg(test)]
    count_flush();
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
std::thread_local! {
    /// Instruction-cache flushes requested on this thread, for the host tests.
    static FLUSHES: core::cell::Cell<u32> = const { core::cell::Cell::new(0) };
}
#[cfg(test)]
fn count_flush() {
    FLUSHES.with(|count| count.set(count.get() + 1));
}
#[cfg(test)]
fn flush_count() -> u32 {
    FLUSHES.with(core::cell::Cell::get)
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

    /// A package with code, data and every fixup kind: a J to its own word 3, a LUI/ADDIU pair
    /// and a data word pointing into the image.
    fn relocated_package(id: u32, image_words: usize) -> std::vec::Vec<u32> {
        let mut image: std::vec::Vec<u32> = (0..image_words as u32)
            .map(|i| i.wrapping_mul(0x9e37_79b9))
            .collect();
        image[0] = 0x0c00_0000 | ((0x8040_8000u32 + 12) >> 2 & 0x03ff_ffff);
        image[1] = 0x3c01_8041;
        image[2] = 0x2421_8010;
        image[image_words - 1] = 0x8040_8000 + 8;
        let fixups = [
            0x1000_0000,
            0x2000_0004,
            0x3000_0008,
            0x4000_0000 | ((image_words as u32 - 1) * 4),
        ];
        let mut words = std::vec![
            overlay::MAGIC,
            id,
            image_words as u32,
            fixups.len() as u32,
            16
        ];
        words.extend_from_slice(&image);
        words.extend_from_slice(&fixups);
        words
    }
    fn linked(words: &[u32]) -> Descriptor {
        Descriptor {
            id: words[1],
            linked: 0x8040_8000,
            checksum: checksum(words),
        }
    }
    /// Feed `words` as a sector reader would: `sector`-word pieces, the ones wholly inside the
    /// image (past the first) read straight into place.
    fn stream_in(
        registry: &mut Registry<4>,
        arena: &mut CallerArena<'_>,
        words: &[u32],
        descriptor: Descriptor,
        sector: usize,
    ) -> Result<Handle, Error> {
        let mut stream = registry.begin_stream(arena, &words[..5], words.len(), descriptor)?;
        let first = sector.min(words.len());
        let mut result = registry.stream_feed(arena, &mut stream, &words[5..first]);
        let mut at = first;
        while result.is_ok() && at < words.len() {
            let piece = sector.min(words.len() - at);
            let direct = registry
                .stream_image(arena, &stream)
                .map(|image| image.len() >= sector);
            result = match direct {
                Ok(true) if piece == sector => {
                    registry.stream_image(arena, &stream).unwrap()[..sector]
                        .copy_from_slice(&words[at..at + sector]);
                    registry.stream_landed(arena, &mut stream, sector)
                }
                Ok(_) => registry.stream_feed(arena, &mut stream, &words[at..at + piece]),
                Err(error) => Err(error),
            };
            at += piece;
        }
        match result {
            Ok(()) => registry.finish_stream(arena, stream),
            Err(error) => {
                registry.abandon_stream(stream);
                Err(error)
            }
        }
    }
    #[test]
    fn a_streamed_install_equals_the_staged_install_word_for_word() {
        for image_words in [1usize, 4, 7, 40, 61] {
            let words = relocated_package(9, image_words.max(4));
            for sector in [5usize, 6, 8, 13, 64] {
                let mut staged_backing = [0u32; 160];
                let mut streamed_backing = [0u32; 160];
                let mut staged_registry = Registry::<4>::new(0, 640);
                let mut streamed_registry = Registry::<4>::new(0, 640);
                // A first image so the package lands past the start of the arena.
                let filler = package(3);
                let (staged, streamed) = {
                    let mut a = CallerArena::new(&mut staged_backing, 0x801d_8000).unwrap();
                    let mut b = CallerArena::new(&mut streamed_backing, 0x801d_8000).unwrap();
                    staged_registry
                        .install(&mut a, &filler, descriptor(&filler))
                        .unwrap();
                    stream_in(
                        &mut streamed_registry,
                        &mut b,
                        &filler,
                        descriptor(&filler),
                        sector,
                    )
                    .unwrap();
                    (
                        staged_registry
                            .install(&mut a, &words, linked(&words))
                            .unwrap(),
                        stream_in(
                            &mut streamed_registry,
                            &mut b,
                            &words,
                            linked(&words),
                            sector,
                        )
                        .unwrap(),
                    )
                };
                assert_eq!(
                    staged_backing, streamed_backing,
                    "image {image_words} sector {sector}"
                );
                assert_eq!(staged, streamed);
                let mut b = CallerArena::new(&mut streamed_backing, 0x801d_8000).unwrap();
                let dependencies = [streamed];
                let admission = streamed_registry.admit(&mut b, &dependencies).unwrap();
                assert_eq!(admission.id(streamed).unwrap(), 9);
            }
        }
    }
    #[test]
    fn a_streamed_package_that_fails_its_checks_publishes_nothing_and_frees_its_place() {
        let words = relocated_package(9, 40);
        let mut backing = [0u32; 64];
        let mut arena = CallerArena::new(&mut backing, 0x801d_8000).unwrap();
        let mut registry = Registry::<4>::new(0, 256);
        // A word changed in flight: the checksum.
        let mut corrupt = words.clone();
        corrupt[20] ^= 1;
        assert_eq!(
            stream_in(&mut registry, &mut arena, &corrupt, linked(&words), 8),
            Err(Error::Integrity)
        );
        assert_eq!(registry.pool.used_pages(), 0);
        // A fixup outside the image and one of an unknown kind.
        for bad in [0x1000_0000 | (40 * 4), 0x5000_0000] {
            let mut corrupt = words.clone();
            let last = corrupt.len() - 1;
            corrupt[last] = bad;
            assert!(matches!(
                stream_in(&mut registry, &mut arena, &corrupt, linked(&corrupt), 8),
                Err(Error::Format(_))
            ));
            assert_eq!(registry.pool.used_pages(), 0);
        }
        // Another module's package, a short count and a stream left unfinished.
        assert_eq!(
            registry
                .begin_stream(
                    &arena,
                    &words[..5],
                    words.len(),
                    Descriptor {
                        id: 8,
                        ..linked(&words)
                    }
                )
                .err(),
            Some(Error::Integrity)
        );
        assert!(matches!(
            registry.begin_stream(&arena, &words[..5], words.len() - 1, linked(&words)),
            Err(Error::Format(_))
        ));
        let mut stream = registry
            .begin_stream(&arena, &words[..5], words.len(), linked(&words))
            .unwrap();
        registry
            .stream_feed(&mut arena, &mut stream, &words[5..30])
            .unwrap();
        assert_eq!(
            registry.finish_stream(&arena, stream),
            Err(Error::Integrity)
        );
        assert_eq!(registry.pool.used_pages(), 0);
        // Words past the package's end.
        let mut stream = registry
            .begin_stream(&arena, &words[..5], words.len(), linked(&words))
            .unwrap();
        let mut long = words[5..].to_vec();
        long.push(0);
        assert!(registry
            .stream_feed(&mut arena, &mut stream, &long)
            .is_err());
        registry.abandon_stream(stream);
        assert_eq!(registry.pool.used_pages(), 0);
        assert!(registry.installed.iter().all(|slot| !slot.ready));
        // And the clean package still installs in the freed place.
        assert!(stream_in(&mut registry, &mut arena, &words, linked(&words), 8).is_ok());
    }

    const TRAP: u32 = 0x80010080;
    const VENEER_AT: u32 = 0x80020000;

    #[test]
    fn veneered_calls_flush_nothing_after_install() {
        let mut backing = [0u32; 8];
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let mut registry = Registry::<2>::new(0, 32);
        let words = package(7);
        let installs = flush_count();
        let handle = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        assert_eq!(flush_count() - installs, 1, "an install flushes once");
        let mut veneer = PackedVeneer::published(VENEER_AT, TRAP);
        let after_install = flush_count();
        for tick in 0..100u32 {
            let dependencies = [handle];
            let admission = registry.admit(&mut arena, &dependencies).unwrap();
            let mut imports =
                // SAFETY: model veneer storage is exclusive and never executed.
                [unsafe { VeneerImport::new(&mut veneer, VENEER_AT, handle, 0) }];
            // SAFETY: the closure executes no model instruction and starts no async user.
            let seen = unsafe {
                admission.invoke_veneered(&mut imports, TRAP, || {
                    // Not reached through the model; the veneer is bound while we run.
                    tick
                })
            };
            assert_eq!(seen, Ok(tick));
        }
        assert_eq!(
            flush_count(),
            after_install,
            "a lease must not flush the instruction cache"
        );
        // The counter sees the patching lease (so the assertion above is not vacuous):
        // it flushes once to bind and once to trap again.
        let mut instruction = 0x0c000000;
        let dependencies = [handle];
        let admission = registry.admit(&mut arena, &dependencies).unwrap();
        // SAFETY: model instruction storage is exclusive and never executed.
        let mut sites = [unsafe { StaticImport::new(&mut instruction, 0x80010000, handle, 0) }];
        // SAFETY: the closure executes no model instruction or async user.
        unsafe { admission.invoke(&mut sites, TRAP, || ()) }.unwrap();
        assert_eq!(flush_count() - after_install, 2);
    }

    #[test]
    fn a_veneer_targets_the_overlay_only_while_the_lease_is_live() {
        let mut backing = [0u32; 8];
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let mut registry = Registry::<2>::new(0, 32);
        let words = package(7);
        let handle = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        let mut veneer = PackedVeneer::published(VENEER_AT, TRAP);
        let dependencies = [handle];
        let admission = registry.admit(&mut arena, &dependencies).unwrap();
        let entry = 0x801d8000 + 4; // installed address plus the export offset
        {
            let veneer_ptr: *const PackedVeneer = &veneer;
            let mut imports =
                // SAFETY: model storage, never executed.
                [unsafe { VeneerImport::new(&mut veneer, VENEER_AT, handle, 4) }];
            // SAFETY: the closure only reads the model veneer through a raw pointer.
            let bound = unsafe {
                admission.invoke_veneered(&mut imports, TRAP, || {
                    core::ptr::read_volatile(&(*veneer_ptr).target)
                })
            };
            assert_eq!(bound, Ok(entry));
        }
        assert_eq!(veneer.target, TRAP, "the lease's end traps the veneer");
        assert_eq!(veneer.code, PackedVeneer::code_for(VENEER_AT));
        // An unwind restores the trap too.
        {
            let mut imports =
                // SAFETY: model storage, never executed.
                [unsafe { VeneerImport::new(&mut veneer, VENEER_AT, handle, 4) }];
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // SAFETY: the closure only unwinds.
                unsafe { admission.invoke_veneered(&mut imports, TRAP, || panic!("unwind")) }
            }));
            assert!(result.is_err());
        }
        assert_eq!(veneer.target, TRAP);
    }

    #[test]
    fn a_veneer_that_is_not_the_published_one_is_refused_before_anything_binds() {
        let mut backing = [0u32; 8];
        let mut arena = CallerArena::new(&mut backing, 0x801d8000).unwrap();
        let mut registry = Registry::<2>::new(0, 32);
        let words = package(7);
        let handle = registry
            .install(&mut arena, &words, descriptor(&words))
            .unwrap();
        let dependencies = [handle];
        let admission = registry.admit(&mut arena, &dependencies).unwrap();
        let mut good = PackedVeneer::published(VENEER_AT, TRAP);
        let mut bad = PackedVeneer::published(VENEER_AT + 32, TRAP);
        bad.code[3] ^= 1; // not a `jr`
        {
            let mut imports = [
                // SAFETY: model storage, never executed.
                unsafe { VeneerImport::new(&mut good, VENEER_AT, handle, 0) },
                // SAFETY: as above.
                unsafe { VeneerImport::new(&mut bad, VENEER_AT + 32, handle, 0) },
            ];
            assert_eq!(
                // SAFETY: validation rejects this before any lease begins.
                unsafe { admission.invoke_veneered(&mut imports, TRAP, || ()) },
                Err(Error::Integrity)
            );
        }
        assert_eq!(good.target, TRAP, "nothing was bound on a refused lease");
        // A veneer already pointing somewhere else (a lease leaked) is refused as well.
        good.target = 0x801d8000;
        let mut imports =
            // SAFETY: model storage, never executed.
            [unsafe { VeneerImport::new(&mut good, VENEER_AT, handle, 0) }];
        assert_eq!(
            // SAFETY: as above.
            unsafe { admission.invoke_veneered(&mut imports, TRAP, || ()) },
            Err(Error::Integrity)
        );
        // An export outside the executable prefix is refused like a site import.
        let mut fresh = PackedVeneer::published(VENEER_AT, TRAP);
        let mut imports =
            // SAFETY: model storage, never executed.
            [unsafe { VeneerImport::new(&mut fresh, VENEER_AT, handle, 8) }];
        assert_eq!(
            // SAFETY: as above.
            unsafe { admission.invoke_veneered(&mut imports, TRAP, || ()) },
            Err(Error::Integrity)
        );
    }

    #[test]
    fn the_veneer_encoding_loads_its_own_target_word_and_jumps_to_it() {
        // lui t0, hi; lw t0, lo(t0); nop; jr t0; nop; then the target word.
        for address in [0x80020000, 0x8002ffec, 0x80017ff8, 0x801fffe8] {
            let code = PackedVeneer::code_for(address);
            let slot = address + 20;
            let hi = code[0] & 0xffff;
            let lo = code[1] & 0xffff;
            assert_eq!(code[0] >> 16, 0x3c08, "lui $t0");
            assert_eq!(code[1] >> 16, 0x8d08, "lw $t0, lo($t0)");
            assert_eq!(code[2], 0, "the load delay slot is a nop");
            assert_eq!(code[3], 0x0100_0008, "jr $t0");
            assert_eq!(code[4], 0, "the jump delay slot is a nop");
            assert_eq!(
                (hi << 16).wrapping_add(lo as u16 as i16 as i32 as u32),
                slot,
                "the pair addresses the veneer's own target word"
            );
        }
        assert_eq!(core::mem::size_of::<PackedVeneer>(), 24);
    }
}
