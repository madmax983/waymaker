# ADR 0019: the commit seal is a masked repeat of the frame check, and the writer is a typestate

- Status: accepted
- Date: 2026-09-05
- Issue: [#24](https://github.com/madmax983/waymaker/issues/24)
- Supersedes: nothing
- Related: [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md),
  [0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md),
  [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md),
  [0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)

## Context

Design document §07 numbers the durable effect protocol at seven steps and puts **two
barriers** around each of the two records an effect writes: a payload barrier, so the frame
body cannot be overtaken by its own seal, and a commit barrier, so the seal is durable before
the next irreversible action. §09 gives the frame a `commit_seal` one storage-program unit
wide and lists "unsealed" first among the four conditions recovery stops at.

Neither existed. `frame.rs` said so under a heading called **Deferred**, `recovery.rs` said so
in three places, and `Ending`'s own documentation predicted the variant it would have to grow.
The cost of the deferral was written down honestly at the time and was real but small: without
the seal a reader cannot tell a torn append from damage, and both stop the scan in the same
place, which is what §14 requires either way.

Issue #24 asks for three things. A writer in which "it is not possible to program a seal
without the intervening payload barrier having returned". A seal sized to the device's program
unit. And per-effect write amplification as a measurable output. Its exit criterion is the
sharp one: **every crash point recovers to either "frame absent" or "frame committed" — never
"sealed but incomplete"**.

## Decision

### 1. The readers require a seal, so this is a wire-format change

The alternative was to teach only the writer about seals and leave `Scan` and `Recovery`
accepting an unsealed frame as history. That would make "never sealed but incomplete" a claim
about a writer nobody is obliged to use, which is the shape of guarantee this repository does
not accept. So a record is now a padded frame body **and** a commit seal, `frame::encoded_len`
counts both, `DecodeError::Unsealed` is §09's missing fourth stop condition, and
`Ending::Unsealed` is the fourth shape `recovery.rs` said it would need.

`Ending::Unsealed` is not `Damaged`, and the difference is what a caller does next. A damaged
bank is a bank to suspect. An unsealed tail is the ordinary shape of a device that lost power
while appending, and a firmware that raised an alarm on it would raise one on every unlucky
reboot. Neither is appendable; only one is a reason to distrust the media.

### 2. The seal is the frame check with bit 7 of each byte cleared, repeated to fill the unit

A seal has to work at every width a program unit can have, which is one byte to 32 KiB. The
shape [ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)
gave the *generation* seal — a magic, a value and a check of its own — needs ten bytes and
cannot exist on a byte-programmable part at all, so it was never a candidate here.

What the seal must do is three things, and the masked repeat does all three at every width:

- **An erased program unit must never be a seal.** Clearing bit 7 of each byte makes `0xFF`
  unreachable, so a frame whose seal was never written is refused rather than read as history.
- **A seal torn part-way through its own program must never be a whole one.** A program writes
  bytes in order, so a tear leaves erased bytes at the end — and no byte of a seal is erased.
  That is what makes "sealed but incomplete" a state that cannot be *reached* rather than one
  a reader has to detect.
- **A seal must be bound to the frame it seals.** It is the frame's own `frame_crc`, which the
  codec has already computed, so binding costs no third pass. A writer that sealed what it
  *meant* to write rather than what landed produces a seal the reader refuses.

The cost is four bits: a seal binds at twenty-eight bits rather than thirty-two. The rejected
alternative was to keep all thirty-two and special-case the one check value whose pattern is
all ones — a branch taken once in 2^32, which means a branch whose first execution is on
somebody's device, during recovery, years from now. §09 is explicit that a CRC "detects
accidental corruption and torn writes; it is not authentication", and the first of those is
what this is for.

### 3. The writer is three types, because the ordering has to not compile

`Journal::stage` programs a frame body and returns a `Staged`. `Staged::payload_barrier` is
the only method it has, and the only thing that produces a `Sealable`. `Sealable::commit` is
the only thing in the crate that programs a commit seal. A caller that wants to skip the
barrier has nowhere to write it.

Two doctests are the test issue #24 asks for, and they differ in one word: the first compiles
`Sealable::commit`, the second is `compile_fail,E0599` on `Staged::commit`. The twin is what
stops the second from passing for an unrelated reason, and the error code is what stops it
passing for a typo. `trybuild` would have been the conventional tool and is a third-party
dev-dependency this workspace does not allow outside `xtask` and `waymaker-conformance`.

`Journal::after(Recovery<C>)` is the only constructor, so a writer can only be pointed at an
offset a finished scan vouched for — [ADR 0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)'s
anti-bricking rule made structural rather than documented. `Recovery::region` exists for that
and for nothing else.

Three more things had to be true for that to mean anything, and all three came out of review
rather than out of writing the code:

- **The recovery is consumed.** With `&Recovery` a caller could ask one finished scan for two
  writers, and both would program their first record at the same offset. Two separate scans of
  one region still produce two writers, and nothing short of owning the media could stop that
  — but a second writer is now a line somebody wrote on purpose.
- **Every step compares the device, not only the first.** `stage` checked that the storage was
  the one the region was validated against; `payload_barrier` and `commit` did not. A barrier
  taken on another device orders nothing on this one, so the frame would be sealed without ever
  having been made durable — §07 step 2 undone — and a commit taken elsewhere programs a seal
  at an offset that device never validated.
- **Anything but a whole committed record ends the journal.** The append point is a fact about
  erased media, and a failed program, a failed barrier and a dropped `Staged` all leave
  programmed cells at the offset. A writer that carried on would program a second frame over
  the first. So the journal is spent from the moment a body write is *issued* — §12 says a
  failed program may still have changed media — and only a returned commit barrier makes it
  appendable again; anything else is `AppendError::Interrupted` for ever, and recovery is what
  decides where the run continues.

The `commit-discipline` gate rule is what stops the typestate being given back: the surface is
pinned, `Staged` must declare exactly one method and must not name `program`, `Sealable` must
declare exactly `commit`, and `Sealable` must be constructed only inside `payload_barrier`.

### 4. Write amplification is counters, and the division is the caller's

`WriteAmplification` carries payload bytes, programmed bytes, program calls and barriers, and
counts what the device was **asked** for rather than what it acknowledged — §12 says a failed
program may still have changed media, and a wear figure that only counted successes would
understate exactly the runs that wore the part. There is no ratio accessor: a division links
software division on `thumbv6m-none-eabi`, which `storage.rs` measures at 408 B of the budget,
to compute a number a device with no console cannot print.

## Consequences

**A record costs one more program unit and one more barrier.** An eight-byte payload on a
device with an eight-byte program unit is a twenty-four-byte frame and an eight-byte seal:
four times its payload, and two barriers rather than one. On real NOR the barriers are what
dominate. That is §07's price and it was always going to be paid.

**The over-striding hole `frame::Scan` documented as undetectable is closed, by accident.**
`Scan`'s own documentation said a reader handed a *larger* granularity than the writer used
"strides over whole frames and lands on erased bytes, which is an ordinary end of history in
every respect this type can see", and pinned the wrong answer in a test on purpose. A seal
sits at a fixed offset from the frame it seals, so a reader that believes in a larger stride
looks for it past the end of the real record and finds erased media — which is never a seal.
The test now asserts the right answer. It is a CRC's kind of certainty rather than a proof:
another record's bytes coinciding with the pattern is twenty-eight bits away.

**The erased-tail walk is not closed.** `recovery.rs` named the commit seal as "the strongest
practical argument" for making a boot cheaper, and that turned out to be wrong. A seal says a
record is committed; it does not say that no record follows, so a reader that stopped at an
erased header without walking the rest of the region could still miss a hole. What would close
it is a marker saying "history ends here", which is a record kind rather than a seal, and none
of §09's vocabulary is one. The claim has been corrected in place rather than left standing.

**Two teeth in `waymaker-fault` had to be re-aimed, and the reason is the result.**
`a_codec_that_stops_sealing_its_payload_recovers_a_record_that_is_half_there` and its sibling
were codec mutants that recovered a torn record. With the seal they recover nothing: a torn
frame has no seal over it whatever the codec believes, so the mutant is caught by a mechanism
that is not the one under test. Both now need a *second* bug — a writer that programs the seal
ahead of its frame — and `the_honest_order_refuses_a_torn_record_whatever_the_codec_believes`
is the new test that says why. That the mutant has to reach around
`waymaker_flash::append` to the session to express itself is the typestate demonstrated from
the outside.

**Three defects were found by review rather than by the tests**, which is the same score as
[ADR 0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)'s two and
is worth writing down rather than quietly fixing: a recovery that could be asked for two
writers, two of the three steps not comparing the device, and a journal that would program
over a frame body a dropped `Staged` had left behind. Each is now a regression test in
`crates/waymaker-flash/tests/append.rs`. The common shape is that the *first* step of a
protocol was guarded and the rest were assumed to inherit the guard — which is exactly what a
typestate is supposed to stop, and does not, because a typestate constrains the order of steps
and says nothing about their arguments.

**What the sweep cannot falsify is the payload barrier itself.** `waymaker_fault::Device`
applies programs in the order they are issued, so it cannot tell a writer that barriers between
the frame and the seal from one that does not. §12's "no later mutation may become durable
before mutations ordered by a completed barrier" is a contract obligation on the driver, and
`waymaker-conformance`'s across-reset witness is what holds a real one to it. Modelling store
reordering is a different harness and is owed. What the sweep *does* falsify is the other half:
`crates/waymaker-fault/tests/commit_discipline.rs` finds the crash point at which a
seal-before-frame writer leaves a valid seal over a torn frame, and the honest writer never
does, at any of the crash points the injector enumerates.

**Code flash moved from 10 976 B to 14 668 B of a 16 384 B budget**, and the split is measured
rather than guessed. Linking the seal into the codec and the two readers, with the writer
dead-stripped, is **13 396 B** — so the seal costs 2 420 B and the writer plus the probe
section that keeps it alive costs a further 1 272 B. The budget is **not** raised: 1 716 B of
headroom is left, which is less than rung 0.2 started with and is a conversation the next
change on this path has to have rather than one this one settles.

The larger half is the surprising one and is worth naming: 2 420 B is not the seal's
arithmetic — a masked repeat is a handful of instructions — it is the second read, the wider
staging and the fourth `Ending` arm spreading through `Recovery::stage`, plus `encode_with`
splitting its output buffer in two. As always the figure includes the probe's own arithmetic,
because `size-probe-reach` requires `journal_append` to link every public function of the new
module and every arm of its refusal; issue
[#72](https://github.com/madmax983/waymaker/issues/72) is where the probe's share of the
measurement is owed a proper attribution.

**The ghost model did not change, and that is worth stating.** `waymaker-spec` describes a
record as one durable unit written by one `Program` and made durable by one `Barrier`, and a
partial write as a `Tear` that recovery must not produce. The two-barrier writer refines that
exactly: with the seal at the end of a record's bytes, "the frame body landed and the seal did
not" *is* a torn record, and the reader refuses it. Before this change a body-complete frame
was a complete record, so the model and the code agreed for a different reason. No transition,
no invariant and no proof needed editing, and `tests/refinement.rs` re-ran against the new
codec unchanged.
