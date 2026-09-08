# ADR 0031: a persistent clock is two registers, and the board run is a checked absence

- Status: accepted
- Date: 2026-09-08
- Issue: [#34](https://github.com/madmax983/waymaker/issues/34)
- Supersedes: nothing
- Related: [ADR 0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md),
  [ADR 0028](0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md),
  [ADR 0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
  [ADR 0030](0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md)

## Context

Design document §16 makes issue [#34](https://github.com/madmax983/waymaker/issues/34) rung
0.5's exit criterion. §11 owes it three things: a `PersistentClock` over a real board RTC, a
power-loss run that arms an `AtPersistentTime` deadline and finds it elapsed after the supply
has been away for longer than the interval, and the same workflow refused on a board that has
no such clock.

Everything above the driver already exists. ADR 0028 gave §11 its vocabulary, ADR 0030 put a
deadline and its clock kind on media, and `waymaker-drive/tests/timer.rs` drives the boundary
end to end. What none of them has is hardware. Every reading in the workspace comes from a
world whose epoch a test sets, and a clock that is a field in a struct never fails, never
loses its battery, and never rolls over.

The failure that matters is the quiet one, and it is not in the arithmetic. A backup domain
that lost power leaves the RTC counter at its reset value. On most parts that value is zero,
and zero is below every instant a workflow ever waits for — so a driver that reported the
number would fire every persistent deadline on the device at once, on the boot after the
battery died, with no checksum failing and no record malformed. §02 decision 8 forbids exactly
that substitution, and nothing in the workspace could have caught it, because no driver had
ever been written.

There is a second path §11 names and nothing implements: a device with no RTC that gets its
time from a network. Its semantics are different from an RTC's in one way that decides
everything, and the difference had only ever been a phrase in a doc comment.

## Decision

**A persistent clock is two registers, and a board brings both.** `BackedRtc` declares
`counter` and `continuity`; `waymaker_rig::rtc::Rtc` is the driver over them. The continuity
bit is the one every part has and every abstraction drops — `OSF` on a DS3231, `INITS`/`RSF`
on an STM32 backup domain, a power-fail latch elsewhere. It is a register the board reads, and
it may not be inferred from the counter: a counter that looks plausible after a battery change
is the failure this trait exists to report.

**A reading the driver cannot vouch for is a fault, never a number.** `Rtc::now` returns
`RtcFault::ContinuityLost` when the domain broke and `RtcFault::Register` when either read
failed. The pinned surface holds it there: `timer-capability` now reads
`waymaker-rig/src/rtc.rs` and `waymaker-rig/src/epoch.rs` as well, so an `Rtc::assume_held` or
an `Rtc::counter_unchecked` is a line a reviewer writes on purpose. That pin is the one that
had to be added rather than argued for, because such an accessor breaks no layering rule,
needs no dependency, and passes every other gate.

It is four checks rather than one, and the extra three were bought the way the kernel half's
were. Review of this change landed a `pub(crate) fn counter_unchecked` on `impl Rtc`, a
`pub registers` field on `Rtc`, and a `pub const ASSUME_HELD: Self = Self::Held` on
`impl Continuity`, and watched a surface pin stay green through all three: it counts `pub `
and not `pub(`, a public field adds no function, and a constant declared on the *enum a driver
answers with* is not in any driver's `impl` body. So the board half pins each driver's methods
at every visibility, refuses a public field on it, and refuses any constant in the module,
read over the whole file. CLAUDE.md had already recorded the first two defeats against the
kernel half; the board half was written without the countermeasures they bought, which is the
way a rule normally rots.

**The counter is read first and the continuity bit second.** A supercapacitor that browns out
*during* the counter read latches the bit, so a read that follows the counter catches a break
the counter met and a read that precedes it cannot. The order reads as arbitrary and is not,
so `continuity_is_read_after_the_counter` pins it.

**An externally-restored epoch is a second driver, not a paragraph.**
`waymaker_rig::epoch::RestoredEpoch` anchors an epoch the firmware supplies to a monotonic
reading and advances it from there. What it says about power loss is the whole documentation
of that path: the anchor is in RAM, a cut takes RAM, and so `now` answers
`EpochFault::NotRestored` until the network has answered again. A device on this path never
fires a durable deadline early on the strength of a zero, and a device that never reaches the
network never fires one at all — which is the honest outcome for a device that has no clock
and no time.

**A driver decides no policy.** Neither module may name a `TimerSpec` or a `ClockCapability`,
and neither may name `AfterBoot` or `BootOnly`, under the same `timer-capability` id. A clock
reports a reading; which deadline that reading meets is `waymaker-core`'s.

**The drivers live in `waymaker-rig`, above the layers.** Two reasons, and the second decided
it. They are board support for hardware Waymaker does not ship, which is `waymaker-rig`'s own
standing under ADR 0021. And a layer pays for every public function it declares against §04's
code-flash budget: ADR 0030 left **66 B** of 12288, so a driver in `waymaker-embassy` would
have arrived with a budget raise attached to it. The capability stays in the façade, where §11
puts it; the concrete driver sits beside the rig that cuts the supply.

**The board run itself is a `HARDWARE_TARGETS` row, not a claim.** `rtc-power-loss` joins
`cortex-m0plus` and `cortex-m4`, `NotRun`. `waymaker-drive/tests/power_loss.rs` drives the
whole scenario — arm, cut, restore, replay — and it drives it on a host. Moving the row needs
an accepted ADR carrying the attestation marker and the id, in the same change.

## Consequences

**The power cut is a function boundary rather than a comment.** `power_loss.rs`'s `power_up`
takes the media and the backup domain and builds everything else itself: the board, the
driver, the workflow, both buffers. So the only state that crosses a cut in that file is state
a part really keeps — bytes on NOR, and a counter a battery holds up. RAM is gone because no
value is left holding it. That is ADR 0021's split at the reset boundary, applied to a driver.

**The code-flash budget does not move.** The drivers are not layers, so `cargo xtask size`
measures what it measured before. The 66 B ADR 0030 left is still 66 B, and rung 0.4 still has
the accounting problem ADR 0029 named.

**A firmware built on Waymaker gets no RTC driver from this workspace.** `waymaker-rig` is
`publish = false` and is never linked into anything Waymaker ships. What a firmware author
gets from this change is `waymaker-embassy`'s `PersistentClock` — which existed already — and
two worked implementations to copy, with the register that matters named and a test suite that
runs on a host. That is less than issue #34's first work item reads as, and it is the honest
consequence of the placement above. If a shipped driver is wanted later it is a crate of its
own beside the façade, not a public function added to a layer with 66 B of budget left.

**`waymaker-rig` now depends on `waymaker-embassy`.** The edge is legal because
`dependency-direction` and `embassy-below-facade` both read `policy::LAYERS`, and this crate is
not a layer. It is *not* the same standing `waymaker-rig`'s dependency on
`waymaker-conformance` has: that one is a dev-dependency between two test-support crates, and
this is a normal dependency onto the one crate `policy::is_embassy_package` returns true for.
The consequence is dated rather than absent. `policy::LAYERS` records that rung 0.4 gives
`waymaker-embassy` a real `may_depend_on_external` of Embassy crates, and from that day the CI
stage `cargo build -p waymaker-rig --lib --target thumbv6m-none-eabi` links the Embassy
ecosystem to build a rig that has no use for it. The fix when it arrives is a feature on this
crate or a third module home, and it is cheaper to write that down now than to meet it as a
build-time surprise.

**A 32-bit counter that rolls over reads backwards, and that is the documented answer.** The
driver reports the register. It has no epoch to widen a wrapped counter with, because RAM did
not survive, and inventing one would be the substitution this ADR is about. So a wrapped
counter reads below the arming reading the record carries, and the kernel refuses the interval
it cannot measure. `a_counter_that_rolled_over_is_refused_rather_than_credited` measures it. A
part whose counter can wrap inside a deadline needs a wider counter or a workflow that does
not ask for one.

**A `continuity` bit that lies is invisible.** The driver believes the register, exactly as
`Timer::arm` believes a declared `ClockCapability` and `Swap::beginning` believes its two
arguments. A board whose implementation returned `Held` unconditionally would fire every
deadline after a battery change, and nothing below the board can see it. That is the same
standing every hardware fact in this repository has, and it is why the board run is a row.

**The two drivers cannot be told apart by a `PersistentTimer`.** `PersistentTimer<C>` carries
the clock type, so an RTC timer cannot be polled with an epoch clock. What it still cannot
tell apart is two *instances* of one driver — the limit ADR 0028 recorded, unchanged here.

**Neither driver sleeps or arms an alarm.** They answer a reading. §11's in-boot sleep and a
dispatcher that arms a hardware alarm are rung 0.4's, as ADR 0030 already records.

**A monotonic tick must already be in the epoch's unit, and nothing checks it.**
`RestoredEpoch` adds a tick count to a restored reading and never converts, for the kernel's
reason: a conversion needs a rate, and a rate nobody checked is a clock that runs fast. A board
whose epoch is seconds and whose timer counts 32 kHz must divide before it answers, and every
arithmetic guard in the module passes on a reading 32768 times too large. Stated on
`Monotonic::ticks`, and it is a board's obligation the way the continuity bit is.

**The board half pins one file each, so a sibling module is still a door.** Review of this
change put a `macro_rules!` in `waymaker-rig/src/window.rs` and invoked it inside
`impl<R> Rtc<R>`; it expands to a public inherent method returning the raw counter, and the
gate stays green. That is the limit CLAUDE.md already records for the kernel and the façade
halves, met again rather than a new one, and closing it needs a scanner that expands macros
rather than a longer list. It is written down because the alternative is a claim in this ADR
that the pin is stronger than it is.

## Alternatives considered

**A concrete driver for one part, over `embedded-hal`.** A DS3231 or an STM32 backup domain,
written out. It would be the most convincing thing to read and the least useful thing to have:
a third-party dependency and a register map for a part nobody in this repository owns, testable
only on that part. Splitting at the two registers keeps everything above them host-testable and
leaves the board with the two reads it alone can do — which is `Cutter` and `Dispatcher`'s shape
in the same crate.

**The driver in `waymaker-embassy`.** §11 puts `PersistentClock` there, so the driver reads as
belonging beside it. It would have cost a code-flash raise on a budget with 66 B left, for a
driver Waymaker does not ship. The capability stays; the driver does not.

**A `RestoredEpoch` whose only guard is its anchor.** The first version compared a reading
against the tick the epoch was anchored at, and nothing else. Review found two failures in it,
and the second is the one worth recording: an anchor stops detecting a boot clock that reset
the moment the clock climbs back past the anchor tick, so a reading already given as 1400 is
followed by an `Ok(1100)` — and a deadline armed under that reading fires *late* rather than
being refused, because `Timer::evaluate` floors at the recorded arming reading and 1100 clears
it. The first was the mirror image: propagating the anchor's own failure out of `restore` locks
a device out of the re-sync that is the remedy for the state it is in. One floor answers both —
the highest reading produced or anchored to, standing whether or not the anchor still
evaluates.

**Inferring continuity from the counter.** "A counter below the last recorded reading means the
battery died" needs no second register and is wrong in the direction that hides bugs: a battery
changed while the supply was away can leave a counter *above* the recorded reading, and a run
that meets one credits an interval that never passed. The bit is in the hardware. Reading it is
cheaper than guessing at it and is the only answer that is right.

**Documenting the externally-restored-epoch path in prose.** Issue #34 asks for documentation
and not for a driver. Prose is the thing this repository is built to distrust: a path with no
implementation has no test, and `an_epoch_nobody_restored_is_a_fault_rather_than_a_zero` is a
sentence that fails a build. Sixty lines bought that.

**Claiming the board run.** The scenario passes on a host, and the row could have said so.
`HARDWARE_TARGETS` exists because a green CI is a thing somebody reads as finished, and a
measurement that did not happen is not a measurement that passed.
