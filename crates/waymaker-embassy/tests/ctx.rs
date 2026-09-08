//! The façade, tested against a journal that records what it was asked.
//!
//! These tests are about the *sequencing* the façade adds, not about media. Whether a
//! record is durable is `waymaker-flash`'s, and whether §08 admits it is `waymaker-core`'s.
//! What is here is that the façade dispatches only after the journal says the intent is
//! durable, that it dispatches nothing on replay, and that it records what the world
//! answered before the workflow sees it.
//!
//! The end-to-end run over real media is `crates/waymaker-drive/tests/ota.rs`.

use core::future::Future;
use core::pin::pin;
use core::task::{Context as Task, Poll, Waker};
use std::sync::Arc;
use std::task::Wake;

use waymaker_core::timer::{ClockKind, TimerSpec};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, Outcome, RunId};
use waymaker_embassy::ctx::{Conclusion, Ctx, Failure};
use waymaker_embassy::dispatch::Produced;
use waymaker_embassy::{ActivityDispatcher, Answer, Decode, Halted, Handoff, Journal};

const RUN: RunId = RunId(9);
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// What the journal was asked to do, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Schedule(ActivityKind, Vec<u8>),
    Resolve(Answered),
    Wait(TimerSpec),
    ContinueAsNew(Vec<u8>),
}

/// [`Answer`] without a borrow, so a test can keep the log.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Answered {
    Completed(Vec<u8>),
    Failed(Vec<u8>),
    Exhausted,
}

/// A journal that answers from a script and keeps what it was asked.
///
/// It writes nothing: what a real journal does with a record is tested where the records
/// are. This one only has to be a faithful *sequence*.
struct Ledger {
    /// What `schedule` answers, in order.
    handoffs: Vec<Result<Handoff<'static>, Halted>>,
    /// What `resolve` answers, in order.
    resolutions: Vec<Result<Vec<u8>, Halted>>,
    /// What `wait` answers, in order.
    waits: Vec<Result<(), Halted>>,
    asked: Vec<Asked>,
    scheduled: usize,
    resolved: usize,
    waited: usize,
    /// Where a resolved outcome is handed back from, so the borrow outlives the call.
    kept: Vec<u8>,
}

impl Ledger {
    const fn new() -> Self {
        Self {
            handoffs: Vec::new(),
            resolutions: Vec::new(),
            waits: Vec::new(),
            asked: Vec::new(),
            scheduled: 0,
            resolved: 0,
            waited: 0,
            kept: Vec::new(),
        }
    }

    fn scheduling(mut self, handoffs: Vec<Result<Handoff<'static>, Halted>>) -> Self {
        self.handoffs = handoffs;
        self
    }

    fn resolving(mut self, resolutions: Vec<Result<Vec<u8>, Halted>>) -> Self {
        self.resolutions = resolutions;
        self
    }

    fn waiting(mut self, waits: Vec<Result<(), Halted>>) -> Self {
        self.waits = waits;
        self
    }
}

fn owned(answer: Answer<'_>) -> Answered {
    match answer {
        Answer::Completed(bytes) => Answered::Completed(bytes.to_vec()),
        Answer::Failed(bytes) => Answered::Failed(bytes.to_vec()),
        Answer::Exhausted => Answered::Exhausted,
    }
}

impl Journal for Ledger {
    fn schedule(&mut self, kind: ActivityKind, input: &[u8]) -> Result<Handoff<'_>, Halted> {
        self.asked.push(Asked::Schedule(kind, input.to_vec()));
        let answer = self
            .handoffs
            .get(self.scheduled)
            .copied()
            .unwrap_or(Err(Halted));
        self.scheduled += 1;
        answer
    }

    fn resolve(&mut self, answer: Answer<'_>) -> Result<Outcome<'_>, Halted> {
        let failed = matches!(answer, Answer::Failed(_) | Answer::Exhausted);
        self.asked.push(Asked::Resolve(owned(answer)));
        let recorded = self
            .resolutions
            .get(self.resolved)
            .cloned()
            .unwrap_or(Err(Halted));
        self.resolved += 1;
        self.kept = recorded?;
        // A real journal answers with the bytes the record holds, so a failure answers
        // `Failed` and an exhausted answer holds nothing.
        Ok(if failed {
            Outcome::Failed(&self.kept)
        } else {
            Outcome::Completed(&self.kept)
        })
    }

    fn wait(&mut self, spec: TimerSpec) -> Result<(), Halted> {
        self.asked.push(Asked::Wait(spec));
        let answer = self.waits.get(self.waited).copied().unwrap_or(Err(Halted));
        self.waited += 1;
        answer
    }

    fn continue_as_new(&mut self, input: &[u8]) -> Halted {
        self.asked.push(Asked::ContinueAsNew(input.to_vec()));
        Halted
    }
}

