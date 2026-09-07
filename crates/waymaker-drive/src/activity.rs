//! The world's half of the boundary: what happens when the kernel says dispatch.
//!
//! Synchronous, and bounded by the run's declared result bound. An activity that cannot
//! answer now says [`Performed::Pending`] rather than blocking, which is how a driver with
//! no executor still has a way to wait.

use waymaker_core::ActivityKind;

use crate::effect::DurableIntent;

/// What an activity did.
///
/// [`Driver`](crate::Driver) makes `out` exactly `Bounds::effect_result_bytes` wide, so for
/// that caller "what fits" and "what the run declared" are one bound. Another caller of
/// [`Activities::perform`] may pass any slice, so the obligation below is stated against
/// `out` rather than against the reserve.
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
    /// and the workflow sees no part of the answer.
    ///
    /// A `Completed(len)` or `Failed(len)` with `len` larger than `out.len()` is a broken
    /// activity. The driver records it the same way, because on media the two are the same
    /// statement, and because a refusal there strands the run.
    Exhausted,
    /// Not now. The run suspends under the identity it was dispatched with, and the next
    /// boot redelivers it — design document §14's redelivery contract.
    Pending,
}

/// The activities a workflow can call.
pub trait Activities {
    /// Perform `intent`'s effect and write its outcome into `out`.
    ///
    /// `intent` is design document §07 step 4's argument. Some boot committed the schedule
    /// record for it before this call — this one, or an earlier one that the reset
    /// redelivered. So an activity that deduplicates downstream sees a repeat rather than a
    /// second effect.
    ///
    /// # Postconditions
    ///
    /// An implementor must not truncate. Write no more than `out.len()` bytes, and report
    /// [`Performed::Exhausted`] when the answer is wider. An implementor that writes what
    /// fits and reports `Completed(out.len())` records a short result, and every replay of
    /// the run returns that short result: the driver cannot tell it from a complete one.
    fn perform(
        &mut self,
        intent: DurableIntent,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Performed;
}
