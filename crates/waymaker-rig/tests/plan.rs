//! The cut plan: randomised where issue #27 asks for randomised, reproducible where it asks
//! for reproducible.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for a rig that "cuts
//! supply at randomised points", and separately that "any recovery violation is reproducible
//! from the rig's log". Those pull in opposite directions unless the randomness is a pure
//! function of something the log can carry, which is what this file holds the plan to.

use waymaker_rig::census::Coverage;
use waymaker_rig::phase::{Phase, ResetCause};
use waymaker_rig::plan::{Cut, Plan, SplitMix64};

#[test]
fn the_generator_matches_the_reference_vectors() {
    // Pinned against the published SplitMix64 reference for seed zero. The plan is a pure
    // function of the seed, so a generator that drifted would make every log line in every
    // previous run name a cut point that no longer exists.
    let mut rng = SplitMix64::new(0);
    assert_eq!(rng.next(), 0xE220_A839_7B1D_CDAF);
    assert_eq!(rng.next(), 0x6E78_9E6A_A1B9_65F4);
    assert_eq!(rng.next(), 0x06C4_5D18_8009_454F);
    assert_eq!(rng.next(), 0xF88B_B8A8_724C_81EC);
}

#[test]
fn the_indexed_generator_is_the_iterated_one() {
    // `Cut::for_iteration` indexes rather than iterates, so the two have to be the same
    // sequence or a resumed rig arms a cut point no log line names.
    let mut iterated = SplitMix64::new(0xABCD_1234);
    let indexed = SplitMix64::new(0xABCD_1234);
    for index in 0..64 {
        assert_eq!(iterated.next(), indexed.at(index), "at index {index}");
    }
}

#[test]
fn the_same_seed_and_iteration_always_give_the_same_cut() {
    let plan = Plan::new(0xDEAD_BEEF_0BAD_F00D);
    for iteration in 0..256 {
        assert_eq!(plan.cut(iteration), plan.cut(iteration));
        assert_eq!(plan.cut(iteration), Plan::new(plan.seed()).cut(iteration));
    }
}

#[test]
fn an_iteration_is_not_reached_by_replaying_the_ones_before_it() {
    // The rig is resumed after a reset, so iteration 900 must be computable without having
    // run iterations 0..900. A generator whose state carried forward would make a log line
    // meaningless on its own.
    let plan = Plan::new(7);
    for iteration in 0..64 {
        let _unused = plan.cut(iteration);
    }
    assert_eq!(plan.cut(900), Plan::new(7).cut(900));
}

#[test]
fn different_seeds_disagree() {
    let left = Plan::new(1);
    let right = Plan::new(2);
    let differing = (0..64).filter(|i| left.cut(*i) != right.cut(*i)).count();
    assert!(
        differing > 32,
        "two seeds agreed on {} of 64 iterations",
        64 - differing
    );
}

#[test]
fn a_plan_covers_all_six_cells_within_a_bounded_number_of_iterations() {
    // The census fails closed, so a plan that could not reach a cell would make every run
    // report a gap for ever. This is the property that makes the census satisfiable.
    for seed in 0..32_u64 {
        let plan = Plan::new(seed);
        let mut coverage = Coverage::EMPTY;
        let mut iteration = 0_u32;
        while coverage.verdict().is_err() && iteration < 256 {
            let cut = plan.cut(iteration);
            coverage = coverage.record(cut.phase(), cut.cause());
            iteration += 1;
        }
        coverage
            .verdict()
            .unwrap_or_else(|gap| panic!("seed {seed} left {gap} after 256 iterations"));
    }
}

#[test]
fn no_cell_is_starved_over_a_long_run() {
    // "Randomised" that always picks the same cell is a run with one observation in it.
    let plan = Plan::new(0x5EED);
    let mut coverage = Coverage::EMPTY;
    for iteration in 0..6_000 {
        let cut = plan.cut(iteration);
        coverage = coverage.record(cut.phase(), cut.cause());
    }
    for phase in Phase::ALL {
        for cause in ResetCause::ALL {
            let count = coverage.iterations(phase, cause);
            assert!(
                count > 600,
                "{} x {} got {count} of 6000 iterations",
                phase.name(),
                cause.name()
            );
        }
    }
    assert_eq!(coverage.total(), 6_000);
}

#[test]
fn a_tear_offset_is_strictly_inside_the_write() {
    // A cut at byte zero is "before the write" and a cut at the length is "after it". Both
    // are real worlds, and neither is a tear; `injections` in `waymaker-fault` draws the same
    // line, and a plan that returned them would be double-counting crash points it does not
    // own.
    let plan = Plan::new(11);
    for iteration in 0..2_000 {
        let cut = plan.cut(iteration);
        for len in [2_u32, 3, 4, 8, 24, 64, 4096] {
            let Some(offset) = cut.tear_offset(len) else {
                panic!("a write of {len} bytes has an interior");
            };
            assert!(offset > 0 && offset < len, "{offset} is not inside {len}");
        }
    }
}

#[test]
fn a_write_with_no_interior_has_no_tear_offset() {
    let cut = Plan::new(3).cut(0);
    assert_eq!(cut.tear_offset(0), None);
    assert_eq!(cut.tear_offset(1), None);
}

#[test]
fn a_tear_offset_reaches_both_ends_of_a_write() {
    // Every byte of a program is a crash point design document §15 asks for. A plan whose
    // offsets clustered in the middle would leave the first and last bytes — the two most
    // interesting — unvisited.
    let plan = Plan::new(0x00C0_FFEE);
    let mut first = false;
    let mut last = false;
    for iteration in 0..4_000 {
        match plan.cut(iteration).tear_offset(24) {
            Some(1) => first = true,
            Some(23) => last = true,
            _ => {}
        }
    }
    assert!(first, "no iteration tore at the first byte");
    assert!(last, "no iteration tore at the last byte");
}

#[test]
fn an_effect_index_is_always_one_the_run_has() {
    let plan = Plan::new(42);
    for iteration in 0..2_000 {
        let cut = plan.cut(iteration);
        for effects in 1..8_u16 {
            let index = cut.effect_index(effects);
            assert!(index < effects, "{index} is not an effect of {effects}");
        }
        assert_eq!(cut.effect_index(0), 0);
    }
}

#[test]
fn a_cut_is_reconstructed_from_the_seed_and_iteration_alone() {
    // This is issue #27's third "done when" in miniature: the two numbers a log line carries
    // are enough to name the cut point again.
    let plan = Plan::new(0x1234_5678_9ABC_DEF0);
    let cut = plan.cut(4_242);
    let reconstructed = Cut::for_iteration(0x1234_5678_9ABC_DEF0, 4_242);
    assert_eq!(cut, reconstructed);
}
