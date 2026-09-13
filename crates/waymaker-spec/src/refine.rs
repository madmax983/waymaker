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
//! # Banks
//!
//! Issue [#22](https://github.com/madmax983/waymaker/issues/22) added the real two-bank
//! adapter, `waymaker_flash::bank`, and issue
//! [#73](https://github.com/madmax983/waymaker/issues/73) is this module abstracting it.
//! [`bank_after_erase`] and [`bank_after_seal`] fold one crashed run's write sequence into a
//! [`Bank`], and a caller with no bank to report — a record-only writer — passes
//! `[Bank::Erased; BANKS]` and `false` into [`Journal::reconstructed`], matching this crate's
//! behaviour before this issue.

use waymaker_fault::{Durability, Interruption, Ledger, Op, Progress, RecordId, Run};
use waymaker_flash::storage::Geometry;

use crate::model::{BANKS, Bank, Journal, OnMedia, Record, Role};

/// The part of a ghost state a crash harness can report.
///
/// Not the whole state: [`Journal::powered`] is a fact about the run rather than about the
/// media. Comparing observations rather than states is what lets a real run be matched
/// against the model without inventing the dimensions the harness has no answer for — the
/// same reason [`waymaker_fault::Recovery`] makes its extra dimensions optional instead of
/// defaulting them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Observation {
    /// Each record in declaration order, as `(id, role, state, torn)`.
    ///
    /// The role comes from the caller rather than from the ledger: `waymaker-fault` names no
    /// record type, which is exactly what makes the harness reusable, so what a record is
    /// *for* is something only the writer under test knows.
    pub records: Vec<(RecordId, Role, Durability, bool)>,
    /// The schedule records of effects the run really handed to the world.
    pub dispatched: Vec<RecordId>,
    /// Both banks, read off the crashed run.
    ///
    /// `[Bank::Erased; BANKS]` for a writer that never touches a bank — see the module docs.
    pub banks: [Bank; BANKS],
    /// Whether either bank has *ever* carried a durable seal, over this run's whole history.
    ///
    /// `false` for a writer that never touches a bank.
    pub sealed_once: bool,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            dispatched: Vec::new(),
            banks: [Bank::Erased; BANKS],
            sealed_once: false,
        }
    }
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
                        record.role,
                        record.durability(),
                        record.media == OnMedia::Partial,
                    )
                })
                .collect(),
            dispatched: self.dispatched().to_vec(),
            banks: *self.banks(),
            sealed_once: self.has_sealed(),
        }
    }

    /// A state carrying `observation` and nothing else, for asking the model what a real run
    /// should have recovered.
    ///
    /// Not a way into the state space: the power is off, and nothing here checks that the
    /// result is reachable. `tests/refinement.rs` does that separately, against
    /// [`crate::explore`](mod@crate::explore)'s closed set, and it is the only reason building
    /// a state outside [`Journal::step`] is legitimate at all.
    ///
    /// # A caller with no bank to report answers the fourth guarantee vacuously
    ///
    /// `observation.banks` is `[Bank::Erased; BANKS]` for a record-only writer, so
    /// [`crate::invariant::Invariant::SingleAuthority`] holds of the result *by construction*
    /// — [`crate::invariant::check`] judges three guarantees over such a state and reports the
    /// fourth as satisfied without looking at anything. A caller that read real banks off a
    /// crashed device — see [`bank_after_erase`] and [`bank_after_seal`] — does not have this
    /// gap; `tests/refinement.rs` is where each kind of writer is driven.
    ///
    /// # Errors
    ///
    /// [`Impossible`] when the observation describes a record no media could hold. Refused
    /// rather than normalised: a state builder that quietly repaired its input would answer
    /// questions about a record the caller did not describe, and answer them cheerfully.
    pub fn reconstructed(observation: &Observation) -> Result<Self, Impossible> {
        let mut records = Vec::with_capacity(observation.records.len());
        for (id, role, state, torn) in &observation.records {
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
                role: *role,
                media,
                acknowledged: *state == Durability::Acknowledged,
            });
        }
        Ok(Self::from_parts(
            records,
            observation.dispatched.clone(),
            observation.banks,
            observation.sealed_once,
        ))
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
/// `role` is what the writer under test says each of its records is for, for the reason
/// [`Observation::records`] gives: the harness names no record type. A writer that told the
/// abstraction the wrong thing would be describing a different run, and question 1 in
/// `tests/refinement.rs` — "is this a state the model says is reachable" — is what catches
/// it, because §11's order makes most wrong answers unreachable.
///
/// `dispatched` is what the caller *saw the writer do* — an effect that reached the world —
/// rather than what media says about it, for the same reason
/// [`waymaker_fault::Recovery::dispatched`] is: an oracle that only admitted an effect once
/// its intent was durable could not describe the violation it exists to catch.
///
/// Reports no bank: a caller with one to report builds an [`Observation`] directly and folds
/// [`bank_after_erase`] and [`bank_after_seal`] into its `banks` field instead.
pub fn abstraction(
    ledger: &Ledger,
    dispatched: &[RecordId],
    role: impl Fn(RecordId) -> Role,
) -> Observation {
    let mut sorted = dispatched.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    Observation {
        records: ledger
            .records()
            .map(|(id, state)| (id, role(id), state, ledger.torn(id).unwrap_or(false)))
            .collect(),
        dispatched: sorted,
        ..Observation::default()
    }
}

