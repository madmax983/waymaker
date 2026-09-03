//! Design document §14's guarantees, as predicates over a ghost state and a reader's answer.
//!
//! Four of §14's five guarantees are statements about one instant of one run, and those are
//! the four here. The fifth, bounded decoding, is a statement about a *decoder* rather than
//! about a state — malformed storage is by definition not a state this model can be in — and
//! it is discharged against the real firmware in `tests/bounded_decoding.rs`. Stable
//! redelivery is likewise a statement about
//! [`waymaker_core::EffectIdAllocator`](waymaker_core::id::EffectIdAllocator) and is
//! discharged in `tests/redelivery.rs`. [`crate::obligation`] is the table that says which
//! is which, so no guarantee can be assumed discharged by the module it is not in.
//!
//! # Why the reader is a parameter
//!
//! Because a specification that computed the recovery it then checked would prove that a
//! function agrees with itself. [`check`] takes the history a reader produced and judges it,
//! and `tests/teeth.rs` runs it against [`crate::reader::Mutant`]s that are wrong in one way
//! each. An invariant no reader can falsify is a comment.

use waymaker_fault::RecordId;

use crate::model::{Journal, OnMedia, Role};

/// One of design document §14's state-level guarantees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Invariant {
    /// "Recovery exposes only a legal prefix of committed records."
    ///
    /// Three clauses, because §14's word *legal* carries two of them.
    ///
    /// 1. The history is a prefix of **declaration order**. §02 decision 2 replays a run by
    ///    walking history in workflow order, so a recovery that skipped a record and carried
    ///    on would hand the replay cursor effect 1's outcome while the workflow is still
    ///    asking about effect 0.
    /// 2. The history is a prefix of **committed history**.
    /// 3. Every record in it is **wholly on media**. §15 permits recovery to include "an
    ///    unacknowledged **complete** record", and complete is the load-bearing word.
    ///
    /// The first two are the same statement exactly when no record can reach media behind
    /// one that did not, which is what [`crate::model::Guard::AppendOnly`] buys and what
    /// `tests/spine.rs` proves. They are checked separately because the equivalence is a
    /// theorem about this machine rather than a fact about recovery: the oracle in
    /// [`waymaker_fault`] compares against committed history alone, and its filter rests on
    /// that same theorem.
    PrefixSafety,
    /// "Any record acknowledged after its barrier is recovered after reset."
    AcknowledgedDurability,
    /// "No Waymaker-dispatched effect lacks a recoverable schedule record."
    ///
    /// Two clauses, because §14 says *schedule* record and means it. The intent has to be in
    /// the recovered history, and it has to be a [`crate::model::Role::Schedule`]: an
    /// acknowledged completion is a record that says an effect finished, and a guarantee
    /// satisfied by one would be satisfied by history written *after* the world was changed
    /// — which is the ordering §02 decision 3 exists to create.
    DurableIntent,
    /// §02 decision 7: exactly one bank is authoritative after any crash, once any bank has
    /// been sealed at all.
    SingleAuthority,
}

impl Invariant {
    /// Every state-level guarantee, in a fixed order.
    pub const ALL: [Self; 4] = [
        Self::PrefixSafety,
        Self::AcknowledgedDurability,
        Self::DurableIntent,
        Self::SingleAuthority,
    ];

    /// The clause id [`crate::obligation`] and the gate know this guarantee by.
    #[must_use]
    pub const fn clause(self) -> &'static str {
        match self {
            Self::PrefixSafety => "prefix-safety",
            Self::AcknowledgedDurability => "acknowledged-durability",
            Self::DurableIntent => "durable-intent",
            Self::SingleAuthority => "single-authority",
        }
    }
}

impl core::fmt::Display for Invariant {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.clause())
    }
}

/// How one guarantee was broken, with enough detail to name the record.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Breach {
    /// The guarantee that failed.
    pub invariant: Invariant,
    /// The state it failed in.
    pub state: Journal,
    /// What the reader produced.
    pub recovered: Vec<RecordId>,
    /// One line naming what went wrong.
    pub detail: String,
}

impl core::fmt::Display for Breach {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "{}: {} (recovered {:?} from {:?})",
            self.invariant, self.detail, self.recovered, self.state
        )
    }
}

