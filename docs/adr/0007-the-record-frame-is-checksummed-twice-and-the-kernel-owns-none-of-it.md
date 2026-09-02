# ADR 0007: The record frame is checksummed twice, and the kernel owns none of it

- Status: accepted
- Date: 2026-09-02
- Issue: [#13](https://github.com/madmax983/waymaker/issues/13)
- Supersedes: nothing
- Related: [ADR 0003](0003-the-eight-settled-design-decisions.md), [ADR 0004](0004-the-layering-contract-is-a-table-a-gate-reads.md), [ADR 0006](0006-effect-identity-is-newtypes-and-exhaustion-is-terminal.md)

## Context

Design document §09 gives the journal a frame and four properties that may not change:

> - All lengths are validated against caller buffers and bank bounds before reading.
> - Unknown record kinds are skippable only when the format version permits forward
>   compatibility.
> - Records are padded to the device's program alignment without interpreting stale tail
>   bytes.
> - CRC detects accidental corruption and torn writes; it is not authentication.

and the frame itself:

```text
magic u16 LE | format_version u8 | record_kind u8 | effect_seq u32 LE
payload_len u16 LE | header_crc u16 LE | payload [payload_len] | payload_crc u32 LE
commit_seal (storage-program unit)
```

§13 gives the decoding target, `RecordRef<'a>`, as borrowed views over the caller's bytes.

Three constraints shape everything below, and none of them is negotiable here.

**The kernel may not own a byte of it.** §05's must-not-own table names *serialization
framework* and *CRC* among the things `waymaker-core` may not have, and
[`kernel-is-dependency-free`](0003-the-eight-settled-design-decisions.md#kernel-is-dependency-free)
leaves it with no crate to borrow either from. `waymaker-flash`'s Owns cell is "stable wire
encoding, CRC and seals". So the seam is not a matter of taste: the *view* is the kernel's
and the *bytes* are the adapter's.

That rule had a hole, and this change closes it. `kernel-zero-dependencies` stops the kernel
*importing* a serialization framework or a CRC crate; nothing stopped it *writing* one. A
hand-rolled `const fn crc32(bytes: &[u8]) -> u32` and a `u32::from_le_bytes` in a decode loop
add no dependency, no manifest entry and no graph edge, so every rule the gate had would have
passed while the layering claim quietly became prose. `kernel-owns-no-encoding` is the
twenty-ninth rule, and it fails a build for the six endianness conversions and for an
`impl From<&[u8]>` or `TryFrom<&[u8]>` in a kernel source — the last of which is the cheapest
way in, because it needs no `pub`, and `size-probe-reach` already credits `try_from` from
elsewhere in the probe. It is a floor rather than a proof: a determined author can still
write a shift-and-or loop the scan does not recognise. What it makes impossible is the
accidental arrival, which is the one that gets merged.

**Neither layer may take a dependency, including a dev-dependency.** `may_depend_on_external`
is empty for both, and the gate reads every dependency table. So the round-trip and fuzz
tests issue #13 asks for cannot use `proptest`, `quickcheck` or `arbitrary`, and the two
checksums cannot come from the `crc` crate.

**Public surface has an enforced price.** `size-probe-reach` fails a build for any public
function of a layer the size probe does not call, because a function nothing calls is
dead-stripped and the 8 KiB code-flash budget then measures a smaller firmware than the one
that ships. Every `pub fn` added here is a call added to the probe, on purpose.

The failure this frame exists to prevent is specific. A device loses power mid-append; on
the next boot something walks the bank and decides what history is. If that walk can be
made to read past the end of a buffer, loop for ever, or accept a half-written record as
committed, the workflow resumes from a history that never happened — and a durable workflow
engine that resumes from fiction is worse than one that refuses to resume at all.

## Decision

**The seam.** `waymaker-core::record` owns `RecordRef<'a>` and `RecordKind`, a `u8` newtype
carrying §09's numbering. `waymaker-flash::frame` owns the magic, the header, both
checksums, the padding, `encode`, `decode` and the append `Scan`. Nothing in the kernel
calls `from_le_bytes`.

`RecordKind` is a newtype rather than an `enum`, for the same reason `ActivityKind` is one:
the number *is* the wire format, and an `enum` could not hold a number this firmware does
not know — which is a thing the format has to be able to talk about. It names all eleven
records in §09's table, including the five this rung does not decode. Reserving
`TimerScheduled = 5` and `TimerFired = 6` now is what stops the timer issue renumbering
`RunCompleted` under firmware that has already written it.

**Two checksums, and the second covers the header.** `header_crc` sits at offset 10 and is
CRC-16/CCITT-FALSE over the ten bytes before it — the whole header except itself, so magic,
version, kind, sequence and `payload_len` are all inside it. `payload_crc` — §09's name, kept — is CRC-32/ISO-HDLC over the
header *and* the payload.

The two-checksum split is what makes §09's first property implementable. `payload_len` is
read out of the bytes being validated, so a decoder that checked one checksum over the whole
frame would have to trust the length in order to find the checksum that would tell it
whether the length could be trusted. The header checksum breaks that circle: it is at a
fixed offset, it is verified first, and only then is `payload_len` a number the writer wrote
rather than a number that was found.

Covering the header in the second checksum costs twelve bytes of checksumming per record and
buys two things. A payload cannot be transplanted onto another frame's header and still
check out. And a record with an empty payload gets a checksum that depends on which record it
is — CRC-32 of nothing is zero, and a field that is zero for a whole class of records is a
field a zeroed page satisfies.

Both algorithms are catalogued, and both are tested against their published check values
(`0x29B1` and `0xCBF43926` for `b"123456789"`). That is deliberate: an encoder and a decoder
sharing one wrong checksum round-trip perfectly, and the published value is the only thing in
reach that was not written here. Both are bitwise and table-free. A byte-at-a-time table is
1 KiB of rodata and a nibble table 64 B, against an 8 KiB budget for the kernel and this
adapter together, to save a few hundred shift-and-xor iterations next to a flash program
cycle that costs tens of microseconds.

**The header layout is frozen across format versions.** `decode` verifies the header
checksum *before* it reads `format_version`, which is only sound if every version of this
format puts the same twelve bytes at the front. That is the commitment: a version bump may
change what a payload means, never where a frame ends. Without it a reader meeting a version
it does not know could not say how far to skip, and §09's forward-compatibility rule would
have nothing to stand on.

**Unknown kinds decode; skipping is the scan's decision.** A frame whose `record_kind` this
firmware does not know, but whose checksums hold, decodes to `Decoded::UnknownKind(kind)`
with a correct `frame_len` — which is what "self-delimiting" means, and is a fact about the
bytes. Whether a reader may then *skip* it is a fact about the format version, so it lives in
`permits_unknown_record_skip`, which reads a list that is empty at version 1. Three things
have to change together for a version to grant it — `decode` must accept the version at all,
the list must name it, and `Scan` must grow the arm that advances past the frame — so the
list carries a `const` assertion that it is empty, and a version added to it is a compile
error naming the other two. A rule that can be half-enabled is a rule that gets half-enabled. Skipping a record asserts that the rest of history means the same thing without
it, and at v0.1 that is false for every record in §09's table: a skipped `TimerFired` is a
timer replay believes never fired.

**Run-scoped records write a zero sequence, and the decoder insists on it.** `RunStarted`,
`RunCompleted` and `RunFailed` have no effect to number. The alternative — ignoring the field
for those kinds — would let two byte sequences decode to one record, and a format meant to be
frozen is easier to reason about when the decoder rejects everything the encoder cannot
produce. A frame that is intact and still not the record it names is
`DecodeError::MalformedRecord`, a variant added to the kernel's vocabulary for exactly this:
it is neither an input that ended early nor a length reaching outside a buffer.

**Padding is `0xFF`, and the frame's length excludes it.** An erased NOR cell already reads
`0xFF`, so programming the pad changes no bits. `Frame::frame_len` is the frame's own length,
*unpadded*; `ProgramAlign::round_up` is what turns it into a stride, and `Scan` is what
applies it. The free function `frame::encoded_len(record, align)` is the other number — what
a writer must reserve, padding included, and what `encode` returns. Two numbers, two names:
one word for both is how a cursor ends up advanced into a pad. So padding is never interpreted.

What that does *not* buy is a journal readable at any granularity. `Scan` must be given the
granularity the journal was **written** at, because nothing on media records it: a smaller
one strides short into a frame's padding, and a larger one strides over whole frames onto
erased bytes, which is an ordinary end of history in every respect the scan can see. The
first is caught — an erased header is the end of history only if the journal is erased to
its end — and the second is not, and cannot be until rung 0.2 puts the writer's program size
in the bank header. `a_scan_at_a_larger_alignment_than_the_writer_used_is_not_caught` asserts
the wrong answer on purpose so the limitation is bounded rather than undiscovered.

`ProgramAlign` accepts a power of two and nothing else. The first version of this decision
said the opposite — zero rejected, anything else accepted, on the reasoning that no
arithmetic needed the restriction and a type that refused a device's honest geometry is a
type a driver has to lie to. The reasoning was wrong about the arithmetic, and it was a
measurement rather than an argument that said so: rounding up with `%` divides by a runtime
`u16`, and `thumbv6m-none-eabi` — Cortex-M0 and M0+ — has no divide instruction, so the
linker pulls in `compiler_builtins`' software division. Removing it took the code-flash delta
from 4 726 B to 4 264 B: **462 B**, more than 5% of the entire budget for the kernel and this
adapter, spent on program granularities no flash device reports. It also left a live
divide-by-zero panic branch, because the non-zero invariant is not visible to the optimiser
across a call boundary. A power of two rounds up by mask in three instructions that cannot
trap. Recorded here rather than quietly changed, because the first version of it is in this
repository's history and a reader who finds it should find the reason it stopped being true.

**Bounded decoding is structural before it is tested.** `#![forbid(unsafe_code)]` removes
`get_unchecked`; `clippy::indexing_slicing = deny` removes `buf[a..b]`; `panic`, `unwrap_used`
and `expect_used` are denied, so the panicking recoveries are gone too. What is left is
`slice::get`, `first_chunk`, `split_at_checked` and iterators, all total. So an out-of-bounds
read is not expressible in this crate; the hazards that *are* expressible are a panic — and
`panic = "abort"` in the release profile makes a panic a bricked device — an overflow in
offset arithmetic, and a scan that fails to advance. Every offset sum is bounded by
`MAX_FRAME_BYTES`, every step of the scan advances by at least `FRAME_OVERHEAD_BYTES`, and
the tests below are what say so from the outside.

**The tests are built to be falsifiable.** Four decisions, each aimed at a way a thorough-
looking test suite proves nothing:

- The golden frames were produced by a reference implementation written separately, from
  §09's field list rather than from `frame.rs`. A golden vector generated by the code under
  test proves only that the code agrees with itself.
- The property tests use a hand-rolled xorshift generator, because no dependency is
  available. Payload lengths of zero, one and the maximum are drawn explicitly rather than
  left to a uniform draw that visits them rarely, and the sweep asserts it saw them.
- The fuzz is in two halves. Damaging a frame *without* resealing it tests the checksums, and
  nothing gets through. But a checksum doing its job is exactly why that sweep can never
  reach the version check, the kind check or the body checks — so the second half reseals
  after damaging, and asserts a non-zero count for each of those refusals. A fuzz test that
  is turned away at the front door every time is decorative, and the counters are what stop
  this one being that.
- Round-trip is not the only direction: the same golden bytes are decoded as well as encoded,
  so an encoder and decoder that agree with each other and not with §09 fail.

**Deferred, visibly.** The commit seal is a storage-program unit and needs §12's barrier
protocol, which arrives at rung 0.2. Until then `Scan` treats a frame whose checksums hold as
history, so a torn write at the tail of a journal is not distinguishable from damage there.
Both stop the scan at the same offset, which is what §14 requires either way — "frame
ignored; previous history prefix wins" — so the deferral costs correctness nothing today;
what the seal will add is the ability to say which of the two happened. Bank bounds are
deferred the same way: `Scan` takes a `&[u8]`, and that slice *is* the bound.

## Consequences

The record codec costs **4 264 B** of the 8 KiB incremental code-flash budget, measured on
`thumbv6m-none-eabi` through the size probe. That is over half of §04's number for one
module, with the replay cursor, the transition rules, the seals, the bank swap and compaction
still to come. It is a real constraint rather than a comfortable margin, and it is the number
to take into the rung 0.2 kickoff rather than discover there.

Two measured costs inside it are known and deliberately not paid:

* **About 368 B** is the `iter_mut().zip(chain(..))` writes in `encode` and `body`, which do
  not fully unroll at `opt-level = "z"`. The cheaper spelling is `copy_from_slice`, which
  panics on a length mismatch — and a panicking call on the path that stages bytes for a
  flash program is exactly what this workspace's lint table exists to keep out. The zip form
  is total by construction. If a later rung needs those bytes, the way to have them is a
  `split_at_mut_checked` that makes the lengths equal by type, not a panicking copy.
* **About 976 B of the reported delta is the size probe's own code**, not the layers'. That
  is inherent to measuring a delta through a fixture that has to keep every public function
  alive — ADR 0002 — and it means the number above overstates what a real firmware links, in
  the safe direction. Nothing here corrects for it: a budget that subtracted an estimate
  would be a budget with an estimate in it.

A frame is sixteen bytes of overhead. On a journal of small records — a completion carrying
four bytes — that is 80% overhead, against a bank that is often a single erase block. §09
chose the fields; this decision adds nothing to them, but the arithmetic is worth stating
where somebody sizing a bank will find it.

`Scan` carries one branch that no test can reach: the arm that skips an unknown kind, which
is unreachable while `VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP` is empty. It is kept because
the alternative is a paragraph, and a paragraph is what a future version bump would have to
remember to read — and the *rule* is testable even though the branch is not, which is what
`permits_unknown_record_skip` is for: it is checked over all 256 values a version byte can
hold. The same goes for a handful of refusals in `decode` that are unreachable by
construction and are spelled as refusals rather than as `unwrap`. Both show up as the
uncovered lines in `waymaker-flash`, against a gate of 85%.

`Scan` also takes the program granularity as a parameter, and nothing on media records what
the writer used. A reader given a *smaller* one strides short and lands inside a frame's
padding — a run of `ERASED_BYTE` — which would otherwise read as the ordinary end of history
with committed records still ahead of it. Silently returning a truncated prefix is the worst
failure this type has, because everything downstream believes it, so the erased-tail rule is
"an erased header *and* erased to the end of the journal", and a mismatch is
`IntegrityFailed` at the offset the reader went wrong. That costs one pass over the tail on
the terminating step, once per recovery. It does not make the mismatch safe — a reader given
a *larger* granularity strides past frames and cannot be caught this way — which is a second
reason rung 0.2 puts the writer's program size in the bank header, where a fact about the
media belongs.

Adding `DecodeError::MalformedRecord` widened the kernel's error vocabulary to six variants.
Neither error enum is `#[non_exhaustive]` — ADR 0006 — so every adapter that matches it
exhaustively got a compile error naming the case it now has to think about, which is the
point.

**The seal will change the stride, and version 1 journals are not forward compatible.** §07
writes a frame, waits on a payload barrier, and only then programs the seal — so the seal has
to be in a program unit of its own, or programming it would rewrite bytes the barrier has
already made durable. That means a rung-0.2 frame occupies `round_up(frame_len, align) +
program_size` bytes where a rung-0.1 frame occupies `round_up(frame_len, align)`, and a
reader that expects seals cannot walk a journal written without them.

That is a deliberate break rather than an oversight, and `format_version` is what makes it a
diagnosable one: a v2 reader meeting a v1 frame refuses with `UnsupportedFormatVersion`
rather than walking off the end of a record. Nothing has shipped, no device holds a v1
journal, and the alternative — reserving space now and writing something seal-shaped into it
— would mean inventing the seal's encoding a rung early *and* writing a seal that no barrier
stands behind, which is precisely the "documentation must not call it power-loss-durable"
failure §11 warns about in another context. The space is not reserved; the version byte is
the mechanism.

The choice to reserve record kinds `5`, `6`, `9`, `10` and `11` means the timer issue,
`VersionMarker` and `SignalReceived` each add a body behind a number that is already spent.
It also means a firmware built today meets a `TimerScheduled` written by a firmware built
tomorrow and stops cleanly with `UnknownRecordKind`, rather than mistaking it for something
else — which is what the reservation is for.
