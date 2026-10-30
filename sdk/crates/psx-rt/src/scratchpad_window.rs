//! A scratchpad stack window for whole call trees, with hop-back to the RAM stack.
//!
//! [`ScratchpadStack::run`](crate::scratchpad::ScratchpadStack::run) moves `$sp` into the scratchpad for
//! one closure and needs the whole call tree to fit. The call trees that spill the most (a renderer's
//! emitters, with recursive tails) do not fit a fixed budget, so this module keeps the stack window on
//! the scratchpad for the shallow part and runs the deep or recursive callees on the RAM stack.
//!
//! * A **level-1 thunk** (`window_fn!` `level1`) switches `$sp` to [`TOP`] for one call, copies the
//!   stack-passed arguments across, calls the target and switches back. Already on the scratchpad stack,
//!   or with a switch in progress, it just calls the target.
//! * A **guard** (`window_fn!` `guard`) sits in front of a deep or recursive callee. It calls the target
//!   in place when `$sp` is in RAM or when the window has `floor + need` bytes left, and otherwise
//!   **hops back**: it runs the target on the RAM stack below the outermost switch, copying the
//!   stack-passed arguments, and returns to the window afterwards.
//!
//! The wrappers forward every argument register and stack word untouched, so a wrapper has the callee's
//! own signature and the Rust call sites do not change. On the host the wrappers call the target.
//!
//! # What makes it sound
//!
//! * `need` is the static worst-case stack depth of the callee with other guards counted as 24 bytes,
//!   read from the linked image. [`psoxide-hazard`'s `window-gate`] fails the build when a `need` is
//!   smaller than that bound, when a level-1 root does not fit [`TOP`], when any code calls a guarded
//!   or wrapped function directly instead of through its stub, or when the guard-free closure holds an
//!   indirect call or a second `$sp` adjustment.
//! * Interrupts: psx-rt's handler uses only `$k0`/`$k1` and never touches `$sp`. A handler that runs on
//!   its own stack (cs-psx's CD wrapper) is equally safe. The same rule as
//!   [`ScratchpadStack`](crate::scratchpad::ScratchpadStack) applies to any other handler.
//! * A panic on the window moves back to the RAM stack through `__psx_rt_scratchpad_return_sp`, which a
//!   level-1 switch sets, so the report never runs on the scratchpad.
//! * [`ScratchpadStack::run`](crate::scratchpad::ScratchpadStack::run) called from inside a hopped-back
//!   region runs in place (a nested run must not restart at the scratchpad top and overwrite the frames
//!   the window left suspended).
//! * The scratchpad bytes `floor..TOP` must be free while a level-1 call runs: no other tenant, no
//!   engine `ScratchpadStack` live around it. Record that with [`Region`](crate::scratchpad::Region) and
//!   `assert_disjoint`.
//!
//! A crate that uses `window_fn!` needs `#![feature(asm_experimental_arch)]` on the PS1 target, like any
//! crate with `global_asm!`.
//!
//! # Layout
//!
//! Stack-argument words are copied to the window top: eight for level-1 calls, twelve for hops
//! ([`LEVEL1_ARG_WORDS`], [`HOP_ARG_WORDS`]). `window_fn!` asserts at compile time that a signature's
//! stack arguments fit (aggregates over two words are passed by pointer on this target, see
//! [`abi_words`]).

/// First byte above the stack: `$sp` starts here for a level-1 call and the copied arguments follow.
pub const TOP: usize = 976;
/// Stack-argument words a level-1 switch copies.
pub const LEVEL1_ARG_WORDS: usize = 8;
/// Stack-argument words a hop copies.
pub const HOP_ARG_WORDS: usize = 12;

/// `[RAM $sp of the outermost switch, busy]`. Not part of the API; the asm reads and writes it.
#[no_mangle]
#[used]
pub static mut __psx_rt_window_state: [u32; 2] = [0; 2];

