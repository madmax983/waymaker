//! Exhaustive enumeration of the ghost model's reachable states.
//!
//! This is what discharges the spine properties. Not a sample of traces and not a seeded
//! sweep: a breadth-first search that keeps going until no transition from any state it has
//! reached produces a state it has not, so "for every crash point" is a closed set rather
//! than a hope. Within [`Bound`] the result is a proof; outside it, it is silent, which is
//! why the bound is carried in [`Explored`] and quoted by every claim.
//!
//! # Fail closed
//!
//! A search that stops early is a proof about a smaller machine, reported as a proof about
//! this one. So there is a ceiling on the state count and reaching it is an
//! [`ExploreError`], never a truncation: the caller finds out that the space grew, rather
//! than finding out nothing.
//!
//! # The census exists because a thin proof passes
//!
//! Every state-space claim in this crate is only worth the states it visited. A guard
//! tightened by accident until half the machine is unreachable leaves every invariant
//! holding, and nothing about "all reachable states satisfy P" notices. [`Census`] counts
//! what the search actually saw — every transition kind, every refusal reason, every
//! record-state and bank-state edge — and `tests/census.rs` fails the build when one of them
//! stops being witnessed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use waymaker_fault::Durability;

use crate::invariant::{Breach, Invariant, check, holds};
use crate::model::{Bank, Bound, Guards, Illegal, Journal, Transition};
use crate::reader::Reader;

/// A transition with its payload dropped, so a census counts kinds rather than instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransitionKind {
    /// [`Transition::Declare`].
    Declare,
    /// [`Transition::Program`].
    Program,
    /// [`Transition::FailedProgram`].
    FailedProgram,
    /// [`Transition::Barrier`].
    Barrier,
    /// [`Transition::Dispatch`].
    Dispatch,
    /// [`Transition::BeginErase`].
    BeginErase,
    /// [`Transition::CommitErase`].
    CommitErase,
    /// [`Transition::BeginSeal`].
    BeginSeal,
    /// [`Transition::CommitSeal`].
    CommitSeal,
    /// [`Transition::Tear`].
    Tear,
    /// [`Transition::PowerLoss`].
    PowerLoss,
}

impl TransitionKind {
    /// Every transition kind, in a fixed order.
    pub const ALL: [Self; 11] = [
        Self::Declare,
        Self::Program,
        Self::FailedProgram,
        Self::Barrier,
        Self::Dispatch,
        Self::BeginErase,
        Self::CommitErase,
        Self::BeginSeal,
        Self::CommitSeal,
        Self::Tear,
        Self::PowerLoss,
    ];
}

impl Transition {
    /// This transition with its payload dropped.
    #[must_use]
    pub const fn kind(self) -> TransitionKind {
        match self {
            Self::Declare => TransitionKind::Declare,
            Self::Program(_) => TransitionKind::Program,
            Self::FailedProgram(_) => TransitionKind::FailedProgram,
            Self::Barrier => TransitionKind::Barrier,
            Self::Dispatch(_) => TransitionKind::Dispatch,
            Self::BeginErase(_) => TransitionKind::BeginErase,
            Self::CommitErase(_) => TransitionKind::CommitErase,
            Self::BeginSeal(_) => TransitionKind::BeginSeal,
            Self::CommitSeal(_) => TransitionKind::CommitSeal,
            Self::Tear => TransitionKind::Tear,
            Self::PowerLoss => TransitionKind::PowerLoss,
        }
    }
}

/// A bank's state with its generation dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BankShape {
    /// [`Bank::Erased`].
    Erased,
    /// [`Bank::Erasing`].
    Erasing,
    /// [`Bank::Sealing`].
    Sealing,
    /// [`Bank::Sealed`].
    Sealed,
}

impl BankShape {
    /// Every bank shape, in a fixed order.
    pub const ALL: [Self; 4] = [Self::Erased, Self::Erasing, Self::Sealing, Self::Sealed];

