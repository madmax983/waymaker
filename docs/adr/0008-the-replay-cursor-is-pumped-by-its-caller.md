# ADR 0008: the replay cursor is pumped by its caller and holds no page

- Status: accepted
- Date: 2026-09-02
- Issue: [#14](https://github.com/madmax983/waymaker/issues/14)
- Supersedes: nothing
- Related: [0004](0004-the-layering-contract-is-a-table-a-gate-reads.md),
  [0006](0006-effect-identity-is-newtypes-and-exhaustion-is-terminal.md),
  [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)

## Context

Design document §02 decision 2 settles replay as sequential: "A cursor advances through
history in workflow order. There is no `Journal::get(id)` and no in-memory event index."
§06's cold-start sequence adds the shape — recover the committed prefix, decode the run
input into caller-owned storage, create a fresh cursor, poll from the beginning, and either
consume the records matching each effect or identify the first unresolved one. Issue #14
adds the resource constraint: **exactly one caller-owned scratch page**, 512 bytes, and the
kernel holds no page buffer of its own.

That last clause is what forces a decision. The cursor lives in `waymaker-core`, which is
`no_std`, `no_alloc` and dependency-free ([0004](0004-the-layering-contract-is-a-table-a-gate-reads.md)),
and the bytes it replays are `waymaker-flash`'s
([0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)). So
the kernel cannot read a record for itself, and something has to say who moves the bytes.
Three answers were on the table, and they differ in what ends up inside the kernel: a
storage-shaped trait, a page, or neither.

## Decision

**The caller pumps the cursor.** `ReplayCursor::advance` takes one already-decoded
`RecordRef` and returns what that record meant for the run. The cursor has **no lifetime
parameter**, holds no slice, and reads no media.

Four things follow directly, and each is checked rather than remembered:

1. **No page in the kernel.** `const _: () = assert!(size_of::<ReplayCursor>() <
   SCRATCH_PAGE_BYTES)` fails the build for a cursor that grew a buffer — a cursor holding a
   page could not be smaller than one. The cursor is registered in `kernel_state_types!`,
   so the 128-byte kernel-state budget charges for it and `cargo xtask size` reports it.
2. **No borrow retained.** `advance<'a>(&mut self, record: RecordRef<'a>) -> Result<Step<'a>,
   KernelError>` ties the returned lifetime to the *record*, not to `&mut self`. The caller
   may overwrite its page the moment it has dealt with the step, which is what makes one page
   enough for a history of any length. The integration tests overwrite the page with `0xA5`
   between every record and replay 200 000 effects through it.
3. **No random access.** No method takes an `EffectSeq` or an `EffectId` as a key.
   `next_seq` peeks at what comes next; nothing goes the other way.
4. **History is validated against itself.** The cursor refuses a record that could not
   legally follow the ones before it — an outcome with no schedule, a sequence that skips or
   repeats, a second `RunStarted`, anything after a terminal record — with the new
   `KernelError::MalformedHistory`. §09 stops recovery at "the first unsealed, malformed,
   out-of-sequence, or integrity-failed frame", and `Scan`'s own documentation already hands
   the out-of-sequence half to this crate, because ordering is a fact about the run rather
   than about the bytes. The refusal is sticky: `Position::Halted` has no way back, so
   §14's "recovery exposes only a legal prefix" is a property of the representation.

`EffectIdAllocator` is folded into the cursor and **replaces** it in the kernel-state
registry rather than joining it — the registry sums independently live types, so
registering both would spend 16 bytes of a 128-byte budget twice. `budget.rs` asked for
exactly that when the allocator was first registered. The fold pays for itself: history's
sequences are checked against the same allocator that will issue the next one, so
"strictly increasing, never repeated, never wrapped" is one type's invariant in both
directions instead of a comparison written by hand.

The cursor decides nothing about a *workflow*. §08's transition table — whether the effect
a workflow asks for matches what history recorded — is issue #15's, and the `Resolve` /
`EffectRequest` boundary of §06 is issue #28's. The line is: **this cursor refuses
ill-formed history; #15 refuses a divergent workflow.**

## Consequences

- The driver is longer. Recovery is a loop the adapter writes — pull a frame, decode it,
  `advance` — rather than a single call into the kernel. `crates/waymaker-flash/tests/cold_start.rs`
  is what that loop looks like, and it is a test rather than an example on purpose.
- **A borrowed step is short-lived, and nothing enforces how the caller uses the bytes
  before it moves on.** The lifetime stops a caller keeping a step across the next
  `advance`; it cannot stop a caller forgetting to copy the run input out of the page. §06
  step 2 says "decode the run input into caller-owned storage", and that remains an
  obligation the borrow checker only half enforces.
- `Step` is a second enum that resembles `RecordRef`. It earns the duplication by carrying
  an `EffectId` where the record carried a bare sequence — the run id lives once in the bank
  header (§07) — and by being unconstructible for a record that was not legal, so a driver
  matching on one has no arm to write for a history that cannot happen. If a later rung
  finds it carrying nothing `RecordRef` does not, it should be deleted.
- The kernel's error vocabulary grows a sixth variant. `size_of::<KernelError>()` is still
  one byte, so no `Result` got wider.
- A caller that hands the cursor records in an order the media did not have them in gets a
  legal-looking replay. The cursor checks that history is *self-consistent*, not that it is
  the history on the device; that is the scan's job, one layer up, and the two are only
  proved to agree by `cold_start.rs` exercising both.

## Alternatives considered

- **A `Journal` trait in the kernel that the cursor pulls from.** The natural signature is a
  lending iterator, so it needs GATs and a lifetime the cursor would have to name; worse, it
  puts a storage-shaped trait in the crate whose must-not-own cell says "storage driver".
  `check-layering`'s `kernel-owns-no-encoding` would still pass, which is precisely the
  problem: the rule would not catch it, and the layering would have moved anyway.
- **The cursor owns the page: `ReplayCursor<'a> { page: &'a mut [u8] }`.** Ergonomic, and
  the thing issue #14 forbids in as many words. It also makes the cursor's lifetime infect
  every type that holds one, and makes `size_of::<ReplayCursor>()` a number about the
  caller's buffer.
- **Index the journal on recovery, then replay by lookup.** Constant-time effect resolution,
  and RAM proportional to history length on a device with a 768-byte runtime budget. This is
  the design §02 decision 2 exists to rule out.
- **Return `Result<(), KernelError>` from `advance` and let the caller re-match the
  `RecordRef` it already holds.** Smaller surface, no `Step`, and every caller
  re-implements the interpretation the cursor just performed — including the part where an
  outcome without a schedule is impossible.
