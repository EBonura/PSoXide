// SPDX-License-Identifier: GPL-2.0-or-later
//! The player's drive steps through the resident scene core's register-level
//! CD slot (`psx_scene_abi::slot::CD`), for a program the core hosts.
//!
//! Each step makes the same controller accesses, in the same order, as its
//! counterpart on psx-io's polled command layer (the `Drive for Cd` impl in
//! the parent module), so the player behaves the same on either. The
//! differences are the core's: a command's acknowledge wait is bounded by
//! time, not by spins, and calls the core's idle hook, and a command that
//! times out has its IRQ enables put back by the core.

use super::{getlocp_lba, Drive, PAUSE_SPINS};
use crate::cd::{lba_to_bcd_msf, wait_irq_flag};
use crate::periph::Cd;
use core::ptr::null_mut;
use psx_hw::cd::{
    irq, CMD_DEMUTE, CMD_GETLOCP, CMD_PAUSE, CMD_READS, CMD_SETFILTER, CMD_SETLOC, CMD_SETMODE,
};
use psx_scene_abi::cd_op;

extern "C" {
    /// The core's CD slot. A hosting program's link script defines the
    /// symbol at `psx_scene_abi::slot::CD`. `a` is an address or a value, the
    /// one 32-bit argument register either way.
    fn psx_core_cd(op: u32, a: usize, b: u32, c: *mut u8) -> u32;
}

fn slot(op: u32, a: usize, b: u32, c: *mut u8) -> u32 {
    // SAFETY: the slot is the core's ABI entry; every op this module sends
    // reads at most `b & 0xFF` bytes at `a` (a live slice) and writes at most
    // `cd_op::RESPONSE_BYTES` at `c` (null, or a buffer of that size).
    unsafe { psx_core_cd(op, a, b, c) }
}

/// Send `command` with `params` and wait for its acknowledge, the response
/// into `response` (null discards it). The response length, or `None` for a
/// drive error or a timeout.
#[inline(never)]
fn command(command: u8, params: &[u8], response: *mut u8) -> Option<u32> {
    crate::diag::cd_command(command);
    let b = params.len() as u32 | u32::from(command) << 8;
    let r = slot(cd_op::COMMAND, params.as_ptr() as usize, b, response);
    (r & 0xff == cd_op::ACKNOWLEDGED).then_some(r >> 8)
}

fn irq_flag() -> u8 {
    slot(cd_op::IRQ_FLAG, 0, 0, null_mut()) as u8
}

fn acknowledge(bits: u8) {
    slot(cd_op::IRQ_ACK, usize::from(bits), 0, null_mut());
}

fn drain() {
    slot(cd_op::RESPONSE, 0, 0, null_mut());
}

/// Set the controller's IRQ enables; the old ones.
fn set_irq_enable(mask: u8) -> u8 {
    slot(cd_op::IRQ_ENABLE, usize::from(mask), 0, null_mut()) as u8
}

/// What every polled command ends with: drain the response, acknowledge
/// everything, give the IRQ enables back.
#[inline(never)]
fn finish(saved: u8) {
    drain();
    acknowledge(irq::ACK_ALL);
    set_irq_enable(saved);
}

/// The drive's mixer at `(left, right)`, plain stereo.
#[inline(never)]
pub(super) fn set_mixer(_cd: &mut Cd, (left, right): (u8, u8)) {
    let levels = usize::from(left) | usize::from(right) << 16;
    slot(cd_op::MIXER, levels, 0, null_mut());
}

