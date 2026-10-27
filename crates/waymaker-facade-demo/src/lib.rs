//! Design document §06's two examples, the driver's reference workflow, and the tests that
//! run them at every crash point.
//!
//! Issue [#106](https://github.com/madmax983/waymaker/issues/106) put the façade edge in a
//! crate of its own;
//! [ADR 0055](https://github.com/madmax983/waymaker/blob/main/docs/adr/0055-the-driver-and-the-bridge-are-layers-firmware-ships.md)
//! made `waymaker-drive` and the bridge layers, firmware Waymaker ships. What stays here is
//! what a layer may not hold: demonstrations, and tests that need `waymaker-fault`'s crash
//! injector.
//!
//! # What this crate owns
//!
//! * [`demo`] — the driver's reference workflow and world, with no `Future`, no Embassy and
//!   no allocation.
//! * [`ota`] — §06's first example: fetch an image, verify it, flash it.
//! * [`provisioning`] — §06's second example: wait for a window, register, retry on
//!   failure.
//!
//! Both examples are `async fn`s over `Ctx`, run through `waymaker_bridge::Bridge` on the
//! synchronous driver.
//!
//! # What this crate must not own
//!
//! On-media authority, a driver, or a bridge. It is written against the layers' public
//! surface and nothing else, which is what makes it a demonstration of that surface.
//!
//! # Why it is a crate above the layers
//!
//! It is not firmware Waymaker ships, and its tests depend on `waymaker-fault` and
//! `waymaker-rig` — test support no layer may name in any table.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod demo;
pub mod ota;
pub mod provisioning;
