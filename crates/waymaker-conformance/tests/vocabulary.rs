//! Every word this crate can say to a driver author.
//!
//! A conformance suite that could not explain itself would be a suite whose report a driver
//! author has to read the source to understand, so every refusal, failure, exemption and
//! breach carries a sentence. This file is what stops one of them from being wrong, absent,
//! or a copy of its neighbour: `message()` and `Display` agree everywhere, and no two
//! variants of one enum say the same thing.

use waymaker_conformance::case::{CaseId, Failure, NotApplicable, Verdict};
use waymaker_conformance::clause::Discharge;
use waymaker_conformance::durability::{Breach, WitnessError, WitnessVerdict};
use waymaker_conformance::nor::{PortError, PortGeometryError};
use waymaker_conformance::region::RegionError;
use waymaker_conformance::suite::SuiteError;
use waymaker_flash::storage::GeometryError;

/// Every message in `messages` is non-empty and unlike every other.
fn distinct(messages: &[&str]) {
    for (index, message) in messages.iter().enumerate() {
        assert!(!message.is_empty(), "message {index} is empty");
        assert!(
            !messages
                .iter()
                .skip(index + 1)
                .any(|other| other == message),
            "two variants both say `{message}`"
        );
    }
}

#[test]
fn every_suite_refusal_says_what_it_is() {
    let all = [
        SuiteError::RegionIsNotForThisDevice,
        SuiteError::BufferTooSmall,
    ];
    distinct(&all.map(SuiteError::message));
    for error in all {
        assert_eq!(error.to_string(), error.message());
    }
}

#[test]
fn every_region_refusal_says_what_it_is() {
    let all = [
        RegionError::NotEraseAligned,
        RegionError::OutOfBounds,
        RegionError::TooFewEraseBlocks,
    ];
    distinct(&all.map(RegionError::message));
    for error in all {
        assert_eq!(error.to_string(), error.message());
    }
}

#[test]
fn every_failure_says_what_it_is() {
    let all = [
        Failure::LegalOperationRefused,
        Failure::IllegalOperationAccepted,
        Failure::RefusedOperationTouchedMedia,
        Failure::ReadBackDiffers,
        Failure::EraseDidNotClearTheRegion,
        Failure::MediaOutsideTheOperationChanged,
        Failure::GeometryIsNotStable,
    ];
    distinct(&all.map(Failure::message));
    for failure in all {
        assert_eq!(failure.to_string(), failure.message());
    }
}

#[test]
fn every_exemption_says_which_property_of_the_device_earned_it() {
    let all = [
        NotApplicable::TheUnitIsOneByte,
        NotApplicable::TheBlockIsOneProgramUnit,
        NotApplicable::TheReadUnitIsTheProgramUnit,
        NotApplicable::TheRegionDoesNotEndAtTheCapacity,
    ];
    distinct(&all.map(NotApplicable::message));
}

#[test]
fn every_discharge_says_what_holds_a_clause_up() {
    let all = [
        Discharge::InProcess,
        Discharge::AcrossReset,
        Discharge::Injected,
        Discharge::Driver,
    ];
    distinct(&all.map(Discharge::message));
}

#[test]
fn every_breach_says_which_barrier_clause_it_breaks() {
    let all = [
        Breach::AcknowledgedMutationLost,
        Breach::LaterMutationOvertookABarrier,
    ];
    distinct(&all.map(Breach::message));
    for breach in all {
        assert_eq!(breach.to_string(), breach.message());
    }
}

#[test]
fn every_port_refusal_says_what_it_is() {
    let all = [
        PortGeometryError::UnitDoesNotFitInAWord,
        PortGeometryError::Geometry(GeometryError::UnitsDoNotNest),
    ];
    distinct(&all.map(PortGeometryError::message));
    for error in all {
        assert_eq!(error.to_string(), error.message());
    }
    // A geometry refusal is passed through rather than restated, so a driver author sees the
    // same sentence `waymaker-flash` would have given them.
    assert_eq!(
        PortGeometryError::Geometry(GeometryError::UnitsDoNotNest).message(),
        GeometryError::UnitsDoNotNest.message()
    );
}

#[test]
fn a_port_error_renders_either_half() {
    let refused: PortError<GeometryError> = PortError::Geometry(GeometryError::MisalignedOffset);
    assert_eq!(
        refused.to_string(),
        GeometryError::MisalignedOffset.message()
    );
    let driver: PortError<GeometryError> = PortError::Driver(GeometryError::OutOfBounds);
    assert_eq!(driver.to_string(), GeometryError::OutOfBounds.to_string());
}

#[test]
fn a_witness_error_carries_the_refusal_it_wraps() {
    // `?` on a `SuiteError` inside `arm` has to become a `WitnessError` without the call
    // sites naming the conversion, which is what the `From` impl is for.
    let converted: WitnessError<GeometryError> = SuiteError::BufferTooSmall.into();
    assert_eq!(converted, WitnessError::Suite(SuiteError::BufferTooSmall));
}

#[test]
fn a_verdict_names_the_case_and_what_went_wrong() {
    let not_run = Verdict::NotRun(CaseId::BarrierSucceeds);
    assert!(not_run.to_string().contains("case never ran"));
    assert!(not_run.to_string().contains(CaseId::BarrierSucceeds.name()));

    let failed = Verdict::Failed(
        CaseId::EraseIsIdempotent,
        Failure::EraseDidNotClearTheRegion,
    );
    assert!(
        failed
            .to_string()
            .contains(CaseId::EraseIsIdempotent.name())
    );
    assert!(
        failed
            .to_string()
            .contains(Failure::EraseDidNotClearTheRegion.message())
    );
}

#[test]
fn a_witness_verdict_reads_the_way_a_report_verdict_does() {
    // `report.verdict()?` propagates a failure, so `verify(..)?` had better not silently
    // discard a breach. `held()` is what makes the two read alike; without it the natural
    // spelling of "check the witness" is one that cannot fail.
    assert_eq!(WitnessVerdict::Held.held(), Ok(()));
    assert_eq!(
        WitnessVerdict::Breached(Breach::AcknowledgedMutationLost).held(),
        Err(Breach::AcknowledgedMutationLost)
    );
}

#[test]
fn a_witness_error_says_what_it_is() {
    let all: [WitnessError<GeometryError>; 3] = [
        WitnessError::Suite(SuiteError::BufferTooSmall),
        WitnessError::Driver(GeometryError::OutOfBounds),
        WitnessError::WitnessDidNotTake,
    ];
    let messages: [&str; 3] = [all[0].message(), all[1].message(), all[2].message()];
    distinct(&messages);
    // The driver's own error is what a caller wants to read, so `Display` reaches it even
    // though `message()` — which has to be `&'static str` — cannot.
    assert_eq!(
        all[1].to_string(),
        GeometryError::OutOfBounds.to_string(),
        "a driver refusal should render as the driver's own error"
    );
    assert_eq!(all[0].to_string(), SuiteError::BufferTooSmall.message());
    assert_eq!(all[2].to_string(), all[2].message());
}
