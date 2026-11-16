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
//! - **Park.** [`Request::park`] reads the sector before `lba` and keeps one word of it, so
//!   the next read from `lba` pays no long seek (a gate that knows where it reads next parks the
//!   head there while the frame renders). It is an ordinary one-sector request, so a program
//!   that never parks carries no park code.
//! - **An exact byte bound.** [`Request::with_byte_len`] stores only the chunk's bytes: the
//!   last sector is popped whole but written only as far as the chunk goes.
//! - **A watchdog.** [`Config::timeout_vblanks`] ends a transfer with no progress as
//!   [`FailureKind::Watchdog`], and [`IrqMaskPolicy::OnlyWhileBusy`] keeps the CD source
//!   closed between requests so the polled reader and the handler never share the controller.
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
//! # One transfer at a time
//!
//! A program that reads one chunk at a time (a loader that decodes each chunk as its sectors land,
//! a weapon stream that must not wait for the frame loop) does not need the queue, the priorities,
//! the chaining, the resumes or the audio lease, and a program cannot drop code it does not use. So
//! the engine is generic over what it carries ([`Engine`]'s `SLOTS`, `RESULTS` and `LEASE`
//! parameters, resolved at compile time; no cargo feature selects it) and [`SingleEngine`] is the
//! configuration with none of them: the transfer state machine and the interrupt handler, and the
//! compiler removes the rest. Its console front is the [`single`] module, with the same calls as
//! the crate root (`install`, `submit`, `state`, `cancel`, `stop_and_wait`, ...) and one
//! difference: [`single::submit`] starts the request now or returns [`SubmitError::QueueFull`].
//! Use [`Request::park`] to leave the head next to the next read, [`Request::with_byte_len`] for an
//! exact last sector, [`Config::irq_mask_policy`] with [`IrqMaskPolicy::OnlyWhileBusy`] when polled
//! reads share the controller, and [`sectors_landed`] for a progress read that is one volatile
//! load. A program that plays music through polled commands stops the read
//! ([`single::stop_and_wait`]), gives the controller back ([`single::uninstall`]) and installs
//! again afterwards: with one request at a time there is no lease to arbitrate.
//!
//! The handler (either engine) runs on psx-rt's dispatcher stack, [`HANDLER_STACK_BYTES`] bytes of
//! which the saved context takes 112: 400 for the handler, whose deepest path in the
//! single-transfer engine is measured by `stack-guard` in `hello-cdchunk` (see the crate's
//! `ADOPT` notes in the repository). The count of transfers a program started
//! ([`submissions`]) and the counters in [`PSX_CD_STATS`] are what a gate reads.
//!
//! # Next to the polled reader
//!
//! A program that loads with `SectorReader` (maps, tables, boot assets) and only
//! wants some reads kept out of the frame loop (a weapon model that streams in
//! while the world renders) has two ways to use this crate without giving up the
//! polled reader.
//!
//! The cheap one is [`drain`]: start the read with the polled reader as always,
//! hand its sectors to the CD interrupt with `drain::start`, look at
//! `drain::poll` once a frame and end the read with the reader's `stop`. There is
//! no transport, so it costs about 1.4 KB of RAM where the transport is about 9 KB
//! in a game built with profile-guided optimisation, and every command still goes
//! out through the polled path that has been proved on a console. A polled read
//! at 150 sectors a second needs a call every 6.7 ms and loses sectors when the
//! frame loop looks once per frame (the controller keeps a few, and delivers the
//! newest); the handler has no such limit.
//!
//! The full one is the transport below, for a program that can route its reads
//! through it: call [`ensure_installed`] with [`IrqMaskPolicy::OnlyWhileBusy`]
//! before a read, submit the request, and look at it once a frame.
//! Between requests the CD interrupt source is closed and the polled reader works
//! as before; before a polled command, [`stop_and_wait`] ends a request still in
//! flight. Use one or the other in a program, not both at once: each registers
//! for the CD source with psx-rt's dispatcher and the second would replace the first.
//!
//! # How it is built
//!
//! [`Engine`] is the whole state machine, generic over [`CdHw`], the
//! controller as it sees it. On the console the CD source's handler, registered
//! with psx-rt's dispatcher (`psx_rt::dispatch`), calls the one global engine
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
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

mod engine;
mod hw;
mod request;

#[cfg(any(target_arch = "mips", doc))]
mod console;
#[cfg(any(target_arch = "mips", doc))]
pub use console::*;
#[cfg(any(target_arch = "mips", doc))]
pub mod drain;

pub use engine::*;
pub use hw::CdHw;
pub use request::*;

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
