# ADR 0041: The model gains banked records, a reboot, and a live compaction

- Status: accepted
- Date: 2026-09-13
- Issue: [#67](https://github.com/madmax983/waymaker/issues/67)
- Supersedes: nothing
- Related: [ADR 0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md),
  [ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)

## Context

Issue #20's review of `waymaker-spec` found that the ghost model's proofs were closed within
`Bound::PROOF`, but the bound was not what they were short of. The shapes of history the model
admitted were one-dimensional — `Whole^w Partial^{≤1} Absent^a` — because the model was missing
three dimensions entirely, all recorded in `obligation.rs`'s `owed` column:

1. **The banks held no records.** `begin_erase` cleared a bank's seal and never touched
   `self.records`, so `single-authority` was a predicate over `state.banks` alone. It could
   state "exactly one bank is bootable" and nothing about whose history that bank held —
   §14's "never recover the old run as current" was a sentence the model had no way to say,
   and no reader could falsify the guarantee by getting the bank wrong, because the guarantee
   never looked at the bank at all.
2. **There was no reboot.** `PowerLoss` and `Tear` ended a run; nothing consumed a recovered
   prefix and continued it. A device on its second or third boot — the ordinary case for a
   durable workflow engine — was not a `Journal` this machine could reach, so
   `acknowledged-durability`'s "recovered *after reset*" was checked at the moment of death
   rather than across one.
3. **A writer that retries was not describable.** `write` refused a second attempt at a torn
   record outright, so a live device stuck behind a `FailedProgram` had no move. The
   analogous real event, §12's "program and erase may fail" followed by the obvious firmware
   response, had no representation.

Two smaller gaps came out of the same review: generation comparisons used `saturating_add`,
which repeats `u32::MAX` forever rather than refusing past it, so a device at the ceiling
could seal a second bank at the same generation as the first — the tie `Guard::StrictGeneration`
exists to forbid. And `waymaker_fault::verify_oracle`'s prefix check rested on an argument —
"sound because no reachable state has a gap" — that was a theorem about the model the oracle
was being used to check, which is the shape of circularity a specification is supposed to
avoid.

## Decision

**Records carry a bank.** `model::Record` gained a `bank: BankId` field. `Journal::declare`
assigns new records to `current_bank()` — the sole authoritative bank once one exists, or
`BankId::A` by the device's pre-seal convention. `Journal::begin_erase` now retains only the
records (and dispatch-log entries) whose bank is not the one being erased: erasing a bank
destroys the journal in it, exactly as design document §10 says. `Journal::recover`,
`committed`, `declared` and `acknowledged` are all scoped to `recovering_bank()` — the sole
authoritative bank, or `BankId::A` before any seal, or nothing when authority is absent or
ambiguous — so a retired bank's leftover bytes are never mistaken for the run recovery is
about.

Two invariants needed the same scoping to stay sound once a device could hold two banks'
worth of history at once. `invariant::acknowledged_durability` and `durable_intent` both skip
a record or a dispatched id whose bank is not the recovering one: a promise made in a bank a
later swap retired is a promise the *next* run's recovery was never asked to keep, the same
way `continue_as_new` already forfeits the old run's effect identity (issue #95).
`Invariant::SingleAuthority` itself now takes the recovered history and refuses any record
that did not come from the sole authoritative bank — the fix issue #67 asked for by name:
`tests/teeth.rs`'s `Mutant::BootsTheRetiredBank` is a reader wrong in exactly this one way,
and it is caught by `single-authority` directly, not only by removing a guard.

**Identity survives an erase and a reboot.** `Journal` gained a `next_id: u32` counter;
`declare` takes the next id from it rather than from `records.len()`, so a record an erase
drops can never be reissued to a different one. `Bound::records` now caps how many records a
run may ever declare in total (matching its own doc comment), not how many are resident at
once, which is what keeps the state space finite once erase can free capacity back up.

**`Transition::Reboot`** is the one transition legal while `Journal::powered` is `false`, and
legal only then. It reseeds `records` and `dispatched` with exactly `recover()`'s answer and
sets `powered` back to `true` — a device on its second or third boot is now a state this
machine reaches, and `tests/machine.rs`'s
`a_reboot_keeps_exactly_the_recovered_prefix_and_nothing_it_declared_after` is the claim that
it reseeds with nothing more forgiving.

**Compaction needed no transition of its own.** `begin_seal` and `begin_erase` never
consulted the *other* bank's record state, so once records carried a bank, a live device
behind a torn record in its current bank could already seal the other, blank bank and carry
on there — retrying by replacement rather than by repair, which is what §10's swap always
was. `tests/compaction.rs` is the proof that this reachable state is really reached, with no
power loss anywhere in the run that got there, and that recovery correctly abandons the torn
bank rather than stopping behind it.

**Generation arithmetic refuses at the ceiling.** `begin_seal`'s `seen.saturating_add(1)`
became `seen.checked_add(1).ok_or(Illegal::GenerationExhausted)?`, matching
`Generation::successor`'s real refusal (ADR 0017) instead of silently repeating `u32::MAX`.
No bound small enough to explore ever reaches the ceiling, so this is proved by a hand-built
state in `model.rs`'s own `#[cfg(test)]` module rather than by the exhaustive search.

**The oracle was not made stricter.** A direct "no gap before committed history" check in
`waymaker_fault::verify_oracle` was tried and reverted: `waymaker-fault`'s own
`tests/harness.rs` drives a writer whose middle record's program call fails outright and who
carries on to the next, and `Ledger::committed`'s filter is what lets a correct recovery of
the two records that really landed be accepted at all. The oracle's permissiveness is
deliberate — a property of the generic harness, not a bug the model's stricter, append-only
discipline should be pushed down onto it. The circularity is closed instead by
`tests/oracle.rs`'s `the_ledger_the_oracle_judges_never_has_a_gap_before_committed_history`,
which checks the claim directly against the `Ledger` the agreement tests build rather than
importing it from a theorem about a different type (`tests/machine.rs`'s, about `Journal`).

## Consequences

The reachable state space at `Bound::PROOF` grew from 2,576 states to 10,104, and every
transition's edge count moved with it (`tests/census.rs`). Two spine claims that used to be
statements about `state.records()` as one sequence —
`a_torn_record_is_always_the_last_one_on_media` and `an_acknowledged_record_is_never_behind_a_gap`
— are now per-bank claims, because two banks can each independently hold one torn record at
once without either interruption being impossible. The record-history-shapes census is
likewise now a per-bank claim rather than a claim about the concatenation of both banks'
records.

What remains owed, and is written down as such in `obligation.rs`'s `single-authority` row:
the refinement against a real two-bank writer. `tests/refinement.rs` still drives no writer
across `waymaker_flash::bank`'s real swap, so a reconstructed state still has no banks and
`single-authority` is still discharged against the model alone —
`a_reconstructed_state_cannot_falsify_the_fourth_guarantee` says so directly. Closing that is
issue #22's adapter, refined the way the record codec already is; it is a project of its own
rather than a corollary of this one.

Issue #95's gap — that `continue_as_new` does not carry an effect's identity across a
swap — is unaffected by this change and is not what
`Guard::DispatchFromCurrentBank` would have been for, had it stayed: the necessity proofs
showed dispatching from a bank a swap has since retired breaks no guarantee once
`durable_intent` correctly treats it as moot, so no such guard was added. A guard this
crate's own proofs cannot show necessary is a guard `tests/necessity.rs` exists to catch,
and it did.

## Alternatives considered

**A `Guard::DispatchFromCurrentBank`**, refusing a live dispatch against a bank a swap has
retired. Tried first, on the reasoning that real firmware's swap consumes the old run's
writer. `tests/necessity.rs` found it unnecessary: once `durable_intent` treats a
retired-bank dispatch as moot (the run it belonged to is superseded either way), removing the
guard breaks no proof. Kept out, per this crate's own stated rule that a guard removable
without cost was never load-bearing.

**A same-bank retry at a new offset**, modelling `Interruption::Failure` as "abandon the torn
record's span and declare a fresh one further into the same bank's journal." Rejected: NOR
flash physically cannot make a new append point past a torn or unsealed frame without an
erase (ADR 0018's anti-bricking rule), so the only real recovery from a stuck append point is
the two-bank swap already in the model. Modelling a same-bank retry would have described a
firmware behaviour that cannot exist.

**Refusing the oracle's committed-history gap directly**, closing issue #67's circularity by
tightening `waymaker_fault::verify_oracle`. Reverted after `cargo test -p waymaker-fault`
found it broke `tests/harness.rs`'s
`a_record_that_never_reached_media_does_not_occupy_a_position_in_history`, a test written on
purpose to hold the oracle to its documented, more permissive contract. The generic harness
and the specified model's stricter, append-only discipline are different things by design,
and the fix belongs in a test that says so rather than in code that erases the difference.
