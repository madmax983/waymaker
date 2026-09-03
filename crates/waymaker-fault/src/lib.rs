//! The in-memory storage model and the crash injector.
//!
//! Issue [#18](https://github.com/madmax983/waymaker/issues/18), from design document §15
//! ("crash testing is part of the design, not a post-MVP hardening phase") and §12 (the
//! required storage contract). Nothing here is ever flashed: this crate models media so
//! that the crates below it can be run against every way a write can go wrong, and it is
//! excluded from the workspace's `default-members` so that no firmware target ever sees it.
//!
//! # What this crate owns
//!
//! * [`Device`] — a [`StableStorage`](waymaker_flash::storage::StableStorage) whose bytes
//!   behave the way NOR flash behaves: erased is `0xFF`, programming can only clear bits,
//!   and every operation is validated against its geometry before media is touched.
//! * [`Op`], [`Injection`] and [`injections`] — a write sequence, and the complete,
//!   deterministic list of points at which *that sequence* can go wrong. Enumerated, never
//!   sampled. It is not a fixpoint: an operation a writer performs only because a call
//!   failed exists in no fault-free sequence and has no crash points of its own.
//! * [`Session`] — the storage a writer under test is handed: it records the sequence, it
//!   carries at most one injection, and after a power loss it is dead.
//! * [`Ledger`] and [`Durability`] — §15's three record states: merely attempted, possibly
//!   durable before acknowledgment, and barrier-returned.
//! * [`Harness`] — runs a writer once per injection, so "every crash point" is a loop
//!   rather than a promise.
//! * [`verify_recovery`] — §15's core property oracle, as a function.
//!
//! # What this crate must not own
//!
//! Records. Nothing here knows what a frame is, which is what makes it reusable: the writer
//! under test is any closure over any `StableStorage`, and the harness sees offsets and
//! lengths. `waymaker-flash`'s journal and the effect protocol are two such writers, and
//! [`tests/`](https://github.com/madmax983/waymaker/tree/main/crates/waymaker-fault/tests)
//! drives both through it unmodified, alongside a third with a byte layout of its own.
//!
//! What "reusable" cannot mean is a test *inside* `waymaker-flash` using this crate: that
//! would be a dependency a layer may not have, in any dependency kind, and the workspace
//! gate refuses it. The harness is generic over the writer instead, and the tests that drive
//! it live here.
//!
//! # Why it is a crate rather than a module in `waymaker-flash`
//!
//! Because `waymaker-flash` is firmware. A public function of a layer must be reached by
//! the size probe, so an exhaustive host-side enumerator inside it would be charged against
//! an 8 KiB code-flash budget — and it could not be written at all under `#![no_std]`. See
//! [ADR 0013](https://github.com/madmax983/waymaker/blob/main/docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
//!
//! ```
//! use waymaker_fault::{Durability, Harness, RecordId};
//! use waymaker_flash::storage::{Geometry, StableStorage};
//!
//! let geometry = Geometry::new(64, 32, 4, 1).expect("a legal geometry");
//! let runs = Harness::new(geometry)
//!     .run(|session| {
//!         session.begin_record(RecordId(0));
//!         session.program(0, b"\xAA\xAA\xAA\xAA")?;
//!         session.barrier()
//!     })
//!     .expect("the writer succeeds with no faults armed");
//!
//! // One fault-free run, and one run per crash point.
//! assert!(runs.len() > 1);
//! // The fault-free run acknowledged the record, so recovery must not lose it.
//! let clean = runs.first().expect("the first run is the fault-free one");
//! assert_eq!(clean.ledger().state(RecordId(0)), Some(Durability::Acknowledged));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod device;
mod inject;
mod model;
mod oracle;
mod rng;
mod session;

pub use device::{Device, ERASED, FaultError, OneWayBits};
pub use inject::{Injection, Interruption, Op, Progress, injections};
pub use model::{Durability, Ledger, RecordId};
pub use oracle::{Breach, Recovery, verify_oracle, verify_recovery};
pub use rng::{Rng, random_geometry};
pub use session::{Harness, HarnessError, Run, Session};
