//! The bridge from `waymaker-drive` to the async façade.
//!
//! Issue [#106](https://github.com/madmax983/waymaker/issues/106) split this edge out of
//! `waymaker-drive`; [ADR 0055](https://github.com/madmax983/waymaker/blob/main/docs/adr/0055-the-driver-and-the-bridge-are-layers-firmware-ships.md)
//! makes it firmware. [`Bridge`] is the one type that turns `waymaker-drive`'s synchronous
//! [`Boundary`](waymaker_drive::Boundary) into `waymaker-embassy`'s
//! [`Journal`](waymaker_embassy::Journal), which is how an `async fn` workflow
//! over `Ctx` runs on a flash journal the driver owns. `waymaker-drive` names no dependency
//! on `waymaker-embassy` at all, so `cargo metadata` still proves the protocol is usable
//! without the façade.
//!
//! # What this crate owns
//!
//! [`Bridge`], and nothing else. It renames four calls. The authority is the driver's, and
//! the façade's own documentation says the façade has none.
//!
//! # What this crate must not own
//!
//! On-media authority, workflows, or a dispatcher. It renames calls between two protocols
//! that each already own theirs; design document §06's examples run through it from
//! `waymaker-facade-demo`, above it.
//!
//! # Carrying `Suspended` across an `.await`
//!
//! [`Suspended`](waymaker_drive::Suspended)'s field is private to `waymaker-drive`, so nothing outside it may build
//! one — a workflow that could forge a stop would report a wait no activity is behind. A
//! plain poll cannot see one fall out of an `.await`: it only ever answers
//! [`Poll::Pending`](core::task::Poll::Pending). So [`Bridge`] keeps the real value the
//! boundary returned, and a [`Workflow`](waymaker_drive::Workflow) that polls a `Ctx`
//! future reads it back after the poll with [`Bridge::take_suspended`] — the same value,
//! never a new one. A dispatcher that is still working stalls a future with no boundary
//! call behind it at all, so there is no value to keep; that case reads `None` back and
//! falls to [`Suspended::awaiting_dispatch`](waymaker_drive::Suspended::awaiting_dispatch)
//! instead.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bridge;

pub use bridge::Bridge;
