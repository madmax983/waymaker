//! Every precondition is load-bearing, shown by deleting it.
//!
//! `tests/spine.rs` proves that no reachable state falsifies design document §14's
//! guarantees. That is only worth as much as the machine it is proved over: a precondition
//! that could be removed with every spine proof still green was never doing any work, and
//! the specification would be claiming design content it does not have.
//!
//! So each of [`Guard`]'s five is removed on its own, and the search is required to *find* a
//! counterexample — to the specific guarantee that guard exists to hold, not to any
//! guarantee at all. A guard whose removal breaks the wrong thing is a guard aimed at the
//! wrong place.
//!
//! This is also where the design smell issue [#20](https://github.com/madmax983/waymaker/issues/20)
//! asks about was found. Acknowledged durability does not hold if a record's bytes may be
//! written while an earlier declared record's are not: `Declare, Declare, Program(1),
//! Barrier` acknowledges the second record behind an absent first one, and every
//! prefix-honest reader loses it. The representation was refined — [`Guard::AppendOnly`] —
//! rather than the proof worked around.

use waymaker_spec::explore::explore;
use waymaker_spec::invariant::Invariant;
use waymaker_spec::model::{Bound, Guard, Guards, Journal};
use waymaker_spec::reader::Specified;

const CEILING: usize = 400_000;

/// Which guarantee each precondition is holding up.
const NECESSITY: [(Guard, Invariant); 6] = [
    (Guard::AppendOnly, Invariant::AcknowledgedDurability),
    (Guard::BarrierNeedsWhole, Invariant::AcknowledgedDurability),
    (Guard::DurableIntent, Invariant::DurableIntent),
    (Guard::DispatchNeedsASchedule, Invariant::DurableIntent),
    (Guard::NeverEraseTheAuthority, Invariant::SingleAuthority),
    (Guard::StrictGeneration, Invariant::SingleAuthority),
];

#[test]
fn every_precondition_holds_up_the_guarantee_it_is_there_for() {
    for (guard, invariant) in NECESSITY {
        let relaxed = explore(Bound::PROOF, Guards::ENFORCED.without(guard), CEILING)
            .unwrap_or_else(|error| panic!("exploring without {guard:?}: {error}"));
        let breach = relaxed.first_breach_of(invariant, &Specified);
        assert!(
            breach.is_some(),
            "removing {guard:?} leaves {invariant} holding in every reachable state, so the \
             precondition is not what makes that guarantee true"
        );
    }
}

#[test]
fn the_guard_table_covers_every_precondition_the_model_declares() {
    // A guard added to `Guard::ALL` and not to `NECESSITY` would go unproved, and the test
    // above would still pass.
    for guard in Guard::ALL {
        assert!(
            NECESSITY.iter().any(|(named, _)| *named == guard),
            "{guard:?} has no necessity proof, so nothing says what it is for"
        );
    }
}

#[test]
fn the_enforced_machine_is_the_only_one_with_no_counterexample() {
    let enforced = explore(Bound::PROOF, Guards::ENFORCED, CEILING).expect("the proof bound");
    assert!(
        enforced.first_breach(&Specified).is_none(),
        "the enforced machine has a counterexample, which the spine proofs should have caught"
    );
    for guard in Guard::ALL {
        let relaxed = explore(Bound::PROOF, Guards::ENFORCED.without(guard), CEILING)
            .unwrap_or_else(|error| panic!("exploring without {guard:?}: {error}"));
        assert!(
            relaxed.first_breach(&Specified).is_some(),
            "removing {guard:?} breaks nothing at all"
        );
    }
}

#[test]
fn removing_the_append_only_precondition_acknowledges_a_record_behind_a_gap() {
    // The counterexample named, rather than merely found, so that a change which happens to
    // break some other guarantee cannot pass this file off as still meaningful.
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::AppendOnly),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::AcknowledgedDurability, &Specified)
        .expect("a record acknowledged behind a gap");
    assert!(
        breach.detail.contains("acknowledged and recovery lost it"),
        "{breach}"
    );
}

