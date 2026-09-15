# 0048. An alarm is armed on a halt, and a driver at a bank can swap

- Status: Accepted
- Date: 2026-09-14
- Issue: [#110](https://github.com/madmax983/waymaker/issues/110)
- Supersedes: nothing
- Related: [ADR 0022](0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md), [ADR 0030](0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md), [ADR 0032](0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md), [ADR 0044](0044-a-device-is-a-borrow-in-three-modules-and-a-value-in-a-fourth.md)

## Context

Issue #110 records two things rung 0.4 still owed, both left open by ADR 0032 and named
there as "rung 0.4's dispatcher":

**In-boot sleep.** `waymaker-embassy`'s `TimerFuture` asked `Journal::wait` on every poll and
registered no waker of its own. Design document §11 wants a device that arms a hardware
alarm and sleeps until it fires; without one, a boot waiting only on a deadline had nothing
that would ever poll it again once every other source of a wakeup was exhausted.

**The `continue_as_new` join.** `waymaker-drive`'s `Boundary::continue_as_new` refused
unconditionally, with `DriveError::ContinueUnsupported`. §10's swap is `waymaker-flash`'s
`swap` module, and it works on a *bank* — the two-bank layout, the authority the device
booted, and the generation seal. `Driver` was pointed at a bare `JournalRegion` and knew
none of them, so a swap there would have been a swap of a bank the driver could not name.
ADR 0022 recorded the two preconditions this left on `Swap::beginning`'s caller — a stale
`booted`, an unverified `run` — as work for "the dispatcher that would join the two", and
issue #110 is that join.

## Decision

**An `Alarm` capability, armed on a halt.** `waymaker-embassy::alarm::Alarm` is one method:

```rust,ignore
fn wake_after(&mut self, kind: ClockKind, remaining: u64, waker: &Waker);
```

`Journal` and `Boundary` each grow one query, `deadline_remaining() -> Option<(ClockKind,
u64)>`, answering `Some` only right after a `wait` that stopped because the deadline had not
passed. `TimerFuture` calls it on every `Halted`; when it answers `Some`, `TimerFuture` arms
the alarm with the task's own waker before returning `Pending`, instead of relying on
something else to poll it again. `Alarm: Send`, because the waker it is handed is typically
woken from an interrupt rather than from the context that armed it — a different concrete
type, not merely a different thread, and the bound is what a `dyn Alarm` needs to be
`Send` at all. `NoAlarm` is the zero-cost implementation for a firmware with no such
peripheral: `wake_after` does nothing, and `TimerFuture` asks again next poll, exactly as it
did before this trait existed. Neither addition changes what `wait`'s own two return values
mean; `deadline_remaining` is deliberately answerable for one reason only, so a caller can
tell "arm something" apart from every other reason to stop.

**A driver pointed at a bank.** `Driver` gains a second constructor, `Driver::at_bank(layout,
reserve)`, alongside the unchanged `Driver::new(region, run, reserve)`. Internally, `Driver`
now holds a `Pointing` — `Region(JournalRegion, RunId)` or `Bank(BankLayout)` — and
`region()`/`run()` answer `Option` accordingly: `None` for a bank-pointed driver, whose
region and run are not known until a device has been read. Every `boot()` of a bank-pointed
driver reads both banks' headers and seals fresh, decides authority with
`waymaker_flash::bank::select`, and keeps the winning bank's scalar facts — its layout,
its `Authority`, its run, its header's `align`/`workflow_kind`/`workflow_version`/
`input_schema` — for the length of that one boot. `Boundary::continue_as_new` on a
region-pointed driver still refuses with `DriveError::ContinueUnsupported`, unchanged; on a
bank-pointed one it performs §10's seven steps for real: it consults
`Reserve::for_layout` against the bank's own layout before touching the device, mints the
next run with `RunId::successor()` (new, mirroring `Generation::successor()` and
`EffectSeq::successor()` — refusing at `u64::MAX` rather than wrapping into a run id this
device may already have sealed a bank under), and drives `Swap::beginning` through
`prepare`, `stage`, `payload_barrier` and `commit` — the point of no return, after which the
new run is authoritative whatever happens next — and then `reclaim`, whose own error this
call discards: a failed erase of the retiring bank does not turn a successful migration
into a failure, matching `Installed::reclaim`'s own documented postcondition, and the next
swap's own `prepare` erases that bank unconditionally before writing anything regardless.
`Progress::Migrated { run }` is the new answer `boot` gives when this succeeds, and it is a
statement about the new run's authority rather than a promise that the old bank's seal is
already gone.

The two fields ADR 0022 named as `Swap::beginning`'s unverified preconditions — `booted` and
`run` — are read from the device fresh on this same boot rather than carried in by a caller,
closing that gap for a caller that only ever swaps through this driver. `swap_in`'s other
inputs are not preconditions of the same shape: `bank.layout` is the geometry `Driver::at_bank`
was configured with, fixed for the life of the driver rather than read per boot, and
`next_header.input` is the workflow's own new content — checked by `verify_header_identity`
below on the *next* boot, not by anything `Swap::beginning` itself could verify against a run
that has not started yet. Closing issue #110's own third precondition is separate again: a
capacity check that was nobody's obligation before is now the swap's own first statement.

`swap_in` also refuses before touching the device in three cases a code review round added.
An effect already scheduled and not yet resolved has a durable schedule record in the bank
about to be reclaimed, so a live call refuses with `DriveError::EffectOutstanding` rather than
forfeiting an identity no crash took. A next-run input wider than the run's own declared
`run_input_bytes` bound would install a journal below `Reserve::for_layout`'s own floor,
stranding the very run it just started, so it is refused with `DriveError::NextRunInputTooLong`
before any byte moves. And a bank a swap has just installed carries no `RunStarted` record yet
for `begin` to check the next workflow's identity against, so `verify_header_identity` makes
the same comparison `begin` makes against a journal, against the header instead — refusing
with `DriveError::NotThisWorkflow` when they disagree, on the very first boot of the bank a
swap installed.

## Consequences

**`Ctx::timer` takes a third argument, and `TimerFuture` lost its derived `Debug`.**
`Ctx::timer(spec, alarm)` now borrows an `&mut dyn Alarm` for the future's own lifetime,
alongside the journal and the spec it already took; every caller — the reference workflows,
the size probe, the test suites — passes one, `&mut NoAlarm` where nothing else is armed.
`TimerFuture` could not keep its `#[derive(Debug)]` once it held a `&mut dyn Alarm`, which has
none; a hand-written impl that skipped the field would have needed a method `CTX_SURFACE`
does not pin, so the derive is dropped rather than replaced.

**The façade's `continue_as_new` join needed no changes of its own.**
`waymaker-embassy::journal::Journal::continue_as_new` was already a pass-through to
`Boundary::continue_as_new` (via `waymaker-drive`'s `Bridge`), and `Bridge` branches on
nothing to decide which shape of driver is behind it. A workflow written against `Ctx` that
calls `.continue_as_new(next_input).await` was already asking the driver to do §10's swap;
before this change every driver answered `ContinueUnsupported`, and after it, a driver built
with `Driver::at_bank` answers by doing it. The async and synchronous callers of `Boundary`
share one implementation, which is the whole reason `Bridge`'s own documentation says it
"adds nothing — it renames four calls."

**`Driver::region()` and `Driver::run()` changed shape.** Both answer `Option` now, `None`
for a bank-pointed driver whose region and run are not fixed values. The one caller outside
this file, `crates/waymaker-drive/tests/boundary.rs`, is the only place this is visible.

**A bank-pointed driver reads at least two header-sized buffers of the caller's page before
recovery begins.** `read_bank` copies up to `page.len()` bytes per bank into the scratch
page to decode a header whose length is not known until its own prefix is read — the same
shape `waymaker-flash`'s own `decode_header` already has, applied twice, before `Recovery`
is ever constructed. A header too large for `page` is not decided outright the way a bank
whose seal does not validate is: `read_bank` answers `BankRead::Oversized`, naming the
claimed generation and the size the caller would need, and bank selection surfaces that as
a retryable `RecoveryError::PageTooSmall` — or ignores the candidate entirely once the
other bank fully validates at a strictly higher generation, the same deferral an unreadable
header gets.

**`Driver::at_bank`'s `continue_as_new` calls `reclaim` immediately rather than leaving it
lazy.** §10's seventh step does not have to happen before a swap is safe — the retiring
bank's erase is idempotent from the reader's side, because `select` never weighs a bank
whose seal fails to validate, torn erase included — but nothing in this driver has anywhere
to *defer* it to across a call that returns. Doing all seven steps in one call means a
`Migrated` boot leaves the device in the same two-bank shape a fresh device would have had
its second bank provisioned into, rather than one bank sealed twice over with the older
sealing left to a swap that has not happened yet.

**What this does not close.** `Swap::beginning`'s third precondition — that `next.run` is
fresh for the *whole device*, not merely different from the run being retired — is closed for
a device that only ever swaps through this driver: `RunId::successor()` only ever advances, so
a run id this driver mints has never been sealed under before, on this bank or the other one.
It is not closed for a device that swaps by another route, and `SwapError::RunReused` still
only catches the adjacent case. Nothing obliges a caller to use
`Driver::at_bank` over `Driver::new` — a region-pointed driver remains a legitimate,
supported shape, and its `continue_as_new` refusal is not a defect to fix but the honest
answer for a driver that cannot name a bank. And this driver's own crash safety rests on
`crates/waymaker-flash/tests/swap.rs` and `crates/waymaker-fault/tests/swap.rs`, which
exhaustively sweep the same `Swap`/`Prepared`/`Staged`/`Sealable`/`Installed` typestate this
driver calls unmodified, at every crash point across all seven steps — `swap_in` is new
glue over an already-swept protocol, verified here on the fault-free path
(`crates/waymaker-drive/tests/continue_as_new.rs`: a real two-bank device, a real swap, the
next boot reading back the installed bank's own bytes, its seal naming the right generation,
and a third boot proving that read came from the bank the swap installed rather than from a
stale one; the same file's own refusals — an effect left outstanding, a next-run input over
the bound, a run id at the ceiling — each with the device read back untouched) rather than by
a crash sweep of the driver's own construction of it. A crash sweep through `continue_as_new`
itself — covering the bank-selection reads alongside the seven steps as one driver-level
operation — is not yet part of this workspace's suite, and CLAUDE.md's "what is not checked"
says so.

## Alternatives considered

**Widening `Journal::wait`'s own error to carry the remaining ticks**, instead of a second
query method. Rejected: `Halted` is deliberately uninformative everywhere else it is
returned, and folding "why" into it for one call would have made every other caller of
`Journal`'s four methods start asking whether *their* `Halted` secretly carried something
too.

**A default, no-op body for `Alarm::wake_after`**, so `NoAlarm` could be `impl Alarm for
NoAlarm {}` with nothing to duplicate. Rejected: a default body is a firmware author's silent
no-op waiting to happen — an alarm driver that forgot to override it would compile clean and
never wake anything.

**Declaring `Alarm` and `NoAlarm` in `waymaker-embassy/src/clock.rs`**, beside
`PersistentTimer`, where design document §11's other clock vocabulary already lives. Rejected
on a gate rather than a design ground: `timer-capability` scans that file's text for a name
declared more than once, and a trait's own signature plus its `impl` body in the same file are
two textual occurrences of `wake_after` it cannot tell apart from a genuine duplicate
declaration. A module of their own (`waymaker-embassy::alarm`) sidesteps the scan rather than
arguing with it, matching the split this codebase already keeps elsewhere between a trait's
declaration and its implementation.

**Extending `Driver`'s existing fields with an `Option<BankLayout>`** rather than an internal
`Pointing` enum with two full constructors. Rejected: a `Driver { region: JournalRegion, run:
RunId, layout: Option<BankLayout>, .. }` can be built in states nothing should ever
construct — a `layout` disagreeing with `region`, or present alongside a `run` `boot` would
ignore — where the enum makes the two shapes exhaustive and mutually exclusive by
construction, matching how this driver's own `Source` type — and `waymaker-flash`'s
`Retired`, beside it in `swap_in` — are each shaped for the same reason.

**Deferring `reclaim` and adding a `Driver` method to perform it later.** Rejected for the
same reason ADR 0022 gives `Swap::beginning` no separate "resume" entry point: a caller that
held a half-finished swap across two calls would need somewhere durable to keep which bank
still needs erasing, which is new state this driver does not otherwise carry, for a benefit
— skipping one erase on the boot that installed the new run — that `prepare`'s own
unconditional erase of the installing bank already makes optional rather than required for
correctness.

**A crash-sweep test through `continue_as_new` in this same change.** Considered, and left
for a follow-up rather than rushed: the seven steps it would sweep are already exhaustively
covered one layer down, and a hastily-built driver-level harness risks asserting something
the underlying model does not actually guarantee, which is worse than the gap being named
plainly in CLAUDE.md.
