//! The rig's oracle.
//!
//! Design document §15 states four lines a recovery must satisfy, and `waymaker-fault`
//! already checks them — against a `Ledger` it kept in RAM while it
//! simulated the crash. This is the same four lines checked against a [`Progress`] read back
//! off media after a *real* reset, which is the only form of them a board can evaluate.
//!
//! # The four, and where each one's evidence comes from
//!
//! | Guarantee | What it needs | Where that comes from |
//! | --- | --- | --- |
//! | `prefix-safety` | the run's declared record order | [`Workload`], derived from the seed |
//! | `acknowledged-durability` | which barriers returned | the witness's `Acknowledged` marks |
//! | `durable-intent` | which effects were dispatched | the witness's `Dispatched` marks |
//! | `single-authority` | how many banks are authoritative | the caller's bank selection |
//!
//! # Why it streams
//!
//! An [`Audit`] is fed one recovered record at a time and holds no history. A rig cannot
//! collect a run's records into a `Vec` and compare afterwards, because there is no `Vec` —
//! and because §04's runtime budget is 768 B with a 512 B page in it. The same reason
//! [`waymaker_flash::recovery::Recovery`] is a position rather than a reader.
//!
//! # Why a breach is reported where it happens
//!
//! [`Audit::saw`] returns the breach on the record that broke the guarantee rather than at
//! the end of the scan. A rig that carried on after seeing recovery diverge would be writing
//! records into a journal it has already proved it cannot trust.

use waymaker_core::RecordRef;

use crate::witness::Progress;
use crate::workload::{Role, Workload};

/// A §14 guarantee the rig watched fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Breach {
    /// Recovery handed back a record that is not the one the run declared at that position.
    RecordDiffers {
        /// Where in the run's declaration order.
        index: u16,
    },
    /// A record whose commit barrier returned was not recovered.
    LostAcknowledgedRecord {
        /// The record the witness says was acknowledged.
        index: u16,
    },
    /// Recovery handed back more records than the writer ever began.
    RecoveredPastWhatWasAttempted {
        /// How many records recovery produced.
        recovered: u16,
        /// The highest index the witness saw begun.
        attempted: u16,
    },
    /// An effect was dispatched and its schedule record was not recovered.
    DispatchedEffectWithoutSchedule {
        /// The schedule record the witness says was dispatched against.
        index: u16,
    },
    /// A dispatch mark names a record that is not a schedule. The instrument is broken.
    DispatchMarkIsNotASchedule {
        /// The record the mark named.
        index: u16,
    },
    /// Other than exactly one bank was authoritative.
    Authority {
        /// How many were.
        banks: usize,
    },
    /// The witness itself could not be read, so nothing about this iteration is known.
    ///
    /// Fails closed. An iteration whose instrument broke is an iteration that proved nothing,
    /// and reporting it as a pass would be the one bug a rig must not have.
    WitnessUnreadable,
}

impl Breach {
    /// The code a log line carries. Never zero — zero is "no breach".
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::RecordDiffers { .. } => 1,
            Self::LostAcknowledgedRecord { .. } => 2,
            Self::RecoveredPastWhatWasAttempted { .. } => 3,
            Self::DispatchedEffectWithoutSchedule { .. } => 4,
            Self::DispatchMarkIsNotASchedule { .. } => 5,
            Self::Authority { .. } => 6,
            Self::WitnessUnreadable => 7,
        }
    }

    /// A short static name, for a log a device with no allocator writes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RecordDiffers { .. } => "record-differs",
            Self::LostAcknowledgedRecord { .. } => "lost-acknowledged-record",
            Self::RecoveredPastWhatWasAttempted { .. } => "recovered-past-attempted",
            Self::DispatchedEffectWithoutSchedule { .. } => "dispatched-without-schedule",
            Self::DispatchMarkIsNotASchedule { .. } => "dispatch-mark-is-not-a-schedule",
            Self::Authority { .. } => "authority",
            Self::WitnessUnreadable => "witness-unreadable",
        }
    }

    /// The name of the breach `code` names, or `None` for zero and for anything unknown.
    ///
    /// A log line carries the code; a reader that could not turn it back into the name would
    /// make a violation found on a board a different violation when it is read on a host.
    #[must_use]
    pub const fn name_of_code(code: u8) -> Option<&'static str> {
        match code {
            1 => Some(Self::RecordDiffers { index: 0 }.name()),
            2 => Some(Self::LostAcknowledgedRecord { index: 0 }.name()),
            3 => Some(
                Self::RecoveredPastWhatWasAttempted {
                    recovered: 0,
                    attempted: 0,
                }
                .name(),
            ),
            4 => Some(Self::DispatchedEffectWithoutSchedule { index: 0 }.name()),
            5 => Some(Self::DispatchMarkIsNotASchedule { index: 0 }.name()),
            6 => Some(Self::Authority { banks: 0 }.name()),
            7 => Some(Self::WitnessUnreadable.name()),
            _ => None,
        }
    }
}