#[test]
fn removing_the_barrier_precondition_acknowledges_a_torn_record() {
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::BarrierNeedsWhole),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::AcknowledgedDurability, &Specified)
        .expect("a torn record claimed by a barrier");
    assert!(
        breach.detail.contains("acknowledged and recovery lost it"),
        "{breach}"
    );
}

#[test]
fn removing_the_durable_intent_precondition_dispatches_an_effect_with_no_record() {
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::DurableIntent),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::DurableIntent, &Specified)
        .expect("an effect dispatched before its intent was durable");
    assert!(breach.detail.contains("dispatched"), "{breach}");
}

#[test]
fn removing_the_schedule_precondition_accounts_for_an_effect_with_a_completion() {
    // Codex, PR #66 round 1. `Dispatch` used to accept any acknowledged record, and the
    // model had no record roles, so an effect could be accounted for by an acknowledged
    // *completion* — a record written after the world was changed, standing in for the one
    // that ordered it. The guarantee then read "some record about this effect is
    // recoverable", which is not §02 decision 3. This is the counterexample the role
    // distinction rules out, produced on demand.
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::DispatchNeedsASchedule),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::DurableIntent, &Specified)
        .expect("an effect accounted for by a record that schedules nothing");
    assert!(breach.detail.contains("schedules nothing"), "{breach}");
}

#[test]
fn removing_the_authority_precondition_leaves_a_device_with_nothing_to_boot_from() {
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::NeverEraseTheAuthority),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::SingleAuthority, &Specified)
        .expect("a device that erased its only authority");
    assert!(breach.detail.contains("none to boot from"), "{breach}");
}

#[test]
fn removing_the_authority_precondition_also_hands_back_an_older_run() {
    // §14's failure table on the two-bank swap: "never recover the old run as current". A
    // guard that only forbade stranding the device would still permit erasing the *newer* of
    // two sealed banks, which reverts authority to the older generation — a state that has
    // exactly one authoritative bank and is still wrong. This is the assertion that keeps
    // the guard at the stronger reading.
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::NeverEraseTheAuthority),
        CEILING,
    )
    .expect("the proof bound");
    let alphabet = Journal::alphabet(Bound::PROOF);
    let regressed = relaxed.states().iter().any(|state| {
        let before = highest_generation(state);
        alphabet.iter().any(|transition| {
            state
                .step(
                    *transition,
                    Guards::ENFORCED.without(Guard::NeverEraseTheAuthority),
                    Bound::PROOF,
                )
                .is_ok_and(|next| matches!((before, highest_generation(&next)), (Some(was), Some(now)) if now < was))
        })
    });
    assert!(
        regressed,
        "without the guard, authority never falls back to an older generation, so the guard \
         is not what forbids it"
    );

    let enforced = explore(Bound::PROOF, Guards::ENFORCED, CEILING).expect("the proof bound");
    for state in enforced.states() {
        let before = highest_generation(state);
        for transition in &alphabet {
            if let Ok(next) = state.step(*transition, Guards::ENFORCED, Bound::PROOF) {
                if let (Some(was), Some(now)) = (before, highest_generation(&next)) {
                    assert!(
                        now >= was,
                        "{transition:?} handed back generation {now} from {was}"
                    );
                }
            }
        }
    }
}

/// The generation a reader would boot at, or `None` for a device with nothing to boot from.
fn highest_generation(state: &Journal) -> Option<u32> {
    state
        .banks()
        .iter()
        .filter_map(|bank| bank.authoritative_generation())
        .max()
}

#[test]
fn removing_the_generation_precondition_leaves_two_banks_claiming_the_run() {
    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::StrictGeneration),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::SingleAuthority, &Specified)
        .expect("two banks sealed at one generation");
    assert!(
        breach.detail.contains("banks are authoritative"),
        "{breach}"
    );
}
