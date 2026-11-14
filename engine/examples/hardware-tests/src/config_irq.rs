// SPDX-License-Identifier: GPL-2.0-or-later
//! PAD CONFIG UNDER IRQ: the analog handshake `psx_pad::Controllers` runs on a pad's first poll,
//! with an interrupt handler forced over the middle of it, and with the game's normal VBlank interrupts on.
//!
//! The handshake is three configuration packets (enter configuration, set analog and lock, exit). Nothing
//! reads their answers and a packet that fails part way is not repeated, so a packet abandoned part way
//! can leave a DualShock digital for the rest of the session. Three games carried console-found protection
//! against exactly that (the handshake before interrupts were enabled, and a request repeated while the pad
//! read digital), and moving them onto `Controllers` deleted it. The mechanism the SDK believes in: a pad's
//! `/ACK` is a pulse of about 90 cycles, 1,600 to 1,700 cycles after each byte is written (`0x610`-`0x618`),
//! that the exchange finds by reading `STAT`; an interrupt handler running over it hides it, the exchange
//! times out, and the packet is dropped. `Controllers` now masks CPU interrupts for every configuration
//! packet and asks again (at most three times) a pad that answered the handshake like a DualShock and still
//! reads digital. This step is the console's answer.
//!
//! Each case is [`ROUNDS`] rounds. A round puts the pad in digital mode (enter configuration, set digital
//! and unlocked, exit, then a poll that must read ID `0x41`), makes a fresh `Controllers` and polls port 1
//! once, which runs the handshake, with the case's interrupt. The handshake is `Controllers`' own code on
//! the real port; the transport under it only watches (it counts and records the bytes) and, in the cases
//! that force an interrupt, starts root counter 1 on the system clock just before the byte after the set-analog
//! command is written, so the interrupt lands [`FIRE_CYCLES`] later. That is before the `/ACK` of that byte
//! (1,600 or more after the write). The handler (`psx_rt::dispatch`, a root-counter-1 interrupt) spins
//! [`ISR_CYCLES`] of the system clock before it returns: 1,600 cycles, from about 1,100 after the write to about
//! 2,700, which covers the pulse (1,600 to 1,780) with 500 cycles before it and 900 after. A VBlank handler is
//! far shorter than that, so this is not a probability: it is the worst case, put where it hurts, and the
//! interrupt does not land again in the round.
//!
//! * `0x6F5`: the case where nothing is masked. The transport leaves `Transport::mask_interrupts` as a model
//!   does, so the handler runs over the `/ACK` as it would in a game before this change.
//! * `0x6F6`: the same interrupt with `Controllers` as shipped: its packets mask CPU interrupts, the timer
//!   interrupt waits and the handler runs after the packet.
//! * `0x6F7`: no forced interrupt; the plain first-poll handshake with VBlank interrupts enabled, as every
//!   game starts, [`ROUNDS`] times, as shipped.
//!
//! Fields of each record (16 bits each, so counts are four bits):
//!
//! * `rounds`: rounds that ended analog (bits 0-3), ended digital (4-7), ended any other way (8-11: no
//!   answer, the pad would not go digital to begin with), rounds tried (12-15).
//! * `bytes_seen`: the ID the first poll after the first set-analog packet answered (high byte, `0x73` the pad
//!   was analog at once, `0x41` the packet was lost) and the reply at position 1 of that packet (low byte,
//!   `0xF3`: the pad had entered configuration mode), both from the last round.
//! * `packets`: handler runs in the last round (bits 0-3), set-analog packets sent in the last round (4-7: 1,
//!   or 2 when `Controllers` asked again), bytes written of the first set-analog packet in the last round
//!   (8-11: 9 when it ran to the end, 3 when the `/ACK` of the byte after the command was missed), rounds
//!   whose first poll after the first set-analog packet already read analog (12-15).
//!
//! What a pass is, with a DualShock in port 1 (a digital-only pad makes every case read "ended digital, ID
//! 0x41" and `0x6F7` says so too; read it as the pad it is):
//!
//! * `0x6F7`: every round ended analog, one set-analog packet a round (`packets` 4-7 is 1), ID `0x73`
//!   straight after. A round that needed a second packet is the failure this step is about, in the wild.
//! * `0x6F6`: every round ended analog, one packet, nine bytes, ID `0x73` straight after, the handler run
//!   after it (bits 0-3 is 1). Masked, the interrupt cannot cost the pad its request.
//! * `0x6F5`: the console either loses the packet or it does not. Lost (3 bytes written, ID `0x41` straight
//!   after, a second packet, ended analog): the mechanism is real, and the second packet is what rescues the
//!   unmasked case. Not lost (9 bytes, analog at once): an interrupt over the pulse does not hide it on this
//!   console and the masking is only insurance; say so before burning more of the SDK on it.
//!
//! Fail: `0x6F6` or `0x6F7` with any round ending digital or needing a second packet.

