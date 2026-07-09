// SPDX-License-Identifier: GPL-2.0-or-later
//! Corrected software 64-bit integer builtins for the PSX target.
//!
//! `compiler-builtins` supplies the 64-bit arithmetic helpers the compiler
//! emits for `i64`/`u64` operations. On this `mipsel-sony-psx` target its
//! **signed** 64-bit divide (`__divdi3`) and remainder (`__moddi3`) produce
//! garbage (verified with `sdk/examples/hello-i64probe`: a signed `/` returns a
//! nonsense value regardless of input, while the *unsigned* `__udivdi3` /
//! `__umoddi3` and 64-bit mul/add/shift all work correctly).
//!
//! `compiler-builtins` exports these as **weak** symbols, so the strong
//! definitions here override them for every guest that links `psx-rt`. We route
//! the signed operations through the working unsigned path with the sign handled
//! explicitly, so `i64` division just works; no per-project workaround.
//!
//! This only overrides the two broken signed symbols; the (correct) unsigned
//! ones are left to `compiler-builtins`. i64 remains slower than i32 (the house
//! rule still says avoid it in per-frame game hot paths), but it is now
//! *correct* where the extra range/precision is worth the cost (e.g. a
//! spreadsheet that recalculates on edit).

/// Signed 64-bit division, routed through the working unsigned divide.
#[no_mangle]
pub extern "C" fn __divdi3(a: i64, b: i64) -> i64 {
    let q = (a.unsigned_abs() / b.unsigned_abs()) as i64;
    if (a < 0) ^ (b < 0) {
        q.wrapping_neg()
    } else {
        q
    }
}

/// Signed 64-bit remainder. The result takes the sign of the dividend.
#[no_mangle]
pub extern "C" fn __moddi3(a: i64, b: i64) -> i64 {
    let r = (a.unsigned_abs() % b.unsigned_abs()) as i64;
    if a < 0 {
        r.wrapping_neg()
    } else {
        r
    }
}
