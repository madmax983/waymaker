//! The ergonomic surface over design document §06's kernel boundary.
//!
//! Issue [#35](https://github.com/madmax983/waymaker/issues/35). A workflow written against
//! [`Ctx`] is an ordinary `async fn`: it `.await`s an activity, a deadline, or a new run,
//! and the compiler builds the state machine. This crate owns no media and keeps no global
//! state. The [`Journal`] implementation owns the media. A [`Ctx`] borrows all it uses.
//!
//! # What the futures do
//!
//! [`ActivityFuture`] is the only one that joins two halves. It asks the journal for
//! §07 steps 1 to 3, `.await`s the dispatcher for step 4, and hands the answer back for
//! steps 5 to 7. The world is reached through [`Handoff::Dispatch`] and through nothing
//! else, so an effect cannot precede its committed intent.
//!
//! # Disposable, by design
//!
//! Create the future again to recover. Within one boot a future may be retained and polled
//! normally; after a reset the future is gone, the workflow is built again, and replay
//! rebuilds what it observed. No future here holds data that must survive a reset.
//!
//! # The executor is not ours, and neither are the wakeups
//!
//! These are plain [`Future`]s. Embassy's executor polls them. This crate has no executor,
//! no timer queue and no waker of its own, and it registers no waker itself. What it does
//! is *plumb* the task's waker through to [`ActivityDispatcher::poll_dispatch`], which is
//! the one place that knows when the world will answer.
//!
//! Two paths therefore register nothing. A [`Halted`] run registers nothing because the
//! boot is over: the caller that drove it looks at the journal next. A deadline that has
//! not passed registers nothing because there is no in-boot sleep yet — [`Journal::wait`]
//! is asked again on the next poll, and issue
//! [#110](https://github.com/madmax983/waymaker/issues/110)'s in-boot sleep is where a
//! hardware alarm arrives.

use core::convert::Infallible;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context as Task, Poll};

use waymaker_core::timer::TimerSpec;
use waymaker_core::{ActivityKind, EffectId, Outcome};

use crate::decode::Decode;
use crate::dispatch::{ActivityDispatcher, Produced};
use crate::journal::{Answer, Halted, Handoff, Journal};

/// Why an activity did not yield a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Failure<T> {
    /// The effect failed. Its payload is the first `len` bytes of [`Ctx::payload`].
    ///
    /// Read it before the next boundary, which overwrites the buffer.
    Activity {
        /// How much of the caller's buffer the failure payload fills.
        len: usize,
    },
    /// The recorded answer is not a value of the type the workflow asked for.
    ///
    /// The same bytes replay on every boot, so this call fails the same way every time.
    Decode(T),
}

/// What a workflow said its run ends with.
///
/// Three answers rather than two. "The payload does not fit" is not "the run has not
/// ended". A caller that could not tell them apart would record a completion for a run that
/// asked to fail, which is what this type prevents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conclusion<'a> {
    /// The run ended. These are its terminal bytes.
    Ended(Outcome<'a>),
    /// The run asked to end with a payload wider than the caller's buffer.
    ///
    /// Refused rather than truncated: a short terminal record replays for ever. The caller
    /// must not write a terminal record for this run.
    Refused,
}

/// How a run ended, and how much of the caller's buffer it ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Ending {
    Completed(usize),
    Failed(usize),
    Refused,
}

/// Copies as much of `src` into `dst` as fits, and says how much that was.
fn copy(src: &[u8], dst: &mut [u8]) -> usize {
    let taken = src.len().min(dst.len());
    let (Some(from), Some(into)) = (src.get(..taken), dst.get_mut(..taken)) else {
        return 0;
    };
    into.copy_from_slice(from);
    taken
}

/// What a workflow asks of the engine.
///
/// It borrows three things and owns none of them: the journal that holds authority, the
/// dispatcher that reaches the world, and one buffer. Every borrowed byte a workflow sees
/// points into that buffer, and the next boundary overwrites it.
#[derive(Debug)]
pub struct Ctx<'a, D: ActivityDispatcher, J: Journal> {
    journal: &'a mut J,
    dispatcher: &'a mut D,
    out: &'a mut [u8],
    payload: usize,
    conclusion: Option<Ending>,
}

