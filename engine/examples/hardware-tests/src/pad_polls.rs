// SPDX-License-Identifier: GPL-2.0-or-later
//! PAD POLLS UNDER IRQ (v2.25, records `0xA80`-`0xA83`): ordinary pad polls,
//! not the configuration packets of `PAD CONFIG UNDER IRQ`.
//!
//! `0x6F5` showed that an interrupt handler running over a pad's `/ACK` loses
//! the transaction on a console. Configuration packets are masked since; the
//! polls a game makes every frame are not (`Controllers::poll` runs with
//! interrupts on and retries a failed poll with a fresh select, four tries in
//! all), so the cost of a lost `/ACK` there is a retry or, at worst, a frame
//! with no input. Each case makes [`POLLS`] polls of port 1 through a fresh
//! `Controllers`, one a VBlank, with the SDK's own retries, under a transport
//! that only watches (it counts transactions):
//!
//! * `0xA80`: as a game polls (interrupts on, VBlank only).
//! * `0xA81`: the same with root counter 1 forced to interrupt over the `/ACK`
//!   of the byte after the command in every transaction (the `0x6F5` handler,
//!   1,600 cycles), interrupts on.
//! * `0xA82`: the same forced interrupt, the poll made with interrupts masked
//!   (the handler runs after the poll).
//!
//! Fields: polls that answered; polls that needed more than one transaction;
//! polls that answered nothing (bits 0-7, a frame with no input) and the most
//! transactions one poll took (bits 8-15).
//!
//! * `0xA83`: the main menu's own polls from boot until CROSS started the run
//!   (the engine's `Controllers`, a pad met cold): menu frames, frames whose
//!   poll answered nothing, the first frame that answered (bits 0-11, `0xFFF`
//!   none) and the mode then (bits 12-15: 0 none, 1 digital, 2 analog, 3
//!   config, 4 other). On a cold SCPH-110 this is the record that says whether
//!   the pad was read from power on.
//!
//! Decoder labels: `pad_polls_shipped`, `pad_polls_forced_irq`,
//! `pad_polls_forced_irq_masked`, `pad_menu_from_boot`.

use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_hw::irq::source;
use psx_io::controller_port::{Port, Transport};
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_pad::{Controllers, PadMode, PadState};
use psx_rt::dispatch::{self, Source};
use psx_rt::interrupts;

type Records = [TimingRecord; TIMING_RECORD_COUNT];

const RECORD: u16 = 0xA80;
const MENU_RECORD: u16 = 0xA83;
/// Polls in a case: two seconds at one a VBlank.
const POLLS: u32 = 120;

/// The menu's polls since boot: frames, frames with no answer, first answer,
/// its mode code.
static mut MENU: [u32; 4] = [0, 0, 0xFFF, 0];

fn mode_code(mode: PadMode) -> u32 {
    match mode {
        PadMode::Disconnected => 0,
        PadMode::Digital => 1,
        PadMode::Analog => 2,
        PadMode::Config => 3,
        _ => 4,
    }
}

/// Count one main-menu frame's poll (from the scene, before the run starts).
pub(crate) fn note_menu_poll(pad: PadState) {
    // SAFETY: single thread; plain static.
    let menu = unsafe { &mut *core::ptr::addr_of_mut!(MENU) };
    let frame = menu[0];
    menu[0] = frame.saturating_add(1);
    if pad.mode.is_connected() {
        if menu[2] == 0xFFF {
            menu[2] = frame.min(0xFFE);
            menu[3] = mode_code(pad.mode);
        }
    } else {
        menu[1] = menu[1].saturating_add(1);
    }
}

/// The real port, watched: transactions begun, and the forced interrupt.
struct Watch {
    port: ControllerPort,
    force: bool,
    position: usize,
    transactions: u32,
}