/// Argument words the Rust ABI uses for a value of `size` bytes on mipsel: aggregates over two words go
/// by pointer.
pub const fn abi_words(size: usize) -> usize {
    if size <= 8 {
        size.div_ceil(4)
    } else {
        1
    }
}

/// True while a level-1 switch is in progress (on the window or hopped back to RAM).
#[inline(always)]
pub fn is_active() -> bool {
    // SAFETY: a plain read of a word only the thunks write, in the same thread of control.
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_window_state[1]) != 0 }
}

// Shared level-1 switch: `$t9` = target. See the module docs.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__psx_rt_window_l1,\"ax\",@progbits",
    ".globl __psx_rt_window_l1",
    ".type __psx_rt_window_l1,@function",
    ".set noreorder",
    ".set noat",
    "__psx_rt_window_l1:",
    "lui $t0, 0x1f80",
    "subu $t1, $sp, $t0",
    "sltiu $t1, $t1, 0x400",
    "bnez $t1, 1f",
    "nop",
    "lui $t3, %hi({state})",
    "lw $t1, %lo({state}+4)($t3)",
    "nop",
    "bnez $t1, 1f",
    "nop",
    "addiu $sp, $sp, -8",
    "sw $ra, 4($sp)",
    "sw $s0, 0($sp)",
    "move $s0, $sp",
    "sw $s0, %lo({state})($t3)",
    "lui $t4, %hi({retsp})",
    "sw $s0, %lo({retsp})($t4)",
    "addiu $t1, $zero, 1",
    "sw $t1, %lo({state}+4)($t3)",
    "lw $t1, 24($s0)",
    "lw $t2, 28($s0)",
    "sw $t1, 992($t0)",
    "sw $t2, 996($t0)",
    "lw $t1, 32($s0)",
    "lw $t2, 36($s0)",
    "sw $t1, 1000($t0)",
    "sw $t2, 1004($t0)",
    "lw $t1, 40($s0)",
    "lw $t2, 44($s0)",
    "sw $t1, 1008($t0)",
    "sw $t2, 1012($t0)",
    "lw $t1, 48($s0)",
    "lw $t2, 52($s0)",
    "sw $t1, 1016($t0)",
    "sw $t2, 1020($t0)",
    "addiu $t0, $t0, 0x3d0",
    "jalr $t9",
    "move $sp, $t0",
    "lui $t3, %hi({state})",
    "sw $zero, %lo({state}+4)($t3)",
    "lui $t4, %hi({retsp})",
    "sw $zero, %lo({retsp})($t4)",
    "move $sp, $s0",
    "lw $ra, 4($sp)",
    "lw $s0, 0($sp)",
    "addiu $sp, $sp, 8",
    "jr $ra",
    "nop",
    "1:",
    "jr $t9",
    "nop",
    ".size __psx_rt_window_l1, .-__psx_rt_window_l1",
    ".set at",
    ".set reorder",
    state = sym __psx_rt_window_state,
    retsp = sym crate::scratchpad::__psx_rt_scratchpad_return_sp,
);

