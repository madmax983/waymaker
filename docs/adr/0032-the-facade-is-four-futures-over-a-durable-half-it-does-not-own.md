# ADR 0032: The façade is four futures over a durable half it does not own

- Status: accepted
- Date: 2026-09-08
- Issue: [#35](https://github.com/madmax983/waymaker/issues/35)
- Supersedes: nothing
- Related: [0024](0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md),
  [0025](0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
  [0028](0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md)

## Context

Design document §02 decision 5 says async syntax is an adapter: `waymaker-embassy` supplies
the ergonomic façade, and the persistence protocol depends on neither Embassy nor `Future`.
Issue #35 states the same thing as a rule for this crate — "it must add sugar, never
authority" — and §05 states it as the crate's must-not-own cell: on-media authority or
hidden global state.

Nothing enforced it, because nothing existed. `waymaker-embassy` held §11's clock capability
and no `Ctx`. The protocol was driven by `waymaker-drive`, a crate above the layers, and the
claim that the façade would be "a façade over exactly this protocol" was an argument.

Three forces shaped the answer.

**The façade must not own the loop.** A `Ctx` that recovered a bank, replayed a journal and
appended records would be a second implementation of §07 and §08, in a crate whose
must-not-own cell forbids the first of them. Two implementations of a durability protocol
drift, and the one in the façade would be the one nobody drives at every crash point.

**The design document's own sketch is a join.** §13 writes `Ctx<'a, D, J>` with `D:
ActivityDispatcher` and `J: Journal`. Those are the two halves §07 already separates: steps
1 to 3 and 5 to 7 are durable, step 4 is the world, and an `async fn` needs an `.await`
between them. A façade that wrapped one undivided `call` would have nothing for `D` to do.

**Removing Embassy must remove nothing else.** Issue #35's second "done when" is that the
protocol stays fully usable through the synchronous driver.

## Decision

`waymaker-embassy` gains `ctx`, `journal`, `dispatch` and `decode`.

**`Journal` is the durable half**, and the façade implements none of it. Four methods:
`schedule` (§07 steps 1 to 3, answering `Handoff::Replayed` or `Handoff::Dispatch`),
`resolve` (steps 5 to 7), `wait` (§11's deadline) and `continue_as_new` (§10). Authority
over media is the implementor's.

**`ActivityDispatcher` is the world's half**, poll-shaped. §13 sketches `async fn dispatch`;
a future that must survive between polls has to be stored, and the type an `async fn` in a
trait returns cannot be named, so a named `ActivityFuture` could not hold one without an
allocation. `poll_dispatch` stores nothing. Issue #36 owns the ergonomic wrapper.

**`Ctx` is the join and nothing else.** Four futures: `ActivityFuture`, `TimerFuture`,
`ContinueFuture` and `TerminalFuture`. The world is reached through `Handoff::Dispatch` and
through no other value, so §02 decision 3 holds at the façade for the reason it holds in
§07's typestate.

Three of the four are the shape §13 asks for. `TimerFuture<'b, J>` drops the dispatcher §13
gives it, for the reason §13 itself drops it from `ContinueFuture`: a deadline never reaches
the world. `ContinueFuture`'s output is `Infallible`, so the code after the `.await` is
unreachable rather than merely unlikely — the run that asked is replaced.

**There is no Embassy dependency.** The futures are plain `core::future::Future`s, so
Embassy's executor polls them and this crate has no executor, no timer queue and no waker of
its own. A façade that pulled in an executor to hand out four futures would be more than the
adapter §02 decision 5 says it is. `embassy-below-facade` still guards the edge.

**`ctx-facade`** is what stops the shape being given back. It pins both surfaces in both
directions, pins the four futures by name and by `fn poll` count, refuses each piece of
on-media authority by identifier — `StableStorage`, `Reserved`, `RecordRef`, `Recovery`,
`ReplayMachine`, `BankLayout`, `Swap` — and refuses a `static`. Its fourth half is the
driver's: six `waymaker-drive` modules may not name `waymaker_embassy`, so the façade edge is
`facade.rs` and `ota.rs` and issue #35's second "done when" is a fact about the source.

`waymaker-drive`'s `Boundary` grows `schedule`, `resolve` and `continue_as_new`, in its own
vocabulary. That is what keeps the driver façade-free: `Handoff` and `Answered` are the
driver's types, and the bridge that renames them to the façade's is one file.

## Consequences

**§07 is unchanged and unduplicated.** `Boundary::schedule` is `Context::decide` with the
`Dispatchable` kept in a field rather than on the stack, and `Boundary::resolve` is the tail
of `Context::dispatch`. The writer lives inside the `Dispatchable` between the two, so a run
with an effect in flight still has no appender.

**A dispatcher error is a failed effect.** `Poll::Pending` is "try again", and `Err` is
recorded as an `EffectFailed` with no payload. The typed error reaches
`Ctx::dispatch_error` for a log and no further: a workflow that branched on it would branch
on something no replay can reproduce. It costs the failure's detail, which ADR 0025 already
records as the price of an empty payload fitting every bound. Neither is a retry *policy* —
§16's `retry-policy-placement` stays open, and nothing here counts attempts or waits.

**A defect fell out of the first end-to-end test.** A dispatcher that answered
`Poll::Pending` after the schedule record was committed left the boot with no recorded
reason: the façade tells the journal nothing on a stall, and only the driver holds the
identity. `Driver`'s `conclude` now reports the outstanding effect as `Progress::Waiting`,
which is what `Performed::Pending` reports on the undivided path.

**A second caller-owned buffer.** `Ctx` holds one for the dispatcher's answer, and the
driver holds its own result buffer. The bytes are copied once between them. Both are the
caller's, so §04's runtime-RAM statics gate does not move, but a device running the façade
carries two buffers where the synchronous driver carries one. Issue #39 is where that is
measured.

**178 B of code flash.** The `facade` row goes from 12334 B to 12512 B of layers. The
*gated* row does not move: §04 states the budget over "core + flash adapter", which is the
`default` row, and the façade row is printed rather than gated. Issue #39 is where it
becomes a gate.

**`continue_as_new` has no implementation that swaps.** `waymaker-drive` refuses with
`DriveError::ContinueUnsupported`, because §10's swap works on a bank and this driver is
pointed at a `JournalRegion`. `ContinueFuture` is a real future over a real boundary
operation whose one implementation today is a refusal. That is stated here rather than left
to be discovered; issue #36's dispatcher is where the two are joined.

**§06's example is adapted in one place.** `ctx.activity(VERIFY_SIGNATURE, ..).await?` with
its result discarded leaves `T` unconstrained, so the example binds `let ()` instead. The
`Decode` implementation for `()` is what makes that read as what it is: an activity whose
result is only that it happened.

## Alternatives considered

**A `Ctx` that owned the loop.** Rejected: it is the must-not-own cell, and it would put a
second §07 in the crate that is meant to be an adapter.

**A `Ctx` over the undivided `Boundary::call`.** Simpler, and it needs no change to the
driver — but `D: ActivityDispatcher` would then be a generic parameter nothing polls, since
the driver's own `Activities` would already have performed the effect. A vestigial parameter
in a pinned surface is worse than the change that removes it.

**`async fn dispatch` in the trait, as §13 writes it.** Rejected for now: the future it
returns cannot be named, so `ActivityFuture` cannot store it without an allocation this
workspace does not have. `poll_dispatch` is the primitive an ergonomic wrapper can be built
over, and issue #36 owns that wrapper.

**Recording a dispatcher error as `Answer::Pending` instead.** It would have left the effect
outstanding and redelivered it on the next boot, which reads as kinder — but a dispatcher
that fails permanently then loops for ever, and choosing how often to try again is exactly
§16's open `retry-policy-placement`. Leaving the choice with the dispatcher decides nothing:
it answers `Poll::Pending` to be tried again and `Err` to record a failure.
