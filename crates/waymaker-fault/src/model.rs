//! The three record states, and the ledger that holds them.
//!
//! Design document §15: "the model distinguishes records that were merely attempted,
//! records that may have become durable before acknowledgment, and records whose barrier
//! returned. Recovery may include an unacknowledged complete record, but it may never lose
//! an acknowledged one."
//!
//! Two of those three are the same shape from the media's point of view — bytes are either
//! there or not — and the difference between them is *when the power went away relative to
//! a barrier*, which is not something an image can be asked. That is why the ledger is
//! built by [`crate::Session`] as the writer runs, rather than reconstructed from the bytes
//! afterwards.

/// A record the writer under test declared, in its own numbering.
///
/// Opaque to this crate: what a record *is* belongs to the protocol being tested, and a
/// harness that knew would be a harness only one caller could use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId(pub u32);

/// What recovery is allowed, and required, to do with a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Durability {
    /// Nothing of it reached media. Recovery must **not** produce it.
    Attempted,
    /// Some or all of it reached media, and no barrier has ordered it since. Recovery may
    /// produce it or not; both are legal.
    PossiblyDurable,
    /// A barrier completed after every one of its writes. Recovery **must** produce it.
    Acknowledged,
}

/// Every record one run declared, in declaration order, with the state it ended in.
///
/// Each entry is `(record, state, torn)`. Tornness is kept beside the state rather than as a
/// fourth [`Durability`] because design document §15 names three record states and this is
/// not a fourth one: a torn record is [`Durability::PossiblyDurable`], and "torn" is the
/// extra thing that says recovery must not produce it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    entries: Vec<(RecordId, Durability, bool)>,
}

impl Ledger {
    /// Builds a ledger from `(record, state, torn)` entries already in declaration order.
    ///
    /// Public so that code with a recovery of its own to test — rung 0.2's bank selection,
    /// rung 0.3's effect protocol — can put [`verify_recovery`] to work against a history
    /// it wrote by hand, instead of only against one [`Harness`] produced.
    ///
    /// [`verify_recovery`]: crate::verify_recovery
    /// [`Harness`]: crate::Harness
    #[must_use]
    pub const fn new(entries: Vec<(RecordId, Durability, bool)>) -> Self {
        Self { entries }
    }

    /// The state `id` ended in, or `None` if this run never declared it.
    ///
    /// When a run declared `id` more than once — which [`verify_recovery`] treats as a
    /// breach in its own right — this is the first of them.
    ///
    /// [`verify_recovery`]: crate::verify_recovery
    #[must_use]
    pub fn state(&self, id: RecordId) -> Option<Durability> {
        self.entries
            .iter()
            .find(|(entry, ..)| *entry == id)
            .map(|(_, state, _)| *state)
    }

    /// Whether some of `id`'s bytes are on media and the rest are not.
    ///
    /// A torn record is never [`Durability::Acknowledged`], and [`verify_recovery`] refuses
    /// a recovery that produces one: design document §15 permits recovery to include "an
    /// unacknowledged **complete** record", and complete is the load-bearing word.
    ///
    /// [`verify_recovery`]: crate::verify_recovery
    #[must_use]
    pub fn torn(&self, id: RecordId) -> Option<bool> {
        self.entries
            .iter()
            .find(|(entry, ..)| *entry == id)
            .map(|(.., torn)| *torn)
    }

    /// The records this run declared, in the order it declared them.
    pub fn order(&self) -> impl Iterator<Item = RecordId> + use<'_> {
        self.entries.iter().map(|(id, ..)| *id)
    }

    /// The records that reached media at all, in declaration order.
    ///
    /// This is *committed history* as design document §15's oracle means it, and it is not
    /// the same list as [`order`](Self::order): a record none of whose bytes ever landed
    /// contributes nothing to media, so it cannot occupy a position between two records
    /// recovery did find. A prefix check run against the declaration order instead would
    /// leave a correct recovery — the records that are really there — with no accepting
    /// answer.
    pub fn committed(&self) -> impl Iterator<Item = RecordId> + use<'_> {
        self.entries
            .iter()
            .filter(|(_, state, _)| *state != Durability::Attempted)
            .map(|(id, ..)| *id)
    }

    /// Every record and its state, in declaration order.
    pub fn records(&self) -> impl Iterator<Item = (RecordId, Durability)> + use<'_> {
        self.entries.iter().map(|(id, state, _)| (*id, *state))
    }

    /// The records recovery is required to produce.
    pub fn acknowledged(&self) -> impl Iterator<Item = RecordId> + use<'_> {
        self.entries
            .iter()
            .filter(|(_, state, _)| *state == Durability::Acknowledged)
            .map(|(id, ..)| *id)
    }

    /// How many records this run declared.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this run declared no records at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
