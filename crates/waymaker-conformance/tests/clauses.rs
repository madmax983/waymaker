//! The clause table and the case table, held to each other.
//!
//! Design document §12's contract is stated in four places — `xtask::docs`, `CLAUDE.md`,
//! ADR 0016 and [`waymaker_conformance::clause::CLAUSES`] — and `cargo xtask
//! check-layering`'s `storage-conformance` rule is what stops those four from drifting.
//! What that rule cannot see is inside this crate: that every clause the table calls
//! in-process is actually reached by a case, and that no case cites a clause that is not
//! there. That is this file.

use waymaker_conformance::case::{CASE_COUNT, CASES, CaseId};
use waymaker_conformance::clause::{CLAUSES, Discharge, clause};

#[test]
fn every_case_cites_a_clause_the_table_declares() {
    for case in CASES {
        let cited = clause(case.clause);
        assert!(
            cited.is_some(),
            "case {:?} cites the clause {}, which is not in CLAUSES",
            case.id,
            case.clause
        );
    }
}

#[test]
fn every_case_cites_a_clause_the_in_process_suite_is_what_discharges() {
    // A case citing `interruptible-mutations` would be a case claiming to observe a crash
    // from inside one process. The table says which clauses this suite can speak for, and
    // this is what stops a case from speaking for one it cannot.
    for case in CASES {
        let Some(cited) = clause(case.clause) else {
            unreachable!("every_case_cites_a_clause_the_table_declares covers this");
        };
        assert_eq!(
            cited.discharge,
            Discharge::InProcess,
            "case {:?} cites {}, which is discharged by {}",
            case.id,
            cited.id,
            cited.discharge.message()
        );
    }
}

#[test]
fn every_in_process_clause_is_reached_by_at_least_one_case() {
    for spec in CLAUSES {
        if spec.discharge != Discharge::InProcess {
            continue;
        }
        assert!(
            CASES.iter().any(|case| case.clause == spec.id),
            "clause {} says the in-process suite discharges it, and no case cites it",
            spec.id
        );
    }
}

#[test]
fn the_case_table_and_the_case_ids_agree() {
    // `Report` is a fixed array indexed by `CaseId::index`, so a `CASES` row out of order
    // would silently report one case's outcome under another case's name.
    for (index, case) in CASES.iter().enumerate() {
        assert_eq!(case.id.index(), index, "{:?} is out of order", case.id);
        assert_eq!(case.id.spec().map(|spec| spec.name), Some(case.name));
    }
    assert_eq!(CASE_COUNT, CASES.len());
}

#[test]
fn clause_ids_and_case_names_are_unique() {
    for (index, spec) in CLAUSES.iter().enumerate() {
        assert!(
            !CLAUSES
                .iter()
                .skip(index + 1)
                .any(|other| other.id == spec.id),
            "clause id {} appears twice",
            spec.id
        );
    }
    for (index, case) in CASES.iter().enumerate() {
        assert!(
            !CASES
                .iter()
                .skip(index + 1)
                .any(|other| other.name == case.name),
            "case name {} appears twice",
            case.name
        );
    }
}

#[test]
fn the_two_barrier_clauses_are_the_ones_the_across_reset_witness_owns() {
    // Issue #21 states five contract sentences; two of them are about surviving a reset,
    // and a suite running in one process cannot observe one. That is the whole reason
    // `durability` exists as a separate two-phase API, so the table saying so is checked
    // rather than assumed.
    let across: Vec<&str> = CLAUSES
        .iter()
        .filter(|spec| spec.discharge == Discharge::AcrossReset)
        .map(|spec| spec.id)
        .collect();
    assert_eq!(
        across,
        ["barrier-is-durable", "barrier-orders-what-follows"]
    );
}

#[test]
fn a_clause_nothing_declares_is_not_found() {
    assert!(clause("no-such-clause").is_none());
}

#[test]
fn every_case_id_names_itself() {
    assert_eq!(
        CaseId::GeometryIsStable.name(),
        "geometry is stable",
        "the first case should name itself through the table"
    );
}
