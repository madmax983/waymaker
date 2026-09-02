//! Waymaker's semantic kernel.
//!
//! A workflow is re-created from its beginning after reboot and deterministically replayed
//! through an ordered journal. This crate owns that semantics and nothing else:
//! borrowed record views, effect identity, the replay cursor, transition rules, and
//! capacity errors.
//!
//! # What this crate must not own
//!
//! Allocation, a serialization framework, CRC, a clock, a storage driver, an executor, or
//! logging. Those belong to [`waymaker-flash`] and [`waymaker-embassy`], which sit above
//! it. The rule is mechanical, not remembered: `cargo xtask check-layering` fails a build
//! in which this crate grows a dependency of any kind.
//!
//! # Budget
//!
//! Kernel state is budgeted at 128 bytes, excluding any page buffer. See design document
//! §04. The budget is not a comment: [`budget`] holds the numbers, emits a `const`
//! assertion for every registered kernel state type, and is the same table
//! `cargo xtask size` gates the measured section deltas against.
//!
//! [`waymaker-flash`]: https://docs.rs/waymaker-flash
//! [`waymaker-embassy`]: https://docs.rs/waymaker-embassy
//!
//! # Status
//!
//! Rung 0.1 in progress: effect identity, the activity kind vocabulary, the allocator,
//! the error vocabulary and the borrowed record views are here; the replay cursor and the
//! transition rules follow. The bytes those views are decoded from belong to
//! `waymaker-flash`.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod activity;
pub mod budget;
pub mod error;
pub mod id;
pub mod record;

pub use activity::{ActivityKind, ActivityName};
pub use error::{DecodeError, KernelError};
pub use id::{EffectId, EffectIdAllocator, EffectSeq, RunId};
pub use record::{RecordKind, RecordRef};
