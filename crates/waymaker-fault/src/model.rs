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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ledger {
    entries: Vec<(RecordId, Durability)>,
}

impl Ledger {
    /// Builds a ledger from entries already in declaration order.
    pub(crate) const fn from_entries(entries: Vec<(RecordId, Durability)>) -> Self {
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
            .find(|(entry, _)| *entry == id)
            .map(|(_, state)| *state)
    }

    /// The records this run declared, in the order it declared them.
    #[must_use]
    pub fn order(&self) -> Vec<RecordId> {
        self.entries.iter().map(|(id, _)| *id).collect()
    }

    /// Every record and its state, in declaration order.
    pub fn records(&self) -> impl Iterator<Item = (RecordId, Durability)> + use<'_> {
        self.entries.iter().copied()
    }

    /// The records recovery is required to produce.
    pub fn acknowledged(&self) -> impl Iterator<Item = RecordId> + use<'_> {
        self.entries
            .iter()
            .filter(|(_, state)| *state == Durability::Acknowledged)
            .map(|(id, _)| *id)
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
