//! The drive as the transport state machine sees it.
//!
//! [`CdHw`] is everything [`Engine`](crate::Engine) asks of the CD-ROM
//! controller and the system around it. The real implementation
//! (`Mmio`, target only) is a thin layer over `psx_io::periph::Cd`; the tests
//! use a scripted fake drive. The methods are the controller steps whose
//! *order* matters on silicon, grouped so that order lives in one place
//! (the implementation) and the state machine only decides which step comes
//! next.

/// Controller steps and system services the state machine needs.
///
/// Nothing here blocks beyond a short bounded poll: every method is called
/// from the CD interrupt handler or from a foreground section that has the
/// CD source masked.
pub trait CdHw {
    /// Whether to record controller events through [`trace`](Self::trace).
    /// Constant per implementation so the unused event words fold away.
    const TRACE: bool = false;

    /// Send one drive command and arm the controller to interrupt for its
    /// responses. Returns `false` when the parameter FIFO never had room (the
    /// command is not sent).
    ///
    /// The order is: mask the controller's interrupt output, acknowledge
    /// every pending flag and the CPU-side pending bit, discard any response
    /// bytes, empty the parameter FIFO, push the parameters, write the
    /// command byte, then enable the controller's interrupt output. Silicon
    /// is picky about it (the SDK's polled reader does the same).
    fn issue(&mut self, command: u8, params: &[u8]) -> bool;

    /// The controller's current interrupt flag (0 none, 1 data ready, 2
    /// complete, 3 acknowledge, 5 error).
    fn interrupt_code(&mut self) -> u8;

    /// The first two response bytes of a drive error: status and error code.
    fn error_response(&mut self) -> (u8, u8);

    /// Pop and discard the response FIFO.
    fn discard_response(&mut self);

    /// Acknowledge interrupt flag `bits` on the controller and the CPU-side
    /// pending bit. `acknowledge(0)` only clears the CPU-side bit.
    fn acknowledge(&mut self, bits: u8);

    /// Mask the controller's interrupt output, so nothing more is raised
    /// until the next [`issue`](Self::issue).
    fn silence_output(&mut self);

    /// Drop a data request left armed by the previous transfer. Called
    /// before the first command of a transfer that did not start from a
    /// recovery pause.
    fn drop_data_request(&mut self);

    /// Pop one sector's 2048 bytes from the data FIFO as 512 little-endian
    /// words, storing the first `store_words` of them at `destination`.
    /// The rest are popped and dropped (the FIFO has to be emptied either
    /// way). Arms the data request and waits a bounded time for the FIFO to
    /// fill; `false` if it never does.
    ///
    /// # Safety
    ///
    /// `destination` must be valid for `store_words` word writes (it is not
    /// used when `store_words` is 0).
    unsafe fn pop_sector(&mut self, destination: *mut u32, store_words: usize) -> bool;

    /// Let the CD interrupt source reach the CPU (or not).
    ///
    /// This only moves the mask. It must not acknowledge the CPU-side pending
    /// bit: the foreground closes the source around every call, and an
    /// interrupt that arrived meanwhile has to fire when the source opens.
    /// Stale flags are cleared by [`acknowledge`](Self::acknowledge) and by
    /// [`issue`](Self::issue).
    fn set_source_enabled(&mut self, enabled: bool);

    /// Monotonic VBlank count, the clock the no-progress timeout runs on.
    fn vblank_count(&mut self) -> u32;

    /// A free-running timestamp for measuring the handler, wrapping at 16
    /// bits. Implementations that do not measure return a constant.
    fn clock(&mut self) -> u16 {
        0
    }

    /// Record one trace word (only called when [`TRACE`](Self::TRACE) is set).
    fn trace(&mut self, _word: u32) {}
}
