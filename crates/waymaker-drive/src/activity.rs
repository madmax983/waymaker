//! The world's half of the boundary: what happens when the kernel says dispatch.
//!
//! Synchronous, and bounded by the run's declared result bound. An activity that cannot
//! answer now says [`Performed::Pending`] rather than blocking, which is how a driver with
//! no executor still has a way to wait.

use waymaker_core::ActivityKind;

use crate::effect::DurableIntent;

/// What an activity did.
///
/// `out` is exactly `Bounds::effect_result_bytes` wide, so "what fits" and "what the run
/// declared" are one bound. An activity whose answer is wider says
/// [`Exhausted`](Self::Exhausted).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Performed {
    /// Success. The first `len` bytes of `out` are the result, and `len <= out.len()`.
    Completed(usize),
    /// Failure. The first `len` bytes of `out` are the failure payload.
    Failed(usize),
    /// The answer does not fit `out`.
    ///
    /// The effect is recorded as a failure with no payload — see
    /// [`Resolution::Exhausted`](crate::Resolution::Exhausted) — so the run makes progress
    /// and the workflow sees no part of the answer. A `Completed(len)` with `len` larger
    /// than `out` is a broken activity rather than this, and the driver refuses it with
    /// [`DriveError::ResultTooLong`](crate::DriveError).
    Exhausted,
    /// Not now. The run suspends under the identity it was dispatched with, and the next
    /// boot redelivers it — design document §14's redelivery contract.
    Pending,
}

/// The activities a workflow can call.
pub trait Activities {
    /// Perform `intent`'s effect and write its outcome into `out`.
    ///
    /// `intent` is design document §07 step 4's argument, and it exists only because steps 1
    /// to 3 completed. An activity that deduplicates downstream sees a repeat rather than a
    /// second effect when a reset redelivers it.
    fn perform(
        &mut self,
        intent: DurableIntent,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Performed;
}
