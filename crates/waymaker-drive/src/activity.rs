//! The world's half of the boundary: what happens when the kernel says dispatch.
//!
//! Synchronous, and bounded by the run's declared result bound. An activity that cannot
//! answer now says [`Performed::Pending`] rather than blocking, which is how a driver with
//! no executor still has a way to wait.
//!
//! [`Clocks`] is the same half of the boundary for design document §11's deadlines: the
//! world is what a run asks things of, and time is one of them.

use waymaker_core::ActivityKind;
use waymaker_core::timer::{ClockCapability, ClockKind};

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
    /// Not now. The run suspends under the identity it was dispatched with. The driver
    /// redelivers it on the next pass, with or without a reset in between — design document
    /// §14's redelivery contract.
    Pending,
}

/// The activities a workflow can call.
///
/// # At-least-once, and no more than that
///
/// Waymaker can perform one effect more than once. Two causes do this:
///
/// * a **retry** — the activity answered [`Performed::Pending`], and the caller drove the
///   run again;
/// * a **reset** — power failed after the activity changed the world and before the outcome
///   record was durable. The driver writes that record at design document §07 step 5 and
///   commits it at step 7, and power can fail at any point between step 4 and step 7.
///
/// A reset in that window has two outcomes, and one of them is not a second attempt. A reset
/// at a boundary between two storage operations leaves a whole journal, and the next boot
/// redelivers. A reset *inside* the outcome frame or its seal leaves a torn tail with no
/// append point, and [`Driver`](crate::Driver) refuses that bank rather than repairing it —
/// so the effect happened, no record of it ever will, and the run stops. Recycling such a
/// bank is §10's `continue_as_new`, which this driver does not perform.
///
/// Neither cause has a limit. Two resets that each redeliver perform the effect three
/// times.
///
/// Every attempt carries one identity: the `(RunId, EffectSeq)` the schedule record
/// committed. [`DurableIntent::id`] returns that pair. It is the only value a downstream
/// system can deduplicate on.
///
/// Waymaker does **not** promise exactly-once physical side effects. No setting changes
/// this. The engine cannot promise it: the world changes before the record of it is durable.
/// Two ways give exactly-once, and both are outside this engine — make the activity
/// idempotent, or deduplicate on the identity downstream. An activity that does neither
/// repeats its effect on each attempt.
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
    /// than once, with no limit, and it carries the same identity every time.
    /// [`Activities`] states what this engine does not promise.
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

/// The clocks a driver may read.
///
/// Design document §11's other half of the world. An activity changes something outside the
/// device; a clock tells the device what time it is. Both are the world's, so both are
/// declared here, and a driver needs the two together — which is what
/// [`Driver::boot`](crate::Driver::boot)'s bound says.
///
/// # What an implementor must uphold
///
/// * A reading is in that clock's own unit, and the same unit across reboots. The kernel
///   compares readings; it never converts them.
/// * [`capability`](Self::capability) must be what the firmware really has. Declaring
///   [`ClockCapability::Persistent`] without a clock that survives power loss is the one
///   substitution §02 decision 8 exists to forbid, and no code below can catch it.
/// * A reading that cannot be trusted is [`None`]. An implementor must not substitute a
///   value: a zero fires every persistent deadline at once, and a maximum fires none.
/// * A persistent reading must not go backwards. Where it can — a battery change, a
///   re-synchronised epoch — the kernel refuses with
///   [`KernelError::ClockWentBackwards`](waymaker_core::KernelError::ClockWentBackwards)
///   rather than crediting an interval it cannot measure.
pub trait Clocks {
    /// Which clocks this firmware can service.
    ///
    /// # Postconditions
    ///
    /// The same value for the life of a boot. A firmware whose capability changed mid-run
    /// would admit a deadline at one boundary and refuse it at the next.
    fn capability(&self) -> ClockCapability;

    /// The current reading of `kind`'s clock, or [`None`] if it cannot be read.
    ///
    /// This is the only thing the synchronous driver does to timing hardware, which is what
    /// makes "replay of a fired timer re-arms nothing" observable: a boot that answers a
    /// deadline from history calls this zero times.
    ///
    /// # Postconditions
    ///
    /// [`None`] for a kind this firmware cannot service, and for a read that failed. The
    /// driver reports either as [`DriveError::ClockUnavailable`](crate::DriveError).
    fn now(&mut self, kind: ClockKind) -> Option<u64>;
}