/// Whether the call recorded at `run.ops()[op]` changed any cell of media at all.
///
/// `false` when `op` is past the end of `run.ops()` — the call was never issued; when
/// [`Run::injection`] names it at [`Progress::None`] or at a zero [`Progress::Bytes`], which
/// that type's own documentation calls the same world as `None`; or when it is an
/// [`Op::Erase`] and `geometry`'s erase block is wider than the bytes [`Run::injection`] says
/// landed — a partial erase block is not a landed one, because
/// `waymaker_fault::Session::erase_blocks_of` rounds it down to zero cells changed. `geometry`
/// is the one this run's device was built with; the enumerated crash points never produce a
/// value that needs it, since an erase only ever tears at a whole block boundary, but a
/// hand-built [`Run`] from `Harness::run_one` can name a byte offset the enumeration would
/// not.
///
/// # Why this and not "did the call return `Ok`"
///
/// `waymaker-fault`'s writes land synchronously, so bytes on media never depend on whether the
/// caller's own `barrier` afterwards ran, or even on whether the call itself returned `Ok`: a
/// watchdog reset finishes the program unit or erase block in flight and still answers `Err`.
/// So a bank's state after a mutation is decided by how much of it reached media, which the
/// caller reads back and passes to [`bank_after_erase`] or [`bank_after_seal`] — not by this
/// module trying to infer "committed" from [`Run::injection`] alone, which cannot see a
/// watchdog's rounding.
#[must_use]
pub fn call_touched(run: &Run, op: usize, geometry: Geometry) -> bool {
    let Some(injection) = run.injection().filter(|injection| injection.op == op) else {
        return op < run.ops().len();
    };
    let bytes = match injection.progress {
        Progress::None => return false,
        Progress::Bytes(bytes) => bytes,
        Progress::Whole => return true,
    };
    if bytes == 0 {
        return false;
    }
    // A watchdog reset finishes the block in flight — rounds up, never down — so only the
    // other two causes can land fewer bytes than `Progress::Bytes` names.
    if injection.interruption == Interruption::Watchdog {
        return true;
    }
    if matches!(run.ops().get(op), Some(Op::Erase { .. })) {
        let block = geometry.erase_size();
        bytes & !block.wrapping_sub(1) != 0
    } else {
        true
    }
}

