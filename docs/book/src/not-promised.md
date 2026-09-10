# What is not promised

Read this chapter before you build on Waymaker. Each item below is a decision, not a gap to
be closed later. Each one is recorded in the engine as well as here.

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

History is reclaimed only at an explicit `continue_as_new` boundary.

## `no-distributed-consensus` — no distributed consensus

One device decides. There is no quorum, no leader election and no replication. Two banks on
one part give atomic replacement, not agreement between machines. A fleet that needs a
single global decision needs a service that makes it.

## `boot-timer-is-not-power-loss-durable` — no AfterBoot timer surviving power loss

A boot clock restarts when the supply does. Waymaker will not claim that time passed while
the power was away, so an `AfterBoot` interval starts again after a power cut.

For a deadline that must survive the cut, ask for `AtPersistentTime` and give the firmware a
clock that survives it: a backed RTC, or an epoch a network restores. A firmware with no
such clock is refused with `NoPersistentClock`. It is never quietly served by the boot
clock.

## `no-downgrade` — no downgrade past a record kind a device has already written

The promise runs one way. Records a shipped device wrote stay readable by every later 1.x
firmware. Older firmware that meets a newer record kind stops.

A rollback past a new record kind is a data-loss operation: recovery reads the unknown
record as damage, and a damaged bank is exactly what `continue_as_new` recycles.

## Also not on offer

- **No allocator, and no dynamic workflow loading.** Activities are selected by number. There
  is no string-addressed registry.
- **No timer that fires while the device is off.** A durable deadline is recognised as
  elapsed on the first replay after power returns, not at the instant it passed.
- **No hardware guarantee this repository has not measured.** See
  [the hardware compatibility matrix](hardware-matrix.md).
