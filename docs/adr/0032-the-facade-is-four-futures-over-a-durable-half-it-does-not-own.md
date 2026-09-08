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

§13 names three futures and this crate ships four. `ActivityFuture` is §13's shape exactly.
`ContinueFuture` is too, and its output is `Infallible`, so the code after the `.await` is
unreachable rather than merely unlikely — the run that asked is replaced. `TimerFuture<'b,
J>` drops the dispatcher §13 gives it, for the reason §13 itself drops it from
`ContinueFuture`: a deadline never reaches the world. `TerminalFuture` is the fourth and is
in neither sketch; §06's example ends with `ctx.complete(&[]).await`, so the "done when"
asks for it even though the API sketch does not list it.

**There is no Embassy dependency.** The futures are plain `core::future::Future`s, so
Embassy's executor polls them and this crate has no executor, no timer queue and no waker of
its own. A façade that pulled in an executor to hand out four futures would be more than the
adapter §02 decision 5 says it is. `embassy-below-facade` still guards the edge.

**`ctx-facade`** is what stops the shape being given back. It pins both surfaces in both
directions, pins the four futures by name and by `fn poll` count, pins the set of types the crate implements `Future` for, pins `Ctx`'s methods at
every visibility and refuses an associated constant on it, refuses each piece of on-media
authority by identifier — `StableStorage`, `Reserved`, `RecordRef`, `Recovery`,
`ReplayMachine`, `BankLayout`, `Swap` — refuses a `static` and refuses a `macro_rules!`.
The last three read *every* file of the crate rather than the two the surfaces are pinned in,
because they are statements about the crate: review of this change put a renamed
`StableStorage`, a `pub static AtomicUsize` and a macro that expands a tenth public method
into `impl Ctx` one file over, and watched a two-file version stay green.

Its fourth half is the driver's, and it is the fast half of issue #35's second "done when":
every `waymaker-drive` module but `facade.rs`, `ota.rs` and `lib.rs` is held to naming
neither the façade crate nor the two modules nor the `Bridge` they re-export. The half a
*compiler* decides is the `drive-facadeless` pipeline stage: `waymaker-drive`'s
`without-facade` feature deletes those two modules, and the stage builds the result for the
firmware target. Both exist because a scanner cannot see an import routed through
`crate::facade` or a dependency renamed in a manifest, and a compiler sees each at once.

`waymaker-drive`'s `Boundary` grows `schedule`, `resolve` and `continue_as_new`, in its own
vocabulary. That is what keeps the driver façade-free: `Handoff` and `Answered` are the
driver's types, and the bridge that renames them to the façade's is one file.

## Consequences

**§07 is unchanged and unduplicated.** `Boundary::schedule` is `Context::decide` with the
`Dispatchable` kept in a field rather than on the stack, and `Boundary::resolve` is the tail
of `Context::dispatch`. The writer lives inside the `Dispatchable` between the two, so a run
with an effect in flight still has no appender.

**A dispatcher error is a failed effect.** `Poll::Pending` is "try again", and `Err` is
recorded as an `EffectFailed` with no payload. The typed error goes no further than the
dispatcher that raised it: a workflow that branched on it would branch on something no
replay can reproduce, and a first review round pointed out that an accessor on `Ctx` made
exactly that available on the first run and never on a replay. It costs the failure's
detail, which ADR 0025 already records as the price of an empty payload fitting every bound.
It also means a *typed* failure payload has no route through this trait at all — `Ok(len)`
is an `EffectCompleted` — which §09 gives `EffectFailed` and this signature does not. Issue
#36's is to close. Neither is a retry *policy*: §16's `retry-policy-placement` stays open,
and nothing here counts attempts or waits.

**A run that ended has no boundaries left.** `TerminalFuture` never resolves, so an
`async fn` that calls `ctx.complete(..)` stops there; and every other future refuses once a
conclusion is recorded, so a caller that reaches a boundary without going through `.await`
does not overwrite the buffer the ending points into. Codex round 2 found the version that
resolved: a workflow could record its ending and then perform an activity, and the run was
committed with the activity's bytes as its terminal payload — permanently, on media. §08
has no edge from a terminal record to another boundary either, so stopping is the protocol
rather than a guard over it.

The cost is that the caller reads `Ctx::conclusion` whatever the poll said: a finished run
and a suspended one are both `Poll::Pending`, and the recorded ending is what tells them
apart.

