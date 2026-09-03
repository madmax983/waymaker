//! Evidence that the guarantees can be broken by a reader, and are caught when they are.
//!
//! `tests/necessity.rs` shows the *writer's* preconditions are load-bearing. This file shows
//! the same about the *reader*: design document §14's first three guarantees are statements
//! about what recovery produces, and a specification that computed the answer it then
//! checked would prove that a function agrees with itself.
//!
//! Each [`Mutant`] is wrong in exactly one way, and each has to be caught by the guarantee
//! it breaks — not by any guarantee at all. A mutant caught by the wrong line is a mutant
//! whose real failure nothing noticed.

use waymaker_spec::explore::explore;
use waymaker_spec::invariant::Invariant;
use waymaker_spec::model::{Bound, Guard, Guards};
use waymaker_spec::reader::{Mutant, Reader, Specified};

const CEILING: usize = 400_000;

fn enforced() -> waymaker_spec::explore::Explored {
    match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    }
}

/// Which guarantee each wrong reader breaks, and the machine it has to be run against.
///
/// [`Mutant::SkipsGaps`] is the interesting row: under the specified machine it is
/// *indistinguishable* from a correct reader, because [`Guard::AppendOnly`] makes the state
/// it exploits unreachable. It is caught in the machine that guard is removed from, and
/// `a_gap_skipping_reader_is_harmless_only_because_of_the_append_only_precondition` is what
/// says so out loud.
const CAUGHT: [(Mutant, Invariant, Option<Guard>); 5] = [
    (Mutant::ProducesOneMore, Invariant::PrefixSafety, None),
    (Mutant::IncludesTorn, Invariant::PrefixSafety, None),
    (Mutant::Reorders, Invariant::PrefixSafety, None),
    (
        Mutant::DropsTheLast,
        Invariant::AcknowledgedDurability,
        None,
    ),
    (
        Mutant::SkipsGaps,
        Invariant::PrefixSafety,
        Some(Guard::AppendOnly),
    ),
];

#[test]
fn every_wrong_reader_is_caught_by_the_guarantee_it_breaks() {
    for (mutant, invariant, relax) in CAUGHT {
        let guards = relax.map_or(Guards::ENFORCED, |guard| Guards::ENFORCED.without(guard));
        let explored = explore(Bound::PROOF, guards, CEILING)
            .unwrap_or_else(|error| panic!("exploring for {mutant}: {error}"));
        let breach = explored.first_breach_of(invariant, &mutant);
        assert!(
            breach.is_some(),
            "a reader that {mutant} is not caught by {invariant}, so that guarantee cannot \
             tell a correct recovery from this one"
        );
    }
}

#[test]
fn the_mutant_table_covers_every_wrong_reader_the_crate_declares() {
    for mutant in Mutant::ALL {
        assert!(
            CAUGHT.iter().any(|(named, ..)| *named == mutant),
            "{mutant:?} has no row saying which guarantee catches it"
        );
    }
}

#[test]
fn a_wrong_reader_is_caught_by_the_whole_check_too() {
    // The per-guarantee assertions above would still pass if `check` stopped calling one of
    // them. This is the same claim through the front door.
    for (mutant, _, relax) in CAUGHT {
        let guards = relax.map_or(Guards::ENFORCED, |guard| Guards::ENFORCED.without(guard));
        let explored = explore(Bound::PROOF, guards, CEILING)
            .unwrap_or_else(|error| panic!("exploring for {mutant}: {error}"));
        assert!(
            explored.first_breach(&mutant).is_some(),
            "a reader that {mutant} passes the whole check"
        );
    }
}

#[test]
fn a_gap_skipping_reader_is_harmless_only_because_of_the_append_only_precondition() {
    // The result worth writing down: under the specification, a reader that carries on past
    // a record that is not wholly on media produces exactly what a correct reader produces,
    // because no reachable state has anything behind such a gap to find. The guard is what
    // makes a whole class of reader bug unobservable — and the moment it goes, the bug is
    // real again.
    let explored = enforced();
    for state in explored.states() {
        assert_eq!(
            Mutant::SkipsGaps.recover(state),
            Specified.recover(state),
            "a gap-skipping reader differs from a correct one in {state:?}, which \
             Guard::AppendOnly is supposed to make impossible"
        );
    }
    assert!(
        explored.first_breach(&Mutant::SkipsGaps).is_none(),
        "and therefore it breaks nothing here"
    );

    let relaxed = explore(
        Bound::PROOF,
        Guards::ENFORCED.without(Guard::AppendOnly),
        CEILING,
    )
    .expect("the proof bound");
    let breach = relaxed
        .first_breach_of(Invariant::PrefixSafety, &Mutant::SkipsGaps)
        .expect("without the guard, a gap-skipping reader exposes history behind a gap");
    assert!(
        breach.detail.contains("wrong question"),
        "the failure should be the declaration-order clause, not the committed-history one: \
         {breach}"
    );
}

#[test]
fn the_specified_reader_is_the_only_one_that_survives() {
    let explored = enforced();
    assert!(explored.first_breach(&Specified).is_none());
    let survivors = Mutant::ALL
        .into_iter()
        .filter(|mutant| explored.first_breach(mutant).is_none())
        .collect::<Vec<_>>();
    // `SkipsGaps` survives, and the test above is why. Anything else surviving is a hole.
    assert_eq!(
        survivors,
        vec![Mutant::SkipsGaps],
        "these wrong readers pass every guarantee in the specified machine"
    );
}
