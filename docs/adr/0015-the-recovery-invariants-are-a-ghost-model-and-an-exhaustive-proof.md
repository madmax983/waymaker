# ADR 0015: the recovery invariants are a ghost model and an exhaustive proof

- Status: accepted
- Date: 2026-09-03
- Issue: [#20](https://github.com/madmax983/waymaker/issues/20)
- Supersedes: nothing
- Related: [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0014](0014-the-oracle-is-four-lines-and-the-sweep-is-seeded.md)

## Context

Design document §14 states five guarantees, and §15 states the four-line oracle that judges
one recovery against one run. Issue [#19](https://github.com/madmax983/waymaker/issues/19)
made that oracle real and swept it over seeded histories at every crash point the injector
lists. Issue [#20](https://github.com/madmax983/waymaker/issues/20) opens by saying why that
is not the end of it: "property tests cover what we remembered to generate. The five
guarantees in §14 are the invariants that must hold for *every* crash point, which makes them
worth stating as a specification rather than a test suite."

So the question is what a *specification* means in this workspace, and the answer is
constrained from three directions.

The first is the toolchain. The natural instrument is a deductive verifier — Verus, Kani,
Creusot. Each is a second toolchain, pinned to a nightly this workspace does not use, and
each would put a large download into a pipeline whose `rust-toolchain.toml` exists precisely
so that "the coverage stage has no hidden download in it". A verification stage that fetches
its own verifier is a stage that is green until the day the network says otherwise, and
§04's own rule here is that a measurement that did not happen is not a measurement that
passed.

The second is what the guarantees are *about*. Four of §14's five are statements about one
instant of one run: what reached media, what was handed to the world, which bank a reader
boots from. Those have a finite state space at any bound, which means they can be settled by
enumeration rather than by reasoning — and an enumeration that terminates is a proof of the
same kind as one an SMT solver produces, just bounded rather than general. The fifth,
bounded decoding, is a statement about a *decoder*, and it is settled by enumerating inputs
rather than states.

The third is the failure mode this repository has already learnt about twice. `waymaker-fault`
has `tests/teeth.rs` because a crash suite that cannot catch a weakened codec is a crash
suite that proves the harness runs. A specification has the same problem in a worse form: a
model that restates the implementation proves that a function agrees with itself, and a
proof over a state space that quietly shrank to nothing is a green build about no machine at
all.

## Decision

The recovery invariants are a **ghost model** in a new crate, `waymaker-spec`, and the
guarantees are discharged by **exhaustive enumeration** of its reachable states rather than
by sampling. There is no Verus stage and no second toolchain.

The crate is three layers, and the separation is the module list, so that issue #20's "the
verified core is small and stable, with unverified glue clearly separated" is the file
structure rather than a claim:

| | Modules | What it is |
| --- | --- | --- |
| The specification | `model`, `reader`, `invariant` | Pure, total, and independent of any implementation. Preconditions, postconditions, the ghost model of committed history, and §14's guarantees as predicates. Nothing here reads a byte. |
| The proofs | `explore` | Breadth-first enumeration of every reachable state, with a ceiling it fails against rather than truncates at. |
| The glue | `refine`, `obligation` | Unverified: the abstraction function from a real crashed run, and the table saying which guarantee is discharged by what. |

Five decisions inside that shape are the ones worth recording.

**The journal is a state machine whose preconditions are separately removable.** Five guards
— append-only writing, a barrier that claims only whole records, durable intent before
dispatch, never erasing the authoritative bank, and a strictly greater seal generation —
carry the whole specification. Each can be removed on its own, and `tests/necessity.rs`
requires each removal to make a *named* guarantee reachable-false. A guard that can be
deleted with every proof still green was never load-bearing, and there is no other way to
find out which those are.

**Recovery is a parameter, not a computation.** §14's first three guarantees are statements
about what a reader produces. A model that computed the recovery it then checked would prove
a function agrees with itself, so `invariant::check` takes the history a reader produced,
and `tests/teeth.rs` runs it against a catalogue of readers that are wrong in one way each.

**The specification and §15's oracle are compared in both directions.** `Journal::legal_recoveries`
says declaratively which histories §15 permits; `waymaker_fault::verify_oracle` decides
operationally. `tests/oracle.rs` requires them to agree over every reachable state and every
candidate history over the record universe — including histories no correct reader would
produce, which is the direction a test suite normally never checks.

**The firmware is compared against the model at every crash point.** `tests/refinement.rs`
drives `waymaker-flash`'s real codec through `waymaker-fault`'s injector, abstracts each run
into the model, and asks three questions: is this a state the model says is reachable, does
`Scan` produce exactly what the specified reader produces, and does the oracle agree. Without
this the model would be a second implementation with no tests.

**The bound travels with every claim.** `explore` carries its `Bound`, reaching the state
ceiling is an error rather than a truncation, and `tests/census.rs` pins the reachable state
count and requires every transition kind, every refusal reason, every §15 record-state edge
and every bank-shape edge to have been walked. A proof over three records is a proof over
three records, and a proof over a machine that shrank is not a proof at all.

All 6 recovery invariants carry an id, and `cargo xtask check-layering`'s **`recovery-spec`**
rule fails a build in which `CLAUDE.md`, this ADR, `xtask::docs::SPEC_CLAUSES` and the
crate's own `obligation.rs` stop naming the same set:

| Clause | Guarantee | Discharged by |
| --- | --- | --- |
| `prefix-safety` | recovery exposes only a legal prefix of committed records | exhaustive enumeration, plus refinement against the real `Scan` |
| `acknowledged-durability` | any record acknowledged after its barrier is recovered after reset | exhaustive enumeration |
| `durable-intent` | no Waymaker-dispatched effect lacks a recoverable schedule record | exhaustive enumeration |
| `single-authority` | exactly one bank is authoritative after any crash | exhaustive enumeration of the bank machine |
| `stable-redelivery` | retries and reboot redelivery reuse the original effect identity | every resume point of a bounded run, against the real allocator |
| `bounded-decoding` | malformed storage cannot cause out-of-bounds reads or allocation | exhaustive input sweep against the real decoder |

### The design smell the proof found

Issue #20 asks that "where a proof is hard, treat it as a design smell and refine the
representation before writing the proof around it". One place it was.

Acknowledged durability does not hold if a record's bytes may be written while an earlier
declared record's are not. `Declare, Declare, Program(1), Barrier` acknowledges the second
record behind an absent first one, and every prefix-honest reader loses it. The
representation was refined rather than the proof worked around: `Guard::AppendOnly` is the
precondition, `tests/necessity.rs` produces that exact counterexample with the guard removed,
and the equivalence it buys is stated as its own theorem —
`committed_history_and_declaration_order_are_the_same_prefix`. That theorem matters beyond
this crate: `waymaker_fault::Ledger::committed` filters out records that never reached media,
and its rationale ("a record none of whose bytes ever landed cannot occupy a position between
two records recovery did find") is exactly this theorem, previously resting on a comment.

A second, smaller one: erasing a bank was first guarded only against leaving *nothing* to
boot from. `tests/machine.rs` found that this permits erasing the *newer* of two sealed
banks, which reverts authority to an older generation — §14's failure table calls that
"never recover the old run as current" and forbids it. The guard is now that the
authoritative bank may not be erased at all, which subsumes the weaker rule.

## Consequences

**A bounded proof is not a general one, and the bound is not what it is short of.** Within
`Bound::PROOF` — three records, three generations, two banks — the enumeration is closed and
nothing is missed. Outside it the crate says nothing. But raising the bound to four or five
records changes no verdict, and it is worth saying so plainly rather than letting a reader
assume the counterexamples are one record away: the shapes of history this model admits are
`Whole^w Partial^{≤1} Absent^a`, which is one-dimensional, so a fourth record adds a longer
instance of a shape already there. What the proof is short of is *expressiveness*, in three
places named in `obligation.rs`'s `owed` column — banks that hold no records, no reboot, and
no writer that retries. A general proof is what Verus would give, and the door is left open:
the specification here is pure total functions over a small state, which is the form a Verus
port would want. Nothing about this decision has to be undone to take that step.

**The state count is a number a reviewer has to look at.** `REACHABLE_STATES` is pinned, so
a model change that shrinks the machine fails a build — and a legitimate model change fails
it too, and has to be updated in the same commit. That is deliberate: the dangerous direction
is silent shrinkage, and the price of catching it is that the number moves.

**Two clauses are only partly discharged, and both say so in a row rather than in a
sentence.** `single-authority` has no refinement because rung 0.1 has no two-bank adapter to
abstract — it is proved against the model alone, rung 0.2 owes the other half, and on a state
rebuilt from a real crashed run it is answered vacuously, which `tests/refinement.rs` asserts
in so many words rather than leaving to be found.
`bounded-decoding`'s allocation clause is structural rather than measured: `waymaker-core`
and `waymaker-flash` are `no_std` with no dependencies and no `extern crate alloc`, which the
`crate-attributes` and `kernel-zero-dependencies` rules fail a build over. Measuring it
instead would need a global allocator, and a global allocator needs the `unsafe` this
workspace denies.

**A fifth crate is a fifth crate.** `waymaker-spec` is another 85% coverage floor, another
manifest, another crate root the attribute rules run over. It is outside `default-members`
and no layer may depend on it, so no firmware target ever sees it — the same bargain
[ADR 0013](0013-the-fault-harness-is-a-crate-above-the-layers.md) made for the harness, and
made for the same reason: an exhaustive host-side search has no business inside an 8 KiB
flash budget.

**Wrong readers and relaxed machines are public API.** `reader::Mutant` and `Guards::without`
exist so that the proofs can be shown to fail. They are documented as such, and each is
required to be accounted for: `tests/teeth.rs` fails if a `Mutant` has no row saying which
guarantee catches it, and `tests/necessity.rs` fails if a `Guard` has no row saying what it
is for. The alternative —
keeping them in test files — means copying them into the four test targets that need them,
which is a mutant that drifts.

## Alternatives considered

**Verus, as the user's standing preference asks.** Not taken, for the toolchain reason
above: a second pinned nightly and a large download inside a pipeline built around neither.
Recorded as the door left open rather than as a rejection of the tool.

**Kani or another bounded model checker.** The same download problem, and it would buy
bounded verification of Rust code — which is what the enumeration here already gives, for
the part of the system that has a small state space, with no dependency at all.

**A specification document with no executable form.** Rejected on this repository's own
terms: a rule that can be broken silently is a comment, and a specification that nothing
compares against the code is the purest form of that.

**Extending `waymaker-fault`'s property tests instead.** That is what issue #19 did, and
issue #20 opens by saying why it is not enough. A seeded sweep covers what was generated; a
closed enumeration covers what exists.

**Putting the model inside `waymaker-fault`.** Tempting — it already owns §15's three record
states — and rejected because the two crates answer different questions. `waymaker-fault`
models *media* and enumerates *crash points*; `waymaker-spec` models *obligations* and
enumerates *states*. Merging them would put a specification's teeth in the same crate as the
harness those teeth are meant to be independent of.
