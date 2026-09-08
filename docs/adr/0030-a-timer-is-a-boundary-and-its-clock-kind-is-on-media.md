# ADR 0030: a timer is a boundary, and its clock kind is on media

- Status: accepted
- Date: 2026-09-08
- Issue: [#33](https://github.com/madmax983/waymaker/issues/33)
- Supersedes: nothing
- Related: [ADR 0009](0009-the-transition-table-is-a-machine-that-owns-the-cursor.md),
  [ADR 0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md),
  [ADR 0024](0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md),
  [ADR 0028](0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md),
  [ADR 0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md)

## Context

Design document §09 numbers `TimerScheduled` at 5 and `TimerFired` at 6 and calls both
required at v0.1. Issue [#13](https://github.com/madmax983/waymaker/issues/13) spent the two
numbers with no bodies behind them, so that this change would write a body rather than a
renumbering. [ADR 0028](0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md)
then gave §11 its vocabulary — two deadlines, two clock kinds, a capability and a refusal —
and spent `ClockKind`'s numbers for the same reason. Neither reached media.

§11 states the requirement this closes in one sentence: a persistent timer record includes
its clock kind so recovery cannot silently reinterpret one policy as another. The failure it
describes is the quiet one. A firmware built without an RTC replays a journal an RTC wrote;
no checksum fails, no frame is malformed, and a best-effort substitution wakes the device
early for the rest of its life. Nothing in the workspace could see that, because no timer had
ever been written down.

Issue #33 also asks for something that reads as an implementation detail and is not: timers
must consume the replay cursor in workflow order **alongside** activities — "one ordered
history, not a parallel timer table". Two orderings are two answers to "what did this run do
next", and a `(RunId, EffectSeq)` that names one thing in one table and another in the other
is not the identity §14's fourth guarantee is about.

## Decision

**Two record bodies, and the clock kind is a field.** `RecordRef::TimerScheduled` carries a
sequence, a clock kind, a deadline and the reading it was armed at; `RecordRef::TimerFired`
carries a sequence and nothing else. On media the schedule is a seventeen-byte body — one
kind byte, then two little-endian `u64`s — and the firing has no body at all. The kind byte
comes first so a reader refuses an unknown policy before it reads sixteen bytes it would
throw away. `timer-record-fields` fails a build over either field set, in both directions.

`armed_at` is the field that has to be argued for, because it reads as redundant beside the
deadline. It is the monotonicity floor. `Timer::evaluate` refuses a reading below the reading
the timer was armed at, and that floor lives in RAM — which a power cut takes. ADR 0028
recorded the gap and named this record as what would close it. Without it, an RTC that moved
backwards while the power was absent is invisible on the next boot.

**An unknown clock-kind byte is not a policy.** `TimerSpec::recorded(kind, deadline)` is
total and has no wildcard arm: a number that is neither of the two the format spends has no
spec. The codec refuses such a body with `DecodeError::MalformedRecord`, and the kernel
refuses it with `KernelError::IncompatibleWorkflow` if one ever reaches it. A `_ =>` arm
anywhere on that path would let a zeroed page decode as a boot deadline, which is the
reinterpretation §11 forbids arriving by the shortest route there is.

**A recorded clock this firmware cannot service is `IncompatibleWorkflow`, never a
substitution.** Issue #33 asks for exactly that word, and the distinction from
`NoPersistentClock` is worth keeping: `NoPersistentClock` is this firmware refusing what the
workflow asks for *now*, and `IncompatibleWorkflow` is this firmware refusing history that
already exists. The second is the stronger statement — there is a committed record this image
cannot honour — and it is the one a fleet running mixed firmware will meet.

**Timers share the run's sequence space and its cursor.** `ReplayCursor` gains one state,
`AwaitingTimer`, beside `AwaitingOutcome`; a timer takes its sequence from the same
`EffectIdAllocator` an activity does; and a run has at most one open boundary of either kind.
That is issue #33's "one ordered history" as a representation rather than as a convention: a
second table would need a second allocator, and there is none.

**The §08 boundary is a second pair of calls, not a widened `Intent`.** `ReplayMachine` gains
`timer_intent` and `timer_outcome`, with `TimerRequest`, `TimerIntent` and `TimerResolve`.
`Intent`, `Resolve`, `Next`, `Outcome` and `EffectRequest` are untouched, which is issue
[#28](https://github.com/madmax983/waymaker/issues/28)'s second "done when" — "adding a new
record kind does not change this signature" — surviving the first two of the five bodies it
was written against. `kernel-boundary` pins all eight types now, so a `Resolve::TimerFired`
still fails a build and so does a `TimerIntent::Downgrade`.

`TimerRequest` carries a capability beside the spec, and that is the one thing the timer
boundary has that the effect boundary does not. §02 decision 8 is decided where the deadline
and the hardware meet, and that is the only place it can be decided.

**A mismatch is a divergence with a flavour of its own.** `Divergence::Deadline` is a
different deadline or a different clock than history recorded; `Divergence::BoundaryKind` is
a workflow that asked for a timer where history recorded an activity, or the reverse.
Reporting `Kind` for either would send an engineer to look for a renamed activity.

**The driver half is `waymaker-drive`'s, and reading a clock is the only thing it does to
hardware.** `Clocks` joins `Activities` as the world's half of §06's boundary, and
`Boundary::wait` is the workflow's. That makes issue #33's first "done when" a counted call:
`crates/waymaker-drive/tests/timer.rs` requires a boot that replays a fired timer to read the
clock **zero** times. The second "done when" is a test that flips the kind byte *on media*
and re-seals the frame with the real codec, so what recovery meets is a frame a writer could
have written; the firmware that has the clock refuses it as a divergence, and the firmware
that does not refuses it as an incompatible workflow.

## Consequences

**The code-flash gate is nearly spent, and the number is the point.** ADR 0029 cut the budget
to 12 KiB against a measured 10852 B and said in as many words that this left "1436 B of room
for issue #33's record bodies". They cost **1370 B**: the layers measure **12222 B** of
12288, with 66 B left. That is the budget doing the job it was set for, and it is also a
warning — rung 0.4's `Ctx`, dispatcher and wakeups do not fit under it. What that rung needs
is the accounting issue [#72](https://github.com/madmax983/waymaker/issues/72) did for the
probe, not a raise argued from a figure nobody has taken apart. No raise is asked for here.

**Kernel state goes from 88 B to 104 B of 128.** A timer's recorded state is a clock kind, a
deadline and an arming reading — 24 bytes where an effect's digest is 12 — so `ReplayCursor`
moves from 32 B to 48 B and `ReplayMachine` from 40 B to 56 B. The two are a union rather
than a sum: a run pays for the larger of its two boundary kinds and never for both. Sixteen
bytes of headroom is not much, and it is the same warning as the paragraph above.

**The capacity reserve over-reserves for a timer, deliberately.** `Reserve::exit_bytes_after`
prices a `TimerScheduled` at the same figure as an `EffectScheduled` — the outcome record the
run's bounds declare, plus a terminal record. A `TimerFired` has no payload, so it is never
wider than that: the reserve holds back a few bytes more than a timer needs and never fewer,
which refuses slightly early in the last moments of a bank's life. Pricing it exactly would
be a third term in a sum on a firmware with 66 B of budget left, and the direction the
approximation errs in is the safe one. It is written down rather than left to be discovered.

**A `TimerFired` records no firing time.** A second `u64` on media would answer "when did it
fire", and nothing in the engine reads it: replay hands the workflow back the fact that the
deadline passed and nothing else. It is bytes on the record kind §04's journal can least
afford, and it can be added later behind the same record number, which is what §09's
forward-compatibility rule is for.

**An `AfterBoot` deadline's recorded arming reading is a high-water mark, not a floor, and
what it costs is measured.** `TimerSpec::rearmed_at` is where the difference lives:
`AtPersistentTime` measures from the recorded reading, because the clock that set it survived
the power cut; `AfterBoot` measures from the lower of the recorded reading and the clock now,
because a boot clock reads below its own arming reading only after a reset. Review of this
change found the version without that rule, and it was not a corner: a boot deadline armed at
5000 ticks and met by a reset answered `ClockWentBackwards` on **every** boot afterwards, and
§08 gives a run with an open boundary no way to end. A stranded device on the ordinary path.

What the rule buys is that the interval accrues within a power cycle and restarts across one.
What it does not buy is precision. A boot clock offers no evidence that a reset happened, so
once the new cycle climbs back past the old mark the interval accrues from it: a 1000-tick
deadline armed at 5000 is reached at 6000 ticks of the new cycle rather than at 1000.
`a_boot_deadline_carried_across_a_reset_waits_longer_than_it_asked_for` measures exactly that
rather than leaving it in prose. §11 calls this deadline not power-loss durable and this is the
shape that takes; a reset-cause register or retained RAM would close it, and both are a
board's — issue [#34](https://github.com/madmax983/waymaker/issues/34) is where a real one is
met.

**The synchronous driver polls; it does not sleep.** `Progress::WaitingUntil` carries the
ticks still owed so a caller with a sleep can use them, and this driver has none. §11's
in-boot sleep and a dispatcher that arms a hardware alarm are rung 0.4's — and a dispatcher
that armed one would have a physical act to order behind §07's barriers, which this driver
does not.

**Nothing obliges a firmware to declare its capability honestly.** `Clocks::capability` is the
firmware's word, exactly as `ClockCapability` was in ADR 0028. `waymaker-embassy`'s
`PersistentTimer` is still the only path where the declaration is witnessed by a clock the
caller holds, and nothing obliges a caller to take it. Joining the two by construction is rung
0.4's dispatcher, which is the same standing as "nothing obliges a future dispatcher to use
the gated writer".

**Every exhaustive `match` on `RecordRef` in the workspace had to grow two arms.** That is the
compiler doing the work a wildcard would have hidden, and the arms are written out rather than
folded into a `_`, so the next record body is a decision at each site instead of a default.

## Alternatives considered

**`RecordRef::TimerScheduled { seq, spec: TimerSpec, armed_at }`.** Better typed, and it
cannot represent an unknown clock kind — which sounds like a feature and is the problem. The
decoder would have to refuse an unrecognised byte *before* it could build the view, so the
refusal would live in the codec alone and the kernel could not state it. Raw fields let both
layers refuse, each in its own vocabulary, and keep the encoder a byte copy.

**Widening `Intent` and `Resolve` with timer variants.** The shortest change, and the one
`kernel-boundary` exists to refuse: CLAUDE.md names `Resolve::TimerFired` by name as the
shape that turns one boundary into a boundary per record. Five record bodies remain unwritten
and each would arrive the same way.

**A separate timer sequence space.** It would have made the two boundaries independent and
each simpler. It also makes `(RunId, EffectSeq)` ambiguous, which is the pair every downstream
system deduplicates on, and it gives one run two orderings that a replay has to reconcile.
Issue #33 rules it out in as many words.

**Recording the firing reading in `TimerFired`.** Considered and not taken; see Consequences.

**Pricing a timer's tail exactly in the capacity reserve.** A third stored figure and a third
term in `exit_bytes_after`, for a few bytes of journal in the last moments of a bank's life,
on a firmware with 66 B of code budget left. Not taken; see Consequences.
