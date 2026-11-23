// SPDX-License-Identifier: GPL-2.0-or-later
//! The program side of the core: the embedded image, `__core_boot`, and the calls.
//!
//! A program on the core links with `-T psoxide-core.ld` before psoxide.ld and a `-T` file that
//! sets `RAM_SIZE_OVERRIDE = 0x1FE000`. The script stores [`IMAGE`] right after `.data` (where
//! `.bss` will be; the program's EXE is written from load addresses, see psoxide.ld), makes
//! `__core_boot` the header's entry and gives the jump-table slots their addresses.
//! `__core_boot` then runs before psx-rt's `_start`:
//!
//! 1. it keeps `$a0..$a3` and moves `$sp` to `STACK_INIT` (a BIOS hands over 0x801FFFF0, inside
//!    the handoff block);
//! 2. on a cold entry (anything but a core switch) it copies the image up into the window and
//!    zeroes the rest of the window (the core's statics);
//! 3. it hashes the window and stops on the fatal screen, stage 0x01, if the hash is not this
//!    program's [`CORE_HASH`];
//! 4. it calls the core's attach with `$a0..$a2` and the hash, and jumps on with `$a0..$a2` as
//!    it found them: to the boot-overlay mover `__boot_entry` when psoxide-boot.ld is linked,
//!    else to psx-rt's `_start` (psoxide-core.ld's `__core_next`).
//!
//! So a program behaves the same booted from the BIOS on its own disc, chain-loaded at an LBA
//! offset on a bigger disc, or switched to by the core.

use crate::abi::{self, block};
use crate::{fatal, fnv};

/// The core image, word aligned.
#[repr(C, align(4))]
pub struct Image<const N: usize>(pub [u8; N]);

const IMAGE_BYTES: &[u8] = include_bytes!(env!("PSX_CORE_IMAGE"));
const LEN: usize = IMAGE_BYTES.len();

/// The image this program carries, stored by psoxide-core.ld after `.data`.
#[cfg_attr(target_arch = "mips", link_section = ".psx_core")]
#[used]
#[no_mangle]
pub static IMAGE: Image<LEN> = {
    let mut bytes = [0u8; LEN];
    let mut i = 0;
    while i < LEN {
        bytes[i] = IMAGE_BYTES[i];
        i += 1;
    }
    Image(bytes)
};

/// Word FNV of [`IMAGE`]: what the window must hash to.
pub const CORE_HASH: u32 = fnv::of_bytes(IMAGE_BYTES);

/// Bytes of the image.
pub const CORE_BYTES: u32 = LEN as u32;

const _: () = assert!(LEN % 4 == 0, "the core image is whole words");
const _: () = assert!(
    LEN as u32 <= abi::WINDOW_BYTES,
    "the core image fits the window"
);

extern "C" {
    static __psx_core_store_lma: u32;
    fn __psx_core_attach(a0: u32, a1: u32, a2: u32, hash: u32);
    fn psx_core_switch(scene: u32, args: *const [u32; 4], table: *const u32) -> u32;
    fn psx_core_load_image(image: u32, table: *const u32) -> u32;
    fn psx_core_read(
        lba: u32,
        sectors: u32,
        dst: *mut u32,
        hook: Option<extern "C" fn(u32)>,
    ) -> u32;
    fn psx_core_pause() -> u32;
    fn psx_core_client_attach() -> u32;
    fn psx_core_client_detach();
    fn psx_core_fatal(stage: u32) -> !;
    fn psx_core_music_yield();
    fn psx_core_stream_open(lba: u32, flags: u32) -> u32;
    fn psx_core_stream_sector(dst: *mut u32) -> u32;
    fn psx_core_cd(op: u32, a: u32, b: u32, c: *mut u8) -> u32;
    fn psx_core_set_idle_hook(hook: Option<extern "C" fn()>);
}

