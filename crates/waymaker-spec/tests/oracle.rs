//! The specification and design document §15's oracle decide the same thing.
//!
//! Two independent judgements of one run: [`waymaker_spec::model::Journal::legal_recoveries`]
//! says declaratively which histories §15 permits, and
//! [`waymaker_fault::verify_oracle`] decides operationally whether one is permitted. Neither
//! is derived from the other, and this file requires them to agree in both directions over
//! every reachable state and every candidate history — not only over the ones a correct
//! reader would produce.
//!
//! Both directions matter for different reasons. Soundness — every permitted history is
//! accepted — is what stops the oracle failing a correct implementation. Completeness —
//! every other history is refused — is what stops the crash suite passing a broken one, and
//! it is the direction a test suite normally never checks.

use std::collections::BTreeSet;

use waymaker_fault::{RecordId, Recovery, verify_oracle, verify_recovery};
use waymaker_spec::explore::explore;
use waymaker_spec::invariant;
use waymaker_spec::model::{Bound, Guard, Guards, Journal};
use waymaker_spec::reader::Reader;
use waymaker_spec::reader::Specified;

const CEILING: usize = 200_000;

/// The bound the agreement sweep runs at: the same one the spine proofs use.
///
/// The sweep is the state space times every history over the record universe, which is the
/// one claim in this crate whose cost grows fastest with the bound. It is kept at
/// [`Bound::PROOF`] anyway, so that "the oracle and the specification agree" is a claim over
/// the same machine every other proof here is about rather than over a smaller one nobody
/// would notice had shrunk.
const AGREEMENT: Bound = Bound::PROOF;

