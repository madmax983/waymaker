//! The world's half of the boundary, as an async dispatcher.
//!
//! Design document §13. The dispatcher performs design document §07 step 4 and nothing
//! else. It never touches media and never decides what a record means.

use core::task::{Context, Poll};

use waymaker_core::{ActivityKind, EffectId};

/// What performs an activity.
///
/// # Why it is poll-shaped
///
/// §13 sketches `async fn dispatch`. A future that survives between polls must be stored.
/// You cannot name the type an `async fn` in a trait returns, so
/// [`ActivityFuture`](crate::ctx::ActivityFuture) cannot store one without an allocation.
/// The poll form stores nothing. Issue
/// [#36](https://github.com/madmax983/waymaker/issues/36) owns the ergonomic wrapper over
/// it.
///
/// # At-least-once
///
/// One `id` can arrive more than once: after a retry, and after a reset between the effect
/// and its committed outcome. An implementor must tolerate that. Waymaker promises the
/// identity and nothing more. Exactly-once needs an idempotent activity or a downstream
/// system that deduplicates `id`.
pub trait ActivityDispatcher {
    /// Why a dispatch could not be completed.
    type Error;

    /// Perform `kind` over `input` for `id`, writing the answer into `out`.
    ///
    /// `id` is the stable `(RunId, EffectSeq)` its schedule record committed.
    ///
    /// # Postconditions
    ///
    /// [`Poll::Ready`] with `Ok(len)` means the first `len` bytes of `out` are the answer.
    /// An implementor must report the answer's *whole* length even when it is wider than
    /// `out`, and must not write past `out`. A short answer reported as complete is
    /// recorded, and every replay of the run returns it.
    ///
    /// [`Poll::Pending`] means the answer is not ready. The implementor registers
    /// `task`'s waker. Nothing is recorded, and the effect stays outstanding under `id`.
    /// That is how a dispatcher asks to be tried again: this crate holds no retry policy,
    /// because §16's `retry-policy-placement` is open.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the activity failed. The façade records an `EffectFailed` with
    /// no payload, so the run makes progress and every replay answers the same way. The
    /// error value goes no further: a workflow that branched on it would branch on
    /// something history does not hold. Keep it in the dispatcher if a log needs it.
    ///
    /// A *typed* failure payload has no route through this trait. `Ok(len)` is recorded as
    /// an `EffectCompleted`, so an activity cannot report bytes and failure together. §09
    /// gives `EffectFailed` a bounded payload and this signature does not, which is issue
    /// [#36](https://github.com/madmax983/waymaker/issues/36)'s to close.
    fn poll_dispatch(
        &mut self,
        task: &mut Context<'_>,
        id: EffectId,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<usize, Self::Error>>;
}