/// A dispatcher that answers from a script and counts how often it was polled.
struct World {
    answers: Vec<Result<Vec<u8>, Fault>>,
    taken: usize,
    polls: usize,
    /// How many polls each answer waits for before it is given.
    stalls: usize,
    stalled: usize,
    /// How wide the buffer was on each poll that reached an answer.
    widths: Vec<usize>,
    /// What to report instead of the answer's own length.
    reports: Option<usize>,
    /// Whether an answer is a failure payload rather than a result.
    as_failure: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fault;

impl World {
    const fn answering(answers: Vec<Result<Vec<u8>, Fault>>) -> Self {
        Self {
            answers,
            taken: 0,
            polls: 0,
            stalls: 0,
            stalled: 0,
            widths: Vec::new(),
            reports: None,
            as_failure: false,
        }
    }

    /// Report `len` rather than the answer's own length.
    const fn reporting(mut self, len: usize) -> Self {
        self.reports = Some(len);
        self
    }

    /// Answer with a failure payload rather than a result.
    const fn failing(mut self) -> Self {
        self.as_failure = true;
        self
    }

    const fn silent() -> Self {
        Self::answering(Vec::new())
    }

    const fn stalling(mut self, polls: usize) -> Self {
        self.stalls = polls;
        self
    }
}

impl ActivityDispatcher for World {
    type Error = Fault;

    fn poll_dispatch(
        &mut self,
        _task: &mut Task<'_>,
        _id: EffectId,
        _kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, Fault>> {
        self.polls += 1;
        if self.stalled < self.stalls {
            self.stalled += 1;
            return Poll::Pending;
        }
        self.widths.push(out.len());
        let answer = self.answers.get(self.taken).cloned();
        self.taken += 1;
        match answer {
            None => Poll::Ready(Err(Fault)),
            Some(Err(fault)) => Poll::Ready(Err(fault)),
            Some(Ok(bytes)) => {
                let taken = bytes.len().min(out.len());
                let (Some(from), Some(into)) = (bytes.get(..taken), out.get_mut(..taken)) else {
                    return Poll::Ready(Err(Fault));
                };
                into.copy_from_slice(from);
                let reported = self.reports.unwrap_or(bytes.len());
                Poll::Ready(Ok(if self.as_failure {
                    Produced::Failed(reported)
                } else {
                    Produced::Completed(reported)
                }))
            }
        }
    }
}

/// The whole of a downloaded image, as a workflow keeps it: a handle, not the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NotASlot;

impl Decode for Slot {
    type Error = NotASlot;

    fn decode(bytes: &[u8]) -> Result<Self, NotASlot> {
        let four: [u8; 4] = bytes.try_into().map_err(|_| NotASlot)?;
        Ok(Self(u32::from_le_bytes(four)))
    }
}

/// Polls `future` once, the way one boot polls a workflow.
fn poll_once<F: Future>(future: F) -> Poll<F::Output> {
    let mut task = Task::from_waker(Waker::noop());
    pin!(future).poll(&mut task)
}

/// A handoff whose bound is the widest a test buffer here ever is.
const fn dispatch(seq: u32) -> Handoff<'static> {
    bounded(seq, usize::MAX)
}

/// A handoff that declares how wide an answer this run can record.
const fn bounded(seq: u32, result_bytes: usize) -> Handoff<'static> {
    Handoff::Dispatch {
        id: EffectId {
            run: RUN,
            seq: EffectSeq(seq),
        },
        result_bytes,
    }
}

