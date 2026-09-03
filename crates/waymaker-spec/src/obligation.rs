//! What each of design document §14's guarantees is discharged by, and what is still owed.
//!
//! A specification whose clauses are not tied to the thing that checks them rots in exactly
//! two ways: a clause is written and never proved, or a proof is deleted and the clause
//! keeps claiming it. So each guarantee is a row here naming its proof, the falsifier that
//! shows the proof can fail, and — where a guarantee is only partly discharged — the rung
//! that owes the rest.
//!
//! [`CLAUSES`] is compared in three directions:
//!
//! * `tests/obligations.rs` requires every row's proof and falsifier to name a test target
//!   that exists, and every [`crate::invariant::Invariant`] to have a row.
//! * `cargo xtask check-layering`'s `recovery-spec` rule compares the ids here against
//!   `xtask::docs::SPEC_CLAUSES`, `CLAUDE.md` and
//!   [ADR 0015](https://github.com/madmax983/waymaker/blob/main/docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).
//! * The ADR itself carries each id, so a clause cannot be added without a decision record
//!   saying why.

/// How a guarantee is established.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Discharge {
    /// Exhaustive enumeration of every reachable ghost state.
    ///
    /// A proof within [`crate::model::Bound`] and silent outside it, which is why the bound
    /// travels in [`crate::explore::Explored`] rather than in a comment.
    Model,
    /// Exhaustive enumeration of inputs against the shipped firmware.
    ///
    /// Not a model of the decoder: the real `waymaker_flash` functions, over a domain the
    /// proof states.
    Firmware,
}

// There is deliberately no `Representation` variant for "true by construction". A property a
// wrong construction makes vacuous is not evidence, and the one place this specification
// leans on the representation — `bounded-decoding`'s allocation clause, which rests on
// `no_std` with no dependencies — is recorded as an `owed` note naming the gate rules that
// hold it, where a reader will see the caveat rather than a reassuring label.

impl Discharge {
    /// One word for the kind of evidence.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Firmware => "firmware",
        }
    }

    /// Every kind of evidence, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Model, Self::Firmware];
}

/// One guarantee, and everything that holds it up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Clause {
    /// Stable id. Cite it when a change touches this guarantee.
    pub id: &'static str,
    /// Design document §14's own words, or §02's where §14 does not state it.
    pub statement: &'static str,
    /// What kind of evidence discharges it.
    pub discharge: Discharge,
    /// The test target that discharges it, relative to this crate.
    pub proof: &'static str,
    /// The test target that shows the proof can fail.
    ///
    /// A proof with no falsifier is a proof nobody has watched fail, and this repository has
    /// already learnt that lesson once: `waymaker-fault`'s `tests/teeth.rs` exists because a
    /// crash suite that cannot catch a weakened codec is a crash suite that proves the
    /// harness runs.
    ///
    /// A [`Discharge::Firmware`] row may name its own proof, and only because those two
    /// files carry their falsifier inside them: `tests/bounded_decoding.rs` sweeps a domain
    /// and asserts both verdicts occur, and `tests/redelivery.rs` runs a catalogue of wrong
    /// allocators and requires each to be caught by a *named* claim. A row that named itself
    /// with neither would be a row exempting itself.
    pub falsifier: &'static str,
    /// What this row does *not* discharge, and the rung that owes it.
    pub owed: Option<&'static str>,
}

/// Every guarantee this crate is a specification of.
///
/// Six rather than issue [#20](https://github.com/madmax983/waymaker/issues/20)'s five:
/// §14 lists stable redelivery alongside the other four, and a specification of the recovery
/// guarantees that left out the one about redelivered effect identity would be a
/// specification of most of them.
pub const CLAUSES: &[Clause] = &[
    Clause {
        id: "prefix-safety",
        statement: "recovery exposes only a legal prefix of committed records",
        discharge: Discharge::Model,
        proof: "tests/spine.rs",
        falsifier: "tests/teeth.rs",
        owed: None,
    },
    Clause {
        id: "acknowledged-durability",
        statement: "any record acknowledged after its barrier is recovered after reset",
        discharge: Discharge::Model,
        proof: "tests/spine.rs",
        falsifier: "tests/necessity.rs",
        owed: None,
    },
    Clause {
        id: "durable-intent",
        statement: "no Waymaker-dispatched effect lacks a recoverable schedule record",
        discharge: Discharge::Model,
        proof: "tests/spine.rs",
        falsifier: "tests/necessity.rs",
        owed: None,
    },
    Clause {
        id: "single-authority",
        statement: "exactly one bank is authoritative after any crash",
        discharge: Discharge::Model,
        proof: "tests/spine.rs",
        falsifier: "tests/necessity.rs",
        owed: Some(
            "rung 0.2 owes the refinement, and three things about the model with it. There \
             is no two-bank adapter to abstract, so this clause is discharged against the \
             model alone — and a reconstructed state has no banks, so it is answered \
             vacuously for one. The model's banks hold no records: no transition changes a \
             bank and a record at once, so \"never recover the old run as current\" is not \
             something this machine can state, only \"exactly one bank is bootable\". And \
             generations are compared as unbounded integers, so a seal counter that wraps is \
             a counterexample no bound reaches",
        ),
    },
    Clause {
        id: "stable-redelivery",
        statement: "retries and reboot redelivery reuse the original effect identity",
        discharge: Discharge::Firmware,
        proof: "tests/redelivery.rs",
        falsifier: "tests/redelivery.rs",
        owed: None,
    },
    Clause {
        id: "bounded-decoding",
        statement: "malformed storage cannot cause out-of-bounds reads or allocation",
        discharge: Discharge::Firmware,
        proof: "tests/bounded_decoding.rs",
        falsifier: "tests/bounded_decoding.rs",
        owed: Some(
            "allocation-freedom is structural rather than measured: `waymaker-core` and \
             `waymaker-flash` are `no_std` with no dependencies and no `extern crate alloc`, \
             which the `crate-attributes` and `kernel-zero-dependencies` gate rules fail a \
             build over. Measuring it instead would need a global allocator, and the \
             workspace denies the `unsafe` a global allocator requires",
        ),
    },
];

/// The clause with this id.
#[must_use]
pub fn clause(id: &str) -> Option<&'static Clause> {
    CLAUSES.iter().find(|clause| clause.id == id)
}
