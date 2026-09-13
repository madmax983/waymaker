# ADR 0042: a terminal watchdog reset is the fault-free run

- Status: accepted
- Date: 2026-09-13
- Issue: [#87](https://github.com/madmax983/waymaker/issues/87)
- Supersedes: nothing
- Related: [0023](0023-a-watchdog-reset-is-modelled-and-its-difference-is-one-return.md)

## Context

[`injections`](../../crates/waymaker-fault/src/inject.rs) lists a `Watchdog` point before
every operation, at every unit boundary, and after every operation. It lists none *after the
last operation*. For a writer shaped like `barrier()?; dispatch(); Ok(())`, no enumerated
point lets `barrier()` return and `dispatch()` run and *then* a reset happen. Every
`Watchdog` point on the last operation makes the call return an error, so `dispatch()` never
runs under it.

Issue #87 asks for a decision, not a guess. It gives two options: enumerate the point anyway,
or write down that the fault-free run already is that point. This ADR takes the second.

## Decision

`injections` still lists no point after the last operation.

There is no operation after the last one. A `Watchdog` point needs an operation to
interrupt. So a caller cannot ask "reset after everything" through an operation index that
exists.

The fault-free run answers the question instead. It is the one run in which the whole
sequence completes and every side effect after the last storage call still happens. A core
reset with nothing left to interrupt changes nothing this crate can observe: not the media,
not the write sequence, not the ledger, not what the closure does after its last storage
call. So the fault-free run already **is** "the writer finished and then the core reset".

`Harness::run_one` now accepts one exact hand-built [`Injection`]: `op` equal to the write
sequence's length, `progress: Progress::None`, `interruption: Interruption::Watchdog`. It
returns the fault-free run's image, ops, and ledger, tagged with the injection the caller
asked for. This is a narrow addition beside the two existing empty-sequence sentinels, not a
general exception: only this one exact shape is accepted, and every other past-the-end
injection is still refused as `HarnessError::CrashPointNeverFired`.

`injections` does not enumerate this point. `Harness::run` — the full sweep — is unchanged.

## Consequences

No pinned count moves: not `waymaker-conformance`'s `SWEEP`, not `waymaker-fault`'s
enumeration arithmetic, not `tests/swap.rs`'s per-operation formulas. The sweep a caller runs
today is the sweep it runs after this change.

A caller who wants "the writer finished and then the core reset" reads the fault-free run.
`crates/waymaker-fault/tests/watchdog.rs` pins two things: `run_one` answers the fault-free
run's image, ops, and ledger for the one accepted sentinel, and `injections` never emits a
point whose operation index reaches the end of the sequence under `Watchdog`. Either test
failing means this decision stopped holding.

## Alternatives considered

**Enumerate the point.** Rejected in [#87](https://github.com/madmax983/waymaker/issues/87)'s
own words: the point would be a run indistinguishable from the fault-free run in every field
the harness can observe, and "an exhaustive list that counts one crash point twice is no
longer a count of anything." Taking this path would also move five pinned counts for no new
coverage.

**Document only, and leave `run_one` refusing the hand-built point.** Cheaper, but it leaves
the issue's own acceptance test unwritable: nothing would show that the point a caller might
reach for really does answer to the fault-free run, only that `run_one` refuses to try. A
refusal proves nothing about what the point would have shown. The one-shape sentinel this ADR
adds costs one `||` in `Harness::injected` and answers the question directly.

**Widen the sentinel to accept any `op` past the end.** Rejected: a past-the-end `op` is
usually a caller's bug — a typo, a stale index after a writer changed — and
`crates/waymaker-fault/tests/harness.rs` already holds a regression for exactly that shape of
mistake having once been silently accepted. Only the one op index that is provably "after the
last operation" is a real question rather than a bug, so only it is accepted.
