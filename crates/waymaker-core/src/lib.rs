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
//! §04.
//!
//! [`waymaker-flash`]: https://docs.rs/waymaker-flash
//! [`waymaker-embassy`]: https://docs.rs/waymaker-embassy
//!
//! # Status
//!
//! Rung 0.0: the crate exists so that the layering is enforceable. The record codec,
//! cursor, and transition rules arrive with rung 0.1.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
