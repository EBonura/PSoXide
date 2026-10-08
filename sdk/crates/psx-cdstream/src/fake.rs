//! A scripted CD drive for the host tests.
//!
//! It models the controller at the level the transport talks to it: commands
//! produce interrupt events in the order the real drive raises them, a
//! flag stays pending until it is acknowledged, and a data sector can be
//! popped only while its interrupt is the pending one. Faults are knobs:
//! a drive error at a given sector, a lost Pause acknowledge, a parameter
//! FIFO that never has room, a data FIFO that never fills, sectors still in
//! flight when Pause lands.

use crate::hw::CdHw;
use psx_hw::cd::{CMD_PAUSE, CMD_READN, CMD_SEEKL, CMD_SETLOC, CMD_SETMODE};
use std::collections::VecDeque;
use std::vec::Vec;

/// One word of the fake disc: sector `lba`, word `word`. Distinct for every
/// (sector, word) pair in the ranges the tests use.
pub fn sector_word(lba: u32, word: u32) -> u32 {
    lba.wrapping_mul(0x9E37_79B1) ^ word.wrapping_mul(0x85EB_CA6B) ^ 0xA5A5_0000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// A data sector is ready; its LBA.
    Data(u32),
    Complete,
    Acknowledge,
    /// The drive failed: status and error code bytes.
    Error(u8, u8),
}

impl Event {
    fn code(self) -> u8 {
        match self {
            Event::Data(_) => 1,
            Event::Complete => 2,
            Event::Acknowledge => 3,
            Event::Error(..) => 5,
        }
    }
}

/// A command the transport sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub command: u8,
    pub params: Vec<u8>,
}

pub struct FakeDrive {
    events: VecDeque<Event>,
    reading: bool,
    next_lba: u32,
    target: u32,
    /// Every command sent, in order.
    pub log: Vec<Sent>,
    /// The Setmode byte last sent.
    pub mode: u8,
    /// LBAs popped from the data FIFO, in order.
    pub popped: Vec<u32>,
    /// Calls to `discard_response`.
    pub discards: u32,
    /// The controller's interrupt output is enabled.
    pub output_enabled: bool,
    /// The CPU-side CD source is open.
    pub source_open: bool,
    /// Every `set_source_enabled` call, in order.
    pub source_calls: Vec<bool>,
    /// The VBlank counter the watchdog reads.
    pub vblank: u32,
    /// Value `clock()` returns next, and how far it moves per call.
    pub clock_now: u16,
    pub clock_step: u16,
    /// Trace words, when the transport records them.
    pub trace: Vec<u32>,

    // Fault knobs.
    /// Data interrupts that arrive after Pause is issued and before its
    /// acknowledge (sectors already on their way).
    pub sectors_in_flight_at_pause: u32,
    /// Raise a drive error instead of the data interrupt for this sector.
    pub error_at: Vec<u32>,
    /// Refuse (no room in the parameter FIFO) this command.
    pub refuse_command: Option<u8>,
    /// The data FIFO never fills.
    pub data_never_ready: bool,
    /// Pause is accepted but never answers.
    pub swallow_pause: bool,
}

impl FakeDrive {
    pub fn new() -> Self {
        FakeDrive {
            events: VecDeque::new(),
            reading: false,
            next_lba: 0,
            target: 0,
            log: Vec::new(),
            mode: 0,
            popped: Vec::new(),
            discards: 0,
            output_enabled: false,
            source_open: false,
            source_calls: Vec::new(),
            vblank: 0,
            clock_now: 0,
            clock_step: 0,
            trace: Vec::new(),
            sectors_in_flight_at_pause: 0,
            error_at: Vec::new(),
            refuse_command: None,
            data_never_ready: false,
            swallow_pause: false,
        }
    }

    /// Commands sent so far, as bytes only.
    pub fn commands(&self) -> Vec<u8> {
        self.log.iter().map(|sent| sent.command).collect()
    }

    /// The LBAs the Setloc commands asked for, decoded independently of the
    /// transport's own encoder.
    pub fn setloc_targets(&self) -> Vec<u32> {
        self.log
            .iter()
            .filter(|sent| sent.command == CMD_SETLOC)
            .map(|sent| decode_msf(&sent.params))
            .collect()
    }

