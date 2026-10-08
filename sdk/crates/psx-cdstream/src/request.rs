//! Read requests and what comes back from them.

/// Bytes in one data sector.
pub const SECTOR_BYTES: u32 = 2048;
/// Words in one data sector.
pub const SECTOR_WORDS: u32 = 512;

/// How urgent a request is. Lower numbers are served first.
///
/// The transport only orders by this number; what each level means is the
/// scheduler's business. The design maps demand loads to [`URGENT`](Self::URGENT)
/// and speculative prefetch to [`BACKGROUND`](Self::BACKGROUND).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub u8);

impl Priority {
    /// Served before everything else.
    pub const URGENT: Self = Self(0);
    /// The default.
    pub const NORMAL: Self = Self(128);
    /// Served only when nothing more urgent waits.
    pub const BACKGROUND: Self = Self(255);
}

impl Default for Priority {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// A run of sectors to read into memory.
///
/// Built with [`Request::new_raw`], then refined with the `with_*` methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub(crate) lba: u32,
    pub(crate) sectors: u32,
    pub(crate) destination: *mut u32,
    /// 0 means every sector is stored whole.
    pub(crate) byte_len: u32,
    pub(crate) priority: Priority,
    pub(crate) resumes: u8,
}

impl Request {
    /// Read `sectors` sectors starting at `lba` (program-relative, shifted by
    /// the loader's disc base like every SDK disc read) into `destination`.
    ///
    /// # Safety
    ///
    /// `destination` must be word-aligned and valid for writes of
    /// `sectors * 2048` bytes, and nothing else may read or write that memory
    /// from the moment the request is submitted until it has finished
    /// (see [`Engine::state`](crate::Engine::state)). The transport writes
    /// from the interrupt handler, so the memory is not held by any borrow the
    /// compiler can see. Memory that is only ever read through `read_volatile`
    /// until the request finishes meets this.
    pub const unsafe fn new_raw(lba: u32, sectors: u32, destination: *mut u32) -> Self {
        Request {
            lba,
            sectors,
            destination,
            byte_len: 0,
            priority: Priority::NORMAL,
            resumes: 0,
        }
    }

    /// Serve this request at `priority`.
    #[must_use]
    pub const fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Let the transport pick this request up again from where it stopped,
    /// up to `times` times, when the drive reports an error mid-transfer.
    /// Each resume costs the recovery pause and one seek. With 0 (the
    /// default) an error ends the request as [`Outcome::Failed`] and the
    /// caller decides.
    #[must_use]
    pub const fn with_resumes(mut self, times: u8) -> Self {
        self.resumes = times;
        self
    }

    /// Store only `bytes` bytes instead of `sectors * 2048`: the last sector
    /// is popped whole but only its first words, rounded up to a word, are
    /// written, so a transfer never writes past a chunk that does not end on
    /// a sector boundary.
    ///
    /// `bytes` must lie in `(sectors - 1) * 2048 + 1 ..= sectors * 2048`;
    /// anything else is ignored (every sector stored whole). The destination
    /// then only has to be valid for `bytes` rounded up to a word.
    #[must_use]
    pub const fn with_byte_len(mut self, bytes: u32) -> Self {
        let sectors = self.sectors;
        if sectors > 0 && bytes > (sectors - 1) * SECTOR_BYTES && bytes <= sectors * SECTOR_BYTES {
            self.byte_len = bytes;
        }
        self
    }

    /// First sector.
    pub const fn lba(&self) -> u32 {
        self.lba
    }

    /// Sector count.
    pub const fn sectors(&self) -> u32 {
        self.sectors
    }

    /// Priority.
    pub const fn priority(&self) -> Priority {
        self.priority
    }

    /// The sector after the last one, where a request that can chain onto
    /// this one starts.
    pub const fn end_lba(&self) -> u32 {
        self.lba.wrapping_add(self.sectors)
    }

    /// The rest of this request after `received` sectors landed: the request
    /// to submit to continue an aborted or failed transfer. `None` when
    /// nothing is left.
    ///
    /// The result is a request like any other. It starts with a fresh seek,
    /// at `lba + received`, into the destination advanced by `received`
    /// sectors, with the same priority and resume budget.
    ///
    /// # Safety
    ///
    /// `received` must be a count the transport reported for this request
    /// (or lower): the memory the new request writes is the tail of this
    /// request's destination, and the safety contract of
    /// [`new_raw`](Self::new_raw) applies to it again.
    pub unsafe fn remaining_after(&self, received: u32) -> Option<Request> {
        if received >= self.sectors {
            return None;
        }
        // SAFETY: `received < sectors`, so the advanced pointer stays inside
        // the `sectors * 2048` bytes the caller vouched for in `new_raw`.
        let destination = unsafe {
            self.destination
                .add(received as usize * SECTOR_WORDS as usize)
        };
        let byte_len = if self.byte_len == 0 {
            0
        } else {
            self.byte_len - received * SECTOR_BYTES
        };
        Some(Request {
            lba: self.lba + received,
            sectors: self.sectors - received,
            destination,
            byte_len,
            priority: self.priority,
            resumes: self.resumes,
        })
    }

