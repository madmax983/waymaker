//! A synchronous driver for design document §06's explicit kernel boundary.
//!
//! Issue [#28](https://github.com/madmax983/waymaker/issues/28). The kernel answers one
//! question at every effect boundary — is this already known, does it need dispatching, or
//! are we waiting — and [`waymaker_core::ReplayMachine`] is that answer. This crate is the
//! proof that the answer is enough: it drives the whole protocol through the boundary, with
//! no `Future`, no Embassy and no allocation anywhere in it.
//!
//! # What this crate owns
//!
//! * [`Boundary`] — the workflow's half of §06: one method, and a [`Suspended`] a workflow
//!   propagates with `?` where an async façade would `.await`.
//! * [`Workflow`] and [`Identity`] — a plain value re-run from its beginning after every
//!   reset.
//! * [`Activities`], [`Performed`] and [`Clocks`] — the world's half: what a run asks of
//!   the world, bounded by the run's declared result bound, and the clocks its deadlines
//!   are measured against.
//! * [`Effect`], [`Dispatchable`], [`DurableIntent`], [`CheckedDispatch`], [`InputMismatch`],
//!   [`Resolution`], [`Scheduled`] and [`Resolved`] — design document §07's seven steps, in
//!   the only order that compiles, with a kind and an input bound to this effect alone.
//! * [`Driver`], [`Progress`] and [`DriveError`] — the loop: `waymaker-flash`'s recovery
//!   scan and two-barrier writer joined to the kernel's transition table.
//!
//! This crate names no dependency on `waymaker-embassy`, in any table. Issue
//! [#106](https://github.com/madmax983/waymaker/issues/106):
//! [`waymaker-bridge`](https://docs.rs/waymaker-bridge) holds the bridge to
//! `waymaker-embassy`'s `Ctx`, one layer above this crate, and `waymaker-facade-demo` runs
//! design document §06's two examples and this crate's reference workflow through it.
//!
//! # What this crate must not own
//!
//! On-media authority and the transition table. Both are below it: a record becomes history
//! in `waymaker-flash` and what may follow what is decided in `waymaker-core`. Nothing here
//! reinterprets either. The driver reads records only to *feed* the kernel and writes only
//! records the kernel asked for.
//!
//! # Why it is a layer
//!
//! Because it is firmware Waymaker ships. A device that runs a workflow on flash needs
//! exactly this: the recovery scan, the two-barrier writer and the transition table joined
//! into one loop. It began as test support above the layers
//! ([ADR 0024](https://github.com/madmax983/waymaker/blob/main/docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md)),
//! where a board had to copy it to use it;
//! [ADR 0055](https://github.com/madmax983/waymaker/blob/main/docs/adr/0055-the-driver-and-the-bridge-are-layers-firmware-ships.md)
//! promotes it, so every public function here is reached by the size probe and charged
//! against a code-flash ceiling of its own. It is still not the Embassy façade:
//! `waymaker-embassy` owns `Ctx` and the futures, and this crate is the protocol they are a
//! façade over. Driving the protocol here is what makes that falsifiable.
//!
//! # At-least-once
//!
//! Waymaker can perform one effect more than once: after a retry, and after a reset between
//! the activity and its committed outcome. Every attempt carries the identity the schedule
//! record committed. Waymaker promises that identity and nothing more. It does not give
//! exactly-once physical side effects. Read [`Activities`]. An implementer writes against
//! that trait.
//!
//! # The lifetime discipline
//!
//! Every borrowed result — a replayed outcome, a redelivered activity's answer, a
//! recovered terminal payload — points into a buffer the *caller* owns and the driver
//! reuses. [`Boundary::call`] derives its borrow from `&mut self`, so a workflow that holds
//! one across the next boundary does not compile; that is a `compile_fail` doctest on the
//! method rather than a paragraph.
//!
//! # Running one
//!
//! [`Driver::boot`] takes the device, the world, the workflow and two caller-owned buffers,
//! and runs the workflow as far as it goes in this boot. `waymaker-facade-demo`'s `demo`
//! module is the reference workflow and world, and its tests drive them at every crash point
//! `waymaker-fault` lists; they live there because a layer may not depend on test support.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod activity;
mod boundary;
mod drive;
mod effect;
mod workflow;

pub use activity::{Activities, Clocks, Performed};
pub use boundary::{Answered, Boundary, Handoff, Suspended};
pub use drive::{Conclusion, DriveError, Driver, Progress, Scratch};
pub use effect::{
    CheckedDispatch, Dispatchable, DurableIntent, Effect, InputMismatch, Resolution, Resolved,
    Scheduled,
};
pub use workflow::{Identity, Workflow};
