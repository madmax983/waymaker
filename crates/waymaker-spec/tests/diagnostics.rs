//! The things a failing proof prints, and the accessors a reader of one needs.
//!
//! A counterexample is only useful if it says what it is. Every [`Illegal`] reason, every
//! [`Mutant`], every [`Invariant`] and every [`ExploreError`] renders into the message a
//! contributor will be reading at the moment they least want to guess, so each is checked
//! here rather than left to be discovered wrong.
//!
//! The census accessors are here for the same reason: `tests/census.rs` asks them specific
//! questions, and a reader debugging a thinned-out search needs the whole list.

use std::collections::BTreeSet;

use waymaker_fault::Durability;
use waymaker_spec::explore::{BankShape, ExploreError, TransitionKind, explore};
use waymaker_spec::invariant::{Invariant, holds};
use waymaker_spec::model::{
    Bank, BankId, Bound, Guard, Guards, Illegal, Journal, OnMedia, Record, Transition,
};
use waymaker_spec::obligation::Discharge;
use waymaker_spec::reader::{Mutant, Reader, Specified};
use waymaker_spec::refine::Impossible;

const CEILING: usize = 200_000;

#[test]
fn every_refusal_reason_says_something_different() {
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
    let messages: BTreeSet<&str> = reasons.iter().map(|reason| reason.message()).collect();
    assert_eq!(
        messages.len(),
        reasons.len(),
        "two refusal reasons print the same line, so a failure names the wrong precondition"
    );
    for reason in reasons {
        assert_eq!(reason.to_string(), reason.message());
        assert!(!reason.message().is_empty());
    }
}

#[test]
fn every_wrong_reader_says_how_it_is_wrong() {
    let messages: BTreeSet<&str> = Mutant::ALL.iter().map(|mutant| mutant.message()).collect();
    assert_eq!(messages.len(), Mutant::ALL.len());
    for mutant in Mutant::ALL {
        assert_eq!(mutant.to_string(), mutant.message());
    }
}

#[test]
fn every_guarantee_prints_the_clause_id_it_is_cited_by() {
    let clauses: BTreeSet<&str> = Invariant::ALL
        .iter()
        .map(|invariant| invariant.clause())
        .collect();
    assert_eq!(clauses.len(), Invariant::ALL.len());
    for invariant in Invariant::ALL {
        assert_eq!(invariant.to_string(), invariant.clause());
    }
}

#[test]
fn a_breach_names_the_state_and_the_history_that_produced_it() {
    // A counterexample that said only "prefix-safety failed" would leave a contributor to
    // re-derive the trace by hand.
    let explored = match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    };
    let breach = explored
        .first_breach_of(Invariant::PrefixSafety, &Mutant::ProducesOneMore)
        .expect("a reader that overshoots is caught");
    let rendered = breach.to_string();
    assert!(rendered.starts_with("prefix-safety:"), "{rendered}");
    assert!(rendered.contains("recovered ["), "{rendered}");
    assert!(rendered.contains("Journal"), "{rendered}");
    assert_eq!(breach.invariant, Invariant::PrefixSafety);
    assert_eq!(
        breach.recovered,
        Mutant::ProducesOneMore.recover(&breach.state)
    );
}

#[test]
fn a_ceiling_error_says_which_ceiling_it_reached() {
    let error = explore(Bound::PROOF, Guards::ENFORCED, 1).expect_err("one state is not enough");
    assert_eq!(error, ExploreError::CeilingReached { ceiling: 1 });
    assert!(error.to_string().contains('1'), "{error}");
}

#[test]
fn an_impossible_observation_says_which_record_claimed_both() {
    for impossible in [
        Impossible::TornAndAcknowledged {
            record: waymaker_fault::RecordId(3),
        },
        Impossible::TornAndAbsent {
            record: waymaker_fault::RecordId(4),
        },
    ] {
        assert!(!impossible.to_string().is_empty());
    }
}

#[test]
fn the_census_lists_every_edge_it_counted() {
    let explored = match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    };
    let census = explored.census();
    let durability = census.durability_edges();
    assert!(!durability.is_empty());
    for edge in &durability {
        assert!(census.durability_steps(edge.0, edge.1) > 0);
    }
    let banks = census.bank_edges();
    assert!(!banks.is_empty());
    for edge in &banks {
        assert!(census.bank_steps(edge.0, edge.1) > 0);
    }
    // An edge nothing walked reads zero rather than being absent, so a caller asking about
    // one it expected gets an answer rather than a lookup failure.
    assert_eq!(
        census.durability_steps(Durability::Acknowledged, Durability::Attempted),
        0
    );
    assert_eq!(census.bank_steps(BankShape::Sealed, BankShape::Sealed), 0);
    assert_eq!(explored.guards(), Guards::ENFORCED);
    assert_eq!(explored.bound(), Bound::PROOF);
}