fn r32(addr: u32) -> u32 {
    // SAFETY: the handoff block or the window, fixed RAM.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Whether the handoff block is valid.
#[must_use]
pub fn block_valid() -> bool {
    r32(block::ADDR + block::OFF_MAGIC) == block::MAGIC
        && r32(block::ADDR + block::OFF_NOT_MAGIC) == !block::MAGIC
        && r32(block::ADDR + block::OFF_VERSION) & 0xffff >= block::VERSION
}

/// The Rust half of `__core_boot`. No statics: `.bss` is not cleared yet, and its first bytes
/// are where the image is stored.
#[no_mangle]
#[cfg_attr(target_arch = "mips", link_section = ".text.__core_boot_rs")]
extern "C" fn __core_boot_rs(a0: u32, a1: u32, a2: u32, a3: u32) {
    let core_entry = a3 == block::ADDR
        && block_valid()
        && r32(block::ADDR + block::OFF_FLAGS) & block::FLAG_CORE_ENTRY != 0;
    let window = abi::WINDOW as *mut u32;
    if !core_entry {
        // SAFETY: the stored image is LEN bytes at __psx_core_store_lma (psoxide-core.ld); the window
        // is this program's to fill, nothing runs from it yet.
        unsafe {
            let from = core::ptr::addr_of!(__psx_core_store_lma);
            let mut i = 0;
            while i < LEN / 4 {
                core::ptr::write_volatile(window.add(i), core::ptr::read_volatile(from.add(i)));
                i += 1;
            }
            while i < (abi::WINDOW_BYTES / 4) as usize {
                core::ptr::write_volatile(window.add(i), 0);
                i += 1;
            }
        }
    }
    // SAFETY: the window is fixed RAM.
    if unsafe { fnv::of_ram(window, CORE_HASH_WORDS) } != CORE_HASH {
        // SAFETY: interrupts off before the screen is touched; nothing to return to.
        #[cfg(target_arch = "mips")]
        unsafe {
            core::arch::asm!("mtc0 $zero, $12", "nop", "nop")
        };
        fatal::paint(abi::code::WINDOW_HASH);
        loop {
            core::hint::spin_loop();
        }
    }
    // SAFETY: the window holds this program's core, checked above.
    unsafe { __psx_core_attach(a0, a1, a2, CORE_HASH) };
}

const CORE_HASH_WORDS: u32 = (LEN / 4) as u32;

// The jump table's slots as absolute symbols, from psx-scene-abi: the `extern "C"` declarations
// above resolve to them, so every call is a direct `jal` and the addresses exist in one place.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".globl __psx_core_attach", ".set __psx_core_attach, {attach}",
    ".globl psx_core_switch", ".set psx_core_switch, {switch}",
    ".globl psx_core_read", ".set psx_core_read, {read}",
    ".globl psx_core_pause", ".set psx_core_pause, {pause}",
    ".globl psx_core_client_attach", ".set psx_core_client_attach, {client_attach}",
    ".globl psx_core_client_detach", ".set psx_core_client_detach, {client_detach}",
    ".globl psx_core_fatal", ".set psx_core_fatal, {fatal}",
    ".globl psx_core_music_yield", ".set psx_core_music_yield, {music_yield}",
    ".globl psx_core_stream_open", ".set psx_core_stream_open, {stream_open}",
    ".globl psx_core_stream_sector", ".set psx_core_stream_sector, {stream_sector}",
    ".globl psx_core_cd", ".set psx_core_cd, {cd}",
    ".globl psx_core_set_idle_hook", ".set psx_core_set_idle_hook, {set_idle_hook}",
    ".globl psx_core_load_image", ".set psx_core_load_image, {load_image}",
    attach = const abi::slot::ATTACH,
    switch = const abi::slot::SWITCH,
    read = const abi::slot::READ,
    pause = const abi::slot::PAUSE,
    client_attach = const abi::slot::CLIENT_ATTACH,
    client_detach = const abi::slot::CLIENT_DETACH,
    fatal = const abi::slot::FATAL,
    music_yield = const abi::slot::MUSIC_YIELD,
    stream_open = const abi::slot::STREAM_OPEN,
    stream_sector = const abi::slot::STREAM_SECTOR,
    cd = const abi::slot::CD,
    set_idle_hook = const abi::slot::SET_IDLE_HOOK,
    load_image = const abi::slot::LOAD_IMAGE,
);

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__core_boot,\"ax\",@progbits",
    ".globl __core_boot",
    ".type __core_boot,@function",
    ".set noreorder",
    "__core_boot:",
    "move $s0, $a0",
    "move $s1, $a1",
    "move $s2, $a2",
    "lui  $sp, {sp_hi}",
    "ori  $sp, $sp, {sp_lo}",
    "jal  __core_boot_rs",
    "nop",
    "move $a0, $s0",
    "move $a1, $s1",
    "j    __core_next",
    "move $a2, $s2",
    ".set reorder",
    ".size __core_boot, . - __core_boot",
    sp_hi = const abi::STACK_INIT >> 16,
    sp_lo = const abi::STACK_INIT & 0xFFFF,
);

/// Switch to `scene`. Returns only when the core refused before anything changed (a
/// [`abi::code`]); on success the scene runs. `table` is the program's RAM copy of its scene
/// table ([`read_table`]).
pub fn switch(scene: u32, args: &[u32; 4], table: &[u32]) -> u32 {
    // SAFETY: the core is in the window (checked at boot); the call follows the ABI.
    unsafe { psx_core_switch(scene, args, table.as_ptr()) }
}