/// Whether `invariant` holds of `recovered` in `state`.
///
/// # Errors
///
/// The [`Breach`] naming what went wrong.
pub fn holds(
    invariant: Invariant,
    state: &Journal,
    recovered: &[RecordId],
) -> Result<(), Box<Breach>> {
    let detail = match invariant {
        Invariant::PrefixSafety => prefix_safety(state, recovered),
        Invariant::AcknowledgedDurability => acknowledged_durability(state, recovered),
        Invariant::DurableIntent => durable_intent(state, recovered),
        Invariant::SingleAuthority => single_authority(state),
    };
    detail.map_or(Ok(()), |detail| {
        Err(Box::new(Breach {
            invariant,
            state: state.clone(),
            recovered: recovered.to_vec(),
            detail,
        }))
    })
}

/// The three clauses of [`Invariant::PrefixSafety`], in the order a reader would check them.
fn prefix_safety(state: &Journal, recovered: &[RecordId]) -> Option<String> {
    let declared: Vec<RecordId> = state.records().iter().map(|record| record.id).collect();
    if let Some(detail) = is_prefix_of(
        recovered,
        &declared,
        "this run declared",
        " — a replay walking history in workflow order would answer the wrong question",
    ) {
        return Some(detail);
    }
    let committed: Vec<RecordId> = state.committed().collect();
    if let Some(detail) = is_prefix_of(recovered, &committed, "committed history has", "") {
        return Some(detail);
    }
    recovered.iter().find_map(|produced| {
        let whole = state
            .records()
            .iter()
            .find(|record| record.id == *produced)
            .is_some_and(|record| record.media == OnMedia::Whole);
        (!whole).then(|| {
            format!(
                "record {} is not wholly on media, and §15 permits recovery to include only \
                 a complete record",
                produced.0
            )
        })
    })
}

/// Whether `recovered` is a prefix of `expected`, and where it stops being one.
fn is_prefix_of(
    recovered: &[RecordId],
    expected: &[RecordId],
    noun: &str,
    aside: &str,
) -> Option<String> {
    for (position, produced) in recovered.iter().enumerate() {
        match expected.get(position) {
            Some(wanted) if wanted == produced => {}
            Some(wanted) => {
                return Some(format!(
                    "record {} at position {position}, where {noun} record {}{aside}",
                    produced.0, wanted.0
                ));
            }
            None => {
                return Some(format!(
                    "record {} at position {position}, past the end of what {noun}",
                    produced.0
                ));
            }
        }
    }
    None
}

/// [`Invariant::AcknowledgedDurability`]: a barrier that returned is a promise.
fn acknowledged_durability(state: &Journal, recovered: &[RecordId]) -> Option<String> {
    state
        .acknowledged()
        .find(|required| !recovered.contains(required))
        .map(|lost| format!("record {} was acknowledged and recovery lost it", lost.0))
}

/// [`Invariant::DurableIntent`]: §02 decision 3, after the fact.
fn durable_intent(state: &Journal, recovered: &[RecordId]) -> Option<String> {
    for intent in state.dispatched() {
        if !recovered.contains(intent) {
            return Some(format!(
                "an effect was dispatched and recovery has no record {} to account for it",
                intent.0
            ));
        }
        let schedules = state
            .records()
            .iter()
            .find(|record| record.id == *intent)
            .is_some_and(|record| record.role == Role::Schedule);
        if !schedules {
            return Some(format!(
                "an effect was dispatched and record {} is what recovery has to account for \
                 it, which schedules nothing",
                intent.0
            ));
        }
    }
    None
}

/// [`Invariant::SingleAuthority`]: §02 decision 7, for a device that has sealed at all.
fn single_authority(state: &Journal) -> Option<String> {
    if !state.has_sealed() {
        return None;
    }
    match state.authoritative().len() {
        1 => None,
        0 => Some("this device sealed a bank and now has none to boot from".to_owned()),
        count => Some(format!(
            "{count} banks are authoritative, so which history is the run's is undecided"
        )),
    }
}

/// The first guarantee `recovered` breaks in `state`, in [`Invariant::ALL`] order.
///
/// # Errors
///
/// The [`Breach`] naming what went wrong.
pub fn check(state: &Journal, recovered: &[RecordId]) -> Result<(), Box<Breach>> {
    for invariant in Invariant::ALL {
        holds(invariant, state, recovered)?;
    }
    Ok(())
}
