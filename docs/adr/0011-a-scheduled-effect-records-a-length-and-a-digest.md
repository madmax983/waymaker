# ADR 0011: A scheduled effect records a length and a digest, and nothing more

- Status: accepted
- Date: 2026-09-02

Settles deferred question: `effect-scheduled-metadata`

## Context

Design document §16's third open question is "how much input metadata an `EffectScheduled`
record stores beyond length and digest". Issue
[#16](https://github.com/madmax983/waymaker/issues/16) states the cost that makes it a
question rather than a preference: "every extra field is paid per effect, per record, in
flash and in write amplification".

The record has carried four fields since rung 0.1, and they cost eight bytes of payload:

| Field | Width | What it is for |
| --- | --- | --- |
| `seq` | 4 B (in the frame header) | where the effect falls in the run's history |
| `kind` | 2 B | which activity the dispatcher is to run |
| `input_len` | 2 B | how many bytes the call passed |
| `input_crc` | 4 B | a digest of those bytes |

On media that is 12 bytes of frame header, 8 of payload and a 4-byte frame CRC: **24 bytes
per scheduled effect**, before program alignment. A single extra `u32` is 28 bytes — 17%
more journal, 17% more erase pressure, and 17% less history before `continue_as_new` — for
the life of the format on every device that ships it.

Three constraints bound what the record has to carry.

- **§07 orders durable intent before the effect.** The record crosses a durability barrier
  before dispatch, so its size is on the critical path of every effect, not just of
  recovery.
- **§08 compares what replay asks for against what history recorded.** The record has to
  carry enough to answer one question: *is this the same call the last run made?*
- **§02's `numeric-kinds-and-borrowed-bytes`** says user payload bytes are opaque to the
  kernel. The engine has no business interpreting an activity's input and no allocator to
  copy it into.

The third constraint is also what makes a small answer sufficient: replay re-executes the
workflow deterministically, so the input bytes of effect *n* are reconstructed by the
workflow itself on the way to effect *n*. The journal never has to supply them; it only has
to be able to say that what the workflow just produced is what it produced last time.

## Decision

**`EffectScheduled` records exactly `seq`, `kind`, `input_len` and `input_crc`, and no other
input metadata.** `input_crc` is the CRC-32/ISO-HDLC of
[ADR 0010](0010-the-integrity-check-is-catalogued-and-table-free.md) — one definition of
"the digest" in this repository, computed the same way on both sides of a barrier, because
a divergence check comparing two numbers nobody computed the same way is a check that
passes.

Kind, length and digest are together sufficient for §08: a call to a different activity
changes `kind`, a call with different input changes `input_len` or `input_crc`, and a
false match needs a CRC-32 collision on an input of identical length to a *specific* other
input — which is a wrong-answer probability of about 2⁻³², against an accidental-divergence
failure mode rather than an adversarial one. §09 is explicit that the CRC "is not
authentication", and neither is this.

### What was considered and rejected

- **The input bytes themselves.** Doubles the journal's scarcest resource to store what
  replay reconstructs anyway. This is the option §16's phrasing already leans against, and
  it is the one that would make the record unbounded.
- **A deadline or timeout.** A deadline is a timer, and §09 reserves record kinds 5 and 6
  for `TimerScheduled` and `TimerFired`. Putting one on every scheduled effect charges every
  effect for a field most of them do not use.
- **A retry count or attempt number.** §14's stable-redelivery guarantee is that "retries
  and reboot redelivery reuse the original effect identity", so a retry is *the same effect*
  and needs no new number in history. Where retry policy lives is §16's second question and
  is still open (`retry-policy-placement`, rung 0.4); this decision deliberately does not
  pre-empt it, and does not have to — a façade that records retries would add a record, not
  a field here.
- **A priority.** Dispatch order is history order. A priority field would be state the
  engine would then have to honour, which is a scheduler, which is not in any rung.
- **A wider digest.** A 64-bit digest costs 4 more bytes per effect to move the collision
  probability from 2⁻³² to 2⁻⁶⁴ on a check that is already only defending against
  accidental divergence, on a device whose journal holds thousands of records rather than
  billions.

### What would revisit this

A concrete §08 divergence case the four fields cannot distinguish, or a rung whose protocol
genuinely needs a fifth field on the *schedule* record specifically — not on a record of its
own. Either is a superseding ADR.

## Consequences

- The `effect-scheduled-fields` gate rule pins the variant's field set in
  `crates/waymaker-core/src/record.rs` and fails in **both** directions. A field added is
  the cost above, taken silently. A field removed is a wire-format change on a record that
  firmware in the field has already written, and is not a thing to discover from a failing
  round-trip test one layer up.
- The pin is on names, not types or widths. A `u16` widened to a `u32` behind a pinned name
  is invisible to it and is a reviewer's job; `waymaker-flash`'s frame tests are what hold
  the byte layout.
- `waymaker-flash` keeps the single definition of the digest, so the kernel still owns no
  CRC — its must-not-own cell names one explicitly.
- Rung 0.4's retry work inherits a constraint rather than a decision: whatever it does, it
  does not do it by widening this record.
