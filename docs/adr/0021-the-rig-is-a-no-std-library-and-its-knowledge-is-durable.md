# 0021. The rig is a `no_std` library, and what it knew survives the cut

- Status: Accepted
- Date: 2026-09-05

## Context

Design document §15 opens by saying that crash testing is part of the design rather than a
post-MVP hardening phase, and §16's rung 0.2 exit criterion is issue
[#27](https://github.com/madmax983/waymaker/issues/27): a concrete `StableStorage` adapter
over real NOR flash, a power-cut rig that cuts supply at randomised points during schedule,
dispatch and completion writes, watchdog-reset tests at the same three points, and erase
counts and per-effect write amplification recorded across the run. It is done when the loops
pass on one Cortex-M0+ board and one Cortex-M4 board, when the write amplification is
published alongside the size report, and when any recovery violation is reproducible from the
rig's log.

Two of those four work items were already answered when this was written. The NOR adapter is
[`waymaker_conformance::nor::NorFlashStorage`](../../crates/waymaker-conformance/src/nor.rs),
landed for issue #21 and argued in
[ADR 0016](0016-the-storage-contract-is-a-conformance-suite-and-a-port.md): an
`embedded_storage::nor_flash::NorFlash` presented as §12's `StableStorage`, validating against
its own `Geometry` before the driver is reached, with `embedded-storage` a dependency above
the layers rather than a kernel one. And `waymaker-fault` already interrupts a writer at every
byte of every program and every block of every erase.

So the question this record answers is narrower and sharper than "how do we test crashes". It
is: **what is a rig, given that we already have an exhaustive host-side crash sweep?** If the
answer is "the same thing, on a board", the rig is a rewrite of `waymaker-fault` and issue #27
is a scheduling problem. It is not, and the difference is one sentence long.

`waymaker-fault` keeps its ledger — which records were attempted, which were acknowledged,
which effects were dispatched — in RAM, because on a host the "crash" is a return from a
function. **A power cut takes RAM.** On a board, the only thing left after the cut is media,
and media is the thing under test rather than the thing that judges it. Two of §14's six
guarantees are statements about the *writer* and cannot be read off a journal at all:
`acknowledged-durability` is about barriers that returned, and `durable-intent` is about
effects that were physically dispatched. Neither is a fact about bytes.

## Decision

**`waymaker-rig` is a `#![no_std]`, allocation-free, test-support crate above the layers, and
its knowledge is durable.**

Five parts of that are load-bearing.

**It is `no_std` and allocation-free** for a sharper reason than `waymaker-conformance`'s. A
rig that could only run on a host would be a simulation wearing a rig's name; §15 asks for
supply cuts during real writes to real NOR, so the code that performs them has to be code a
board can link. It is a test-support crate rather than a layer for
[ADR 0013](0013-the-fault-harness-is-a-crate-above-the-layers.md)'s reason: nothing here
belongs in an 18 KiB code-flash budget, and `size-probe-reach` would otherwise oblige the
probe to link a rig.

**It keeps a durable witness.** Before each record's first program the rig marks
`Attempted`; after the record's commit barrier returns it marks `Acknowledged`; before each
physical effect it marks `Dispatched`. Each mark is one program unit, carrying an iteration, a
record index, a stage byte that is never `0x00` or `0xFF`, and a sixteen-bit check computed
with the same `IntegrityCheck` the firmware seals frames with — so erased, torn and written
are three different answers rather than two.

The *order* of those three marks is the whole design, and each is chosen for which way an
interrupted mark is allowed to be wrong. A torn `Acknowledged` under-claims, so the rig
demands less of recovery than it might have; an under-claim can only fail to catch a bug. A
torn `Attempted` under-claims harmlessly, because a mark that did not land is a record whose
first program had not begun. `Dispatched` is written *before* the effect precisely so that it
**over**-claims: an effect the rig may not have got round to performing still has its schedule
record demanded of recovery, and demanding more than happened is the safe direction for an
instrument.

**The run and the cut point are pure functions of a seed and an iteration number.** SplitMix64,
indexed rather than iterated, so iteration 900's cut is computable without replaying 899 —
which is what lets a rig resumed after a reset carry one counter rather than a generator state
a power cut could have torn. It is also the whole of issue #27's third "done when": a log line
carrying two numbers carries the run, the workload, every payload and the cut point.

**A power cut and a watchdog reset are modelled differently, not relabelled.** They differ in
what the flash controller was allowed to finish: a brownout stops a program inside a program
unit, leaving a torn write; a watchdog reset leaves the supply up, so the unit already handed
to the controller completes or is abandoned whole, and RAM is not cleared. That partitions
`waymaker_fault::injections` exactly — `Progress::Bytes` against `Progress::{None, Whole}` —
and the census requires both partitions to be non-empty at all three write points.

**Erase counting is a decorator, not a field on `WriteAmplification`.** `wear::Metered` is a
`StableStorage` that counts what the device beneath it was asked for, and it attributes the
rig's own witness traffic separately from the engine's. A fifth field on
`waymaker_flash::append::WriteAmplification` would be reached by the size probe, measured by
`cargo xtask size` and paid for by every shipped image, in exchange for a number only a rig
reads. Separating the traffic matters for a second reason: publishing the instrument's cost
inside the engine's write amplification would be a figure that gets worse the more carefully
you measure.

**The two board runs are a checked obligation rather than a silence.** `xtask::docs::HARDWARE_TARGETS`
holds them, both `Not run`, and the `hardware-attestation` rule compares that table against
`CLAUDE.md` in both directions. Flipping a row to `Passed` without an accepted ADR carrying
the attestation marker — `docs::HARDWARE_ATTESTATION_MARKER`, which is deliberately not
spelled out in this paragraph, because a rule that reads a marker cannot tell a claim from a
sentence about one — fails the build; and writing that line without moving the row fails too.
Those are the two ways a list like this normally rots.

## Consequences

**What now holds.** The rig runs on a host through `waymaker-fault` at every crash point the
injector lists — `crates/waymaker-rig/tests/sweep.rs` — and its own oracle accepts every one.
The census over three write points and two reset causes is complete on that sweep and fails
closed when it is not. `crates/waymaker-rig/tests/teeth.rs` runs two writers that are wrong in
one way each — an acknowledgment mark before the commit seal, a dispatch mark before it — and
requires each to be caught by the guarantee it breaks, with an unmodified control writer
required to pass. Without those the oracle would be a function nobody has seen fail.

Write amplification is published by `cargo xtask size`, measured by running the real
`Journal` through the real rig over three parts that differ in program granularity, because
that is what the answer turns on: a commit seal is one program unit, so a sixteen-byte-page
part pays sixteen bytes to commit a twenty-four-byte frame. Erase counts are reported as run
totals rather than per-effect figures, because §10 erases a whole bank once per run and
dividing by eight effects prints a zero that reads as "this engine does not erase".

**What this cost.** A sixth non-layer crate, a `Window` adapter so the engine and the
instrument can share one part and therefore one supply, and three new `xtask` dependencies —
`waymaker-flash`, `waymaker-fault` and `waymaker-rig` — for the same reason `waymaker-core`
was already one: the published figure is measured by running the writer rather than
transcribed from an arithmetic model that would agree with the writer right up until the
writer changed.

**What is owed, and it is the exit criterion itself.** No board has run this. Everything above
is a model: `waymaker-fault`'s media starts erased and only clears bits, its barrier is a
no-op, and a bit that programmed weakly is not a state it has. The `Cutter` a host supplies
stops an iteration at a record boundary; the interior tears come from the injector rather than
from a timer, so the "randomised points" a board would sample are covered here by exhaustion
instead. §12's `barrier-is-durable` and `barrier-orders-what-follows` remain what
`waymaker-conformance`'s across-reset witness is for, and neither this sweep nor that suite can
observe them on a host. `HARDWARE_TARGETS` is where all of that is written down as owed, and
issue #27 stays open on it.

**What was found by writing this rather than by reading the code.** Two things. §14's
`single-authority` is a statement about a device that *has* an authority: a part whose
preparation was interrupted before its first generation seal landed has none, and an oracle
that reported that as a violation failed a third of the crash points on the first run of the
sweep. The witness is what tells "never installed" apart from "lost its authority", because
`Rig::iterate` selects an authoritative bank before it writes its first mark. And
`waymaker-flash`'s own typestate turned out to defend the seam this rig was built to test:
`Sealable` borrows the caller's page between the payload barrier and the commit seal, so the
deliberately-wrong writer in `teeth.rs` could not reuse that page to mark the witness in the
window it was trying to open, and had to be given a buffer of its own.