// Common hop-back: `$t9` = target, `$sp` on the window. Runs the target on the RAM stack below the outermost
// switch.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__psx_rt_window_hop,\"ax\",@progbits",
    ".globl __psx_rt_window_hop",
    ".type __psx_rt_window_hop,@function",
    ".set noreorder",
    ".set noat",
    "__psx_rt_window_hop:",
    "lui $t2, %hi({state})",
    "lw $t2, %lo({state})($t2)",
    "addiu $sp, $sp, -16",
    "sw $ra, 12($sp)",
    "sw $s0, 8($sp)",
    "move $s0, $sp",
    "addiu $t2, $t2, -80",
    "lw $t1, 32($s0)",
    "lw $t3, 36($s0)",
    "sw $t1, 16($t2)",
    "sw $t3, 20($t2)",
    "lw $t1, 40($s0)",
    "lw $t3, 44($s0)",
    "sw $t1, 24($t2)",
    "sw $t3, 28($t2)",
    "lw $t1, 48($s0)",
    "lw $t3, 52($s0)",
    "sw $t1, 32($t2)",
    "sw $t3, 36($t2)",
    "lw $t1, 56($s0)",
    "lw $t3, 60($s0)",
    "sw $t1, 40($t2)",
    "sw $t3, 44($t2)",
    "lw $t1, 64($s0)",
    "lw $t3, 68($s0)",
    "sw $t1, 48($t2)",
    "sw $t3, 52($t2)",
    "lw $t1, 72($s0)",
    "lw $t3, 76($s0)",
    "sw $t1, 56($t2)",
    "sw $t3, 60($t2)",
    "jalr $t9",
    "move $sp, $t2",
    "move $sp, $s0",
    "lw $ra, 12($sp)",
    "lw $s0, 8($sp)",
    "addiu $sp, $sp, 16",
    "jr $ra",
    "nop",
    ".size __psx_rt_window_hop, .-__psx_rt_window_hop",
    ".set at",
    ".set reorder",
    state = sym __psx_rt_window_state,
);

// Shared guard test: `$t9` = target, `$t8` = room needed (floor + need).
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__psx_rt_window_guard,\"ax\",@progbits",
    ".globl __psx_rt_window_guard",
    ".type __psx_rt_window_guard,@function",
    ".set noreorder",
    ".set noat",
    "__psx_rt_window_guard:",
    "lui $t0, 0x1f80",
    "subu $t1, $sp, $t0",
    "sltiu $t2, $t1, 0x400",
    "beqz $t2, 1f",
    "sltu $t2, $t1, $t8",
    "beqz $t2, 1f",
    "nop",
    "j __psx_rt_window_hop",
    "nop",
    "1:",
    "jr $t9",
    "nop",
    ".size __psx_rt_window_guard, .-__psx_rt_window_guard",
    ".set at",
    ".set reorder",
);

/// The asm of a level-1 stub (an implementation detail of `window_fn!`).
#[doc(hidden)]
#[macro_export]
macro_rules! __window_level1_stub {
    ($name:literal, $target:ident) => {
        #[cfg(target_arch = "mips")]
        core::arch::global_asm!(
            ".section .text.__psx_rt_window_stub,\"ax\",@progbits",
            concat!(".globl ", $name),
            concat!(".type ", $name, ",@function"),
            ".set noreorder",
            concat!($name, ":"),
            "lui $t9, %hi({f})",
            "addiu $t9, $t9, %lo({f})",
            "j __psx_rt_window_l1",
            "nop",
            concat!(".size ", $name, ", 16"),
            ".set reorder",
            f = sym $target,
        );
    };
}

/// The asm of a guard stub (an implementation detail of `window_fn!`).
#[doc(hidden)]
#[macro_export]
macro_rules! __window_guard_stub {
    ($name:literal, $target:ident, $room:expr) => {
        #[cfg(target_arch = "mips")]
        core::arch::global_asm!(
            ".section .text.__psx_rt_window_stub,\"ax\",@progbits",
            concat!(".globl ", $name),
            concat!(".type ", $name, ",@function"),
            ".set noreorder",
            concat!($name, ":"),
            "lui $t9, %hi({f})",
            "addiu $t9, $t9, %lo({f})",
            "j __psx_rt_window_guard",
            "addiu $t8, $zero, {room}",
            concat!(".size ", $name, ", 16"),
            ".set reorder",
            f = sym $target,
            room = const $room,
        );
    };
}

