# 0049. An unserviceable kind is an answer, not a record

- Status: Accepted
- Date: 2026-09-14
- Issue: [#111](https://github.com/madmax983/waymaker/issues/111)
- Supersedes: one decision of
  [ADR 0033](0033-the-dispatcher-answers-in-a-bound-the-journal-states.md) — "`Unhandled::NoSuchActivity`
  is what a kind no row declares answers with. It is recorded as a failure with no payload,
  which is a **permanent** decision for that run." A kind no row declares now answers
  `Produced::Unserviceable` and records nothing. Nothing else in 0033 changes: the bound on
  `out`, `Produced::Failed`'s recorded payload, and the dispatch table's numeric selection all
  stand.
- Related: [ADR 0028](0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md)

## Context

ADR 0033 gave `waymaker-embassy`'s dispatcher two answers for a kind no row declares:
`wiring::Unhandled::NoSuchActivity`, recorded as an `EffectFailed` with no payload. That
choice was already named as the least bad of three. Its own consequences section said so:

> An unknown kind is recorded as a failure, and replay answers the same way on every later
> boot — including a boot whose firmware has the row. ... A route for "this firmware cannot
> service this kind" ... has no place in this trait, and is issue #111.

The recorded failure is **permanent**. §08's transition table has no edge back from a
resolved effect to an unresolved one. Once the code writes `EffectFailed`, no later firmware
can complete that effect — even one that adds more rows. A workflow that asked for an
activity kind not yet wired loses the run for good, on the first boot that reaches it.

The two alternatives ADR 0033 rejected are still worse. A refusal that aborted the run
outright has nowhere to put its answer: §08 has no edge from an unresolved effect to a
terminal record either, so the run is stranded rather than ended. A bare `Poll::Pending`
loses the reason. Nothing distinguishes "the world will answer, wait" from "no firmware will
ever answer this without a reboot". A caller cannot tell a normal stall from one that will
never resolve on its own.

Design document §11 already has the shape this needs. `KernelError::IncompatibleWorkflow`
means this firmware can never replay this history. `KernelError::NoPersistentClock` means
this firmware cannot service a request *now*. The two answers keep this distinction: never,
versus not yet. An unserviceable activity kind is the second shape. Nothing about the
identity changes. A firmware update that adds the row should still be able to complete it.

## Decision

**`Produced` gains a third answer.** `dispatch::Produced::Unserviceable` sits beside
`Completed` and `Failed`. It carries no payload and needs none: the activity kind a workflow
asked for is already known to whatever drove the boot, and §14's own catalogue of "named
reasons" (`KernelError`'s variants) carries none either.

**`Ctx` stops the boot for `Produced::Unserviceable`, but does not treat it as a retry.**
`ActivityFuture::poll`'s `Stage::Dispatching` arm answers `Poll::Pending` for both
`dispatched == Poll::Pending` and `dispatched == Poll::Ready(Ok(Produced::Unserviceable))`.
Neither calls `Journal::resolve`. Nothing is written, so the effect stays outstanding under
the identity its schedule record already committed. The two differ in what they do next.
`Poll::Pending` leaves `stage` at `Dispatching`, so a retained future keeps asking on every
poll — the ordinary shape of "the world is slow". `Unserviceable` moves `stage` to `Ended`
**and** sets a flag on the `Ctx` itself. The first stops a retained future's own spurious
repoll; the second stops a *different* future for the same still-outstanding effect, because
`stage` lives in the future and does not survive a drop. `ActivityFuture::poll`'s own first
check refuses once that flag is set, before `Stage::Scheduling` ever asks the journal again.
A later boot — the same run, replayed against a fresh `Ctx` and a dispatcher that has gained
the row — starts over with no flag set, reaches the same `Handoff::Dispatch`, and may
complete it.

**`wiring::Table::poll_dispatch` answers `Produced::Unserviceable` for a kind no row
declares**, in place of the `Unhandled::NoSuchActivity` it used to construct. That was
`Unhandled`'s only reason to exist beside wrapping a row's own error. So `Unhandled<E>` is
removed, and `Table<W, E>::Error` is `E` directly. A row's failure travels as itself, with no
wrapper to unwrap.

```text
Table::poll_dispatch(kind: a number no row declares)
  before: Poll::Ready(Err(Unhandled::NoSuchActivity(kind)))  -- recorded as EffectFailed
  after:  Poll::Ready(Ok(Produced::Unserviceable))           -- nothing recorded
```

A dispatcher written by hand, not through `Table`, may answer `Produced::Unserviceable` the
same way for the same reason. The trait's own documentation says so.

## Consequences

**A run that meets a kind its firmware does not have is no longer lost.** It stalls, the same
way a slow external world stalls it, until a firmware update adds the row. The next boot
completes it under the identity the first schedule record committed. Two tests prove this at
two levels. `crates/waymaker-embassy/tests/wiring.rs`'s
`a_firmware_that_later_gains_the_row_completes_the_run_its_predecessor_could_not` is the
façade's own sequencing, over a fake journal: a table with no row leaves nothing resolved,
and a second table that gains the row resolves the same effect under the same identity.
`crates/waymaker-drive/tests/dispatch.rs`'s
`a_firmware_that_later_gains_the_row_completes_the_run_its_predecessor_left_outstanding` is
the same claim over real media: a table with no row commits a schedule record and writes
nothing else; a second boot, over the same device, with a table that has the row, redelivers
and completes it.

That second test's first boot answers `Ok(Progress::Waiting)`, the clean stall a real caller
wants to see. Its own `Wired::run` bridges the async façade through `waymaker-drive`'s
synchronous `Boundary` the same way `ota.rs`'s `Downloader::run` does: it keeps the real
`Suspended` the boundary returned on its last call, and falls back to
`Suspended::awaiting_dispatch()` on the one path with no boundary call behind it at all — a
dispatcher that answers `Produced::Unserviceable` with no journal call in between. An earlier
version of this test believed the bridge had no such fallback and asserted
`Err(DriveError::EffectOutstanding)` instead; that was wrong about the bridge, which already
had the mechanism `ota.rs` uses, and Codex found it on review of this change.

**The code-flash cost is nil.** `cargo xtask size`'s `facade` row measures **13174 B** of
layers both before and after this change. Removing `Unhandled`'s wrapping paid for handling
the third `Produced` arm. No budget moved and no raise was needed.

**A caller still cannot tell "the world is slow" from "no firmware will ever service this"
from the return value alone.** Both cases return `Poll::Pending` — a halted journal and an
unpassed deadline already work the same way. Design document §13's boundary gives no reason
for any stop, and this change adds none. What changes is not observability but durability:
unlike the old behaviour, nothing is committed while the boot waits. The stall costs nothing
to leave and nothing to be wrong about.

**`Unhandled<E>` is a breaking rename for anyone matching on it.** `waymaker-embassy` is
still rung 0.4 work. Every caller in this workspace — `waymaker-drive`'s tests and the size
probe — is updated in the same change.

A second Codex round found the first version of the fix incomplete: `stage` alone stops a
*retained* future's spurious repoll, but a caller that drops the future and asks again this
boot — a `select!` cancellation, say — gets a fresh `ActivityFuture` starting at
`Stage::Scheduling`, which does not remember what the dropped one learned. `Ctx` now carries
the flag instead, shared across every `ActivityFuture` it builds, the same way issue #107
moved a run's terminal/continued flag out of `TerminalFuture` and `ContinueFuture` and into
`Ctx` for the identical reason: a value only a future holds is a value a drop can lose.
`crates/waymaker-embassy/tests/ctx.rs`'s
`an_unserviceable_kind_is_not_a_retry_after_the_future_is_dropped_and_recreated` drives the
second future to `Poll::Pending` and asserts both the dispatcher and the journal saw the
first `schedule` call and nothing after it.

A third Codex round found the flag was still read in only one place. `waymaker-drive`'s
boundary refuses a *second* boundary call while `Context`'s own `pending` field names an
effect the workflow's last `run()` did not itself suspend over — `schedule`, `decide_timer`
and `decide_gate` each set a hard `Stop::Failed(DriveError::EffectOutstanding)` when they see
it, and `Context::conclude` answers the same way when `run()` returns `Ok(Outcome)` while
`pending` is still `Some`. A workflow that met `Produced::Unserviceable` on one boundary and
then asked for a different one on the same boot — a `ctx.timer()`, a `ctx.complete()`, a
`ctx.continue_as_new()` — would reach exactly that: the still-outstanding effect turns a
clean stall into a hard boot error, the very failure this ADR's mechanism exists to prevent
for the boundary that actually met `Unserviceable`. `TimerFuture`, `ContinueFuture` and
`TerminalFuture` now each carry the same `&'b bool` `Ctx` already gave `ActivityFuture`, and
each refuses to reach the journal or record a conclusion once it is set — the same shape as
`ActivityFuture`'s own first check, propagated to the three futures that had not needed it
before this issue. Three tests in `crates/waymaker-embassy/tests/ctx.rs` —
`an_unserviceable_kind_stops_a_timer_future_built_after_it_from_reaching_the_journal`,
`an_unserviceable_kind_stops_continue_as_new_from_reaching_the_journal`, and
`an_unserviceable_kind_stops_a_terminal_future_from_recording_a_conclusion` — each drive an
activity to `Produced::Unserviceable` and then build one of the other three futures on the
same `Ctx`, asserting the journal is asked nothing further and, for the terminal future, that
`Ctx::conclusion` still answers `None`. Three `&'b bool` fields, one per future, moved the
`facade` row from 13114 B to **13122 B** of the 14336 B gate — the second and third rounds'
combined cost, against the first round's own claim of nil, which was true only of that
round's own diff.

## Alternatives considered

**A fifth `ActivityDispatcher` method**, asking a dispatcher up front whether it can service a
kind. Rejected. It adds a call to every dispatch for a question `poll_dispatch` can already
answer, and a dispatcher could still lie on one call and not the other.

**A `Halted`-shaped return from `poll_dispatch`**, mirroring the journal's own stop marker.
Rejected. `Halted` belongs to the *journal* half of the boundary. Reusing it here would treat
a dispatch failure as the same kind of event as a journal that cannot commit. §07 splits
those into two halves with two different owners. `Produced` is the dispatcher's own
vocabulary, and extending it keeps the boundary between the two traits where §07 draws it.

**Keeping `Unhandled::NoSuchActivity` and adding `Produced::Unserviceable` beside it.**
Rejected once `Unhandled` had nothing else to distinguish. A type with one variant
(`Activity(E)`) is a rename of `E`. A rename that only makes matching noisier is not worth
keeping for the sake of leaving a type's name unchanged.