use crate::console_tests::record;
use crate::{push_timing_record, TimingRecord, TIMING_RECORD_COUNT};
use psx_hw::irq::source;
use psx_io::controller_port::{Port, Timing, Transport};
use psx_io::periph::ControllerPort;
use psx_io::timers::{self, Timer};
use psx_pad::{Controllers, PadMode};
use psx_rt::dispatch::{self, Source};

type Records = [TimingRecord; TIMING_RECORD_COUNT];

/// First record of the step: three records, `0x6F5` to `0x6F7`.
const RECORD: u16 = 0x6F5;
/// Rounds a case runs (four bits in the record).
const ROUNDS: u32 = 15;
/// System-clock cycles from starting root counter 1 to its interrupt: after the shift of the byte begins
/// and well before its `/ACK`, which rises 1,600 or more cycles after the write.
const FIRE_CYCLES: u16 = 1_000;
/// System-clock cycles the handler spins: it starts about 1,100 after the write and ends about 2,700 after
/// it, over the whole of the pulse (1,600 to 1,780 after the write).
const ISR_CYCLES: u16 = 1_600;
/// Status reads between two configuration packets, the driver's own gap (one video frame).
const GAP_SPINS: u32 = 96 * 1_024;

/// Handler runs since the round began.
static mut HANDLER_RUNS: u32 = 0;

fn runs() -> u32 {
    // SAFETY: a volatile read of a word only the handler writes, from foreground code on one CPU.
    unsafe { core::ptr::read_volatile(&raw const HANDLER_RUNS) }
}

/// The handler of root counter 1: spin [`ISR_CYCLES`] of the system clock, count the run, acknowledge.
fn spin_over_the_ack() {
    let start = timers::counter(Timer::Timer0);
    while timers::counter(Timer::Timer0).wrapping_sub(start) < ISR_CYCLES {}
    // SAFETY: the handler runs with interrupts off on the dispatcher's stack and is the only writer.
    unsafe { core::ptr::write_volatile(&raw mut HANDLER_RUNS, runs() + 1) };
    // Reading the mode register clears the counter's own flag, and the acknowledge clears the pending bit.
    let _ = timers::mode(Timer::Timer1);
    psx_io::irq::acknowledge(1 << source::TIMER1);
}

fn token() -> ControllerPort {
    // SAFETY: a token is a logic guard, not a memory-safety one, and nothing else drives SIO0 while a
    // step of the run is in progress.
    unsafe { ControllerPort::steal() }
}

/// What the watching transport saw in a round.
#[derive(Clone, Copy, Default)]
struct Seen {
    /// Set-analog (`0x44`) packets begun.
    set_analog_packets: u32,
    /// Bytes written of the first of them, address byte included.
    first_bytes: u32,
    /// Reply at position 1 of the first.
    first_reply: u8,
    /// ID answered by the first poll (`0x42`) after the first set-analog packet; zero until then.
    id_after: u8,
}

/// The real port under `Controllers`, watching it: it records the packets and, when `force` is set,
/// starts root counter 1 for an interrupt in the middle of the first set-analog packet. `mask` says whether
/// the packets run with CPU interrupts masked, as `Controllers` asks of a real port.
struct Probe {
    port: ControllerPort,
    mask: bool,
    force: bool,
    seen: Seen,
    /// Bytes written in the transaction now (before `finish`) and its command byte.
    position: usize,
    command: u8,
}

impl Probe {
    fn new(mask: bool, force: bool) -> Self {
        Self {
            port: token(),
            mask,
            force,
            seen: Seen::default(),
            position: 0,
            command: 0,
        }
    }

    /// The first set-analog packet is the transaction now.
    fn in_first_set_analog(&self) -> bool {
        self.command == 0x44 && self.seen.set_analog_packets == 1
    }
}

