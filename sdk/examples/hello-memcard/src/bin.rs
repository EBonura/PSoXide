// SPDX-License-Identifier: GPL-2.0-or-later
#![no_std]
#![no_main]

extern crate psx_rt;

#[no_mangle]
fn main() -> ! {
    hello_memcard_recovery::run_standalone()
}