impl Transport for Watch {
    fn status(&mut self) -> u32 {
        self.port.status()
    }
    fn receive(&mut self) -> u8 {
        self.port.receive()
    }
    fn transmit(&mut self, byte: u8) {
        if self.position == 0 {
            self.transactions += 1;
        }
        if self.position == 2 && self.force {
            timers::set_mode(
                Timer::Timer1,
                psx_hw::timers::mode::clock_source(0)
                    | psx_hw::timers::mode::IRQ_AT_TARGET
                    | psx_hw::timers::mode::RESET_AT_TARGET,
            );
            timers::set_target(Timer::Timer1, crate::config_irq::FIRE_CYCLES);
        }
        self.position += 1;
        self.port.transmit(byte);
    }
    fn set_mode(&mut self, value: u16) {
        self.port.set_mode(value);
    }
    fn set_baud(&mut self, value: u16) {
        self.port.set_baud(value);
    }
    fn set_control(&mut self, value: u16) {
        self.port.set_control(value);
    }
    fn control(&mut self) -> u16 {
        self.port.control()
    }
    fn clock(&mut self) -> u16 {
        self.port.clock()
    }
    fn finish(&mut self, succeeded: bool) {
        self.position = 0;
        // One interrupt a transaction: stop the counter until the next one.
        timers::set_mode(Timer::Timer1, 0);
        self.port.finish(succeeded);
    }
    fn mask_interrupts(&mut self) -> bool {
        self.port.mask_interrupts()
    }
    fn restore_interrupts(&mut self, were_enabled: bool) {
        self.port.restore_interrupts(were_enabled);
    }
}

#[optimize(size)]
#[inline(never)]
fn case(records: &mut Records, next: &mut usize, index: u16, force: bool, masked: bool) {
    let id = RECORD + index;
    crate::bounds::record_start(id);
    let mut watch = Watch {
        port: crate::config_irq::token(),
        force,
        position: 0,
        transactions: 0,
    };
    let mut pads = Controllers::new();
    // The handshake on first contact is not what this measures.
    let _ = pads.poll(&mut watch, Port::One);
    let (mut answered, mut retried, mut silent, mut most) = (0u32, 0u32, 0u32, 0u32);
    for _ in 0..POLLS {
        interrupts::wait_vblank();
        watch.transactions = 0;
        let state = if masked {
            let _guard = crate::IrqGuard::mask();
            *pads.poll(&mut watch, Port::One)
        } else {
            *pads.poll(&mut watch, Port::One)
        };
        timers::set_mode(Timer::Timer1, 0);
        psx_io::irq::acknowledge(1 << source::TIMER1);
        if state.mode.is_connected() {
            answered += 1;
        } else {
            silent += 1;
        }
        if watch.transactions > 1 {
            retried += 1;
        }
        most = most.max(watch.transactions);
    }
    push_timing_record(
        records,
        next,
        record(id, answered, retried, silent.min(0xFF) | most.min(0xFF) << 8),
    );
}

/// The three cases and the menu record. CPU interrupts on, as in a game.
#[optimize(size)]
#[inline(never)]
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    timers::set_mode(Timer::Timer0, 0);
    let mask_before = psx_io::irq::mask();
    dispatch::attach(Source::Timer1, crate::config_irq::spin_over_the_ack);
    psx_io::irq::set_mask(mask_before | 1 << source::TIMER1 | 1 << source::VBLANK);
    let were_on = psx_rt::interrupts::are_interrupts_enabled();
    if !were_on {
        // SAFETY: undone below; the pad and the timers are the step's own while it runs.
        unsafe { psx_io::irq::restore_cpu_interrupts(true) };
    }
    case(records, next, 0, false, false);
    case(records, next, 1, true, false);
    case(records, next, 2, true, true);
    if !were_on {
        // SAFETY: the matching undo of the enable above.
        unsafe { psx_io::irq::disable_cpu_interrupts() };
    }
    psx_io::irq::set_mask(mask_before);
    dispatch::detach(Source::Timer1);
    // SAFETY: single thread; plain static.
    let menu = unsafe { *core::ptr::addr_of!(MENU) };
    push_timing_record(
        records,
        next,
        record(MENU_RECORD, menu[0], menu[1], menu[2] | menu[3] << 12),
    );
}
