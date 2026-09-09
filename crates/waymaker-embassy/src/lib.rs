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
//! # What is here
//!
//! * [`ctx`] — issue [#35](https://github.com/madmax983/waymaker/issues/35)'s [`Ctx`] and
//!   its futures: an activity, a deadline, a new run, and the run's own ending.
//! * [`journal`] — the durable half [`Ctx`] asks. An implementor owns the media.
//! * [`dispatch`] — the world's half: what performs design document §07 step 4.
//! * [`wiring`] — issue [#36](https://github.com/madmax983/waymaker/issues/36)'s table: a
//!   row per activity, selected by its number, named for a log.
//! * [`decode`] — how a workflow reads recorded bytes. [`Decode`] names no codec, and
//!   nothing else in this crate does either: issue
//!   [#37](https://github.com/madmax983/waymaker/issues/37)'s `serde` and `postcard`
//!   features add a bridge and one format, and both stay inside that one module. They are
//!   not re-exported here, because a codec type on the crate root is a codec every
//!   workflow reads about.
//! * [`clock`] — design document §11's `PersistentClock` capability. It is here rather
//!   than in the kernel because the kernel's must-not-own cell names a clock, and
//!   `waymaker-flash`'s names timers.
//!
//! # There is no Embassy dependency
//!
//! The futures here are plain [`core::future::Future`]s, so Embassy's executor polls them
//! and this crate has no executor, no timer queue and no waker of its own. §02 decision 5
//! is that async syntax is an adapter; a façade that pulled in an executor to hand out four
//! futures would be more than one.
//!
//! # Status
//!
//! Rung 0.4's first three items are here, and so is the exit criterion: `cargo xtask size`
//! gates this crate's row against a ceiling of its own (issue
//! [#39](https://github.com/madmax983/waymaker/issues/39)). Still owed: the provisioning
//! example (issue [#38](https://github.com/madmax983/waymaker/issues/38)), and in-boot sleep
//! and the `continue_as_new` join (issue
//! [#110](https://github.com/madmax983/waymaker/issues/110)).

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod clock;
pub mod ctx;
pub mod decode;
pub mod dispatch;
pub mod journal;
pub mod wiring;

pub use clock::{ClockError, PersistentClock, PersistentTimer};
pub use ctx::{ActivityFuture, ContinueFuture, Ctx, Failure, TerminalFuture, TimerFuture};
pub use decode::Decode;
pub use dispatch::{ActivityDispatcher, Produced};
pub use journal::{Answer, Halted, Handoff, Journal};
pub use wiring::{Activity, Perform, Table, Unhandled};
