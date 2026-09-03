//! The bridge from the ghost model to the firmware that has to obey it.
//!
//! A model nothing is compared against is a second implementation with no tests. This module
//! is the abstraction function α: it turns one run of a real writer, crashed at one real
//! point by [`waymaker_fault`]'s injector, into the part of a ghost state a harness can
//! actually observe. `tests/refinement.rs` then asks the three questions that make the model
//! load-bearing:
//!
//! 1. Is α(run) a state the model says is reachable? A crash the firmware can be in and the
//!    model cannot describe is a model that is wrong about the firmware.
//! 2. Does the real reader — `waymaker_flash`'s `Scan` over the media the crash left —
//!    produce exactly what [`crate::reader::Specified`] produces from α(run)? A reader that
//!    is right about media and wrong about the specification is the failure a model-only
//!    proof cannot see.
//! 3. Does [`waymaker_fault::verify_recovery`] accept it? The oracle and the model are two
//!    independent judgements of the same run, and they have to agree.
//!
//! # What is deliberately not abstracted
//!
//! Banks. Rung 0.1 has no two-bank adapter to drive, so [`Observation`] carries records and
//! dispatched effects and nothing else, and the fourth guarantee is discharged against the
//! model alone. That is a gap, it is owed at rung 0.2 where the banks arrive, and
//! [`crate::obligation`] says so in a table rather than leaving it to be noticed.

use waymaker_fault::{Durability, Ledger, RecordId};

use crate::model::{Journal, OnMedia, Record};

/// The part of a ghost state a crash harness can report.
///
/// Not the whole state: [`Journal::powered`] is a fact about the run rather than about the
/// media, and the banks are rung 0.2's. Comparing observations rather than states is what
/// lets a real run be matched against the model without inventing the dimensions the harness
/// has no answer for — the same reason [`waymaker_fault::Recovery`] makes its extra
/// dimensions optional instead of defaulting them.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Observation {
    /// Each record in declaration order, as `(id, state, torn)`.
    pub records: Vec<(RecordId, Durability, bool)>,
    /// The schedule records of effects the run really handed to the world.
    pub dispatched: Vec<RecordId>,
}

impl Journal {
    /// The part of this state a crash harness could report.
    #[must_use]
    pub fn observation(&self) -> Observation {
        Observation {
            records: self
                .records()
                .iter()
                .map(|record| {
                    (
                        record.id,
                        record.durability(),
                        record.media == OnMedia::Partial,
                    )
                })
                .collect(),
            dispatched: self.dispatched().to_vec(),
        }
    }

    /// A state carrying `observation` and nothing else, for asking the model what a real run
    /// should have recovered.
    ///
    /// Not a way into the state space: the power is off, both banks are erased, and nothing
    /// here checks that the result is reachable. `tests/refinement.rs` does that separately,
    /// against [`crate::explore`](mod@crate::explore)'s closed set, and it is the only reason building a state
    /// outside [`Journal::step`] is legitimate at all.
    ///
    /// # The bank dimension is absent, and that makes one guarantee vacuous
    ///
    /// A reconstructed state has both banks erased and has never sealed, so
    /// [`crate::invariant::Invariant::SingleAuthority`] holds of it *by construction* —
    /// [`crate::invariant::check`] judges three guarantees here and reports the fourth as
    /// satisfied without looking at anything. `tests/refinement.rs` asserts that in so many
    /// words rather than leaving rung 0.2's bank adapter to discover it, and
    /// [`crate::obligation`]'s `single-authority` row says the same thing in the place a
    /// reader looks for what is owed.
    ///
    /// # Errors
    ///
    /// [`Impossible`] when the observation describes a record no media could hold. Refused
    /// rather than normalised: a state builder that quietly repaired its input would answer
    /// questions about a record the caller did not describe, and answer them cheerfully.
    pub fn reconstructed(observation: &Observation) -> Result<Self, Impossible> {
        let mut records = Vec::with_capacity(observation.records.len());
        for (id, state, torn) in &observation.records {
            let media = match (state, torn) {
                (Durability::Attempted, false) => OnMedia::Absent,
                (Durability::Attempted, true) => {
                    return Err(Impossible::TornAndAbsent { record: *id });
                }
                (Durability::PossiblyDurable, true) => OnMedia::Partial,
                (Durability::PossiblyDurable | Durability::Acknowledged, false) => OnMedia::Whole,
                (Durability::Acknowledged, true) => {
                    return Err(Impossible::TornAndAcknowledged { record: *id });
                }
            };
            records.push(Record {
                id: *id,
                media,
                acknowledged: *state == Durability::Acknowledged,
            });
        }
        Ok(Self::from_parts(records, observation.dispatched.clone()))
    }
}

/// An observation no run could have produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Impossible {
    /// A record claims a barrier returned for bytes only half of which are on media.
    TornAndAcknowledged {
        /// The record that claimed both.
        record: RecordId,
    },
    /// A record claims to be half on media and to have reached it not at all.
    TornAndAbsent {
        /// The record that claimed both.
        record: RecordId,
    },
}

impl core::fmt::Display for Impossible {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TornAndAcknowledged { record } => write!(
                formatter,
                "record {} is torn and acknowledged, and a barrier cannot return for bytes                  that were never written",
                record.0
            ),
            Self::TornAndAbsent { record } => write!(
                formatter,
                "record {} is torn and never reached media, and half of it cannot be both",
                record.0
            ),
        }
    }
}

impl core::error::Error for Impossible {}

/// α: one crashed run, as an observation of a ghost state.
///
/// `dispatched` is what the caller *saw the writer do* — an effect that reached the world —
/// rather than what media says about it, for the same reason
/// [`waymaker_fault::Recovery::dispatched`] is: an oracle that only admitted an effect once
/// its intent was durable could not describe the violation it exists to catch.
#[must_use]
pub fn abstraction(ledger: &Ledger, dispatched: &[RecordId]) -> Observation {
    let mut sorted = dispatched.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    Observation {
        records: ledger
            .records()
            .map(|(id, state)| (id, state, ledger.torn(id).unwrap_or(false)))
            .collect(),
        dispatched: sorted,
    }
}