impl Drive for Cd {
    fn unmute(&mut self) -> bool {
        command(CMD_DEMUTE, &[], null_mut()).is_some()
    }
    fn set_mode(&mut self, mode: u8) -> bool {
        command(CMD_SETMODE, &[mode], null_mut()).is_some()
    }
    fn set_filter(&mut self, file: u8, channel: u8) -> bool {
        command(CMD_SETFILTER, &[file, channel], null_mut()).is_some()
    }
    fn set_target(&mut self, lba: u32) -> bool {
        command(CMD_SETLOC, &lba_to_bcd_msf(lba), null_mut()).is_some()
    }
    fn start_streaming(&mut self) -> bool {
        command(CMD_READS, &[], null_mut()).is_some()
    }
    #[inline(never)]
    fn pause(&mut self) -> bool {
        // The IRQ enables stay masked from the command to the second answer.
        let saved = set_irq_enable(0);
        let complete = command(CMD_PAUSE, &[], null_mut()).is_some()
            && wait_irq_flag(irq::COMPLETE, PAUSE_SPINS, irq_flag, |flag| {
                drain();
                acknowledge(flag);
            })
            .is_ok();
        finish(saved);
        complete
    }
    #[inline(never)]
    fn begin_stop(&mut self) -> u8 {
        set_mixer(self, (0, 0));
        let saved = set_irq_enable(0);
        acknowledge(irq::ACK_ALL);
        drain();
        // A Pause has no parameter, so the parameter FIFO wait cannot fail it.
        crate::diag::cd_command(CMD_PAUSE);
        slot(
            cd_op::COMMAND_NO_WAIT,
            0,
            u32::from(CMD_PAUSE) << 8,
            null_mut(),
        );
        saved
    }
    #[inline(never)]
    fn look_pause(&mut self) -> bool {
        match irq_flag() {
            irq::COMPLETE => return true,
            // Refused: the read carries on; send it again (the first send's enable is kept).
            irq::ERROR => {
                self.begin_stop();
            }
            // The first answer, or a sector of the read: nothing final yet. Drop it and keep
            // the output masked, as it is.
            0 => {}
            _ => finish(0),
        }
        false
    }
    #[inline(never)]
    fn end_stop(&mut self, saved: u8, volume: (u8, u8)) {
        finish(saved);
        set_mixer(self, volume);
    }
    #[inline(never)]
    fn head_lba(&mut self) -> Option<u32> {
        let mut response = [0u8; cd_op::RESPONSE_BYTES];
        let len = command(CMD_GETLOCP, &[], response.as_mut_ptr())?;
        getlocp_lba(response.get(..len as usize)?)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::cd::xa::{DriveSpeed, Event, File, Player};
    use std::cell::RefCell;
    use std::vec::Vec;

    /// One slot call: op, `a` (the parameter bytes for a command), `b`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Call {
        op: u32,
        a: Vec<u8>,
        b: u32,
    }

    #[derive(Default)]
    struct FakeCore {
        calls: Vec<Call>,
        /// Flags the next `IRQ_FLAG` ops read, in order; 0 when empty.
        flags: Vec<u8>,
        /// The GetlocP answer.
        getlocp: [u8; 8],
        enable: u8,
    }

    std::thread_local! {
        static CORE: RefCell<FakeCore> = RefCell::new(FakeCore::default());
    }

    /// The host stand-in for the core's slot.
    #[no_mangle]
    extern "C" fn psx_core_cd(op: u32, a: usize, b: u32, c: *mut u8) -> u32 {
        CORE.with(|core| {
            let mut core = core.borrow_mut();
            let params = if op <= cd_op::COMMAND_NO_WAIT && b & 0xff != 0 {
                // SAFETY: the module passes a live slice of `b & 0xFF` bytes.
                unsafe { std::slice::from_raw_parts(a as *const u8, (b & 0xff) as usize) }.to_vec()
            } else if op > cd_op::COMMAND_NO_WAIT {
                std::vec![a as u8, (a >> 8) as u8, (a >> 16) as u8, (a >> 24) as u8]
            } else {
                Vec::new()
            };
            core.calls.push(Call { op, a: params, b });
            match op {
                cd_op::COMMAND if (b >> 8) as u8 == CMD_GETLOCP && !c.is_null() => {
                    for (i, &byte) in core.getlocp.iter().enumerate() {
                        // SAFETY: the module's 16-byte response buffer.
                        unsafe { *c.add(i) = byte };
                    }
                    8 << 8
                }
                cd_op::IRQ_FLAG => {
                    if core.flags.is_empty() {
                        0
                    } else {
                        u32::from(core.flags.remove(0))
                    }
                }
                cd_op::IRQ_ENABLE => {
                    let old = core.enable;
                    core.enable = a as u8;
                    u32::from(old)
                }
                _ => 0,
            }
        })
    }

    fn take_calls() -> Vec<Call> {
        CORE.with(|core| core::mem::take(&mut core.borrow_mut().calls))
    }

    fn command_call(command: u8, params: &[u8]) -> Call {
        Call {
            op: cd_op::COMMAND,
            a: params.to_vec(),
            b: params.len() as u32 | u32::from(command) << 8,
        }
    }

