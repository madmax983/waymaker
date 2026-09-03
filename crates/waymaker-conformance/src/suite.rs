//! The in-process conformance run.
//!
//! Nineteen cases, each one an observation an adapter either survives or does not. Two of
//! design document §12's clauses are what they speak for: `validated-before-media`, which
//! is about what an adapter *refuses*, and `operations-act-on-what-they-name`, which is
//! about what it does when it agrees.
//!
//! # What a run costs the device
//!
//! Three erase blocks, erased and reprogrammed several times, inside the [`Region`] the
//! caller named. Nothing outside the region is ever mutated — the illegal operations the
//! suite issues are chosen so that a *conformant* adapter refuses them, and a broken one
//! can only damage bytes the region already covers.
//!
//! # Why the caller supplies the buffer
//!
//! For the reason `waymaker_core::ReplayCursor` is pumped by its caller
//! ([ADR 0008](https://github.com/madmax983/waymaker/blob/main/docs/adr/0008-the-replay-cursor-is-pumped-by-its-caller.md)):
//! the page size a device wants is the device's business, and a suite with an internal
//! `[u8; N]` either refuses a 256-byte-page SPI part or charges every 4-byte-page internal
//! flash for one. Four program units is what the widest case needs — a source, the bytes
//! before it, and the bytes after it — and [`SuiteError::BufferTooSmall`] is what a caller
//! who supplied less is told.

use waymaker_flash::storage::StableStorage;

use crate::case::{CaseId, Failure, NotApplicable, Outcome, Report};
use crate::region::Region;

/// How many program units of scratch a run needs.
///
/// The widest case is [`CaseId::RefusedProgramTouchesNoMedia`]: the media as it stands, an
/// illegally-long source, and the media afterwards, held at once so the three can be
/// compared without a second pass.
pub const REQUIRED_BUFFER_UNITS: u32 = 4;

/// Why a conformance run could not start.
///
/// Distinct from a [`Failure`]: a failure means the adapter is wrong, and one of these
/// means the run never happened. A suite that reported "no failures" for a run it could not
/// start would be the exact reverse of what this crate is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuiteError {
    /// The region was checked against a different device than the one handed over.
    RegionIsNotForThisDevice,
    /// The caller's buffer is smaller than [`REQUIRED_BUFFER_UNITS`] program units.
    BufferTooSmall,
}

impl SuiteError {
    /// A short static description of this refusal.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::RegionIsNotForThisDevice => "the region describes a different device",
            Self::BufferTooSmall => "the buffer is smaller than four program units",
        }
    }
}

impl core::fmt::Display for SuiteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for SuiteError {}

/// Runs every case of [`crate::case::CASES`] against `storage`.
///
/// Every case runs, whatever the ones before it did: a report is a picture of the adapter
/// rather than a stack trace, and a driver author fixing two bugs would rather see both.
///
/// # Errors
///
/// [`SuiteError`] if the run could not start at all — a region for another device, or a
/// buffer smaller than [`REQUIRED_BUFFER_UNITS`] program units.
pub fn run<S: StableStorage>(
    storage: &mut S,
    region: Region,
    buffer: &mut [u8],
) -> Result<Report, SuiteError> {
    let geometry = region.geometry();
    if storage.geometry() != geometry {
        return Err(SuiteError::RegionIsNotForThisDevice);
    }
    let unit = usize::try_from(geometry.program_size()).map_err(|_| SuiteError::BufferTooSmall)?;
    let needed = unit
        .checked_mul(REQUIRED_BUFFER_UNITS as usize)
        .ok_or(SuiteError::BufferTooSmall)?;
    if buffer.len() < needed {
        return Err(SuiteError::BufferTooSmall);
    }

    let mut run = Run {
        storage,
        region,
        buffer,
        unit,
        report: Report::new(),
        erased: None,
    };
    run.everything();
    Ok(run.report)
}