#[test]
fn an_activity_is_dispatched_only_after_the_journal_says_the_intent_is_durable() {
    // §02 decision 3 at the façade. The dispatcher is reached through `Handoff::Dispatch`
    // and through nothing else, so a journal that has not committed the intent cannot
    // produce the value that reaches the world.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(dispatch(0))])
        .resolving(vec![Ok(b"\x07\x00\x00\x00".to_vec())]);
    let mut world = World::answering(vec![Ok(b"\x07\x00\x00\x00".to_vec())]);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(answered, Poll::Ready(Ok(Slot(7))));
    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Completed(b"\x07\x00\x00\x00".to_vec())),
        ]
    );
    assert_eq!(world.polls, 1);
}

#[test]
fn a_replayed_activity_reaches_the_world_zero_times() {
    // The whole reason a façade may not own authority. History answered, so the effect must
    // not happen again, and the count is the evidence rather than the absence of a record.
    let mut ledger = Ledger::new().scheduling(vec![Ok(Handoff::Replayed(Outcome::Completed(
        b"\x02\x00\x00\x00",
    )))]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(answered, Poll::Ready(Ok(Slot(2))));
    assert_eq!(world.polls, 0);
    assert_eq!(
        ledger.asked,
        vec![Asked::Schedule(DOWNLOAD, b"url".to_vec())]
    );
}

#[test]
fn a_dispatcher_that_is_not_ready_records_nothing_and_leaves_the_effect_outstanding() {
    // The façade has no retry policy: §16's `retry-policy-placement` is open. A world that
    // cannot answer now leaves the intent committed and the outcome unwritten, which is
    // what makes the next pass a redelivery under the same identity.
    let mut ledger = Ledger::new().scheduling(vec![Ok(dispatch(0))]);
    let mut world = World::silent().stalling(1);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(answered, Poll::Pending);
    assert_eq!(
        ledger.asked,
        vec![Asked::Schedule(DOWNLOAD, b"url".to_vec())]
    );
}

#[test]
fn a_dispatcher_that_fails_records_a_failure_with_no_payload_and_keeps_the_typed_error() {
    // A failed activity has to reach media, or §08 strands the run: there is no edge from
    // an unresolved effect to a terminal record. The error value itself cannot, because a
    // replay could not reproduce it, so it stays where a log can read it.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(dispatch(0))])
        .resolving(vec![Ok(Vec::new())]);
    let mut world = World::answering(vec![Err(Fault)]);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(answered, Poll::Ready(Err(Failure::Activity { len: 0 })));
    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Failed(Vec::new())),
        ]
    );
}

#[test]
fn an_answer_wider_than_the_buffer_is_exhausted_rather_than_truncated() {
    // The façade's buffer is the caller's, and a world that reported more than it fits is
    // the same statement `Performed::Exhausted` makes one crate over. Recording the short
    // prefix would replay a wrong answer for ever.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(dispatch(0))])
        .resolving(vec![Ok(Vec::new())]);
    let mut world = World::answering(vec![Ok(b"far too many bytes".to_vec())]);
    let mut out = [0_u8; 4];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Exhausted),
        ]
    );
    assert_eq!(answered, Poll::Ready(Err(Failure::Activity { len: 0 })));
}

