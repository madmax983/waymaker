//! The in-process suite, run against an adapter written by somebody else.
//!
//! `waymaker_fault::Device` is the in-memory NOR model issue #18 built, and nothing about
//! it was written for this crate. Running the suite against it is the first half of issue
//! #21's "a conformance test suite exists that any adapter can be run against": the suite
//! knows nothing about the `Device` and the `Device` knows nothing about the suite, and
//! `StableStorage` is all they share.

use waymaker_conformance::case::{CaseId, Failure, NotApplicable, Outcome};
use waymaker_conformance::region::{REQUIRED_ERASE_BLOCKS, Region, RegionError};
use waymaker_conformance::suite::{SuiteError, run};
use waymaker_fault::Device;
use waymaker_flash::storage::Geometry;

/// A geometry in which every unit is wider than the one below it, so no case is exempt.
fn nested() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 64, 4, 2) else {
        unreachable!("1024 is whole 64-byte blocks of whole 4-byte units of 2-byte reads")
    };
    geometry
}

/// A geometry with byte-granular programs and reads, where no misalignment exists.
fn byte_granular() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 64, 1, 1) else {
        unreachable!("1024 is whole 64-byte blocks of single bytes")
    };
    geometry
}

/// A geometry whose erase block is a single program unit.
fn block_is_one_unit() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 4, 4, 2) else {
        unreachable!("1024 is whole 4-byte blocks that are one 4-byte program unit")
    };
    geometry
}

fn whole(geometry: Geometry) -> Region {
    let Ok(region) = Region::whole_device(geometry) else {
        unreachable!("every geometry here has at least three erase blocks")
    };
    region
}

#[test]
fn a_conformant_adapter_passes_every_case() {
    let geometry = nested();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    assert_eq!(report.verdict(), Ok(()), "{report:?}");
    assert_eq!(report.first_failure(), None);
}

#[test]
fn a_conformant_adapter_leaves_no_case_unrun() {
    // The failure this guards is a case added to `CASES` and never wired into the runner:
    // the report would be shorter rather than redder, and a shorter report reads as a pass.
    let geometry = nested();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    for (case, outcome) in report.entries() {
        assert_ne!(outcome, Outcome::NotRun, "{} never ran", case.name);
    }
}

#[test]
fn a_geometry_with_no_degenerate_unit_exempts_no_case() {
    // Every exemption is a fact about the geometry. On a device where every unit is wider
    // than the one below it there is no such fact, so a report with an exemption in it
    // would be a case quietly declining to run.
    let geometry = nested();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    let exemptions: Vec<(CaseId, NotApplicable)> = report.exemptions().collect();
    assert_eq!(exemptions, [], "{report:?}");
}

#[test]
fn a_byte_granular_device_exempts_exactly_the_misalignment_cases() {
    let geometry = byte_granular();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    assert_eq!(report.verdict(), Ok(()), "{report:?}");
    assert_eq!(
        report.outcome(CaseId::MisalignedReadIsRefused),
        Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte)
    );
    assert_eq!(
        report.outcome(CaseId::MisalignedProgramIsRefused),
        Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte)
    );
    assert_eq!(
        report.outcome(CaseId::RefusedProgramTouchesNoMedia),
        Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte)
    );
    // The erase unit is still 64 bytes, so the erase half of the contract is unaffected.
    assert_eq!(
        report.outcome(CaseId::MisalignedEraseIsRefused),
        Outcome::Passed
    );
    assert_eq!(
        report.outcome(CaseId::RefusedEraseTouchesNoMedia),
        Outcome::Passed
    );
}

#[test]
fn a_block_that_is_one_program_unit_exempts_the_rest_of_the_block_case() {
    let geometry = block_is_one_unit();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    assert_eq!(report.verdict(), Ok(()), "{report:?}");
    assert_eq!(
        report.outcome(CaseId::ProgramLeavesTheRestOfTheBlockAlone),
        Outcome::NotApplicable(NotApplicable::TheBlockIsOneProgramUnit)
    );
}

