//! Issue [#80](https://github.com/madmax983/waymaker/issues/80): a bank too small for the
//! run it will be asked to write is refused by [`Rig::new`], not discovered mid-iteration.
//!
//! `Rig::new` already refused a bank that could not hold §10's two banks, or an instrument
//! that could not hold a clean run's witness marks. It did not refuse a bank whose *journal*
//! could not hold the run itself: [`BankLayout::new`] only guarantees room for one record, and
//! nothing compared that against `effects` schedule/completion pairs. The reported geometry —
//! `Geometry::new(3 * 1024, 1024, 256, 1)` — was accepted at construction and then failed
//! partway through the first iteration with `RigError::Append(AppendError::NoRoom)`, at every
//! effect count including zero.
//!
//! [`RigError::BankTooSmall`] is the fix: a run's records are priced at
//! [`Workload::MAX_PAYLOAD_BYTES`] each, which is the worst any iteration of the plan can ask
//! for, and a bank whose journal cannot hold that many is refused before anything is written.

use waymaker_flash::storage::Geometry;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Rig, RigError};

/// The geometry issue #80 reported: three erase blocks, one bank each of one, and a
/// 256-byte program unit four times the widest record this rig ever writes.
fn reported_geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(3 * 1024, 1024, 256, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

#[test]
fn a_bank_too_small_for_the_run_is_refused_at_construction() {
    // The smallest run there is: `RunStarted` and `RunCompleted`, no effects at all. Even
    // this does not fit, so every larger run fails the same way — refused here rather than
    // discovered in the middle of the first iteration.
    for effects in [0_u16, 1, 5, 100] {
        let built =
            Rig::new::<waymaker_fault::FaultError>(reported_geometry(), Plan::new(0), effects);
        let Err(RigError::BankTooSmall { needed, capacity }) = built else {
            panic!("{effects} effects against the reported geometry: {built:?}");
        };
        assert!(
            needed > capacity,
            "a refusal must name a need that really exceeds the capacity it names"
        );
    }
}

#[test]
fn the_refusal_names_the_bank_rather_than_a_writer_mid_run() {
    // Before the fix this geometry was accepted and `iterate` failed with
    // `RigError::Append(AppendError::NoRoom)`. The refusal now has to come from `new`, with
    // a variant that names the bank rather than the writer.
    let built = Rig::new::<waymaker_fault::FaultError>(reported_geometry(), Plan::new(0), 0);
    assert!(
        matches!(built, Err(RigError::BankTooSmall { .. })),
        "expected a bank refusal, got {built:?}"
    );
}

/// Geometries and effect counts this crate's own suite, and `xtask::wear::PARTS`, already
/// exercise successfully. `waymaker-rig` cannot depend on `xtask`, so the wear report's own
/// parts are restated here rather than imported — see `xtask::wear::PARTS` and
/// `xtask::wear::EFFECTS`.
const EXERCISED: &[(u32, u32, u32, u32, u16)] = &[
    // `xtask::wear::PARTS`, at `xtask::wear::EFFECTS`.
    (6 * 4096, 4096, 1, 1, 8),
    (6 * 4096, 4096, 4, 1, 8),
    (6 * 4096, 4096, 16, 1, 8),
    // `crates/waymaker-rig/tests/{matrix,port,sweep,teeth,vocabulary}.rs`'s shared geometry.
    (6 * 256, 256, 4, 1, 2),
    (6 * 256, 256, 4, 1, 3),
];

#[test]
fn every_geometry_the_wear_report_and_the_rig_suite_use_is_still_accepted() {
    for &(capacity, erase, program, read, effects) in EXERCISED {
        let Ok(geometry) = Geometry::new(capacity, erase, program, read) else {
            unreachable!("every entry here is a geometry the rest of the suite already builds")
        };
        let built = Rig::new::<waymaker_fault::FaultError>(geometry, Plan::new(0), effects);
        assert!(
            built.is_ok(),
            "{capacity}/{erase}/{program}/{read} at {effects} effects was accepted before \
             issue #80's fix and must still be: {built:?}"
        );
    }
}