    fn value_call(op: u32, value: u32) -> Call {
        Call {
            op,
            a: value.to_le_bytes().to_vec(),
            b: 0,
        }
    }

    #[test]
    fn a_play_sends_its_five_commands_as_slot_commands() {
        // SAFETY: a test-local token on the host; the slot is the fake above.
        let mut player = Player::new(unsafe { Cd::steal() });
        let song = File::new(1000, 104, 1, DriveSpeed::Double).song(2);
        player.play(song, true).unwrap();
        let msf = lba_to_bcd_msf(1000);
        assert_eq!(
            take_calls(),
            [
                command_call(CMD_DEMUTE, &[]),
                command_call(CMD_SETMODE, &[DriveSpeed::Double.mode()],),
                command_call(CMD_SETFILTER, &[1, 2]),
                command_call(CMD_SETLOC, &msf),
                command_call(CMD_READS, &[]),
            ]
        );
    }

    #[test]
    fn a_stop_silences_masks_and_sends_pause_without_waiting() {
        // SAFETY: as above.
        let mut player = Player::new(unsafe { Cd::steal() });
        CORE.with(|core| core.borrow_mut().enable = 0x1f);
        player
            .play(File::new(1000, 104, 1, DriveSpeed::Single).song(0), false)
            .unwrap();
        take_calls();
        player.stop();
        assert_eq!(
            take_calls(),
            [
                value_call(cd_op::MIXER, 0),
                value_call(cd_op::IRQ_ENABLE, 0),
                value_call(cd_op::IRQ_ACK, u32::from(irq::ACK_ALL)),
                value_call(cd_op::RESPONSE, 0),
                Call {
                    op: cd_op::COMMAND_NO_WAIT,
                    a: Vec::new(),
                    b: u32::from(CMD_PAUSE) << 8,
                },
            ]
        );
        assert!(player.is_stopping());
        // The second answer: the stop ends, the enables and the level come back.
        CORE.with(|core| core.borrow_mut().flags.push(irq::COMPLETE));
        assert!(!player.poll_stop());
        assert_eq!(
            take_calls(),
            [
                value_call(cd_op::IRQ_FLAG, 0),
                value_call(cd_op::RESPONSE, 0),
                value_call(cd_op::IRQ_ACK, u32::from(irq::ACK_ALL)),
                value_call(cd_op::IRQ_ENABLE, 0x1f),
                value_call(cd_op::MIXER, 0x0080_0080),
            ]
        );
    }

    #[test]
    fn a_poll_reads_the_head_through_getlocp() {
        // SAFETY: as above.
        let mut player = Player::new(unsafe { Cd::steal() });
        player
            .play(File::new(1000, 104, 1, DriveSpeed::Single).song(0), false)
            .unwrap();
        take_calls();
        // LBA 1010 is absolute frame 1160: 00:15:35, BCD 0x00 0x15 0x35.
        CORE.with(|core| core.borrow_mut().getlocp = [1, 1, 0, 0, 0, 0x00, 0x15, 0x35]);
        assert_eq!(player.poll(), Event::Playing);
        assert!(player.is_streaming());
        assert_eq!(player.head_sample().unwrap().sector, 10);
        assert_eq!(take_calls(), [command_call(CMD_GETLOCP, &[])]);
    }

    #[test]
    fn a_blocking_pause_waits_for_the_second_answer_with_the_enables_masked() {
        // SAFETY: as above.
        let mut cd = unsafe { Cd::steal() };
        CORE.with(|core| {
            let mut core = core.borrow_mut();
            core.enable = 0x1f;
            // A stale sector, then the completion.
            core.flags.extend([irq::DATA_READY, irq::COMPLETE]);
        });
        assert!(Drive::pause(&mut cd));
        assert_eq!(
            take_calls(),
            [
                value_call(cd_op::IRQ_ENABLE, 0),
                command_call(CMD_PAUSE, &[]),
                value_call(cd_op::IRQ_FLAG, 0),
                value_call(cd_op::RESPONSE, 0),
                value_call(cd_op::IRQ_ACK, u32::from(irq::DATA_READY)),
                value_call(cd_op::IRQ_FLAG, 0),
                value_call(cd_op::RESPONSE, 0),
                value_call(cd_op::IRQ_ACK, u32::from(irq::ACK_ALL)),
                value_call(cd_op::IRQ_ENABLE, 0x1f),
            ]
        );
    }
}
