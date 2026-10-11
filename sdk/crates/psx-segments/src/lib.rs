// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounded segment selection and independent launch requests.
//!
//! Games render the selection with their existing debug menu, own the load and
//! spawn types, and apply their complete save state. Diagnostic reset launches
//! remain a separate operation and never count as progression-backed coverage.
//! This crate neither polls controllers nor owns a renderer or a mailbox.
#![no_std]

use core::ops::Range;

/// Current control request version, separate from a game's observed gate state.
pub const REQUEST_VERSION: u32 = 1;
/// Launch from a real captured playthrough state.
pub const MODE_PLAYTHROUGH: u32 = 1;
/// Launch with the game's existing diagnostic reset behavior.
pub const MODE_DIAGNOSTIC: u32 = 2;

/// Immutable evidence for a game's complete captured state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Provenance<'a> {
    /// Game save format version used to decode the capture.
    pub save_version: u32,
    /// Actual retained capture path or immutable repository reference.
    pub capture: &'a str,
    /// SHA-256 of the captured bytes, in lowercase hexadecimal.
    pub capture_sha256: &'a str,
    /// Retained normal-play journey or recording receipt.
    pub playthrough: &'a str,
}

impl Provenance<'_> {
    fn is_valid(&self) -> bool {
        self.save_version != 0
            && !self.capture.is_empty()
            && !self.playthrough.is_empty()
            && self.capture_sha256.len() == 64
            && self
                .capture_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}

/// Availability of earned progression at this segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Progression<'a, State> {
    /// No complete captured state exists; explain the missing evidence.
    Unverified(&'a str),
    /// Complete game-owned state decoded from a normal playthrough capture.
    Verified {
        /// State including every progression field needed for independent play.
        state: State,
        /// Evidence that produced this state.
        provenance: &'a Provenance<'a>,
    },
}

/// A row shared by the debug page, coverage export and journey launch lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Segment<'a, Load, Spawn, State> {
    /// Stable game-assigned identifier; distinct from the display row index.
    pub id: u32,
    /// Player-facing segment name.
    pub name: &'a str,
    /// Game-owned load target.
    pub load: Load,
    /// Game-owned spawn description.
    pub spawn: Spawn,
    /// Full earned state, or the reason independent progression is unavailable.
    pub progression: Progression<'a, State>,
    /// Whether the game supports an explicit diagnostic reset for this row.
    pub diagnostic: bool,
}

/// Fixed-width control request. Zero is an idle mailbox, never a launch.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LaunchRequest {
    /// Must equal [`REQUEST_VERSION`].
    pub version: u32,
    /// Stable [`Segment::id`] to launch.
    pub segment_id: u32,
    /// [`MODE_PLAYTHROUGH`] or [`MODE_DIAGNOSTIC`].
    pub mode: u32,
}

impl LaunchRequest {
    /// Idle request for initialization and clearing a consumed mailbox.
    pub const IDLE: Self = Self {
        version: 0,
        segment_id: 0,
        mode: 0,
    };
}

/// Validated launch, borrowing the table without copying a save payload.
#[derive(Debug, Eq, PartialEq)]
pub enum Launch<'table, 'data, Load, Spawn, State> {
    /// Complete captured state, available only with valid evidence metadata.
    Playthrough {
        /// Matching table row.
        segment: &'table Segment<'data, Load, Spawn, State>,
        /// Exact captured state borrowed from that row.
        state: &'table State,
        /// Capture provenance borrowed from that row.
        provenance: &'table Provenance<'data>,
    },
    /// Explicit diagnostic reset, with no earned state supplied.
    Diagnostic(&'table Segment<'data, Load, Spawn, State>),
}

/// Request or table rejection, before any game callback runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchError {
    /// Unsupported control protocol version.
    Version,
    /// Unknown launch mode.
    Mode,
    /// Table has ambiguous identifiers.
    DuplicateId,
    /// Stable identifier is absent from the table.
    UnknownId,
    /// No full playthrough state has been captured.
    Unverified,
    /// Capture metadata is absent or malformed.
    Provenance,
    /// Row does not permit diagnostic reset.
    DiagnosticUnavailable,
}

