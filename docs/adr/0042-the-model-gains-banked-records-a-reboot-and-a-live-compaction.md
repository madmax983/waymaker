# ADR 0042: The model gains banked records, a reboot, and a live compaction

- Status: accepted
- Date: 2026-09-13
- Issue: [#67](https://github.com/madmax983/waymaker/issues/67)
- Supersedes: nothing
- Related: [ADR 0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md),
  [ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md),
  [ADR 0041](0041-the-bank-refinement-abstracts-a-real-swap.md)

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
legal only then. It sets `powered` back to `true`, and every *media-backed* record survives
untouched — `dispatched` and every bank's seal too — so `recover()` computes the recovered
prefix fresh from those same bytes rather than needing the transition to prune anything
toward it. A device on its second or third boot is now a state this machine reaches, and
`tests/machine.rs`'s `a_reboot_changes_nothing_media_backed_but_the_power` is that claim.

What does not survive is a record that was never media-backed to begin with:
`Journal::declare` puts a record in `records` before a single byte is programmed, so an
`OnMedia::Absent` record is a fact about RAM, and a power cut takes RAM with the power.
`reboot` discards every still-`Absent` record and prunes `dispatched` to match — the same way
`begin_erase` already does for an erased bank's records — and rolls `next_id` back by the
count discarded, floored at one past the highest surviving id so an id an earlier
`begin_erase` already spent in this same run is never handed out twice.
`tests/machine.rs`'s `a_reboot_discards_only_records_still_absent_from_media` and
`a_crash_before_the_first_media_write_never_spends_capacity` are that pair of claims, and
`a_declared_record_is_never_renumbered_or_removed_except_by_erasing_its_bank` gained `Reboot`
as a second, narrower exception beside `BeginErase`'s.

An earlier version of this transition left *every* record in place, `Absent` ones included,
on the reasoning that a reboot "changes nothing but the power" — this ADR used to say so, and
named `a_reboot_changes_nothing_but_the_power` as the test that proved it, which is now
`a_reboot_changes_nothing_media_backed_but_the_power`'s narrower claim. Codex's review of the
pull request that added this ADR found the defect that reasoning hid: `Declare(Schedule)`
immediately followed by `PowerLoss`/`Reboot` left the phantom declaration in place, and
`unresolved_schedule_in` and `whole_before` read it exactly as they would a real one —
permanently stranding that bank, with no real bytes anywhere to blame it on. Discarding
`Absent` records fixes it, and a still-earlier version of *that* fix — dropping them without
rolling `next_id` back — traded one strand for another: a device that crashes before its
first-ever media write, over and over, would otherwise eventually read
`Illegal::CapacityReached` against media that has never held a single byte, which
`waymaker_core::id::EffectIdAllocator::resume`'s real behaviour (deriving the next sequence
from the highest *committed* one) says a real device never does.

Before either of the above, a still earlier version of this transition pruned `records` down
to `recover()`'s own answer entirely, on the reasoning that a reboot should carry forward
"nothing more forgiving" than what recovery already permits. That version's defect was
sharper still: the prune was scoped to `recovering_bank()`, so it silently erased the *other*
bank's own history too — not only a torn tail in the current bank — which only `begin_erase`
may do, and which let a live write land right back in a bank a crash had just left with no
legal append point
([ADR 0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)'s rule,
reachable again one transition later). Discarding only `Absent` records rather than pruning to
`recover()`'s answer avoids that: recovery still answers correctly, because it already scopes
and stops at the right place, and `whole_before` still refuses a write past a torn *media*
record until an erase — not a reboot — clears it.

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

**`Guard::NeverEraseTheAuthority` now protects the pre-seal bank too.** Codex's review of the
pull request found a fourth gap: `authoritative()` is always empty before the first seal, so
the guard's original check — `self.authoritative().contains(&bank)` — protected nothing
pre-seal. `BeginErase(A)` was legal on a fresh device even though `A` is where `declare`
puts every record, so `BeginErase(A)` → `Declare` → `Program` → `CommitErase(A)` left a
record behind that a real erase should have destroyed — `CommitErase` never touches
`records` — and sealing `A` afterward recovered those bytes as current history.
`Journal::protects_current_run(bank)` replaces the direct `authoritative()` check inside
`begin_erase`: post-seal it is unchanged, and pre-seal it is `bank == current_bank()`, the
implicit bank a fresh device writes into. `tests/machine.rs`'s
`erasing_the_pre_seal_current_bank_is_refused_the_same_as_erasing_the_authority` proves the
refusal over every pre-seal reachable state, and
`a_record_programmed_while_the_pre_seal_current_bank_erases_can_never_be_sealed_in` drives
the exact sequence Codex named end to end and shows the first step alone is now enough to
block it.

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

The reachable state space at `Bound::PROOF` grew from 2,576 states, and every transition's
edge count moved with it (`tests/census.rs`, whose pinned numbers are the number to read —
this paragraph is not). It shrank once `protects_current_run` closed the pre-seal gap: a
whole family of states in which a fresh device erased its only writable bank stopped being
reachable. It shrank a third time once `begin_seal` refused a bank that still held records
unless that bank was the one currently being written to: a whole family of states in which a
superseded run's leftover, un-erased bytes got resealed at a higher generation than the bank
that retired them stopped being reachable. `tests/compaction.rs`'s surviving-bank witness
search had to look for a bank with no records rather than trusting its `Erased` tag, because
the tag-only search had been finding this bug's own witness and calling it a demonstration.
Two spine claims that used to be statements about `state.records()`
as one sequence —
`a_torn_record_is_always_the_last_one_on_media_in_its_own_bank` and
`an_acknowledged_record_is_never_behind_a_gap_in_its_own_bank`
— are now per-bank claims, because two banks can each independently hold one torn record at
once without either interruption being impossible. The record-history-shapes census is
likewise now a per-bank claim rather than a claim about the concatenation of both banks'
records.

[ADR 0041](0041-the-bank-refinement-abstracts-a-real-swap.md) closes the other half of
`single-authority`'s gap: issue #73's `tests/refinement.rs` now drives a real writer across
`waymaker_flash::bank`'s swap and checks it against this machine's reachable set, so a
reconstructed state is no longer limited to the record dimension alone. Between the two
issues, `obligation.rs`'s `single-authority` row now says nothing is owed.

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

Codex asked for the same guard again on review of this pull request, from a different
angle: a dispatch happening *after* a bank's retirement, it argued, is not merely an old
run's forfeited effect (issue #95's accepted cost) but a physical effect with no run behind
it at all, since the real swap's consumed writer makes it impossible — and durable_intent's
unconditional exemption cannot tell the two apart. Tried again, directly: restricting
`Dispatch` to `current_bank()` moves `tests/census.rs`'s `TRANSITION_EDGES` (fewer legal
`Dispatch` edges) and moves `REACHABLE_STATES` **not at all** — confirmed by rerunning the
census with the restriction in place. Every state reachable by dispatching after retirement
is also reachable by dispatching while the bank is still current and retiring it afterward,
which `tests/necessity.rs`'s
`a_dispatch_from_a_bank_a_swap_later_retires_can_happen_before_the_swap_ever_starts`
constructs by hand. A `Journal` is a snapshot rather than a log, so "dispatched before
retirement" and "dispatched after" are the same state once retirement has happened; the
guard Codex asked for removes one of two (already redundant) paths to that one state, and the
distinguishing Codex wants does not survive being asked of a state rather than of a trace.
Kept out a second time, for the same reason and now with the state count checked rather than
argued.

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