impl Transport for Probe {
    fn status(&mut self) -> u32 {
        self.port.status()
    }
    fn receive(&mut self) -> u8 {
        let byte = self.port.receive();
        // A reply belongs to the byte written before it; position 0 is a drain before the first byte.
        if let Some(at) = self.position.checked_sub(1) {
            if at == 1 && self.in_first_set_analog() {
                self.seen.first_reply = byte;
            }
            if at == 1
                && self.command == 0x42
                && self.seen.set_analog_packets != 0
                && self.seen.id_after == 0
            {
                self.seen.id_after = byte;
            }
        }
        byte
    }
    fn transmit(&mut self, byte: u8) {
        if self.position == 1 {
            self.command = byte;
            if byte == 0x44 {
                self.seen.set_analog_packets += 1;
                if self.seen.set_analog_packets == 1 {
                    // The address byte went before the command was known.
                    self.seen.first_bytes = 1;
                }
            }
        }
        if self.in_first_set_analog() {
            self.seen.first_bytes += 1;
            // The byte after the command: its `/ACK` is the one the handler is to cover. Once only: the
            // packet's later bytes and the packets after it run unforced.
            if self.position == 2 && self.force {
                self.force = false;
                timers::set_mode(
                    Timer::Timer1,
                    psx_hw::timers::mode::clock_source(0)
                        | psx_hw::timers::mode::IRQ_AT_TARGET
                        | psx_hw::timers::mode::RESET_AT_TARGET,
                );
                timers::set_target(Timer::Timer1, FIRE_CYCLES);
            }
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
        self.command = 0;
        self.port.finish(succeeded);
    }
    fn mask_interrupts(&mut self) -> bool {
        self.mask && self.port.mask_interrupts()
    }
    fn restore_interrupts(&mut self, were_enabled: bool) {
        if self.mask {
            self.port.restore_interrupts(were_enabled);
        }
    }
}

/// One configuration packet on port 1, as the driver sends it, then the driver's gap.
fn send(port: &mut ControllerPort, packet: [u8; 8]) {
    let mut ok = port.begin_pad(Port::One, None);
    let mut i = 0;
    while ok && i <= packet.len() {
        let tx = if i == 0 { 0x01 } else { packet[i - 1] };
        ok = port.exchange(Port::One, tx, i == packet.len(), Timing::PAD).is_ok();
        i += 1;
    }
    port.finish(ok);
    port.delay(GAP_SPINS);
}

/// The ID a poll of port 1 answers, `0xFF` when nothing does.
fn poll_id(port: &mut ControllerPort) -> u8 {
    let mut id = 0xFF;
    if port.begin_pad(Port::One, None) {
        let mut ok = true;
        let mut i = 0;
        let mut length = 9;
        while ok && i < length {
            let tx = match i {
                0 => 0x01,
                1 => 0x42,
                _ => 0x00,
            };
            match port.exchange(Port::One, tx, i + 1 == length, Timing::PAD) {
                Ok(reply) => {
                    if i == 1 {
                        id = reply;
                        if reply == 0x41 {
                            length = 5;
                        }
                    }
                }
                Err(_) => ok = false,
            }
            i += 1;
        }
        port.finish(ok);
    }
    id
}

/// Put the pad in digital mode and unlocked, and say whether a poll then answers ID `0x41`.
fn make_digital() -> bool {
    let mut port = token();
    send(&mut port, [0x43, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
    send(&mut port, [0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    send(&mut port, [0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
    poll_id(&mut port) == 0x41
}

/// One case: [`ROUNDS`] rounds, one record.
fn case(records: &mut Records, next: &mut usize, index: u16, mask: bool, force: bool) {
    let id = RECORD + index;
    crate::bounds::record_start(id);
    let (mut analog, mut digital, mut other, mut tried) = (0u32, 0u32, 0u32, 0u32);
    let mut at_once = 0u32;
    let mut last = Seen::default();
    let mut last_runs = 0u32;
    while tried < ROUNDS {
        tried += 1;
        // A second go: the first round meets the pad as the earlier steps left it.
        if !make_digital() && !make_digital() {
            other += 1;
            continue;
        }
        // SAFETY: single thread; the handler only writes after the counter below fires.
        unsafe { core::ptr::write_volatile(&raw mut HANDLER_RUNS, 0) };
        let mut probe = Probe::new(mask, force);
        let mut pads = Controllers::new();
        let state = *pads.poll(&mut probe, Port::One);
        // The interrupt waits in `I_STAT` when the packet masked it; let it run before the next round.
        timers::set_mode(Timer::Timer1, 0);
        psx_io::irq::acknowledge(1 << source::TIMER1);
        match state.mode {
            PadMode::Analog => analog += 1,
            PadMode::Digital => digital += 1,
            _ => other += 1,
        }
        if probe.seen.id_after == 0x73 {
            at_once += 1;
        }
        last = probe.seen;
        last_runs = runs();
    }
    let rounds = analog | digital << 4 | other << 8 | tried << 12;
    let bytes = u32::from(last.id_after) << 8 | u32::from(last.first_reply);
    let packets = last_runs | last.set_analog_packets << 4 | last.first_bytes << 8 | at_once << 12;
    push_timing_record(records, next, record(id, rounds, bytes, packets));
}

/// The three cases. CPU interrupts must be on, as in a game: they are enabled here when an earlier step
/// left them off, and left as found.
pub(crate) fn run(records: &mut Records, next: &mut usize) {
    // Root counter 0 is the clock the pad's setup wait and this step's handler read.
    timers::set_mode(Timer::Timer0, 0);
    let mask_before = psx_io::irq::mask();
    dispatch::attach(Source::Timer1, spin_over_the_ack);
    psx_io::irq::set_mask(mask_before | 1 << source::TIMER1);
    // A game runs with CPU interrupts on; turn them on if an earlier step left them off.
    let were_on = psx_rt::interrupts::are_interrupts_enabled();
    if !were_on {
        // SAFETY: undone below; the pad and the timers are the step's own while it runs.
        unsafe { psx_io::irq::restore_cpu_interrupts(true) };
    }
    case(records, next, 0, false, true);
    case(records, next, 1, true, true);
    case(records, next, 2, true, false);
    if !were_on {
        // SAFETY: the matching undo of the enable above.
        unsafe { psx_io::irq::disable_cpu_interrupts() };
    }
    psx_io::irq::set_mask(mask_before);
    dispatch::detach(Source::Timer1);
}
