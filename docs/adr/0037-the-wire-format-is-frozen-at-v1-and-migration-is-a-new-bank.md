# ADR 0037: The wire format is frozen at v1, and migration is a new bank

- Status: accepted
- Date: 2026-09-10
- Issue: [#41](https://github.com/madmax983/waymaker/issues/41)
- Supersedes: nothing
- Related: [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md),
  [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md),
  [0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md),
  [0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md),
  [0022](0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md),
  [0036](0036-workflow-versioning-is-a-range-and-a-recorded-branch.md)

Settles deferred question: `wire-format-migration`

## Context

Design document §16 leaves five questions open and says each needs an answer before the wire
format freezes at 1.0. The fifth is "how stable wire-format migration is performed after a
deployed fleet outlives v1". Its exit criterion was that "the version-marker record of §09 is
implemented and a fleet with two format versions in it can be described end to end". Issue
[#40](https://github.com/madmax983/waymaker/issues/40) implemented the record. This is the
description.

Issue #41 states the promise this ADR is written to make safe: after it, "records written by
a shipped device must remain readable by every later 1.x firmware".

Three things stood in the way of writing the policy down.

**The format was frozen in prose and nowhere else.** `frame.rs` fixes twelve header bytes
and eleven record numbers, and every test in the crate drives the encoder and the decoder
together. A kind renumbered, a field reordered, a check taken over the wrong range: each
moves both sides at once, so every round trip passes and every property holds. The break is
invisible until a device that shipped last year reads a journal this firmware wrote.

**A reader's obligation on an unknown kind was a behaviour, not a rule.**
`permits_unknown_record_skip` says *whether* a reader may skip. Nothing said what a reader
must do when it may not — and "stop" is only half of it, because a reader that stopped and
then appended, or stopped and then truncated, would lose a committed record while obeying
the half that was written down.

**The read side was an equality.** `decode` refused any version but `FORMAT_VERSION`, and so
did the bank header reader. A fleet in a format transition has two versions in it at once,
and an image whose reader is an equality cannot read the bank it is migrating from. The
question asks for a fleet described end to end, and the code could not have one. This is the
same defect §08's `workflow_version` had before ADR 0036 replaced its comparison with a
range — met one layer down, in the bytes rather than in the workflow.

## Decision

### 1. The format is frozen, and the freeze is bytes rather than a sentence

[`docs/format/wire-format-v1.md`](../format/wire-format-v1.md) states v1 byte by byte: the
frame, the commit seal, the record table, the bank header, the generation seal, both check
algorithms and their widths.

[`crates/waymaker-flash/tests/corpus/v1`](../../crates/waymaker-flash/tests/corpus/v1/README.md)
is the conformance corpus — fifteen files, every record kind a v1 writer produces, four
program granularities, a multi-record journal, a bank header and its seal. They were
produced by an encoder written from the field list rather than from `frame.rs`, and that
encoder reproduces `tests/frame.rs`'s golden frames byte for byte. `tests/corpus.rs` decodes
each file, re-encodes what it decoded, and requires the bytes back. The `corpus` CI stage
runs it under a name that says which claim broke.

A case is added, never regenerated. A corpus file that stops decoding is a wire-format
break.

The `wire-format` gate rule holds the constants and the numbering: the frozen widths in
`frame.rs`, the eleven record numbers in `record.rs`, and the specification document, all
compared against one table in `xtask::docs`.

### 2. Inside 1.x, a record kind may be added and nothing else may move

**May**: add a record kind at the next unused number, with a body of its own.

**May not**: renumber a kind, reuse a retired number, move or resize a header field, change
an existing body's meaning, or change either check's algorithm or width.

The promise is one-directional and says so. A later firmware reads what an earlier one wrote;
an earlier firmware meeting a later kind stops. **Downgrade is not supported.**

### 3. A reader that may not skip must stop, expose the prefix, and offer no append point

The three obligations are stated in the specification and are what the code already does:
the scan stops with `UnknownRecordKind`, recovery ends `Damaged`, and a damaged ending
carries no append offset — ADR 0018's anti-bricking rule. A reader must not skip the frame,
must not truncate history at it, and must not overwrite it.

### 4. The read side is a set; the write side stays one number

`waymaker_flash::frame::reads_format_version` is the set of format versions this firmware
reads. `FORMAT_VERSION` is the one it writes. Both decoders — the record frame and the bank
header — take their answer from the predicate, so a widened range cannot reach one and miss
the other.

At v1 the set is `{1}`, so behaviour is unchanged. What the predicate buys is that the fleet
below can be described in the code as well as in this file, and it costs **0 B** of code
flash: the layers measure 12820 B of 13312, exactly where ADR 0036 left them.

### 5. Migration is a new bank at a `continue_as_new` boundary, never an upgrade in place

A journal is append-only and a bank header names its own format version, so a bank is
single-version by construction. There is no rewriting one: §12 forbids overwriting committed
history, and a partly-rewritten bank is a bank with two formats in it and one seal over both.

So the transition is §10's swap, which already writes a whole new bank and installs it with
one barrier. The fleet end to end:

1. **Ship the reading image first.** Firmware whose `reads_format_version` covers `{v, v+1}`
   and whose `FORMAT_VERSION` is still `v`. It writes nothing new and reads nothing new. Its
   only job is to be everywhere before anything writes `v+1`.
2. **Wait for the fleet.** A device still on the old image is a device that cannot read a
   `v+1` bank at all.
3. **Ship the writing image.** `reads_format_version` still covers `{v, v+1}`; `FORMAT_VERSION`
   becomes `v+1`. A run already in flight continues in its `v` bank, read at `v` — its
   records are decoded by the reader the first step deployed, and no byte of it is rewritten.
4. **The run reaches `continue_as_new`.** The swap erases the spare bank, writes a `v+1`
   header, writes the new run's records at `v+1`, and seals it at the next generation. Step 5
   of §10 is the format transition: before it the device boots `v`, after it the device boots
   `v+1`, and a crash anywhere in between recovers one whole bank at one version.
5. **Drop `v` from the read set** once no bank in the fleet holds it — which is once every
   run that started under `v` has rolled over, not once the image is everywhere.

Read-old/write-new is therefore not a mode the engine has to grow: it is what two banks
already are. The retiring bank is read at its own version and the installing bank written at
the new one, and the seal that makes the new bank authoritative is the same seal that makes
the format transition atomic.

## Consequences

**A fleet that skips step 1 bricks the runs it upgrades.** A device that meets a `v+1` bank
with a `v`-only reader gets `UnsupportedFormatVersion` from the header reader, which is no
authority at all. Ordering the two images is a release process, and no binary can check it —
the same standing ADR 0036 records for widening `oldest` before narrowing `current`.

**Downgrade after a new record kind may reclaim a run.** This is the sharpest consequence
and it is not hypothetical. An old image meeting a newer record kind stops with
`UnknownRecordKind` and recovery ends `Damaged` — and a damaged recovery is exactly the input
§10 says to recycle, so `Swap::beginning` accepts it. The old image may therefore
`continue_as_new` over a run a newer image could have finished. Recovery is doing the right
thing with the information it has; the information it does not have is that the damage is a
record from the future. Rolling a fleet back past a record kind is a data-loss operation and
this is where that is written down.

**`Ending` does not distinguish "newer than me" from "damaged".** The iterator does — a
caller sees `DecodeError::UnknownRecordKind` rather than a checksum failure — but the ending
a scan finishes with is `Damaged` either way, which is what makes the paragraph above true.
Separating them is an `Ending` variant and four exhaustive matches, and it would change no
decision this driver takes: both endings have no append point and both refuse the bank. It is
recorded in CLAUDE.md's "what is not checked" rather than taken.

**The corpus is a maintenance obligation.** A record kind added without a corpus case fails
the build, which is the point, and it means the person adding kind 10 writes bytes by hand
from the specification rather than from the encoder. That is the cost of a fixture the code
under test did not produce.

**The read set is a promise about bodies that no rule checks.** Widening
`reads_format_version` is sound only while a later version adds kinds and changes none. A
version that changed an existing body would have to be refused rather than admitted, and
nothing mechanical can tell the two apart — the constant is one line and the reasoning is
this ADR's. It is a decision somebody takes, not a number somebody moves.

**Kinds 10 and 11 stay reserved and uncorpused.** `SignalReceived` and `ChildStarted` have
numbers and no bodies. A corpus case for either would be bytes no writer produces, so the
census names the nine a v1 writer really writes and requires exactly those.

## Alternatives considered

**In-place format upgrade of a live bank.** Rewrite the journal record by record into the
new format, in the bank it is already in. Rejected: the journal is append-only and §12
forbids overwriting committed history, so the "in place" is really an erase-and-rewrite with
no second copy of anything. A crash halfway leaves a bank with two formats and one seal, and
every recovery invariant in §14 is a statement about a bank that has one. The two-bank swap
already provides the second copy, and the generation seal already makes the cutover atomic.

**A translating reader: decode `v` records and hand back `v+1` views.** Rejected for the
reason §02 decision 2 gives about indexes — it puts a mapping between media and the kernel
that nothing on media describes. It also multiplies: two versions is one translator, three is
three, and each is a code path whose first execution is recovery after a power loss.

**A `FormatVersionRange` type mirroring `VersionRange`.** Rejected on budget and on shape. A
newtype with `oldest`, `current` and `admits` is a public type in a layer, which
`size-probe-reach` then requires the probe to reach, against 492 B of headroom — and the
range is not a value anything passes around. Two constants and a `const fn` say the same
thing and cost nothing.

**Allowing skipping at v1 so that downgrade works.** Rejected, and it is the alternative
worth being clearest about. Skipping asserts that the rest of history means the same thing
without the record, and that is false for every kind in the table: a skipped `TimerFired` is
a timer replay believes never fired, a skipped `EffectCompleted` is an effect replay performs
again. A format whose forward compatibility is "ignore what you do not understand" is a
format that silently changes what a workflow did.
