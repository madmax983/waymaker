# ADR 0020: the capacity reserve is an outcome and a terminal record

- Status: accepted
- Date: 2026-09-05
- Issue: [#25](https://github.com/madmax983/waymaker/issues/25)
- Supersedes: nothing
- Related: [0009](0009-the-transition-table-is-a-machine-that-owns-the-cursor.md),
  [0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md),
  [0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md),
  [0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)

## Context

Design document §10 "Capacity is explicit" is three sentences:

> Waymaker reserves enough tail space for a terminal record or `continue_as_new`. Ordinary
> effect scheduling fails early with `HistoryNearCapacity`; the runtime never overwrites
> committed history to make room.

An append-only journal in a fixed bank runs out. §02 decision 2 has already decided what
happens then — history is a committed prefix, so nothing may be evicted — which leaves
exactly one way for a full journal to stay useful: never become full by surprise. Stop
admitting *ordinary* records while the two ways out are still affordable.

`waymaker_core::KernelError::HistoryNearCapacity` has existed since issue #12 and until now
had no caller. `crates/waymaker-flash/src/append.rs` says in as many words that the reserve
is "a policy above this type", and `crates/waymaker-flash/src/recovery.rs` says the same
about the append offset: "does this fit" is a policy question with a reserve in it, and the
reserve is not those modules'. Issue [#25](https://github.com/madmax983/waymaker/issues/25)
is where it becomes somebody's.

Three things about the shape of the problem decided most of what follows.

**The reserve is not one number.** §10 names two exits and they are paid in different
places. A terminal record is written to *this* journal, at its tail. A `continue_as_new`
header is written to the **inactive** bank — §10's steps 2 to 6 — so it costs this journal
nothing at all. A reserve that kept tail space for the larger of the two would reserve up to
64 KiB of journal for a header that goes somewhere else.

**A terminal record is not always writable.** This is the finding, and it came out of §08
rather than out of §10.
[ADR 0009](0009-the-transition-table-is-a-machine-that-owns-the-cursor.md)'s transition table
admits `RunCompleted` and `RunFailed` from a `Replaying` position and from nowhere else. An
outstanding `EffectScheduled` leaves the cursor in `AwaitingOutcome`, which is not one. So a
run with an effect outstanding **cannot end** until the outcome record is written — and the
reserve everybody writes first, room for one terminal record, admits a schedule and then
strands the run: the outcome does not fit, and §08 refuses to let a terminal record follow an
unresolved effect. The journal is not full. The run is over anyway, with no way to say so.

**A refusal has to be cheap in the one way that matters.** Issue #25 asks that "the failure
produces no mutation at all", and §12 is explicit that "a failed program may still have
changed media". So the only refusal that changes nothing is one taken before the device is
called — not one that programs and then rolls back, because on NOR there is no rolling back.

## Decision

`waymaker-flash` grows a `capacity` module. `Bounds` is what a run declares its records may
be worth — a run input, an effect result, a terminal result — and `Reserve` is what those
bounds cost on a particular `BankLayout`.

**The tail reserve is an outcome plus a terminal record.** `Reserve::exit_bytes_after` is
what a record still owes once it is committed, as a total function of its kind by exhaustive
`match`, so a record kind added to §09's table is a compile error rather than an exit nothing
budgeted for:

| After | Still owed |
| --- | --- |
| `EffectScheduled` | an outcome, then a terminal record |
| `RunStarted`, `EffectCompleted`, `EffectFailed` | a terminal record |
| `RunCompleted`, `RunFailed` | nothing |

`Reserve::admits(record, room)` is §10's whole decision: the record's own encoded length plus
what it still owes, against the room left. It is a pure predicate over a length — no media,
no state, the same answer on every boot — and it refuses with `HistoryNearCapacity`, which is
§10's own word.

**The `continue_as_new` header is priced but is not in the tail.**
`Reserve::swap_bytes` is the padded bank header the swap writes, and what it buys is a
*construction-time* refusal rather than tail space: `Reserve::for_layout` rejects bounds whose
run input a bank header could not carry (`SwapDoesNotFit`), and bounds under which a bank
could not hold that header *and* the run behind it — its own `RunStarted` record and its own
exit (`ReserveDoesNotFit`). So a `Reserve` in hand is a promise that this device can end the
run it is running and roll over into another one that can do the same. That is §10's "a
terminal record **or** `continue_as_new`" read as two exits that must both stay open, checked
once where a device is configured rather than hoped for at each append.

**The gate is a type, and it is applied first.** `Reserved` wraps a `Journal` and **consumes**
it, for the reason `Journal::after` consumes a `Recovery`: a caller holding both would have an
ungated path to the same offset, and a linear discipline is what makes the second route a line
somebody writes on purpose. `Reserved::stage` calls `Reserve::admits` before `Journal::stage`
is called at all, so a refusal reads nothing, programs nothing, barriers nothing, and does not
move the write-amplification counters.

**A record longer than its bound is refused, and says something different.** A reserve is a
promise about records of a declared size; admitting a larger one would make the promise false
for whatever came after it. That refusal is `KernelError::Decode(LengthOutOfBounds)` rather
than `HistoryNearCapacity`, because a caller acts on the two differently — one means end the
run, and the other means the bounds are wrong and ending the run will not help.

The gate rule is **`capacity-reserve`**. It pins the module's public surface, in both
directions, and pins the *spelling* of the admission call and its position before the
delegation — because "calls `admits` somewhere" is satisfied by `let _ = self.reserve.admits(..);`
and by an `admits` taken after the frame body is already on media, and each of those is a
refusal that programs the record it was meant to refuse.

## Consequences

**The reserve is bigger than §10's sentence suggests, and that is the point.** Keeping room
for an outcome as well as a terminal record costs a device the size of one worst-case outcome
record of journal it will usually not use. The alternative is a run that cannot end. The
necessity is falsifiable rather than argued:
`a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` drives the tempting policy
against the real writer and finds the point at which it strands.

**`append` grew one public function.** `Journal::region` hands out a `Copy`, read-only
`JournalRegion`, because a reserve cannot price a record without the granularity the journal
was written at, and taking that from anywhere but the writer's own region is how a reserve
computed for one device under-reserves on another. It is on `APPEND_SURFACE` with that
argument written beside it.

**`frame` grew one too.** `encoded_len_for(payload_bytes, align)` is `encoded_len` without a
record in hand, because the worst case of a bound is a *length* and a firmware with a 16 KiB
budget cannot conjure a 64 KiB record to measure one. `encoded_len` and `body_len` now route
through it, so there is one copy of the sum rather than two that agree until somebody changes
the frame's overhead.

**The code-flash figure moved and the gate did not.** 14764 B to **16232 B** against the same
16 KiB budget. The split is worth stating because it is almost entirely not the module: the
library change measured with the probe not reaching it is **40 B** — `Journal::region` and the
`encoded_len_for` refactor — and the remaining **1428 B** is what the probe drags in to reach
eleven public functions and both arms of every refusal. That is issue
[#72](https://github.com/madmax983/waymaker/issues/72)'s defect in the flesh rather than an
estimate: `size-probe-reach` obliges the probe to name every public function, and the probe's
own arithmetic is charged to the engine's row.

**152 B of headroom is what is left, and it is not enough for issue #26.** ADR 0017 raised
this gate once and ADR 0019 declined to raise it again, saying 1620 B was what the rest of
rung 0.2 had to fit in. This spends most of it. The bank swap of issue
[#26](https://github.com/madmax983/waymaker/issues/26) will not fit in what remains, so the
budget conversation now falls due there — and it should be had against a measurement that has
issue #72 fixed first, because a raise argued from a figure a third of which is the probe is a
raise argued from the wrong number.

**Nothing obliges a caller to use the gate.** `Journal` is still public and still ungated,
which is what `append`'s own documentation promises: it programs what it is handed. `Reserved`
makes the ungated path deliberate rather than impossible. The thing that would make it
obligatory is a dispatcher that only ever holds a `Reserved`, and that is rung 0.4's — issue
[#36](https://github.com/madmax983/waymaker/issues/36). Stated here so its absence is a
decision.

**The reserve does not know what history says.** `exit_bytes_after` is a function of the
record's kind alone, not of whether an effect is actually outstanding, so a schedule is always
charged for an outcome even where the run is about to fail instead. That is conservative in
the safe direction and keeps the reserve stateless — which is what lets a device recompute the
same number on every boot from the bounds and the geometry, with nothing to recover. A reserve
that consulted the cursor would be a second replay state machine in the adapter layer, which
§05 puts in the kernel.

**`continue_as_new` is still issue #26's.** The test that shows the roll-over succeeds from
the near-capacity state runs §10's seven steps by hand, the way
`crates/waymaker-fault/tests/banks.rs` did before issue #22 gave the protocol a module. What
is shipped here is the *price* of the header that swap writes, not the swap.

## Alternatives considered

**A single tail reserve covering both exits.** The literal reading of "enough tail space for a
terminal record or `continue_as_new`". Rejected because the swap writes to the other bank: the
reserve would be the larger of two numbers, one of which is zero in this journal, and a device
with a 64 KiB run input would reserve half its bank for a header that never lands in it.

**Putting the check inside `Journal::stage`.** Unavoidable, and one fewer type. Rejected
because it makes the writer own a policy — `append`'s documentation is explicit that whether a
record *should* be written is not its question — and because a `Journal` that carried a
reserve could not be used for the bank header or by `waymaker-fault`'s crash writers, which
have no run and no bounds.

**A pure predicate and no gate type.** `Reserve::admits` alone, applied by the caller.
Cheapest in flash by roughly 600 B, and it is what is left if the budget forces a choice.
Rejected because a reserve nobody is obliged to consult is a comment: the failure mode is a
dispatcher that forgets on one path, which is exactly the shape of bug the rest of this
repository spends its gates on.

**Charging a schedule only for its outcome, and a terminal record separately.** It looks
tighter: reserve one outcome after a schedule, and let the terminal record be checked when it
is written. Rejected because it moves the failure rather than removing it — the outcome
commits, the run then tries to end, and there is no room. §10's promise is that a run can
*always* end, which means the terminal record has to be paid for at the last moment the
journal still had a choice, and that moment is the schedule.

**Deriving the bounds from the bank rather than declaring them.** A reserve sized to whatever
the bank could hold needs no `Bounds` and cannot be misconfigured. Rejected because it inverts
the guarantee: bounds declared by the run are what let `for_layout` say "this device cannot
run this workflow" at configuration time, and a reserve derived from the bank can only ever
say "this record does not fit" at the moment it is written.