/// Every history a reader could conceivably return, within the bound.
///
/// Includes an id no run ever declared, so that "recovery invented a record" is a case the
/// sweep really contains rather than one it assumes cannot happen.
fn candidates(bound: Bound) -> Vec<Vec<RecordId>> {
    let mut universe: Vec<RecordId> = (0..bound.records)
        .map(|index| RecordId(u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    universe.push(RecordId(99));

    let mut all = vec![Vec::new()];
    let mut frontier = vec![Vec::new()];
    for _ in 0..=bound.records {
        let mut next = Vec::new();
        for prefix in &frontier {
            for id in &universe {
                let mut longer = prefix.clone();
                longer.push(*id);
                next.push(longer);
            }
        }
        all.extend(next.iter().cloned());
        frontier = next;
    }
    all
}

#[test]
fn the_oracle_accepts_exactly_the_recoveries_the_specification_permits() {
    let explored = explore(AGREEMENT, Guards::ENFORCED, CEILING).expect("the agreement bound");
    let candidates = candidates(AGREEMENT);
    let mut accepted = 0_usize;
    let mut refused = 0_usize;

    for state in explored.states() {
        let ledger = state.ledger();
        let permitted: BTreeSet<Vec<RecordId>> = state.legal_recoveries();
        for candidate in &candidates {
            let verdict = verify_recovery(&ledger, candidate);
            if permitted.contains(candidate) {
                accepted += 1;
                assert!(
                    verdict.is_ok(),
                    "the specification permits {candidate:?} in {state:?} and the oracle \
                     refused it: {verdict:?}"
                );
            } else {
                refused += 1;
                assert!(
                    verdict.is_err(),
                    "the specification forbids {candidate:?} in {state:?} and the oracle \
                     accepted it"
                );
            }
        }
    }

    // A sweep that judged nothing either way would pass both assertions above.
    assert!(
        accepted > 0 && refused > 0,
        "{accepted} accepted, {refused} refused"
    );
}

#[test]
fn the_oracles_third_line_agrees_with_the_model_about_dispatched_effects() {
    let explored = explore(AGREEMENT, Guards::ENFORCED, CEILING).expect("the agreement bound");
    let mut with_effects = 0_usize;
    for state in explored.states() {
        if state.dispatched().is_empty() {
            continue;
        }
        with_effects += 1;
        let ledger = state.ledger();
        let history = Specified.recover(state);
        let recovery = Recovery::new(&history).dispatched(state.dispatched());
        assert!(
            verify_oracle(&ledger, &recovery).is_ok(),
            "the oracle refuses the specified reader's history in {state:?}"
        );
    }
    assert!(with_effects > 0, "no state dispatched an effect");
}

#[test]
fn the_oracles_fourth_line_agrees_with_the_model_about_bank_authority() {
    let explored = explore(AGREEMENT, Guards::ENFORCED, CEILING).expect("the agreement bound");
    let mut with_banks = 0_usize;
    for state in explored.states().iter().filter(|state| state.has_sealed()) {
        with_banks += 1;
        let ledger = state.ledger();
        let history = Specified.recover(state);
        let recovery = Recovery::new(&history).authoritative_banks(state.authoritative().len());
        assert!(
            verify_oracle(&ledger, &recovery).is_ok(),
            "the oracle refuses the bank count the model reports in {state:?}"
        );
    }
    assert!(with_banks > 0, "no state ever sealed a bank");
}

#[test]
fn the_oracle_refuses_the_bank_counts_the_model_never_reports() {
    // The other direction for the fourth line: the model's claim is that a sealed device has
    // exactly one authoritative bank, and the oracle has to be the thing that would notice
    // if it did not.
    let explored = explore(AGREEMENT, Guards::ENFORCED, CEILING).expect("the agreement bound");
    let state = explored
        .states()
        .iter()
        .find(|state| state.has_sealed())
        .expect("a state that sealed a bank");
    let ledger = state.ledger();
    let history = Specified.recover(state);
    for count in [0_usize, 2, 3] {
        let recovery = Recovery::new(&history).authoritative_banks(count);
        assert!(
            verify_oracle(&ledger, &recovery).is_err(),
            "the oracle accepted {count} authoritative banks"
        );
    }
}

#[test]
fn the_specification_is_strictly_stronger_than_the_oracle_where_they_differ() {
    // The agreement above holds over the specified machine, and this is the claim about
    // where it stops. Relax the append-only precondition and the two part company in one
    // direction only: the model's prefix-safety is a prefix of *declaration order*, and
    // `waymaker_fault::verify_oracle` compares against committed history, which filters out
    // records that never reached media. So the oracle accepts a history that skips a gap and
    // carries on, and the model refuses it.
    //
    // Written down and measured rather than left implied, because the argument for the
    // oracle's filter is otherwise circular: the filter is sound *because* no reachable
    // state has a gap, which is a theorem about the machine the filter is being used to
    // check. Naming the difference — and asserting it never runs the other way — is what
    // turns that into a stated limit.
    let relaxed = explore(
        AGREEMENT,
        Guards::ENFORCED.without(Guard::AppendOnly),
        CEILING,
    )
    .expect("the agreement bound");
    let candidates = candidates(AGREEMENT);
    let mut oracle_is_weaker = 0_usize;

    for state in relaxed.states() {
        let ledger = state.ledger();
        for candidate in &candidates {
            let oracle_accepts = verify_recovery(&ledger, candidate).is_ok();
            let model_accepts = invariant::check(state, candidate).is_ok();
            if oracle_accepts && !model_accepts {
                oracle_is_weaker += 1;
                continue;
            }
            assert!(
                !model_accepts || oracle_accepts,
                "the model accepts {candidate:?} in {state:?} and the oracle refuses it; the \
                 specification is supposed to be the stricter of the two"
            );
        }
    }
    assert!(
        oracle_is_weaker > 0,
        "the two never differ even with the append-only precondition removed, so this claim \
         is about nothing"
    );
}

#[test]
fn the_candidate_sweep_contains_the_histories_it_is_supposed_to() {
    // The sweep is only exhaustive over what it generates, so what it generates is asserted
    // rather than described.
    let candidates = candidates(AGREEMENT);
    assert!(candidates.contains(&Vec::new()), "the empty history");
    assert!(
        candidates.contains(&vec![RecordId(99)]),
        "a record no run declared"
    );
    assert!(
        candidates.contains(&vec![RecordId(1), RecordId(0)]),
        "history in the wrong order"
    );
    assert!(
        candidates.contains(&vec![RecordId(0), RecordId(0)]),
        "one record twice"
    );
    assert!(
        candidates
            .iter()
            .any(|history| history.len() > AGREEMENT.records),
        "a history longer than any run could have declared"
    );
    // Four ids — the three a run can declare, plus one it never can — at every length from
    // nothing up to one more record than any run has: 1 + 4 + 16 + 64 + 256.
    assert_eq!(candidates.len(), 341);
}

#[test]
fn the_agreement_bound_reaches_the_states_that_make_the_sweep_worth_running() {
    let explored = explore(AGREEMENT, Guards::ENFORCED, CEILING).expect("the agreement bound");
    assert!(
        explored
            .states()
            .iter()
            .any(|state| state.acknowledged().count() > 0),
        "no state has an acknowledged record, so the sweep never tests losing one"
    );
    assert!(
        explored.states().iter().any(Journal::has_torn_record),
        "no state has a torn record, so the sweep never tests recovering half of one"
    );
    assert!(
        explored.states().iter().any(|state| {
            state
                .records()
                .iter()
                .any(|record| record.media == waymaker_spec::model::OnMedia::Absent)
        }),
        "no state has an unwritten record, so the sweep never tests inventing one"
    );
}