    /// This bank's shape.
    #[must_use]
    pub const fn of(bank: Bank) -> Self {
        match bank {
            Bank::Erased => Self::Erased,
            Bank::Erasing => Self::Erasing,
            Bank::Sealing(_) => Self::Sealing,
            Bank::Sealed(_) => Self::Sealed,
        }
    }
}

/// What the search actually saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Census {
    transitions: BTreeMap<TransitionKind, usize>,
    refusals: BTreeMap<Illegal, usize>,
    durability_steps: BTreeMap<(Durability, Durability), usize>,
    arrivals: BTreeMap<Durability, usize>,
    bank_steps: BTreeMap<(BankShape, BankShape), usize>,
    edges: usize,
}

impl Census {
    /// How many legal edges of this kind the search followed.
    #[must_use]
    pub fn transitions(&self, kind: TransitionKind) -> usize {
        self.transitions.get(&kind).copied().unwrap_or_default()
    }

    /// How many times a transition was refused for this reason.
    #[must_use]
    pub fn refusals(&self, reason: Illegal) -> usize {
        self.refusals.get(&reason).copied().unwrap_or_default()
    }

    /// How many edges moved a record from one §15 state to another.
    #[must_use]
    pub fn durability_steps(&self, from: Durability, to: Durability) -> usize {
        self.durability_steps
            .get(&(from, to))
            .copied()
            .unwrap_or_default()
    }

    /// How many edges moved a bank from one shape to another.
    #[must_use]
    pub fn bank_steps(&self, from: BankShape, to: BankShape) -> usize {
        self.bank_steps
            .get(&(from, to))
            .copied()
            .unwrap_or_default()
    }

    /// How many records arrived in this state.
    ///
    /// Its own counter rather than a self-pair in [`durability_steps`](Self::durability_steps):
    /// a record being declared is not a record moving between two of design document §15's
    /// states, and filing it as one would make `durability_edges` report a transition that
    /// does not exist the moment any transition appends a record that is not `Attempted`.
    #[must_use]
    pub fn arrivals(&self, state: Durability) -> usize {
        self.arrivals.get(&state).copied().unwrap_or_default()
    }

    /// How many legal edges the search followed in total.
    ///
    /// Pinned by `tests/census.rs` alongside the per-kind counts, because the reachable
    /// *state set* is not the transition *relation*: a guard tightened until a whole family
    /// of interleavings is unreachable can leave every state still reachable by another
    /// path, and then the state count, every invariant and every mutant verdict are
    /// unchanged while the machine has lost edges.
    #[must_use]
    pub const fn edges(&self) -> usize {
        self.edges
    }

    /// Every record-state edge the search saw, in a fixed order.
    #[must_use]
    pub fn durability_edges(&self) -> Vec<(Durability, Durability)> {
        self.durability_steps.keys().copied().collect()
    }

    /// Every bank-state edge the search saw, in a fixed order.
    #[must_use]
    pub fn bank_edges(&self) -> Vec<(BankShape, BankShape)> {
        self.bank_steps.keys().copied().collect()
    }
}

/// Why the search could not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExploreError {
    /// The state space is larger than the ceiling the caller allowed.
    ///
    /// Never a truncated answer: a proof over part of a machine, reported as a proof over
    /// the machine, is exactly the failure design document §15's "for every possible crash
    /// point" is a claim against.
    CeilingReached {
        /// The ceiling that was reached.
        ceiling: usize,
    },
}

impl core::fmt::Display for ExploreError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::CeilingReached { ceiling } => write!(
                formatter,
                "the state space exceeds the ceiling of {ceiling} states, so this search \
                 would have proved something about a smaller machine"
            ),
        }
    }
}

impl core::error::Error for ExploreError {}

/// A closed set of reachable states, and what it took to reach them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explored {
    bound: Bound,
    guards: Guards,
    states: BTreeSet<Journal>,
    census: Census,
}

impl Explored {
    /// The bound every claim about this search is a claim within.
    #[must_use]
    pub const fn bound(&self) -> Bound {
        self.bound
    }