/// Validate the whole identifier table and resolve a request before side effects.
pub fn validate<'table, 'data, Load, Spawn, State>(
    request: LaunchRequest,
    table: &'table [Segment<'data, Load, Spawn, State>],
) -> Result<Launch<'table, 'data, Load, Spawn, State>, LaunchError> {
    if request.version != REQUEST_VERSION {
        return Err(LaunchError::Version);
    }
    if request.mode != MODE_PLAYTHROUGH && request.mode != MODE_DIAGNOSTIC {
        return Err(LaunchError::Mode);
    }
    for (index, segment) in table.iter().enumerate() {
        if table[..index]
            .iter()
            .any(|previous| previous.id == segment.id)
        {
            return Err(LaunchError::DuplicateId);
        }
    }
    let segment = table
        .iter()
        .find(|segment| segment.id == request.segment_id)
        .ok_or(LaunchError::UnknownId)?;
    if request.mode == MODE_DIAGNOSTIC {
        return if segment.diagnostic {
            Ok(Launch::Diagnostic(segment))
        } else {
            Err(LaunchError::DiagnosticUnavailable)
        };
    }
    match &segment.progression {
        Progression::Unverified(_) => Err(LaunchError::Unverified),
        Progression::Verified { state, provenance } if provenance.is_valid() => {
            Ok(Launch::Playthrough {
                segment,
                state,
                provenance,
            })
        }
        Progression::Verified { .. } => Err(LaunchError::Provenance),
    }
}

/// An explicit result for a once-consumed control request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumeResult<T, E> {
    /// Mailbox was zero; neither callback ran.
    Idle,
    /// Validation failed; neither callback ran.
    Rejected(LaunchError),
    /// The selected game callback ran exactly once and returned this result.
    Applied(Result<T, E>),
}

