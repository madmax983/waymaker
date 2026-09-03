//! Design document §15's core property oracle, as a function.
//!
//! ```text
//! // For every possible crash point:
//! recovered_history.is_prefix_of(committed_history)
//!     && acknowledged_records.all(|r| recovered_history.contains(r))
//!     && dispatched_effects.all(|e| recovered_history.has_schedule(e.id))
//!     && recovered_banks.count_authoritative() == 1
//! ```
//!
//! All four lines are [`verify_oracle`], plus a fifth the design document leaves implicit:
//! recovery must not produce a record that never reached media at all, whole or in part.
//!
//! # Why the last two lines are optional rather than always checked
//!
//! Because a caller that has no banks and dispatched nothing would otherwise have to invent
//! answers for them, and an invented `1` is a check that passes by construction. A
//! [`Recovery`] carries what its caller actually observed: [`verify_recovery`] is the
//! two-line form for a journal with neither, and each extra dimension is a builder call
//! that a caller only writes when it has something to put there. Issue
//! [#19](https://github.com/madmax983/waymaker/issues/19) is what asks for all four; rung
//! 0.1's journal exercises the first three and `tests/banks.rs` the fourth.
//!
//! # Why it fails closed
//!
//! Every way this function could pass vacuously is a breach instead. An empty recovery
//! against a ledger with an acknowledged record in it is a lost acknowledgment, not a
//! trivial prefix. A recovery longer than the ledger is a prefix violation rather than an
//! ignored tail. A ledger that names one record twice is refused outright, because a
//! `contains` over it would answer questions about the wrong record and answer them
//! cheerfully.

use core::fmt;
use std::collections::BTreeSet;

use crate::model::{Durability, Ledger, RecordId};

/// A way a recovery can be illegal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Breach {
    /// The run declared one record id twice, so no question about it has one answer.
    DuplicateRecordId {
        /// The id that appeared more than once.
        record: RecordId,
    },
    /// Recovery produced something other than a prefix of the committed history.
    NotAPrefix {
        /// Where the two disagreed.
        position: usize,
        /// What the committed history has there, if it reaches that far.
        expected: Option<RecordId>,
        /// What recovery produced there.
        found: RecordId,
    },
    /// Recovery produced a record none of whose bytes ever reached media.
    RecoveredWhatWasNeverAttempted {
        /// The record recovery invented.
        record: RecordId,
    },
    /// Recovery produced a record only part of which reached media.
    ///
    /// Design document §15 permits recovery to include "an unacknowledged **complete**
    /// record". Half a record is not one, and no integrity check should have accepted it.
    RecoveredATornRecord {
        /// The record recovery half-read.
        record: RecordId,
    },
    /// Recovery lost a record whose barrier had returned.
    LostAnAcknowledgedRecord {
        /// The record that was acknowledged and is not there.
        record: RecordId,
    },
    /// An effect happened whose durable intent is not in the recovered history.
    ///
    /// Design document §02 decision 3: "the schedule record crosses a durability barrier
    /// before dispatch. A physical effect never precedes its committed intent." A physical
    /// effect cannot be un-done by a reboot, so a recovery that cannot account for one has
    /// lost the only record that says it happened.
    DispatchedWithoutADurableIntent {
        /// The schedule record the dispatched effect needed and recovery did not find.
        intent: RecordId,
    },
    /// No bank is authoritative, so there is nothing to boot from.
    ///
    /// Design document §02 decision 7. Reported for a device that has committed at least
    /// once: a device that never has is not a device that lost its authority.
    NoAuthoritativeBank,
    /// More than one bank is authoritative, so which history is the run's is not decided.
    AmbiguousAuthority {
        /// How many banks a reader would have booted from.
        count: usize,
    },
}

impl fmt::Display for Breach {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRecordId { record } => {
                write!(formatter, "record {} was declared twice", record.0)
            }
            Self::NotAPrefix {
                position,
                expected,
                found,
            } => match expected {
                Some(expected) => write!(
                    formatter,
                    "recovery has record {} at position {position}, where history has record {}",
                    found.0, expected.0
                ),
                None => write!(
                    formatter,
                    "recovery has record {} at position {position}, past the end of history",
                    found.0
                ),
            },
            Self::RecoveredWhatWasNeverAttempted { record } => write!(
                formatter,
                "recovery produced record {}, none of which ever reached media",
                record.0
            ),
            Self::RecoveredATornRecord { record } => write!(
                formatter,
                "recovery produced record {}, only part of which reached media",
                record.0
            ),
            Self::LostAnAcknowledgedRecord { record } => write!(
                formatter,
                "recovery lost record {}, whose barrier had returned",
                record.0
            ),
            Self::DispatchedWithoutADurableIntent { intent } => write!(
                formatter,
                "an effect was dispatched and recovery has no record {} to account for it",
                intent.0
            ),
            Self::NoAuthoritativeBank => {
                formatter.write_str("no bank is authoritative, so there is nothing to boot from")
            }
            Self::AmbiguousAuthority { count } => write!(
                formatter,
                "{count} banks are authoritative, so which history is the run's is undecided"
            ),
        }
    }
}

impl core::error::Error for Breach {}

