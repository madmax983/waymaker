# Architecture decision record

Every settled decision in this repository has a file here. Chat logs are not a record: they
cannot be searched for "why is the kernel not allowed a CRC implementation", and they cannot
be superseded.

## The record

| ADR | Decision | Status |
| --- | --- | --- |
| [0000](0000-template.md) | Template — copy this to start a new ADR | n/a |
| [0001](0001-one-pipeline-table-and-a-per-crate-coverage-gate.md) | One pipeline table, and a per-crate coverage gate | accepted |
| [0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md) | Size budgets are measured as deltas against a probe firmware | accepted |
| [0003](0003-the-eight-settled-design-decisions.md) | The eight settled design decisions (design document §02) | accepted |
| [0004](0004-the-layering-contract-is-a-table-a-gate-reads.md) | The layering contract is a table a gate reads | accepted |
| [0005](0005-documentation-is-checked-against-the-tables-it-describes.md) | Documentation is checked against the tables it describes | accepted |

## Reading the numbers

The number is the order a decision was **written down**, not the order it was taken. Two
of these are older than their numbers suggest:

- **0003** records the eight decisions design document §02 settled before any code existed.
  It was written third because the gates in 0001 and 0002 were built first. Issue
  [#11](https://github.com/madmax983/waymaker/issues/11) asked for §02 to be ADR-0001;
  renumbering two accepted ADRs and breaking the cross-link between them to free a number
  would have rewritten the one thing a decision record exists to keep stable.
- **0004** records issue #8's decisions, which predate this record entirely — ADR 0001 cites
  them as settled precedent. It was written when issue #11 asked for every settled decision
  to have an ADR and this one turned out not to.

In date order, then, the decisions run 0003 → 0004 → 0001 → 0002 → 0005.

## Writing one

Copy [`0000-template.md`](0000-template.md) to `NNNN-a-short-slug.md`, using the next unused
number, fill it in, and add a row to the table above.

`cargo xtask check-layering` fails a pull request that:

- skips or reuses a number, or names the file anything but `NNNN-slug.md` (`adr-numbering`);
- leaves out the title, `- Status:`, `- Date:`, `## Context`, `## Decision` or
  `## Consequences`, or uses a status outside `proposed` / `accepted` / `rejected` /
  `deprecated` / `superseded` (`adr-structure`);
- adds an ADR that this index does not link, or links one that does not exist
  (`adr-index`).

A decision that is revisited gets a **new** ADR that names what it supersedes. Editing an
accepted ADR to say something else turns the record into a snapshot of today's opinion,
which is the one thing it must never be.

## See also

- [CLAUDE.md](../../CLAUDE.md) — the invariants a contributor works to.
- [The architecture diagrams](../architecture.md) — the crate flow, the effect protocol, the
  two-bank swap.
- [The design document](../design/waymaker-design-v0.2.html) — v0.2, the source these
  decisions are taken against.