impl core::fmt::Display for Breach {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

impl core::error::Error for Breach {}

/// A recovery being checked against what the rig durably knew.
///
/// # Invariants
///
/// Records are fed in recovery order and compared against the run's declaration order at the
/// same position, so [`saw`](Self::saw) is `prefix-safety` and nothing else; the other three
/// guarantees are [`finish`](Self::finish)'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Audit {
    workload: Workload,
    progress: Progress,
    recovered: u16,
}

impl Audit {
    /// An audit of `workload` against what `progress` says the rig durably knew.
    #[must_use]
    pub const fn new(workload: Workload, progress: Progress) -> Self {
        Self {
            workload,
            progress,
            recovered: 0,
        }
    }

    /// How many records have been fed in.
    #[must_use]
    pub const fn recovered(&self) -> u16 {
        self.recovered
    }

    /// What the rig durably knew.
    #[must_use]
    pub const fn progress(&self) -> Progress {
        self.progress
    }

    /// Feeds in the next record recovery produced.
    ///
    /// `scratch` holds the payload of the record the run declared at this position, for the
    /// duration of the comparison.
    ///
    /// # Errors
    ///
    /// [`Breach::RecordDiffers`] when the record is not the one declared here, and
    /// [`Breach::RecoveredPastWhatWasAttempted`] when recovery has run past the end of the
    /// run or past what the writer ever began.
    pub fn saw(&mut self, record: &RecordRef<'_>, scratch: &mut [u8]) -> Result<(), Breach> {
        let index = self.recovered;
        let attempted = self.progress.attempted();
        // A record at position `index` was begun only if the witness saw index `index` or
        // later begun. `None` means the writer never began anything.
        match attempted {
            Some(high) if index <= high => {}
            _ => {
                return Err(Breach::RecoveredPastWhatWasAttempted {
                    recovered: index.saturating_add(1),
                    attempted: attempted.unwrap_or(0),
                });
            }
        }
        let Some(expected) = self.workload.record(index, scratch) else {
            return Err(Breach::RecoveredPastWhatWasAttempted {
                recovered: index.saturating_add(1),
                attempted: attempted.unwrap_or(0),
            });
        };
        if expected != *record {
            return Err(Breach::RecordDiffers { index });
        }
        self.recovered = self.recovered.saturating_add(1);
        Ok(())
    }

    /// The three guarantees that are statements about the whole recovery.
    ///
    /// # Errors
    ///
    /// [`Breach::LostAcknowledgedRecord`], [`Breach::DispatchedEffectWithoutSchedule`],
    /// [`Breach::DispatchMarkIsNotASchedule`] and [`Breach::Authority`].
    pub const fn finish(self, authoritative_banks: usize) -> Result<(), Breach> {
        if let Some(acknowledged) = self.progress.acknowledged() {
            // Record `acknowledged` had its barrier return, so recovery owes every record up
            // to and including it: `acknowledged + 1` records.
            if self.recovered <= acknowledged {
                return Err(Breach::LostAcknowledgedRecord {
                    index: acknowledged,
                });
            }
        }
        if let Some(dispatched) = self.progress.dispatched() {
            match self.workload.role(dispatched) {
                Some(Role::Schedule(_)) => {}
                _ => return Err(Breach::DispatchMarkIsNotASchedule { index: dispatched }),
            }
            if self.recovered <= dispatched {
                return Err(Breach::DispatchedEffectWithoutSchedule { index: dispatched });
            }
        }
        // §14's `single-authority` is a statement about a device that *has* an authority.
        // A part whose preparation was interrupted before its first generation seal landed
        // has none, and reporting that as a violation would fail every run whose crash point
        // fell in the install — which is a third of them.
        //
        // The witness is what tells "never installed" apart from "lost its authority":
        // `Rig::iterate` selects an authoritative bank and recovers its journal *before* it
        // writes its first mark, so a witness with a mark in it is a device that had exactly
        // one authority at the moment that mark was written. Two authorities is a breach
        // either way — no crash can produce a second one out of nothing.
        let began = self.progress.marks() > 0;
        if authoritative_banks > 1 || (began && authoritative_banks != 1) {
            return Err(Breach::Authority {
                banks: authoritative_banks,
            });
        }
        Ok(())
    }
}
