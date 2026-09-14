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

use crate::model::{BANKS, Bank, BankId, Journal, OnMedia, Record, Role};

/// The part of a ghost state a crash harness can report.
///
/// Not the whole state: [`Journal::powered`] is a fact about the run rather than about the
/// media. Comparing observations rather than states is what lets a real run be matched
/// against the model without inventing the dimensions the harness has no answer for — the
/// same reason [`waymaker_fault::Recovery`] makes its extra dimensions optional instead of
/// defaulting them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Observation {
    /// Each record in declaration order, as `(id, role, state, torn, bank)`.
    ///
    /// The role comes from the caller rather than from the ledger: `waymaker-fault` names no
    /// record type, which is exactly what makes the harness reusable, so what a record is
    /// *for* is something only the writer under test knows.
    ///
    /// The bank is real, not a hardcoded `BankId::A` — the earlier version of this field
    /// carried no bank at all, so a device with record 0 retired in bank A and record 1
    /// authoritative in bank B abstracted to `observation()`'s correct
    /// `recover() == [1]` but `reconstructed()`'s wrong `[]`, because every record landed in
    /// `BankId::A` regardless of which bank it was really in. Codex found it on review of the
    /// pull request that closed the erase/reboot version of the reboot gap, one round after
    /// `next_id`'s own missing floor. [`abstraction`] tags every record `BankId::A`, matching
    /// the module docs' "no writer this function abstracts ever touches a second bank".
    pub records: Vec<(RecordId, Role, Durability, bool, BankId)>,
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
    /// The record-id counter, exactly as the real device's own reads.
    ///
    /// Not inferred from `records`: an id an erase dropped is not one `records.max_id + 1`
    /// can see, and inferring it would hand that id out a second time on the next `Declare`
    /// — the collision issue #67's identity scheme exists to forbid. Codex found this on
    /// review of the pull request that closed issue #67, on the observation/reconstruction
    /// path specifically. `Some(0)` for a writer that has declared nothing, matching
    /// [`crate::model::Journal::new`]; `None` only once the counter itself is exhausted.
    ///
    /// Must be strictly past every id in `records`, or [`Journal::reconstructed`] refuses it
    /// with [`Impossible::NextIdReissuesAResident`] — a floor `reconstructed` checks rather
    /// than trusts, since a caller can misreport this field within one observation and not
    /// only across the erase this doc comment's first paragraph is about.
    pub next_id: Option<u32>,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            dispatched: Vec::new(),
            banks: [Bank::Erased; BANKS],
            sealed_once: false,
            next_id: Some(0),
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
                        record.bank,
                    )
                })
                .collect(),
            dispatched: self.dispatched().to_vec(),
            banks: *self.banks(),
            sealed_once: self.has_sealed(),
            next_id: self.next_id(),
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
    /// [`Impossible`] when the observation describes a record no media could hold, or a
    /// `next_id` that lands at or before a resident record's own id. Refused rather than
    /// normalised: a state builder that quietly repaired its input would answer questions
    /// about a record the caller did not describe, and answer them cheerfully.
    ///
    /// The second check exists because the first round of review that added `next_id` to this
    /// struct closed only half the gap: a caller can still report a `next_id` that collides
    /// with a record it is naming in the very same observation, rather than one an erase
    /// dropped from an earlier one. Records 0 and 1 with `next_id: Some(1)` used to reconstruct
    /// without complaint; the next `Declare` then minted a second `RecordId(1)`, and the
    /// `Program` after it found the *older* record already whole and refused with
    /// `RecordAlreadyWritten`, stranding the new declaration — the identity collision issue
    /// #67's whole counter scheme exists to forbid, reached without ever going through an
    /// erase at all. Codex found it on review of the pull request that closed the erase/reboot
    /// version of this gap. The floor is exactly `reboot`'s own: `next_id` must be past every
    /// record this observation names, the same way a real device's counter can never point at
    /// an id something on media already holds.
    ///
    /// The third check is `record.id` itself: `next_id` is a single counter over the whole
    /// device, so no legal transition sequence can ever declare the same id twice, in one bank
    /// or two. `Journal::bank_of` — which `single_authority` and `durable_intent` both use to
    /// ask "which bank is this id's record really in" — answers with the *first* matching
    /// record it finds, so an `Observation` naming `RecordId(0)` once in a retired bank and
    /// again in the sole authoritative one made `single_authority` misreport a legitimately
    /// recovered record as coming from the wrong bank, and could equally make `durable_intent`
    /// skip checking a dispatch that genuinely needed checking. Codex found it on the same
    /// review round that gave records a real bank field. The real firmware's own effect
    /// sequence *does* restart at zero across a swap ([`waymaker_core::id::EffectIdAllocator`]
    /// via `Installed::allocator`), but that is a different identity space from this one:
    /// `RecordId` is this crate's own bookkeeping label, invented by issue #67 specifically to
    /// never be reused, and a caller bridging a real device into it has to assign each real
    /// record a distinct label the way [`abstraction`] already does — reusing the real
    /// restarting sequence number directly is a translation mistake, not a state the model can
    /// or should represent.
    pub fn reconstructed(observation: &Observation) -> Result<Self, Impossible> {
        let mut records = Vec::with_capacity(observation.records.len());
        let mut seen = std::collections::BTreeSet::new();
        for (id, role, state, torn, bank) in &observation.records {
            if !seen.insert(*id) {
                return Err(Impossible::RecordIdDeclaredTwice { record: *id });
            }
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
                bank: *bank,
            });
        }
        if let Some(next_id) = observation.next_id {
            if let Some(resident) = records.iter().map(|record| record.id).max() {
                if next_id <= resident.0 {
                    return Err(Impossible::NextIdReissuesAResident { resident });
                }
            }
        }
        if !observation.sealed_once {
            if let Some((bank, _)) = BankId::ALL
                .into_iter()
                .zip(observation.banks)
                .find(|(_, state)| state.authoritative_generation().is_some())
            {
                return Err(Impossible::SealedBeforeAnyHistoryOfSealing { bank });
            }
        }
        Ok(Self::from_parts(
            records,
            observation.dispatched.clone(),
            observation.banks,
            observation.sealed_once,
            observation.next_id,
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
    /// `next_id` names an id at or before a record this observation already holds.
    NextIdReissuesAResident {
        /// The highest resident record's id, which `next_id` must be strictly past.
        resident: RecordId,
    },
    /// The same `RecordId` names two different records, in one bank or two.
    RecordIdDeclaredTwice {
        /// The id declared more than once.
        record: RecordId,
    },
    /// A bank is durably [`Bank::Sealed`], but the device has never sealed anything.
    ///
    /// The only place a bank becomes [`Bank::Sealed`] is the model's own `commit_seal`, which
    /// sets `sealed_once` true in the same step — so a currently-sealed bank is itself proof
    /// that some seal has happened, and a caller reporting `sealed_once: false` beside one is
    /// describing two different devices at once. Left unchecked, [`Journal::recovering_bank`]
    /// takes the pre-seal convention at face value and answers
    /// [`BankId::A`] regardless of which bank the observation actually
    /// shows sealed, and [`Journal::has_sealed`] then exempts the state from
    /// [`crate::invariant::Invariant::SingleAuthority`] entirely — so a reconstructed state
    /// could recover a stale bank's records while the truly sealed bank's are ignored, with the
    /// one guarantee that would catch it never even consulted.
    SealedBeforeAnyHistoryOfSealing {
        /// The bank the observation reports as sealed.
        bank: BankId,
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
            Self::NextIdReissuesAResident { resident } => write!(
                formatter,
                "next_id is not past resident record {}, so the next declaration would reissue \
                 an id this observation already holds",
                resident.0
            ),
            Self::RecordIdDeclaredTwice { record } => write!(
                formatter,
                "record {} is named twice, and this crate's id scheme never reuses one",
                record.0
            ),
            Self::SealedBeforeAnyHistoryOfSealing { bank } => write!(
                formatter,
                "{bank:?} is sealed, but sealed_once is false, and a bank cannot be durably \
                 sealed on a device that has never sealed one"
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
/// Reports every record in `BankId::A`, and no bank *state*: a caller with a bank to report
/// builds an [`Observation`] directly and folds [`bank_after_erase`] and [`bank_after_seal`]
/// into its `banks` field instead. Exact here and only here, for the same reason `next_id`'s
/// own inference is — no writer this function abstracts ever touches a second bank.
///
/// `next_id` is inferred from `ledger.records()`'s highest id, which is exact here and only
/// here: no writer this function abstracts ever erases anything, so nothing is ever dropped
/// from what the ledger still holds for `records.max_id + 1` to lose track of — see
/// `Journal::from_parts`'s docs for the caller that does erase and cannot take this shortcut.
/// `checked_add` rather than `saturating_add`: an id already at `u32::MAX` has no id left to
/// set `next_id` *to*, and saturating back to
/// `u32::MAX` would hand that same id out a second time.
pub fn abstraction(
    ledger: &Ledger,
    dispatched: &[RecordId],
    role: impl Fn(RecordId) -> Role,
) -> Observation {
    let mut sorted = dispatched.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let records: Vec<_> = ledger
        .records()
        .map(|(id, state)| {
            (
                id,
                role(id),
                state,
                ledger.torn(id).unwrap_or(false),
                BankId::A,
            )
        })
        .collect();
    let next_id = records
        .iter()
        .map(|(id, ..)| id.0)
        .max()
        .map_or(Some(0), |highest| highest.checked_add(1));
    Observation {
        records,
        dispatched: sorted,
        next_id,
        ..Observation::default()
    }
}

/// Whether the call recorded at `run.ops()[op]` changed any cell of media at all.
///
/// `false` when `op` is past the end of `run.ops()` — the call was never issued; when it names
/// an [`Op::Barrier`] or a legal zero-length [`Op::Program`]/[`Op::Erase`] — neither moves a
/// byte whatever [`Run::injection`] says about it, the same reason
/// `waymaker_fault::inject::Op::mutates_nothing` excludes them from the enumeration; when
/// [`Run::injection`] names it at [`Progress::None`] or at a zero [`Progress::Bytes`], which
/// that type's own documentation calls the same world as `None`; or when it is an
/// [`Op::Erase`] and `geometry`'s erase block is wider than the bytes [`Run::injection`] says
/// landed — a partial erase block is not a landed one, because
/// `waymaker_fault::Session::erase_blocks_of` rounds it down to zero cells changed. `geometry`
/// is the one this run's device was built with; the enumerated crash points never produce a
/// value that needs any of the last three checks — a barrier and a zero-length call are never
/// offered a `Whole` point, and an erase only ever tears at a whole block boundary — but a
/// hand-built [`Run`] from `Harness::run_one` can name any of them.
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
///
/// # What a length and a progress cannot see
///
/// An idempotent call — `0xFF` programmed over media that is already erased, or an
/// already-erased block erased again — changes no cell either, and this reports it touched
/// anyway: `Op` carries no payload, so nothing here can tell such a call apart from an
/// ordinary one without `waymaker_fault::Session`'s own per-operation record surviving into
/// [`Run`], which it does not today. No call in this crate's own driven writers is ever
/// idempotent — every program writes real header or seal bytes over freshly erased media, and
/// the one erase always targets a bank an earlier install actually wrote — so the gap is
/// stated here rather than closed.
#[must_use]
pub fn call_touched(run: &Run, op: usize, geometry: Geometry) -> bool {
    let len = match run.ops().get(op) {
        None | Some(Op::Barrier) => return false,
        Some(Op::Program { len, .. } | Op::Erase { len, .. }) => *len,
    };
    if len == 0 {
        return false;
    }
    let Some(injection) = run.injection().filter(|injection| injection.op == op) else {
        return true;
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
/// every later generation too — except the last one. The real `Generation::successor` refuses
/// only at `Generation::MAX`, so the firmware can validly seal a bank *at* `Generation::MAX`;
/// this shift has no model number left for it (`u32::MAX + 1` does not exist), and
/// `Journal::step`'s own `begin_seal` refuses one generation earlier than that for the same
/// reason, at model generation `u32::MAX` rather than `u32::MAX + 1`. Codex found this reading
/// the shift on review of the pull request that gave records a real bank field, and it is real
/// — but removing the reservation (numbering the model's first seal `0` instead of `1`, since
/// [`Bank`] already tells "unsealed" apart from `Sealed(0)` through its own variants rather
/// than through the number) would change how many distinct generation values
/// [`Bound::generations`](crate::model::Bound::generations) admits at any given cap, which
/// `tests/census.rs`'s pinned counts would have to absorb for a boundary nothing here comes
/// anywhere near: `Bound::PROOF` caps generations at 3, `tests/refinement.rs`'s bank-swap
/// sweep at 3 more, and the one place this crate drives a real `u32::MAX` at all is
/// `model.rs`'s own hand-built `a_generation_at_the_ceiling_is_refused_rather_than_tied_with_the_other_bank`,
/// which exercises the model's ceiling entirely on its own terms and never through this shift.
/// This is the same standing `obligation.rs` already records for `single-authority`'s
/// generation dimension — "a generation is an unbounded integer, where the firmware refuses at
/// the ceiling rather than proving the refusal unnecessary" — one integer narrower than stated
/// there, and stated here rather than silently inherited.
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

    #[test]
    fn a_barrier_is_never_touched() {
        // A barrier moves no byte, whatever a hand-built injection claims about it.
        let injection = Injection {
            op: 0,
            progress: Progress::Whole,
            interruption: Interruption::PowerLoss,
        };
        let run = Harness::new(geometry())
            .run_one(injection, |session: &mut Session| session.barrier())
            .expect("the injection fires on the one barrier this writer issues");
        assert!(!call_touched(&run, 0, geometry()));
    }

    #[test]
    fn a_zero_length_program_is_never_touched() {
        let injection = Injection {
            op: 0,
            progress: Progress::None,
            interruption: Interruption::Failure,
        };
        let run = Harness::new(geometry())
            .run_one(injection, |session: &mut Session| session.program(0, &[]))
            .expect("the injection fires on the one program call this writer issues");
        assert!(!call_touched(&run, 0, geometry()));
    }
}