#[test]
fn a_failed_effect_is_an_error_and_its_payload_stays_in_the_callers_buffer() {
    let mut ledger =
        Ledger::new().scheduling(vec![Ok(Handoff::Replayed(Outcome::Failed(b"nope")))]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(answered, Poll::Ready(Err(Failure::Activity { len: 4 })));
    assert_eq!(ctx.payload(), b"nope");
}

#[test]
fn a_halted_journal_stops_the_workflow_and_reaches_no_dispatcher() {
    // A halt is this boot ending. The world is never asked, and no waker is registered:
    // there is nothing left to wake, and the caller that drove the boot looks at the
    // journal next.
    let mut ledger = Ledger::new().scheduling(vec![Err(Halted)]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(answered, Poll::Pending);
    assert_eq!(world.polls, 0);
}

#[test]
fn a_schedule_that_halted_is_asked_again_on_the_next_poll() {
    // Nothing was committed and nothing was consumed, so the boundary has not happened.
    // Ending it here would make a retained future one that can never make progress.
    let mut ledger = Ledger::new().scheduling(vec![
        Err(Halted),
        Ok(Handoff::Replayed(Outcome::Completed(b"\x04\x00\x00\x00"))),
    ]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let mut future = pin!(ctx.activity::<Slot>(DOWNLOAD, b"url"));
    let mut task = Task::from_waker(Waker::noop());
    let first = future.as_mut().poll(&mut task);
    let second = future.as_mut().poll(&mut task);

    assert_eq!(first, Poll::Pending);
    assert_eq!(second, Poll::Ready(Ok(Slot(4))));
}

#[test]
fn a_deadline_that_has_not_passed_is_asked_again_on_the_next_poll() {
    // §11's deadline is the one case an in-boot wakeup exists for, and this crate has none
    // yet. Asking again on every poll is what a retained timer future needs instead.
    let spec = TimerSpec::AfterBoot { ticks: 25 };
    let mut ledger = Ledger::new().waiting(vec![Err(Halted), Ok(())]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let mut future = pin!(ctx.timer(spec));
    let mut task = Task::from_waker(Waker::noop());
    let first = future.as_mut().poll(&mut task);
    let second = future.as_mut().poll(&mut task);

    assert_eq!(first, Poll::Pending);
    assert_eq!(second, Poll::Ready(()));
    assert_eq!(ledger.asked, vec![Asked::Wait(spec), Asked::Wait(spec)]);
}

#[test]
fn a_timer_reaches_the_journal_and_never_the_dispatcher() {
    // §11's deadline is not an activity. That is why `TimerFuture` carries no dispatcher.
    let spec = TimerSpec::AfterBoot { ticks: 25 };
    let mut ledger = Ledger::new().waiting(vec![Ok(())]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let waited = poll_once(ctx.timer(spec));

    assert_eq!(waited, Poll::Ready(()));
    assert_eq!(ledger.asked, vec![Asked::Wait(spec)]);
    assert_eq!(world.polls, 0);
}

#[test]
fn a_deadline_that_has_not_passed_suspends_the_run() {
    let spec = TimerSpec::AtPersistentTime { instant: 900 };
    let mut ledger = Ledger::new().waiting(vec![Err(Halted)]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let waited = poll_once(ctx.timer(spec));

    assert_eq!(waited, Poll::Pending);
    assert_eq!(spec.clock_kind(), ClockKind::AT_PERSISTENT_TIME);
}

#[test]
fn continue_as_new_never_returns_to_the_run_that_asked_for_it() {
    // §10 replaces the run. There is nothing for this future to resolve to, and its output
    // type says so: the code after it is unreachable rather than merely unlikely.
    let mut ledger = Ledger::new();
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let restarted = poll_once(ctx.continue_as_new(b"next"));

    assert!(restarted.is_pending());
    assert_eq!(ledger.asked, vec![Asked::ContinueAsNew(b"next".to_vec())]);
}

#[test]
fn a_terminal_call_records_no_record_and_leaves_the_conclusion_for_the_caller() {
    // `complete` is sugar. The terminal record is the driver's, so the façade only says
    // what the run ended with — which is what keeps on-media authority out of this crate.
    let mut ledger = Ledger::new();
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let ended: Poll<Result<(), Fault>> = poll_once(ctx.complete(b"done"));

    // The run is over, so the future never resolves. What it ended with is the conclusion.
    assert_eq!(ended, Poll::Pending);
    assert_eq!(
        ctx.conclusion(),
        Some(Conclusion::Ended(Outcome::Completed(b"done")))
    );
    assert!(ledger.asked.is_empty());
}

#[test]
fn a_failing_run_ends_with_a_failure_payload() {
    let mut ledger = Ledger::new();
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let ended: Poll<Result<(), Fault>> = poll_once(ctx.fail(b"bad"));

    assert_eq!(ended, Poll::Pending);
    assert_eq!(
        ctx.conclusion(),
        Some(Conclusion::Ended(Outcome::Failed(b"bad")))
    );
}

#[test]
fn a_run_that_has_not_ended_has_no_conclusion() {
    let mut ledger = Ledger::new();
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    assert_eq!(ctx.conclusion(), None);
    assert_eq!(ctx.payload(), b"");
}

#[test]
fn a_terminal_payload_wider_than_the_buffer_is_refused_rather_than_truncated() {
    // The buffer is the caller's and the bound is the run's. A short terminal record that
    // replayed for ever is the same defect an exhausted effect has, so it is refused.
    //
    // `Refused` rather than `None`. The two are different runs — one did not finish, the
    // other asked to finish with bytes the caller cannot carry — and a caller that read the
    // refusal as "never ended" would record a completion for a run that asked to fail.
    let mut ledger = Ledger::new();
    let mut world = World::silent();
    let mut out = [0_u8; 2];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let ended: Poll<Result<(), Fault>> = poll_once(ctx.complete(b"too long"));

    assert_eq!(ended, Poll::Pending);
    assert_eq!(ctx.conclusion(), Some(Conclusion::Refused));
}

#[test]
fn a_refused_failure_is_not_read_back_as_a_completion() {
    // The sharpest shape of the defect above: a run that asked to *fail* with a payload the
    // buffer cannot carry. Answering `None` here made a caller record `RunCompleted`.
    let mut ledger = Ledger::new();
    let mut world = World::silent();
    let mut out = [0_u8; 2];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let ended: Poll<Result<(), Fault>> = poll_once(ctx.fail(b"the image did not verify"));

    assert_eq!(ended, Poll::Pending);
    assert_eq!(ctx.conclusion(), Some(Conclusion::Refused));
    assert_ne!(
        ctx.conclusion(),
        Some(Conclusion::Ended(Outcome::Completed(b"")))
    );
}

#[test]
fn a_future_polled_after_it_answered_asks_the_journal_nothing_more() {
    // Within one boot a future may be retained and polled normally, which means a second
    // poll of a finished future must not schedule a second effect.
    let mut ledger = Ledger::new().scheduling(vec![Ok(Handoff::Replayed(Outcome::Completed(
        b"\x02\x00\x00\x00",
    )))]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let mut future = pin!(ctx.activity::<Slot>(DOWNLOAD, b"url"));
    let mut task = Task::from_waker(Waker::noop());
    let first = future.as_mut().poll(&mut task);
    let second = future.as_mut().poll(&mut task);

    assert_eq!(first, Poll::Ready(Ok(Slot(2))));
    assert_eq!(second, Poll::Pending);
    assert_eq!(ledger.asked.len(), 1);
}

#[test]
fn a_stalled_dispatcher_is_polled_again_and_the_effect_resolves_once() {
    // Within one boot the future is retained and polled normally. The journal must see one
    // schedule and one resolve however many polls the world took.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(dispatch(0))])
        .resolving(vec![Ok(b"\x09\x00\x00\x00".to_vec())]);
    let mut world = World::answering(vec![Ok(b"\x09\x00\x00\x00".to_vec())]).stalling(2);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let mut future = pin!(ctx.activity::<Slot>(DOWNLOAD, b"url"));
    let mut task = Task::from_waker(Waker::noop());
    let first = future.as_mut().poll(&mut task);
    let second = future.as_mut().poll(&mut task);
    let third = future.as_mut().poll(&mut task);

    assert_eq!(first, Poll::Pending);
    assert_eq!(second, Poll::Pending);
    assert_eq!(third, Poll::Ready(Ok(Slot(9))));
    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Completed(b"\x09\x00\x00\x00".to_vec())),
        ]
    );
}

/// A waker that counts how often it was woken, so a test can see registration.
struct Counting {
    woken: core::sync::atomic::AtomicUsize,
}

impl Wake for Counting {
    fn wake(self: Arc<Self>) {
        self.woken
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    }
}

/// A dispatcher that keeps the waker it was handed and wakes it later.
struct Deferred {
    kept: Option<Waker>,
}

impl ActivityDispatcher for Deferred {
    type Error = Fault;

    fn poll_dispatch(
        &mut self,
        task: &mut Task<'_>,
        _id: EffectId,
        _kind: ActivityKind,
        _input: &[u8],
        _out: &mut [u8],
    ) -> Poll<Result<Produced, Fault>> {
        // What a real dispatcher does: keep the waker and answer when the world does.
        self.kept = Some(task.waker().clone());
        Poll::Pending
    }
}

#[test]
fn the_task_waker_reaches_the_dispatcher_and_is_the_one_the_executor_gave() {
    // Issue #35's first work bullet says the futures "register Embassy wakeups". This crate
    // registers none of its own: it plumbs the task's waker to the one thing that knows when
    // the world will answer. That plumbing is what this measures — the dispatcher keeps the
    // waker, wakes it, and the executor's own counter moves.
    let counter = Arc::new(Counting {
        woken: core::sync::atomic::AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&counter));
    let mut task = Task::from_waker(&waker);

    let mut ledger = Ledger::new().scheduling(vec![Ok(dispatch(0))]);
    let mut world = Deferred { kept: None };
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = pin!(ctx.activity::<Slot>(DOWNLOAD, b"url")).poll(&mut task);

    assert_eq!(answered, Poll::Pending);
    let kept = world
        .kept
        .take()
        .expect("the dispatcher was handed a waker");
    assert_eq!(counter.woken.load(core::sync::atomic::Ordering::Relaxed), 0);
    kept.wake();
    assert_eq!(counter.woken.load(core::sync::atomic::Ordering::Relaxed), 1);
}

#[test]
fn a_halt_registers_no_waker_at_all() {
    // The other half, and the honest one: a halted boot has nothing to wake. Nothing in the
    // façade holds the waker, so an executor is never asked to poll a run that is over.
    let counter = Arc::new(Counting {
        woken: core::sync::atomic::AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&counter));
    let mut task = Task::from_waker(&waker);

    let mut ledger = Ledger::new().scheduling(vec![Err(Halted)]);
    let mut world = Deferred { kept: None };
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = pin!(ctx.activity::<Slot>(DOWNLOAD, b"url")).poll(&mut task);

    assert_eq!(answered, Poll::Pending);
    assert!(world.kept.is_none(), "the world was never asked");
    assert_eq!(counter.woken.load(core::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn a_boundary_after_a_terminal_call_cannot_rewrite_what_the_run_ended_with() {
    // Codex round 2. The recorded ending points into the caller's buffer, and that buffer
    // is where the next boundary writes — so a terminal future that *resolved* let a
    // workflow record its ending and then perform an activity, and the run was committed
    // with the activity's bytes as its terminal payload. §08 has no edge from a terminal
    // record to another boundary either, so the future never resolving is the protocol
    // rather than a guard over it.
    let mut ledger = Ledger::new().scheduling(vec![Ok(Handoff::Replayed(Outcome::Completed(
        b"\x63\x63\x63\x63",
    )))]);
    let mut world = World::silent();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let _ended: Poll<Result<(), Fault>> = poll_once(ctx.complete(b"done"));
    let _after = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        ctx.conclusion(),
        Some(Conclusion::Ended(Outcome::Completed(b"done")))
    );
}

#[test]
fn a_run_that_ended_reaches_neither_the_journal_nor_the_world_again() {
    // The other half of the guard above, measured rather than implied: the boundary is not
    // merely harmless after an ending, it does not happen. A journal call would be a record
    // after a terminal one, which §08 refuses anyway.
    let spec = TimerSpec::AfterBoot { ticks: 1 };
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(dispatch(0))])
        .waiting(vec![Ok(())]);
    let mut world = World::answering(vec![Ok(b"\x01\x00\x00\x00".to_vec())]);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let _ended: Poll<Result<(), Fault>> = poll_once(ctx.fail(b"bad"));
    let after = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));
    let waited = poll_once(ctx.timer(spec));
    let restarted = poll_once(ctx.continue_as_new(b"next"));

    assert_eq!(after, Poll::Pending);
    assert_eq!(waited, Poll::Pending);
    assert!(restarted.is_pending());
    assert_eq!(
        ctx.conclusion(),
        Some(Conclusion::Ended(Outcome::Failed(b"bad")))
    );
    // The `Ctx` borrow ends here, so the two counters below can be read.
    let _ = ctx;
    assert_eq!(world.polls, 0, "the world was never asked");
    assert!(ledger.asked.is_empty(), "the journal was never asked");
}

#[test]
fn a_dispatcher_is_handed_exactly_the_runs_declared_result_bound() {
    // The caller's buffer is the wider of the run's two bounds, so it is wider than an
    // activity answer may be. Issue #36 asks that the length be validated against the
    // *bound*; narrowing the slice is the stronger half of that, because a dispatcher then
    // cannot write past it at all.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(bounded(0, 4))])
        .resolving(vec![Ok(Vec::new())]);
    let mut world = World::answering(vec![Ok(b"ab".to_vec())]);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let _answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        world.widths,
        vec![4],
        "the world is handed the bound, not the caller's whole buffer"
    );
}

#[test]
fn an_answer_over_the_declared_bound_is_exhausted_rather_than_truncated() {
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(bounded(0, 4))])
        .resolving(vec![Ok(Vec::new())]);
    // Five bytes reported against a four-byte bound, in a sixteen-byte buffer: the buffer
    // would take it and the run's declared bound would not.
    let mut world = World::answering(vec![Ok(b"abcde".to_vec())]).reporting(5);
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Exhausted),
        ],
        "over the bound is a clean refusal, and no part of the answer is recorded"
    );
    assert_eq!(answered, Poll::Ready(Err(Failure::Activity { len: 0 })));
}

