//! The world's half of the boundary, as an async dispatcher.
//!
//! Design document §13. The dispatcher performs design document §07 step 4 and nothing
//! else. It never touches media and never decides what a record means.
//!
//! [`wiring`](crate::wiring) is the table that reaches one row of it by number.

use core::task::{Context, Poll};

use waymaker_core::{ActivityKind, EffectId};

/// What an activity produced, in the buffer it was handed.
///
/// Two shapes, because design document §09 gives `EffectFailed` a bounded payload. Before
/// issue [#36](https://github.com/madmax983/waymaker/issues/36) a dispatcher answered
/// `Ok(len)` or `Err(E)`, so it could report bytes or failure and never both.
///
/// `len` is the answer's **whole** length, even when it is wider than the buffer. A length
/// over the run's declared bound is recorded as a failure with no payload; a short answer
/// reported as complete is recorded, and every replay of the run returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Produced {
    /// Success. The first `len` bytes of `out` are the answer.
    Completed(usize),
    /// Failure, with a payload: the first `len` bytes of `out`.
    Failed(usize),
}

/// What performs an activity.
///
/// # Why it is poll-shaped
///
/// §13 sketches `async fn dispatch`. A future that survives between polls must be stored.
/// The future an `async fn` in a trait returns borrows the dispatcher, and
/// [`ActivityFuture`](crate::ctx::ActivityFuture) already holds that borrow — so storing it
/// needs a self-referential value or an allocation, and this crate has neither. The poll
/// form stores nothing. [`Table`](crate::wiring::Table) is the ergonomic form over it: a
/// row per activity, and no state machine to write.
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
    /// `id` is the stable `(RunId, EffectSeq)` its schedule record committed. `kind` is a
    /// number: an activity's name is compile-time metadata for a log, and no record holds
    /// one.
    ///
    /// # Preconditions
    ///
    /// `out` is never wider than the bound the run declared for an effect result: the
    /// façade narrows the caller's buffer before this call, so an implementor cannot write
    /// past the bound. It may be *narrower*, when the caller's own buffer is — so `out.len()`
    /// is the room, and a length over it is recorded as a failure with no payload.
    ///
    /// # Postconditions
    ///
    /// [`Poll::Ready`] with `Ok` says what the activity produced. An implementor must
    /// report the answer's *whole* length even when it is wider than `out`, and must not
    /// write past `out`.
    ///
    /// [`Poll::Pending`] means the answer is not ready. The implementor registers
    /// `task`'s waker. Nothing is recorded, and the effect stays outstanding under `id`.
    /// That is how a dispatcher asks to be tried again: this crate holds no retry policy,
    /// because §16's `retry-policy-placement` is open.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the activity failed with nothing to record. The façade records
    /// an `EffectFailed` with no payload, so the run makes progress and every replay
    /// answers the same way. The error value goes no further: a workflow that branched on
    /// it would branch on something history does not hold. Keep it in the dispatcher if a
    /// log needs it. A failure the workflow *must* see is
    /// [`Produced::Failed`], whose bytes are recorded.
    fn poll_dispatch(
        &mut self,
        task: &mut Context<'_>,
        id: EffectId,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, Self::Error>>;
}