    /// Raise an interrupt flag the transport did not ask for.
    pub fn inject_complete(&mut self) {
        self.events.push_front(Event::Complete);
    }

    /// Make the next interrupt available: the head of the pending events, or
    /// the next sector while reading. `false` when nothing is pending or the
    /// interrupt is masked, so the harness stops.
    pub fn raise(&mut self) -> bool {
        if self.events.is_empty() && self.reading {
            let lba = self.next_lba;
            self.next_lba += 1;
            if let Some(at) = self.error_at.iter().position(|&bad| bad == lba) {
                self.error_at.remove(at);
                self.reading = false;
                self.events.push_back(Event::Error(0x03, 0x04));
            } else {
                self.events.push_back(Event::Data(lba));
            }
        }
        !self.events.is_empty() && self.output_enabled && self.source_open
    }
}

fn decode_msf(params: &[u8]) -> u32 {
    let bcd = |v: u8| u32::from(v >> 4) * 10 + u32::from(v & 15);
    (bcd(params[0]) * 60 + bcd(params[1])) * 75 + bcd(params[2]) - 150
}

impl CdHw for FakeDrive {
    const TRACE: bool = true;

    fn issue(&mut self, command: u8, params: &[u8]) -> bool {
        if self.refuse_command == Some(command) {
            self.output_enabled = false;
            return false;
        }
        self.log.push(Sent {
            command,
            params: params.to_vec(),
        });
        // The command preamble acknowledges every pending flag.
        self.events.clear();
        match command {
            CMD_SETLOC => {
                self.target = decode_msf(params);
                self.events.push_back(Event::Acknowledge);
            }
            CMD_SEEKL => {
                self.events.push_back(Event::Acknowledge);
                self.events.push_back(Event::Complete);
                self.next_lba = self.target;
            }
            CMD_SETMODE => {
                self.mode = params[0];
                self.events.push_back(Event::Acknowledge);
            }
            CMD_READN => {
                self.reading = true;
                self.events.push_back(Event::Acknowledge);
            }
            CMD_PAUSE => {
                if self.reading {
                    for _ in 0..self.sectors_in_flight_at_pause {
                        let lba = self.next_lba;
                        self.next_lba += 1;
                        self.events.push_back(Event::Data(lba));
                    }
                }
                self.reading = false;
                if !self.swallow_pause {
                    self.events.push_back(Event::Acknowledge);
                    self.events.push_back(Event::Complete);
                }
            }
            other => panic!("the transport sent command {other:#04x}"),
        }
        self.output_enabled = true;
        true
    }

    fn interrupt_code(&mut self) -> u8 {
        self.events.front().map_or(0, |event| event.code())
    }

    fn error_response(&mut self) -> (u8, u8) {
        match self.events.front() {
            Some(Event::Error(status, code)) => (*status, *code),
            other => panic!("error_response with {other:?} pending"),
        }
    }

    fn discard_response(&mut self) {
        self.discards += 1;
    }

    fn acknowledge(&mut self, bits: u8) {
        if self
            .events
            .front()
            .is_some_and(|head| head.code() & bits != 0)
        {
            self.events.pop_front();
        }
    }

    fn silence_output(&mut self) {
        self.output_enabled = false;
    }

    fn drop_data_request(&mut self) {}

    unsafe fn pop_sector(&mut self, destination: *mut u32, store_words: usize) -> bool {
        if self.data_never_ready {
            return false;
        }
        let Some(Event::Data(lba)) = self.events.front().copied() else {
            panic!("pop_sector with {:?} pending", self.events.front());
        };
        self.popped.push(lba);
        for word in 0..store_words {
            // SAFETY: the caller guarantees `destination` is valid for
            // `store_words` word writes.
            unsafe {
                destination
                    .add(word)
                    .write_volatile(sector_word(lba, word as u32))
            };
        }
        true
    }

    fn set_source_enabled(&mut self, enabled: bool) {
        self.source_open = enabled;
        self.source_calls.push(enabled);
    }

    fn vblank_count(&mut self) -> u32 {
        self.vblank
    }

    fn trace(&mut self, word: u32) {
        self.trace.push(word);
    }

    fn clock(&mut self) -> u16 {
        let now = self.clock_now;
        self.clock_now = self.clock_now.wrapping_add(self.clock_step);
        now
    }
}
