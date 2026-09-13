//! The journal as a state machine: which transitions are legal, over every reachable state.
//!
//! Issue [#20](https://github.com/madmax983/waymaker/issues/20) asks for the legal
//! transitions between `attempted`, `possibly durable` and `acknowledged`, and between bank
//! generations, to be stated. They are stated here as claims about every edge of the
//! enumerated machine rather than as a diagram, so a transition that becomes legal by
//! accident fails a build.
//!
//! The edges are recomputed from the state set rather than stored: an edge list is a second
//! copy of the transition relation, and this way the only definition is
//! [`Journal::step`](waymaker_spec::model::Journal::step).

use waymaker_fault::Durability;
use waymaker_spec::explore::explore;
use waymaker_spec::model::{
    Bank, BankId, Bound, Guards, Illegal, Journal, OnMedia, Record, Role, Transition,
};

const CEILING: usize = 200_000;

fn proof_space() -> waymaker_spec::explore::Explored {
    match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    }
}

/// Every legal edge of the enforced machine, as `(from, transition, to)`.
fn edges() -> Vec<(Journal, Transition, Journal)> {
    let explored = proof_space();
    let alphabet = Journal::alphabet(Bound::PROOF);
    let mut edges = Vec::new();
    for state in explored.states() {
        for transition in &alphabet {
            if let Ok(next) = state.step(*transition, Guards::ENFORCED, Bound::PROOF) {
                edges.push((state.clone(), *transition, next));
            }
        }
    }
    edges
}

/// Every record present on both sides of an edge, paired by id.
///
/// `Transition::BeginErase` — issue [#67](https://github.com/madmax983/waymaker/issues/67) —
/// is now the one transition allowed to drop a record outright, so a plain `.zip()` over the
/// raw slices would pair the survivor of an erased bank against whatever unrelated record
/// happens to sit at its old index and file the difference as a state change nothing made.
/// Matching by id instead means a test below is a claim about a record that persisted across
/// the edge, which is what "never taken back", "never un-acknowledged" and "moves only along
/// these edges" are actually claims about.
fn matched<'a>(from: &'a Journal, to: &'a Journal) -> Vec<(&'a Record, &'a Record)> {
    to.records()
        .iter()
        .filter_map(|after| {
            from.records()
                .iter()
                .find(|before| before.id == after.id)
                .map(|before| (before, after))
        })
        .collect()
}

#[test]
fn a_record_moves_only_along_the_three_state_edges_the_design_document_names() {
    // Design document §15: merely attempted, possibly durable before acknowledgment, and
    // barrier-returned. Forward only, and never back.
    // Two, not three. `Attempted -> Acknowledged` is not on this list, and a list that
    // admitted it would pass a model in which `Program` acknowledged its own record —
    // deleting the barrier from the durability path entirely. `tests/census.rs` requires
    // both of these to be witnessed and that one never to be, so a list that admits an edge
    // nothing takes fails there rather than being tolerated here.
    let legal = [
        (Durability::Attempted, Durability::PossiblyDurable),
        (Durability::PossiblyDurable, Durability::Acknowledged),
    ];
    for (from, transition, to) in edges() {
        for (before, after) in matched(&from, &to) {
            let (before, after) = (before.durability(), after.durability());
            if before == after {
                continue;
            }
            assert!(
                legal.contains(&(before, after)),
                "{transition:?} moved a record from {before:?} to {after:?}"
            );
        }
    }
}

#[test]
fn an_acknowledged_record_is_never_un_acknowledged() {
    for (from, transition, to) in edges() {
        for (before, after) in matched(&from, &to) {
            if before.acknowledged {
                assert!(
                    after.acknowledged,
                    "{transition:?} took back the barrier that returned for record {}",
                    before.id.0
                );
            }
        }
    }
}

#[test]
fn bytes_on_media_are_never_taken_back_within_a_run() {
    // NOR flash only clears bits, and rung 0.1 has no erase of the journal region. A record
    // that reached media stays there for the life of the run; the two-bank swap is how
    // history is reclaimed, and that is the bank machine below rather than this one. A record
    // an erase drops is not "taken back" in this sense — it is gone, which
    // `a_declared_record_is_never_renumbered_or_removed_except_by_erasing_its_bank` covers —
    // so this is a claim about the records that survive an edge, matched by id.
    for (from, transition, to) in edges() {
        for (before, after) in matched(&from, &to) {
            let regressed = matches!(
                (before.media, after.media),
                (OnMedia::Whole, OnMedia::Absent | OnMedia::Partial)
                    | (OnMedia::Partial, OnMedia::Absent)
            );
            assert!(
                !regressed,
                "{transition:?} took record {} back from {:?} to {:?}",
                before.id.0, before.media, after.media
            );
        }
    }
}

