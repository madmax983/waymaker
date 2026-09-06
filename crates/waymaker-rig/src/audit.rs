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
    /// The caller's scratch buffer is shorter than the record it was asked to rebuild.
    ///
    /// Not a §14 violation, and that is the whole reason it exists.
    /// [`Workload::record`](crate::workload::Workload::record) answers `None` for two
    /// unrelated reasons — past the end of the run, and a buffer too short for the payload —
    /// and folding the second into [`RecoveredPastWhatWasAttempted`](Self::RecoveredPastWhatWasAttempted)
    /// makes a rig report a recovery violation that did not happen, which is the one failure
    /// direction an instrument must not have.
    ShortScratch,
    /// The witness says nothing about this iteration.
    ///
    /// It could not be read, or — the case that costs a board a night's run — it is a
    /// *previous* iteration's, still on media because the reset landed before the instrument
    /// was erased. Auditing run `N` against run `N - 1`'s marks is unsound in both
    /// directions: it invents a breach on a healthy device, and it excuses a real loss
    /// whenever the stale marks happen to claim less than this run did.
    ///
    /// Fails closed either way. An iteration whose instrument said nothing is an iteration
    /// that proved nothing, and reporting it as a pass would be the one bug a rig must not
    /// have.
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
            Self::ShortScratch => 8,
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
            Self::ShortScratch => "short-scratch",
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
            8 => Some(Self::ShortScratch.name()),
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
    /// [`Breach::WitnessUnreadable`] when the witness is a different iteration's,
    /// [`Breach::RecordDiffers`] when the record is not the one declared here, and
    /// [`Breach::RecoveredPastWhatWasAttempted`] when recovery has run past the end of the
    /// run or past what the writer ever began.
    pub fn saw(&mut self, record: &RecordRef<'_>, scratch: &mut [u8]) -> Result<(), Breach> {
        self.witness_is_this_run()?;
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
        // The run's own length, asked first, so that the two reasons `record` can answer
        // `None` stay apart: past the end of the run is a §14 breach, and a scratch buffer
        // too short is the caller's mistake.
        if self.workload.role(index).is_none() {
            return Err(Breach::RecoveredPastWhatWasAttempted {
                recovered: index.saturating_add(1),
                attempted: attempted.unwrap_or(0),
            });
        }
        let Some(expected) = self.workload.record(index, scratch) else {
            return Err(Breach::ShortScratch);
        };
        if expected != *record {
            return Err(Breach::RecordDiffers { index });
        }
        self.recovered = self.recovered.saturating_add(1);
        Ok(())
    }

    /// Whether the witness in hand is the one this run wrote.
    ///
    /// A [`Mark`](crate::witness::Mark) carries the iteration that wrote it, and
    /// [`Progress::iteration`] carries it out of a scan. Comparing the two is the whole of
    /// this check, and without it the field would be a comment: a rig's loop is
    /// `prepare(n)` → `iterate(n)` → reset → `verify(n)`, and a reset landing anywhere before
    /// `prepare` erased the instrument leaves iteration `n - 1`'s marks in front of run `n`'s
    /// declarations.
    const fn witness_is_this_run(&self) -> Result<(), Breach> {
        match self.progress.iteration() {
            // An empty witness names no iteration and demands nothing, which is the state a
            // part is in before the first mark of a run lands.
            None => Ok(()),
            Some(iteration) if iteration == self.workload.iteration() => Ok(()),
            Some(_) => Err(Breach::WitnessUnreadable),
        }
    }

    /// The three guarantees that are statements about the whole recovery, and the one that
    /// is about the instrument.
    ///
    /// # Errors
    ///
    /// [`Breach::WitnessUnreadable`], [`Breach::LostAcknowledgedRecord`],
    /// [`Breach::DispatchedEffectWithoutSchedule`], [`Breach::DispatchMarkIsNotASchedule`]
    /// and [`Breach::Authority`].
    pub const fn finish(self, authoritative_banks: usize) -> Result<(), Breach> {
        // Checked here as well as in `saw`, because a recovery that produced no records at
        // all never reaches `saw` — and "no records recovered" against a stale witness that
        // claims six acknowledged is exactly the shape this must not report as a loss.
        if let Err(breach) = self.witness_is_this_run() {
            return Err(breach);
        }
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
        // The witness is what tells "never installed" apart from "lost its authority", and
        // the implication that does the work runs the other way from the obvious one. What is
        // needed is that an *empty* witness excuses a missing authority, and that holds
        // because `Rig::prepare` erases the instrument *before* it erases and installs the
        // engine: the only window in which authority is gone is one in which the witness is
        // already empty. `Rig::iterate`'s own authority pre-check is the converse and is not
        // what this rests on. Two authorities is a breach either way — no crash makes a
        // second one out of nothing.
        // `torn` as well as `marks`: a witness whose *first* mark was interrupted has no
        // whole marks and yet the device certainly began. Safe today either way — no engine
        // write precedes the first mark — but the tighter reading costs nothing and does not
        // depend on that ordering staying true.
        let began = self.progress.marks() > 0 || self.progress.torn();
        if authoritative_banks > 1 || (began && authoritative_banks != 1) {
            return Err(Breach::Authority {
                banks: authoritative_banks,
            });
        }
        Ok(())
    }
}
