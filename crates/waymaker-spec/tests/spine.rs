//! The spine proofs: design document §14's state-level guarantees, over every reachable
//! state.
//!
//! Issue [#20](https://github.com/madmax983/waymaker/issues/20) asks for these four to be
//! proved rather than sampled. The search is closed — breadth-first until no transition
//! produces a state it has not already seen — so within
//! [`Bound::PROOF`](waymaker_spec::model::Bound::PROOF) "for every possible crash point" is a
//! finished enumeration.
//!
//! What each of these tests is *not* is evidence that it could have failed.
//! `tests/necessity.rs` and `tests/teeth.rs` are that, and `tests/census.rs` is the evidence
//! that the enumeration did not quietly shrink to nothing.

use waymaker_spec::explore::explore;
use waymaker_spec::invariant::Invariant;
use waymaker_spec::model::{Bound, Guards, Journal, OnMedia};
use waymaker_spec::reader::{Reader, Specified};

/// The ceiling every proof in this crate runs under. Reaching it is an error, not a stop.
const CEILING: usize = 200_000;

fn proof_space() -> waymaker_spec::explore::Explored {
    match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    }
}

#[test]
fn no_reachable_state_falsifies_any_guarantee() {
    let explored = proof_space();
    if let Some(breach) = explored.first_breach(&Specified) {
        panic!("{breach}");
    }
}

#[test]
fn recovery_exposes_only_a_legal_prefix_of_committed_records() {
    let explored = proof_space();
    if let Some(breach) = explored.first_breach_of(Invariant::PrefixSafety, &Specified) {
        panic!("{breach}");
    }
}

#[test]
fn a_record_acknowledged_after_its_barrier_is_recovered_after_reset() {
    let explored = proof_space();
    if let Some(breach) = explored.first_breach_of(Invariant::AcknowledgedDurability, &Specified) {
        panic!("{breach}");
    }
}

#[test]
fn no_dispatched_effect_lacks_a_recoverable_schedule_record() {
    let explored = proof_space();
    if let Some(breach) = explored.first_breach_of(Invariant::DurableIntent, &Specified) {
        panic!("{breach}");
    }
}

#[test]
fn exactly_one_bank_is_authoritative_after_any_crash() {
    let explored = proof_space();
    if let Some(breach) = explored.first_breach_of(Invariant::SingleAuthority, &Specified) {
        panic!("{breach}");
    }
}

#[test]
fn every_legal_recovery_the_design_document_permits_satisfies_every_guarantee() {
    // §15 permits recovery to include an unacknowledged complete record *or not*, so the
    // specification is a relation and the proof has to range over all of it. A proof that
    // only checked the longest answer would leave a reader that stops one record early — a
    // legal reader — unproved.
    let explored = proof_space();
    for state in explored.states() {
        for recovery in state.legal_recoveries() {
            if let Err(breach) = waymaker_spec::invariant::check(state, &recovery) {
                panic!("{breach}");
            }
        }
    }
}

#[test]
fn a_legal_recovery_is_a_prefix_that_holds_every_acknowledged_record() {
    // `legal_recoveries` states §15's rule as membership, and this is what stops it drifting
    // back into a length comparison. The two agree under the specified machine because
    // acknowledged records form a prefix — a theorem `tests/machine.rs` proves — and the
    // point of stating it as membership is that the definition does not *depend* on the
    // theorem. Checked over the relaxed machine as well, which is where a length would give
    // the wrong answer.
    for guards in [
        Guards::ENFORCED,
        Guards::ENFORCED.without(waymaker_spec::model::Guard::AppendOnly),
        Guards::ENFORCED.without(waymaker_spec::model::Guard::BarrierNeedsWhole),
    ] {
        let explored = match explore(Bound::PROOF, guards, CEILING) {
            Ok(explored) => explored,
            Err(error) => unreachable!("{error}"),
        };
        for state in explored.states() {
            let full = state.recover();
            for recovery in state.legal_recoveries() {
                assert!(
                    full.starts_with(&recovery),
                    "{recovery:?} is not a prefix of {full:?} in {state:?}"
                );
                for required in state.acknowledged() {
                    assert!(
                        recovery.contains(&required),
                        "{recovery:?} is called legal in {state:?} and loses acknowledged \
                         record {}",
                        required.0
                    );
                }
            }
        }
    }
}

#[test]
fn a_torn_record_is_always_the_last_one_on_media() {
    // Not one of §14's five, and that is why it is here: it is the reachability invariant
    // the proof of acknowledged durability *rests on*. If a torn record could sit behind a
    // whole one, a prefix-honest reader would stop before records a barrier had already
    // acknowledged. Proving the guarantee without proving this would be proving the theorem
    // and assuming the lemma.
    let explored = proof_space();
    for state in explored.states() {
        let torn: Vec<usize> = state
            .records()
            .iter()
            .enumerate()
            .filter(|(_, record)| record.media == OnMedia::Partial)
            .map(|(index, _)| index)
            .collect();
        assert!(
            torn.len() <= 1,
            "two records are torn at once in {state:?}, which no single interruption could do"
        );
        if let Some(position) = torn.first() {
            for later in state.records().iter().skip(position + 1) {
                assert_eq!(
                    later.media,
                    OnMedia::Absent,
                    "record {} reached media behind a torn one in {state:?}",
                    later.id.0
                );
            }
        }
    }
}

#[test]
fn an_acknowledged_record_is_never_behind_a_gap() {
    // The other lemma acknowledged durability rests on, stated as its own claim so that a
    // change which breaks it fails here rather than three inferences away.
    let explored = proof_space();
    for state in explored.states() {
        let mut seen_gap = false;
        for record in state.records() {
            if record.media != OnMedia::Whole {
                seen_gap = true;
            } else if seen_gap {
                assert!(
                    !record.acknowledged,
                    "record {} is acknowledged behind a gap in {state:?}",
                    record.id.0
                );
            }
        }
    }
}

#[test]
fn the_power_going_away_is_the_end_of_the_run() {
    let explored = proof_space();
    let alphabet = Journal::alphabet(Bound::PROOF);
    for state in explored.states().iter().filter(|state| !state.powered()) {
        for transition in &alphabet {
            assert!(
                state
                    .step(*transition, Guards::ENFORCED, Bound::PROOF)
                    .is_err(),
                "{transition:?} is legal after the power went away in {state:?}"
            );
        }
    }
}

#[test]
fn the_specified_reader_and_the_model_agree_on_what_recovery_is() {
    // `Specified` delegates to `Journal::recover`, and this is the assertion that keeps the
    // delegation from being replaced by a second definition.
    let explored = proof_space();
    for state in explored.states() {
        assert_eq!(Specified.recover(state), state.recover());
    }
}
