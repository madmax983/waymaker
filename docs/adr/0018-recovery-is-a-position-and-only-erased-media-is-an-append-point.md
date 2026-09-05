# ADR 0018: recovery is a position pumped by its caller, and only erased media is an append point

- Status: accepted
- Date: 2026-09-05
- Issue: [#23](https://github.com/madmax983/waymaker/issues/23)
- Supersedes: nothing
- Related: [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md),
  [0008](0008-the-replay-cursor-is-pumped-by-its-caller.md),
  [0016](0016-the-storage-contract-is-a-conformance-suite-and-a-port.md),
  [0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)

## Context

Design document §09 says recovery "stops at the first unsealed, malformed, out-of-sequence,
or integrity-failed frame", §10 says the authoritative bank's journal is scanned forward, and
§14 says a recovery exposes only a legal prefix of committed records. `waymaker-flash` has had
a reader for that since rung 0.1: `frame::Scan` walks a journal, stops in the right place, and
honours program-alignment padding.

It walks a `&[u8]`. §04 states the runtime RAM budget as **768 B with a 512 B scratch page**,
so a journal that is in RAM is a journal on a host — rung 0.2's banks are whole erase blocks,
which on the parts §04 targets are 4 KiB each. Nothing in the workspace read a journal off a
device, and `Scan`'s own documentation said the offset it reports "is not yet an append
point".

Issue #23 asks for both halves: a forward scan through the caller's single page, and the
append offset as a by-product of it. The second half is the dangerous one. It is a *writer's*
precondition, and on NOR a programmed bit cannot be returned to one without erasing the block,
so an append offset that is wrong is not a bug that costs a record — it is a bank that fails
its own header checksum on every boot for ever.

## Decision

**Three decisions, and the third is the one that matters.**

### 1. The caller pumps, and the borrow rides on the page

`Recovery::next(&mut self, storage: &mut S, page: &'page mut [u8]) -> Option<Result<RecordRef<'page>, _>>`.
The same shape [ADR 0008](0008-the-replay-cursor-is-pumped-by-its-caller.md) chose for the
replay cursor, for the same reason: the lifetime on the answer is the *page's*, not
`&mut self`'s, so the caller may overwrite its page the moment it has dealt with a record and
one page is enough for a history of any length.

`Recovery` is **40 bytes** — a 28-byte region, a four-byte offset and an eight-byte verdict —
and three `const` assertions fail a build for one that grew a buffer:
`size_of::<JournalRegion>() == 28`, `size_of::<Recovery>() == 40`, and the sum of the two
restated, so a field added to either moves a number rather than fitting under a ceiling. The
equalities are the point, as they are for the cursor: a `<=` leaves room to hide a page in.

Sixteen of those forty bytes are the `Geometry` the region carries, which decision 2 below
argues for; it is stack rather than `.bss`, and §04's runtime-RAM row measures statics. An
earlier draft of this ADR recorded 24 bytes and a 24-byte assertion, from before the region
carried the geometry — Codex caught the record understating the type by exactly that sixteen.

Nothing about this is `Iterator`. A lending iterator needs GATs and a lifetime the type would
have to name, for ergonomics only; the manual pump is the same thing with a name, and it lets
the caller interleave `ReplayCursor::advance` and its own work, which §06 step 4 requires.

### 2. A region is validated once, as a *program*, and carries the device it was validated against

`JournalRegion` is the bytes between a bank's header and its seal. It is built either from a
geometry directly (`spanning`, for a port that lays journals out its own way) or from §10's
chain (`of(layout, bank, header)`), and both go through one validation: not empty, inside the
device, whole **program** units at both ends, and the journal's granularity at least the
device's program unit.

Validated as a program rather than as a read, which is stronger in both places it has to be. A
geometry nests — `erase >= program >= read` — so whatever is legal to program is legal to
read, and one check covers both. And a region that is readable but not programmable is one
whose append offset no driver would accept: on a device that reads single bytes and programs
eight, `validate_read` admits a base of 1, a recovery of an erased region there reports a clean
end at offset 0, and the absolute offset a caller would then program is 1. Codex found that on
pull request #74; `a_region_a_driver_could_read_but_never_program_is_refused` is it as a test.

The region also **keeps** the geometry, and every step **compares it** with the storage it is
handed. Reading the units back off whichever `StableStorage` a
caller hands to `next` would prove every bound against a different device from the one they
were established on: a region built at granularity 4 and walked on a device that reads sixteen
bytes at a time rounds a 24-byte frame up to a 32-byte read and runs eight bytes past the
region's end, into the generation seal or the neighbouring bank. Two independent reviews found
that, one of them with a running reproduction.

Carrying the geometry fixes the read bound. It does not on its own fix the *append* guarantee,
which is the half Codex found next: a region built where the program unit is one byte reads
perfectly on a device that programs eight — every read is one byte aligned — reports a clean
end, and hands back an offset that device must refuse. So the two geometries are compared on
every step, with `RecoveryError::WrongDevice` before a byte is read. Four integer comparisons
per record against an anti-bricking guarantee, and it turns "the caller must hand over the
right device" from an obligation nothing checks into a refusal.

`of` is also the one call that welds the *writer's* granularity to the *reader's*:
`journal_offset` comes from the header on media, `payload_bytes` from the layout in hand. It
refuses a header whose granularity is not the layout's, which closes two silent failures at
once — a coarser writer reserved more room for its seal than this reader subtracts, so a sound
bank reads as damaged on every boot; a finer one shortens the region and drops history past its
end under a clean ending.

### 3. Only `Ending::Clean` carries an append offset

The scan ends in one of three ways, and they are three different things a caller must do next:

| Ending | The prefix is | May a caller append? |
| --- | --- | --- |
| `Clean { append_at }` | all of history | yes, at `append_at` |
| `Damaged { at }` | final, and complete | **no** — the region must be recycled |
| `Incomplete { at }` | possibly **short** | **no**, and it must not be replayed as complete either |

The invariant is: **whenever an append offset comes back, every byte from it to the end of the
region is erased.** `Damaged` and `Incomplete` have no `append_at` field, so the unsafe answer
has no representation — this is checked by the compiler as much as by a test.

Why not simply report the stopping offset? Because without §09's commit seal a scan cannot
tell a torn write from corruption, so it may have stopped at a frame whose header was
half-programmed. Appending *there* programs cells a cycle has already cleared. Appending
*past* it is worse: the next boot's scan stops at the tear again and never reaches what was
written, so the records are lost while the device reports success. The distinction between
`Damaged` and `Incomplete` is the second half: a read that failed teaches a caller nothing
about what is on media, and a caller that replayed that prefix as if it were all of history
would be replaying a truncated run.

### And one function in `frame`

`frame::frame_len_of(header) -> Result<usize, DecodeError>` verifies the magic, the header
seal and the version, and then returns `FRAME_OVERHEAD_BYTES + payload_len`. This is what §09's
two-checksum design is *for*, made usable rather than only explained: a page-bounded reader
has to decide how many bytes to stage before it stages them, and a reader that trusted an
unverified `payload_len` is one an erased page could send anywhere.
`decode_with` and `frame_len_of_with` now share one `verify_header_with`, and
`SEALING_FUNCTIONS` grew from a list into a table so each body is pinned to the seals it must
route through — the same shape `BANK_SEALING_FUNCTIONS` already had, and for the same reason:
a table of names pins the bodies it names and says nothing about a new one.

## Consequences

- **Two readers of one format now exist**, which is a drift hazard rather than a duplication
  problem. It is held rather than hoped: `crates/waymaker-flash/tests/recovery.rs` walks 256
  generated journals both ways and requires the same records, the same stopping offset and the
  same verdict, and `crates/waymaker-fault/tests/recovery.rs` does the same at every crash
  point an injector enumerates. If they ever disagree, those two go red together.
- **The append-offset invariant is swept rather than asserted.** `waymaker-fault` drives the
  real reader over media a real crash left behind, at every point a power loss can land, and
  requires every offset it hands back to be the start of an erased run. The sweep carries its
  own tooth: a test that finds a crash point at which the *obvious* implementation — report
  the stopping offset whatever the ending — would have pointed at programmed media. Without
  it, the rule would be a paragraph.
- **The cost model is an equation, not an inequality.** A recovery costs two reads per record,
  one more at the erased tail, and a walk of what is left of the *region* in page-sized
  chunks. `the_cost_of_a_record_does_not_depend_on_how_many_came_before_it` asserts that
  exactly, at 8 records and at 200, so a cost model that changed shape fails rather than
  passing a slope check.
- **The erased-tail walk is a fixed cost bounded by the region.** It is what stops a hole — an
  erased run with records on the far side of it — from reading as the end of a journal. It is
  paid once, at the end of a scan, and it is the one cost that is not per record.
- **Two reads per record rather than one.** The header read is re-read as part of the frame
  read, which is twelve wasted bytes per record. The alternative was one page-sized read per
  record: half the transactions and *twenty times* the bytes — 512 B fetched for a 24 B record
  — and a `Truncated` that could not distinguish "the frame is longer than the page" from "the
  region is shorter than the frame". On a QSPI part transfer time is what dominates, so
  precision and twelve wasted header bytes beat four hundred and eighty wasted payload ones.
  The header seal is likewise verified twice, ten bytes of CRC-16, rather than growing a second
  public decoder entry point that takes an already-verified header.
- **The erased-tail walk will dominate boot latency before the commit seal lands.** A 64 KiB
  bank with a 512 B page is 128 reads on every boot, however short its history, because an
  erased header is only the end of history if the whole tail is erased. That is correct and it
  is the strongest practical argument for issue
  [#24](https://github.com/madmax983/waymaker/issues/24): a sealed tail is one a reader can
  stop at without proving what lies beyond it. Worth knowing before it is measured on hardware
  rather than after.
- **`Ending` is expected to grow a variant, not to have `Damaged` overloaded.** Every payload
  is `Recovery::offset` today, so the two are interchangeable; with the commit seal they stop
  being the same number, and a tail that is present but unsealed is a fourth thing — recoverable
  and not appendable — that is neither of the two refusals. Every `match` on it in this
  workspace is exhaustive so that day is a list of call sites rather than a silent
  reinterpretation. `frame_len_of`'s postcondition is the other half: `stride` is computed from
  it, so a seal-aware length has to widen *that* rather than arrive as a second function only
  `Recovery` knows to call.
- **`integrity-check` grew a fourth half, and its third was found to be decorative.** Review
  demonstrated that replacing both `::<C>` calls in the reader with their non-generic siblings
  passed all 38 rules and the whole suite: `Recovery<C>`'s parameter selected nothing. It also
  demonstrated three shapes that escaped the derived scan over `frame.rs` — a `where`-clause
  bound, a method in a generic `impl` block, and a wrapped signature — and one shape no scan
  over signatures can see, a helper that takes no `C` at all and calls `crc16`. So the scan now
  reads joined signatures and generic `impl` blocks, `frame.rs` gains a file-wide checksum ban
  with `input_digest` as its one named exception, and `recovery.rs` gains a routing table of
  its own. Each of those has a test that makes it fire; a derived scan that quietly found
  nothing would have passed every test written for it.
- **A new gate rule, `recovery-surface`.** The reader's public function names are pinned, in
  both directions, for the reason `replay-cursor-surface` and `storage-contract` are: a
  `seek`, a `resume_at` or a `read_all` breaks no layering rule and needs no dependency, and
  neither does a second accessor that returns the stopping offset regardless of the ending.
  A test cannot call a function that is not there.
- **The code-flash delta moved from 10 976 B to 12 944 B**, against the 16 KiB gate
  [ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)
  raised it to. Of that, the probe's own arithmetic is a growing share and is still not
  attributed — issue [#72](https://github.com/madmax983/waymaker/issues/72) is where that is
  filed.
- **`RecoveryError` has no `message` and no `Display`.** `Display` would need `E: Display`,
  which spreads to every signature; a `message` would be a second one in a file whose surface
  is a list of names. Every variant already carries something better than a string — the
  driver's own error, `DecodeError::message` behind a one-line `match`, and a byte count that
  says how much larger the page would have to be. It is the one error type in the workspace
  shaped this way, and its documentation says so.
- **`JournalRegion::spanning`, not `new`.** Two `new`s in one file is a public function the
  surface pin and `size-probe-reach` are both blind to. The rename is what the pin costs, and
  it is a cost worth naming rather than working around.
- **"Unsealed" is still not a stop condition this can make.** §09's commit seal is issue
  [#24](https://github.com/madmax983/waymaker/issues/24)'s, so a torn tail and a damaged frame
  stop the scan in the same place. That is what §14 requires either way — "frame ignored;
  previous history prefix wins" — but it means `Ending::Damaged` cannot yet say whether the
  power went during an append or the bank is damaged, and the recycling a caller must do is
  the same in both cases. `Ending` is where that distinction lands when #24 arrives.
- **"Out-of-sequence" is the caller's composition, not one call.** Ordering is a fact about the
  run rather than about the bytes, so `waymaker_core::ReplayCursor` owns it
  ([ADR 0008](0008-the-replay-cursor-is-pumped-by-its-caller.md)) and a caller pairs the two.
  What makes the composition sound is that `append_offset` is derived from the *ending* rather
  than from the offset: a caller that stops pumping has an unfinished scan, and an unfinished
  scan has no append point. `crates/waymaker-flash/tests/cold_start.rs` drives that end to end.
- **A journal region is still not proved to hold a legal journal.** `JournalRegion::of` takes
  the bank header's word for where the journal starts, and the seal covers the header alone —
  the limit [ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)
  already records. This module inherits it: a bank whose header and seal agree is
  authoritative however damaged its journal is, and the scan is what stops at the damage.

## Alternatives considered

- **Read the whole journal into RAM and reuse `Scan`.** What every test in `waymaker-flash`
  did before this, and what §04 forbids on a device. It would also have left the *only* tested
  recovery path one no external-flash part can take.
- **Memory-map the flash and reuse `Scan` over a `&'static [u8]`.** Real on XIP parts, and not
  something §12's contract offers — a port on SPI NOR could not implement it. Building the
  tested path on a capability half the targets lack is how a firmware acquires a second,
  untested path.
- **A visitor: `recover(storage, region, page, |record| ...)`.** Inverts control, so the caller
  cannot interleave the replay cursor and its own polling, which §06 step 4 needs.
- **Verify each frame's seal incrementally, so a frame never has to be staged whole.** Removes
  `PageTooSmall` entirely, and removes the contiguous payload a `RecordRef` borrows — which is
  §02 decision 4's whole point. A record the kernel cannot borrow is a record it has to copy,
  with no allocator to copy it into.
- **Report the stopping offset always, plus a `safe_to_append: bool`.** The same information
  with the invariant moved into a flag a caller can ignore. `waymaker-fault`'s sweep shows what
  ignoring it costs.
- **Defer the append offset entirely to issue #24, with the commit seal.** Throws away the
  case that is both safe and the common one — a journal that ended in erased media — and
  leaves every caller to work the offset out for itself, which is the arithmetic this type
  exists to stop four people getting wrong four ways.