/// Declare one window call target: the extern symbol, its asm stub and an inline wrapper with the callee's
/// own signature.
///
/// ```text
/// psx_rt::window_fn!(level1, "__ws_patches", ext_patches, patches, orig_patches,
///     (a: &mut X, n: usize) -> bool);
/// psx_rt::window_fn!(guard FLOOR + 368, "__ws_cell", ext_cell, cell, orig_cell,
///     (a: &mut X) );
/// ```
///
/// `level1` roots must not be called from inside a hop-free guarded callee they themselves call; a guard
/// takes the room it needs (`floor + need`, at most 32767). Call sites use the wrapper name, never the
/// target, and the `window-gate` fails the build otherwise. The wrapper calls the target directly when
/// `$on` is false (a control build) and on the host.
#[macro_export]
macro_rules! window_fn {
    (level1, $on:expr, $sym:literal, $ext:ident, $wrap:ident, $orig:ident,
     ($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)?) => {
        #[cfg(target_arch = "mips")]
        extern "Rust" { #[link_name = $sym] fn $ext($($a: $t),*) $(-> $r)?; }
        $crate::__window_level1_stub!($sym, $orig);
        const _: () = assert!(
            1 $(+ $crate::scratchpad_window::abi_words(core::mem::size_of::<$t>()))*
                <= 4 + $crate::scratchpad_window::LEVEL1_ARG_WORDS,
            "level-1 thunk copies only 8 stack words"
        );
        #[inline(always)]
        unsafe fn $wrap($($a: $t),*) $(-> $r)? {
            #[cfg(target_arch = "mips")]
            if $on { return unsafe { $ext($($a),*) }; }
            unsafe { $orig($($a),*) }
        }
    };
    (guard $room:expr, $on:expr, $sym:literal, $ext:ident, $wrap:ident, $orig:ident,
     ($($a:ident : $t:ty),* $(,)?) $(-> $r:ty)?) => {
        #[cfg(target_arch = "mips")]
        extern "Rust" { #[link_name = $sym] fn $ext($($a: $t),*) $(-> $r)?; }
        $crate::__window_guard_stub!($sym, $orig, $room);
        const _: () = assert!(
            1 $(+ $crate::scratchpad_window::abi_words(core::mem::size_of::<$t>()))*
                <= 4 + $crate::scratchpad_window::HOP_ARG_WORDS,
            "hop copies only 12 stack words"
        );
        const _: () = assert!(($room) <= 32767, "guard room does not fit the stub immediate");
        #[inline(always)]
        unsafe fn $wrap($($a: $t),*) $(-> $r)? {
            #[cfg(target_arch = "mips")]
            if $on { return unsafe { $ext($($a),*) }; }
            unsafe { $orig($($a),*) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_take_their_words_and_aggregates_go_by_pointer() {
        assert_eq!(abi_words(1), 1);
        assert_eq!(abi_words(4), 1);
        assert_eq!(abi_words(8), 2);
        assert_eq!(abi_words(12), 1);
        assert_eq!(abi_words(16), 1);
    }

    unsafe fn add3(a: u32, b: u32, c: u32, d: u32, e: u32, f: u32) -> u32 {
        a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6
    }
    unsafe fn pair(x: &[u8], y: u32) -> (u32, u32) {
        (x.len() as u32, y)
    }

    crate::window_fn!(level1, true, "__test_add3", ext_add3, win_add3, add3,
        (a: u32, b: u32, c: u32, d: u32, e: u32, f: u32) -> u32);
    crate::window_fn!(guard 100 + 368, true, "__test_pair", ext_pair, win_pair, pair,
        (x: &[u8], y: u32) -> (u32, u32));

    #[test]
    fn host_wrappers_forward_arguments_and_results() {
        // SAFETY: the host wrappers only call the target.
        unsafe {
            assert_eq!(win_add3(1, 2, 3, 4, 5, 6), add3(1, 2, 3, 4, 5, 6));
            assert_eq!(win_pair(&[1, 2, 3], 9), (3, 9));
        }
    }

    #[test]
    fn the_window_is_never_active_on_the_host() {
        assert!(!is_active());
    }
}
