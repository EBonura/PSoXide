// SPDX-License-Identifier: GPL-2.0-or-later
//! Same-compilation Rust entry/import, borrowed arena and unload/re-admit probe.
#![no_std]
#![no_main]
extern crate psx_rt;

use psx_residency::arena::{checksum, CallerArena, Descriptor, Registry, StaticImport};
use psx_rt::tty;

// The packer writes this fixed-capacity transport fixture after linking. The
// production boundary accepts the identical completed words from caller CD I/O.
#[used]
#[no_mangle]
#[link_section = ".data.overlay_payload"]
static mut OVERLAY_PAYLOAD: [u32; 1024] = [0; 1024];
#[used]
#[no_mangle]
#[link_section = ".data.overlay_descriptor"]
static mut OVERLAY_DESCRIPTOR: [u32; 4] = [0; 4]; // words, checksum, callsite, entry

// This models a game's existing scene/title workspace, rather than reserving
// an arena inside the SDK. Gameplay overwrites it between title admissions.
#[repr(C, align(16))]
struct Workspace([u32; 1024]);
static mut WORKSPACE: Workspace = Workspace([0; 1024]);

struct State {
    count: u32,
    value: f32,
}

#[inline(never)]
#[no_mangle]
fn overlay_resident_service(value: f32) -> f32 {
    value * 1.5 + 0.25
}

// The function uses the exact private Rust type shared with its resident
// caller. No separate ABI declarations or serialized game-state struct exist.
#[inline(never)]
#[link_section = ".overlay.text"]
#[no_mangle]
unsafe fn overlay_cold_update(state: &mut State, frames: u32) -> u32 {
    state.count = state.count.wrapping_add(frames);
    state.value = overlay_resident_service(state.value);
    state.count
}

#[inline(never)]
#[no_mangle]
unsafe fn overlay_typed_call(state: &mut State, frames: u32) -> u32 {
    // Keep work after the call so this is a direct JAL, not a tail jump.
    let result = overlay_cold_update(state, frames);
    core::hint::black_box(result)
}

#[inline(never)]
#[no_mangle]
fn overlay_unloaded_trap() -> ! {
    tty::println("OVERLAY UNLOADED CALL FAIL");
    loop {
        core::hint::spin_loop();
    }
}

#[no_mangle]
fn main() {
    // SAFETY: single-threaded probe; no DMA, interrupts or asynchronous users
    // touch workspace, payload or the typed callsite during these scopes.
    let passed = unsafe { probe() };
    tty::println(if passed {
        "CALLER ARENA OVERLAY PASS"
    } else {
        "CALLER ARENA OVERLAY FAIL"
    });
    loop {
        core::hint::spin_loop();
    }
}

unsafe fn probe() -> bool {
    let descriptor = (&raw const OVERLAY_DESCRIPTOR).read();
    if descriptor[0] == 0 || descriptor[0] > 1024 {
        return false;
    }
    let payload = core::slice::from_raw_parts(
        (&raw const OVERLAY_PAYLOAD).cast::<u32>(),
        descriptor[0] as usize,
    );
    if checksum(payload) != descriptor[1] {
        return false;
    }
    let info = Descriptor {
        id: 1,
        linked: 0x80408000,
        checksum: descriptor[1],
    };
    let backing = &mut *(&raw mut WORKSPACE);
    let address = backing.0.as_mut_ptr() as usize as u32;
    let mut registry = Registry::<1>::new(1, 4096);
    let mut state = State {
        count: 0,
        value: 2.0,
    };
    let first;
    {
        let mut arena = match CallerArena::new(&mut backing.0, address) {
            Ok(v) => v,
            Err(_) => return false,
        };
        first = match registry.install(&mut arena, payload, info) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let dependencies = [first];
        let admission = match registry.admit(&mut arena, &dependencies) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let site = descriptor[2];
        let mut imports = [StaticImport::new(
            &mut *(site as *mut u32),
            site,
            first,
            descriptor[3],
        )];
        if admission.invoke(
            &mut imports,
            overlay_unloaded_trap as *const () as usize as u32,
            || overlay_typed_call(&mut state, 7),
        ) != Ok(7)
        {
            return false;
        }
    }
    tty::print("overlay first count=");
    tty::print_hex_u32(state.count);
    tty::print(" float=");
    tty::print_hex_u32(state.value.to_bits());
    tty::println("");
    let trap_target = overlay_unloaded_trap as *const () as usize as u32;
    let trapped_target = || {
        let instruction = (descriptor[2] as *const u32).read_volatile();
        ((descriptor[2] + 4) & 0xf0000000) | ((instruction & 0x03ffffff) << 2)
    };
    if trapped_target() != trap_target {
        return false;
    }
    registry.invalidate_all();
    // No live admission or module view exists while gameplay reuses storage.
    backing.0.fill(0xdeadbeef);
    {
        let mut arena = match CallerArena::new(&mut backing.0, address) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let second = match registry.install(&mut arena, payload, info) {
            Ok(v) => v,
            Err(_) => return false,
        };
        if registry.admit(&mut arena, &[first]).is_ok() {
            return false;
        }
        let dependencies = [second];
        let admission = match registry.admit(&mut arena, &dependencies) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let site = descriptor[2];
        let mut imports = [StaticImport::new(
            &mut *(site as *mut u32),
            site,
            second,
            descriptor[3],
        )];
        if admission.invoke(
            &mut imports,
            overlay_unloaded_trap as *const () as usize as u32,
            || overlay_typed_call(&mut state, 5),
        ) != Ok(12)
        {
            return false;
        }
    }
    tty::print("overlay reloaded count=");
    tty::print_hex_u32(state.count);
    tty::print(" float=");
    tty::print_hex_u32(state.value.to_bits());
    tty::println("");
    state.count == 12 && state.value == 5.125 && trapped_target() == trap_target
}