impl<'a, D: ActivityDispatcher, J: Journal> Ctx<'a, D, J> {
    /// A context over `journal`, `dispatcher` and `out`.
    ///
    /// `out` must be at least as wide as the **wider** of the run's two declared bounds:
    /// its effect-result bound and its terminal bound. An activity answer over the first is
    /// recorded as [`Answer::Exhausted`] and replays for ever; a terminal payload over the
    /// second is [`Conclusion::Refused`] and the run cannot end. Both bounds are §10's, and
    /// neither is one this crate can read.
    pub const fn new(journal: &'a mut J, dispatcher: &'a mut D, out: &'a mut [u8]) -> Self {
        Self {
            journal,
            dispatcher,
            out,
            payload: 0,
            conclusion: None,
        }
    }

    /// Run `kind` over `input` and decode the answer.
    ///
    /// The returned future resolves once the outcome is committed, and never earlier.
    #[must_use]
    pub fn activity<'b, T: Decode>(
        &'b mut self,
        kind: ActivityKind,
        input: &'b [u8],
    ) -> ActivityFuture<'b, T, D, J> {
        ActivityFuture {
            journal: self.journal,
            dispatcher: self.dispatcher,
            out: self.out,
            payload: &mut self.payload,
            concluded: &self.conclusion,
            kind,
            input,
            stage: Stage::Scheduling,
            decoded: PhantomData,
        }
    }

    /// Ask whether `spec`'s deadline has passed.
    ///
    /// If it has not, the run stops for this boot. The future asks again on every poll, so
    /// a caller that drives more than one poll per boot sees the deadline pass.
    ///
    /// It carries no dispatcher. A deadline is not an activity: §11 measures it against a
    /// clock the journal reads, and nothing outside the device is asked.
    #[must_use]
    pub const fn timer(&mut self, spec: TimerSpec) -> TimerFuture<'_, J> {
        TimerFuture {
            journal: self.journal,
            concluded: &self.conclusion,
            spec,
            ended: false,
        }
    }

    /// Retire this run and start a new one over `input`.
    ///
    /// §10. The future never resolves — its output type has no value — because the run that
    /// asked is over either way.
    #[must_use]
    pub const fn continue_as_new<'b>(&'b mut self, input: &'b [u8]) -> ContinueFuture<'b, J> {
        ContinueFuture {
            journal: self.journal,
            concluded: &self.conclusion,
            input,
            asked: false,
        }
    }

    /// End the run successfully, with `result` as its terminal payload.
    ///
    /// It writes no record. The terminal record is the journal's, and the caller that drove
    /// the boot reads [`conclusion`](Self::conclusion) and writes it. That is what keeps
    /// on-media authority out of this crate.
    ///
    /// The returned future never resolves: the run is over, so nothing after it runs.
    #[must_use]
    pub fn complete<'b, E>(&'b mut self, result: &'b [u8]) -> TerminalFuture<'b, E> {
        self.ending(result, false)
    }

    /// End the run with a failure payload.
    #[must_use]
    pub fn fail<'b, E>(&'b mut self, error: &'b [u8]) -> TerminalFuture<'b, E> {
        self.ending(error, true)
    }

    fn ending<'b, E>(&'b mut self, bytes: &'b [u8], failed: bool) -> TerminalFuture<'b, E> {
        TerminalFuture {
            out: self.out,
            conclusion: &mut self.conclusion,
            bytes,
            failed,
            ended: false,
            error: PhantomData,
        }
    }

    /// What the run ends with, or [`None`] if it has not ended.
    ///
    /// A refused payload answers [`Conclusion::Refused`] and never [`None`]. The two are
    /// different runs: one did not finish, the other asked to finish with bytes the caller
    /// cannot carry.
    #[must_use]
    pub fn conclusion(&self) -> Option<Conclusion<'_>> {
        match self.conclusion? {
            Ending::Completed(len) => {
                Some(Conclusion::Ended(Outcome::Completed(self.out.get(..len)?)))
            }
            Ending::Failed(len) => Some(Conclusion::Ended(Outcome::Failed(self.out.get(..len)?))),
            Ending::Refused => Some(Conclusion::Refused),
        }
    }

    /// The last boundary's payload.
    ///
    /// The bytes behind [`Failure::Activity`]. Valid until the next boundary.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        self.out.get(..self.payload).unwrap_or_default()
    }
}

/// Where an activity boundary has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// §07 steps 1 to 3 have not been asked for yet.
    Scheduling,
    /// The intent is durable. Step 4 is legal, under this identity, for an answer no
    /// wider than `result_bytes`.
    Dispatching {
        /// The identity the schedule record committed.
        id: EffectId,
        /// §10's `effect_result_bytes`, as the journal stated it.
        result_bytes: usize,
    },
    /// The boundary is over. A further poll asks nothing.
    Ended,
}

