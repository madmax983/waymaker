# ADR 0003: The eight settled design decisions

- Status: accepted
- Date: 2026-09-01
- Issue: [#11](https://github.com/madmax983/waymaker/issues/11)
- Supersedes: nothing
- Related: [ADR 0001](0001-one-pipeline-table-and-a-per-crate-coverage-gate.md), [ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md)

## Context

Design document §02 settles eight decisions. They are the ones every later decision is
taken against: what the kernel may depend on, how replay works, when a physical effect is
allowed to happen, what a record is made of, where async syntax lives, what is never
snapshotted, how a run is replaced atomically, and what a durable timer requires.

Until now they lived in one HTML document and in whatever conversation produced it. That is
enough to build against and not enough to argue with: there is no record of what each
decision costs, and no way for a later ADR to say "this revisits §02's third decision" and
have anyone know which one that is.

Issue #11 asks for these to be recorded as ADR-0001. By the time the issue was worked,
0001 and 0002 were accepted decisions with a cross-link between them and a merge history
behind them. Renumbering an accepted ADR to free a number would rewrite the one thing a
decision record exists to keep stable, so the eight are recorded here as 0003 and
[the index](README.md) states the ordering. The numbers are the order decisions were
*written down*, not the order they were taken.

## Decision

**The eight decisions below are recorded as settled, each with a stable id.** The id is what
`CLAUDE.md`, this ADR, and any later ADR that revisits a decision all cite, so that
"revisits the two-bank decision" is a link rather than an archaeology problem.

**The list is a table in the gate, not only prose here.** `xtask::docs::SETTLED_DECISIONS`
holds the eight ids and headlines; the `settled-decisions` rule fails a build in which this
file stops recording one of them, and the `claude-md` rule fails a build in which `CLAUDE.md`
stops citing one. A ninth decision added to §02 is a row in that table, and the row fails
until it has been written down.

**Revisiting one of these is a new ADR, not an edit to this one.** This file records what
was settled and why; if a decision changes, the new ADR names the id, sets this file's
entry in context, and says what it now is. An ADR that is edited to say something else is
a record of nothing.

---

<a id="kernel-is-dependency-free"></a>

### 1. The kernel is `no_std`, `no_alloc`, and dependency-free

- Decision id: `kernel-is-dependency-free`
- Design document: §02, §05

Serialization, logging, executors, and hardware drivers live above `waymaker-core`. The
kernel owns borrowed record views, effect identity, the replay cursor, transition rules, and
capacity errors, and reaches for nothing at all — not another layer, not a registry crate.

*Why it is not a convention:* `cargo xtask check-layering`'s `kernel-zero-dependencies` rule
reads the resolved `cargo metadata` graph, so an optional dependency or a
`[target.'cfg(...)'.dependencies]` table does not slip past it.

*Cost:* every convenience a dependency would have provided is either written here or pushed
up a layer. CRC is the clearest example: it is a natural thing for a journal to own, and it
lives in `waymaker-flash` instead.

<a id="replay-is-sequential"></a>

### 2. Replay is sequential

- Decision id: `replay-is-sequential`
- Design document: §02, §06, §08

A cursor advances through history in workflow order. The engine never requires
`Journal::get(id)` or an in-memory event index.

*Why:* it is what makes replay constant-memory. History streams from flash through one
caller-owned scratch page, with no random reads and no lookup table.

*Cost:* a workflow cannot ask about an effect it has already passed. Anything it needs later
it must carry forward itself.

<a id="durable-intent-before-effect"></a>

### 3. Physical effects happen only after durable intent

- Decision id: `durable-intent-before-effect`
- Design document: §02, §07

The schedule record crosses a durability barrier before dispatch. The seven-step protocol —
[drawn here](../architecture.md#durable-effect-protocol) — writes a frame, barriers, writes
a seal, barriers, and only then dispatches.

*Cost:* two barriers per committed record, and no exactly-once physical promise. Power can
fail after the activity has changed the world and before its outcome is committed, so the
same stable effect id is redelivered. Exactly-once needs an idempotent activity or a
downstream system that deduplicates that id — this decision is what makes the id stable
enough for either to work.

<a id="numeric-kinds-and-borrowed-bytes"></a>

### 4. History records use numeric kinds and borrowed bytes

- Decision id: `numeric-kinds-and-borrowed-bytes`
- Design document: §02, §09

Strings, `Vec`, Serde, and Postcard are optional user-facing conveniences, not wire-format
requirements. A record is a numeric kind and a borrowed slice.

*Why:* it is what lets [decision 1](#kernel-is-dependency-free) hold. A wire format that
needed a serialization framework would put one in the kernel.

*Cost:* the ergonomic surface is a layer up, and the kernel's API is full of lifetimes.

<a id="async-syntax-is-an-adapter"></a>

### 5. Normal async syntax is an adapter

- Decision id: `async-syntax-is-an-adapter`
- Design document: §02, §05, §13

`waymaker-embassy` supplies the ergonomic façade — `ctx.activity(...).await`, timers, the
dispatcher. The persistence protocol does not depend on Embassy or on `Future`.

*Why:* the protocol is testable on the host without an executor, and a host or browser
adapter can be written later against the same semantic kernel.

*Cost:* the pleasant API is not the one the guarantees are stated over, so a reader has two
surfaces to learn. `waymaker-embassy` must not expand the firmware traits to accommodate
host conveniences.

<a id="no-snapshotted-futures"></a>

### 6. Arbitrary suspended futures are not snapshotted

- Decision id: `no-snapshotted-futures`
- Design document: §02, §03, §10

Bounded-history reclamation happens at an explicit `continue_as_new` boundary, where the
workflow supplies the bounded input for its next run.

*Why:* a Rust future's suspension state is hidden, unstable across compilations, and
unbounded. Persisting it would be the one promise this engine could not keep across a
firmware update.

*Cost:* `continue_as_new` is manual, and history capacity is a thing a workflow author has
to think about. `HistoryNearCapacity` is raised early rather than committed history being
overwritten to make room.

<a id="two-banks-for-atomic-replacement"></a>

### 7. Two flash banks provide atomic run replacement

- Decision id: `two-banks-for-atomic-replacement`
- Design document: §02, §10

A new run becomes authoritative only after its payload and generation seal are durable. The
bank with the highest valid generation seal wins; the swap is
[drawn here](../architecture.md#two-bank-swap).

*Why:* it makes run replacement a single durable event. A crash before the seal recovers the
old run, a crash after it recovers the new one, and recovery never combines their
footprints.

*Cost:* two erase blocks minimum — typically 2 × 4 KiB — for a device that will only ever
have one live run.

<a id="durable-timers-need-durable-time"></a>

### 8. Durable timers require durable time

- Decision id: `durable-timers-need-durable-time`
- Design document: §02, §11

A resettable monotonic clock cannot claim that time elapsed while power was absent, so
Waymaker exposes timer semantics that match the hardware's actual clock capability rather
than pretending to a wall clock it does not have.

*Cost:* "sleep for an hour" means different things on a device with a backup-domain RTC and
on one without, and the API has to make that visible instead of hiding it.

## Consequences

- The eight ids are now a public vocabulary. `CLAUDE.md` cites them, this ADR defines them,
  and the `settled-decisions` and `claude-md` rules fail a build in which either drifts.
- A ninth decision in §02 is a row in `xtask::docs::SETTLED_DECISIONS`, and the row fails the
  gate until it is recorded here. That is the intended friction.
- The gate checks that each id and each headline appears, not that the prose beneath it is
  true. Text matching cannot do more than that; a wrong explanation is caught in review, as
  it would be anywhere.
- The numbering does not run oldest-decision-first. §02's decisions predate ADRs 0001 and
  0002 and carry a higher number, which is stated in the index and in the Context above so
  that nobody reads the sequence as a timeline of when things were decided.

## Alternatives considered

- **Eight separate ADRs, one per decision.** Truer to "one ADR, one decision", and it would
  have made each one individually supersedable. Rejected because these eight were settled
  together as one design, and eight files each recording a sentence from §02 would have made
  the record look larger than the deliberation behind it. A decision that is *revisited*
  gets its own ADR, which is where the granularity is actually useful.
- **Renumbering ADRs 0001 and 0002 to free 0001, as issue #11 literally asks.** Rejected: it
  rewrites accepted decisions and breaks ADR 0002's link to ADR 0001 to satisfy a number.
- **Leaving §02 as the only record.** Rejected because §02 states the decisions without their
  costs, and a decision recorded without its cost cannot be revisited honestly.
