# ADR 0017: The two-bank layout is geometry-derived and the seal names its header

- Status: accepted
- Date: 2026-09-05
- Issue: [#22](https://github.com/madmax983/waymaker/issues/22)
- Supersedes: nothing
- Related: [0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md), [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md), [0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md), [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md), [0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md)

## Context

Design document §10 states the two-bank lifecycle in two sentences and a seven-step swap:
"Waymaker owns two fixed storage banks, usually one erase block each. The bank with the
highest valid generation seal is authoritative." §02 decision 7 is the invariant behind it —
"a new run becomes authoritative only after its payload and generation seal are durable" —
and §14's failure table adds the half that is easy to lose sight of: recovery must "never
recover the old run as current", and must never combine the footprints of two runs.

Issue [#22](https://github.com/madmax983/waymaker/issues/22) asks for four things. A bank
header carrying the `RunId`, the workflow identity, an input schema and the bounded run
input. A generation seal written after the header payload is durable. A selection rule in
which "a bank whose seal fails validation is not a candidate at any generation". And a
layout that is "geometry-derived, not hardcoded", because 2 × 4 KiB is typical and not
required.

Two of those sentences hide a decision each, and both of them are about crash windows rather
than about layout.

**What makes a seal valid.** A seal is a handful of bytes at a fixed offset. §10's swap
erases the inactive bank and then writes into it, so every crash point in the middle leaves
half a bank on media. If a seal's validity is decided by the seal alone, a seal that survived
an erase which took its header with it is a valid seal — and the bank it names has nothing in
it. The same shape arrives from the other direction: a header program that fails, a writer
that carries on, and a perfectly good seal over a torn header at the highest generation on
the device.

**What "highest" means.** A generation is a `u32`, and a `u32` wraps. Issue #22 is explicit
that "generation wraparound is handled explicitly rather than by unsigned comparison luck",
which is a rule about the comparison and, it turns out, better answered somewhere else.

There is also a constraint that was written down in advance.
[ADR 0013](0013-the-fault-harness-is-a-crate-above-the-layers.md) closed with the incremental
code-flash figure at 8180 B of an 8192 B budget and said, in as many words: "rung 0.2's
banks, seals and barriers do not fit, and the budget conversation §04 implies has to happen
before they are written." This is that conversation.

## Decision

### The layout is `erase_blocks / 2` whole blocks per bank

`BankLayout::new` takes a `Geometry` and derives two equal banks of `erase_blocks() / 2`
blocks each, bank A at offset zero and bank B after it. Whole erase blocks because a bank is
recycled by erasing it. Equal because a run that fits one bank has to fit the other, and a
layout that handed the odd block of a three-block device to one of them would make a swap
between them a swap that can fail one way and not the other. The odd block is therefore not
addressed by the layout, and that is written down rather than left to be discovered.

Three refusals, all `LayoutError`. A device of fewer than two erase blocks
(`TooFewEraseBlocks` — §04's "two erase blocks minimum"). A device that programs in units no
bank header could record (`ProgramUnitTooLarge`): `Geometry` takes a `u32` program size and
`ProgramAlign` is a `u16`, so above 32 KiB the only granularity a writer *could* record is
smaller than the one it programs at — and a reader striding short lands inside a frame's
padding and reports a clean end of history, which `frame::Scan` names as the worst failure it
has. And a bank that could not hold a padded header, a padded seal and one padded record
frame (`BankTooSmall`), which is a real part whose program unit is a large fraction of its
erase block. All three measured *padded*: an earlier draft compared the bank against the
unpadded 26-byte header and reserved nothing for the journal, which admitted a device whose
header filled its whole payload and whose journal was zero bytes long.

### The seal names its header

A `Seal` is twelve bytes — magic, a `u32` generation, a `u32` digest, and its own sixteen-bit
check — and the digest is the frame check of the bank header it makes authoritative.
`bank::seal_for` is the one definition of it, so a writer and a reader cannot compute it two
ways.

That single field is what closes both crash windows above, and it closes them *structurally*
rather than by ordering:

- A seal that outlived its header names a digest the bytes on that bank do not compute to,
  so the bank is not a candidate — whichever direction the driver's erase ran in. Nothing in
  `waymaker-flash` assumes an erase order, and nothing needs to.
- A seal over a torn header is refused for the same reason, which turns a writer that does
  not check its program results from a device-bricking bug into a device that boots the
  previous run.

`crates/waymaker-fault/tests/banks.rs` holds both, and each is held by a mutant.
`a_selection_that_ignores_the_header_boots_a_bank_that_was_never_written` drives a writer that
seals the header it *intended*, shows a seal-blind reader booting a bank whose header is torn,
and shows the real selection booting the previous run instead.
`a_seal_that_names_another_banks_header_is_never_authoritative` drives a writer that seals
bank B with bank A's digest — every structure intact, both headers decoding, the highest
generation over a header that is not beneath it — which is the case the first mutant cannot
reach.

That second mutant exists because review of this change measured the sweep without it and
found the digest comparison carrying nothing: deleting it from `sealed_generation` left all
eight tests green, across 303 crash points, because the only way a header stops decoding under
*damage* is a tear or an erase, and both refuse the bank before a digest is ever compared. A
seal binding held only by a hand-built unit test is a seal binding no crash has been near.

The seal sits at the end of its bank so that its offset is a function of the geometry alone —
a reader finds it before it has decoded anything, and the header may grow with the run input
without it moving.

### Generations do not wrap, so the comparison cannot get wraparound wrong

`Generation::successor()` returns `None` at `Generation::MAX`. No writer can mint a
generation that follows the ceiling, so the derived `Ord` on `u32` *is* the order in which
the seals were written, and `select` is not getting wraparound right by luck — it is
comparing numbers that cannot have gone round. A device that reaches the ceiling refuses to
swap, which is this workspace's treatment of every other bounded counter:
[ADR 0006](0006-effect-identity-is-newtypes-and-exhaustion-is-terminal.md)'s exhaustion is
terminal, never silent reuse.

The alternative — RFC 1982 serial-number arithmetic — is rejected below.

### A tie is reported, never resolved

`select` returns `Authority::Unsealed`, `Authority::Bank { id, generation }`, or
`Authority::Ambiguous { generation }`. Two validly sealed banks at one generation is a state
no protocol may produce, so a selection that picked one would hide the bug the third variant
exists to find. `a_swap_that_forgets_to_bump_the_generation_leaves_two_authorities` is what
says the variant is reachable and reported.

### The bank header records the program granularity it was written at

`crate::frame::Scan`'s own documentation states the hazard and names this as its answer: a
reader handed a *larger* program granularity than the writer used strides over whole frames
and lands on erased bytes, which is an ordinary end of history in every respect the scan can
see. Nothing on media contradicted it, so nothing could catch it. The header now carries
`program_shift`, and `BankHeader::journal_offset` is computed from the same number — so the
stride a reader uses is a fact it read rather than one it assumed.

This is one byte and it is not on issue #22's list. It is here because the bank header is the
only structure that has ever been going to carry it, and a deferral with no arrival is a
deferral that becomes permanent.

### The bank codec seals through the integrity trait, and a gate says so

[ADR 0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)
put the frame's two seals behind `IntegrityCheck` and pinned the *routing* — because a trait
nothing is obliged to call is a swap point that selects nothing. The bank header and the
generation seal are a second file with the same hazard, so `integrity-check` grew a second
routing table: `source::BANK_SEALING_FUNCTIONS` names five bodies and, per body, which of the
two seals it must reach through `C` exactly once and use the answer of. It is the same rule
id, because it is the same decision.

Without it, a firmware could seal its banks with one algorithm and its records with another,
and read back neither half with the other's reader.

### The incremental code-flash gate goes to 16 KiB, and the measurement is written down

`waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES` goes from 8192 B to 16384 B. §04
states 8 KiB and labels the column "**v0.1** target"; the two-bank lifecycle is §10, which
rung 0.1 does not have and §04's row does not scope. The gate stays a gate — `cargo xtask
size` fails a build over the new number exactly as it failed over the old one — and the
number stays in `waymaker-core` so that there is one place to change it.

The measurement, so that the raise is a decision rather than a shrug. `cargo xtask size`
moves from **8180 B to 10976 B**, and the delta attributes as (`llvm-nm --print-size` on the
unstripped `engine` image):

| Symbol | Bytes |
| --- | --- |
| `bank::encode_header_with` | 432 |
| `bank::decode_header_with` | 396 |
| `bank::encode_seal_with` | 248 |
| `bank::decode_seal_with` | 184 |
| `bank::seal_for_with` | 92 |
| `bank::sealed_generation_with` | 72 |
| `LayoutError`'s `Display` | 36 |
| `BankHeader::journal_offset` | 24 |
| **`waymaker-flash::bank`, total** | **1484** |
| the size probe's own `two_bank_lifecycle` and `bank_seal_and_selection` | 1032 |
| `.rodata` and knock-on | ~280 |

`BankLayout`'s accessors, `BankRegion`'s and `select` do not appear: each is a field read or a
`match` on two `Option`s, and the optimiser folds them into their callers. They are reached —
`size-probe-reach` requires it — and they cost what an inlined field read costs.

So more than a third of the growth is not library code at all: it is the probe's own `match`
arms and folds, which exist to keep the library's code alive past `--gc-sections`. That is
not new —
[ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md) says the
`default` row measures "the probe's own arithmetic plus the cost of linking the crates", and
at rung 0.0 that was an honest zero. It is now roughly 4 KiB of a 10976 B figure that §04
describes as "core + flash adapter", and it is a defect in the measurement rather than a cost
of the engine. It is filed as issue
[#72](https://github.com/madmax983/waymaker/issues/72) rather than fixed here, because fixing
it means changing what every row of the size matrix means and that does not belong in the pull
request that also writes the bank layer.

The measurement also has about 4 B of run-to-run variance in the linker's output, so the
figures above are quoted to the byte for attribution rather than as a reproducible constant.

What 16 KiB buys: about 5.3 KiB of headroom for the rest of rung 0.2 — the barriers, the
capacity reserve and `continue_as_new`. It is a number chosen to be revisited once the probe
attribution is fixed, and `crates/waymaker-core/tests/budget.rs` asserts it explicitly so
that the next change to it is a line somebody writes on purpose.

## Consequences

- §04's 8 KiB claim is no longer what CI enforces, and the design document says 8 KiB. That
  disagreement is real and it is recorded here, in `budget.rs`, in the assertion in
  `crates/waymaker-core/tests/budget.rs` and in CLAUDE.md's budget table, rather than being
  smoothed over. CLAUDE.md already said `budget.rs` is the source of truth when the two
  differ; this is the first time they do.
- The bank header is a second wire format alongside §09's record frame, with its own magic,
  its own two checksums and its own frozen prefix. It is 26 bytes of overhead per bank, paid
  twice per device and never per record.
- A seal is twelve bytes rounded up to a program unit, so on a part with a 256-byte page a
  bank spends a whole page on four bytes of generation. That is what a program unit is, and
  the alternative — packing the seal beside the header — would make the seal's offset depend
  on the header's length, which is the one thing a reader has to know before it decodes
  anything.
- `sealed_generation` decodes the header once and *frame-checks* it twice: `decode_header_with`
  verifies the stored trailer, and `seal_for_with` then recomputes the same digest rather than
  reading those four bytes back. The second pass is redundant given the first — if the decode
  returned `Ok`, the trailer *is* `C::frame_check` under this `C` — and it is kept anyway, for
  a reason worth stating plainly rather than dressing up: it is what gives the
  `integrity-check` routing pin a call to hold in that body. The cost is one table-free CRC-32
  over the header frame, per bank, per boot; on a 4 KiB header at 93 cycles per byte that is
  around 8 ms on a 48 MHz Cortex-M0+. A cheaper binding that the gate could still hold would
  supersede this.
- `waymaker-spec`'s `single-authority` is still proved against the model alone. There is now
  a two-bank adapter to abstract, so the refinement rung 0.2 owes is now owed against real
  code rather than against nothing; `crates/waymaker-spec/src/obligation.rs` says so, and it
  is filed as issue [#73](https://github.com/madmax983/waymaker/issues/73).
  [ADR 0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md) gives
  the reason as "rung 0.1 has no two-bank adapter to abstract"; that reason has now expired,
  and this line is the amendment rather than an edit to an accepted record.
- **Two limits of the seal's binding, stated rather than discovered.** The seal names the
  bank *header*'s digest, so it says nothing about the journal: a bank whose header and seal
  agree is authoritative however damaged its records are. And the header carries no
  generation, so re-writing byte-identical header content under a seal that survived from an
  earlier generation of the same bank produces a matching digest and a bank reporting the
  *stale* generation. Neither is reachable in the fault model — an interrupted erase there
  always lands a block-aligned prefix, and no writer in the sweep carries on past a failed
  erase — and both are in CLAUDE.md's "What is not checked" for that reason.
- §10's step 7 — "lazily erase the old bank" — is not implemented here. #22 is the layout,
  the seal and the selection; the swap that drives them end to end is `continue_as_new`, and
  the writer in `crates/waymaker-fault/tests/banks.rs` is the protocol modelled, not shipped.

## Alternatives considered

**RFC 1982 serial-number arithmetic for the generation.** The textbook answer to comparing
counters that wrap: `a < b` iff `(b - a) mod 2^32 < 2^31`. Rejected because it does not
remove the problem, it relocates it — serial arithmetic has an ambiguity zone in which
neither of two values is defined to precede the other, and the one thing a bank selection may
not do is be undefined about which of two banks is newer. Refusing at the ceiling has no such
zone. It also costs a device nothing that could ever be reached: 2^32 swaps is 2^32 erase
cycles on a part rated for about 10^5.

**A seal that carries only its generation.** Smaller, simpler, and what a first draft
naturally writes. Rejected because it makes the two crash windows in the Context section
unrecoverable-by-construction, and both are ordinary points in §10's own swap. The four bytes
of digest are the cheapest part of this decision.

**Deriving the layout from a constant rather than from the geometry.** Rejected by issue #22
directly: "the layout must be geometry-derived, not hardcoded". Recorded here because the
temptation is real — the seal's offset is much easier to reason about when it is a literal —
and because the geometry-derived version is what lets one firmware image serve a 2 × 4 KiB
part and a 2 × 64 KiB one.

**Putting the generation in the bank header.** Four bytes, and it would close the
identical-header rewrite above: two generations of one bank would then have different header
bytes and therefore different digests. Rejected for now because it puts the generation on
media in two places, which needs a rule for what a device does when they disagree — and
inventing that rule under review pressure is worse than recording the gap. It is the change to
make when `continue_as_new` arrives and there is a writer whose behaviour would settle it.

**Putting the seal at the start of the bank.** It would mean a front-to-back erase clears the
seal first, which is a second, order-dependent defence. Rejected as a *substitute* for the
header digest, because it is only true of drivers that erase front to back and nothing in §12
says one does. Rejected as an addition because it would put the seal's program unit between
the bank's base and its header, and every header offset would then be a function of the
program size rather than of the bank.

**Raising the budget to 12 KiB.** Would have left about 1.5 KiB for the rest of rung 0.2 and
forced this conversation again mid-rung. Rejected as churn: the number is going to be
revisited when the probe attribution is fixed either way, and 16 KiB is the figure that lets
the rung be finished before then.

**Fixing the probe attribution first, and keeping 8 KiB** (issue
[#72](https://github.com/madmax983/waymaker/issues/72)). The honest fix, and the one that
might have made the raise unnecessary — the library's own contribution is 1548 B against
8180 B of prior measurement, of which roughly 3.2 KiB was already the probe. Rejected for
this pull request because it changes what every row of the size matrix means, in the same
change that introduces a wire format. It is filed instead, and this ADR is what a later one
would supersede.
