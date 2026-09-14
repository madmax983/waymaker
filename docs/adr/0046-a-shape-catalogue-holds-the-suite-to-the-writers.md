# ADR 0046: A shape catalogue holds the suite to the writers

- Status: accepted
- Date: 2026-09-14
- Issue: [#130](https://github.com/madmax983/waymaker/issues/130)
- Supersedes: nothing
- Related: [ADR 0016](0016-the-storage-contract-is-a-conformance-suite-and-a-port.md)

## Context

Issue [#70](https://github.com/madmax983/waymaker/issues/70) asked for three mechanical
checks to replace round-by-round manual review of `waymaker-conformance`'s own test rigor.
The first landed. The other two were filed separately as issue #130, because each is its own
multi-round effort.

Item 2 is this ADR's: "every legal operation shape the firmware issues must appear in the
suite". `waymaker-flash`'s writers issue a program or an erase of one unit and of more than
one — the journal's frame body and the bank swap's header are wider than a single program
unit for any non-trivial record, and the bank swap's erase spans more than one erase block on
any device with more than the minimum two. Before this, the suite's two multi-unit cases
proved the *category* legal at a width of exactly two, and nothing stopped a *third* shape
from going unexercised the way earlier review rounds found a *fourth* one had. A hand-written
table would say so and rot the moment a case changed; what was missing was a check that fails
when it does.

Item 3 — a generator that mutation-tests the suite against its own model, with a conformant
arm — is `waymaker-spec`-shaped work and stays open. Deciding it here would be a decision
written for an implementation that does not exist, which
[the decision record itself](README.md) says a record must never be.

## Decision

**`waymaker-conformance`'s `shape` module is the catalogue.** Six shapes: a program of one
program unit and of more than one, an erase of one erase block and of more than one, a read
of one read unit and of more than one. A read wider than a record — the erased-tail scan
`recovery::Recovery` makes — is not a seventh row: it is still a multi-unit read, the same
shape a wide record's read has, and a caller-chosen chunk size is not a shape of its own.

**Each row names who issues it**, transcribed by a reviewer from `waymaker-flash`'s source
the way `STORAGE_CONTRACT_CLAUSES` transcribes design document §12 rather than deriving it:

| Shape | Issued by |
| --- | --- |
| `program-single-unit` | the commit seal in `append::Journal::commit` and `swap::Staged::commit` |
| `program-multi-unit` | the frame body in `append::Journal::stage` and the bank header in `swap::Prepared::stage` |
| `erase-single-block` | `swap::Swap::prepare` and `Installed::reclaim`, on a device whose bank is one erase block |
| `erase-multi-block` | `swap::Swap::prepare` and `Installed::reclaim`, on a device with more than two erase blocks |
| `read-single-unit` | `recovery::Recovery::stage`'s frame reads, on a geometry where a header or record fits in one read unit |
| `read-multi-unit` | `recovery::Recovery::stage`'s whole-record read and its erased-tail walk |

**`shape::ShapeWitness` proves the claim rather than only stating it.** It wraps a
`StableStorage` and, for every call the wrapped adapter *accepts*, records which shape it
had — crediting nothing a driver refused, for the reason `waymaker-conformance`'s own module
documentation gives for erased being a constant: a check that trusted a call before it was
accepted could be talked out of testing anything. A run of the full suite through a
`ShapeWitness` and a check that nothing is left unseen is
`crates/waymaker-conformance/tests/shapes.rs::a_full_run_issues_every_declared_shape` — the
mechanical half of item 2's ask, run every time the workspace test stage runs.

**The `storage-shapes` rule holds the four places the catalogue lives together**:
`xtask::docs::STORAGE_SHAPES`, `CLAUDE.md`, this ADR, and the crate's own table in
`crates/waymaker-conformance/src/shape.rs`. It compares ids, sentences and issuers in both
directions, the same shape `storage-conformance` already holds `STORAGE_CONTRACT_CLAUSES` to.

## Consequences

A shape added to the crate's table with no case behind it now fails a build rather than
passing review by omission, and a case deleted from the suite that was the only one issuing a
shape fails the same way. The catalogue does not claim to read `waymaker-flash`'s source
itself: `storage-shapes` compares tables, the way every other documentation rule in this
workspace does, and a shape appearing in the writers that nobody transcribed is a reviewer's
job — the same limit `storage-contract`'s own surface pin states for itself.

What this leaves owed:

- **Item 3 of issue #130 is still open.** A generator that mutation-tests the suite against
  its own model, with a conformant arm so a false positive is reachable and not only a
  broken adapter, is `waymaker-spec`-shaped work and is not attempted here.
- **A shape's width is a category, not an exact figure.** `program-multi-unit` covers two
  program units and two hundred alike; the suite's own case widths stay at two, which is
  what §12's contract needs to distinguish "one" from "more than one" and no wider a claim
  than that.
- **The two remaining loose ends issue #70 named stay open**: `Geometry`'s field widths
  diverging from issue #21's, and `durability::verify` being unable to check the `Reset` it
  is told.
