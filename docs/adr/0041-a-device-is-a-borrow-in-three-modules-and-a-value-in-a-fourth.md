# 0041. A device is a borrow in three modules, and a value in a fourth

- Status: Accepted
- Date: 2026-09-13
- Issue: [#84](https://github.com/madmax983/waymaker/issues/84)
- Supersedes: nothing
- Related: [ADR 0022](0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md), [ADR 0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md), [ADR 0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md)

## Context

CLAUDE.md's "what is not checked" list carried this line since issue #26's second review
round:

> Four modules refuse storage that is "not the device this was validated against" —
> `append` at three steps, `recovery`, `capacity` and `swap` at all five — and all four
> decide it by comparing a `Geometry`. Two parts of the same model have the same one, so
> none of them can tell two instances apart: a caller holding two chips can prepare a swap
> on one and commit it on the other, sealing a bank whose erase happened elsewhere or
> erasing an unrelated device's active bank.

`Geometry` is capacity, erase size, program size and read size — a description of a *model*
of part, not of the chip soldered to one board rather than another. Two Winbond W25Q64s
answer `geometry()` identically. So a `WrongDevice` check written as `if storage.geometry()
!= expected { return Err(WrongDevice) }` refuses a caller who mixed up a NOR chip and an SD
card, and says nothing at all to a caller who mixed up two NOR chips of the same part
number — which, on a board with two sockets or two chip-selects, is the mistake actually
worth refusing.

Every one of `append`'s three steps, `swap`'s five, and `recovery`'s five all took `storage:
&mut S` as a fresh argument at every call, re-proving nothing about *which* `S` value had
arrived beyond its `Geometry`. Closing the gap by adding a sixth field to compare — a serial
number, a pointer address wrapped in `unsafe`, anything an adapter could report — would have
made the check stronger and the trait wider, which is exactly the shape design document §05
forbids: "a host or browser adapter must not expand the firmware traits to accommodate host
conveniences."

## Decision

Three of the four modules close the gap by construction instead of by a wider comparison.
`append::Journal::stage`, `recovery::Recovery::new`/`with_integrity`, and `swap::Swap::
prepare` are now the *only* calls in their protocols that accept a `storage: &mut S`
argument at all. Each borrows it for the rest of the protocol's life — `Staged`, `Sealable`,
`Recovery` and `swap::Prepared`/`Staged`/`Sealable`/`Installed` all carry a `storage:
&'storage mut S` field — so a caller who wants to finish a record, a scan, or a swap on a
second device does not meet a runtime refusal. The call does not exist:

```compile_fail,E0061
# use waymaker_flash::append::Staged;
# use waymaker_flash::storage::StableStorage;
fn a_second_device_has_no_call_to_make<S: StableStorage>(
    staged: Staged<'_, '_, '_, S>,
    mut other: S,
) {
    let _ = staged.payload_barrier(&mut other);
}
```

and the same shape of doctest sits in `swap`'s module documentation. `AppendError::
WrongDevice` and `SwapStepError::WrongDevice` still exist and still fire, exactly once each,
at `Journal::stage` and `Swap::prepare` respectively — the one call in each protocol that
still takes a `storage` argument, and so the one call a caller could hand a second device
to. Every later step's `WrongDevice` case — the one issue #84 opens with — is not a
refusal these types make any more. It is a program that does not compile.

`recovery::RecoveryError::WrongDevice` is the same borrow, kept for a narrower reason:
`Recovery::new` and `with_integrity` are `const fn`, and a `const fn` cannot fail. The
device-swap issue #84 names is closed the same way as the other two — one borrow, taken
once, for the whole scan — but a `Recovery` can still be *constructed* from a region and a
device that never matched to begin with, and the first fallible point that mismatch can be
reported at is the first call to `next`. The check stays a per-step comparison rather than
moving to a fallible constructor, because a constructor that could fail would cost every
caller a `Result` for a mistake a borrowed device can now only ever have made once.

`capacity::CapacityError::WrongDevice` is not moved to a borrow, because it is not a
device-instance check at all. `Reserve::for_layout` and `Reserved::over` take no `storage`
argument between them — no `&mut S` of any kind — only a `Journal` and the numbers a
`BankLayout` derived. What the check compares is `(here.capacity(), here.erase_size())` — a
bank size and a program granularity — against the pair a `Reserve` was priced for, both
plain values with no device attached to either. Two devices of the same model agreeing here
is not the gap issue #84 describes; it is the check working as intended, because a reserve
priced for one bank size is wrong for a device of *any* identity whose banks are a different
size. Binding this to a borrow would add a lifetime parameter across `Reserve` and
`Reserved` for a comparison neither type needs a device to make.

Each variant's documentation now says which of the two shapes it is, and names issue #84.

## Consequences

**`Journal::after` needed a way to hand the device back.** `Recovery` no longer lets a
caller reach `storage` except by finishing the scan, so `waymaker-drive`'s `Context` — which
moves from a `Recovery` mid-boot to a `Journal` it appends with, and needs the same device
for both — could no longer get one without the other. `Journal::after_taking_storage`
and `Recovery::into_storage` are the two new functions this required: the first returns
`(&mut S, Option<Journal<C>>)` from a finished scan, and the second is the escape hatch it is
built from. Both are additions to `APPEND_SURFACE` and `RECOVERY_SURFACE`, and both are
linked from `waymaker-size-probe` so the code-flash gate charges for them.

**`waymaker-drive`'s `Context` cannot hold `storage` as a field of its own any more.** A
struct cannot have one field borrow another field of the same instance, and once `Recovery`
holds the device for its own life, a `Context { storage: &mut S, source: Source<C> }` shape
— which is what this crate had — would need `source` to borrow `storage` from inside the
same struct. The fix is `Source<'storage, S, C>` itself carrying the device in every state
— `Scanning(Recovery<'storage, S, C>)`, `Writing(&'storage mut S, Reserved<C>)`,
`Spent(&'storage mut S)`, and a `Taken` sentinel for the one `mem::replace` needs — so the
device has exactly one owner across the whole boot rather than two that would have to agree.

**Three test-support crates needed the same treatment for the same reason.**
`waymaker-fault`'s crash-sweep writers, `waymaker-rig`'s writers under test, and
`waymaker-size-probe`'s linked calls all drove the old per-call API, and all three had to
move to the new one. Two of `waymaker-fault`'s writers needed to interleave host-side
bookkeeping — `Session::begin_record`/`end_record`, marking which operations belong to which
record for the crash oracle — at a point in the protocol a borrowed `Sealable` now owns
exclusively. `Session::operations()` and `Session::mark_operations()` are the fix: the
bracket a writer used to open live, immediately before a fallible step, it now declares
*before that step is attempted*, from a range computed off the pinned, fixed operation
counts each step is known to spend. Declaring it only after the step succeeds was tried
first and is wrong — a crash inside the step then leaves the declaration unmade in the
faulted run and made in the fault-free one, and the harness's own determinism check (which
compares the two) reports every such writer as non-deterministic. `waymaker-rig`'s
`tests/teeth.rs` had the harder version of the same problem — a real device write, not
bookkeeping — and the fix there is to move the wrongly-timed witness mark earlier, before
the frame is staged at all, rather than to widen `Sealable`'s surface: the property under
test is "the mark precedes the seal by any margin", which an earlier mark satisfies more
robustly, not less.

