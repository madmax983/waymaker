//! The clause table is complete, and every row points at something that exists.
//!
//! [`CLAUSES`] is what says which of design document §14's guarantees is discharged by what.
//! A table like that fails in two directions — a clause with no proof, and a proof whose
//! clause was deleted — and both are silent, so both are checked here. The third direction,
//! drift between this table and the documentation, is `cargo xtask check-layering`'s
//! `recovery-spec` rule.

use std::collections::BTreeSet;
use std::path::Path;

use waymaker_spec::invariant::Invariant;
use waymaker_spec::obligation::{CLAUSES, Discharge, clause};

/// This crate's directory, so that a row naming a test target can be checked against the
/// tree rather than against a reader's memory.
fn crate_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn every_state_level_guarantee_has_a_clause() {
    for invariant in Invariant::ALL {
        assert!(
            clause(invariant.clause()).is_some(),
            "{invariant} has no row in CLAUSES, so nothing says how it is discharged"
        );
    }
}

#[test]
fn every_clause_names_a_proof_and_a_falsifier_that_exist() {
    for entry in CLAUSES {
        for (role, target) in [("proof", entry.proof), ("falsifier", entry.falsifier)] {
            let path = crate_root().join(target);
            assert!(
                path.is_file(),
                "clause `{}` names {role} `{target}`, which is not a file in this crate",
                entry.id
            );
        }
    }
}

#[test]
fn every_clause_id_is_distinct_and_looked_up_by_its_own_id() {
    let ids: BTreeSet<&str> = CLAUSES.iter().map(|entry| entry.id).collect();
    assert_eq!(
        ids.len(),
        CLAUSES.len(),
        "two clauses share an id, so citing one names both"
    );
    for entry in CLAUSES {
        assert_eq!(clause(entry.id), Some(entry));
    }
    assert_eq!(clause("no-such-clause"), None);
}

#[test]
fn every_clause_id_is_a_kebab_case_identifier() {
    // The ids are cited in commit messages, in the ADR and in `CLAUDE.md`, and the gate
    // matches them in backticks. One with a space or a capital in it would be matched by
    // accident or not at all.
    for entry in CLAUSES {
        assert!(
            !entry.id.is_empty()
                && entry
                    .id
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character == '-')
                && !entry.id.starts_with('-')
                && !entry.id.ends_with('-'),
            "`{}` is not a kebab-case id",
            entry.id
        );
    }
}

#[test]
fn every_clause_carries_the_design_documents_own_words() {
    for entry in CLAUSES {
        assert!(
            entry.statement.len() > 20,
            "clause `{}` has no statement worth citing",
            entry.id
        );
        assert!(
            !entry.statement.ends_with('.'),
            "clause `{}`'s statement is a fragment quoted from §14, not a sentence of ours",
            entry.id
        );
    }
}

#[test]
fn the_five_guarantees_the_design_document_lists_all_have_a_clause() {
    // §14's own list, transcribed. A guarantee dropped from CLAUDE.md, the README and this
    // table at once would otherwise leave nothing to notice.
    let expected = [
        "prefix-safety",
        "acknowledged-durability",
        "durable-intent",
        "stable-redelivery",
        "bounded-decoding",
    ];
    for id in expected {
        assert!(
            clause(id).is_some(),
            "design document §14 lists `{id}` and CLAUSES does not"
        );
    }
    // Plus §02 decision 7, which issue #20 asks for alongside them.
    assert!(clause("single-authority").is_some());
    assert_eq!(CLAUSES.len(), expected.len() + 1);
}

#[test]
fn a_clause_discharged_against_the_model_is_falsified_somewhere_other_than_its_own_proof() {
    // A model-level proof and its falsifier in one file is a file that can be deleted whole.
    // The two firmware clauses are exempt on purpose: their falsifier is the same exhaustive
    // sweep — an input the decoder mishandles fails the proof directly, with no second
    // machine to compare against.
    for entry in CLAUSES {
        if entry.discharge == Discharge::Model {
            assert_ne!(
                entry.proof, entry.falsifier,
                "clause `{}` is proved and falsified in one file",
                entry.id
            );
        }
    }
}

#[test]
fn what_is_still_owed_is_written_down_rather_than_left_out() {
    // Two clauses are only partly discharged, and both say so. This asserts the count so
    // that a third gap has to be declared rather than absorbed, and that a gap closed is a
    // row edited rather than a note quietly left behind.
    let owed: Vec<&str> = CLAUSES
        .iter()
        .filter(|entry| entry.owed.is_some())
        .map(|entry| entry.id)
        .collect();
    assert_eq!(owed, vec!["single-authority", "bounded-decoding"]);
    for entry in CLAUSES {
        if let Some(note) = entry.owed {
            assert!(
                note.len() > 40,
                "clause `{}` says something is owed and not what",
                entry.id
            );
        }
    }
}

#[test]
fn every_discharge_kind_is_used_by_a_clause() {
    // A kind of evidence the table declares and no row claims is a category with nothing in
    // it, which is how a label that means nothing gets applied to something later.
    let used: BTreeSet<&str> = CLAUSES
        .iter()
        .map(|entry| entry.discharge.label())
        .collect();
    for discharge in Discharge::ALL {
        assert!(
            used.contains(discharge.label()),
            "no clause is discharged by `{}`",
            discharge.label()
        );
    }
    assert_eq!(used.len(), Discharge::ALL.len());
}

#[test]
fn every_named_proof_contains_tests_rather_than_merely_existing() {
    // A file check is not a proof check: `tests/spine.rs` emptied to zero `#[test]`
    // functions still exists, still compiles, still "passes", and four guarantees have
    // silently lost their evidence. The count is a floor rather than a pin, because a proof
    // gaining a test should not be a build failure.
    for entry in CLAUSES {
        for (role, target) in [("proof", entry.proof), ("falsifier", entry.falsifier)] {
            let path = crate_root().join(target);
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
            let tests = contents.matches("#[test]").count();
            assert!(
                tests >= 2,
                "clause `{}` names {role} `{target}`, which holds {tests} test(s)",
                entry.id
            );
        }
    }
}
