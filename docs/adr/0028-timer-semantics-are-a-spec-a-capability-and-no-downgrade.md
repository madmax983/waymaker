# 0028. Timer semantics are a spec, a capability, and no downgrade

- Status: accepted
- Date: 2026-09-07

## Context

Design document §11 opens with a hardware fact: "A monotonic MCU timer usually returns to
zero after reset. It cannot reveal how long the device was unpowered." §02 decision 8 turns
that into a rule — `durable-timers-need-durable-time` — and issue
[#32](https://github.com/madmax983/waymaker/issues/32) asks for the two types that carry it:
`TimerSpec`, with an `AfterBoot` deadline and an `AtPersistentTime` one, and a
`PersistentClock` capability.

The failure this exists to prevent is not an arithmetic bug. It is a *convenience*. A device
with no RTC asks for a deadline that survives power loss, and the engine gives it the nearest
thing it has. The result is a delay that restarts on every reset, in firmware whose author
believes it does not, and nothing in the journal records the substitution. §11's own sentence
about the record — "so recovery cannot silently reinterpret one policy as another" — is about
the same failure one layer down.

Two more failures are in the same family, and issue #32's third work item names both. A clock
that cannot be read is easy to paper over with a default reading: a zero fires every
persistent timer at once, a maximum fires none of them, and both look like working code. A
clock that reads *backwards* — a battery change, an epoch re-synchronisation, or a reset on
the boot clock — makes elapsed time unknowable, and crediting or discarding the interval is a
guess either way.

Where the two halves live is settled by the layering rather than by preference.
`waymaker-core`'s must-not-own cell names a clock, and `PersistentClock::now` reads hardware,
so the capability sits one layer above the kernel for the reason `StableStorage` does.
`waymaker-flash` is not the home either: its own must-not-own cell names timers.

## Decision

Design document §11's semantics are `waymaker-core::timer`. The capability is
`waymaker-embassy::clock`. Neither can downgrade the other, and three separate mechanisms
hold three separate claims.

**A timer carries the spec it was armed from.** `Timer` holds a `TimerSpec` and the clock
reading it was armed at, and nothing else. `Timer::arm` takes a spec, a `ClockCapability` and
a reading, and has two possible answers: a refusal, or a timer for that spec. It has no third
answer because it holds no second spec to return. So "never a silent downgrade" is a shape
rather than a discipline, and `arming_never_changes_a_spec_into_another_one` states it over
every spec and every capability rather than over the one pair a downgrade would have used.

**The refusal names the missing capability.** `ClockCapability::BootOnly` admitting an
`AtPersistentTime` spec is `KernelError::NoPersistentClock`, whose message is "this firmware
has no persistent clock". Issue [#34](https://github.com/madmax983/waymaker/issues/34) asks
for exactly that, and `IncompatibleWorkflow` — which issue #33 correctly uses for a *recorded*
clock kind this firmware cannot service — says only that the workflow cannot be replayed.

**A persistent deadline needs a clock in hand, and the clock that armed it.**
`PersistentTimer::<C>::arm` takes `&mut C where C: PersistentClock` and is the only
constructor. Firmware with no such type cannot write the call at all, which is issue #32's
compile-time half; `ClockCapability` is the runtime half, for a firmware that declares what it
has. The clock type stays in the timer, so `poll` accepts no other driver — a `compile_fail`
doctest beside a compiling twin is what states that. Codex found the version without it on the
first review round, and the finding was right: an RTC counting milliseconds since an epoch and
a network clock counting seconds since boot are both `u64`, so a timer armed by one and polled
by the other fires early or late with nothing to say so.

`evaluate` adds nothing. An `AfterBoot` deadline compares the interval against
`reading - armed_at`, and an `AtPersistentTime` one compares the reading against the instant.
Both subtractions happen after the reading has been checked against the arming reading, so no
input wraps a deadline into the past or into a future that never arrives. A reading below the
arming reading is `KernelError::ClockWentBackwards` rather than a credited or a discarded
interval. Its message says "a clock read below a reading already accepted" rather than naming
the arming reading, because the two callers compare against different floors: `Timer::evaluate`
against the arming reading, which is all a `Copy` value can remember, and
`PersistentTimer::poll` against the highest reading it has been shown. Codex found the message
naming only the first, which would have sent a firmware log to the wrong regression.

`ClockCapability::admits` names every pair and uses no `_`. That is the finding this change's
own review turned up, and it inverts the reasoning: with a wildcard, a third spec added later
would have been *admitted* by a firmware with no persistent clock — the silent downgrade this
module exists to forbid, arriving in the one place that decides policy, while `clock_kind` and
`evaluate` failed to compile and named the two places that do not. The kernel's error
vocabulary refuses `#[non_exhaustive]` for the same reason, and this is that rule applied to a
policy table. It is also what makes the gate's residual limits tolerable: a third policy is now
a compile error at the arm that would have permitted it, whatever a scanner can or cannot see.

`PersistentTimer` keeps a high-water mark rather than trusting `Timer::armed_at` alone. Review
found that a clock which moved back *after* a poll, but stayed above the arming reading, was
believed: a deadline reported `Elapsed` and then `Remaining` on the next look. A timer that
un-fires is worse than one that never fired, and `PersistentClock`'s contract — "poll catches
it" — was not true without this.

The size probe drives a spec whose *discriminant* is opaque, not merely one whose fields are.
Codex found the version that boxed only the `ticks`: `TimerSpec::AfterBoot` was then a
compile-time fact, so `admits` folded to `Ok(())`, the `NoPersistentClock` refusal was
unreachable, and the `AtPersistentTime` arms of `clock_kind` and `evaluate` were dead. The row
measured the boot half of §11 and reported it as §11. The correction is 40 B — 18338 B to
18378 B, before the message fix below took it to 18386 B — which is the size of the answer,
and the reason the finding was a P1 rather than a tidy-up: 40 B of unmeasured code against
what was then 94 B of headroom.

`ClockKind` spends two numbers now — 1 for the boot clock, 2 for the persistent one, and zero
for neither, so an erased or zeroed field does not decode as a policy. That is the same move
`RecordKind` made for the five records it cannot decode: issue #33 writes a record body rather
than a renumbering.

The `timer-capability` gate rule is what stops the shape being given back. It pins the
semantics module's public surface, the members of `TimerSpec`, `ClockCapability` and
`Deadline`, and the capability module's surface — each in both directions, so a module renamed
or deleted is a pin that has stopped checking. It also refuses two identifiers in
`waymaker-embassy/src/clock.rs`: `AfterBoot` and `BootOnly`. A module that exists because a
boot clock is not good enough has no honest use for either.

Three of its halves are there because review defeated the version without them, and each was
watched passing on a mutation before it was closed. A `pub(crate) const fn arm_or_downgrade`
on `impl Timer` — the same mutation ADR 0025's review used on `DurableIntent` — because a
surface pin counts `pub ` and not `pub(`, and `pub(crate)` is reach enough for rung 0.4's
`Ctx`; the method sets are read at every visibility now. A `pub spec` field on `Timer`, which
adds no function and changes no member and makes the invariant the whole design rests on a
value any caller can set. And the sharpest: `pub const BEST_EFFORT: Self = Self::AfterBoot
{ ticks: 0 }` on `impl TimerSpec`, reached from `arm` as `TimerSpec::BEST_EFFORT` behind a
plausible "epoch not restored yet" guard — neither file named a banned identifier, no surface
changed, and the whole pipeline was green on a persistent deadline served by a clock that
restarts on every reset. An identifier blacklist closes one spelling at a time, so the pin is
positive instead: `clock.rs` must name a spec, and every spec it names must be
`TimerSpec::AtPersistentTime`.

## Consequences

Rung 0.5's first item is done, and the two "done when" tests are what state it:
`an_after_boot_timer_restarts_its_interval_after_a_reset` arms an interval, reads it part-way,
then re-arms from the same spec against a boot clock that has returned to zero — which is what
a reset leaves — and requires the whole interval to start again after 1 040 ticks of total
powered time. Nothing here performs a reset, because no record yet carries a timer across one;
`a_timer_armed_before_a_reset_refuses_a_reading_from_after_it` covers the timer that somehow
survived one. `an_at_persistent_time_timer_is_elapsed_when_the_restored_epoch_is_past_its_instant`
does the same across a power loss and requires the first look to say `Elapsed`.

The `facade` row of `cargo xtask size` measures something for the first time. It read 0 B and
carried a standing notice — "either it costs nothing, or `waymaker-size-probe` does not reach
any code the feature adds" — because `waymaker-embassy` declared no code. It now reads 292 B,
and the notice is gone.

The code-flash figure is the number worth recording. §11's vocabulary cost **284 B**: 18102 B
to 18386 B against the same 18 KiB gate,
[ADR 0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md) asked for no
third raise and there is none. What that leaves is 46 B of headroom, which is not enough for
rung 0.5's remaining work: issue #33's two record bodies and their codec will not fit under
it. A third of the measured figure is still the size probe's own arithmetic rather than the
engine's, which is issue [#72](https://github.com/madmax983/waymaker/issues/72), and that
issue is now the binding constraint on this rung rather than a tidy-up.

`Timer` is registered in `kernel_state_types!` at 24 B, taking §04's kernel state from 64 B to
88 B of 128 B. It is registered beside the replay machine rather than inside it because a
pending timer is live independently of what the machine is doing; when rung 0.4's `Ctx`
contains both, the registry replaces the entries rather than adding beside them.

What is owed, stated so that a green build does not imply it:

- **A backwards clock is only caught within one arming.** The floor is `Timer::armed_at`,
  which lives in RAM. A persistent clock that moved backwards *while the power was off* is
  invisible, because the run re-arms from the spec on the new boot and has nothing to compare
  the new reading against. Issue #33's `TimerScheduled` record is what carries the floor
  across a reboot, and it is one of the reasons that record has to hold the arming reading as
  well as the deadline.
- **Two instances of one driver are one clock to the compiler.** The type parameter closes
  the two-drivers case and not the two-RTCs-on-one-board case, nor one driver re-created with
  a different epoch. It is the same limit `waymaker-flash` records for a `Geometry`, and the
  same fix would close it — bind the clock by a borrow rather than by a type — except that a
  deadline holding a borrow of its clock could not be held across the wait it describes.
- **Nothing obliges a caller to go through the capability.** `Timer::arm` with
  `ClockCapability::Persistent` is available to a firmware that declares it and does not have
  it. The declaration is the firmware's word, exactly as `Swap::beginning`'s two arguments
  are; the façade is where the word is witnessed by a clock, and joining the two by
  construction is rung 0.4's dispatcher.
- **No record, and no in-boot sleep.** Issue #33 owns `TimerScheduled` and `TimerFired`, and
  issue #34 owns the RTC driver and the board test. §11's "the Embassy adapter may also offer
  ordinary in-boot sleep" arrives with the dispatcher at 0.4; there is no sleep here to
  document as not power-loss-durable, and the `AfterBoot` variant carries that sentence
  instead.
- **The member pin reads a header string.** A rename that carries the crate root with it —
  the shipped `TimerSpec` becomes `TimerSpecV2`, a decoy `mod compat` keeps the pinned name
  and the pinned members — leaves `TIMER_TYPES` comparing a type nobody ships. Review ran it.
  The crate-root check closes the careless version (the rename loses its re-export, or does
  not compile), and what closes the *dangerous* version is not the gate at all: a third policy
  has to be handled in `admits`, which no longer has a wildcard, so it is a line a reviewer
  reads. `kernel-boundary` shares the reader and the limit.
- **`ClockKind`'s numbers are not pinned.** The surface pin counts functions and the member
  pin names three enums, so `pub const AFTER_BOOT: Self = Self(1)` — the one thing here that
  reaches media — can be renumbered with the gate green. `each_spec_reports_the_clock_kind_its_record_will_carry`
  is what holds them, and issue #33 is where they become a wire format worth a pin.
- **A `Timer` handed out can be evaluated with any reading.** `PersistentTimer::timer` returns
  the kernel type, and `Timer::evaluate` is public and takes a bare `u64`, so a caller that
  goes around `poll` can measure a persistent deadline with a boot reading. The accessor
  exists because issue #33 has to record what was armed; `poll` is the path that carries the
  clock, and nothing obliges a caller to take it — the same standing as the reserve and the
  witness above.
- **The gate compares names.** An `admits` that stopped consulting its argument, or an
  `evaluate` that credited an interval it could not measure, are invisible to
  `timer-capability` and are `crates/waymaker-core/tests/timer.rs`'s. Each half pins one file,
  so an `impl Timer { pub fn force(..) }` in a sibling module adds the door with the rule
  silent — the same limit `capacity-reserve`, `recovery-surface` and `storage-contract` each
  record of the one file they pin.
