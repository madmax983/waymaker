//! The formal specification of Waymaker's recovery invariants.
//!
//! Issue [#20](https://github.com/madmax983/waymaker/issues/20), from design document §14
//! (Guarantees) and §15 (Testing and verification). Property tests cover what somebody
//! remembered to generate; §14's guarantees have to hold at *every* crash point, which makes
//! them worth stating as a specification with proofs behind it rather than as a suite.
//!
//! # The three layers of this crate
//!
//! Issue #20 asks for a verified core that is small and stable, with the unverified glue
//! clearly separated. The separation is the module list:
//!
//! | | Modules | What it is |
//! | --- | --- | --- |
//! | **The specification** | [`model`], [`reader`], [`invariant`] | Pure, total, and independent of any implementation. Preconditions, postconditions, the ghost model of committed history, and §14's guarantees as predicates. Nothing here reads a byte, allocates a device or knows a frame exists. |
//! | **The proofs** | [`mod@explore`] | Exhaustive breadth-first enumeration of every reachable state, with a ceiling it fails against rather than truncates at. |
//! | **The glue** | [`refine`], [`obligation`] | Unverified: the abstraction function from a real crashed run, and the table saying which guarantee is discharged by what. Wrong glue makes a proof about the wrong thing, which is why the refinement test checks α's output against the enumerated state space instead of trusting it. |
//!
//! # What is proved, and within what
//!
//! Every claim is bounded and the bound travels with it in [`explore::Explored::bound`].
//! Within [`model::Bound::PROOF`] the enumeration is closed — no reachable state is missed —
//! so "for every possible crash point" is a finished search rather than a sample. Outside it
//! the crate says nothing, and `tests/census.rs` is what stops the search quietly shrinking
//! until the silence covers everything.
//!
//! # Why this is not Verus
//!
//! The workspace pins one stable toolchain and refuses a pipeline stage with a download in
//! it, and an SMT-backed verifier is a second toolchain and a large one. What is here
//! instead is an exhaustive finite-state proof: weaker than Verus in that it is bounded,
//! stronger than a property test in that within the bound it is not a sample, and it runs in
//! CI on the pinned toolchain with no dependency the workspace did not already have. See
//! [ADR 0015](https://github.com/madmax983/waymaker/blob/main/docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).
//!
//! ```
//! use waymaker_spec::explore::explore;
//! use waymaker_spec::model::{Bound, Guards};
//! use waymaker_spec::reader::Specified;
//!
//! let explored = explore(Bound::PROOF, Guards::ENFORCED, 100_000)
//!     .expect("the proof bound's state space fits under the ceiling");
//!
//! // No reachable state falsifies any of design document §14's state-level guarantees.
//! assert!(explored.first_breach(&Specified).is_none());
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod explore;
pub mod invariant;
pub mod model;
pub mod obligation;
pub mod reader;
pub mod refine;

pub use explore::{BankShape, Census, ExploreError, Explored, TransitionKind, explore};
pub use invariant::{Breach, Invariant, check, holds};
pub use model::{
    BANKS, Bank, BankId, Bound, Guard, Guards, Illegal, Journal, OnMedia, Record, Role, Transition,
};
pub use obligation::{CLAUSES, Clause, Discharge, clause};
pub use reader::{Mutant, Reader, Specified};
pub use refine::{Impossible, Observation, abstraction};
