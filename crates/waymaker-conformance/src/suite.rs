//! The in-process conformance run.
//!
//! Twenty cases, each one an observation an adapter either survives or does not. Two of
//! design document §12's clauses are what they speak for: `validated-before-media`, which
//! is about what an adapter *refuses*, and `operations-act-on-what-they-name`, which is
//! about what it does when it agrees.
//!
//! # What a run costs the device
//!
//! Three erase blocks, erased and reprogrammed several times, inside the [`Region`] the
//! caller named. No case names a byte outside it — not even in an operation it expects to be
//! refused, which is the part that matters: an adapter that wrongly *accepted* one could then
//! only damage media the caller declared expendable. Where no such operation exists — the
//! mutations that straddle the end of the device, on a region that is not at the end of the
//! device — the case is [`NotApplicable`] rather than issued somewhere unsafe.
//!
//! What that cannot promise is containment of an adapter whose *legal* operations run wild.
//! An erase of one block of the region is an operation the caller authorised, and a driver
//! that answers it by erasing the whole chip — or a `barrier` that scribbles — is caught
//! rather than contained.
//!
//! # Why erased is `0xFF`
//!
//! Because that is what the contract above this suite is written against. `embedded-storage`
//! says an erased NOR range "will contain all 1s afterwards"; design document §09's frame
//! reads a stale tail as `0xFF`; `waymaker-fault` models media that starts at `0xFF` and can
//! only clear bits. A suite that tried to be polarity-agnostic would have to learn the
//! erased byte from the adapter under test, and an adapter whose erase does nothing on media
//! that happens to read `0x00` would teach it that `0x00` is erased and that nothing is
//! programmable — which is how a broken driver talks a suite out of testing it. [`ERASED`]
//! is a constant, and an erase that does not produce it is a failure.
//!
//! # Why the caller supplies the buffer
//!
//! For the reason `waymaker_core::ReplayCursor` is pumped by its caller
//! ([ADR 0008](https://github.com/madmax983/waymaker/blob/main/docs/adr/0008-the-replay-cursor-is-pumped-by-its-caller.md)):
//! the page size a device wants is the device's business, and a suite with an internal
//! `[u8; N]` either refuses a 256-byte-page SPI part or charges every 4-byte-page internal
//! flash for one. Two program units is what the widest case needs, and
//! [`SuiteError::BufferTooSmall`] is what a caller who supplied less is told.
//!
//! Nothing here holds a copy of a whole erase block: what media *should* say is computed
//! from the run's own pattern rather than photographed beforehand, so a case can check a
//! region far larger than the buffer, one chunk at a time.

use waymaker_flash::storage::StableStorage;

use crate::case::{CaseId, Failure, NotApplicable, Outcome, Report};
use crate::region::Region;

/// The byte an erased cell reads as.
///
/// See the module documentation for why this is a constant rather than something the suite
/// learns from the adapter it is testing.
pub const ERASED: u8 = 0xFF;

/// How many program units of scratch a run needs.
///
/// Two: a source and the read-back of it, which is the widest any case holds at once.
pub const REQUIRED_BUFFER_UNITS: u32 = 2;

/// Why a conformance run could not start.
///
/// Distinct from a [`Failure`]: a failure means the adapter is wrong, and one of these
/// means the run never happened. A suite that reported "no failures" for a run it could not
/// start would be the exact reverse of what this crate is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
            Self::BufferTooSmall => "the buffer is smaller than two program units",
        }
    }
}

impl core::fmt::Display for SuiteError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for SuiteError {}

