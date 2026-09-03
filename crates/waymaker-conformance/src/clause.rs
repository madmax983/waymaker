//! Design document §12's contract, as a table.
//!
//! Issue [#21](https://github.com/madmax983/waymaker/issues/21) lists what the storage
//! contract has to "document and test". A list in an issue is a list that rots, so it is a
//! table here, every row carries the sentence it came from, and every row says *what*
//! discharges it — including the two rows nothing in this crate can discharge, which are
//! the rows a suite that wanted to look complete would have quietly dropped.
//!
//! The `storage-conformance` rule of `cargo xtask check-layering` compares this table
//! against `xtask::docs::STORAGE_CONTRACT_CLAUSES`, against `CLAUDE.md`, and against
//! [ADR 0016](https://github.com/madmax983/waymaker/blob/main/docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md),
//! so a clause cannot be added to one of the four and forgotten in the others.

/// What holds a clause up.
///
/// The point of the distinction is that two of §12's five sentences are about what survives
/// a *reset*, and no suite running inside one process can observe a reset. Saying so is the
/// difference between a conformance suite and a conformance suite's reputation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Discharge {
    /// [`crate::suite::run`] observes it directly, in one process, against the adapter.
    InProcess,
    /// [`crate::durability::arm`] writes a witness, the caller cuts power, and
    /// [`crate::durability::verify`] reads the answer. Only a caller that actually resets
    /// the device discharges it.
    AcrossReset,
    /// A crash injector, not a suite: `waymaker-fault` interrupts a write at every byte and
    /// every erase block. Nothing an adapter can be *asked* to do demonstrates it, because a
    /// driver that never fails satisfies "may fail" vacuously.
    Injected,
    /// The driver's, and stated so that its absence from the suite is a decision rather than
    /// an oversight.
    Driver,
}

impl Discharge {
    /// A short static description, for a report a driver author reads.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::InProcess => "the in-process suite",
            Self::AcrossReset => "the across-reset witness",
            Self::Injected => "a crash injector, not a suite",
            Self::Driver => "the driver, not the protocol",
        }
    }
}

/// One sentence of design document §12's required storage contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Clause {
    /// Stable identifier, cited when a change touches this clause.
    pub id: &'static str,
    /// The sentence, as §12 and issue #21 state it.
    pub sentence: &'static str,
    /// What holds it up.
    pub discharge: Discharge,
}

/// Every clause of the storage contract.
///
/// The first five are issue #21's "contract to document and test", in its order. The sixth
/// is not from that list: it is what [`StableStorage`](waymaker_flash::storage::StableStorage)'s
/// own documentation says each operation does, and without it the suite would be a suite of
/// refusals that never checked that a legal operation works.
pub const CLAUSES: &[Clause] = &[
    Clause {
        id: "interruptible-mutations",
        sentence: "`program` and `erase` may fail or be interrupted at any supported unit.",
        discharge: Discharge::Injected,
    },
    Clause {
        id: "barrier-is-durable",
        sentence: "After `barrier` returns, all earlier successful mutations survive reset.",
        discharge: Discharge::AcrossReset,
    },
    Clause {
        id: "barrier-orders-what-follows",
        sentence: "No later mutation may become durable before mutations ordered by a completed barrier.",
        discharge: Discharge::AcrossReset,
    },
    Clause {
        id: "validated-before-media",
        sentence: "The adapter validates erase/program alignment before touching media.",
        discharge: Discharge::InProcess,
    },
    Clause {
        id: "one-way-bits-are-the-drivers",
        sentence: "Flash-specific one-way bit programming rules remain the driver's responsibility.",
        discharge: Discharge::Driver,
    },
    Clause {
        id: "operations-act-on-what-they-name",
        sentence: "`read`, `program` and `erase` act on exactly the region they name, and `barrier` changes no media.",
        discharge: Discharge::InProcess,
    },
];

/// The clause with this id, if there is one.
#[must_use]
pub fn clause(id: &str) -> Option<&'static Clause> {
    CLAUSES.iter().find(|candidate| candidate.id == id)
}
