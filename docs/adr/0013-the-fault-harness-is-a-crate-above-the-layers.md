# ADR 0013: The fault harness is a crate above the layers, and the storage contract is pinned

- Status: accepted
- Date: 2026-09-02
- Issue: [#18](https://github.com/madmax983/waymaker/issues/18)
- Supersedes: nothing
- Related: [0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md), [0004](0004-the-layering-contract-is-a-table-a-gate-reads.md), [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md), [0008](0008-the-replay-cursor-is-pumped-by-its-caller.md)

## Context

Design document §15 opens with a sentence that is a scheduling decision as much as a
testing one: "crash testing is part of the design, not a post-MVP hardening phase". Issue
[#18](https://github.com/madmax983/waymaker/issues/18) is the rung-0.1 deliverable that
makes it true — an in-memory `StableStorage` parameterised over geometry, fault injection
covering torn writes, power loss and failed operations, and a model that distinguishes
three record states — with two exit criteria: the harness "can enumerate every crash point
in a given write sequence, not sample it randomly", and it "is reusable by `waymaker-flash`
and the effect-protocol tests without modification".

Three questions had to be answered before any of it could be written.

**Where does §12's `StableStorage` live?** It did not exist in the workspace at all;
`frame.rs` referred to it by linking to the design document. `waymaker-core`'s must-not-own
cell names "storage driver", and §05's own diagram gives `waymaker-flash` the words
"erase/program/barrier". So the contract is `waymaker-flash`'s.

**Where does the harness live?** Four placements were weighed.

| Placement | Why not |
| --- | --- |
| A shared module under `waymaker-flash/tests/` | An integration test exports nothing. The effect-protocol tests of rung 0.3 could not reach it, which is half the issue's second exit criterion. |
| A `pub mod fault` in `waymaker-flash` | Every public function of a layer must be reached by the size probe, so an exhaustive host-side enumerator would be charged against an 8 KiB code-flash budget. It also cannot be written under `#![no_std]`: the enumeration is a `Vec`. |
| A non-default `fault-model` feature of `waymaker-flash` | CI builds and tests everything with `--no-default-features`, so the module would be uncompiled and uncovered; and a crate cannot cleanly dev-depend on itself to turn its own feature on. |
| A new workspace member above the layers | Chosen. |

**How does a layer use it, then?** It does not, and that is the point rather than a
concession. `waymaker-fault` depends on `waymaker-flash`; a dev-dependency the other way
would be a cycle, and `check_dependency_direction` reads `policy::LAYERS` in every
dependency kind, so it would also be a violation. The tests that drive the harness live
*with* the harness. What "reusable without modification" then has to mean is that the
harness is generic over the writer rather than over the caller's crate — which is a
stronger property, and one that can be demonstrated instead of asserted.

## Decision

**`Geometry` and `StableStorage` go in `waymaker-flash/src/storage.rs`**, and that file's
public surface is pinned by a new `storage-contract` gate rule. §05 says a host or browser
adapter "must not expand the firmware traits to accommodate host conveniences"; that is a
rule about absence, and a test cannot call a method that is not there.

**The erase, program and read units must be powers of two.** Every alignment check is
`offset & (unit - 1)`. Written as `offset % unit` instead, `thumbv6m-none-eabi` — which has
no divider — links `__aeabi_uidivmod` and the incremental code-flash measurement moves by
408 B against an 8 KiB budget. Measured on the size probe, not estimated. Nothing is given
up: erase blocks, program pages and read widths are powers of two on every NOR part this
firmware could run on. The *capacity* is not required to be one, because a device of three
erase blocks is an ordinary thing.

**`waymaker-fault` is a new workspace member in a new policy category**,
`policy::TEST_SUPPORT_CRATES`, alongside the layers, the host tooling and the size probe. It
is outside `default-members`, so no firmware target builds it; it uses `std`, because the
enumeration is a `Vec`; and it has no third-party dependencies, so the workspace stays as
it is.

**Crash points are a pure function.** `injections(&[Op], Geometry)` takes a recorded write
sequence and returns the complete list *for that sequence*: `(0, None, PowerLoss)`, every interior tear point of
every operation, `(i, Whole, PowerLoss)` for each operation — which is also "power loss
before operation `i + 1`", so the two are one entry — and the failure points of each
operation that can fail after the fact. There is no seed and no sampling budget.

**The enumeration is exact for the fault-free sequence and is not a fixpoint.** `Harness`
runs the writer once with nothing armed to learn the sequence, then once per crash point.
Everything before an injection is identical by construction, so each crash point is aimed at
the operation it names — but an operation a writer performs *only* because a call failed, a
retry being the obvious one, exists in no fault-free sequence and therefore has no crash
points of its own. Iterating to a fixpoint over the sequences a reacting writer can produce
is a larger machine than issue #18 asks for; the limitation is written down here and in
CLAUDE.md's "What is not checked" rather than left for someone to discover.

**A writer that fails with nothing armed, or that is not a function of the storage it was
handed, is refused.** `Harness::run` returns a `Result`. A writer that gives up in the
fault-free run produces a short sequence, a handful of crash points, and a suite in which
every assertion passes because nothing was checked; a writer carrying state between runs
aims its crash points at operations that are not there. Both are the shape CLAUDE.md's "a
measurement that did not happen is not a measurement that passed" is about, so neither is a
value a caller can ignore.

**A program tears at every byte; an erase is interrupted at erase blocks.** §15 asks for
"every byte/program unit" for writes, and the unit boundaries are a subset of the byte
boundaries. No device erases byte by byte, so offering byte granularity there would invent
failure modes rather than cover them. The cost is that a *partially erased block* — one
whose cells were left half-restored and may drift — is not representable at all: an erase is
modelled as whole blocks back to `0xFF` and an untouched suffix. §09's stale-tail family
lives partly there, and what the model does cover of it — erased bytes with real frames
behind them — is swept in `tests/committed_prefix.rs`.

**The three record states are computed as the writer runs, not reconstructed from bytes.**
`Attempted`, `PossiblyDurable` and `Acknowledged` differ by *when the power went away
relative to a barrier*, which is not a question an image can answer. They are the three §15
names and there is no fourth: *tornness* — part of what a record meant to put on media is
there and the rest is not — is kept beside the state rather than as a state, because §15
permits recovery to include "an unacknowledged **complete** record" and complete is the
load-bearing word. A torn record is `PossiblyDurable`, is never acknowledged by a later
barrier, and may not be produced by recovery.

Tornness is measured against the media, not the byte count. A frame is padded to the program
granularity with `0xFF`, and programming `0xFF` over erased media changes nothing, so a
write interrupted inside its own padding leaves exactly what a completed write would have —
and calling that torn would fail a recovery that is entirely correct. By the same token a
write that changed no cell did not start.

**§15's core property oracle is a function**, `verify_recovery`, and it fails closed: a
recovery that is not a prefix, one that produces a record which never reached media, one
that loses an acknowledged record, and a ledger that names one record twice are four
distinct breaches rather than four ways to pass quietly.

## Consequences

The harness names no record type, no frame constant and no activity kind. Three writers are
driven through it unmodified — `waymaker-flash`'s §09 record codec, §11's durable-intent
effect shape, and a writer with a byte layout of its own — and rung 0.3 adds a fourth by
writing a closure, not by editing this crate. Two of the three share an `append` helper and
the same `frame::encode`/`Scan` pair, so what they demonstrate together is that the harness
is indifferent to the *protocol* rather than to the encoding; the third is what shows it is
indifferent to the encoding too.

What "reusable by `waymaker-flash`" cannot mean here is a test *in* `waymaker-flash` using
it. That would be a dependency a layer may not have, so the criterion is met by the harness
being generic over the writer and by those tests living with the harness. The crate
documentation says so where a reader will meet it, not only here.

The size probe now links a driver that validates and returns, so the delta charges for the
geometry arithmetic §12 obliges every port to run and not for a model of media. That is
directional rather than exact: of the 620 B the change adds, about 24 B is the probe's own
driver — measured by replacing it with direct calls to the three validators — and the rest
is `storage.rs`. The incremental code-flash figure moves from 7560 B to 8180 B against the
8192 B budget, which leaves 12 B. That is a real result and it is reported rather than absorbed: rung 0.2's banks,
seals and barriers do not fit, and the budget conversation §04 implies has to happen before
they are written.

What this does not give anyone is a proof. The harness models NOR flash — erased is `0xFF`,
programming only clears bits, an operation the geometry forbids never reaches media — and a
model that is wrong in the same direction as the code would agree with it. Three things
bound that: the model's own properties are tested against what hardware does rather than
against the code that uses it; a recovery deliberately short by one record is asserted to be
*caught*, so the oracle can fail; and §15's hardware half — "run hardware power-cut loops
against real NOR flash" — is still owed, at rung 0.2, where the boards are.

The remaining cost is time. Every crash point is a re-run of the writer, so a three-record
journal is 154 runs and a longer sequence is longer still. That is the price of enumerating
rather than sampling, and it is the trade the issue asks for by name.