/// Consume a locally owned request before validation or callbacks.
///
/// Games that expose a volatile mailbox must first read and clear it through
/// their own synchronized control channel, then pass that local request here.
/// Never share this ordinary reference directly with an asynchronous writer.
/// Callback failure still consumes the request; retries require a new request.
pub fn consume<Load, Spawn, State, T, E>(
    request: &mut LaunchRequest,
    table: &[Segment<'_, Load, Spawn, State>],
    playthrough: impl FnOnce(&Load, &Spawn, &State, &Provenance<'_>) -> Result<T, E>,
    diagnostic: impl FnOnce(&Load, &Spawn) -> Result<T, E>,
) -> ConsumeResult<T, E> {
    let pending = core::mem::replace(request, LaunchRequest::IDLE);
    if pending == LaunchRequest::IDLE {
        return ConsumeResult::Idle;
    }
    match validate(pending, table) {
        Err(error) => ConsumeResult::Rejected(error),
        Ok(Launch::Playthrough {
            segment,
            state,
            provenance,
        }) => ConsumeResult::Applied(playthrough(
            &segment.load,
            &segment.spawn,
            state,
            provenance,
        )),
        Ok(Launch::Diagnostic(segment)) => {
            ConsumeResult::Applied(diagnostic(&segment.load, &segment.spawn))
        }
    }
}

/// Allocator-free selection whose bounds are fixed at construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    selected: usize,
    count: usize,
}

impl Selection {
    /// Select the first row of a table with `count` rows.
    pub const fn new(count: usize) -> Self {
        Self { selected: 0, count }
    }
    /// Current row, or no row for an empty table.
    pub const fn selected(&self) -> Option<usize> {
        if self.count == 0 {
            None
        } else {
            Some(self.selected)
        }
    }
    /// Move up one row, wrapping at the first row.
    pub fn previous(&mut self) {
        if self.count != 0 {
            self.selected = if self.selected == 0 {
                self.count - 1
            } else {
                self.selected - 1
            };
        }
    }
    /// Move down one row, wrapping at the last row.
    pub fn next(&mut self) {
        if self.count != 0 {
            self.selected = if self.selected == self.count - 1 {
                0
            } else {
                self.selected + 1
            };
        }
    }
    /// Visible rows, centered on selection where the table bounds permit.
    /// A zero row capacity always returns an empty range.
    pub fn visible_range(&self, row_count: usize) -> Range<usize> {
        let visible = row_count.min(self.count);
        let start = self
            .selected
            .saturating_sub(visible / 2)
            .min(self.count - visible);
        start..start + visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    // Synthetic unit fixture only; this is not a captured game state.
    fn row() -> Segment<'static, u16, [i32; 3], u32> {
        Segment {
            id: 7,
            name: "unit fixture",
            load: 96,
            spawn: [1, 2, 3],
            progression: Progression::Verified {
                state: 42,
                provenance: &Provenance {
                    save_version: 4,
                    capture: "unit fixture bytes",
                    capture_sha256:
                        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    playthrough: "unit fixture receipt",
                },
            },
            diagnostic: true,
        }
    }
    fn request(mode: u32) -> LaunchRequest {
        LaunchRequest {
            version: REQUEST_VERSION,
            segment_id: 7,
            mode,
        }
    }

    #[test]
    fn selection_stays_bounded_and_wraps() {
        let mut empty = Selection::new(0);
        empty.previous();
        empty.next();
        assert_eq!(empty.selected(), None);
        assert_eq!(empty.visible_range(5), 0..0);
        let mut selection = Selection::new(10);
        selection.previous();
        assert_eq!(selection.selected(), Some(9));
        assert_eq!(selection.visible_range(4), 6..10);
        selection.next();
        assert_eq!(selection.selected(), Some(0));
        assert_eq!(selection.visible_range(4), 0..4);
        assert_eq!(selection.visible_range(0), 0..0);
        assert_eq!(selection.visible_range(usize::MAX), 0..10);
        let mut single = Selection::new(1);
        single.next();
        single.previous();
        assert_eq!(single.selected(), Some(0));
    }

    #[test]
    fn malformed_requests_and_tables_fail_closed() {
        let table = [row()];
        assert_eq!(
            validate(LaunchRequest::IDLE, &table),
            Err(LaunchError::Version)
        );
        assert_eq!(validate(request(0), &table), Err(LaunchError::Mode));
        assert_eq!(
            validate(
                request(MODE_PLAYTHROUGH),
                &[] as &[Segment<'_, u16, [i32; 3], u32>]
            ),
            Err(LaunchError::UnknownId)
        );
        let mut unknown = request(MODE_PLAYTHROUGH);
        unknown.segment_id = u32::MAX;
        assert_eq!(validate(unknown, &table), Err(LaunchError::UnknownId));
        assert_eq!(
            validate(request(MODE_PLAYTHROUGH), &[row(), row()]),
            Err(LaunchError::DuplicateId)
        );
    }

    #[test]
    fn rejection_consumes_request_without_callbacks() {
        let mut unverified = row();
        unverified.progression = Progression::Unverified("capture missing");
        let calls = Cell::new(0);
        let mut pending = request(MODE_PLAYTHROUGH);
        let result: ConsumeResult<(), ()> = consume(
            &mut pending,
            &[unverified],
            |_, _, _, _| {
                calls.set(calls.get() + 1);
                Ok(())
            },
            |_, _| {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, ConsumeResult::Rejected(LaunchError::Unverified));
        assert_eq!(pending, LaunchRequest::IDLE);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn callback_failure_still_consumes_exactly_once() {
        let calls = Cell::new(0);
        let table = [row()];
        let mut pending = request(MODE_PLAYTHROUGH);
        let result = consume(
            &mut pending,
            &table,
            |load, spawn, state, provenance| {
                calls.set(calls.get() + 1);
                assert_eq!((*load, *spawn, *state), (96, [1, 2, 3], 42));
                assert_eq!(
                    *provenance,
                    match table[0].progression {
                        Progression::Verified { provenance, .. } => *provenance,
                        _ => unreachable!(),
                    }
                );
                Err::<(), _>("game refused")
            },
            |_, _| panic!("unexpected diagnostic launch"),
        );
        assert_eq!(result, ConsumeResult::Applied(Err("game refused")));
        let again: ConsumeResult<(), ()> = consume(
            &mut pending,
            &table,
            |_, _, _, _| panic!("request replayed"),
            |_, _| panic!("request replayed"),
        );
        assert_eq!(again, ConsumeResult::Idle);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn diagnostics_do_not_need_or_supply_earned_state() {
        let mut segment = row();
        segment.progression = Progression::Unverified("capture missing");
        let mut pending = request(MODE_DIAGNOSTIC);
        let result: ConsumeResult<u16, ()> = consume(
            &mut pending,
            &[segment],
            |_, _, _, _| panic!("unverified state used"),
            |load, _| Ok(*load),
        );
        assert_eq!(result, ConsumeResult::Applied(Ok(96)));
        segment.diagnostic = false;
        assert_eq!(
            validate(request(MODE_DIAGNOSTIC), &[segment]),
            Err(LaunchError::DiagnosticUnavailable)
        );
    }

    #[test]
    fn invalid_provenance_never_becomes_playthrough_launch() {
        let mut segment = row();
        let mut invalid = match segment.progression {
            Progression::Verified { provenance, .. } => *provenance,
            _ => unreachable!(),
        };
        invalid.capture_sha256 = "missing";
        segment.progression = Progression::Verified {
            state: 42,
            provenance: &invalid,
        };
        assert_eq!(
            validate(request(MODE_PLAYTHROUGH), &[segment]),
            Err(LaunchError::Provenance)
        );
    }
}
