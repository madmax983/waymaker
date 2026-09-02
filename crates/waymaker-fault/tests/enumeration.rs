//! Crash points are enumerated, not sampled.
//!
//! Issue [#18](https://github.com/madmax983/waymaker/issues/18): "the harness can enumerate
//! every crash point in a given write sequence, not sample it randomly". These tests are
//! what that sentence means when a build can fail over it — the enumeration is a pure
//! function of the write sequence and the geometry, and its output is asserted element by
//! element for a sequence small enough to write down.

use std::collections::BTreeSet;

use waymaker_fault::{Effect, Injection, Op, Progress, injections};
use waymaker_flash::storage::Geometry;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(64, 32, 4, 1) else {
        unreachable!("64 is two 32-byte blocks of eight 4-byte units of single bytes")
    };
    geometry
}

#[test]
fn a_four_byte_program_tears_at_every_byte_inside_it() {
    let ops = [Op::Program { offset: 0, len: 4 }];
    let points = injections(&ops, geometry());

    let power = |progress| Injection {
        op: 0,
        progress,
        effect: Effect::PowerLoss,
    };
    let failure = |progress| Injection {
        op: 0,
        progress,
        effect: Effect::Failure,
    };

    assert_eq!(
        points,
        vec![
            // Power loss: before anything, then after each byte, then after the whole op.
            Injection {
                op: 0,
                progress: Progress::None,
                effect: Effect::PowerLoss
            },
            power(Progress::Bytes(1)),
            power(Progress::Bytes(2)),
            power(Progress::Bytes(3)),
            power(Progress::Whole),
            // Failure: the call returns an error having done nothing, part of it, or all
            // of it. The last is not a contradiction — an operation whose status read
            // fails after the media changed is a real device.
            failure(Progress::None),
            failure(Progress::Bytes(1)),
            failure(Progress::Bytes(2)),
            failure(Progress::Bytes(3)),
            failure(Progress::Whole),
        ]
    );
}

#[test]
fn every_program_unit_boundary_is_among_the_torn_points() {
    // "Torn writes at every byte and every program unit": the unit boundaries are a subset
    // of the byte boundaries, and this is the assertion that says so rather than assuming
    // it.
    let ops = [Op::Program { offset: 0, len: 16 }];
    let program_unit = geometry().program_size();
    let torn: BTreeSet<u32> = injections(&ops, geometry())
        .into_iter()
        .filter(|injection| injection.effect == Effect::PowerLoss)
        .filter_map(|injection| match injection.progress {
            Progress::Bytes(bytes) => Some(bytes),
            Progress::None | Progress::Whole => None,
        })
        .collect();

    assert_eq!(torn, (1..16).collect::<BTreeSet<u32>>());
    for boundary in (1..4).map(|unit| unit * program_unit) {
        assert!(torn.contains(&boundary), "missing unit boundary {boundary}");
    }
}

#[test]
fn an_erase_is_interrupted_at_erase_blocks_rather_than_at_bytes() {
    // An erase does not proceed byte by byte on any device that exists. "At any supported
    // unit" is the erase block, and a model that offered byte granularity here would be
    // inventing failure modes rather than covering them.
    let ops = [Op::Erase { offset: 0, len: 64 }];
    let torn: Vec<Progress> = injections(&ops, geometry())
        .into_iter()
        .filter(|injection| injection.effect == Effect::PowerLoss)
        .map(|injection| injection.progress)
        .collect();
    assert_eq!(
        torn,
        vec![Progress::None, Progress::Bytes(32), Progress::Whole]
    );
}

#[test]
fn a_barrier_has_a_crash_point_before_it_and_a_crash_point_after_it() {
    let ops = [Op::Program { offset: 0, len: 4 }, Op::Barrier];
    let points = injections(&ops, geometry());

    // "Before the barrier" is the whole of the program having landed and nothing more.
    assert!(points.contains(&Injection {
        op: 0,
        progress: Progress::Whole,
        effect: Effect::PowerLoss,
    }));
    // "After the barrier" is the barrier itself having completed.
    assert!(points.contains(&Injection {
        op: 1,
        progress: Progress::Whole,
        effect: Effect::PowerLoss,
    }));
    // A barrier cannot be torn: there is no half of it to land.
    assert!(
        !points
            .iter()
            .any(|injection| injection.op == 1 && matches!(injection.progress, Progress::Bytes(_)))
    );
    // And a barrier that returns an error is one crash point, not two: whether the
    // ordering was established or not, a caller that saw the error learned nothing.
    assert_eq!(
        points
            .iter()
            .filter(|injection| injection.op == 1 && injection.effect == Effect::Failure)
            .count(),
        1
    );
}

#[test]
fn the_enumeration_is_a_pure_function_and_has_no_duplicates() {
    let ops = [
        Op::Erase { offset: 0, len: 32 },
        Op::Program { offset: 0, len: 8 },
        Op::Barrier,
        Op::Program { offset: 8, len: 4 },
        Op::Barrier,
    ];
    let first = injections(&ops, geometry());
    assert_eq!(first, injections(&ops, geometry()));

    let unique: BTreeSet<&Injection> = first.iter().collect();
    assert_eq!(unique.len(), first.len(), "{first:?}");
}

#[test]
fn the_count_is_the_arithmetic_the_sequence_implies() {
    // Power loss: one before anything, plus one per tear point, plus one after each op.
    // Failure: one per tear point plus `None` and `Whole` for each mutating op, and a
    // single point for each barrier.
    let ops = [Op::Program { offset: 0, len: 8 }, Op::Barrier];
    let points = injections(&ops, geometry());
    let power = 1 + 7 + 1 + 1;
    let failure = (7 + 2) + 1;
    assert_eq!(points.len(), power + failure);
}

#[test]
fn an_empty_sequence_still_has_the_crash_point_before_it_started() {
    let points = injections(&[], geometry());
    assert_eq!(
        points,
        vec![Injection {
            op: 0,
            progress: Progress::None,
            effect: Effect::PowerLoss,
        }]
    );
}

#[test]
fn a_single_unit_operation_has_no_interior_tear_points() {
    let ops = [
        Op::Program { offset: 0, len: 1 },
        Op::Erase { offset: 0, len: 32 },
    ];
    let points = injections(&ops, geometry());
    assert!(
        !points
            .iter()
            .any(|injection| matches!(injection.progress, Progress::Bytes(_)))
    );
}
