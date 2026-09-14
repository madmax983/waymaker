# ADR 0046: CRC-16 folds its nibble round to a multiply, and CRC-32 stays bitwise

- Status: accepted
- Date: 2026-09-14
- Issue: [#153](https://github.com/madmax983/waymaker/issues/153)
- Supersedes: one sentence of
  [ADR 0010](0010-the-integrity-check-is-catalogued-and-table-free.md) — "both computed
  bitwise with no lookup table". `crc16` no longer processes one bit at a time. Nothing
  else in 0010 changes: the algorithms, the catalogue check values, the Hamming-distance
  analysis, and the ban on a lookup table all stand.
- Related: [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md),
  [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md),
  [0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)

## Context

Issue [#153](https://github.com/madmax983/waymaker/issues/153) is a follow-up to issue
#152. ADR 0010 named the one condition that would revisit its table-free decision: "a
profile of a real workload ... showing the checksum on the critical path", most likely
answered by a nibble table.

`cargo xtask profile` measured that condition, in part. Its `journal`, `driver` and
`facade` workloads are host-side instruction counts under Valgrind — not a hardware cycle
count, and not a run on real flash. `crc16` and `crc32` together are 33–49% of
engine-attributed instructions in every workload that touches them.

That is not the evidence ADR 0010 asked for. It is a host instruction count, and
`xtask::profile`'s own doc comment states plainly that this figure "converts into no cycle
count on the hardware §04's budgets are stated for". ADR 0010 asked for "a profile of a
real workload **on real flash**, against a latency requirement §04 does not currently
state". No such profile or requirement exists. So this ADR does not add a table on the
strength of it.

Investigating the table option anyway turned up a fact worth recording, because it changes
nothing ADR 0010 decided and still cuts the cost. `crc16`'s nibble table folds to a plain
multiply for this specific polynomial:

- `0x1021` sets three bits: 0, 5, and 12.
- A 4-bit nibble reaches no higher than bit 3.
- Shifted to each set bit, a nibble's copy spans four bits: `0..=3`, `5..=8`, `12..=15`.
  Each span leaves a gap before the next one starts. No span overlaps another.
- With no overlapping span, adding the three copies never carries. A carry is the only way
  an XOR-based fold and an integer multiply can disagree. So `nibble * 0x1021`, plain
  integer multiplication, equals the same four bitwise rounds a nibble table would hold.

This is verified exhaustively for every 16-bit CRC state against the bitwise definition —
see `crc16_nibble_round_matches_four_bitwise_rounds_for_every_crc_state` in
`crates/waymaker-flash/src/crc.rs`. **This is not a table.** No array, no rodata, no
`const`/`static` beside the multiply itself. `integrity-check`'s ban on adding one does not
apply, and this change needed no gate edit.

`crc32`'s reflected polynomial, `0xEDB8_8320`, has no such gaps: it sets 14 bits, several
of them within 4 of each other (for example 19, 20 and 21). Their spans overlap, so a
nibble's four rounds carry, and the fold does not reduce.
`crc32_does_not_reduce_to_a_nibble_multiply` in the same file pins that fact, so nobody
tries the same shortcut here without re-deriving it. Closing that gap for real needs a
16-entry table — ADR 0010 already priced one at 64 B of rodata — and a real table needs a
superseding ADR to add.

## Decision

**`crc16` computes its checksum by folding two nibble-rounds per byte through a multiply,
with no lookup table. `crc32` keeps its eight-round bitwise loop. Neither carries a table,
and the crc32 table stays declined.**

1. The `crc16` change is a rewrite of *how* the algorithm is computed, not a change to
   *which* algorithm it is. It still passes every catalogue and property test in
   `crates/waymaker-flash/src/crc.rs`, unmodified: the published check value, the empty
   input, the leading-zero sensitivity, and the exhaustive single-bit-flip sweep. It costs
   nothing in code flash or rodata, because it introduces no table.
2. The `crc32` table is declined. The only evidence offered for it is a host instruction
   count. ADR 0010 requires a real-flash measurement against a stated latency limit to
   revisit the table decision, and §04 states no such limit today. A host instruction
   count is a real signal for something — see
   ["What it is a signal for"](#what-it-is-a-signal-for) — but it is not that evidence.

## Consequences

- `crc16`'s instruction count drops, host-measured, without spending any code flash: two
  nibble-rounds (a shift, an XOR, and a multiply each) replace eight bit-rounds (a shift,
  a mask computation, and an XOR each).
- `crc32`'s cost is unchanged. It remains the more expensive of the two per byte, exactly
  as ADR 0010 measured.
- The wire format is unchanged. Both functions are pure and total, and produce the same
  output for every input as before — this is checked by the existing catalogue and
  property tests, not merely assumed. The corpus tests in
  `crates/waymaker-flash/tests/corpus.rs` still pass unmodified.
- `integrity-check`'s parameter pins (`0x1021` once, `0xFFFF` once in `crc16`'s body) still
  hold, because the polynomial and initial value are declared exactly where they were.
- A future contributor cannot apply `crc16`'s trick to `crc32` by habit: the negative test
  and this ADR both explain why the polynomials differ in this one respect.
- The crc32 table stays undecided rather than closed off. A real-flash, real-latency case
  for it can still open a new superseding ADR, exactly as ADR 0010 already provided for.

### What it is a signal for

`cargo xtask profile`'s instruction counts are not thrown away by this decision. They are
still useful for what ADR
[0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md) says
they are useful for: a deterministic, relative cost signal for the host, good for comparing
one commit against another. That is exactly what motivated looking at `crc16` and `crc32`
in the first place, and exactly why the `crc16` half of this decision needed no new
evidence-gathering machinery — the improvement held on its own terms, as an algorithm
question, once looked at closely. It is not, on its own, a stand-in for a hardware latency
measurement against a stated budget, and this ADR does not treat it as one for `crc32`.

## Alternatives considered

**Add the crc32 nibble table now, on the strength of the profile.** Rejected. This is the
literal ask in issue #153's title, and it is exactly the substitution ADR 0010 warned
against: a host instruction count standing in for "a profile of a real workload on real
flash". Spending 64 B of an 8 KiB budget on a signal §04 does not have a budget for is how
a budget goes, which is ADR 0010's own reasoning restated.

**Leave `crc16` bitwise too, and treat this as a pure "no decision yet" issue.** Rejected.
The nibble-multiply fold is not a table, needs no gate change, and passes every existing
correctness test unmodified. Declining a free, verified improvement because a related,
unrelated question is still open would be conflating the two.

**Supersede ADR 0010 wholesale.** Rejected. Everything else in 0010 still holds: the
algorithm choice (CRC-32/ISO-HDLC over CRC-32C), the catalogue check values, the
Hamming-distance analysis, and the refusal of a lookup table without a superseding ADR.
Only the one descriptive sentence about `crc16`'s mechanism is out of date, and only that
sentence is superseded.