#[test]
fn the_guards_bitmask_answers_for_every_precondition() {
    assert_eq!(Guards::default(), Guards::ENFORCED);
    for guard in Guard::ALL {
        assert!(Guards::ENFORCED.enforces(guard));
        let relaxed = Guards::ENFORCED.without(guard);
        assert!(!relaxed.enforces(guard));
        // Removing one leaves the others alone, which is what makes a necessity proof a
        // proof about that guard rather than about the four it took with it.
        for other in Guard::ALL {
            if other != guard {
                assert!(relaxed.enforces(other), "{guard:?} took {other:?} with it");
            }
        }
    }
}

#[test]
fn a_record_reports_the_design_documents_state_for_every_shape_it_can_hold() {
    let record = |media, acknowledged| Record {
        id: waymaker_fault::RecordId(0),
        media,
        acknowledged,
    };
    assert_eq!(
        record(OnMedia::Absent, false).durability(),
        Durability::Attempted
    );
    assert_eq!(
        record(OnMedia::Partial, false).durability(),
        Durability::PossiblyDurable
    );
    assert_eq!(
        record(OnMedia::Whole, false).durability(),
        Durability::PossiblyDurable
    );
    assert_eq!(
        record(OnMedia::Whole, true).durability(),
        Durability::Acknowledged
    );
    // The shape only a machine without `Guard::BarrierNeedsWhole` can reach, kept
    // representable so that `tests/necessity.rs` can produce the counterexample.
    assert_eq!(
        record(OnMedia::Partial, true).durability(),
        Durability::Acknowledged
    );
    assert!(record(OnMedia::Whole, false).is_recoverable());
    assert!(!record(OnMedia::Partial, true).is_recoverable());
    assert!(!record(OnMedia::Absent, false).is_recoverable());
}

#[test]
fn a_bank_reports_a_generation_only_once_its_seal_is_durable() {
    assert_eq!(Bank::Erased.authoritative_generation(), None);
    assert_eq!(Bank::Sealing(3).authoritative_generation(), None);
    assert_eq!(Bank::Sealed(3).authoritative_generation(), Some(3));
    for shape in BankShape::ALL {
        assert!(matches!(
            shape,
            BankShape::Erased | BankShape::Sealing | BankShape::Sealed
        ));
    }
    assert_eq!(BankShape::of(Bank::Sealing(1)), BankShape::Sealing);
    for bank in BankId::ALL {
        assert_eq!(bank.other().other(), bank);
        assert_ne!(bank.other(), bank);
    }
}

#[test]
fn a_fresh_device_has_nothing_recovered_dispatched_or_authoritative() {
    let fresh = Journal::new();
    assert_eq!(fresh, Journal::default());
    assert!(fresh.powered());
    assert!(fresh.records().is_empty());
    assert!(fresh.dispatched().is_empty());
    assert!(fresh.recover().is_empty());
    assert!(fresh.authoritative().is_empty());
    assert!(!fresh.has_sealed());
    assert!(!fresh.has_torn_record());
    assert_eq!(fresh.banks(), &[Bank::Erased, Bank::Erased]);
    assert_eq!(fresh.bank(BankId::A), Bank::Erased);
    assert_eq!(fresh.ledger().len(), 0);
    assert_eq!(fresh.committed().count(), 0);
    assert_eq!(fresh.acknowledged().count(), 0);
    // The empty history is the only legal recovery from a device that has written nothing.
    assert_eq!(fresh.legal_recoveries().len(), 1);
    assert!(holds(Invariant::SingleAuthority, &fresh, &[]).is_ok());
    assert!(Specified.recover(&fresh).is_empty());
}

#[test]
fn the_alphabet_covers_every_transition_kind_at_every_record_the_bound_allows() {
    let alphabet = Journal::alphabet(Bound::PROOF);
    for kind in TransitionKind::ALL {
        assert!(
            alphabet.iter().any(|transition| transition.kind() == kind),
            "{kind:?} is not in the alphabet, so no state can ever take it"
        );
    }
    for index in 0..Bound::PROOF.records {
        let id = waymaker_fault::RecordId(u32::try_from(index).expect("a small index"));
        assert!(alphabet.contains(&Transition::Program(id)));
        assert!(alphabet.contains(&Transition::FailedProgram(id)));
        assert!(alphabet.contains(&Transition::Dispatch(id)));
    }
}

#[test]
fn the_discharge_kinds_each_have_a_word() {
    let labels: BTreeSet<&str> = [
        Discharge::Model,
        Discharge::Firmware,
        Discharge::Representation,
    ]
    .iter()
    .map(|discharge| discharge.label())
    .collect();
    assert_eq!(labels.len(), 3);
}
