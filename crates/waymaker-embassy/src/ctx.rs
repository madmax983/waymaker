//! The ergonomic surface over design document §06's kernel boundary.
//!
//! Issue [#35](https://github.com/madmax983/waymaker/issues/35). A workflow written against
//! [`Ctx`] is an ordinary `async fn`: it `.await`s an activity, a deadline, or a new run,
//! and the compiler builds the state machine. Nothing here owns media and nothing here
//! keeps global state — a [`Journal`] holds the first and a [`Ctx`] borrows everything it
//! touches.
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
//! Within one boot a future may be retained and polled normally. After a reset the future
//! is gone; the workflow is created again and replay reconstructs its observable state.
//! Recreating it *is* the recovery mechanism, so no future here holds anything a reset must
//! not take.
//!
//! # The executor is not ours
//!
//! These are plain [`Future`]s. Embassy's executor polls them; this crate has no executor,
//! no timer queue and no waker of its own. A [`Halted`] run registers no waker at all,
//! because the boot is over and the caller that drove it is what looks next.

use core::convert::Infallible;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context as Task, Poll};

use waymaker_core::timer::TimerSpec;
use waymaker_core::{ActivityKind, EffectId, Outcome};

use crate::decode::Decode;
use crate::dispatch::ActivityDispatcher;
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

/// How a run ended, and how much of the caller's buffer it ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Ending {
    Completed(usize),
    Failed(usize),
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
    stalled: Option<D::Error>,
}

impl<'a, D: ActivityDispatcher, J: Journal> Ctx<'a, D, J> {
    /// A context over `journal`, `dispatcher` and `out`.
    ///
    /// `out` must be at least as wide as the run's declared effect-result bound. A narrower
    /// buffer makes every wider answer [`Answer::Exhausted`], which the run records and
    /// replays for ever.
    pub const fn new(journal: &'a mut J, dispatcher: &'a mut D, out: &'a mut [u8]) -> Self {
        Self {
            journal,
            dispatcher,
            out,
            payload: 0,
            conclusion: None,
            stalled: None,
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
            stalled: &mut self.stalled,
            kind,
            input,
            stage: Stage::Scheduling,
            decoded: PhantomData,
        }
    }

    /// Wait until `spec`'s deadline has passed.
    ///
    /// It carries no dispatcher. A deadline is not an activity: §11 measures it against a
    /// clock the journal reads, and nothing outside the device is asked.
    #[must_use]
    pub const fn timer(&mut self, spec: TimerSpec) -> TimerFuture<'_, J> {
        TimerFuture {
            journal: self.journal,
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
            input,
            asked: false,
        }
    }

    /// End the run successfully, with `result` as its terminal payload.
    ///
    /// It writes no record. The terminal record is the journal's, and the caller that drove
    /// the boot reads [`conclusion`](Self::conclusion) and writes it. That is what keeps
    /// on-media authority out of this crate.
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

    /// What the run ended with, or [`None`] if it has not ended.
    ///
    /// [`None`] also answers a terminal payload wider than `out`. A short terminal record
    /// would replay for ever, so it is refused rather than truncated, and the caller sees a
    /// run that never concluded.
    #[must_use]
    pub fn conclusion(&self) -> Option<Outcome<'_>> {
        match self.conclusion? {
            Ending::Completed(len) => Some(Outcome::Completed(self.out.get(..len)?)),
            Ending::Failed(len) => Some(Outcome::Failed(self.out.get(..len)?)),
        }
    }

    /// The last boundary's payload.
    ///
    /// The bytes behind [`Failure::Activity`]. Valid until the next boundary.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        self.out.get(..self.payload).unwrap_or_default()
    }

    /// The last dispatcher error, for a log.
    ///
    /// It is diagnosis and not control flow. History records that the effect failed and
    /// nothing about why, so a workflow that branched on this would branch on something no
    /// replay can reproduce.
    #[must_use]
    pub const fn dispatch_error(&self) -> Option<&D::Error> {
        self.stalled.as_ref()
    }
}

/// Where an activity boundary has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// §07 steps 1 to 3 have not been asked for yet.
    Scheduling,
    /// The intent is durable. Step 4 is legal, under this identity.
    Dispatching(EffectId),
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
    stalled: &'b mut Option<D::Error>,
    kind: ActivityKind,
    input: &'b [u8],
    stage: Stage,
    decoded: PhantomData<fn() -> T>,
}

/// The workflow's value, once the outcome is committed.
///
/// A free function rather than a method: `outcome` borrows the journal, and `out` and
/// `payload` are the other two fields, so the three borrows are disjoint only while they
/// stay apart.
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

impl<T: Decode, D: ActivityDispatcher, J: Journal> Future for ActivityFuture<'_, T, D, J> {
    type Output = Result<T, Failure<T::Error>>;

    fn poll(self: Pin<&mut Self>, task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        loop {
            match me.stage {
                // A boundary that is over answers nothing more. The run stops here, and it
                // is the caller of the boot that acts on it.
                Stage::Ended => return Poll::Pending,
                Stage::Scheduling => match me.journal.schedule(me.kind, me.input) {
                    Err(Halted) => {
                        me.stage = Stage::Ended;
                        return Poll::Pending;
                    }
                    Ok(Handoff::Replayed(recorded)) => {
                        me.stage = Stage::Ended;
                        return Poll::Ready(observed(recorded, me.out, me.payload));
                    }
                    // The intent is durable. Only this value reaches the world.
                    Ok(Handoff::Dispatch(id)) => me.stage = Stage::Dispatching(id),
                },
                Stage::Dispatching(id) => {
                    let dispatched = me
                        .dispatcher
                        .poll_dispatch(task, id, me.kind, me.input, me.out);
                    let answer = match dispatched {
                        // The world asked to be tried again. Nothing is recorded, so the
                        // effect stays outstanding under the identity it was committed with.
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Err(error)) => {
                            *me.stalled = Some(error);
                            Answer::Failed(&[])
                        }
                        // A length over the buffer is recorded as a failure with no
                        // payload: a truncation replays a wrong answer for ever, and a
                        // refusal strands the run.
                        Poll::Ready(Ok(produced)) => me
                            .out
                            .get(..produced)
                            .map_or(Answer::Exhausted, Answer::Completed),
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
    spec: TimerSpec,
    ended: bool,
}

impl<J: Journal> Future for TimerFuture<'_, J> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        if me.ended {
            return Poll::Pending;
        }
        me.ended = true;
        match me.journal.wait(me.spec) {
            Ok(()) => Poll::Ready(()),
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
    input: &'b [u8],
    asked: bool,
}

impl<J: Journal> Future for ContinueFuture<'_, J> {
    type Output = Infallible;

    fn poll(self: Pin<&mut Self>, _task: &mut Task<'_>) -> Poll<Self::Output> {
        let me = self.get_mut();
        if !me.asked {
            me.asked = true;
            let Halted = me.journal.continue_as_new(me.input);
        }
        Poll::Pending
    }
}

/// The run's own ending, recorded for the caller that drove the boot.
///
/// `E` is the workflow's error type, so that `ctx.complete(&[]).await` is the tail of a
/// function returning `Result<(), E>`. This future never produces one.
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
            // replays for ever, and the caller sees a run that never concluded.
            if me.bytes.len() <= me.out.len() {
                let len = copy(me.bytes, me.out);
                *me.conclusion = Some(if me.failed {
                    Ending::Failed(len)
                } else {
                    Ending::Completed(len)
                });
            }
        }
        Poll::Ready(Ok(()))
    }
}
