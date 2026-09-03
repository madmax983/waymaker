//! Evidence that the enumeration is not thin.
//!
//! Every claim in `tests/spine.rs` has the shape "no reachable state falsifies P", and every
//! claim of that shape passes trivially when the reachable set is small. A guard tightened
//! by accident, a transition that stopped being generated, a bound quietly reduced — each
//! leaves every spine proof green and the proof about a machine nobody meant to specify.
//!
//! So the search is counted. Every transition kind must have been followed, every refusal
//! reason must have been reached, every §15 record-state edge and every bank-shape edge must
//! have been walked, and the state count itself is pinned. Issue
//! [#19](https://github.com/madmax983/waymaker/issues/19) established this pattern for the
//! crash sweep; this is the same idea one level up.

use std::collections::BTreeSet;

use waymaker_fault::Durability;
use waymaker_spec::explore::{BankShape, TransitionKind, explore};
use waymaker_spec::model::{Bound, Guards, Illegal, OnMedia};

const CEILING: usize = 200_000;

fn proof_space() -> waymaker_spec::explore::Explored {
    match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    }
}

/// How many states the proof bound reaches.
///
/// Pinned rather than described. A change that makes this number *smaller* is the dangerous
/// one — it means part of the machine stopped being reachable while every proof about the
/// rest kept passing — and a change that makes it larger is one a reviewer should see too.
/// Either way the number is the review, and it is expected to move when the model does.
const REACHABLE_STATES: usize = 1_972;

#[test]
fn the_state_space_is_the_size_it_was_when_these_proofs_were_written() {
    let explored = proof_space();
    assert_eq!(
        explored.states().len(),
        REACHABLE_STATES,
        "the reachable state space changed size; if that was on purpose, update \
         REACHABLE_STATES in the same commit as the model change"
    );
}

#[test]
fn every_transition_kind_was_followed_at_least_once() {
    let explored = proof_space();
    for kind in TransitionKind::ALL {
        assert!(
            explored.census().transitions(kind) > 0,
            "{kind:?} is in the alphabet and no reachable state admits it, so every proof \
             about it is vacuous"
        );
    }
}

#[test]
fn every_precondition_refused_something() {
    let explored = proof_space();
    // Every reason `Illegal` can give, listed here rather than derived, so that a new one
    // has to be either witnessed or deliberately excused.
    let reasons = [
        Illegal::PowerIsGone,
        Illegal::NoOpenRecord,
        Illegal::EarlierRecordIncomplete,
        Illegal::RecordAlreadyWritten,
        Illegal::SealAlreadyInFlight,
        Illegal::CapacityReached,
        Illegal::UndeclaredRecord,
        Illegal::IntentNotDurable,
        Illegal::AlreadyDispatched,
        Illegal::BankNotErased,
        Illegal::BankNotSealing,
        Illegal::WouldEraseTheAuthority,
        Illegal::GenerationExhausted,
    ];
    for reason in reasons {
        assert!(
            explored.census().refusals(reason) > 0,
            "no reachable state was ever refused for `{reason}`, so that precondition \
             guards nothing the search can see"
        );
    }
}

#[test]
fn every_record_state_edge_the_design_document_names_was_walked() {
    let explored = proof_space();
    // Design document §15's three record states, and the moves between them a run can make.
    // `Attempted -> Attempted` is a record arriving; the other three are the transitions
    // §15 describes. `Acknowledged -> anything` is deliberately absent: a barrier that
    // returned cannot be taken back, and `tests/machine.rs` proves that rather than
    // assuming it.
    let required = [
        (Durability::Attempted, Durability::Attempted),
        (Durability::Attempted, Durability::PossiblyDurable),
        (Durability::PossiblyDurable, Durability::Acknowledged),
    ];
    for (from, to) in required {
        assert!(
            explored.census().durability_steps(from, to) > 0,
            "no run ever moved a record from {from:?} to {to:?}"
        );
    }
}

