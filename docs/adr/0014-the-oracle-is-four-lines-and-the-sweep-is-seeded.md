# ADR 0014: The recovery oracle is four lines, and the sweep that proves it is seeded

- Status: accepted
- Date: 2026-09-03
- Issue: [#19](https://github.com/madmax983/waymaker/issues/19)
- Supersedes: nothing
- Related: [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0009](0009-the-transition-table-is-a-machine-that-owns-the-cursor.md),
  [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)

## Context

Design document §15 states the core property oracle in four lines, and issue #19 makes it
rung 0.1's exit criterion:

```text
recovered_history.is_prefix_of(committed_history)
    && acknowledged_records.all(|r| recovered_history.contains(r))
    && dispatched_effects.all(|e| recovered_history.has_schedule(e.id))
    && recovered_banks.count_authoritative() == 1
```

Issue [#18](https://github.com/madmax983/waymaker/issues/18) built the machinery underneath
it — the modelled NOR device, the exhaustive crash injector, the ledger of §15's three
record states — and checked the first two lines against three writers whose histories were
written by hand. That leaves three gaps, and each of them is the kind that makes a test
suite look finished while checking less than it appears to.

The first is coverage of the *input*: three hand-written histories on two hand-written
geometries is a sample of one shape. §19 asks for "random record sequences across random
storage geometries", and for the enumerated fault families beside them — a tear at every
byte and program unit, a power loss before and after every barrier, CRC corruption, stale
tails, malformed lengths, sequence and capacity boundaries, and bank selection under a
partial swap.

The second is the oracle itself: two of its four lines were not being checked, and the
module said so honestly but said it in prose.

The third is the one issue #19 names as a "done when" clause of its own: *the test is proven
to have teeth*. A property suite that has only ever passed is indistinguishable from one that
checks nothing, and no amount of reading it settles which it is.

## Decision

**The oracle is a `Recovery` and all four lines.** `verify_oracle(&Ledger, &Recovery)` checks
the prefix, the acknowledgment obligation, the dispatched-intent obligation and the bank
count, plus the fifth the design document leaves implicit — recovery must not produce a
record that never reached media, nor one only half of which did. `verify_recovery` remains
as the two-line form and delegates.

The last two dimensions are optional rather than defaulted. A `Recovery` carries what its
caller *observed*: `dispatched` is empty until a caller says which effects really happened,
and `authoritative_banks` is `None` until a caller has banks to count. A default of `1` would
report a passing fourth line for every journal in the workspace, which is a check that cannot
fail — and a check that cannot fail is the thing this repository exists not to ship.

**The sweep is seeded, and the generator is thirty lines of `SplitMix64` in the harness.**
`Rng` and `random_geometry` live in `waymaker-fault` rather than in one integration test,
because a geometry is the harness's own vocabulary and rung 0.2's bank tests will want the
same draw. Randomness here means *drawn*, never *unrepeatable*: every failure names its seed,
and re-running it is re-running the same stream. `random_geometry` clamps rather than gambles,
so every draw is legal by construction and no larger than the budget asked for.

**Issue #19's coverage list is asserted, not claimed.** `tests/property.rs` computes a census
over the whole sweep and fails when it is thin: fewer than eight distinct geometries or
histories, no run that kept an acknowledged record while losing an unacknowledged one, no
effect dispatched and then lost to a power cut, no record torn in half, no journal the scan
refused. Two further tests read the enumeration directly and assert that `injections` really
did produce a tear at every interior byte of every write, a tear on a program-unit boundary
and one inside a unit, and a power loss both before and after every barrier.

**Teeth are demonstrated through the real swap point.** The codec mutants in `tests/teeth.rs`
are implementations of `waymaker-flash`'s `IntegrityCheck` trait — the one
[ADR 0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)
introduced — that return the value an unprogrammed seal field reads back as. The journal is
written *and* read by the same weakened firmware, no fork of `frame.rs` and no `#[cfg]`, and
the oracle catches it: a write torn inside its payload becomes history, and
`RecoveredATornRecord` names it. The cursor mutants are models rather than injections —
`ReplayCursor` is a `const fn` state machine with no seam — so what they establish is that the
oracle names history read one short, two records swapped, one skipped and one invented past
the end, each at some crash point. Every mutant runs against a control on the same runs first.

**Bank selection is modelled in the test, not implemented in the layer.** `waymaker-flash` has
no bank geometry and no generation seal at rung 0.1, and it does not grow one here: a public
function of a layer is charged against an 8 KiB code-flash budget, and 0.2 owns the swap.
`tests/banks.rs` builds the smallest thing that gives the fourth line something real — a bank
is a run's history in genuine §09 frames, its generation seal is the terminal record that
closes it, and the selection rule is "the frames verify, the seal is present, the higher
generation wins". The writer, the erase across two blocks, the barriers and every crash point
between them are the real harness.

## Consequences

Rung 0.1's exit criterion is met, and it is met by something that can fail: `cargo test`
fails when the oracle is breached, and it also fails when the sweep stops covering what
issue #19 asked it to. `waymaker-fault` is at 96.7% line coverage, and the firmware image is
byte-for-byte what it was — 8180 B of the 8192 B budget — because nothing was added to a
layer.

What got worse, and where the limits are. Three of these were found by review after the
first draft of this ADR claimed more than the code did, which is the reason the list is this
long:

* **The suite is slower and larger.** The three sweeps run the writer once per crash point
  per seed — roughly eighteen thousand runs between them — and the bit-flip test re-recovers a
  journal once per bit. It is still well under a second, but it is a runtime dial (`SEEDS`,
  `MIN_CAPACITY`, `MAX_CAPACITY`) that a future contributor can turn until it is not. A slow
  suite is a suite that gets `--skip`ped.
* **Half the enumerated crash set is a duplicate for a writer that propagates every error.**
  `Interruption::Failure` and `Interruption::PowerLoss` armed at the same point of the same
  operation leave identical media unless the writer *reacts*, and a writer built out of `?`
  never does — which made `Harness`'s documented reason for taking a closure rather than a
  recorded write log true of nothing.
  `a_writer_that_retries_a_failed_write_is_a_world_the_sweep_would_not_otherwise_reach`
  retries a failed program once, which on NOR repairs it, and asserts both that the two
  interruptions now lead to different media and that a record torn by a failure and put right
  by the retry ends the run whole. It is one writer, not the default: the run counts above
  still overstate distinct worlds for the others.
* **The sweep cannot catch a broken cursor, and that is structural.** `History::draw`
  generates only histories `ReplayCursor` accepts, and every crash point leaves a *prefix* of
  a legal history, which is legal too — so across every run in this workspace the cursor
  never refuses a record the scan accepted. Measured, not assumed: replacing every
  `cursor.advance(..)` refusal with a no-op leaves all three sweeps passing. That is the code
  being right rather than the suite being weak, but it means the sweeps alone would not
  notice a cursor that stopped checking. Two tests notice instead:
  `without_the_cursor_a_reordered_journal_would_be_replayed_as_history` builds a journal
  whose frames all verify and whose order is illegal — a shape no crash point can produce —
  and shows the scan alone accepting what the cursor refuses; and
  `a_sequence_at_the_top_of_the_space_survives_the_frame_and_stops_the_run` fails when the
  sequence comparison in `ReplayCursor::transition` is deleted. Most of the other cursor
  mutants remain *models*: they mutate the step a caller takes with a record, not the state
  machine, so they prove the oracle has teeth rather than that the cursor is correct.
* **The oracle's third line is reached by the sweep and cannot fail there.** A record whose
  barrier returned is `Acknowledged`, so for any writer that dispatches *after* its barrier —
  which is what §02 decision 3 requires — the second line already demands the intent and is
  the line that fires. The third is load-bearing only for an intent recovery is permitted to
  drop, so `an_effect_dispatched_before_its_intent_is_durable_is_caught` drives a writer that
  commits the inversion: the effect happens, and the record is written afterwards. It fails
  when the dispatched check is removed from `verify_oracle`; the sweep does not.
* **The bank model is a stand-in.** Its selection rule is exercised against every way a swap
  can be interrupted, so the *shape* of the answer is held; the seal it turns on is a record
  and not the storage-program unit §09 describes. When rung 0.2 writes the real one,
  `tests/banks.rs` is what it has to keep satisfying, and this is the paragraph that will look
  wrong until it does.
* **The oracle is deliberately not applied where it cannot hold.** A writer that erases a
  block holding an acknowledged record destroys it on purpose, so the stale-tail case asserts
  a property of the *scan* — an erased header with frames behind it is never a clean end of
  history — rather than a guarantee of the journal. Two banks are what make an erase safe, and
  that is where the oracle meets one.
* **Damage inflicted after the writer finished is outside the fault model, so the bit-flip
  sweep asks the oracle a weaker question.** The ledger records what the writer achieved; a
  bit somebody flipped afterwards is not a crash point, and a journal of acknowledged records
  loses one for every flip that lands in a frame. `LostAnAcknowledgedRecord` is the correct
  verdict there rather than a bug, so what is asserted is that it is the *only* breach and
  that the record it names still reaches the damaged byte — never one whose frame ended
  before the damage began. That is the property a mis-striding reader breaks. Corruption
  *under* the full oracle arrives the realistic way instead: most runs in the sweep contain a
  frame the scan refuses, and every one of them is verified.
* **The bank model's fourth line is checked against a selection rule that lives in the test.**
  Both halves of "exactly one" are now reachable from that rule rather than only from a
  substituted one — `a_swap_that_clears_both_banks_first_leaves_nothing_to_boot_from` produces
  zero and `a_swap_that_forgets_to_bump_the_generation_leaves_two_authorities` produces two —
  but nothing in `waymaker-flash` is under test, because at rung 0.1 there is nothing there to
  test. The `count <= 1` assertion in the honest sweep is a statement about the protocol and
  cannot fail: three distinct generations across two banks cannot tie.
* **`Ledger::torn` changed meaning slightly, in the direction of its own documentation.** It
  reported `true` for a record whose first write was interrupted before it changed a cell —
  a record that is `Attempted`, none of whose bytes are on media. "Half of it is there" and
  "none of it is" are mutually exclusive, and they now are, with a postcondition that says so.
  No verdict moved: the oracle refuses to recover either.
* **The modulo bias in `Rng::below` is real and accepted.** The bounds are small constants
  against a 64-bit draw; a sweep that leaned one part in 2^60 towards a shorter history would
  still be a sweep. Nothing here is, or may become, a source of cryptographic randomness.

## Alternatives considered

**A third-party property-testing crate (`proptest`, `quickcheck`).** Shrinking is genuinely
valuable, and it is the one thing given up here. It was not taken because the failures this
suite produces are already minimal in the dimension that matters: a crash point is one entry
in an enumerated list, and the failure message names the seed, the geometry, the injection and
the ledger. Adding a dependency to the one crate that has none — and doing it for a generator
that is thirty lines — buys a shrinker for the record sequence and pays for it in
`waymaker-fault`'s single distinguishing property.

**Sampling the crash set instead of enumerating it.** This is what a property-testing crate
would naturally do, and it is exactly what issue #18 refused: `injections` is a pure function
of the recorded write sequence, so "every crash point" is a loop rather than a budget. A
sampled sweep would have found most of these bugs most of the time, which is the failure mode
a firmware crash suite is least able to tolerate.

**Implementing bank selection in `waymaker-flash` so the fourth line could be checked against
real firmware.** Correct eventually and wrong now. It would move rung 0.2's design decisions
— seal placement, generation width, swap order — into a pull request whose subject is testing,
and it would spend part of a 12 B code-flash headroom on them before anything needs them. The
model gives the oracle's fourth line something to be checked against today and leaves the
decisions to the rung that owns them.

**Mutating the codec with `#[cfg(test)]` branches inside `waymaker-flash`.** A branch that
exists only to be wrong is a branch that ships. ADR 0012 already put the seal behind a trait
precisely so the choice would be swappable, and a mutant that is an ordinary implementation of
that trait costs the firmware nothing and is not reachable from it at all.
