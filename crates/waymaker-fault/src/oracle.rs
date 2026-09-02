//! Design document §15's core property oracle, as a function.
//!
//! ```text
//! // For every possible crash point:
//! recovered_history.is_prefix_of(committed_history)
//!     && acknowledged_records.all(|r| recovered_history.contains(r))
//! ```
//!
//! The two lines the design document writes about banks and dispatched effects belong to
//! rungs 0.2 and 0.3; the two above are what a rung-0.1 journal can be held to, and they
//! are what [`verify_recovery`] checks — plus a third the design document leaves implicit:
//! recovery must not produce a record that never reached media at all.
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
    /// Recovery lost a record whose barrier had returned.
    LostAnAcknowledgedRecord {
        /// The record that was acknowledged and is not there.
        record: RecordId,
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
            Self::LostAnAcknowledgedRecord { record } => write!(
                formatter,
                "recovery lost record {}, whose barrier had returned",
                record.0
            ),
        }
    }
}

impl core::error::Error for Breach {}

/// Whether `recovered` is a legal recovery of the run `ledger` describes.
///
/// # Errors
///
/// One [`Breach`] — the first the checks below find, in the order they are written, so that
/// the most structural failure is the one reported. A recovery that is not a prefix is
/// described as that rather than as four missing records.
pub fn verify_recovery(ledger: &Ledger, recovered: &[RecordId]) -> Result<(), Breach> {
    let committed = ledger.order();

    let mut seen = BTreeSet::new();
    for id in &committed {
        if !seen.insert(*id) {
            return Err(Breach::DuplicateRecordId { record: *id });
        }
    }

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

    let recovered_set: BTreeSet<RecordId> = recovered.iter().copied().collect();
    for id in &recovered_set {
        if ledger.state(*id) == Some(Durability::Attempted) {
            return Err(Breach::RecoveredWhatWasNeverAttempted { record: *id });
        }
    }

    for id in ledger.acknowledged() {
        if !recovered_set.contains(&id) {
            return Err(Breach::LostAnAcknowledgedRecord { record: id });
        }
    }

    Ok(())
}