**Nothing on `Sealable` or `Staged` got wider to make any of this easier.** The temptation,
met twice while fixing the test-support crates, was a `storage_mut(&mut self) -> &mut S`
accessor on `Sealable` — a reborrow of the same device already bound, not a second one — to
let a caller interleave unrelated work mid-protocol. `commit-discipline` and
`swap-discipline` both refuse it: "the one type that may program a seal should do nothing
else" is not a rule about which device a call reaches, and an accessor that cost nothing
architecturally still turns a type with one method into a type with two. Both call sites
that wanted it were rewritten instead, as described above.

**`recovery::JournalRegion`'s captured `Geometry` and the storage-comparison in `stage`,
`next`, and `capacity`'s admission are all still there, doing the job they always did.**
This ADR closes the device-*instance* half of "not the device this was validated against";
it does not touch the two arguments `Swap::beginning` still cannot verify (a stale `booted`,
a reused `run`) or the run-id uniqueness `SwapError::RunReused` still cannot make global —
both remain preconditions on a future dispatcher, recorded in CLAUDE.md's "what is not
checked" exactly as before.

## Alternatives considered

**A wider `Geometry`**, adding a field an adapter reports as an identity witness. Rejected in
the Context: it is the shape §05 forbids, it is `unsafe` on a workspace that denies the
keyword the moment the witness is a pointer, and it needs every existing adapter —
`waymaker-fault`'s model included — to invent an identity a real chip may not expose at all.

**`core::ptr::eq`-based identity checking**, comparing the address of the `&mut S` across
calls instead of a borrow. Considered and rejected: it needs the pointer stored somewhere
between calls — which is the same lifetime problem a borrow solves more directly — and it is
a check that can be *wrong* in the pointer's favour, where a borrow cannot be evaded at all.

**A `Recovery::position()`/`Recovery::at(region, position, storage)` pair**, letting a caller
suspend and resume a scan explicitly. Rejected because, made public, it reopens exactly the
substitution this ADR closes: any caller could reconstruct a scan with a different device
between the suspend and the resume, which is a second door built next to the one just
locked.

**`Context.storage: Option<&mut S>`**, taken out and put back around each call that needs
it. Rejected: `waymaker-drive`'s `Boundary` trait drives the workflow with no way to inject
storage per call, so `dispatch` and its siblings need it reachable through `&mut self` at
any time, which an `Option` taken elsewhere cannot guarantee.
