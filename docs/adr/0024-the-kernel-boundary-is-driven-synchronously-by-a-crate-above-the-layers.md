# ADR 0024: the kernel boundary is driven synchronously, by a crate above the layers

- Status: accepted
- Date: 2026-09-06
- Issue: [#28](https://github.com/madmax983/waymaker/issues/28)
- Supersedes: nothing
- Related: [0009](0009-the-transition-table-is-a-machine-that-owns-the-cursor.md),
  [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md),
  [0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md),
  [0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md)

## Context

Design document §06 states an explicit kernel boundary, and §02 decision 5 states that the
async syntax is an adapter. [ADR 0009](0009-the-transition-table-is-a-machine-that-owns-the-cursor.md)
built the boundary — `EffectRequest`, `Intent`, `Resolve`, `Outcome`, `Next`, and the
`ReplayMachine` that answers with them — and `waymaker-flash` built the two halves it is
answered from: the recovery scan of
[ADR 0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md) and the
two-barrier writer of
[ADR 0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md).

Nothing joined them. Every part of the protocol was tested against its own neighbours, and
the claim that the parts *are* the protocol — that `waymaker-embassy` has nothing left to own
but `async` syntax — was an argument rather than a check. Issue #28 asks for two things it can
be held to: a workflow driven to completion "with no `Future`, no Embassy, and no allocation",
and a boundary whose signature "adding a new record kind does not change".

Three places could hold the driver, and two of them are wrong.

`waymaker-embassy` is wrong twice over. Its `Ctx`, dispatcher and wakeups are rung 0.4's, and
a driver placed there could not falsify the claim it exists to make: a façade that contains
the protocol is not a façade.

A layer is wrong for the reason
[ADR 0013](0013-the-fault-harness-is-a-crate-above-the-layers.md) gives about the fault
harness. Every public function of a layer must be reached by the size probe, so a driver in
`waymaker-flash` would be charged against §04's incremental code-flash budget — which stood at
18098 B of 18432 B when this was written — for code no shipped firmware links. And
`waymaker-flash`'s must-not-own cell names workflow types.

An integration test is wrong for a third reason. The claim is "no allocation", and a test
compiled for the host with `std` behind it cannot fail over an `extern crate alloc` appearing
under the driver.

## Decision

`waymaker-drive` is a crate above the layers, in `policy::TEST_SUPPORT_CRATES` with
`waymaker-fault`, `waymaker-spec`, `waymaker-conformance` and `waymaker-rig`. It is
`#![no_std]`, allocation-free, and outside `default-members`, and the `drive-firmware`
pipeline stage builds its library for `thumbv6m-none-eabi`.

That stage holds the `std` half and not the allocation half, and the difference is worth
being exact about: `cargo build --lib` produces an rlib and never links, so no global
allocator is required and an `extern crate alloc` would compile clean. `crate-attributes` is
what fails a build over it — `policy::NO_STD_TEST_SUPPORT_CRATES` names the three
test-support crates that make the `#![no_std]` claim, and the rule holds all three to it.
`waymaker-conformance` and `waymaker-rig` made the same claim before this change with nothing
checking it either.

It owns four things and no more.

**The workflow's half of §06** is `Boundary`, with one method. A workflow asks for an effect
and gets an outcome or `Suspended`, which it propagates with `?` — the place `.await` will go.
`Suspended` has a private field, so only the driver can make one: a workflow that could build
one could stop a run nothing asked to stop.

**The world's half** is `Activities`, which performs an effect into the caller's buffer and
may answer `Pending`. That is how a driver with no executor still has a way to wait.

**The loop** is `Driver::boot`: the recovery scan feeds the machine, the machine's answer says
what to do, and §10's gated writer records it. `Progress` has two shapes, `Finished` and
`Waiting`, and the errors are refusals rather than repairs.

Every append goes through `waymaker_flash::capacity::Reserved` rather than through `Journal`
directly, and that is not tidiness. An ungated driver commits a schedule record, tells the
world to perform the effect, and *then* finds the outcome record does not fit — and §08 has
no edge from an unresolved effect to a terminal record, so the run can never end, and every
boot after it performs the effect again. The reserve refuses before the schedule record, so
the run declines to start the effect instead of having already asked for it. A driver
carrying no reserve is strictly weaker than the wrong reserve
[ADR 0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md) already has a
test for, so the reserve is a constructor argument rather than an option.

**A reference workflow and world**, in the library rather than in `tests/`, so the firmware
target builds them too.

Two rules hold it. The existing `transition-surface` already pins the machine's functions.
The new **`kernel-boundary`** rule has two halves under one id, because it is one decision:
the *shape* half pins the member set of every boundary type in both directions, which is
issue #28's second "done when" — §09 reserves six record kinds nobody has written a body for,
and a `Resolve::TimerFired` added when the first lands would turn one boundary into a boundary
per record. The *routing* half pins that `waymaker-drive` decides from `Intent` and `Resolve`
and names neither `RecordKind` nor `Step` — as *identifiers*, so `Step ::Record` and
`use …::Step as S;` are caught too — because a driver that read history for itself would be a
second transition table.

What the routing half does not say is that the driver reads no record at all. It reads two:
`recorded` classifies a terminal record when the workflow ends outside an effect boundary, and
`begin` refuses a journal whose first record is not a `RunStarted`. Both are `RecordRef`, both
are exhaustive matches the compiler breaks when a variant is added, and both are named in
[what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked)
rather than left for a reader to find.

The lifetime discipline §06 asks to be documented is enforced instead. Every borrowed result
points into a buffer the caller owns and the driver reuses, and `Boundary::call` derives its
borrow from `&mut self`, so a workflow holding one across the next boundary does not compile.
That is a `compile_fail` doctest beside a compiling twin, the shape
[ADR 0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md) used for
the writer's typestate.

## Consequences

The protocol is now driven end to end by code that is not the façade, so "`waymaker-embassy`
is a façade and nothing more" is falsifiable: anything the façade would have to add to run a
workflow is something this driver had to add first.

§02 decision 3 is a property of the call order rather than a comment. The schedule record is
committed inside `decide`, and the world is not called until `dispatch`, which cannot be
reached without it. `crates/waymaker-drive/tests/crash.rs` sweeps that at every crash point
`waymaker-fault` enumerates and requires every dispatched effect to have a recoverable
schedule record — with a hand-written driver that dispatches first as the tooth, because a
sweep whose failure nobody has seen is a sweep that proves nothing.

The workspace gains a fifth test-support crate, a rule (43 → 44) and a pipeline stage. That is
the cost, and it is the same cost `waymaker-rig` paid for the same reason.

What this driver does **not** do is written down rather than implied. It does not swap banks.
A crash that leaves a torn or unsealed tail leaves a journal with no append point — ADR 0018's
anti-bricking rule — and this driver refuses with `DriveError::Recovery` or
`DriveError::NoAppendPoint` rather than repairing it. Recovering from that is §10's
`continue_as_new`, which `waymaker-flash`'s `swap` owns, and joining the two is the dispatcher's
work at rung 0.4. The sweep measures how often each happens rather than assuming: it requires
both a crash image the run carries on from and one it cannot.

It has no timers, because `TIMER_SCHEDULED` and `TIMER_FIRED` have no bodies yet. When they
arrive they are new *records*, and `kernel-boundary` is the rule that fails a build in which
they become new boundary variants instead.

And it takes a `JournalRegion` and a `RunId` from its caller rather than selecting a bank. Bank
selection is `waymaker-flash`'s `bank::select`, and a driver that did it too would be the
second place authority is decided.

Three defects came out of review rather than out of writing this, and the sharpest is the
reserve above: the first version of this driver appended through `Journal` directly, and a
journal too small for a run's outcome record left it dispatching a real effect on every boot
for ever. The other two were the same shape as each other — a length measured against the
wrong thing. `conclude` committed the terminal record and
*then* copied the run's outcome into the caller's buffer, so a payload the buffer could not
hold left a run that really completed reporting `ResultTooLong` on that boot and on every boot
after it. And the reference world reported how many bytes *fit* rather than how many it
produced, so a small buffer had the driver record a truncated result as history instead of
refusing it. Both are now the other way round, and both are held by a test that was watched
failing against the old code.

## Alternatives considered

**A `Ctx` in `waymaker-embassy`, non-async now and wrapped later.** Rejected on both counts
above: rung 0.4 owns that crate's content, and a façade containing the protocol proves the
opposite of what issue #28 asks.

**A synchronous driver as a `waymaker-flash` integration test.** Rejected because "no
allocation" would then be an argument, `waymaker-flash` must not own workflow types, and
CLAUDE.md forbids `unwrap` in an integration test's helper functions — which a several-hundred
line driver written as test helpers would need throughout.

**Renaming `Resolve::Redeliver` to the issue's sketched `Resolve::Pending`.** Rejected. The
sketch spreads three cases over what are now two types — its `Replayed` is `Resolve::Replayed`,
its `Dispatch` is `Intent::Schedule`, and its `Pending` is `Resolve::Redeliver` for an effect
whose intent is committed. "Redeliver" is the name §14's redelivery contract uses and ADR 0009
settled; the waiting the sketch's name suggests is the *driver's* state, and it is
`Progress::Waiting`.

**Taking the activity input as bytes, as the issue sketches `EffectRequest { kind, input }`.**
Already settled by ADR 0009 and unchanged here: comparing bytes against history means digesting
them, and §05's must-not-own cell for the kernel names CRC. The digest is computed one layer up
— this driver calls `waymaker_flash::frame::input_digest` — and the pair travels down.
