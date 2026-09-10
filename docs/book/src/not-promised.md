# What is not promised

Read this chapter before you build on Waymaker. Each item below is a decision, not a gap to
close later. The engine records each one as well as this page.

## `at-least-once-effects` — no exactly-once physical effects

Waymaker delivers an effect **at least once**. Power can fail after an activity changed the
world and before the outcome record is durable, so the next boot performs it again. A retry
does the same.

Every attempt carries one identity, the `(RunId, EffectSeq)` the schedule record committed.
Two things give you exactly-once, and both are outside this engine: make the activity
idempotent, or deduplicate on that pair downstream.

No setting changes this.

## `no-snapshotted-futures` — no persisted stacks or suspended futures

Waymaker writes records. It does not write a suspended `async fn`, a stack, or a heap. A run
resumes by re-executing from its beginning, so the code between two effects runs again on
every boot.

Waymaker reclaims history only at an explicit `continue_as_new` boundary. Neither driver in
this repository calls it yet, so today Waymaker reclaims no history at all. See issue
[#110](https://github.com/madmax983/waymaker/issues/110).

This promise is flat for the `async` façade, and it stays flat. Design document §16 leaves
one question open: whether a future explicit-state, non-`async` API could take a real storage
snapshot. That question does not relax this promise for `async fn` workflows.

## `no-distributed-consensus` — no distributed consensus

One device decides. There is no quorum, no leader election and no replication. Two banks on
one part give atomic replacement, not agreement between machines. A fleet that needs a
single global decision needs a service that makes it.

## `boot-timer-is-not-power-loss-durable` — no AfterBoot timer surviving power loss

A boot clock restarts when the supply restarts. Waymaker will not claim that time passed
while the power was away.

An `AfterBoot` deadline therefore measures against a reading from a power cycle that is gone.
After a cut it can wait **longer** than the interval it asked for. The worst case is the old
arming reading plus the interval, on a clock that restarted at zero. It never waits a shorter
time.

For a deadline that must survive the cut, ask for `AtPersistentTime`. Give the firmware a
clock that survives the cut: a backed RTC, or an epoch that a network restores. Waymaker
refuses a firmware with no such clock, and answers `NoPersistentClock`. It never serves the
deadline from the boot clock instead.

## `no-downgrade` — no downgrade past a record kind a device has already written

The promise runs one way. Records a shipped device wrote stay readable by every later 1.x
firmware. Older firmware that meets a newer record kind stops.

A rollback past a new record kind loses data. Recovery reads the unknown record as damage,
and `continue_as_new` recycles a damaged bank.

## Also not on offer

- **No allocator, and no dynamic workflow loading.** A number selects an activity. There is
  no string-addressed registry.
- **No timer that fires while the device is off.** Waymaker recognises a durable deadline as
  elapsed on the first replay after power returns. It does not act at the instant the
  deadline passed.
- **No hardware guarantee this repository has not measured.** See
  [the hardware compatibility matrix](hardware-matrix.md).
