# 0045. An alarm is armed on a halt, and a driver at a bank can swap

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
`prepare`, `stage`, `payload_barrier`, `commit` and `reclaim` — every step, so a `Migrated`
run leaves no bank behind holding a stale seal. `Progress::Migrated { run }` is the new
answer `boot` gives when this succeeds.

Every field `swap_in` hands to `Swap::beginning` is either read from the device this same
boot (`booted`, `run`) or the caller's own already-supplied `reserve` — closing ADR 0022's
two named preconditions for a caller that only ever swaps through this driver, since neither
value can be a caller-carried, potentially stale copy any more, and closing issue #110's own
third: a capacity check that was nobody's obligation before is now the swap's own first
statement.

## Consequences

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
is ever constructed. A run whose header does not fit `page` is not a candidate at any
generation, exactly as a bank whose seal does not validate is not; both fail the same way,
by `read_bank` answering `None` rather than by a distinguishable error, because a caller
handed a page too small to read its own device's header has nothing this driver can act on
differently.

**`Driver::at_bank`'s `continue_as_new` calls `reclaim` immediately rather than leaving it
lazy.** §10's seventh step does not have to happen before a swap is safe — the retiring
bank's erase is idempotent from the reader's side, because `select` never weighs a bank
whose seal fails to validate, torn erase included — but nothing in this driver has anywhere
to *defer* it to across a call that returns. Doing all seven steps in one call means a
`Migrated` boot leaves the device in the same two-bank shape a fresh device would have had
its second bank provisioned into, rather than one bank sealed twice over with the older
sealing left to a swap that has not happened yet.

**What this does not close.** `Swap::beginning`'s third precondition — that `next.run` is
fresh for the *whole device*, not merely different from the run being retired — is closed
for a device that only ever swaps through this driver, by the same argument ADR 0022's
`RunId::successor()` monotonicity gives; it is not closed for one that does not, and
`SwapError::RunReused` still only catches the adjacent case. Nothing obliges a caller to use
`Driver::at_bank` over `Driver::new` — a region-pointed driver remains a legitimate,
supported shape, and its `continue_as_new` refusal is not a defect to fix but the honest
answer for a driver that cannot name a bank. And this driver's own crash safety rests on
`crates/waymaker-flash/tests/swap.rs` and `crates/waymaker-fault/tests/swap.rs`, which
exhaustively sweep the same `Swap`/`Prepared`/`Staged`/`Sealable`/`Installed` typestate this
driver calls unmodified, at every crash point across all seven steps — `swap_in` is new
glue over an already-swept protocol, verified here on the fault-free path
(`crates/waymaker-drive/tests/continue_as_new.rs`: a real two-bank device, a real swap, the
next boot reading back the installed bank's own bytes) rather than by a crash sweep of the
driver's own construction of it. A crash sweep through `continue_as_new` itself — covering
the bank-selection reads alongside the seven steps as one driver-level operation — is not yet
part of this workspace's suite, and CLAUDE.md's "what is not checked" says so.

## Alternatives considered

**Widening `Journal::wait`'s own error to carry the remaining ticks**, instead of a second
query method. Rejected: `Halted` is deliberately uninformative everywhere else it is
returned, and folding "why" into it for one call would have made every other caller of
`Journal`'s four methods start asking whether *their* `Halted` secretly carried something
too.

**A default, no-op body for `Alarm::wake_after`**, so `NoAlarm` could be `impl Alarm for
NoAlarm {}` with nothing to duplicate. Rejected: a default body is a firmware author's silent
no-op waiting to happen — an alarm driver that forgot to override it would compile clean and
never wake anything — and the one place `NoAlarm`'s own `{}` impl would have needed the
default, `waymaker-embassy/src/clock.rs`, already declares `PersistentTimer::arm` under the
same name family; keeping `Alarm` and `NoAlarm` in a module of their own
(`waymaker-embassy::alarm`) sidesteps that collision without leaning on a default that would
have weakened every other implementor's contract to do it.

**Extending `Driver`'s existing fields with an `Option<BankLayout>`** rather than an internal
`Pointing` enum with two full constructors. Rejected: a `Driver { region: JournalRegion, run:
RunId, layout: Option<BankLayout>, .. }` can be built in states nothing should ever
construct — a `layout` disagreeing with `region`, or present alongside a `run` `boot` would
ignore — where the enum makes the two shapes exhaustive and mutually exclusive by
construction, matching how `waymaker-flash`'s own `Source`/`Retired` types are shaped for
the same reason.

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