/// Load overlay `image` into its range unless it is in RAM, and return: the caller then calls
/// into it. Refused (a code) when its program is not the one running, or when its range would
/// overwrite the calling code. `table` as for [`switch`].
pub fn load_image(image: u32, table: &[u32]) -> u32 {
    // SAFETY: the core is in the window (checked at boot); the call follows the ABI.
    unsafe { psx_core_load_image(image, table.as_ptr()) }
}

/// Read this disc's SCENES.TBL once (at boot) into `table`, the program's own RAM copy that
/// [`switch`] and [`load_image`] take: the header and every row, `16 + 8 * (scenes + images)` words
/// (a 2 KB sector passes through the stack while it is read). Returns the words kept, or the code
/// of a failed read, a table of another version (0x21) or a `table` too small for it (0x24).
pub fn read_table(table: &mut [u32]) -> Result<usize, u32> {
    let mut sector = [0u32; 512];
    // SAFETY: one sector's worth of words.
    let r = unsafe { read(abi::table::LBA, 1, sector.as_mut_ptr(), None) };
    if r != abi::code::OK {
        return Err(r);
    }
    if sector[0] != abi::table::MAGIC[0]
        || sector[1] != abi::table::MAGIC[1]
        || sector[2] & 0xffff != abi::table::VERSION
    {
        return Err(abi::code::TABLE_MAGIC);
    }
    let rows = (sector[2] >> 16) + (sector[3] & 0xffff);
    let words = (16 + 8 * rows.min(abi::table::MAX_ROWS)) as usize;
    if table.len() < words {
        return Err(abi::code::BOUNDS);
    }
    table[..words].copy_from_slice(&sector[..words]);
    Ok(words)
}

/// Blocking read of `sectors` from member-relative `lba` into `dst`, ending with a proven Pause.
///
/// # Safety
/// `dst` must hold `sectors * 2048` writable, word-aligned bytes.
pub unsafe fn read(lba: u32, sectors: u32, dst: *mut u32, hook: Option<extern "C" fn(u32)>) -> u32 {
    // SAFETY: as the caller vouches.
    unsafe { psx_core_read(lba, sectors, dst, hook) }
}

/// A proven Pause.
pub fn pause() -> u32 {
    // SAFETY: ABI call.
    unsafe { psx_core_pause() }
}

/// A stream client takes the drive.
pub fn client_attach() -> u32 {
    // SAFETY: ABI call.
    unsafe { psx_core_client_attach() }
}

/// The client gives it back.
pub fn client_detach() {
    // SAFETY: ABI call.
    unsafe { psx_core_client_detach() }
}

/// The core's fatal screen.
pub fn fatal(stage: u32) -> ! {
    // SAFETY: ABI call.
    unsafe { psx_core_fatal(stage) }
}

/// Mute the SPU's CD input before a data read after music.
pub fn music_yield() {
    // SAFETY: ABI call.
    unsafe { psx_core_music_yield() }
}

/// Open a stream at member-relative `lba`.
pub fn stream_open(lba: u32, flags: u32) -> u32 {
    // SAFETY: ABI call.
    unsafe { psx_core_stream_open(lba, flags) }
}

/// The next sector of the open stream.
///
/// # Safety
/// `dst` must hold 2048 writable, word-aligned bytes.
pub unsafe fn stream_sector(dst: *mut u32) -> u32 {
    // SAFETY: as the caller vouches.
    unsafe { psx_core_stream_sector(dst) }
}

/// The register-level CD slot ([`abi::cd_op`]).
///
/// # Safety
/// `a` and `c` must be what `op` reads and writes (see the core image's `__psx_core_cd_impl`).
pub unsafe fn cd(op: u32, a: u32, b: u32, c: *mut u8) -> u32 {
    // SAFETY: as the caller vouches.
    unsafe { psx_core_cd(op, a, b, c) }
}

/// Install (or with `None` remove) the hook every core wait calls.
pub fn set_idle_hook(hook: Option<extern "C" fn()>) {
    // SAFETY: ABI call.
    unsafe { psx_core_set_idle_hook(hook) }
}

/// One word of the handoff block, by byte offset.
#[must_use]
pub fn block_word(offset: u32) -> u32 {
    r32(block::ADDR + offset)
}

/// Word FNV of the window's image bytes now.
#[must_use]
pub fn window_hash() -> u32 {
    // SAFETY: the window is fixed RAM.
    unsafe { fnv::of_ram(abi::WINDOW as *const u32, CORE_HASH_WORDS) }
}
