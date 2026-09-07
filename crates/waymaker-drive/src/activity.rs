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
    /// Not now. The run suspends under the identity it was dispatched with. The next boot
    /// redelivers it, whether a reset came between the two or not — design document §14's
    /// redelivery contract.
    Pending,
}

/// The activities a workflow can call.
///
/// # At-least-once, and no more than that
///
/// Waymaker can perform one effect more than once. There are two causes:
///
/// * a **retry** — the activity answered [`Performed::Pending`], and the caller drove the
///   run again;
/// * a **reset** — power failed after the activity changed the world and before the outcome
///   record was durable. Design document §07 writes that record at step 5 and commits it at
///   step 7. Power can go at any point between step 4 and step 7.
///
/// Every attempt carries one identity: the `(RunId, EffectSeq)` the schedule record
/// committed, which [`DurableIntent::id`] gives. That pair is the only value a downstream
/// system can deduplicate on.
///
/// Waymaker does **not** promise exactly-once physical side effects. No setting changes
/// this, and the engine cannot: the world changed before the record of it did. There are two
/// ways to get exactly-once, and both are outside this engine — make the activity
/// idempotent, or deduplicate the identity downstream. An activity that does neither
/// performs its effect twice after a reset in that window.
pub trait Activities {
    /// Perform `intent`'s effect and write its outcome into `out`.
    ///
    /// `intent` is design document §07 step 4's argument. Some boot committed the schedule
    /// record for it before this call — this one, or an earlier one that a reset or a retry
    /// redelivered.
    ///
    /// # Postconditions
    ///
    /// An implementor must tolerate a duplicate attempt. The same `intent` can arrive more
    /// than once, and it carries the same identity every time. The trait's own docs say what
    /// this engine does not promise.
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
