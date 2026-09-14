//! The bridge from `waymaker-drive` to `waymaker-embassy`.
//!
//! Issue [#106](https://github.com/madmax983/waymaker/issues/106). This crate holds design
//! document §06's async façade edge, so `waymaker-drive` does not have to.
//!
//! # What this crate owns
//!
//! * [`facade`] — [`Bridge`], the one type that turns
//!   [`waymaker_drive::Boundary`] into `waymaker-embassy`'s `Journal`.
//! * [`ota`] — §06's first example: fetch an image, verify it, flash it.
//! * [`provisioning`] — §06's second example: wait for a window, register, retry on
//!   failure.
//!
//! # Why a crate of its own
//!
//! Issue [#35](https://github.com/madmax983/waymaker/issues/35) asked that removing the
//! Embassy crate leave the protocol usable through the synchronous driver alone. A feature
//! flag on `waymaker-drive` once deleted these three modules to test that claim, but the
//! crate's dependency on `waymaker-embassy` stayed in the manifest, so the flag proved only
//! that no *other* module needed the façade — a façade regression still failed the same
//! build. Moving the edge here makes `waymaker-drive`'s independence a fact `cargo metadata`
//! states: it names no dependency on `waymaker-embassy`, in any table.
//!
//! # What this crate must not own
//!
//! On-media authority. It renames calls between two protocols that each already own theirs.
//!
//! # Why it is a crate above the layers
//!
//! For `waymaker-drive`'s reason. It is not firmware Waymaker ships, and it depends on both
//! `waymaker-drive` and `waymaker-embassy` at once — an edge no layer may hold.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod facade;
pub mod ota;
pub mod provisioning;

pub use facade::Bridge;
