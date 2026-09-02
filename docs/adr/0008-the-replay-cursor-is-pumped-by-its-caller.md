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

1. **No page in the kernel.** `const _: () = assert!(size_of::<ReplayCursor>() == 32)`
   fails the build for a cursor that grew an inline buffer. The equality is the point: the
   cursor is registered in `kernel_state_types!`, whose assertion is `<= 128`, and 96 bytes
   of headroom is enough room to hide a 64-byte scratch buffer in — which a review of an
   earlier draft of this branch confirmed by doing exactly that and watching every test
   pass. What the number cannot establish is that the cursor holds no page *at all*: a
   `&'static mut [u8]` is sixteen bytes. What rules that out is item 2, for any page that is
   not `'static`, and review for the rest.
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

**The journal is the allocator.** `EffectIdAllocator` is folded into the cursor and
**replaces** it in the kernel-state registry rather than joining it — the registry sums
independently live types, so registering both would spend 16 bytes of a 128-byte budget
twice, and `budget.rs` asked for exactly that when the allocator was first registered.

The fold comes with a rule about *when* a sequence is spent, and getting it wrong is the
one defect this branch actually shipped and had to fix. `next_effect_id` takes `&self`: it
answers what the next effect's identity will be and moves nothing. A sequence is spent when
its schedule record is **committed**, which is `advance` over that record — the same call
replay makes. §07 orders a durable intent before the physical effect, so an identity that
never reached media is one no dispatcher saw and no journal remembers; a reboot would hand
it out again, and would be right to. The first version of this cursor spent the sequence at
the moment of asking, which meant a driver that minted an id, wrote the schedule record and
then advanced over it was told its own sound journal was out of order — the live path could
not record a single effect. Two properties follow from the corrected rule, and both are
what a driver needs: asking twice before the intent is committed answers the same effect,
so a retried schedule write reuses the identity rather than skipping one; and the counter
is a pure function of history, so a cursor rebuilt after a reset lands on the same number.

The cursor holds no opinion about *what* a workflow asked for. §08's transition table —
whether the effect's kind and input digest match what history recorded — is issue #15's, and
the `Resolve` / `EffectRequest` boundary of §06 is issue #28's. One §08 verdict does live
here and is worth naming rather than glossing: `next_effect_id` refuses to mint identity
anywhere but `Position::Replaying`, with `NondeterministicWorkflow`. That is the row
"history says this effect cannot come next", which is a fact about *position* and so is
answerable without the request. The line is: **this cursor refuses ill-formed history and
identity history cannot have issued; #15 refuses a divergent request.**

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
  proved to agree by `cold_start.rs` exercising both. The module documentation says history
  "can only arrive in the order it was written", which is an obligation on the caller rather
  than a property this type enforces.
- **Nothing on media binds a journal to a `RunId` yet.** `ReplayCursor::new` takes the run
  from its caller, and §07 puts the run id in the bank header, which is rung 0.2. Until then
  a cursor constructed with the wrong run mints `EffectId`s nothing on the device accounts
  for, undetectably.
- **#15 has no way to halt this cursor.** `Position::Halted` is reachable only through a
  failing `advance`, and `advance` never sees a workflow's request. So a divergence check
  built on top will have to add either a request-taking entry point or an explicit
  `halt(KernelError)` — additive either way, but "recovery exposes only a legal prefix is a
  property of the representation" is a claim about *history*, and it does not extend to
  divergence for free.
- **One error path has no reachable code behind it.** `advance` can return
  `IdExhausted`, and reaching it needs a run that committed `EffectSeq::MAX` schedules —
  2³² records at the frame's sixteen-byte floor, or 64 GiB of journal. It is written, drawn
  in the `Position` diagram, and untestable. The alternative was a `resume`-style
  constructor that positions a cursor part-way through history, which is the seek this whole
  design exists not to have.

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
  re-implements the interpretation the cursor just performed.
- **Echo the record with the identity beside it: `Result<(Option<EffectId>, RecordRef<'a>),
  KernelError>`.** This is the honest competitor to `Step`, and it buys everything the
  bullet above says `Step` buys except the enum — including the "only `advance` produces
  one" property, which is a fact about the *constructor* rather than about the type.
  `Step` was kept for the pairing and for `match` arms that read as run history rather than
  as media, at a cost of 16 bytes on a return value and a second six-arm enum in the
  code-flash delta. Issue #28's `Resolve` will be a third enum over these same six shapes;
  that is the point at which this should be revisited, and if `Step` still carries nothing
  the tuple would not, it should be the one that goes.