    /// Which preconditions the machine enforced.
    #[must_use]
    pub const fn guards(&self) -> Guards {
        self.guards
    }

    /// Every reachable state.
    #[must_use]
    pub const fn states(&self) -> &BTreeSet<Journal> {
        &self.states
    }

    /// What the search saw.
    #[must_use]
    pub const fn census(&self) -> &Census {
        &self.census
    }

    /// The first state in which `reader` breaks any guarantee.
    ///
    /// `None` is the proof: no reachable state falsifies any of design document §14's
    /// state-level guarantees, within [`bound`](Self::bound).
    #[must_use]
    pub fn first_breach<R: Reader>(&self, reader: &R) -> Option<Box<Breach>> {
        self.states
            .iter()
            .find_map(|state| check(state, &reader.recover(state)).err())
    }

    /// The first state in which `reader` breaks this one guarantee.
    ///
    /// The counterexample search the necessity and teeth proofs run: they need a specific
    /// guarantee to fail, because a mutant caught by the wrong guarantee is a mutant that
    /// was not caught.
    #[must_use]
    pub fn first_breach_of<R: Reader>(
        &self,
        invariant: Invariant,
        reader: &R,
    ) -> Option<Box<Breach>> {
        self.states
            .iter()
            .find_map(|state| holds(invariant, state, &reader.recover(state)).err())
    }
}

/// Enumerates every state reachable from a fresh device.
///
/// # Errors
///
/// [`ExploreError::CeilingReached`] when the space is larger than `ceiling`. The search
/// fails rather than truncating.
pub fn explore(bound: Bound, guards: Guards, ceiling: usize) -> Result<Explored, ExploreError> {
    let alphabet = Journal::alphabet(bound);
    let start = Journal::new();
    let mut states = BTreeSet::new();
    let mut census = Census::default();
    let mut queue = VecDeque::new();

    if ceiling == 0 {
        // The start state counts. Checked here rather than only at the insert inside the
        // loop, so that a ceiling of zero is an error rather than a search that returns one
        // state and calls it closed.
        return Err(ExploreError::CeilingReached { ceiling });
    }
    states.insert(start.clone());
    queue.push_back(start);

    while let Some(state) = queue.pop_front() {
        for transition in &alphabet {
            match state.step(*transition, guards, bound) {
                Ok(next) => {
                    record_edge(&mut census, *transition, &state, &next);
                    if states.insert(next.clone()) {
                        if states.len() > ceiling {
                            return Err(ExploreError::CeilingReached { ceiling });
                        }
                        queue.push_back(next);
                    }
                }
                Err(reason) => {
                    *census.refusals.entry(reason).or_default() += 1;
                }
            }
        }
    }

    Ok(Explored {
        bound,
        guards,
        states,
        census,
    })
}

/// Counts one legal edge, and every record- and bank-state change it made.
fn record_edge(census: &mut Census, transition: Transition, from: &Journal, to: &Journal) {
    *census.transitions.entry(transition.kind()).or_default() += 1;
    census.edges = census.edges.saturating_add(1);

    for (before, after) in from.records().iter().zip(to.records()) {
        // `tests/machine.rs` proves no transition renumbers or drops a record, and this is
        // what stops the census silently mis-attributing an edge in the window before that
        // test runs: a zip over reordered records compares different records at the same
        // position and files the difference as a state change.
        debug_assert_eq!(before.id, after.id, "a transition renumbered a record");
        let (before, after) = (before.durability(), after.durability());
        if before != after {
            *census.durability_steps.entry((before, after)).or_default() += 1;
        }
    }
    // A record the transition created has no predecessor to compare against, and its
    // arrival is counted on its own rather than as a step between two states.
    for new in to.records().iter().skip(from.records().len()) {
        *census.arrivals.entry(new.durability()).or_default() += 1;
    }

    for (before, after) in from.banks().iter().zip(to.banks()) {
        let (before, after) = (BankShape::of(*before), BankShape::of(*after));
        if before != after {
            *census.bank_steps.entry((before, after)).or_default() += 1;
        }
    }
}
