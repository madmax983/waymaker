# ADR 0041: the bank refinement abstracts a real swap

- Status: accepted
- Date: 2026-09-13
- Issue: [#73](https://github.com/madmax983/waymaker/issues/73)
- Supersedes: nothing
- Related: [0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md),
  [0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)

## Context

[ADR 0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md) proves
`single-authority` — "exactly one bank is authoritative after any crash" — against the ghost
model alone. `tests/refinement.rs` compares the model to real firmware for the other three
state-level guarantees, but not for this one: at the time, `waymaker_flash::bank` did not
exist, so there was no real writer to compare against, and a state rebuilt from a real run
had no banks in it at all.

Issue [#22](https://github.com/madmax983/waymaker/issues/22) added that writer. Issue #73 is
this refinement catching up: `crates/waymaker-spec/src/obligation.rs` has said "there is now
a two-bank adapter to abstract" since #22 landed, and this ADR is the answer.

## Decision

`waymaker-spec`'s abstraction gains two functions, `refine::bank_after_erase` and
`refine::bank_after_seal`. Each folds one real crash — driven through
`waymaker-fault`'s injector — into a `crate::model::Bank`. `Observation` carries the result,
`Journal::reconstructed` builds a real state from it, and `tests/refinement.rs` drives a swap
writer styled on `crates/waymaker-fault/tests/banks.rs`'s own, checking every crash point
against three things: the model's reachable set, the guarantee itself, and
`waymaker_flash::bank::select` over the same crash's real bytes.

Two things about the fold are worth stating, because both are the opposite of the first
guess.

**A bank's committed state comes from what its bytes show, not from what its call
returned.** `waymaker-fault` writes land synchronously, so a program call that reached media
in full leaves the same bytes whether it returned `Ok` or `Err` — a watchdog reset finishes
the unit in flight and still answers `Err`. Design document §02 decision 7's "durable" is
therefore not "the call succeeded"; it is "the bytes are there", and a caller reads that back
from the region itself (`waymaker_flash::bank::sealed_generation` for a seal, a byte scan for
an erase) rather than from `Injection`'s progress, which cannot see a watchdog's rounding.

**The model's generations start at 1; the firmware's start at 0.** `Journal::step`'s
`begin_seal` numbers a device's first-ever seal `1`, so that "no seal" and "not yet
distinguished from no seal" both read as nothing. `Generation::FIRST` is `0`, because the
firmware has `Option::None` for that case. A caller passes `real_generation.0 + 1`; both
schemes increment by one per seal, so the shift holds at every later generation too.

## Consequences

`single-authority`'s `owed` note in `obligation.rs` no longer says the refinement is missing.
What it still says: the model's banks hold no records, so "never recover the old run as
current" is not a statement the machine can make, only "exactly one bank is bootable"; and
its generations are unbounded integers, where the firmware refuses at `Generation::MAX`
rather than proving the refusal is never needed. Neither is closed here, and neither is a
refinement question — both are gaps in what the *model* can express.

Two independent judgements have to agree at every crash point: `Journal::authoritative` on
the reconstructed state, and `waymaker_flash::bank::select` on the crash's real bytes. A wrong
fold could still land on a state the model's search reaches — the reachable set is large — but
it could not also agree with the real selection by accident at every one of the run's crash
points. That is what makes the check load-bearing rather than a formality.

## Alternatives considered

**Extend the model so a bank can hold a record.** This is the larger of the two remaining
gaps, and closing it changes what `single-authority` can even claim ("never recover the old
run as current" needs it). It is a model change, not a refinement, and belongs with issue
#73's "Also owed" items rather than folded into this one.

**Derive "committed" from `waymaker-fault`'s barrier bookkeeping**, the way a record's
`Durability` already does. Rejected: a bank's seal has no record wrapped around it in the
writer this ADR drives, and — more importantly — the harness's own barrier tracking answers a
different question ("did a later barrier return") than the one that decides what
`waymaker_flash::bank::select` reports ("what do the bytes say"). Using it would have made the
abstraction disagree with the real selection at the crash point right after a seal lands and
before its barrier runs, which is exactly the point `crates/waymaker-fault/tests/banks.rs`
already counts as authoritative.