/// One byte of the pattern this suite programs.
///
/// Two properties, and both are load-bearing. It always has a bit clear, so it is
/// programmable from [`ERASED`] and is never equal to it — a pattern that happened to be
/// `0xFF` would make every round-trip case pass against an adapter that ignores programs.
/// And it varies with the index, so an adapter that writes the right number of bytes with
/// the wrong contents is still caught.
#[must_use]
pub fn pattern(index: usize) -> u8 {
    let position = u32::try_from(index % 8).unwrap_or(0);
    let mixed = 0xA5_u8 ^ u8::try_from(index & 0xFF).unwrap_or(0);
    mixed & !(1_u8 << position)
}

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
        self.mutation_straddling_the_capacity_is_refused();
        self.refused_program_touches_no_media();
        self.refused_erase_touches_no_media();
        self.erase_yields_the_erased_byte();
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

    /// The third erase block: what a barrier that scribbled elsewhere would show up in.
    const fn block_c(&self) -> u32 {
        self.region.offset()
            + self.region.geometry().erase_size()
            + self.region.geometry().erase_size()
    }

    const fn capacity(&self) -> u32 {
        self.region.geometry().capacity()
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

    /// Reads `len` bytes at `offset` into the buffer at `at`.
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

    /// Whether `len` bytes at `offset` read as `expected(position)`, position by position.
    ///
    /// Chunked through the caller's buffer, so a case can check an erase block far wider
    /// than the buffer without holding a copy of it. `None` if a legal read was refused.
    fn media_matches(
        &mut self,
        offset: u32,
        len: u32,
        expected: impl Fn(u32) -> u8,
    ) -> Option<bool> {
        let step = self.unit;
        let mut seen = 0_u32;
        while seen < len {
            let chunk = core::cmp::min(step, usize::try_from(len - seen).ok()?);
            self.read_into(offset.checked_add(seen)?, chunk, 0)?;
            let held = self.bytes(0, chunk)?;
            for (index, byte) in held.iter().enumerate() {
                if *byte != expected(seen.checked_add(u32::try_from(index).ok()?)?) {
                    return Some(false);
                }
            }
            seen = seen.checked_add(u32::try_from(chunk).ok()?)?;
        }
        Some(true)
    }

    /// Whether `len` bytes at `offset` are all erased.
    fn media_is_erased(&mut self, offset: u32, len: u32) -> Option<bool> {
        self.media_matches(offset, len, |_| ERASED)
    }

    /// Whether the block at `offset` holds one unit of the pattern and is erased after it.
    fn block_holds_the_pattern(&mut self, offset: u32) -> Option<bool> {
        let unit = self.program_size();
        let block = self.erase_size();
        Some(
            self.media_matches(offset, unit, |position| {
                pattern(usize::try_from(position).unwrap_or(0))
            })? && self.media_is_erased(offset.checked_add(unit)?, block.checked_sub(unit)?)?,
        )
    }

    /// Fills the front of the buffer with `len` bytes of `byte`.
    fn fill_source(&mut self, len: usize, byte: u8) {
        if let Some(slot) = self.buffer.get_mut(..len) {
            slot.fill(byte);
        }
    }

    /// Fills the front of the buffer with one program unit of the run's pattern.
    fn fill_pattern(&mut self) {
        for index in 0..self.unit {
            let wanted = pattern(index);
            if let Some(cell) = self.buffer.get_mut(index) {
                *cell = wanted;
            }
        }
    }

    /// Programs whatever is in `buffer[..len]` at `offset`.
    fn program_source(&mut self, offset: u32, len: usize) -> bool {
        match self.buffer.get(..len) {
            Some(source) => self.storage.program(offset, source).is_ok(),
            None => false,
        }
    }

    /// Erases the block at `offset` and programs one unit of the pattern at its start.
    ///
    /// `false` when the adapter refused either; the case has already been recorded.
    fn program_a_unit(&mut self, case: CaseId, offset: u32) -> bool {
        if !self.erase_block(case, offset) {
            return false;
        }
        self.fill_pattern();
        if self.program_source(offset, self.unit) {
            true
        } else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            false
        }
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
        let half = usize::try_from(unit >> 1).unwrap_or(1);
        let whole = usize::try_from(unit).unwrap_or(1);
        let base = self.block_a();

        let offset_refused = match self.buffer.get_mut(..whole) {
            Some(slot) => self.storage.read(base + (unit >> 1), slot).is_err(),
            None => false,
        };
        let length_refused = match self.buffer.get_mut(..whole + half) {
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
        // All ones: on erased media this clears nothing, so an adapter that wrongly accepted
        // the operation has not damaged the block the next case needs. Whether it *did*
        // accept it is what this case reports, and whether a refusal touched media is
        // `RefusedProgramTouchesNoMedia`'s question rather than this one's.
        self.fill_source(self.unit + half, ERASED);

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
        let capacity = self.capacity();
        let unit = self.read_size();
        let whole = usize::try_from(unit).unwrap_or(1);

        let past = match self.buffer.get_mut(..whole) {
            Some(slot) => self.storage.read(capacity, slot).is_err(),
            None => false,
        };
        // The end is `u32::MAX + 1`, which an adapter computing `offset + len` in 32 bits
        // wraps to zero and then finds comfortably in bounds. Safe because `unit` is a power
        // of two no larger than the capacity, so the subtraction cannot underflow.
        let overflowing = match self.buffer.get_mut(..whole) {
            Some(slot) => self.storage.read(u32::MAX - unit + 1, slot).is_err(),
            None => false,
        };
        // Starts in bounds and ends out of them, which is the "validate the start and forget
        // the end" bug. Safe to issue anywhere: a read mutates nothing, so an adapter that
        // wrongly accepts it damages no media inside the region or outside it.
        let straddling = match self.buffer.get_mut(..whole + whole) {
            Some(slot) => self.storage.read(capacity - unit, slot).is_err(),
            None => false,
        };
        let outcome = if past && overflowing && straddling {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::ReadPastCapacityIsRefused, outcome);
    }

    fn program_past_capacity_is_refused(&mut self) {
        let capacity = self.capacity();
        let unit = self.program_size();
        self.fill_source(self.unit, ERASED);

        // Both probes start at or past the capacity, so the bytes they name that are inside
        // the device number zero: an adapter that wrongly accepted one has nothing in range
        // to damage. The probe that *starts* in bounds is
        // `MutationStraddlingTheCapacityIsRefused`, which is only issued when the region
        // reaches the end of the device.
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
        let capacity = self.capacity();
        let unit = self.erase_size();

        let past = self.storage.erase(capacity, unit).is_err();
        let overflowing = self.storage.erase(u32::MAX - unit + 1, unit).is_err();
        let outcome = if past && overflowing {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::IllegalOperationAccepted)
        };
        self.record(CaseId::ErasePastCapacityIsRefused, outcome);
    }

    fn mutation_straddling_the_capacity_is_refused(&mut self) {
        let case = CaseId::MutationStraddlingTheCapacityIsRefused;
        let capacity = self.capacity();
        if self.region.end() != capacity {
            // The only mutation that starts in bounds and ends out of them begins in the
            // device's last erase block. When that block is not the caller's, issuing one
            // would ask an adapter that forgot to check the end to destroy the media the
            // caller said not to touch — which is the failure this whole suite is careful
            // not to cause. Reported rather than skipped, so a run on a mid-device region
            // says which question it could not ask.
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheRegionDoesNotEndAtTheCapacity),
            );
            return;
        }
        let block = self.erase_size();
        let unit = self.program_size();
        let last = capacity - block;
        if !self.program_a_unit(case, last) {
            return;
        }

        // A source of all zeros, so an adapter that applied the valid prefix would visibly
        // clear the unit just programmed.
        self.fill_source(self.unit + self.unit, 0x00);
        let program_refused = !self.program_source(capacity - unit, self.unit + self.unit);
        let erase_refused = self.storage.erase(last, block + block).is_err();
        if !(program_refused && erase_refused) {
            self.record(case, Outcome::Failed(Failure::IllegalOperationAccepted));
            return;
        }
        let outcome = match self.block_holds_the_pattern(last) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::RefusedOperationTouchedMedia),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn refused_program_touches_no_media(&mut self) {
        let case = CaseId::RefusedProgramTouchesNoMedia;
        let unit = self.program_size();
        if unit == 1 {
            // With a one-byte program unit every in-bounds program is legal, so there is no
            // illegal operation that names bytes inside the region to observe. Refusing one
            // that named bytes outside it would mean asking a broken adapter to damage the
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

        // A source of all zeros over erased media, so any byte of it that reached media is
        // visible. The whole block is checked afterwards rather than only the bytes the
        // operation named: an adapter that wrote the valid prefix and an adapter that
        // scribbled past it are the same bug, and the second is the one a narrower window
        // would miss.
        self.fill_source(self.unit + half, 0x00);
        if self.program_source(base, self.unit + half) {
            self.record(case, Outcome::Failed(Failure::IllegalOperationAccepted));
            return;
        }
        let block = self.erase_size();
        let outcome = match self.media_is_erased(base, block) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::RefusedOperationTouchedMedia),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn refused_erase_touches_no_media(&mut self) {
        let case = CaseId::RefusedEraseTouchesNoMedia;
        let block = self.erase_size();
        if block == 1 {
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        // A witness in *both* blocks, because the two misalignments an erase can have reach
        // different media. A refused erase is only observable where the erase would have
        // landed: with a 64-byte block and a 4-byte unit, `erase(base + 32, 64)` never
        // touches `base..base + 4`, so a witness in the first block alone leaves an adapter
        // that performs the erase and then refuses looking spotless.
        if !self.program_a_unit(case, self.block_b()) {
            return;
        }
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        let base = self.block_a();
        let neighbour = self.block_b();

        // Misaligned in length, starting at the first witness; and misaligned in offset,
        // reaching across into the second. Between them every byte either erase would have
        // cleared is a byte one of the two witnesses occupies.
        for (offset, len) in [(base, block + (block >> 1)), (base + (block >> 1), block)] {
            if self.storage.erase(offset, len).is_ok() {
                self.record(case, Outcome::Failed(Failure::IllegalOperationAccepted));
                return;
            }
            let (Some(first), Some(second)) = (
                self.block_holds_the_pattern(base),
                self.block_holds_the_pattern(neighbour),
            ) else {
                self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                return;
            };
            if !(first && second) {
                self.record(case, Outcome::Failed(Failure::RefusedOperationTouchedMedia));
                return;
            }
        }
        self.record(case, Outcome::Passed);
    }

    // ---- operations-act-on-what-they-name ----------------------------------------------

    fn erase_yields_the_erased_byte(&mut self) {
        // Programmed first, on purpose. An erase that does nothing at all leaves a block
        // reading whatever it read before, and a case that only erased an already-erased
        // block would call that a pass — which is exactly how an adapter whose `erase` is
        // `Ok(())` and nothing else talks a suite out of testing it.
        let case = CaseId::EraseYieldsTheErasedByte;
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        let base = self.block_a();
        let block = self.erase_size();
        if !self.erase_block(case, base) {
            return;
        }
        let outcome = match self.media_is_erased(base, block) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::EraseDidNotClearTheRegion),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn program_round_trips_through_read(&mut self) {
        let case = CaseId::ProgramRoundTripsThroughRead;
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        let base = self.block_a();
        let unit = self.program_size();
        let outcome = match self.media_matches(base, unit, |position| {
            pattern(usize::try_from(position).unwrap_or(0))
        }) {
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
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        let rest = self.block_a() + unit;
        let outcome = match self.media_is_erased(rest, block - unit) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn erase_leaves_the_neighbouring_block_alone(&mut self) {
        let case = CaseId::EraseLeavesTheNeighbouringBlockAlone;
        if !self.program_a_unit(case, self.block_b()) {
            return;
        }
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        let neighbour = self.block_b();
        let outcome = match self.block_holds_the_pattern(neighbour) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn erase_is_idempotent(&mut self) {
        let case = CaseId::EraseIsIdempotent;
        let block = self.erase_size();
        let base = self.block_a();
        for _ in 0..2 {
            if !self.erase_block(case, base) {
                return;
            }
            match self.media_is_erased(base, block) {
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
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        let base = self.block_a();

        // A caller with nothing to write is not a caller with a bug, and an adapter that
        // refused would push the empty case into every call site above it. The capacity is
        // an aligned offset for all three units, so it is a legal empty operation too.
        let capacity = self.capacity();
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
        let outcome = match self.block_holds_the_pattern(base) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }

    fn partial_reads_agree_with_the_whole(&mut self) {
        let case = CaseId::PartialReadsAgreeWithTheWhole;
        if self.read_size() == self.program_size() {
            // One read of the unit and one read of the whole unit are the same read, so
            // there is nothing partial to disagree. Reported rather than passed, because a
            // case that compared a value with itself would be a hollow green row.
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheReadUnitIsTheProgramUnit),
            );
            return;
        }
        if !self.program_a_unit(case, self.block_a()) {
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
            if self.read_into(base + offset, step, 0).is_none() {
                self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                return;
            }
            if self.bytes(0, step) != self.bytes(self.unit + at, step) {
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
        // The third block is where a barrier that scribbled somewhere *else* would show
        // up: nothing programs it, so it must still read erased when the barrier is done.
        let third = self.block_c();
        if !self.erase_block(case, third) {
            return;
        }
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        if self.storage.barrier().is_err() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let base = self.block_a();
        let Some(unchanged) = self.block_holds_the_pattern(base) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let block = self.erase_size();
        let Some(elsewhere) = self.media_is_erased(third, block) else {
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

#[cfg(test)]
mod tests {
    use super::{ERASED, pattern};

    #[test]
    fn every_pattern_byte_is_programmable_from_erased_and_is_not_the_erased_byte() {
        // The two properties every round-trip case rests on. A pattern byte equal to `0xFF`
        // would make "program it and read it back" pass against an adapter that ignores
        // programs, and one with a bit `ERASED` does not hold could not be programmed at
        // all. Swept over more indices than any program unit this suite will meet.
        for index in 0..4096_usize {
            let byte = pattern(index);
            assert_eq!(
                byte & !ERASED,
                0,
                "pattern({index}) sets a bit erase clears"
            );
            assert_ne!(byte, ERASED, "pattern({index}) is the erased byte");
        }
    }

    #[test]
    fn the_pattern_varies_within_a_program_unit() {
        // An adapter that programs one byte over and over is caught only if the bytes it
        // should have written differ from each other.
        let first = pattern(0);
        assert!(
            (1..8).any(|index| pattern(index) != first),
            "the pattern is constant across a program unit"
        );
    }

    #[test]
    fn the_pattern_repeats_every_two_hundred_and_fifty_six_bytes() {
        // A program unit wider than 256 bytes repeats, which is stated rather than left to
        // be discovered: nothing here needs the pattern to be injective.
        for index in 0..512_usize {
            assert_eq!(pattern(index), pattern(index + 256));
        }
    }
}
