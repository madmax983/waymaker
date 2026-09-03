//! The seeded generator: deterministic, total, and legal by construction.
//!
//! Issue [#19](https://github.com/madmax983/waymaker/issues/19) asks for "random record
//! sequences across random storage geometries". Random here means *drawn*, never
//! *unrepeatable*: a suite whose failures cannot be re-run is a suite nobody can fix, and
//! one that pulls in a third-party generator is a dependency this crate does not have.
//! So [`Rng`] is SplitMix64 in thirty lines, and this file is what holds it to being a
//! generator rather than a source of surprises.

use std::collections::BTreeSet;

use waymaker_fault::{Rng, random_geometry};

#[test]
fn one_seed_is_one_sequence_for_ever() {
    let first: Vec<u64> = (0..64).scan(Rng::new(7), |rng, _| Some(rng.next_u64())).collect();
    let second: Vec<u64> = (0..64).scan(Rng::new(7), |rng, _| Some(rng.next_u64())).collect();
    assert_eq!(first, second, "the same seed produced two different streams");
}

#[test]
fn two_seeds_are_two_sequences() {
    let mut left = Rng::new(1);
    let mut right = Rng::new(2);
    let shared = (0..64)
        .filter(|_| left.next_u64() == right.next_u64())
        .count();
    assert_eq!(shared, 0, "two seeds agreed on {shared} of 64 draws");
}

#[test]
fn below_stays_below_and_reaches_everything_under_it() {
    let mut rng = Rng::new(11);
    let mut seen = BTreeSet::new();
    for _ in 0..4096 {
        let drawn = rng.below(7);
        assert!(drawn < 7, "below(7) drew {drawn}");
        seen.insert(drawn);
    }
    assert_eq!(seen, (0..7).collect::<BTreeSet<u32>>(), "some value never came up");
}

#[test]
fn below_zero_is_zero_rather_than_a_division_by_it() {
    let mut rng = Rng::new(13);
    assert_eq!(rng.below(0), 0);
    assert_eq!(rng.below(1), 0);
}

#[test]
fn a_flip_is_a_coin_and_not_a_constant() {
    let mut rng = Rng::new(17);
    let heads = (0..1024).filter(|_| rng.flip()).count();
    assert!((256..768).contains(&heads), "1024 flips gave {heads} heads");
}

#[test]
fn every_drawn_geometry_is_a_legal_one_the_caller_asked_for() {
    let mut rng = Rng::new(19);
    let mut capacities = BTreeSet::new();
    let mut erases = BTreeSet::new();
    let mut programs = BTreeSet::new();
    for _ in 0..2048 {
        let geometry = random_geometry(&mut rng, 256);
        assert!(geometry.capacity() <= 256, "{geometry:?} is bigger than asked for");
        assert!(geometry.capacity() >= geometry.erase_size());
        assert_eq!(geometry.capacity() % geometry.erase_size(), 0);
        assert!(geometry.erase_size() >= geometry.program_size());
        assert!(geometry.program_size() >= geometry.read_size());
        assert!(geometry.erase_size().is_power_of_two());
        assert!(geometry.program_size().is_power_of_two());
        assert!(geometry.read_size().is_power_of_two());
        capacities.insert(geometry.capacity());
        erases.insert(geometry.erase_size());
        programs.insert(geometry.program_size());
    }
    // A generator that always draws one shape is a constant with a seed in it.
    assert!(capacities.len() > 4, "only {capacities:?}");
    assert!(erases.len() > 2, "only {erases:?}");
    assert!(programs.len() > 1, "only {programs:?}");
}

#[test]
fn a_budget_too_small_for_the_shape_drawn_still_yields_a_legal_device() {
    // The clamp, which is the branch a caller only reaches by asking for something tiny.
    // A generator that answered with an illegal geometry — or with one larger than the
    // budget — would put the property sweep on a device no `Geometry::new` would admit.
    let mut rng = Rng::new(23);
    for budget in [1, 2, 3, 4, 7, 8, 15, 16] {
        for _ in 0..64 {
            let geometry = random_geometry(&mut rng, budget);
            assert!(
                geometry.capacity() <= budget,
                "asked for at most {budget} and got {geometry:?}"
            );
            assert!(geometry.capacity() > 0);
        }
    }
}

#[test]
fn a_budget_of_zero_is_the_smallest_legal_device_rather_than_no_device() {
    // `Geometry::new` refuses a zero capacity, so there is no such thing as an empty
    // device to hand back. One byte is the honest answer, and it is a legal one.
    let mut rng = Rng::new(29);
    let geometry = random_geometry(&mut rng, 0);
    assert_eq!(geometry.capacity(), 1);
    assert_eq!(geometry.erase_size(), 1);
    assert_eq!(geometry.program_size(), 1);
    assert_eq!(geometry.read_size(), 1);
}