    /// Words to store from sector `index` of this request.
    pub(crate) const fn store_words(&self, index: u32) -> usize {
        if self.byte_len == 0 {
            return SECTOR_WORDS as usize;
        }
        let remaining = self.byte_len - index * SECTOR_BYTES;
        let bytes = if remaining < SECTOR_BYTES {
            remaining
        } else {
            SECTOR_BYTES
        };
        bytes.div_ceil(4) as usize
    }
}

/// Names a submitted request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(pub(crate) u32);

impl Ticket {
    /// The ticket's number, for logs.
    pub const fn id(self) -> u32 {
        self.0
    }
}

/// Why a request was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// [`QUEUE_DEPTH`](crate::QUEUE_DEPTH) requests are already waiting.
    QueueFull,
    /// The destination pointer is null.
    NullDestination,
    /// The destination is not word-aligned.
    MisalignedDestination,
    /// The request has no sectors.
    Empty,
}

/// How a request ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every sector landed.
    Done,
    /// Stopped by a cancel or an audio lease. `received` sectors landed.
    Cancelled,
    /// The drive or the controller failed. `received` sectors landed.
    Failed(Failure),
}

/// A finished request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Which request.
    pub ticket: Ticket,
    /// How it ended.
    pub outcome: Outcome,
    /// Sectors stored at the destination, starting from its first word.
    pub received: u32,
}

/// Where a request is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestState {
    /// Waiting behind other requests, or for the drive to be handed back.
    Queued,
    /// The drive is working on it; `received` sectors have landed.
    Active {
        /// Sectors stored so far.
        received: u32,
    },
    /// It ended. Results of the last [`RECENT_RESULTS`](crate::RECENT_RESULTS)
    /// requests are kept.
    Finished(Completion),
    /// Not queued, not active, and no longer remembered (or never submitted).
    Unknown,
}

/// A transfer failure, as the code the transport records for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure(pub(crate) u32);

/// What a [`Failure`] code says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The parameter FIFO never had room for this command.
    CommandRefused {
        /// The command byte.
        command: u8,
    },
    /// The data FIFO never filled after a sector interrupt.
    DataNotReady,
    /// The drive answered with an error.
    Drive {
        /// Status byte of the error response.
        status: u8,
        /// Error code byte.
        code: u8,
    },
    /// An interrupt arrived that the current phase has no use for.
    UnexpectedInterrupt {
        /// The interrupt flag.
        flag: u8,
    },
    /// No progress for the configured number of VBlanks.
    Watchdog,
    /// A code this version does not produce.
    Unknown,
}

impl Failure {
    pub(crate) const fn command_refused(command: u8) -> Self {
        Failure(0xfe00_0000 | (command as u32) << 8)
    }

    pub(crate) const fn data_not_ready(phase: u32) -> Self {
        Failure(0xfa00_0000 | phase)
    }

    pub(crate) const fn drive(status: u8, phase: u32, code: u8) -> Self {
        Failure(0x0500_0000 | (status as u32) << 16 | phase << 8 | code as u32)
    }

    pub(crate) const fn unexpected(phase: u32, flag: u8) -> Self {
        Failure(0xf900_0000 | phase << 8 | flag as u32)
    }

    pub(crate) const fn watchdog(phase: u32) -> Self {
        Failure(0xff00_0000 | phase)
    }

    /// The raw code, as published in `PSX_CD_STATS`.
    pub const fn code(self) -> u32 {
        self.0
    }

    /// Decode the code.
    pub const fn kind(self) -> FailureKind {
        match self.0 >> 24 {
            0xfe => FailureKind::CommandRefused {
                command: (self.0 >> 8) as u8,
            },
            0xfa => FailureKind::DataNotReady,
            0x05 => FailureKind::Drive {
                status: (self.0 >> 16) as u8,
                code: self.0 as u8,
            },
            0xf9 => FailureKind::UnexpectedInterrupt { flag: self.0 as u8 },
            0xff => FailureKind::Watchdog,
            _ => FailureKind::Unknown,
        }
    }
}