/// The bank state after the erase recorded at `run.ops()[op]`, given `prior`.
///
/// [`Bank::Erased`] if `erased`, [`Bank::Erasing`] if the call touched media without leaving
/// it erased, and `prior` unchanged if it never touched media at all — which is also this
/// run's answer for an erase never issued, since `op` is then past the end of `run.ops()`.
///
/// `erased` is the caller's own read of the bank after the run: whether the region is, in
/// fact, fully erased. `geometry` is [`call_touched`]'s. This module does not read bytes, for
/// the reason [`call_touched`] gives.
#[must_use]
pub fn bank_after_erase(
    prior: Bank,
    run: &Run,
    op: usize,
    geometry: Geometry,
    erased: bool,
) -> Bank {
    if !call_touched(run, op, geometry) {
        return prior;
    }
    if erased { Bank::Erased } else { Bank::Erasing }
}

/// The bank state after the seal program recorded at `run.ops()[op]`, given `prior`.
///
/// [`Bank::Sealed`] at `generation` if `sealed`, [`Bank::Sealing`] at `generation` if the call
/// touched media without that, and `prior` unchanged if it never touched media at all.
///
/// `sealed` is the caller's own read of the bank after the run: whether its header and seal
/// decode together at `generation` — see `waymaker_flash::bank::sealed_generation`.
/// `geometry` is [`call_touched`]'s. This module does not read bytes, for the reason
/// [`call_touched`] gives.
///
/// # `generation` is the model's number, not `waymaker_flash::bank::Generation`'s
///
/// [`crate::model::Journal::step`]'s `begin_seal` numbers a device's first-ever seal `1`, so
/// that a bank with no seal (`authoritative_generation() == None`) and a bank the model has
/// not yet distinguished from one both read as "nothing sealed here". The real
/// `Generation::FIRST` is `0`, because the firmware has a `Bank`-shaped `None` for that case
/// and does not need the reservation. A caller passes `real_generation.0 + 1` here, and the
/// two schemes agree from there: both increment by one per seal, so the shift is exact at
/// every later generation too.
#[must_use]
pub fn bank_after_seal(
    prior: Bank,
    run: &Run,
    op: usize,
    geometry: Geometry,
    generation: u32,
    sealed: bool,
) -> Bank {
    if !call_touched(run, op, geometry) {
        return prior;
    }
    if sealed {
        Bank::Sealed(generation)
    } else {
        Bank::Sealing(generation)
    }
}

#[cfg(test)]
mod tests {
    use waymaker_fault::{Harness, Injection, Interruption, Session};
    use waymaker_flash::storage::StableStorage;

    use super::*;

    /// Two 32-byte erase blocks, so a hand-built injection can land short of one.
    fn geometry() -> Geometry {
        Geometry::new(64, 32, 4, 1).expect("64 is two whole 32-byte blocks of 4-byte units")
    }

    /// A hand-built injection is the only route to a byte offset the enumerated crash points
    /// never produce for an erase — see [`call_touched`]'s docs.
    fn run_one_erase(progress: Progress) -> Run {
        let injection = Injection {
            op: 0,
            progress,
            interruption: Interruption::PowerLoss,
        };
        Harness::new(geometry())
            .run_one(injection, |session: &mut Session| session.erase(0, 32))
            .expect("the injection fires on the one erase this writer issues")
    }

    #[test]
    fn less_than_one_erase_block_is_not_touched() {
        // `Session::erase_blocks_of` rounds a partial-block landing down to zero, so this
        // never changed a cell of media even though `Progress::Bytes(1)` is not zero.
        let run = run_one_erase(Progress::Bytes(1));
        assert!(!call_touched(&run, 0, geometry()));
    }

    #[test]
    fn a_whole_erase_block_is_touched() {
        let run = run_one_erase(Progress::Bytes(32));
        assert!(call_touched(&run, 0, geometry()));
    }

    #[test]
    fn a_watchdog_reset_never_rounds_an_erase_down() {
        // The one cause that rounds up rather than down: the block in flight finishes.
        let injection = Injection {
            op: 0,
            progress: Progress::Bytes(1),
            interruption: Interruption::Watchdog,
        };
        let run = Harness::new(geometry())
            .run_one(injection, |session: &mut Session| session.erase(0, 32))
            .expect("the injection fires on the one erase this writer issues");
        assert!(call_touched(&run, 0, geometry()));
    }
}
