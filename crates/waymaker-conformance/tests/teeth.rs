//! Adapters that are wrong in one way each, and the case that has to catch each one.
//!
//! A conformance suite is worth exactly the failures it can produce. `tests/suite.rs` shows
//! that a correct adapter passes, which is the half that is easy to arrange; this is the
//! other half. Every row below is a driver bug somebody has actually shipped — a validator
//! that validates after the write, an erase that takes the whole chip, a barrier that is a
//! `no-op` with a scribble in it — and the table names the case that must go red for it.
//!
//! The model here is deliberately *not* `waymaker_fault::Device`: that device is correct on
//! purpose and cannot be asked to misbehave, and a suite tested only against a correct
//! adapter has never been observed failing.

use std::cell::Cell;

use waymaker_conformance::case::{CaseId, Failure, NotApplicable, Outcome};
use waymaker_conformance::region::Region;
use waymaker_conformance::suite::run;
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// The one thing a [`Broken`] gets wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flaw {
    /// Nothing. The control.
    None,
    /// Alignment and bounds are never checked.
    NoValidation,
    /// Bounds are not checked; an out-of-range access is quietly clamped.
    PastCapacityIsClamped,
    /// `geometry()` answers differently every other call.
    WanderingGeometry,
    /// A refused program writes first and refuses second.
    RefusalScribblesFirst,
    /// An erase leaves alternating bytes behind.
    EraseYieldsMixedBytes,
    /// A program is accepted and dropped.
    ProgramIsIgnored,
    /// A program also clears the following program unit.
    ProgramSpillsIntoTheNextUnit,
    /// An erase clears the whole device.
    EraseTakesTheWholeDevice,
    /// An erase is accepted and dropped.
    EraseDoesNothing,
    /// A zero-length operation is refused.
    ZeroLengthIsRefused,
    /// `barrier()` refuses.
    BarrierFails,
    /// `barrier()` clears a byte on its way through.
    BarrierScribbles,
    /// A read always returns the erased byte.
    ReadReturnsErased,
    /// Every read refuses.
    ReadAlwaysFails,
    /// Every program refuses.
    ProgramAlwaysFails,
    /// Every erase refuses.
    EraseAlwaysFails,
    /// An erase leaves a byte with no set bits, so nothing can be programmed afterwards.
    EraseYieldsZeros,
}

const ERASED: u8 = 0xFF;

/// A NOR model with one deliberate bug in it.
struct Broken {
    geometry: Geometry,
    media: Vec<u8>,
    flaw: Flaw,
    geometry_calls: Cell<u32>,
}

/// How a [`Broken`] refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Refused;

impl Broken {
    fn new(geometry: Geometry, flaw: Flaw) -> Self {
        Self {
            geometry,
            media: vec![ERASED; geometry.capacity() as usize],
            flaw,
            geometry_calls: Cell::new(0),
        }
    }

    fn validates(&self) -> bool {
        self.flaw != Flaw::NoValidation
    }

    fn checks_bounds(&self) -> bool {
        self.validates() && self.flaw != Flaw::PastCapacityIsClamped
    }

    /// Clears the bits of `src` at `offset`, as NOR programming does.
    fn apply(&mut self, offset: u32, src: &[u8]) {
        let Ok(start) = usize::try_from(offset) else {
            return;
        };
        let Some(end) = start.checked_add(src.len()) else {
            return;
        };
        let Some(target) = self.media.get_mut(start..end) else {
            return;
        };
        for (cell, wanted) in target.iter_mut().zip(src) {
            *cell &= *wanted;
        }
    }

    fn fill(&mut self, offset: u32, len: u32, pattern: impl Fn(usize) -> u8) {
        let (Ok(start), Ok(len)) = (usize::try_from(offset), usize::try_from(len)) else {
            return;
        };
        let Some(end) = start.checked_add(len) else {
            return;
        };
        let Some(target) = self.media.get_mut(start..end) else {
            return;
        };
        for (index, cell) in target.iter_mut().enumerate() {
            *cell = pattern(index);
        }
    }
}

impl StableStorage for Broken {
    type Error = Refused;