#[test]
fn every_bank_shape_edge_the_two_bank_swap_needs_was_walked() {
    let explored = proof_space();
    let required = [
        (BankShape::Erased, BankShape::Sealing),
        (BankShape::Sealing, BankShape::Sealed),
        (BankShape::Sealed, BankShape::Erased),
    ];
    for (from, to) in required {
        assert!(
            explored.census().bank_steps(from, to) > 0,
            "no run ever moved a bank from {from:?} to {to:?}, so §02 decision 7's swap is \
             not being explored"
        );
    }
}

#[test]
fn the_search_reaches_every_shape_of_record_history_the_model_can_hold() {
    let explored = proof_space();
    let mut shapes: BTreeSet<Vec<OnMedia>> = BTreeSet::new();
    for state in explored.states() {
        shapes.insert(state.records().iter().map(|record| record.media).collect());
    }
    // Every whole-prefix-then-gap shape up to the bound, including the torn tail. Written
    // out rather than generated, because a generator that agreed with the model would agree
    // with a wrong model too.
    // Whole records first, then at most one torn, then records that never reached media:
    // `Whole^w Partial^p Absent^a` with `p <= 1` and `w + p + a <= 3`. Written out rather
    // than generated from that rule, because a generator sharing the model's reasoning
    // would agree with a wrong model too.
    let expected: BTreeSet<Vec<OnMedia>> = [
        vec![],
        vec![OnMedia::Whole],
        vec![OnMedia::Partial],
        vec![OnMedia::Absent],
        vec![OnMedia::Whole, OnMedia::Whole],
        vec![OnMedia::Whole, OnMedia::Partial],
        vec![OnMedia::Whole, OnMedia::Absent],
        vec![OnMedia::Partial, OnMedia::Absent],
        vec![OnMedia::Absent, OnMedia::Absent],
        vec![OnMedia::Whole, OnMedia::Whole, OnMedia::Whole],
        vec![OnMedia::Whole, OnMedia::Whole, OnMedia::Partial],
        vec![OnMedia::Whole, OnMedia::Whole, OnMedia::Absent],
        vec![OnMedia::Whole, OnMedia::Partial, OnMedia::Absent],
        vec![OnMedia::Whole, OnMedia::Absent, OnMedia::Absent],
        vec![OnMedia::Partial, OnMedia::Absent, OnMedia::Absent],
        vec![OnMedia::Absent, OnMedia::Absent, OnMedia::Absent],
    ]
    .into_iter()
    .collect();
    assert_eq!(
        shapes, expected,
        "the shapes of history the search reaches are not the ones the model is supposed to \
         admit"
    );
}

#[test]
fn the_search_reaches_states_with_effects_actually_dispatched() {
    let explored = proof_space();
    let dispatching = explored
        .states()
        .iter()
        .filter(|state| !state.dispatched().is_empty())
        .count();
    assert!(
        dispatching > 0,
        "no reachable state ever dispatched an effect, so durable intent is proved about \
         nothing"
    );
    let two = explored
        .states()
        .iter()
        .filter(|state| state.dispatched().len() >= 2)
        .count();
    assert!(
        two > 0,
        "no reachable state ever dispatched two effects, so the proof never sees a second one"
    );
}

#[test]
fn the_search_reaches_a_device_that_swapped_banks_at_least_twice() {
    let explored = proof_space();
    let deep = explored
        .states()
        .iter()
        .filter(|state| {
            state
                .banks()
                .iter()
                .filter_map(|bank| bank.authoritative_generation())
                .any(|generation| generation >= 2)
        })
        .count();
    assert!(
        deep > 0,
        "no reachable state ever sealed a second generation, so the swap is proved about \
         one bank"
    );
}

#[test]
fn the_search_fails_rather_than_truncating_when_the_ceiling_is_too_low() {
    // The other half of "a measurement that did not happen is not a measurement that
    // passed": a ceiling that silently stopped the search would report a proof about the
    // states it happened to reach first.
    let error = explore(Bound::PROOF, Guards::ENFORCED, 10).expect_err("10 states is not enough");
    assert!(error.to_string().contains("ceiling"), "{error}");
}
