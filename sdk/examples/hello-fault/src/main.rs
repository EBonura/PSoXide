//! `hello-fault` -- a guest that panics on purpose, for the panic gate.
//!
//! Built with `-Cpanic=immediate-abort` (the shared policy, see
//! `tools/psoxide-size`), the out-of-range index below is a bare `break` at the
//! faulting instruction. psx-rt's exception handler records Cause, EPC and
//! BadVAddr in `__psx_rt_fault_*` and halts, and `psoxide-size faults` reads
//! them from a `--dump-ram` image and names the function around EPC.
//! `tools/abort_gate.sh` runs this and fails unless the fault was recorded.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_rt::interrupts;

static TABLE: [u8; 4] = [1, 2, 3, 4];
static mut INDEX: usize = 9;
static mut SINK: u8 = 0;

#[no_mangle]
fn main() {
    // Games install the runtime handler first; it is what records the fault.
    interrupts::install_vblank_counter();
    // A volatile read keeps the optimiser from proving the index out of range.
    let index = unsafe { core::ptr::read_volatile(core::ptr::addr_of!(INDEX)) };
    let value = TABLE[index];
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(SINK), value) };
    loop {}
}
