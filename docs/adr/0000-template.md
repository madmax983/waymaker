# ADR 0000: template

- Status: proposed
- Date: YYYY-MM-DD
- Issue: [#N](https://github.com/madmax983/waymaker/issues/N)
- Supersedes: nothing
- Related: nothing

> Copy this file to `NNNN-a-short-slug.md`, where `NNNN` is the next unused four-digit
> number. `cargo xtask check-layering` fails a pull request that skips a number, reuses one,
> names the file some other way, drops a section below, or leaves the ADR out of
> [the index](README.md). Statuses are `proposed`, `accepted`, `rejected`, `deprecated`, or
> `superseded`; anything else is a violation, because "status: mostly" is how a decision
> nobody took ends up looking like one that was. Delete this block when you write the ADR.

## Context

What made a decision necessary. The forces, the constraint, the failure that would follow
from doing nothing. Quote the design document section this touches, and link the issue.

Write this so that someone who was not in the conversation can tell whether the decision
still applies. A context that only says "we needed to pick something" is a context that
cannot be revisited.

## Decision

What was decided, stated so that a reader can tell whether the code obeys it. Where the
decision is enforced by a gate, name the rule id `cargo xtask check-layering` emits — a
decision this repository can fail a build over is worth more than one it can only describe.

## Consequences

What follows, including what got worse. An ADR that lists only benefits is a decision that
was not weighed.

## Alternatives considered

Optional, but usually the most useful section a year later: what else was on the table, and
the specific reason it was not taken.
