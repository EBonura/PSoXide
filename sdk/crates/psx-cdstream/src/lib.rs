//! Interrupt-driven CD-ROM sector streaming.
//!
//! The SDK's `SectorReader` reads a disc by waiting: fine at boot, wrong in
//! the middle of a game, where the drive delivers a sector every 6.5 ms and a
//! frame loop that looks once per frame loses most of them. This crate reads
//! the way a streamer has to: you queue [`Request`]s, the CD interrupt
//! handler pops one sector per interrupt straight into the request's
//! destination, and the foreground only submits, cancels and asks how far a
//! request has got. Nothing busy-waits for the drive.
//!
//! # What it does
//!
//! - **PIO, one sector per interrupt.** The sector is popped from the data
//!   FIFO by the CPU. Chopped CD DMA (channel 3) is unreliable on a console;
//!   the PIO path is the one that has loaded byte-perfect streams there. The
//!   price is CPU time while a read runs (about 1.3 ms per sector more than a
//!   working DMA is the figure on record), so read in bursts when there is
//!   work, not continuously.
//! - **Seek first.** Every transfer is Setloc, SeekL, Setmode (double speed),
//!   ReadN: the bracket the BIOS uses. A bare Setloc plus ReadN starts
//!   delivering while the mechanism is still settling and corrupts a long
//!   stream on a console.
//! - **Abort at the next sector.** [`Engine::cancel`] only sets a flag; the
//!   handler drops the next sector and pauses the drive itself. (A Pause sent
//!   from the foreground while sectors were arriving lost its acknowledge on
//!   silicon.) The request ends [`Outcome::Cancelled`] with the sectors that
//!   had landed; [`Request::remaining_after`] builds the request that
//!   continues it.
//! - **A small priority queue.** [`QUEUE_DEPTH`] requests wait behind the
//!   active one, most urgent [`Priority`] first.
//! - **Contiguous chaining.** A queued request that starts at the sector
//!   after the active one's last continues without a Pause or a seek: the
//!   handler swaps the destination and keeps popping. A layout-ordered group
//!   of regions costs one seek.
//! - **Resume.** A request can carry a resume budget
//!   ([`Request::with_resumes`]): after a drive error the transport pauses,
//!   seeks back to `lba + received` and carries on.
//! - **A drive arbiter.** CD-DA and XA playback and data reads cannot share
//!   the laser. [`Engine::request_audio_lease`] stops the read in flight,
//!   waits for the drive to stop, closes the CD interrupt source and (on the
//!   console) hands the controller token to the audio code;
//!   releasing the lease takes it back and queued reads carry on. End audio
//!   with Pause, never Stop: after Stop the motor spins down, reports status
//!   0 for a second or two, and reads started then failed on a console.
//!   A lease that is asked for and left pending is granted the moment the
//!   drive stops, ahead of any read queued meanwhile. Audio that must not
//!   hold up reads uses [`Engine::try_audio_lease`], which takes the drive
//!   only if it is free and leaves nothing pending, or ends the pending
//!   request with [`Engine::withdraw_audio_lease`].
//!
//! # Use
//!
//! ```ignore
//! // After the polled boot reads are done (SectorReader::release gives the
//! // token back):
//! psx_cdstream::install(cd, psx_cdstream::Config::DEFAULT).ok();
//!
//! // SAFETY: ROOM is not touched until the request has finished.
//! let request = unsafe { Request::new_raw(lba, sectors, ROOM.as_mut_ptr().cast()) };
//! let ticket = psx_cdstream::submit(request.with_priority(Priority::URGENT))?;
//!
//! // Each frame:
//! if let RequestState::Finished(done) = psx_cdstream::state(ticket) {
//!     // done.outcome, done.received
//! }
//! ```
//!
//! # How it is built
//!
//! [`Engine`] is the whole state machine, generic over [`CdHw`], the
//! controller as it sees it. On the console [`install`] puts an exception
//! wrapper in front of psx-rt's handler that calls the one global engine
//! once per CD interrupt, and the free functions (`submit`, `state`, ...)
//! reach it with the CD source closed. On the host the tests drive the same
//! engine against a scripted drive, so every phase, cancel point, error path,
//! chaining rule and lease transition is exercised without hardware. The
//! counters are published as `PSX_CD_STATS` ([`StreamStats`]) for tools that
//! read guest memory.
//!
//! Boot-time and blocking loads still use `psx_io::cd::reader::SectorReader`
//! (or `psx_pack::cd`): it needs the controller token, and so does
//! `install`, so the compiler keeps the two from sharing the drive.
//! `uninstall` hands the token back.
#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod engine;
mod hw;
mod request;

#[cfg(any(target_arch = "mips", doc))]
mod console;
#[cfg(any(target_arch = "mips", doc))]
pub use console::*;

pub use engine::*;
pub use hw::CdHw;
pub use request::*;

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
