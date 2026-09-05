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
            "the refinement, and three things about the model with it. There is now a \
             two-bank adapter to abstract — `waymaker_flash::bank`, issue #22 — and \
             `tests/refinement.rs` does not yet abstract it, so this clause is still \
             discharged against the model alone and a reconstructed state has no banks, \
             which answers it vacuously for one. The model's banks hold no records: no \
             transition changes a bank and a record at once, so \"never recover the old run \
             as current\" is not something this machine can state, only \"exactly one bank \
             is bootable\". And generations are compared as unbounded integers, so a seal \
             counter that wraps is a counterexample no bound reaches — the firmware makes \
             that unreachable rather than orderable (`Generation::successor` refuses at the \
             ceiling, ADR 0017), which is a fact about the code and not yet about the model",
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
            "two restrictions. Allocation-freedom is structural rather than measured: \
             `waymaker-core` and `waymaker-flash` are `no_std` with no dependencies and no \
             `extern crate alloc`, which the `crate-attributes` and \
             `kernel-zero-dependencies` gate rules fail a build over — measuring it instead \
             would need a global allocator, and the workspace denies the `unsafe` one \
             requires. And the input domain is stated rather than universal: exhaustive to \
             three bytes, which is a quarter of a header, plus truncations, single-byte \
             mutations, coordinated pairs over an eight-value corruption alphabet, and every \
             declared payload length. A bug needing three coordinated corrupt fields, or two \
             outside that alphabet, is outside it. The scan half has a domain of its own: \
             every stale-tail gap up to two frames on and off the program granularity, every \
             truncation of a three-frame journal, and sampled coordinated corruption of a \
             two-frame one — a termination bug needing a longer gap, a fourth frame or an \
             unsampled pair is outside that",
        ),
    },
];

/// The clause with this id.
#[must_use]
pub fn clause(id: &str) -> Option<&'static Clause> {
    CLAUSES.iter().find(|clause| clause.id == id)
}
