//! The shape catalogue, held to the suite that has to issue every row of it.
//!
//! Issue [#130](https://github.com/madmax983/waymaker/issues/130) item 2 asks that "every
//! legal operation shape the firmware issues must appear in the suite". Design document
//! §12's contract is stated in four places for the same reason the clause table is —
//! `xtask::docs`, `CLAUDE.md`, an ADR and [`waymaker_conformance::shape::SHAPES`] — and the
//! `storage-shapes` rule of `cargo xtask check-layering` is what stops those four drifting.
//!
//! What that rule cannot see is inside this crate: whether a full suite run really issues
//! every shape the table declares, or only claims to. That is this file. A hand-written
//! table can go stale the moment a case changes; a run that is actually checked cannot.

use waymaker_conformance::case::CaseId;
use waymaker_conformance::region::Region;
use waymaker_conformance::shape::{SHAPES, ShapeWitness, shape};
use waymaker_conformance::suite::run;
use waymaker_fault::Device;
use waymaker_flash::storage::Geometry;

/// A geometry where every unit is a different width, so a multi-unit call is never a
/// single-unit one in disguise.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 64, 4, 2) else {
        unreachable!("1024 is whole 64-byte blocks of whole 4-byte units of 2-byte reads")
    };
    geometry
}

#[test]
fn every_shape_id_is_unique_and_non_empty() {
    for (index, spec) in SHAPES.iter().enumerate() {
        assert!(!spec.id.is_empty(), "row {index} has an empty id");
        assert!(
            !SHAPES
                .iter()
                .skip(index + 1)
                .any(|other| other.id == spec.id),
            "shape id {} appears twice",
            spec.id
        );
    }
}

#[test]
fn a_shape_nothing_declares_is_not_found() {
    assert!(shape("no-such-shape").is_none());
}

#[test]
fn every_declared_shape_is_found_by_its_own_id() {
    for spec in SHAPES {
        assert_eq!(shape(spec.id), Some(spec));
    }
}

/// The mechanical proof: every shape [`SHAPES`] declares is actually issued somewhere by a
/// full run of [`waymaker_conformance::case::CASES`] against a real adapter.
///
/// This is what makes the catalogue more than a claim. A row added to [`SHAPES`] with no
/// case behind it fails here rather than only in a reviewer's head, and a case deleted from
/// the suite that was the only one issuing a shape fails here too.
#[test]
fn a_full_run_issues_every_declared_shape() {
    let geometry = geometry();
    let mut device = Device::new(geometry);
    let mut witness = ShapeWitness::new(&mut device);
    let mut buffer = [0_u8; 64];
    let region = Region::whole_device(geometry).expect("four 64-byte blocks is enough");

    let report = run(&mut witness, region, &mut buffer).expect("the run starts");
    assert_eq!(report.verdict(), Ok(()), "{report:?}");

    let unseen: Vec<&str> = witness.unseen().map(|spec| spec.id).collect();
    assert_eq!(
        unseen,
        Vec::<&str>::new(),
        "shapes no case in this run issued"
    );
}

#[test]
fn a_witness_reports_nothing_seen_before_a_run() {
    let geometry = geometry();
    let mut device = Device::new(geometry);
    let witness = ShapeWitness::new(&mut device);
    assert_eq!(witness.unseen().count(), SHAPES.len());
}

#[test]
fn a_witness_ignores_a_refused_operation() {
    // A refusal is not a shape: `waymaker_conformance::case::Outcome` already has its own
    // vocabulary for "this was refused", and crediting a refused call would let a row read
    // as issued by an adapter that never accepted it.
    let geometry = geometry();
    let mut device = Device::new(geometry);
    let mut witness = ShapeWitness::new(&mut device);
    // Misaligned by one byte against a four-byte program unit: every adapter this suite
    // accepts refuses it, so nothing here can be credited.
    let refused = waymaker_flash::storage::StableStorage::program(&mut witness, 1, &[0_u8; 4]);
    assert!(refused.is_err());
    assert_eq!(witness.unseen().count(), SHAPES.len());
}

#[test]
fn the_case_table_still_agrees_with_itself() {
    // Not this file's job to re-prove, but the shape witness runs the whole suite, so a
    // silent `NotRun` here would be the fastest way for `SHAPES` coverage to look green for
    // the wrong reason.
    assert!(CaseId::GeometryIsStable.spec().is_some());
}