/// One activity boundary: §07 steps 1 to 3, then the world, then steps 5 to 7.
#[derive(Debug)]
pub struct ActivityFuture<'b, T, D: ActivityDispatcher, J: Journal> {
    journal: &'b mut J,
    dispatcher: &'b mut D,
    out: &'b mut [u8],
    payload: &'b mut usize,
    concluded: &'b Option<Ending>,
    kind: ActivityKind,
    input: &'b [u8],
    stage: Stage,
    decoded: PhantomData<fn() -> T>,
}

/// The workflow's value, once the outcome is committed.
///
/// A free function rather than a method. Separate arguments keep the three borrows
/// disjoint: `outcome` borrows the journal, `out` and `payload` are two other fields. A
/// method would borrow all of `self`, and none of them would be usable.
fn observed<T: Decode>(
    outcome: Outcome<'_>,
    out: &mut [u8],
    payload: &mut usize,
) -> Result<T, Failure<T::Error>> {
    let (bytes, completed) = match outcome {
        Outcome::Completed(bytes) => (bytes, true),
        Outcome::Failed(bytes) => (bytes, false),
    };
    // Copied first, so a failure payload outlives the borrow the journal answered in.
    let len = copy(bytes, out);
    *payload = len;
    if completed {
        T::decode(bytes).map_err(Failure::Decode)
    } else {
        Err(Failure::Activity { len })
    }
}

/// What the journal is told, for a length the world reported against `room`.
///
/// A free function, for [`observed`]'s reason: it takes the buffer rather than `self`, so
/// the journal borrow beside it stays disjoint.
fn answered(produced: Produced, out: &[u8], room: usize) -> Answer<'_> {
    let (len, completed) = match produced {
        Produced::Completed(len) => (len, true),
        Produced::Failed(len) => (len, false),
    };
    if len > room {
        return Answer::Exhausted;
    }
    let Some(bytes) = out.get(..len) else {
        // Unreachable: `room` is at most `out.len()`. Refused rather than panicked.
        return Answer::Exhausted;
    };
    if completed {
        Answer::Completed(bytes)
    } else {
        Answer::Failed(bytes)
    }
}

impl<T: Decode, D: ActivityDispatcher, J: Journal> Future for ActivityFuture<'_, T, D, J> {
    type Output = Result<T, Failure<T::Error>>;

    fn poll(self: Pin<&mut Self>, task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        // A run that has recorded its ending has no boundaries left. `TerminalFuture` never
        // resolves, so an `async fn` stops there on its own; this is the same statement for
        // a caller that reaches a boundary without going through `.await`, and it is what
        // stops the recorded ending — which points into `out` — being overwritten.
        if me.concluded.is_some() {
            return Poll::Pending;
        }
        loop {
            match me.stage {
                // A boundary that is over answers nothing more. The run stops here, and it
                // is the caller of the boot that acts on it.
                Stage::Ended => return Poll::Pending,
                Stage::Scheduling => match me.journal.schedule(me.kind, me.input) {
                    // Nothing was committed and nothing was consumed, so the stage does not
                    // move: an executor that polls again asks again. A journal that has
                    // stopped answers the same way, and one that has not may have moved on.
                    Err(Halted) => return Poll::Pending,
                    Ok(Handoff::Replayed(recorded)) => {
                        me.stage = Stage::Ended;
                        return Poll::Ready(observed(recorded, me.out, me.payload));
                    }
                    // The intent is durable. Only this value reaches the world.
                    Ok(Handoff::Dispatch { id, result_bytes }) => {
                        me.stage = Stage::Dispatching { id, result_bytes };
                    }
                },
                Stage::Dispatching { id, result_bytes } => {
                    // The room, not the caller's whole buffer. `out` is the wider of the
                    // run's two bounds, so a world handed all of it could write an answer
                    // the run cannot record. Narrowing here is issue #36's "validated
                    // against the bound" as an impossibility rather than a check — and it
                    // is the smaller of the two figures, so a caller that undersized `out`
                    // still cannot be written past.
                    let room = result_bytes.min(me.out.len());
                    let Some(into) = me.out.get_mut(..room) else {
                        // Unreachable: `room` is at most `out.len()`. Refused rather than
                        // panicked, because the workspace denies both.
                        return Poll::Pending;
                    };
                    let dispatched = me
                        .dispatcher
                        .poll_dispatch(task, id, me.kind, me.input, into);
                    let answer = match dispatched {
                        // The world asked to be tried again. Nothing is recorded, so the
                        // effect stays outstanding under the identity it was committed with.
                        Poll::Pending => return Poll::Pending,
                        // The activity failed with nothing to record. It is recorded as a
                        // failure with no payload, so the run makes progress and every
                        // replay answers the same way. The error value stops here: a
                        // workflow that branched on it would branch on something no replay
                        // can reproduce.
                        Poll::Ready(Err(_dropped)) => Answer::Failed(&[]),
                        // A length over the bound is recorded as a failure with no payload:
                        // a truncation replays a wrong answer for ever, and a refusal
                        // strands the run. Both shapes are bounded by the same figure.
                        Poll::Ready(Ok(produced)) => answered(produced, me.out, room),
                    };
                    me.stage = Stage::Ended;
                    return match me.journal.resolve(answer) {
                        Err(Halted) => Poll::Pending,
                        Ok(committed) => Poll::Ready(observed(committed, me.out, me.payload)),
                    };
                }
            }
        }
    }
}