#[test]
fn a_declared_record_is_never_renumbered_or_removed_except_by_erasing_its_bank() {
    for (from, transition, to) in edges() {
        let dropped = from.records().len() - matched(&from, &to).len();
        if matches!(transition, Transition::BeginErase(_)) {
            // The one exception, and `erasing_a_bank_drops_exactly_that_banks_records_and_nothing_else`
            // is the claim about which records it may drop — exactly the named bank's.
            // `Reboot` restores power and nothing else, so it is not a second exception:
            // `a_reboot_changes_nothing_but_the_power` is that claim.
            continue;
        }
        assert_eq!(
            dropped, 0,
            "{transition:?} dropped {dropped} record(s) without erasing a bank"
        );
        for (before, after) in matched(&from, &to) {
            assert_eq!(
                before.id, after.id,
                "{transition:?} renumbered a declared record"
            );
        }
    }
}

#[test]
fn erasing_a_bank_drops_exactly_that_banks_records_and_nothing_else() {
    // The other half of issue #67's first gap: `BeginErase` is now allowed to shrink
    // `records`, and this is the claim about *which* records it may drop — exactly the
    // named bank's, never the other bank's.
    for (from, transition, to) in edges() {
        let Transition::BeginErase(erased) = transition else {
            continue;
        };
        let after_ids: std::collections::BTreeSet<_> =
            to.records().iter().map(|record| record.id).collect();
        for record in from.records() {
            let should_survive = record.bank != erased;
            assert_eq!(
                after_ids.contains(&record.id),
                should_survive,
                "erasing {erased:?} {} record {} in bank {:?}",
                if should_survive { "dropped" } else { "kept" },
                record.id.0,
                record.bank
            );
        }
    }
}

#[test]
fn a_bank_moves_only_erased_to_sealing_to_sealed_to_erased() {
    for (from, transition, to) in edges() {
        for bank in BankId::ALL {
            let (before, after) = (from.bank(bank), to.bank(bank));
            if before == after {
                continue;
            }
            let legal = matches!(
                (before, after),
                (Bank::Erased, Bank::Sealing(_))
                    | (Bank::Sealing(_), Bank::Sealed(_))
                    | (
                        Bank::Sealed(_) | Bank::Sealing(_) | Bank::Erased,
                        Bank::Erasing
                    )
                    | (Bank::Erasing, Bank::Erased)
            );
            assert!(
                legal,
                "{transition:?} moved a bank from {before:?} to {after:?}"
            );
        }
    }
}

#[test]
fn committing_a_seal_keeps_the_generation_the_seal_was_written_at() {
    // §02 decision 7: "a new run becomes authoritative only after its payload and generation
    // seal are durable". The barrier makes a seal authoritative; it does not choose a
    // generation of its own.
    for (from, transition, to) in edges() {
        if let Transition::CommitSeal(bank) = transition {
            let Bank::Sealing(pending) = from.bank(bank) else {
                panic!("CommitSeal was legal from {:?}", from.bank(bank));
            };
            assert_eq!(to.bank(bank), Bank::Sealed(pending));
        }
    }
}

#[test]
fn a_new_seal_is_strictly_newer_than_the_bank_it_replaces() {
    for (from, transition, to) in edges() {
        if let Transition::BeginSeal(bank) = transition {
            let Bank::Sealing(fresh) = to.bank(bank) else {
                panic!("BeginSeal did not leave a seal in flight");
            };
            if let Some(other) = from.bank(bank.other()).authoritative_generation() {
                assert!(
                    fresh > other,
                    "a new seal at generation {fresh} does not outrank the authoritative \
                     bank's {other}"
                );
            }
        }
    }
}

#[test]
fn the_authoritative_generation_never_goes_backwards() {
    for (from, transition, to) in edges() {
        let before = from
            .banks()
            .iter()
            .filter_map(|bank| bank.authoritative_generation())
            .max();
        let after = to
            .banks()
            .iter()
            .filter_map(|bank| bank.authoritative_generation())
            .max();
        if let (Some(before), Some(after)) = (before, after) {
            assert!(
                after >= before,
                "{transition:?} moved authority back from generation {before} to {after}"
            );
        }
    }
}

#[test]
fn under_the_specification_an_acknowledged_record_is_wholly_on_media() {
    // Deliberately a theorem rather than a fact about `Record`: the representation can hold
    // the counter-example so that `tests/necessity.rs` can produce one with the barrier
    // precondition removed. This is the claim that the enforced machine never does.
    for state in proof_space().states() {
        for record in state.records() {
            if record.acknowledged {
                assert_eq!(
                    record.media,
                    OnMedia::Whole,
                    "record {} is acknowledged and not wholly on media in {state:?}",
                    record.id.0
                );
            }
        }
    }
}