What this is *not* is a cancellation story, and Codex round 5 is where the difference
showed. Both mechanisms assume the future that recorded the ending is still alive:
`TerminalFuture` guards on a field of its own rather than on the `Ctx`'s conclusion, and
`ContinueFuture` on a field of its own because a continued run has no `Ending` to record. A
dropped future takes its flag with it, so a cancelled `complete` followed by a `fail` keeps
the second, and a cancelled `continue_as_new` leaves the `Ctx` unconcluded with the run
already asked to be replaced. No `async fn` can express either, since neither future
resolves; it takes a manual poll or a cancellation combinator, and the one that brings those
is #36's executor. One flag on the `Ctx` closes both, and issue
[#107](https://github.com/madmax983/waymaker/issues/107) carries the done-when list. It is
recorded here rather than fixed in #105 because that PR's review budget was spent and
neither finding is reachable by the code #105 ships.

**The terminal payload has a third answer.** `Ctx::conclusion()` returns
`Conclusion::Refused` for a payload wider than the caller's buffer, rather than the `None`
that also means "the run has not ended". Review of the first commit found that a caller
reading the two as one recorded a `RunCompleted` for a run that called `ctx.fail`. The
buffer is bounded by *both* of the run's declared bounds, not by `effect_result_bytes`
alone, and `Driver::boot` now refuses a narrower one before a record is read — the
alternative is discovering it at the last record, after every effect has been performed.

**A defect fell out of the first end-to-end test.** A dispatcher that answered
`Poll::Pending` after the schedule record was committed left the boot with no recorded
reason: the façade tells the journal nothing on a stall, and only the driver holds the
identity. `Driver`'s `conclude` now reports the outstanding effect as `Progress::Waiting`,
which is what `Performed::Pending` reports on the undivided path.

**A negative cargo feature, and a claim it does not quite reach.** `without-facade` is
normally an anti-pattern: feature unification turns one crate's opt-out into everyone's. It
is safe here for a reason particular to this crate — nothing depends on it. The alternative,
an optional dependency off by default, would have taken the façade out of the lint, test,
docs and coverage stages, which all pass `--no-default-features`.

What the `drive-facadeless` build establishes is that no `waymaker-drive` module outside
`facade.rs` and `ota.rs` *needs* the façade. It does not establish that the crate would build
with `waymaker-embassy` deleted: the manifest entry is not optional, so that configuration
still resolves and compiles the façade, and a `compile_error!` inside the façade fails the
stage. Codex round 3 measured that rather than arguing it. The answer that would say it in
the dependency graph is to move the two modules into a crate of their own, above
`waymaker-drive` — which also removes this feature and this stage — and it is issue
[#106](https://github.com/madmax983/waymaker/issues/106) rather than this change, because a
restructure taken at the end of a review round is one no round has reviewed.

**A second caller-owned buffer.** `Ctx` holds one for the dispatcher's answer, and the
driver holds its own result buffer. The bytes are copied once between them. Both are the
caller's, so §04's runtime-RAM statics gate does not move, but a device running the façade
carries two buffers where the synchronous driver carries one. Issue #39 is where that is
measured.

**182 B of code flash.** The `facade` row goes from 12334 B to 12516 B of layers. The
*gated* row does not move at all: §04 states the budget over "core + flash adapter", which
is the `default` row, and the façade row is printed rather than gated. Issue #39 is where it
becomes a gate.

**Wakeups are the dispatcher's.** §05's Owns cell names wakeups and this crate registers
none of its own. What it does is plumb the task's waker to
`ActivityDispatcher::poll_dispatch`, the one thing that knows when the world will answer;
`crates/waymaker-embassy/tests/ctx.rs` measures that with a counting waker. Two paths
register nothing at all — a halted boot, because there is nothing left to wake, and a
deadline that has not passed, because there is no in-boot sleep. The timer future asks its
journal again on every poll instead, which is what makes a retained one able to make
progress; issue #36's dispatcher is where a hardware alarm arrives.

**The gate's readers had to be fixed twice.** Round 1 found that a public method sharing a
line with its `impl` was invisible to nine surface pins and to `size-probe-reach`; round 3
found that the fix read the line's *start*, so an attribute in front of the item —
`#[rustfmt::skip] impl Bank { pub fn raw() {} }`, which is the exact form of the mutation
round 1 was about, and which `cargo fmt` leaves alone — hid it again. Leading attributes are
now set aside before anything classifies a line, in `public_functions` and in the `impl`-body
reader beside it, so `ctx-facade`'s method pin and `effect-protocol` see through them too.

**A generic body no caller names is compiled for nothing.** `ota_update` and `Ota` are
generic, so `nm` on the `thumbv6m` rlib found zero `ota_update` and zero `ActivityFuture`
symbols: the firmware build type-checked them and compiled neither. `Downloader` and
`poll_ota` are concrete and name them, so it now monomorphises this workflow's future, the
bridge, and the two futures §06's example uses. It uses neither `TimerFuture` nor
`ContinueFuture`, so neither is in that rlib; the size probe drives all four, which is what
the `facade` row measures.

**`continue_as_new` has no implementation that swaps.** `waymaker-drive` refuses with
`DriveError::ContinueUnsupported`, because §10's swap works on a bank and this driver is
pointed at a `JournalRegion`. `ContinueFuture` is a real future over a real boundary
operation whose one implementation today is a refusal. That is stated here rather than left
to be discovered; issue #36's dispatcher is where the two are joined.

**§06's example is adapted in five places**, and none of them changes what it demonstrates.
`ctx.activity(VERIFY_SIGNATURE, ..).await?` with its result discarded leaves `T`
unconstrained, so the example binds `let ()` instead; the `Decode` implementation for `()`
is what makes that read as what it is. `ota_update` is generic over `D` and `J`, which is
§13's own `Ctx<'a, D, J>` rather than §06's bare `Ctx<'_>` — the two sketches disagree and
§13 is the specific one. `OtaInput` borrows its url rather than owning it, because a
workflow future holds every local that survives an `.await`. The activity kinds are
`ota::DOWNLOAD` rather than `ActivityKind::DOWNLOAD`, because the kernel must not carry one
workflow's constants. And the run input comes from a module constant rather than from the
recorded `RunStarted` record: `Workflow::run` has no channel for it, so the example
exercises §06's "recorded effect results" and not its "recorded input". That last one is a
gap in the example rather than in the engine — `Driver::begin` compares the recorded input
against `Workflow::identity` on every boot — and issue #38's provisioning example is where a
run reads its own input.

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