/// One deadline boundary.
#[derive(Debug)]
pub struct TimerFuture<'b, J: Journal> {
    journal: &'b mut J,
    concluded: &'b Option<Ending>,
    spec: TimerSpec,
    ended: bool,
}

impl<J: Journal> Future for TimerFuture<'_, J> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        if me.ended || me.concluded.is_some() {
            return Poll::Pending;
        }
        // The deadline is asked again on every poll until it passes. Ending here would make
        // a retained timer future one that can never make progress within a boot, which is
        // the opposite of what §06 says a future may be.
        match me.journal.wait(me.spec) {
            Ok(()) => {
                me.ended = true;
                Poll::Ready(())
            }
            Err(Halted) => Poll::Pending,
        }
    }
}

/// §10's `continue_as_new`, as a future that never resolves.
///
/// [`Infallible`] has no value, so the code after the `.await` is unreachable rather than
/// merely unlikely. That is the shape of the operation: the run that asked is replaced.
#[derive(Debug)]
pub struct ContinueFuture<'b, J: Journal> {
    journal: &'b mut J,
    concluded: &'b Option<Ending>,
    input: &'b [u8],
    asked: bool,
}

impl<J: Journal> Future for ContinueFuture<'_, J> {
    type Output = Infallible;

    fn poll(self: Pin<&mut Self>, _task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        if !me.asked && me.concluded.is_none() {
            me.asked = true;
            let Halted = me.journal.continue_as_new(me.input);
        }
        Poll::Pending
    }
}

/// The run's own ending, recorded for the caller that drove the boot.
///
/// `E` is the workflow's error type, so that `ctx.complete(&[]).await` is the tail of a
/// function returning `Result<(), E>`. This future never produces one, and never resolves:
/// the run that asked is over, exactly as it is for [`ContinueFuture`].
///
/// # Why it does not resolve
///
/// The recorded ending points into the caller's buffer, and that buffer is where the next
/// boundary writes. A future that resolved would let a workflow record its ending and then
/// perform an activity, and the run would be committed with the activity's bytes as its
/// terminal payload. §08 has no edge from a terminal record to another boundary either, so
/// stopping here is the protocol rather than a guard over it. Codex round 2 found the
/// version that resolved.
#[derive(Debug)]
pub struct TerminalFuture<'b, E> {
    out: &'b mut [u8],
    conclusion: &'b mut Option<Ending>,
    bytes: &'b [u8],
    failed: bool,
    ended: bool,
    error: PhantomData<fn() -> E>,
}

impl<E> Future for TerminalFuture<'_, E> {
    type Output = Result<(), E>;

    fn poll(self: Pin<&mut Self>, _task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        if !me.ended {
            me.ended = true;
            // Refused rather than truncated when it does not fit: a short terminal record
            // replays for ever. The refusal is recorded, so the caller cannot read it as a
            // run that never ended and complete it with nothing.
            *me.conclusion = Some(if me.bytes.len() > me.out.len() {
                Ending::Refused
            } else if me.failed {
                Ending::Failed(copy(me.bytes, me.out))
            } else {
                Ending::Completed(copy(me.bytes, me.out))
            });
        }
        // The run is over. Nothing after this runs, so nothing can overwrite the buffer the
        // ending points into. The caller that drove the boot reads
        // [`Ctx::conclusion`](Ctx::conclusion), which is why this needs no value.
        Poll::Pending
    }
}
