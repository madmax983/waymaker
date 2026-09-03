//! What a reader produces from a ghost state, and the wrong readers the proofs must reject.
//!
//! Design document §14's first two guarantees are statements about *a reader*, not about
//! media: "recovery exposes only a legal prefix of committed records", "any record
//! acknowledged after its barrier is recovered after reset". A specification that hard-wired
//! the answer would make both of them true by construction and prove nothing, so the model
//! quantifies over readers instead: [`Specified`] is the one the design document describes,
//! and [`Mutant`] is a small catalogue of readers that are wrong in one way each.
//!
//! `tests/teeth.rs` requires every mutant to be caught, and to be caught by the guarantee it
//! breaks rather than by any guarantee at all. A proof that cannot fail is not a proof, and
//! the way to find that out is to break it on purpose.

use waymaker_fault::RecordId;

use crate::model::{Journal, OnMedia};

/// Something that turns a ghost state into the history a reboot would see.
pub trait Reader {
    /// The records this reader would produce after a reset in `journal`'s state.
    fn recover(&self, journal: &Journal) -> Vec<RecordId>;
}

/// The reader design document §06 describes: walk the journal from the start, stop at the
/// first record that is not wholly on media.
///
/// Everything after a gap is unreachable whether or not its bytes are there, which is why
/// the stopping rule is the specification of recovery rather than an optimisation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specified;

impl Reader for Specified {
    fn recover(&self, journal: &Journal) -> Vec<RecordId> {
        journal.recover()
    }
}

/// A reader that is wrong in exactly one way.
///
/// Each variant exists so that one guarantee can be shown to fail. They are in `src/` rather
/// than in a test file because three test targets need them — `tests/teeth.rs`,
/// `tests/refinement.rs` and `tests/diagnostics.rs` — and a mutant copied into three test
/// files is a mutant that drifts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mutant {
    /// Produces one record past the end of history. Breaks prefix safety.
    ProducesOneMore,
    /// Produces a torn record as though it were whole. Breaks prefix safety: §15 permits
    /// recovery to include "an unacknowledged **complete** record", and half a record is not
    /// one.
    IncludesTorn,
    /// Produces the specified history with its last two records swapped. Breaks prefix
    /// safety without producing anything that is not on media, which is the failure a
    /// membership check rather than an order check would miss.
    Reorders,
    /// Stops one record early. Breaks acknowledged durability, and durable intent with it
    /// when the dropped record is a dispatched effect's schedule.
    DropsTheLast,
    /// Skips a record that is not wholly on media and carries on past it. Breaks prefix
    /// safety by exposing history behind a gap — the failure mode a reader that treated an
    /// erased header as "keep looking" would have.
    SkipsGaps,
}

impl Mutant {
    /// Every mutant, in a fixed order.
    pub const ALL: [Self; 5] = [
        Self::ProducesOneMore,
        Self::IncludesTorn,
        Self::Reorders,
        Self::DropsTheLast,
        Self::SkipsGaps,
    ];

    /// One line naming the way this reader is wrong.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ProducesOneMore => "produces one record past the end of history",
            Self::IncludesTorn => "produces a torn record as though it were whole",
            Self::Reorders => "produces the last two records in the wrong order",
            Self::DropsTheLast => "stops one record early",
            Self::SkipsGaps => "carries on past a record that is not wholly on media",
        }
    }
}

impl core::fmt::Display for Mutant {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Reader for Mutant {
    fn recover(&self, journal: &Journal) -> Vec<RecordId> {
        let mut history = Specified.recover(journal);
        match self {
            Self::ProducesOneMore => {
                let next = journal.records().get(history.len()).map_or_else(
                    || RecordId(u32::try_from(journal.records().len()).unwrap_or(u32::MAX)),
                    |record| record.id,
                );
                history.push(next);
            }
            Self::IncludesTorn => {
                if let Some(torn) = journal
                    .records()
                    .iter()
                    .find(|record| record.media == OnMedia::Partial)
                {
                    history.push(torn.id);
                }
            }
            Self::Reorders => {
                let length = history.len();
                if length >= 2 {
                    history.swap(length - 1, length - 2);
                }
            }
            Self::DropsTheLast => {
                history.pop();
            }
            Self::SkipsGaps => {
                history = journal
                    .records()
                    .iter()
                    .filter(|record| record.is_recoverable())
                    .map(|record| record.id)
                    .collect();
            }
        }
        history
    }
}
