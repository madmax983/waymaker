//! Embassy façade for Waymaker.
//!
//! This crate owns `Ctx`, activity futures, the dispatcher, wakeups, the persistent-clock
//! capability, and optional typed codec helpers. It is the only crate in the workspace
//! permitted to know that Embassy exists.
//!
//! # What this crate must not own
//!
//! On-media authority or hidden global state. Authority over what is committed belongs to
//! [`waymaker-flash`]; the semantics of replay belong to [`waymaker-core`].
//!
//! [`waymaker-flash`]: https://docs.rs/waymaker-flash
//! [`waymaker-core`]: https://docs.rs/waymaker-core
//!
//! # Status
//!
//! Rung 0.5's first item is here: [`clock`] holds design document §11's `PersistentClock`
//! capability and the only route to a deadline that needs one. The async `Ctx`, the
//! dispatcher and in-boot sleep arrive with rung 0.4.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod clock;

pub use clock::{ClockError, PersistentClock, PersistentTimer};
