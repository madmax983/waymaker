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
use waymaker_conformance::suite::ERASED as SUITE_ERASED;
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
    /// An erase does nothing, on media that reads as zeros to start with.
    ///
    /// The shape a suite that *learned* the erased byte could be talked out of testing: it
    /// would conclude that erased is `0x00`, that nothing is programmable, and that it had
    /// no questions to ask. `ERASED` is a constant for this reason.
    EraseDoesNothingOnZeroedMedia,
    /// The start offset is bounds-checked and the end is not.
    ///
    /// Far more common than not checking bounds at all, and invisible to a probe whose
    /// offset is already past the capacity.
    BoundsCheckedAtTheStartOnly,
    /// A straddling mutation applies its in-bounds prefix and then refuses.
    StraddlingMutationWipesTheValidPrefix,
    /// A legal program of more than one unit is refused.
    RejectsAMultiUnitProgram,
    /// A legal erase of more than one block is refused.
    RejectsAMultiBlockErase,
    /// A read hands back the right bytes and then corrupts the media it read.
    ReadCorruptsWhatItReturned,
    /// A program also clears the program unit *before* the one it was given.
    ProgramCorruptsThePrecedingUnit,
    /// An erase also clears the erase block *before* the one it was given.
    EraseTakesThePrecedingBlock,
    /// `barrier()` clears a byte in the middle block on its way through.
    BarrierScribblesInTheMiddleBlock,
    /// `barrier()` clears a byte in the fourth block, past the three the run works in.
    BarrierScribblesBeyondTheWorkingBlocks,
    /// A refused *misaligned* erase clears exactly the range it named on its way out.
    ///
    /// The one an adapter that validates after the fact really has: it does the work, then
    /// notices, then reports. Whether the suite sees it depends entirely on whether its
    /// witness bytes lie inside the range the erase named.
    RefusedMisalignedEraseTakesTheRangeItNamed,
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
        let initial = if flaw == Flaw::EraseDoesNothingOnZeroedMedia {
            0x00
        } else {
            ERASED
        };
        Self {
            geometry,
            media: vec![initial; geometry.capacity() as usize],
            flaw,
            geometry_calls: Cell::new(0),
        }
    }

    /// The bytes as they stand.
    fn image(&self) -> &[u8] {
        &self.media
    }

    fn validates(&self) -> bool {
        self.flaw != Flaw::NoValidation
    }

    fn checks_bounds(&self) -> bool {
        self.validates() && self.flaw != Flaw::PastCapacityIsClamped
    }

    /// Whether an out-of-bounds verdict on a request that *starts* in bounds is honoured.
    fn checks_the_end(&self, offset: u32) -> bool {
        self.checks_bounds()
            && !(self.flaw == Flaw::BoundsCheckedAtTheStartOnly
                && offset < self.geometry.capacity())
    }

    /// The in-bounds part of `offset..offset + len`.
    fn valid_prefix(&self, offset: u32, len: u32) -> u32 {
        self.geometry.capacity().saturating_sub(offset).min(len)
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
                Err(GeometryError::OutOfBounds) if !self.checks_the_end(offset) => {}
                Err(_) => return Err(Refused),
            }
        }
        if self.flaw == Flaw::ReadReturnsErased {
            dst.fill(ERASED);
            return Ok(());
        }
        if self.flaw == Flaw::ReadCorruptsWhatItReturned {
            let Ok(start) = usize::try_from(offset) else {
                return Err(Refused);
            };
            let Some(end) = start.checked_add(dst.len()) else {
                return Err(Refused);
            };
            let Some(source) = self.media.get(start..end) else {
                return Err(Refused);
            };
            dst.copy_from_slice(source);
            // The bytes handed back are the right ones; the media they came from is not,
            // one instruction later. Nothing that compares what a read returned can see it.
            self.fill(offset, len, |_| 0x00);
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
            || (self.flaw == Flaw::RejectsAMultiUnitProgram && len > self.geometry.program_size())
        {
            return Err(Refused);
        }
        if self.validates() {
            match self.geometry.validate_program(offset, len) {
                Ok(()) => {}
                Err(GeometryError::OutOfBounds) if !self.checks_the_end(offset) => {}
                Err(error) => {
                    if self.flaw == Flaw::RefusalScribblesFirst {
                        self.apply(offset, src);
                    }
                    if self.flaw == Flaw::StraddlingMutationWipesTheValidPrefix
                        && error == GeometryError::OutOfBounds
                    {
                        let keep = self.valid_prefix(offset, len) as usize;
                        if let Some(prefix) = src.get(..keep) {
                            let owned = prefix.to_vec();
                            self.apply(offset, &owned);
                        }
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
            Flaw::ProgramCorruptsThePrecedingUnit => {
                self.apply(offset, src);
                let unit = self.geometry.program_size();
                if let Some(previous) = offset.checked_sub(unit) {
                    let spill = vec![0_u8; unit as usize];
                    self.apply(previous, &spill);
                }
            }
            _ => self.apply(offset, src),
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        if self.flaw == Flaw::EraseAlwaysFails
            || (self.flaw == Flaw::ZeroLengthIsRefused && len == 0)
            || (self.flaw == Flaw::RejectsAMultiBlockErase && len > self.geometry.erase_size())
        {
            return Err(Refused);
        }
        if self.validates() {
            match self.geometry.validate_erase(offset, len) {
                Ok(()) => {}
                Err(GeometryError::OutOfBounds) if !self.checks_the_end(offset) => {}
                Err(error) => {
                    if self.flaw == Flaw::RefusedMisalignedEraseTakesTheRangeItNamed
                        && error != GeometryError::OutOfBounds
                    {
                        self.fill(offset, len, |_| ERASED);
                    }
                    if self.flaw == Flaw::StraddlingMutationWipesTheValidPrefix
                        && error == GeometryError::OutOfBounds
                    {
                        let keep = self.valid_prefix(offset, len);
                        self.fill(offset, keep, |_| ERASED);
                    }
                    return Err(Refused);
                }
            }
        }
        match self.flaw {
            Flaw::EraseDoesNothing | Flaw::EraseDoesNothingOnZeroedMedia => {}
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
            Flaw::EraseTakesThePrecedingBlock => {
                self.fill(offset, len, |_| ERASED);
                let block = self.geometry.erase_size();
                if let Some(previous) = offset.checked_sub(block) {
                    self.fill(previous, block, |_| ERASED);
                }
            }
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
            Flaw::BarrierScribblesInTheMiddleBlock => {
                let block = self.geometry.erase_size();
                self.apply(block, &[0x00]);
                Ok(())
            }
            Flaw::BarrierScribblesBeyondTheWorkingBlocks => {
                let block = self.geometry.erase_size();
                self.apply(block.saturating_mul(3), &[0x00]);
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
        CaseId::EraseYieldsTheErasedByte,
        Failure::EraseDidNotClearTheRegion,
    ),
    (
        Flaw::EraseYieldsZeros,
        CaseId::EraseYieldsTheErasedByte,
        Failure::EraseDidNotClearTheRegion,
    ),
    (
        Flaw::EraseDoesNothingOnZeroedMedia,
        CaseId::EraseYieldsTheErasedByte,
        Failure::EraseDidNotClearTheRegion,
    ),
    (
        Flaw::BoundsCheckedAtTheStartOnly,
        CaseId::ReadPastCapacityIsRefused,
        Failure::IllegalOperationAccepted,
    ),
    (
        Flaw::StraddlingMutationWipesTheValidPrefix,
        CaseId::MutationStraddlingTheCapacityIsRefused,
        Failure::RefusedOperationTouchedMedia,
    ),
    (
        Flaw::RefusedMisalignedEraseTakesTheRangeItNamed,
        CaseId::RefusedEraseTouchesNoMedia,
        Failure::RefusedOperationTouchedMedia,
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
        Flaw::RejectsAMultiUnitProgram,
        CaseId::MultiUnitProgramIsLegal,
        Failure::LegalOperationRefused,
    ),
    (
        Flaw::RejectsAMultiBlockErase,
        CaseId::MultiBlockEraseIsLegal,
        Failure::LegalOperationRefused,
    ),
    (
        Flaw::ReadCorruptsWhatItReturned,
        CaseId::ReadingChangesNoMedia,
        Failure::ReadBackDiffers,
    ),
    (
        Flaw::ProgramCorruptsThePrecedingUnit,
        CaseId::ProgramLeavesTheRestOfTheBlockAlone,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::EraseTakesThePrecedingBlock,
        CaseId::EraseLeavesTheNeighbouringBlockAlone,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::BarrierScribblesInTheMiddleBlock,
        CaseId::BarrierChangesNoMedia,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::BarrierScribblesBeyondTheWorkingBlocks,
        CaseId::BarrierChangesNoMedia,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::EraseTakesTheWholeDevice,
        CaseId::EraseLeavesTheNeighbouringBlockAlone,
        Failure::MediaOutsideTheOperationChanged,
    ),
    (
        Flaw::EraseDoesNothing,
        CaseId::EraseYieldsTheErasedByte,
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
        CaseId::EraseYieldsTheErasedByte,
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
fn every_flaw_the_model_can_wear_names_the_case_that_catches_it() {
    // `expected` below is an exhaustive `match`, so a `Flaw` added to the model and left
    // out of it does not compile. That is the half a hand-written list cannot do; what a
    // list still has to carry is which flaws actually get *run*, and this checks that every
    // row of `TEETH` is one of them and that no flaw is claimed twice.
    for (index, (flaw, _, _)) in TEETH.iter().enumerate() {
        assert_eq!(
            expected(*flaw),
            TEETH.get(index).map(|(_, case, failure)| (*case, *failure)),
            "{flaw:?}'s row disagrees with the exhaustive table"
        );
        assert!(
            !TEETH
                .iter()
                .skip(index + 1)
                .any(|(other, _, _)| other == flaw),
            "{flaw:?} appears twice"
        );
    }
    assert_eq!(
        TEETH.len(),
        ALL.len(),
        "every flaw the model can wear should be run"
    );
    for flaw in ALL {
        assert!(
            TEETH.iter().any(|(candidate, _, _)| candidate == flaw),
            "{flaw:?} is a wrong adapter no case is required to catch"
        );
    }
}

#[test]
fn the_model_and_the_suite_agree_on_the_erased_byte() {
    // The model fills media with `ERASED`; the suite requires an erase to produce
    // `suite::ERASED`. Two constants that drifted apart would make every teeth row above a
    // test of the disagreement rather than of the adapter.
    assert_eq!(ERASED, SUITE_ERASED);
}

/// Whether a flaw makes a *legal* operation touch media the operation did not name.
///
/// The two that do are outside what any suite can contain, which is why the region claim is
/// about the offsets the cases *name* rather than about the bytes that end up changed. An
/// adapter asked to erase one block of the region — an operation the caller authorised — and
/// which erases the whole chip cannot be avoided by choosing better offsets, and neither can
/// a `barrier` that scribbles. Both are *caught*, by
/// `EraseLeavesTheNeighbouringBlockAlone` and `BarrierChangesNoMedia`; neither is contained.
///
/// An exhaustive `match`, so a flaw added to the model has to say which side of this it is
/// on rather than defaulting into a claim it might break.
const fn runs_wild_on_a_legal_operation(flaw: Flaw) -> bool {
    match flaw {
        // Every one of these answers a *legal* operation by touching media the operation did
        // not name, which no choice of probe offsets can avoid. Each is caught by the case
        // that watches for it; none is contained.
        Flaw::EraseTakesTheWholeDevice
        | Flaw::EraseTakesThePrecedingBlock
        | Flaw::ProgramCorruptsThePrecedingUnit
        | Flaw::BarrierScribbles
        | Flaw::BarrierScribblesInTheMiddleBlock
        | Flaw::BarrierScribblesBeyondTheWorkingBlocks => true,
        Flaw::None
        | Flaw::NoValidation
        | Flaw::PastCapacityIsClamped
        | Flaw::BoundsCheckedAtTheStartOnly
        | Flaw::WanderingGeometry
        | Flaw::RefusalScribblesFirst
        | Flaw::StraddlingMutationWipesTheValidPrefix
        | Flaw::RefusedMisalignedEraseTakesTheRangeItNamed
        | Flaw::EraseYieldsMixedBytes
        | Flaw::EraseYieldsZeros
        | Flaw::EraseDoesNothingOnZeroedMedia
        | Flaw::ProgramIsIgnored
        | Flaw::ProgramSpillsIntoTheNextUnit
        | Flaw::RejectsAMultiUnitProgram
        | Flaw::RejectsAMultiBlockErase
        | Flaw::ReadCorruptsWhatItReturned
        | Flaw::EraseDoesNothing
        | Flaw::ZeroLengthIsRefused
        | Flaw::BarrierFails
        | Flaw::ReadReturnsErased
        | Flaw::ReadAlwaysFails
        | Flaw::ProgramAlwaysFails
        | Flaw::EraseAlwaysFails => false,
    }
}

/// The case and failure each flaw must produce, as an exhaustive `match`.
const fn expected(flaw: Flaw) -> Option<(CaseId, Failure)> {
    match flaw {
        // The control wears no flaw, so nothing catches it.
        Flaw::None => None,
        Flaw::NoValidation => Some((
            CaseId::MisalignedReadIsRefused,
            Failure::IllegalOperationAccepted,
        )),
        Flaw::PastCapacityIsClamped | Flaw::BoundsCheckedAtTheStartOnly => Some((
            CaseId::ReadPastCapacityIsRefused,
            Failure::IllegalOperationAccepted,
        )),
        Flaw::WanderingGeometry => Some((CaseId::GeometryIsStable, Failure::GeometryIsNotStable)),
        Flaw::RefusalScribblesFirst => Some((
            CaseId::RefusedProgramTouchesNoMedia,
            Failure::RefusedOperationTouchedMedia,
        )),
        Flaw::StraddlingMutationWipesTheValidPrefix => Some((
            CaseId::MutationStraddlingTheCapacityIsRefused,
            Failure::RefusedOperationTouchedMedia,
        )),
        Flaw::RefusedMisalignedEraseTakesTheRangeItNamed => Some((
            CaseId::RefusedEraseTouchesNoMedia,
            Failure::RefusedOperationTouchedMedia,
        )),
        Flaw::EraseYieldsMixedBytes
        | Flaw::EraseYieldsZeros
        | Flaw::EraseDoesNothingOnZeroedMedia
        | Flaw::EraseDoesNothing => Some((
            CaseId::EraseYieldsTheErasedByte,
            Failure::EraseDidNotClearTheRegion,
        )),
        Flaw::ProgramIsIgnored | Flaw::ReadReturnsErased => Some((
            CaseId::ProgramRoundTripsThroughRead,
            Failure::ReadBackDiffers,
        )),
        Flaw::ProgramSpillsIntoTheNextUnit | Flaw::ProgramCorruptsThePrecedingUnit => Some((
            CaseId::ProgramLeavesTheRestOfTheBlockAlone,
            Failure::MediaOutsideTheOperationChanged,
        )),
        Flaw::EraseTakesTheWholeDevice | Flaw::EraseTakesThePrecedingBlock => Some((
            CaseId::EraseLeavesTheNeighbouringBlockAlone,
            Failure::MediaOutsideTheOperationChanged,
        )),
        Flaw::BarrierScribbles
        | Flaw::BarrierScribblesInTheMiddleBlock
        | Flaw::BarrierScribblesBeyondTheWorkingBlocks => Some((
            CaseId::BarrierChangesNoMedia,
            Failure::MediaOutsideTheOperationChanged,
        )),
        Flaw::RejectsAMultiUnitProgram => Some((
            CaseId::MultiUnitProgramIsLegal,
            Failure::LegalOperationRefused,
        )),
        Flaw::RejectsAMultiBlockErase => Some((
            CaseId::MultiBlockEraseIsLegal,
            Failure::LegalOperationRefused,
        )),
        Flaw::ReadCorruptsWhatItReturned => {
            Some((CaseId::ReadingChangesNoMedia, Failure::ReadBackDiffers))
        }
        Flaw::ZeroLengthIsRefused => Some((
            CaseId::ZeroLengthOperationsAreLegalAndChangeNothing,
            Failure::LegalOperationRefused,
        )),
        Flaw::BarrierFails => Some((CaseId::BarrierSucceeds, Failure::LegalOperationRefused)),
        Flaw::ReadAlwaysFails => Some((
            CaseId::EraseYieldsTheErasedByte,
            Failure::LegalOperationRefused,
        )),
        Flaw::ProgramAlwaysFails => Some((
            CaseId::RefusedEraseTouchesNoMedia,
            Failure::LegalOperationRefused,
        )),
        Flaw::EraseAlwaysFails => Some((
            CaseId::RefusedProgramTouchesNoMedia,
            Failure::LegalOperationRefused,
        )),
    }
}

/// Every flaw the model can wear, which is what the runs above iterate.
const ALL: &[Flaw] = &[
    Flaw::NoValidation,
    Flaw::PastCapacityIsClamped,
    Flaw::BoundsCheckedAtTheStartOnly,
    Flaw::WanderingGeometry,
    Flaw::RefusalScribblesFirst,
    Flaw::StraddlingMutationWipesTheValidPrefix,
    Flaw::RefusedMisalignedEraseTakesTheRangeItNamed,
    Flaw::EraseYieldsMixedBytes,
    Flaw::EraseYieldsZeros,
    Flaw::EraseDoesNothingOnZeroedMedia,
    Flaw::ProgramIsIgnored,
    Flaw::ProgramSpillsIntoTheNextUnit,
    Flaw::ProgramCorruptsThePrecedingUnit,
    Flaw::RejectsAMultiUnitProgram,
    Flaw::RejectsAMultiBlockErase,
    Flaw::ReadCorruptsWhatItReturned,
    Flaw::EraseTakesTheWholeDevice,
    Flaw::EraseTakesThePrecedingBlock,
    Flaw::BarrierScribblesInTheMiddleBlock,
    Flaw::BarrierScribblesBeyondTheWorkingBlocks,
    Flaw::EraseDoesNothing,
    Flaw::ZeroLengthIsRefused,
    Flaw::BarrierFails,
    Flaw::BarrierScribbles,
    Flaw::ReadReturnsErased,
    Flaw::ReadAlwaysFails,
    Flaw::ProgramAlwaysFails,
    Flaw::EraseAlwaysFails,
];

#[test]
fn no_wrong_adapter_lets_the_suite_damage_media_outside_the_region() {
    // The claim `lib.rs` makes is that the suite never mutates a byte outside the caller's
    // region — and the only interesting version of that claim is against an adapter that
    // does *not* refuse what it should. A conformant device makes it trivially true, which
    // is why `tests/suite.rs`'s region test cannot stand in for this one.
    //
    // The region here deliberately ends three blocks short of the device, so the erase block
    // an out-of-bounds mutation would start in is outside it.
    let geometry = nested();
    let Ok(region) = Region::new(geometry, 64, 192) else {
        unreachable!("64 and 192 are whole 64-byte blocks inside 1024 bytes")
    };
    let start = region.offset() as usize;
    let end = region.end() as usize;

    for flaw in ALL.iter().chain(core::iter::once(&Flaw::None)) {
        if runs_wild_on_a_legal_operation(*flaw) {
            continue;
        }
        let mut device = Broken::new(geometry, *flaw);
        // Something on both sides of the region, so "untouched" is a statement about bytes
        // that had a value worth keeping.
        let witness = [0x5A_u8; 4];
        for offset in [0_u32, 512, 1020] {
            // Whether a wrong adapter accepts the witness is not this test's question; that
            // the suite leaves whatever is there alone is.
            let _ = StableStorage::program(&mut device, offset, &witness[..]);
        }
        let before = device.image().to_vec();
        let mut buffer = [0_u8; 64];

        // A wrong adapter is expected to fail the run; where its media ends up is the point.
        let _ = run(&mut device, region, &mut buffer);

        let after = device.image();
        assert_eq!(
            before.get(..start),
            after.get(..start),
            "{flaw:?} let the suite change media before the region"
        );
        assert_eq!(
            before.get(end..),
            after.get(end..),
            "{flaw:?} let the suite change media after the region"
        );
    }
}

#[test]
fn a_region_short_of_the_end_of_the_device_says_which_question_it_cannot_ask() {
    // The other half of the finding above: the mutation that starts in bounds and ends past
    // the capacity is not merely skipped, it is reported, so a run on a mid-device region
    // cannot be mistaken for one that asked everything.
    let geometry = nested();
    let Ok(region) = Region::new(geometry, 64, 192) else {
        unreachable!("64 and 192 are whole 64-byte blocks inside 1024 bytes")
    };
    let mut device = Broken::new(geometry, Flaw::None);
    let mut buffer = [0_u8; 64];

    let report = run(&mut device, region, &mut buffer).expect("the run starts");

    assert_eq!(report.verdict(), Ok(()), "{report:?}");
    assert_eq!(
        report.outcome(CaseId::MutationStraddlingTheCapacityIsRefused),
        Outcome::NotApplicable(NotApplicable::TheRegionDoesNotEndAtTheCapacity)
    );
}