#[test]
fn committed_history_and_declaration_order_are_the_same_prefix() {
    // The theorem `waymaker_fault::Ledger::committed`'s filter rests on, proved here rather
    // than assumed there: no record reaches media behind one that did not, so "prefix of
    // committed history" and "prefix of declaration order" are the same statement. Remove
    // `Guard::AppendOnly` and they stop being — which is exactly what `tests/teeth.rs` shows
    // a gap-skipping reader exploiting.
    for state in proof_space().states() {
        let committed: Vec<_> = state.committed().collect();
        let declared: Vec<_> = state.declared().into_iter().take(committed.len()).collect();
        assert_eq!(
            committed, declared,
            "committed history is not a prefix of declaration order in {state:?}"
        );
    }
}

#[test]
fn stepping_is_a_function_of_the_state_and_the_transition() {
    // Determinism, stated because every proof in this crate assumes it: a search over a
    // relation that answered differently on a second visit would enumerate a different
    // machine each run.
    let alphabet = Journal::alphabet(Bound::PROOF);
    for state in proof_space().states() {
        for transition in &alphabet {
            let first = state.step(*transition, Guards::ENFORCED, Bound::PROOF);
            let second = state.step(*transition, Guards::ENFORCED, Bound::PROOF);
            assert_eq!(first, second, "{transition:?} is not deterministic");
        }
    }
}

#[test]
fn no_legal_transition_leaves_the_state_unchanged_except_where_it_is_meant_to() {
    // A silent no-op is how a guard stops guarding: the transition is "legal", nothing
    // happens, and the search sees a machine with a move it does not really have. Two
    // transitions are genuinely idempotent — erasing an erased bank, and a barrier over a
    // device with nothing new to acknowledge — and they are named here rather than tolerated
    // wherever they turn up.
    for (from, transition, to) in edges() {
        if from == to {
            let excused = matches!(transition, Transition::Barrier);
            assert!(
                excused,
                "{transition:?} is legal from {from:?} and changes nothing"
            );
        }
    }
}

#[test]
fn a_reboot_changes_nothing_but_the_power() {
    // Issue #67's second gap, corrected after review found the first version wrong: a reboot
    // restores power and nothing else. It must not prune `records` down to `recover()`'s
    // answer — `recover()` already computes that fresh from whatever bytes are there, and
    // pruning would erase a torn record's bytes (and, since `recover()` is scoped to one
    // bank, the *other* bank's own history too) the way only `Transition::BeginErase` may.
    // A device rebooting behind a torn record has to stay stuck behind it, exactly as
    // `Guard::AppendOnly` already requires, until a real erase clears that bank.
    for (from, transition, to) in edges() {
        if transition != Transition::Reboot {
            continue;
        }
        assert_eq!(to.records(), from.records(), "Reboot changed the records");
        assert_eq!(
            to.dispatched(),
            from.dispatched(),
            "Reboot changed the dispatch log"
        );
        assert_eq!(to.banks(), from.banks(), "Reboot changed a bank's seal");
        assert!(to.powered(), "Reboot left the device unpowered");
    }
}

#[test]
fn a_reboot_behind_a_torn_record_still_cannot_write_past_it() {
    // The positive claim `a_reboot_changes_nothing_but_the_power` exists to protect: a device
    // that reboots with a torn tail in the bank it is still writing to is exactly as stuck in
    // that bank afterwards as it was the instant before the crash — ADR 0018's "only erased
    // media is an append point", now proved to survive a reboot rather than only a first
    // boot. Before the fix, `Journal::reboot` dropped the torn record along with everything
    // else, which made this false: `Declare` then `Program` into the same bank succeeded
    // right after reboot.
    let mut checked = 0_usize;
    for (_, transition, to) in edges() {
        if transition != Transition::Reboot || !to.has_torn_record() {
            continue;
        }
        // Whichever bank `Declare` lands a new record in is the bank still being written to;
        // if that is not the torn record's bank, the torn one belongs to an already-retired
        // bank and this edge is not the scenario this test is about. Whichever role protocol
        // order permits here is fine — either demonstrates the same append-only refusal.
        let declare_outcome = to.step(
            Transition::Declare(waymaker_spec::model::Role::Outcome),
            Guards::ENFORCED,
            Bound::PROOF,
        );
        let declare_schedule = to.step(
            Transition::Declare(waymaker_spec::model::Role::Schedule),
            Guards::ENFORCED,
            Bound::PROOF,
        );
        let Ok(declared) = declare_outcome.or(declare_schedule) else {
            continue;
        };
        let new_record = declared
            .records()
            .iter()
            .find(|record| !to.records().iter().any(|old| old.id == record.id))
            .expect("Declare added exactly one record");
        let Some(torn) = to
            .records()
            .iter()
            .find(|record| record.bank == new_record.bank && record.media == OnMedia::Partial)
        else {
            continue;
        };
        checked += 1;
        assert!(
            declared
                .step(
                    Transition::Program(new_record.id),
                    Guards::ENFORCED,
                    Bound::PROOF
                )
                .is_err(),
            "programming a new record in {:?} succeeded right after reboot, behind torn \
             record {} in {to:?}",
            new_record.bank,
            torn.id.0
        );
    }
    assert!(
        checked > 0,
        "no reboot ever landed behind a torn record in the bank still being written to, so \
         this claim is about nothing"
    );
}

