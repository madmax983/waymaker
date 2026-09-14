# 0046. An unserviceable kind is an answer, not a record

- Status: Accepted
- Date: 2026-09-14
- Issue: [#111](https://github.com/madmax983/waymaker/issues/111)
- Supersedes: nothing
- Related: [ADR 0033](0033-the-dispatcher-answers-in-a-bound-the-journal-states.md), [ADR 0028](0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md)

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

**`Ctx` stops the boot the same way it stops for `Poll::Pending`, and for the same reason.**
`ActivityFuture::poll`'s `Stage::Dispatching` arm answers `Poll::Pending` for both
`dispatched == Poll::Pending` and `dispatched == Poll::Ready(Ok(Produced::Unserviceable))`.
Neither calls `Journal::resolve`. Nothing is written, so the effect stays outstanding under
the identity its schedule record already committed. A later boot — the same run, replayed
against a dispatcher that has gained the row — reaches the same `Handoff::Dispatch` and may
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

That second test surfaces a rough edge worth naming rather than hiding. Its own `Wired::run`
bridges the async façade through `waymaker-drive`'s synchronous `Boundary`, and that bridge
has no way to build a real `Suspended` for a stall the async dispatcher never reports back to
the driver. So the first boot's `Driver::boot` answers `Err(DriveError::EffectOutstanding)`
rather than `Ok(Progress::Waiting)` — a driver-level refusal rather than the wait a real
caller would want to see. What the test actually proves does not rest on that return value:
the journal on real media holds a schedule record and nothing else, and the second boot's
redelivery and completion are what settle the claim. Closing this rough edge belongs to issue
[#110](https://github.com/madmax983/waymaker/issues/110), which joins the async dispatcher to
a real executor; it is not `waymaker-embassy`'s to close.

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
