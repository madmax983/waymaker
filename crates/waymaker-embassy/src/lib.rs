//! Embassy façade for Waymaker.
//!
//! This crate owns `Ctx`, activity futures, the dispatcher, wakeups, and optional typed
//! codec helpers. It is the only crate in the workspace permitted to know that Embassy
//! exists.
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
//! Rung 0.0: the crate exists so that the layering is enforceable. The async `Ctx`,
//! dispatcher, and in-boot timer arrive with rung 0.4.

#![no_std]
#![forbid(unsafe_code)]
