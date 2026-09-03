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
use waymaker_spec::model::{Bank, BankId, Bound, Guards, Journal, OnMedia, Transition};

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
        for (before, after) in from.records().iter().zip(to.records()) {
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
        for (before, after) in from.records().iter().zip(to.records()) {
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
    // history is reclaimed, and that is the bank machine below rather than this one.
    for (from, transition, to) in edges() {
        for (before, after) in from.records().iter().zip(to.records()) {
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
fn a_declared_record_is_never_renumbered_or_removed() {
    for (from, transition, to) in edges() {
        assert!(
            to.records().len() >= from.records().len(),
            "{transition:?} dropped a record"
        );
        for (before, after) in from.records().iter().zip(to.records()) {
            assert_eq!(
                before.id, after.id,
                "{transition:?} renumbered a declared record"
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
        let declared: Vec<_> = state
            .records()
            .iter()
            .take(committed.len())
            .map(|record| record.id)
            .collect();
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