/// One conformance run in progress.
struct Run<'a, S: StableStorage> {
    storage: &'a mut S,
    region: Region,
    buffer: &'a mut [u8],
    unit: usize,
    report: Report,
    /// The byte an erase leaves behind, learned rather than assumed.
    ///
    /// `None` until [`CaseId::EraseYieldsOneRepeatedByte`] has run, which is why that case
    /// runs before every case that programs anything.
    erased: Option<u8>,
}

impl<S: StableStorage> Run<'_, S> {
    fn everything(&mut self) {
        self.geometry_is_stable();
        self.misaligned_read_is_refused();
        self.misaligned_program_is_refused();
        self.misaligned_erase_is_refused();
        self.read_past_capacity_is_refused();
        self.program_past_capacity_is_refused();
        self.erase_past_capacity_is_refused();
        self.refused_program_touches_no_media();
        self.refused_erase_touches_no_media();
        self.erase_yields_one_repeated_byte();
        self.program_round_trips_through_read();
        self.program_leaves_the_rest_of_the_block_alone();
        self.erase_leaves_the_neighbouring_block_alone();
        self.erase_is_idempotent();
        self.zero_length_operations_are_legal_and_change_nothing();
        self.partial_reads_agree_with_the_whole();
        self.barrier_succeeds();
        self.barrier_changes_no_media();
        self.repeated_barriers_are_legal();
    }

    // ---- the block layout every case works in -----------------------------------------

    /// The first erase block of the region: where a case programs.
    const fn block_a(&self) -> u32 {
        self.region.offset()
    }

    /// The second erase block: the neighbour a confined erase must leave alone.
    const fn block_b(&self) -> u32 {
        self.region.offset() + self.region.geometry().erase_size()
    }

    const fn erase_size(&self) -> u32 {
        self.region.geometry().erase_size()
    }

    const fn program_size(&self) -> u32 {
        self.region.geometry().program_size()
    }

    const fn read_size(&self) -> u32 {
        self.region.geometry().read_size()
    }

    fn record(&mut self, case: CaseId, outcome: Outcome) {
        self.report.record(case, outcome);
    }

    // ---- the primitives the cases are written in ---------------------------------------

    /// Reads `len` bytes at `offset` into the front of the buffer, returning them.
    ///
    /// `None` if the driver refused a read the geometry permits, which every caller turns
    /// into [`Failure::LegalOperationRefused`].
    fn read_into(&mut self, offset: u32, len: usize, at: usize) -> Option<()> {
        let slot = self.buffer.get_mut(at..at.checked_add(len)?)?;
        self.storage.read(offset, slot).ok()
    }

    /// The bytes the last [`Run::read_into`] left at `at`.
    fn bytes(&self, at: usize, len: usize) -> Option<&[u8]> {
        self.buffer.get(at..at.checked_add(len)?)
    }

    /// Erases one block, reporting a refusal as a failure of `case`.
    fn erase_block(&mut self, case: CaseId, offset: u32) -> bool {
        let len = self.erase_size();
        if self.storage.erase(offset, len).is_err() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return false;
        }
        true
    }

    /// Whether every byte of `len` bytes at `offset` reads as `wanted`.
    ///
    /// `None` if a legal read was refused.
    fn all_bytes_are(&mut self, offset: u32, len: u32, wanted: u8) -> Option<bool> {
        let step = self.unit;
        let mut seen = 0_u32;
        while seen < len {
            let chunk = core::cmp::min(step, usize::try_from(len - seen).ok()?);
            self.read_into(offset.checked_add(seen)?, chunk, 0)?;
            if self.bytes(0, chunk)?.iter().any(|byte| *byte != wanted) {
                return Some(false);
            }
            seen = seen.checked_add(u32::try_from(chunk).ok()?)?;
        }
        Some(true)
    }

    /// The pattern this run programs: a subset of the erased byte's bits, so that it is
    /// programmable from erased on media of either polarity.
    ///
    /// The index is taken modulo 256, so a program unit wider than that repeats. Nothing
    /// here needs the pattern to be injective — it needs it to differ from the erased byte,
    /// which [`Run::pattern_bites`] is what checks.
    fn pattern(index: usize, erased: u8) -> u8 {
        erased & (0xA5_u8 ^ u8::try_from(index & 0xFF).unwrap_or(0))
    }

    /// Whether the pattern would change anything on erased media.
    fn pattern_bites(&self, erased: u8) -> bool {
        (0..self.unit).any(|index| Self::pattern(index, erased) != erased)
    }

    // ---- validated-before-media --------------------------------------------------------

    fn geometry_is_stable(&mut self) {
        let first = self.storage.geometry();
        let second = self.storage.geometry();
        let third = self.storage.geometry();
        let outcome = if first == second && second == third && first == self.region.geometry() {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::GeometryIsNotStable)
        };
        self.record(CaseId::GeometryIsStable, outcome);
    }

    fn misaligned_read_is_refused(&mut self) {
        let unit = self.read_size();
        if unit == 1 {
            self.record(
                CaseId::MisalignedReadIsRefused,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        let step = usize::try_from(unit >> 1).unwrap_or(1);
        let whole = usize::try_from(unit).unwrap_or(1);
        let base = self.block_a();

        let offset_refused = match self.buffer.get_mut(..whole) {
            Some(slot) => self.storage.read(base + (unit >> 1), slot).is_err(),
            None => false,
        };
        let length_refused = match self.buffer.get_mut(..whole + step) {
            Some(slot) => self.storage.read(base, slot).is_err(),
            None => false,
        };
        let outcome = if offset_refused && length_refused {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::MisalignedReadIsRefused, outcome);
    }

    fn misaligned_program_is_refused(&mut self) {
        let unit = self.program_size();
        if unit == 1 {
            self.record(
                CaseId::MisalignedProgramIsRefused,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        let half = usize::try_from(unit >> 1).unwrap_or(1);
        let base = self.block_a();
        // All ones: on NOR this clears nothing, so a driver that wrongly accepted the
        // operation is still caught by `refused_program_touches_no_media` rather than by
        // this case having quietly damaged the block the next case needs.
        if let Some(slot) = self.buffer.get_mut(..self.unit + half) {
            slot.fill(0xFF);
        }

        let offset_refused = match self.buffer.get(..self.unit) {
            Some(src) => self.storage.program(base + (unit >> 1), src).is_err(),
            None => false,
        };
        let length_refused = match self.buffer.get(..self.unit + half) {
            Some(src) => self.storage.program(base, src).is_err(),
            None => false,
        };
        let outcome = if offset_refused && length_refused {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::MisalignedProgramIsRefused, outcome);
    }

    fn misaligned_erase_is_refused(&mut self) {
        let unit = self.erase_size();
        if unit == 1 {
            self.record(
                CaseId::MisalignedEraseIsRefused,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        let base = self.block_a();
        let offset_refused = self.storage.erase(base + (unit >> 1), unit).is_err();
        let length_refused = self.storage.erase(base, unit + (unit >> 1)).is_err();
        let outcome = if offset_refused && length_refused {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::MisalignedEraseIsRefused, outcome);
    }

    fn read_past_capacity_is_refused(&mut self) {
        let capacity = self.region.geometry().capacity();
        let unit = self.read_size();
        let whole = usize::try_from(unit).unwrap_or(1);

        let past = match self.buffer.get_mut(..whole) {
            Some(slot) => self.storage.read(capacity, slot).is_err(),
            None => false,
        };
        // The end is `u32::MAX + 1`, which a driver computing `offset + len` in 32 bits
        // wraps to zero and then finds comfortably in bounds.
        let overflowing = match self.buffer.get_mut(..whole) {
            Some(slot) => self.storage.read(u32::MAX - unit + 1, slot).is_err(),
            None => false,
        };
        let outcome = if past && overflowing {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::ReadPastCapacityIsRefused, outcome);
    }

    fn program_past_capacity_is_refused(&mut self) {
        let capacity = self.region.geometry().capacity();
        let unit = self.program_size();
        if let Some(slot) = self.buffer.get_mut(..self.unit) {
            slot.fill(0xFF);
        }

        let past = match self.buffer.get(..self.unit) {
            Some(src) => self.storage.program(capacity, src).is_err(),
            None => false,
        };
        let overflowing = match self.buffer.get(..self.unit) {
            Some(src) => self.storage.program(u32::MAX - unit + 1, src).is_err(),
            None => false,
        };
        let outcome = if past && overflowing {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::ProgramPastCapacityIsRefused, outcome);
    }

    fn erase_past_capacity_is_refused(&mut self) {
        let capacity = self.region.geometry().capacity();
        let unit = self.erase_size();

        let past = self.storage.erase(capacity, unit).is_err();
        let straddling = self.storage.erase(capacity - unit, unit + unit).is_err();
        let overflowing = self.storage.erase(u32::MAX - unit + 1, unit).is_err();
        let outcome = if past && straddling && overflowing {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::ErasePastCapacityIsRefused, outcome);
    }

    fn refused_program_touches_no_media(&mut self) {
        let case = CaseId::RefusedProgramTouchesNoMedia;
        let unit = self.program_size();
        if unit == 1 {
            // With a one-byte program unit every in-bounds program is legal, so there is no
            // illegal operation that names bytes inside the region to observe. Refusing one
            // that names bytes outside it would mean asking a broken driver to damage the
            // media the caller said not to touch.
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        let half = usize::try_from(unit >> 1).unwrap_or(1);
        let base = self.block_a();
        let before = 0;
        let source = self.unit;
        let after = self.unit + self.unit + half;

        if self.read_into(base, self.unit, before).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        // A source that clears every bit the media currently holds, so an applied write
        // cannot be mistaken for a no-op.
        for index in 0..self.unit + half {
            let seen = self
                .bytes(before, self.unit)
                .and_then(|held| held.get(index % self.unit).copied())
                .unwrap_or(0xFF);
            if let Some(cell) = self.buffer.get_mut(source + index) {
                *cell = !seen;
            }
        }
        let accepted = match self.buffer.get(source..source + self.unit + half) {
            Some(src) => self.storage.program(base, src).is_ok(),
            None => false,
        };
        if accepted {
            self.record(case, Outcome::Failed(Failure::IllegalOperationAccepted));
            return;
        }
        if self.read_into(base, self.unit, after).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let unchanged = self.bytes(before, self.unit) == self.bytes(after, self.unit);
        let outcome = if unchanged {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::RefusedOperationTouchedMedia)
        };
        self.record(case, outcome);
    }

    fn refused_erase_touches_no_media(&mut self) {
        let case = CaseId::RefusedEraseTouchesNoMedia;
        let unit = self.erase_size();
        if unit == 1 {
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        let base = self.block_a();
        let before = 0;
        let source = self.unit;
        let after = self.unit + self.unit;

        if self.read_into(base, self.unit, before).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        // Put something in the block that an erase would visibly remove.
        for index in 0..self.unit {
            let seen = self
                .bytes(before, self.unit)
                .and_then(|held| held.get(index).copied())
                .unwrap_or(0xFF);
            if let Some(cell) = self.buffer.get_mut(source + index) {
                *cell = !seen;
            }
        }
        let programmed = match self.buffer.get(source..source + self.unit) {
            Some(src) => self.storage.program(base, src).is_ok(),
            None => false,
        };
        if !programmed {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        if self.read_into(base, self.unit, before).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }

        // Misaligned by half an erase block, and anchored so that a driver which applied it
        // anyway would clear the block just programmed.
        let accepted = self.storage.erase(base + (unit >> 1), unit).is_ok();
        if accepted {
            self.record(case, Outcome::Failed(Failure::IllegalOperationAccepted));
            return;
        }
        if self.read_into(base, self.unit, after).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let unchanged = self.bytes(before, self.unit) == self.bytes(after, self.unit);
        let outcome = if unchanged {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::RefusedOperationTouchedMedia)
        };
        self.record(case, outcome);
    }

    // ---- operations-act-on-what-they-name ----------------------------------------------

    fn erase_yields_one_repeated_byte(&mut self) {
        let case = CaseId::EraseYieldsOneRepeatedByte;
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        let base = self.block_a();
        if self.read_into(base, self.unit, 0).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let Some(first) = self
            .bytes(0, self.unit)
            .and_then(|held| held.first().copied())
        else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let block = self.erase_size();
        match self.all_bytes_are(base, block, first) {
            Some(true) => {
                self.erased = Some(first);
                self.record(case, Outcome::Passed);
            }
            Some(false) => self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion)),
            None => self.record(case, Outcome::Failed(Failure::LegalOperationRefused)),
        }
    }

    /// The erased byte, or the failure to record when it was never learned.
    fn erased_or_report(&mut self, case: CaseId) -> Option<u8> {
        match self.erased {
            Some(erased) if self.pattern_bites(erased) => Some(erased),
            Some(_) => {
                self.record(
                    case,
                    Outcome::NotApplicable(NotApplicable::TheErasedStateHasNoProgrammableBits),
                );
                None
            }
            None => {
                self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion));
                None
            }
        }
    }

    /// Erases block A and programs one unit of the run's pattern at its start.
    ///
    /// `Some(())` when the media now holds the pattern; the case has already been recorded
    /// when it is `None`.
    fn program_a_unit(&mut self, case: CaseId, erased: u8) -> Option<()> {
        if !self.erase_block(case, self.block_a()) {
            return None;
        }
        for index in 0..self.unit {
            let wanted = Self::pattern(index, erased);
            if let Some(cell) = self.buffer.get_mut(index) {
                *cell = wanted;
            }
        }
        let base = self.block_a();
        let programmed = match self.buffer.get(..self.unit) {
            Some(src) => self.storage.program(base, src).is_ok(),
            None => false,
        };
        if programmed {
            Some(())
        } else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            None
        }
    }

    /// Whether the unit at `offset` reads back as the run's pattern.
    fn unit_holds_the_pattern(&mut self, offset: u32, erased: u8) -> Option<bool> {
        self.read_into(offset, self.unit, self.unit)?;
        Some((0..self.unit).all(|index| {
            self.bytes(self.unit, self.unit)
                .and_then(|held| held.get(index).copied())
                == Some(Self::pattern(index, erased))
        }))
    }

    fn program_round_trips_through_read(&mut self) {
        let case = CaseId::ProgramRoundTripsThroughRead;
        let Some(erased) = self.erased_or_report(case) else {
            return;
        };
        if self.program_a_unit(case, erased).is_none() {
            return;
        }
        let base = self.block_a();
        let outcome = match self.unit_holds_the_pattern(base, erased) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::ReadBackDiffers),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn program_leaves_the_rest_of_the_block_alone(&mut self) {
        let case = CaseId::ProgramLeavesTheRestOfTheBlockAlone;
        let block = self.erase_size();
        let unit = self.program_size();
        if block == unit {
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheBlockIsOneProgramUnit),
            );
            return;
        }
        let Some(erased) = self.erased_or_report(case) else {
            return;
        };
        if self.program_a_unit(case, erased).is_none() {
            return;
        }
        let rest = self.block_a() + unit;
        let outcome = match self.all_bytes_are(rest, block - unit, erased) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn erase_leaves_the_neighbouring_block_alone(&mut self) {
        let case = CaseId::EraseLeavesTheNeighbouringBlockAlone;
        let Some(erased) = self.erased_or_report(case) else {
            return;
        };
        if !self.erase_block(case, self.block_b()) {
            return;
        }
        // Program the neighbour, then erase the block next to it.
        for index in 0..self.unit {
            let wanted = Self::pattern(index, erased);
            if let Some(cell) = self.buffer.get_mut(index) {
                *cell = wanted;
            }
        }
        let neighbour = self.block_b();
        let programmed = match self.buffer.get(..self.unit) {
            Some(src) => self.storage.program(neighbour, src).is_ok(),
            None => false,
        };
        if !programmed {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        let outcome = match self.unit_holds_the_pattern(neighbour, erased) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn erase_is_idempotent(&mut self) {
        let case = CaseId::EraseIsIdempotent;
        let Some(erased) = self.erased else {
            self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion));
            return;
        };
        let block = self.erase_size();
        let base = self.block_a();
        for _ in 0..2 {
            if !self.erase_block(case, base) {
                return;
            }
            match self.all_bytes_are(base, block, erased) {
                Some(true) => {}
                Some(false) => {
                    self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion));
                    return;
                }
                None => {
                    self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                    return;
                }
            }
        }
        self.record(case, Outcome::Passed);
    }

    fn zero_length_operations_are_legal_and_change_nothing(&mut self) {
        let case = CaseId::ZeroLengthOperationsAreLegalAndChangeNothing;
        let Some(erased) = self.erased else {
            self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion));
            return;
        };
        if self.program_a_unit(case, erased).is_none() {
            return;
        }
        let base = self.block_a();

        // A caller with nothing to write is not a caller with a bug, and a driver that
        // refused would push the empty case into every call site above it. The capacity is
        // an aligned offset for all three units, so it is a legal empty operation too.
        let capacity = self.region.geometry().capacity();
        let legal = self.storage.read(base, &mut []).is_ok()
            && self.storage.program(base, &[]).is_ok()
            && self.storage.erase(base, 0).is_ok()
            && self.storage.read(capacity, &mut []).is_ok()
            && self.storage.program(capacity, &[]).is_ok()
            && self.storage.erase(capacity, 0).is_ok();
        if !legal {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let outcome = match self.unit_holds_the_pattern(base, erased) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn partial_reads_agree_with_the_whole(&mut self) {
        let case = CaseId::PartialReadsAgreeWithTheWhole;
        let Some(erased) = self.erased else {
            self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion));
            return;
        };
        if self.program_a_unit(case, erased).is_none() {
            return;
        }
        let base = self.block_a();
        if self.read_into(base, self.unit, self.unit).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let step = usize::try_from(self.read_size()).unwrap_or(1);
        let mut at = 0_usize;
        while at < self.unit {
            let Ok(offset) = u32::try_from(at) else {
                self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                return;
            };
            if self
                .read_into(base + offset, step, self.unit + self.unit)
                .is_none()
            {
                self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                return;
            }
            let piece = self.bytes(self.unit + self.unit, step);
            let whole = self.bytes(self.unit + at, step);
            if piece != whole {
                self.record(case, Outcome::Failed(Failure::ReadBackDiffers));
                return;
            }
            at += step;
        }
        self.record(case, Outcome::Passed);
    }

    fn barrier_succeeds(&mut self) {
        let outcome = if self.storage.barrier().is_ok() {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::LegalOperationRefused)
        };
        self.record(CaseId::BarrierSucceeds, outcome);
    }

    fn barrier_changes_no_media(&mut self) {
        let case = CaseId::BarrierChangesNoMedia;
        let Some(erased) = self.erased else {
            self.record(case, Outcome::Failed(Failure::EraseDidNotClearTheRegion));
            return;
        };
        // The third block is where a barrier that scribbled somewhere *else* would show
        // up: nothing programs it, so it must still read erased when the barrier is done.
        let third = self.region.offset() + self.erase_size() + self.erase_size();
        if !self.erase_block(case, third) {
            return;
        }
        if self.program_a_unit(case, erased).is_none() {
            return;
        }
        let base = self.block_a();
        if self.read_into(base, self.unit, self.unit).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        if self.storage.barrier().is_err() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        if self
            .read_into(base, self.unit, self.unit + self.unit)
            .is_none()
        {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let unchanged =
            self.bytes(self.unit, self.unit) == self.bytes(self.unit + self.unit, self.unit);
        let block = self.erase_size();
        let Some(elsewhere) = self.all_bytes_are(third, block, erased) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if unchanged && elsewhere {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
        };
        self.record(case, outcome);
    }

    fn repeated_barriers_are_legal(&mut self) {
        let outcome = if self.storage.barrier().is_ok() && self.storage.barrier().is_ok() {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::LegalOperationRefused)
        };
        self.record(CaseId::RepeatedBarriersAreLegal, outcome);
    }
}
