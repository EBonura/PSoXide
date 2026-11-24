// SPDX-License-Identifier: GPL-2.0-or-later
//! The player's waited commands through the resident scene core's CD slot
//! (`psx_scene_abi::slot::CD`, op `psx_scene_abi::cd_op::COMMAND`), for a
//! program the core hosts.
//!
//! The core sends the command with the controller's IRQ enables masked, waits
//! for the acknowledge (bounded by time, calling the core's idle hook) and
//! hands the response back. Everything that waits on nothing (the second
//! answer of a Pause, the acknowledges, the IRQ enables) stays psx-io's own
//! register access, the same steps the polled implementation takes. The one
//! difference that comes from the core: a command that times out gets its IRQ
//! enables back.

use crate::cd::{
    ack_irq, drain_response_fifo, finish_failed_polled_command, irq_enable, irq_flag,
    set_irq_enable, wait_irq_flag,
};
use core::ptr::null_mut;
use psx_hw::cd::{irq, CMD_GETLOCP, CMD_PAUSE};
use psx_scene_abi::cd_op;

extern "C" {
    /// The core's CD slot. A hosting program's link script defines the
    /// symbol at `psx_scene_abi::slot::CD`. `a` is the parameter address, the
    /// one 32-bit argument register on the console.
    fn psx_core_cd(op: u32, a: usize, b: u32, c: *mut u8) -> u32;
}

/// Send `command` with `params` and wait for its acknowledge, the response
/// into `response` (null discards it). The response length, or `None` for a
/// drive error or a timeout.
#[inline(never)]
fn send(command: u8, params: &[u8], response: *mut u8) -> Option<u32> {
    crate::diag::cd_command(command);
    let b = params.len() as u32 | u32::from(command) << 8;
    // SAFETY: the slot is the core's ABI entry. Op COMMAND reads `b & 0xFF`
    // bytes at `a` (the live `params`) and writes at most
    // `cd_op::RESPONSE_BYTES` at `response` (null, or a buffer of that size).
    let r = unsafe { psx_core_cd(cd_op::COMMAND, params.as_ptr() as usize, b, response) };
    (r & 0xff == cd_op::ACKNOWLEDGED).then_some(r >> 8)
}

/// Send `command` with `params` and wait for its acknowledge; `false` for a
/// drive error or a timeout.
pub(super) fn command(command: u8, params: &[u8]) -> bool {
    send(command, params, null_mut()).is_some()
}

/// Pause and wait for the second answer, the IRQ enables masked from the
/// command to the end; `false` if the drive refused or either answer is late.
#[inline(never)]
pub(super) fn pause_until_complete(spins: u32) -> bool {
    let saved = irq_enable();
    set_irq_enable(0);
    let complete = command(CMD_PAUSE, &[])
        && wait_irq_flag(irq::COMPLETE, spins, irq_flag, |flag| {
            drain_response_fifo();
            ack_irq(flag);
        })
        .is_ok();
    finish_failed_polled_command(saved);
    complete
}

/// Absolute LBA the head is reading, if the drive answers GetlocP.
pub(super) fn head_lba() -> Option<u32> {
    let mut response = [0u8; cd_op::RESPONSE_BYTES];
    let len = send(CMD_GETLOCP, &[], response.as_mut_ptr())?;
    super::getlocp_lba(response.get(..len as usize)?)
}

#[cfg(test)]
mod tests {
    //! Only the steps that are all slot commands run on the host: the
    //! register steps touch the controller's addresses.
    extern crate std;
    use crate::cd::lba_to_bcd_msf;
    use crate::cd::xa::{DriveSpeed, Error, Event, File, Player};
    use crate::periph::Cd;
    use psx_hw::cd::{CMD_DEMUTE, CMD_GETLOCP, CMD_READS, CMD_SETFILTER, CMD_SETLOC, CMD_SETMODE};
    use psx_scene_abi::cd_op;
    use std::cell::RefCell;
    use std::vec::Vec;

    /// One slot call: the command byte and its parameter bytes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Call {
        command: u8,
        params: Vec<u8>,
    }

    #[derive(Default)]
    struct FakeCore {
        calls: Vec<Call>,
        /// Outcomes of the next commands, in order; acknowledged when empty.
        outcomes: Vec<u32>,
        /// The GetlocP answer.
        getlocp: [u8; 8],
    }

    std::thread_local! {
        static CORE: RefCell<FakeCore> = RefCell::new(FakeCore::default());
    }

    /// The host stand-in for the core's slot: op COMMAND only.
    #[no_mangle]
    extern "C" fn psx_core_cd(op: u32, a: usize, b: u32, c: *mut u8) -> u32 {
        assert_eq!(
            op,
            cd_op::COMMAND,
            "the player sends the core commands only"
        );
        CORE.with(|core| {
            let mut core = core.borrow_mut();
            let count = (b & 0xff) as usize;
            let params = if count == 0 {
                Vec::new()
            } else {
                // SAFETY: the module passes a live slice of `b & 0xFF` bytes.
                unsafe { std::slice::from_raw_parts(a as *const u8, count) }.to_vec()
            };
            let command = (b >> 8) as u8;
            core.calls.push(Call { command, params });
            let outcome = if core.outcomes.is_empty() {
                cd_op::ACKNOWLEDGED
            } else {
                core.outcomes.remove(0)
            };
            if outcome == cd_op::ACKNOWLEDGED && command == CMD_GETLOCP && !c.is_null() {
                for (i, &byte) in core.getlocp.iter().enumerate() {
                    // SAFETY: the module's 16-byte response buffer.
                    unsafe { *c.add(i) = byte };
                }
                return outcome | 8 << 8;
            }
            outcome
        })
    }

    fn take_calls() -> Vec<Call> {
        CORE.with(|core| core::mem::take(&mut core.borrow_mut().calls))
    }

    fn call(command: u8, params: &[u8]) -> Call {
        Call {
            command,
            params: params.to_vec(),
        }
    }

    #[test]
    fn a_play_sends_its_five_commands_through_the_core() {
        // SAFETY: a test-local token on the host; the slot is the fake above.
        let mut player = Player::new(unsafe { Cd::steal() });
        let song = File::new(1000, 104, 1, DriveSpeed::Double).song(2);
        player.play(song, true).unwrap();
        assert_eq!(
            take_calls(),
            [
                call(CMD_DEMUTE, &[]),
                call(CMD_SETMODE, &[DriveSpeed::Double.mode()]),
                call(CMD_SETFILTER, &[1, 2]),
                call(CMD_SETLOC, &lba_to_bcd_msf(1000)),
                call(CMD_READS, &[]),
            ]
        );
        assert!(player.is_playing());
    }

    #[test]
    fn a_command_the_core_does_not_see_acknowledged_fails_its_step() {
        // SAFETY: as above.
        let mut player = Player::new(unsafe { Cd::steal() });
        let song = File::new(1000, 104, 1, DriveSpeed::Single).song(0);
        CORE.with(|core| core.borrow_mut().outcomes.push(cd_op::DRIVE_ERROR));
        assert_eq!(player.play(song, false), Err(Error::Unmute));
        CORE.with(|core| {
            core.borrow_mut()
                .outcomes
                .extend([cd_op::ACKNOWLEDGED, cd_op::TIMEOUT]);
        });
        assert_eq!(player.play(song, false), Err(Error::SetMode));
        assert!(!player.is_playing());
        take_calls();
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
        assert_eq!(take_calls(), [call(CMD_GETLOCP, &[])]);
    }
}