/// What one recovery produced, in the dimensions §15's oracle asks about.
///
/// The history is required; the other two are what a caller *observed* rather than what it
/// believes, and each is absent until it says otherwise. That asymmetry is the point: an
/// oracle handed a default of "one authoritative bank" would report a passing fourth line
/// for a journal that has no banks at all, which is a check that cannot fail.
///
/// # Building one
///
/// ```
/// use waymaker_fault::{Recovery, RecordId};
///
/// let history = [RecordId(0), RecordId(1)];
/// let dispatched = [RecordId(1)];
/// let recovery = Recovery::new(&history)
///     .dispatched(&dispatched)
///     .authoritative_banks(1);
///
/// assert_eq!(recovery.history(), &history);
/// assert_eq!(recovery.authoritative_banks_seen(), Some(1));
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Recovery<'a> {
    history: &'a [RecordId],
    dispatched: &'a [RecordId],
    banks: Option<usize>,
}

impl<'a> Recovery<'a> {
    /// A recovery that produced `history` and nothing else is claimed about.
    #[must_use]
    pub const fn new(history: &'a [RecordId]) -> Self {
        Self {
            history,
            dispatched: &[],
            banks: None,
        }
    }

    /// The schedule records of the effects this run really dispatched.
    ///
    /// A physical effect, not an intention to perform one: §02 decision 3 orders the
    /// schedule record before the effect, so an id belongs here once the barrier that made
    /// its intent durable has returned and the effect has been handed to the world.
    #[must_use]
    pub const fn dispatched(mut self, dispatched: &'a [RecordId]) -> Self {
        self.dispatched = dispatched;
        self
    }

    /// How many banks a reader would have booted from.
    ///
    /// Only for a device that has committed at least once. A device on which nothing has
    /// ever been sealed has no authority to have lost, and calling it a breach would fail
    /// every first boot.
    #[must_use]
    pub const fn authoritative_banks(mut self, count: usize) -> Self {
        self.banks = Some(count);
        self
    }

    /// The history recovery produced.
    #[must_use]
    pub const fn history(&self) -> &'a [RecordId] {
        self.history
    }

    /// The schedule records of the effects that were dispatched.
    #[must_use]
    pub const fn dispatched_effects(&self) -> &'a [RecordId] {
        self.dispatched
    }

    /// How many banks were authoritative, or [`None`] if the caller has no banks.
    #[must_use]
    pub const fn authoritative_banks_seen(&self) -> Option<usize> {
        self.banks
    }
}

/// Whether `recovered` is a legal recovery of the run `ledger` describes.
///
/// The two-line form: [`verify_oracle`] with a [`Recovery`] that claims nothing about banks
/// or dispatched effects. This is what a plain journal is held to, and it is what rung
/// 0.1's sweeps call.
///
/// # Errors
///
/// As [`verify_oracle`].
pub fn verify_recovery(ledger: &Ledger, recovered: &[RecordId]) -> Result<(), Breach> {
    verify_oracle(ledger, &Recovery::new(recovered))
}

/// Whether `recovery` is a legal recovery of the run `ledger` describes.
///
/// Design document §15's core property oracle, all four lines of it, and the implicit
/// fifth: recovery must not produce a record that never reached media, nor one only half
/// of which did.
///
/// # Errors
///
/// One [`Breach`] — the first the checks below find, in the order they are written, so that
/// the most specific diagnosis is the one reported. A recovery that produced a record which
/// never reached media is described as that rather than as a prefix that disagrees at
/// position one, and a recovery that is not a prefix is described as that rather than as
/// four missing records.
///
/// The bank check is last on purpose. "Two banks are authoritative" is a true statement
/// about a device whose recovered history is also wrong, and the history is the more
/// specific diagnosis; a caller reading the failure wants to know what was lost before it
/// is told which bank the loss came out of.
pub fn verify_oracle(ledger: &Ledger, recovery: &Recovery<'_>) -> Result<(), Breach> {
    let mut seen = BTreeSet::new();
    for id in ledger.order() {
        if !seen.insert(id) {
            return Err(Breach::DuplicateRecordId { record: id });
        }
    }

    let recovered = recovery.history();

    // Before the prefix check, because "recovery produced a record that never reached
    // media" is a diagnosis and "position 1 disagrees" is a symptom of it.
    let recovered_set: BTreeSet<RecordId> = recovered.iter().copied().collect();
    for id in &recovered_set {
        if ledger.state(*id) == Some(Durability::Attempted) {
            return Err(Breach::RecoveredWhatWasNeverAttempted { record: *id });
        }
        if ledger.torn(*id) == Some(true) {
            return Err(Breach::RecoveredATornRecord { record: *id });
        }
    }

    let committed: Vec<RecordId> = ledger.committed().collect();
    for (position, found) in recovered.iter().enumerate() {
        let expected = committed.get(position).copied();
        if expected != Some(*found) {
            return Err(Breach::NotAPrefix {
                position,
                expected,
                found: *found,
            });
        }
    }

    for id in ledger.acknowledged() {
        if !recovered_set.contains(&id) {
            return Err(Breach::LostAnAcknowledgedRecord { record: id });
        }
    }

    for intent in recovery.dispatched_effects() {
        if !recovered_set.contains(intent) {
            return Err(Breach::DispatchedWithoutADurableIntent { intent: *intent });
        }
    }

    match recovery.authoritative_banks_seen() {
        None | Some(1) => Ok(()),
        Some(0) => Err(Breach::NoAuthoritativeBank),
        Some(count) => Err(Breach::AmbiguousAuthority { count }),
    }
}
