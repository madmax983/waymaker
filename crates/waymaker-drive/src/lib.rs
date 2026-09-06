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
//! * [`Activities`] and [`Performed`] — the world's half, bounded by the caller's buffer.
//! * [`Driver`], [`Progress`] and [`DriveError`] — the loop: `waymaker-flash`'s recovery
//!   scan and two-barrier writer joined to the kernel's transition table.
//! * [`demo`] — a reference workflow and world, in the library so that the firmware target
//!   builds them too.
//!
//! # What this crate must not own
//!
//! On-media authority and the transition table. Both are below it: a record becomes history
//! in `waymaker-flash` and what may follow what is decided in `waymaker-core`. Nothing here
//! reinterprets either. The driver reads records only to *feed* the kernel and writes only
//! records the kernel asked for.
//!
//! # Why it is a crate above the layers
//!
//! For [`waymaker-rig`](https://docs.rs/waymaker-rig)'s reason and one more. A layer's
//! public functions must all be reached by the size probe, so a driver listed in
//! `xtask::policy::LAYERS` would be charged against design document §04's code-flash budget
//! — and this is not firmware Waymaker ships. It is also not the Embassy façade: `Ctx`, the
//! async dispatcher and wakeups are rung 0.4's, and `waymaker-embassy` is meant to be a
//! façade over exactly this protocol. Driving the protocol here is what makes that
//! falsifiable. See
//! [ADR 0024](https://github.com/madmax983/waymaker/blob/main/docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md).
//!
//! # The lifetime discipline
//!
//! Every borrowed result — a replayed outcome, a redelivered activity's answer, a
//! recovered terminal payload — points into a buffer the *caller* owns and the driver
//! reuses. [`Boundary::call`] derives its borrow from `&mut self`, so a workflow that holds
//! one across the next boundary does not compile; that is a `compile_fail` doctest on the
//! method rather than a paragraph.
//!
//! ```
//! use waymaker_core::RunId;
//! use waymaker_drive::demo::{HASHED, Pipeline, World};
//! use waymaker_drive::{Conclusion, Driver, Progress};
//! # use waymaker_fault::Device;
//! # use waymaker_flash::frame::ProgramAlign;
//! # use waymaker_flash::recovery::JournalRegion;
//! # use waymaker_flash::storage::Geometry;
//! # let geometry = Geometry::new(4096, 1024, 4, 1).expect("a legal geometry");
//! # let align = ProgramAlign::new(4).expect("a legal granularity");
//! # let region = JournalRegion::spanning(geometry, 0, 1024, align).expect("a legal region");
//! let mut device = Device::new(geometry);
//! let mut workflow = Pipeline::new();
//! let mut world = World::new();
//! let mut page = [0_u8; 256];
//! let mut result = [0_u8; 64];
//!
//! let progress = Driver::new(region, RunId(1))
//!     .boot(&mut device, &mut world, &mut workflow, &mut page, &mut result)
//!     .expect("the run completes");
//!
//! assert_eq!(
//!     progress,
//!     Progress::Finished { conclusion: Conclusion::Completed, result_len: HASHED.len() }
//! );
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod activity;
mod boundary;
pub mod demo;
mod drive;
mod workflow;

pub use activity::{Activities, Performed};
pub use boundary::{Boundary, Suspended};
pub use drive::{Conclusion, DriveError, Driver, Progress};
pub use workflow::{Identity, Workflow};