#[test]
fn the_suite_refuses_a_buffer_it_cannot_work_in() {
    let geometry = nested();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 8];

    assert_eq!(
        run(&mut device, whole(geometry), &mut buffer),
        Err(SuiteError::BufferTooSmall)
    );
}

#[test]
fn the_suite_refuses_a_region_checked_against_another_device() {
    let mine = nested();
    let Ok(theirs) = Geometry::new(2048, 64, 4, 2) else {
        unreachable!("2048 is whole 64-byte blocks")
    };
    let mut device = Device::new(mine);
    let mut buffer = [0_u8; 64];

    assert_eq!(
        run(&mut device, whole(theirs), &mut buffer),
        Err(SuiteError::RegionIsNotForThisDevice)
    );
}

#[test]
fn a_region_must_be_erase_aligned_bounded_and_three_blocks_long() {
    let geometry = nested();
    assert_eq!(
        Region::new(geometry, 1, 192),
        Err(RegionError::NotEraseAligned)
    );
    assert_eq!(
        Region::new(geometry, 0, 65),
        Err(RegionError::NotEraseAligned)
    );
    assert_eq!(
        Region::new(geometry, 1024, 192),
        Err(RegionError::OutOfBounds)
    );
    assert_eq!(
        Region::new(geometry, 0, 128),
        Err(RegionError::TooFewEraseBlocks)
    );
    assert!(Region::new(geometry, 64, 192).is_ok());
}

#[test]
fn a_device_with_too_few_erase_blocks_cannot_be_conformance_tested() {
    // Told, rather than discovered from a suite that passed. Three blocks is what the
    // neighbour case and the across-reset witness need.
    let Ok(two_blocks) = Geometry::new(128, 64, 4, 2) else {
        unreachable!("128 is two 64-byte blocks")
    };
    assert_eq!(two_blocks.erase_blocks(), REQUIRED_ERASE_BLOCKS - 1);
    assert_eq!(
        Region::whole_device(two_blocks),
        Err(RegionError::TooFewEraseBlocks)
    );
}

#[test]
fn the_suite_runs_inside_the_region_it_was_given() {
    // The bytes outside the region are the ones the caller said not to destroy: on a real
    // part they are the firmware the driver is running from.
    let geometry = nested();
    let mut device = Device::new(geometry);
    let mut buffer = [0_u8; 64];
    let region = match Region::new(geometry, 256, 256) {
        Ok(region) => region,
        Err(error) => unreachable!("{error}"),
    };

    // Program a witness on both sides of the region, so "untouched" is a statement about
    // bytes that had something in them.
    let witness = [0x5A_u8; 4];
    for offset in [0_u32, 512, 1020] {
        match waymaker_flash::storage::StableStorage::program(&mut device, offset, &witness[..]) {
            Ok(()) => {}
            Err(error) => unreachable!("{error}"),
        }
    }
    let before = device.image().to_vec();

    let report = run(&mut device, region, &mut buffer).expect("the run starts");
    assert_eq!(report.verdict(), Ok(()), "{report:?}");

    let after = device.image();
    let start = region.offset() as usize;
    let end = region.end() as usize;
    assert_eq!(before.get(..start), after.get(..start), "before the region");
    assert_eq!(before.get(end..), after.get(end..), "after the region");
}

#[test]
fn a_region_reports_the_window_it_was_built_from() {
    let geometry = nested();
    let Ok(region) = Region::new(geometry, 128, 192) else {
        unreachable!("128 and 192 are whole 64-byte blocks inside 1024 bytes")
    };
    assert_eq!(region.geometry(), geometry);
    assert_eq!(region.offset(), 128);
    assert_eq!(region.len(), 192);
    assert_eq!(region.end(), 320);
    assert!(!region.is_empty());
    assert_eq!(region.block(0), Some(128));
    assert_eq!(region.block(2), Some(256));
    assert_eq!(region.block(3), None);
    assert_eq!(region.block(u32::MAX), None);
}
