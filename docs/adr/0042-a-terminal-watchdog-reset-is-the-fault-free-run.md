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
not the write sequence, not the ledger, not what the writer does after its last storage
call. So the fault-free run already **is** "the writer finished and then the core reset".

`Harness::run_one` now accepts one exact hand-built [`Injection`]: `op` equal to the write
sequence's length, `progress: Progress::None`, `interruption: Interruption::Watchdog`. It
answers with a clone of the fault-free run's own session, relabelled with the injection the
caller asked for — it does **not** run the writer a second time to check. Every other
past-the-end injection is still refused as `HarnessError::CrashPointNeverFired`.

`injections` does not enumerate this point. `Harness::run` — the full sweep — is unchanged.

## Consequences

No pinned count moves: not `waymaker-conformance`'s `SWEEP`, not `waymaker-fault`'s
enumeration arithmetic, not `tests/swap.rs`'s per-operation formulas. The sweep a caller runs
today is the sweep it runs after this change.

A caller who wants "the writer finished and then the core reset" reads the fault-free run.
`crates/waymaker-fault/tests/watchdog.rs` pins three things: `run_one` answers the fault-free
run's image, ops, and ledger for the one accepted sentinel; the writer is not run a second
time to produce that answer; and `injections` never emits a point whose operation index
reaches the end of the sequence under `Watchdog`. Any of the three failing means this
decision stopped holding.

**A first version of this ADR ran the writer a second time and trusted the ordinary trace
comparison to confirm it matched.** A GitHub review of the pull request (Codex) found the
gap: `Session::injection` is public, so a writer can see the crash point is armed and add a
record mark — `begin_record` or `end_record` — after its last storage call. `trace` compares
operations up to the crash point and clamps to the sequence's length, so a mark placed
exactly at `ops.len()` sits outside every span it computes, on both sides of the comparison.
Such a writer could make the second run's ledger disagree with the fault-free run's while
`trace` reported no difference. The fix removes the second run entirely for this one shape:
the answer is a clone of `baseline`, not a fresh session checked against it, so there is
nothing left for a writer to notice. `a_watchdog_reset_after_the_last_operation_does_not_run_the_writer_again`
holds it, with a writer built to do exactly what the review described.

This also let `is_empty_sequence_sentinel` drop its `Watchdog` arm. An empty sequence's
"before everything" and "after everything" are the same point — `injection.op == 0` is
`injection.op == baseline.ops.len()` there — so the new sentinel already answers it, and the
two predicates no longer overlap.

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
adds is a short early return in `Harness::injected` and answers the question directly.

**Widen the sentinel to accept any `op` past the end.** Rejected: a past-the-end `op` is
usually a caller's bug — a typo, a stale index after a writer changed — and
`crates/waymaker-fault/tests/harness.rs` already holds a regression for exactly that shape of
mistake having once been silently accepted. Only the one op index that is provably "after the
last operation" is a real question rather than a bug, so only it is accepted.
