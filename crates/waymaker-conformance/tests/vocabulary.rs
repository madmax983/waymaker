//! Every word this crate can say to a driver author.
//!
//! A conformance suite that could not explain itself would be a suite whose report a driver
//! author has to read the source to understand, so every refusal, failure, exemption and
//! breach carries a sentence. This file is what stops one of them from being wrong, absent,
//! or a copy of its neighbour: `message()` and `Display` agree everywhere, and no two
//! variants of one enum say the same thing.

use waymaker_conformance::case::{
    CASE_COUNT, CaseId, Failure, NotApplicable, Outcome, Report, Verdict,
};
use waymaker_conformance::clause::Discharge;
use waymaker_conformance::durability::{Breach, WitnessError};
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
        NotApplicable::TheErasedStateHasNoProgrammableBits,
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
fn a_report_that_was_never_run_is_not_a_pass() {
    // The failure this guards is the one a conformance suite is most likely to have: a run
    // that did nothing and reported no failures.
    let report = Report::default();
    assert_eq!(report, Report::new());
    assert_eq!(report.entries().count(), CASE_COUNT);
    assert_eq!(
        report.verdict(),
        Err(Verdict::NotRun(CaseId::GeometryIsStable))
    );
    assert_eq!(report.first_failure(), None);
    assert_eq!(report.exemptions().count(), 0);
    assert_eq!(report.outcome(CaseId::BarrierSucceeds), Outcome::NotRun);
}

#[test]
fn a_report_records_what_it_is_told() {
    let mut report = Report::new();
    for (case, _) in Report::new().entries() {
        report.record(case.id, Outcome::Passed);
    }
    assert_eq!(report.verdict(), Ok(()));

    report.record(
        CaseId::ProgramRoundTripsThroughRead,
        Outcome::Failed(Failure::ReadBackDiffers),
    );
    assert_eq!(
        report.verdict(),
        Err(Verdict::Failed(
            CaseId::ProgramRoundTripsThroughRead,
            Failure::ReadBackDiffers
        ))
    );
    assert_eq!(
        report.first_failure(),
        Some((
            CaseId::ProgramRoundTripsThroughRead,
            Failure::ReadBackDiffers
        ))
    );

    report.record(
        CaseId::MisalignedReadIsRefused,
        Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
    );
    assert_eq!(
        report.exemptions().collect::<Vec<_>>(),
        [(
            CaseId::MisalignedReadIsRefused,
            NotApplicable::TheUnitIsOneByte
        )]
    );
}

#[test]
fn every_case_id_indexes_its_own_row() {
    for (case, _) in Report::new().entries() {
        assert_eq!(case.id.spec(), Some(case));
        assert_eq!(case.id.name(), case.name);
        assert!(case.id.index() < CASE_COUNT);
    }
}