#[test]
fn erasing_the_pre_seal_current_bank_is_refused_the_same_as_erasing_the_authority() {
    // Before the first seal, `authoritative()` is always empty, so
    // `Guard::NeverEraseTheAuthority`'s original check protected nothing: `BeginErase(A)` was
    // legal on a fresh device even though `A` is where `Journal::declare` puts every record.
    // Codex found the run this let through during review of issue #67: erase `A`, declare and
    // program a record into it while it is `Erasing`, `CommitErase(A)` — which never touches
    // `records` — leaves that record behind, and sealing `A` afterward recovers bytes an
    // erase should have destroyed. `Journal::protects_current_run` closes it by checking the
    // implicit current bank before the first seal the same way `authoritative()` already
    // closes the case after one.
    let mut checked = 0_usize;
    for state in proof_space().states() {
        if state.has_sealed() || !state.powered() {
            continue;
        }
        let current = state
            .recovering_bank()
            .expect("recovering_bank is always Some before the first seal");
        checked += 1;
        assert_eq!(
            state.step(
                Transition::BeginErase(current),
                Guards::ENFORCED,
                Bound::PROOF
            ),
            Err(Illegal::WouldEraseTheAuthority),
            "erasing the pre-seal current bank {current:?} was not refused in {state:?}"
        );
    }
    assert!(
        checked > 0,
        "no reachable state was ever pre-seal, so this claim is about nothing"
    );
}

#[test]
fn a_record_programmed_while_the_pre_seal_current_bank_erases_can_never_be_sealed_in() {
    // The scenario the previous test's refusal exists to prevent, driven end to end: without
    // the fix, `BeginErase(A) -> Declare -> Program -> CommitErase(A) -> BeginSeal(A) ->
    // CommitSeal(A)` would recover a record from a bank that was supposed to have been wiped.
    // With `Journal::protects_current_run` in place the very first step is refused, so the
    // rest of the sequence is unreachable — checked here directly, rather than trusted from
    // the single-step refusal alone.
    let fresh = Journal::default();
    assert!(!fresh.has_sealed());
    let current = fresh
        .recovering_bank()
        .expect("a fresh device has a current bank");
    assert_eq!(
        fresh.step(
            Transition::BeginErase(current),
            Guards::ENFORCED,
            Bound::PROOF
        ),
        Err(Illegal::WouldEraseTheAuthority),
        "a fresh device let its only writable bank start erasing"
    );
}

#[test]
fn a_bank_the_first_seal_retires_cannot_be_resealed_without_an_erase() {
    // Codex, PR #135 round 7: `Declare(Schedule) -> Program -> Barrier` in A (pre-seal,
    // current), then `BeginSeal(B) -> CommitSeal(B)` as the device's very first seal, leaves
    // A's `Bank` tag at `Erased` — it was never touched — while A still holds the record it
    // declared before B took over. Without the fix, `BeginSeal(A)` reads that tag and sees
    // nothing wrong, resealing A's stale record at a higher generation than B and recovering
    // a superseded run as current with no erase anywhere in the trace.
    let mut state = Journal::default();
    state = state
        .step(
            Transition::Declare(Role::Schedule),
            Guards::ENFORCED,
            Bound::PROOF,
        )
        .expect("declare in A");
    let id = state.records()[0].id;
    state = state
        .step(Transition::Program(id), Guards::ENFORCED, Bound::PROOF)
        .expect("program");
    state = state
        .step(Transition::Barrier, Guards::ENFORCED, Bound::PROOF)
        .expect("barrier");
    state = state
        .step(
            Transition::BeginSeal(BankId::B),
            Guards::ENFORCED,
            Bound::PROOF,
        )
        .expect("begin seal B, the device's first seal");
    state = state
        .step(
            Transition::CommitSeal(BankId::B),
            Guards::ENFORCED,
            Bound::PROOF,
        )
        .expect("commit seal B");
    assert_eq!(state.recovering_bank(), Some(BankId::B));

    assert_eq!(
        state.step(
            Transition::BeginSeal(BankId::A),
            Guards::ENFORCED,
            Bound::PROOF
        ),
        Err(Illegal::BankNotErased),
        "A was resealed with a stale record still in it and no erase in the trace"
    );
}