    fn geometry(&self) -> Geometry {
        let seen = self.geometry_calls.get();
        self.geometry_calls.set(seen.wrapping_add(1));
        if self.flaw == Flaw::WanderingGeometry && seen % 2 == 1 {
            let Ok(other) = Geometry::new(
                self.geometry.capacity(),
                self.geometry.erase_size(),
                self.geometry.program_size(),
                1,
            ) else {
                unreachable!("narrowing the read unit to one byte is always a geometry")
            };
            return other;
        }
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        if self.flaw == Flaw::ReadAlwaysFails
            || (self.flaw == Flaw::ZeroLengthIsRefused && dst.is_empty())
        {
            return Err(Refused);
        }
        let Ok(len) = u32::try_from(dst.len()) else {
            return Err(Refused);
        };
        if self.validates() {
            match self.geometry.validate_read(offset, len) {
                Ok(()) => {}
                Err(GeometryError::OutOfBounds) if !self.checks_bounds() => {}
                Err(_) => return Err(Refused),
            }
        }
        if self.flaw == Flaw::ReadReturnsErased {
            dst.fill(ERASED);
            return Ok(());
        }
        let Ok(start) = usize::try_from(offset) else {
            return Err(Refused);
        };
        let Some(end) = start.checked_add(dst.len()) else {
            return Err(Refused);
        };
        match self.media.get(start..end) {
            Some(source) => dst.copy_from_slice(source),
            // Only reachable with bounds checking off, which is the point of that flaw.
            None => dst.fill(ERASED),
        }
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let Ok(len) = u32::try_from(src.len()) else {
            return Err(Refused);
        };
        if self.flaw == Flaw::ProgramAlwaysFails
            || (self.flaw == Flaw::ZeroLengthIsRefused && src.is_empty())
        {
            return Err(Refused);
        }
        if self.validates() {
            match self.geometry.validate_program(offset, len) {
                Ok(()) => {}
                Err(GeometryError::OutOfBounds) if !self.checks_bounds() => {}
                Err(_) => {
                    if self.flaw == Flaw::RefusalScribblesFirst {
                        self.apply(offset, src);
                    }
                    return Err(Refused);
                }
            }
        }
        match self.flaw {
            Flaw::ProgramIsIgnored => {}
            Flaw::ProgramSpillsIntoTheNextUnit => {
                self.apply(offset, src);
                let spill = vec![0_u8; self.geometry.program_size() as usize];
                let next = offset.saturating_add(len);
                self.apply(next, &spill);
            }
            _ => self.apply(offset, src),
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        if self.flaw == Flaw::EraseAlwaysFails
            || (self.flaw == Flaw::ZeroLengthIsRefused && len == 0)
        {
            return Err(Refused);
        }
        if self.validates() {
            match self.geometry.validate_erase(offset, len) {
                Ok(()) => {}
                Err(GeometryError::OutOfBounds) if !self.checks_bounds() => {}
                Err(_) => return Err(Refused),
            }
        }
        match self.flaw {
            Flaw::EraseDoesNothing => {}
            Flaw::EraseTakesTheWholeDevice => {
                self.fill(0, self.geometry.capacity(), |_| ERASED);
            }
            Flaw::EraseYieldsMixedBytes => {
                self.fill(
                    offset,
                    len,
                    |index| {
                        if index % 2 == 0 { ERASED } else { 0xFE }
                    },
                );
            }
            Flaw::EraseYieldsZeros => self.fill(offset, len, |_| 0x00),
            _ => self.fill(offset, len, |_| ERASED),
        }
        Ok(())
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        match self.flaw {
            Flaw::BarrierFails => Err(Refused),
            Flaw::BarrierScribbles => {
                self.apply(0, &[0x00]);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// A geometry in which every unit is wider than the one below it, so nothing is exempt.
fn nested() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 64, 4, 2) else {
        unreachable!("1024 is whole 64-byte blocks of whole 4-byte units of 2-byte reads")
    };
    geometry
}

fn whole(geometry: Geometry) -> Region {
    let Ok(region) = Region::whole_device(geometry) else {
        unreachable!("sixteen erase blocks is more than three")
    };
    region
}

/// Every wrong adapter, and the case that must catch it.
const TEETH: &[(Flaw, CaseId, Failure)] = &[
    (
        Flaw::NoValidation,
        CaseId::MisalignedReadIsRefused,
        Failure::IllegalOperationAccepted,
    ),
    (
        Flaw::PastCapacityIsClamped,
        CaseId::ReadPastCapacityIsRefused,
        Failure::IllegalOperationAccepted,
    ),
    (
        Flaw::WanderingGeometry,
        CaseId::GeometryIsStable,
        Failure::GeometryIsNotStable,
    ),
    (
        Flaw::RefusalScribblesFirst,
        CaseId::RefusedProgramTouchesNoMedia,
        Failure::RefusedOperationTouchedMedia,
    ),
    (
        Flaw::EraseYieldsMixedBytes,
        CaseId::EraseYieldsOneRepeatedByte,
        Failure::EraseDidNotClearTheRegion,
    ),
    (
        Flaw::ProgramIsIgnored,
        CaseId::ProgramRoundTripsThroughRead,
        Failure::ReadBackDiffers,
    ),
    (
        Flaw::ReadReturnsErased,
        CaseId::ProgramRoundTripsThroughRead,
        Failure::ReadBackDiffers,
    ),
    (
        Flaw::ProgramSpillsIntoTheNextUnit,
        CaseId::ProgramLeavesTheRestOfTheBlockAlone,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::EraseTakesTheWholeDevice,
        CaseId::EraseLeavesTheNeighbouringBlockAlone,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::EraseDoesNothing,
        CaseId::EraseIsIdempotent,
        Failure::EraseDidNotClearTheRegion,
    ),
    (
        Flaw::ZeroLengthIsRefused,
        CaseId::ZeroLengthOperationsAreLegalAndChangeNothing,
        Failure::LegalOperationRefused,
    ),
    (
        Flaw::BarrierFails,
        CaseId::BarrierSucceeds,
        Failure::LegalOperationRefused,
    ),
    (
        Flaw::BarrierScribbles,
        CaseId::BarrierChangesNoMedia,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::ReadAlwaysFails,
        CaseId::EraseYieldsOneRepeatedByte,
        Failure::LegalOperationRefused,
    ),
    (
        Flaw::ProgramAlwaysFails,
        CaseId::RefusedEraseTouchesNoMedia,
        Failure::LegalOperationRefused,
    ),
    (
        Flaw::EraseAlwaysFails,
        CaseId::RefusedProgramTouchesNoMedia,
        Failure::LegalOperationRefused,
    ),
];

/// The adapters the suite does *not* catch, and the exemption each one earns instead.
///
/// A hole recorded as a test rather than as a footnote. `EraseYieldsZeros` is the honest
/// limit of a suite that refuses to assume NOR polarity: an erased state with no set bits is
/// something media is allowed to be, and it is indistinguishable from a read that is broken
/// in that direction. What the suite does instead of guessing is say which cases it could not
/// ask — which is what `NotApplicable` is for.
const EXEMPTED: &[(Flaw, CaseId, NotApplicable)] = &[(
    Flaw::EraseYieldsZeros,
    CaseId::ProgramRoundTripsThroughRead,
    NotApplicable::TheErasedStateHasNoProgrammableBits,
)];

#[test]
fn the_control_adapter_passes() {
    // Without this, every row below could be passing because the model is broken rather
    // than because the flaw is caught.
    let geometry = nested();
    let mut device = Broken::new(geometry, Flaw::None);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    assert_eq!(report.verdict(), Ok(()), "{report:?}");
}

#[test]
fn every_wrong_adapter_is_caught_by_the_case_that_names_it() {
    let geometry = nested();
    for (flaw, case, failure) in TEETH {
        let mut device = Broken::new(geometry, *flaw);
        let mut buffer = [0_u8; 64];

        let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

        assert_eq!(
            report.outcome(*case),
            Outcome::Failed(*failure),
            "{flaw:?} should have been caught by {case:?}: {report:?}"
        );
        assert!(
            report.verdict().is_err(),
            "{flaw:?} produced a passing verdict"
        );
    }
}

#[test]
fn an_adapter_the_suite_cannot_judge_is_exempted_rather_than_passed() {
    let geometry = nested();
    for (flaw, case, reason) in EXEMPTED {
        let mut device = Broken::new(geometry, *flaw);
        let mut buffer = [0_u8; 64];

        let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

        assert_eq!(
            report.outcome(*case),
            Outcome::NotApplicable(*reason),
            "{flaw:?} should have exempted {case:?}: {report:?}"
        );
        assert!(
            report.exemptions().any(|(exempt, _)| exempt == *case),
            "{flaw:?} exempted {case:?} without saying so in the report"
        );
    }
}

#[test]
fn a_failing_report_names_its_first_failure() {
    let geometry = nested();
    let mut device = Broken::new(geometry, Flaw::WanderingGeometry);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, whole(geometry), &mut buffer).expect("the run starts");

    assert_eq!(
        report.first_failure(),
        Some((CaseId::GeometryIsStable, Failure::GeometryIsNotStable))
    );
}

#[test]
fn every_flaw_the_model_can_wear_is_in_the_table() {
    // A flaw added to the model and left out of the table is a wrong adapter nobody
    // required the suite to catch, which is exactly the shape of an untested gate.
    const ALL: &[Flaw] = &[
        Flaw::NoValidation,
        Flaw::PastCapacityIsClamped,
        Flaw::WanderingGeometry,
        Flaw::RefusalScribblesFirst,
        Flaw::EraseYieldsMixedBytes,
        Flaw::ProgramIsIgnored,
        Flaw::ProgramSpillsIntoTheNextUnit,
        Flaw::EraseTakesTheWholeDevice,
        Flaw::EraseDoesNothing,
        Flaw::ZeroLengthIsRefused,
        Flaw::BarrierFails,
        Flaw::BarrierScribbles,
        Flaw::ReadReturnsErased,
        Flaw::ReadAlwaysFails,
        Flaw::ProgramAlwaysFails,
        Flaw::EraseAlwaysFails,
        Flaw::EraseYieldsZeros,
    ];
    for flaw in ALL {
        let caught = TEETH.iter().any(|(candidate, _, _)| candidate == flaw);
        let exempted = EXEMPTED.iter().any(|(candidate, _, _)| candidate == flaw);
        assert!(
            caught != exempted,
            "{flaw:?} must be in exactly one of TEETH and EXEMPTED: a wrong adapter in \
             neither is one no case is required to catch, and one in both is a claim that \
             contradicts itself"
        );
    }
}
