//! The three write points, the two reset causes, and the census over them.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) names three points at which
//! the supply is cut — "schedule, dispatch, and completion writes" — and asks separately for
//! "watchdog-reset tests at the same three points", with the reason stated: "a watchdog reset
//! is not identical to a brownout and both must be covered". Six cells, and a run that missed
//! one is not a run that passed.

use waymaker_rig::census::Coverage;
use waymaker_rig::phase::{Phase, ResetCause};

#[test]
fn the_three_write_points_are_the_ones_the_issue_names() {
    assert_eq!(
        Phase::ALL,
        [Phase::Schedule, Phase::Dispatch, Phase::Completion]
    );
    assert_eq!(Phase::Schedule.name(), "schedule");
    assert_eq!(Phase::Dispatch.name(), "dispatch");
    assert_eq!(Phase::Completion.name(), "completion");
}

#[test]
fn the_two_reset_causes_are_distinct_and_both_are_named() {
    assert_eq!(
        ResetCause::ALL,
        [ResetCause::PowerCut, ResetCause::Watchdog]
    );
    assert_ne!(ResetCause::PowerCut, ResetCause::Watchdog);
    assert_eq!(ResetCause::PowerCut.name(), "power-cut");
    assert_eq!(ResetCause::Watchdog.name(), "watchdog");
}

#[test]
fn a_phase_and_a_cause_round_trip_through_their_indices() {
    for phase in Phase::ALL {
        assert_eq!(Phase::from_index(phase.index()), Some(phase));
    }
    for cause in ResetCause::ALL {
        assert_eq!(ResetCause::from_index(cause.index()), Some(cause));
    }
    assert_eq!(Phase::from_index(Phase::ALL.len()), None);
    assert_eq!(ResetCause::from_index(ResetCause::ALL.len()), None);
}

#[test]
fn an_index_is_the_position_in_the_all_array() {
    // The census indexes its cells by these, so an index that did not agree with `ALL`
    // would credit one cell for another's coverage and the gap would go unreported.
    for (position, phase) in Phase::ALL.into_iter().enumerate() {
        assert_eq!(phase.index(), position);
    }
    for (position, cause) in ResetCause::ALL.into_iter().enumerate() {
        assert_eq!(cause.index(), position);
    }
}

#[test]
fn an_empty_census_is_a_gap_rather_than_a_pass() {
    let gap = Coverage::EMPTY
        .verdict()
        .expect_err("a census with nothing in it has not covered anything");
    assert_eq!(gap.phase(), Phase::Schedule);
    assert_eq!(gap.cause(), ResetCause::PowerCut);
}

#[test]
fn every_cell_must_be_reached_before_the_census_passes() {
    let mut coverage = Coverage::EMPTY;
    for phase in Phase::ALL {
        for cause in ResetCause::ALL {
            assert!(
                coverage.verdict().is_err(),
                "the census passed before {} x {} was covered",
                phase.name(),
                cause.name()
            );
            coverage = coverage.record(phase, cause);
        }
    }
    coverage.verdict().expect("every cell has been reached");
    assert_eq!(coverage.total(), 6);
}

#[test]
fn covering_one_cell_many_times_never_covers_another() {
    let mut coverage = Coverage::EMPTY;
    for _ in 0..1_000 {
        coverage = coverage.record(Phase::Schedule, ResetCause::PowerCut);
    }
    assert_eq!(
        coverage.iterations(Phase::Schedule, ResetCause::PowerCut),
        1_000
    );
    assert_eq!(
        coverage.iterations(Phase::Dispatch, ResetCause::Watchdog),
        0
    );
    let gap = coverage.verdict().expect_err("five cells are still empty");
    assert_eq!(gap.phase(), Phase::Schedule);
    assert_eq!(gap.cause(), ResetCause::Watchdog);
}

#[test]
fn the_census_reports_the_first_gap_in_a_fixed_order() {
    // A gap that moved around with the iteration order would make two runs of the same rig
    // report different things about the same hole.
    let coverage = Coverage::EMPTY
        .record(Phase::Schedule, ResetCause::PowerCut)
        .record(Phase::Schedule, ResetCause::Watchdog)
        .record(Phase::Dispatch, ResetCause::PowerCut);
    let gap = coverage.verdict().expect_err("three cells are still empty");
    assert_eq!(gap.phase(), Phase::Dispatch);
    assert_eq!(gap.cause(), ResetCause::Watchdog);
}

#[test]
fn a_cell_count_saturates_rather_than_wrapping() {
    // A wrapped counter reads as an uncovered cell, which would turn a very long run into a
    // reported gap it does not have — the census must fail closed on absence, not on length.
    let mut coverage = Coverage::EMPTY;
    for phase in Phase::ALL {
        for cause in ResetCause::ALL {
            coverage = coverage.saturated(phase, cause);
        }
    }
    coverage.verdict().expect("a saturated census is covered");
    let after = coverage.record(Phase::Schedule, ResetCause::PowerCut);
    assert_eq!(
        after.iterations(Phase::Schedule, ResetCause::PowerCut),
        u32::MAX
    );
    after.verdict().expect("saturation is not a gap");
}