#[test]
fn a_failure_payload_over_the_declared_bound_is_exhausted_rather_than_truncated() {
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(bounded(0, 4))])
        .resolving(vec![Ok(Vec::new())]);
    let mut world = World::answering(vec![Ok(b"abcde".to_vec())])
        .reporting(5)
        .failing();
    let mut out = [0_u8; 16];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let _answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Exhausted),
        ],
        "a failure payload is bounded by the same figure a result is"
    );
}

#[test]
fn a_typed_failure_payload_is_recorded_rather_than_dropped() {
    // Design document §09 gives `EffectFailed` a bounded payload. Before issue #36 the
    // dispatcher had no route to one: `Err(E)` recorded an empty failure and `Ok(len)` a
    // completion, so an activity could not report bytes and failure together.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(bounded(0, 8))])
        .resolving(vec![Ok(b"why".to_vec())]);
    let mut world = World::answering(vec![Ok(b"why".to_vec())]).failing();
    let mut out = [0_u8; 8];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Failed(b"why".to_vec())),
        ]
    );
    assert_eq!(answered, Poll::Ready(Err(Failure::Activity { len: 3 })));
}

#[test]
fn an_untyped_failure_still_records_no_payload() {
    // The other half of the same decision. `Self::Error` is the implementor's own value and
    // reaches no record: a workflow that branched on it would branch on something no replay
    // reproduces.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(bounded(0, 8))])
        .resolving(vec![Ok(Vec::new())]);
    let mut world = World::answering(vec![Err(Fault)]);
    let mut out = [0_u8; 8];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let _answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Failed(Vec::new())),
        ]
    );
}

#[test]
fn a_bound_wider_than_the_callers_buffer_cannot_produce_a_truncated_record() {
    // A caller that sized `out` under the run's declared bound. The room is the smaller of
    // the two, so an answer that overflows it is `Exhausted` — never a short record that
    // replays for ever.
    let mut ledger = Ledger::new()
        .scheduling(vec![Ok(bounded(0, 32))])
        .resolving(vec![Ok(Vec::new())]);
    let mut world = World::answering(vec![Ok(b"abcdefgh".to_vec())]).reporting(8);
    let mut out = [0_u8; 4];
    let mut ctx = Ctx::new(&mut ledger, &mut world, &mut out);

    let _answered = poll_once(ctx.activity::<Slot>(DOWNLOAD, b"url"));

    assert_eq!(world.widths, vec![4], "the room is the smaller of the two");
    assert_eq!(
        ledger.asked,
        vec![
            Asked::Schedule(DOWNLOAD, b"url".to_vec()),
            Asked::Resolve(Answered::Exhausted),
        ]
    );
}
