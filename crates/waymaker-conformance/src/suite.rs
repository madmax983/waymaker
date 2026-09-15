//! The in-process conformance run.
//!
//! Twenty cases, each one an observation an adapter either survives or does not. Two of
//! design document §12's clauses are what they speak for: `validated-before-media`, which
//! is about what an adapter *refuses*, and `operations-act-on-what-they-name`, which is
//! about what it does when it agrees.
//!
//! # What a run costs the device
//!
//! Four erase blocks, erased and reprogrammed several times, inside the [`Region`] the
//! caller named — and one erase-and-read pass over the whole region, which
//! [`CaseId::BarrierChangesNoMedia`] needs because "changes no media" is a claim about media
//! and not about the four blocks that happened to be convenient. A caller who wants a
//! cheaper run passes a smaller region; that is what naming one is for. No case names a byte
//! outside it — not even in an operation it expects to be
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

/// Whether every byte of `bytes` is [`ERASED`], a word at a time.
///
/// [`Run::media_is_erased`] is the suite's most-called primitive — nearly every case that
/// touches media ends in an erased-tail or erased-region check, frequently over a span the
/// width of an erase block — so a byte-at-a-time `iter().any(|&b| b != ERASED)` here pays a
/// bounds check and a compare per byte for an answer usable in `usize`-sized chunks. Same
/// technique `waymaker_flash::recovery::is_erased` uses for the same reason, reimplemented
/// rather than shared: that function is private to a different layer and reaching it from
/// here would be a `waymaker-conformance` dependency the module does not otherwise need for
/// one helper. Checked against the byte-at-a-time definition at every length and
/// single-byte-mutation position around a word boundary in this module's tests, so a
/// remainder handled short does not pass silently.
///
/// # Do not change the loop to `split_first_chunk`
///
/// This change looks like an improvement. It is not. Do not make it again.
///
/// `split_first_chunk::<WORD>()` removes the `TryFrom` check. It removes the
/// `chunks_exact` iterator too. It appears to need fewer operations per word.
///
/// A test shows the opposite result. On the `conformance` workload
/// (`cargo xtask profile`), the `split_first_chunk` form increases
/// engine-attributed instructions by 9.3% (41113 Ir to 44931 Ir). The
/// `journal`, `driver` and `facade` workloads do not change. They do not call
/// this function.
///
/// Here is the reason. This loop already compiles into a loop with three
/// instructions for each word. Compiler output shows this, not source code.
/// Use `objdump` on `target/profiling/xtask` to check the compiler output.
/// The `split_first_chunk` form compiles into a slower loop for this exact
/// case. See issue #152 for the full measurement.
fn slice_is_erased(bytes: &[u8]) -> bool {
    const WORD: usize = size_of::<usize>();
    let mut words = bytes.chunks_exact(WORD);
    let words_erased = words.by_ref().all(|word| {
        matches!(<[u8; WORD]>::try_from(word), Ok(word) if usize::from_ne_bytes(word) == usize::MAX)
    });
    words_erased && words.remainder().iter().all(|&byte| byte == ERASED)
}

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
        self.multi_unit_program_is_legal();
        self.multi_block_erase_is_legal();
        self.reading_changes_no_media();
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

    /// The fourth erase block: the far witness for a two-block bulk erase.
    const fn block_d(&self) -> u32 {
        self.region.offset()
            + self.region.geometry().erase_size()
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
    ///
    /// The chunk is the widest whole number of program units the buffer holds rather than
    /// one unit at a time: [`run`] refuses a buffer under [`REQUIRED_BUFFER_UNITS`] units, so
    /// this is always at least one, and a whole number of units is always a whole number of
    /// [`read_size`](Self::read_size)s too, `program_size` nesting inside it by construction
    /// (see [`Geometry::new`](waymaker_flash::storage::Geometry::new)). A caller with a
    /// 256-byte page and a 4-byte program unit was, before this, still reading and
    /// comparing four bytes at a time.
    fn media_matches(
        &mut self,
        offset: u32,
        len: u32,
        expected: impl Fn(u32) -> u8,
    ) -> Option<bool> {
        let step = self.buffer.len() / self.unit * self.unit;
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
    ///
    /// The same chunking [`Run::media_matches`] does, but without going through it: the
    /// expected byte here never depends on position, so there is no reason to pay
    /// `media_matches`'s per-byte closure call and checked position arithmetic
    /// (`seen.checked_add(u32::try_from(index)?)?`) for an answer that is `expected(_) ==
    /// ERASED` at every index. Checked against `media_matches(offset, len, |_| ERASED)` by
    /// an independent oracle in this module's tests, so a chunk-boundary difference
    /// between the two loops does not pass silently.
    fn media_is_erased(&mut self, offset: u32, len: u32) -> Option<bool> {
        let step = self.buffer.len() / self.unit * self.unit;
        let mut seen = 0_u32;
        while seen < len {
            let chunk = core::cmp::min(step, usize::try_from(len - seen).ok()?);
            self.read_into(offset.checked_add(seen)?, chunk, 0)?;
            let held = self.bytes(0, chunk)?;
            if !slice_is_erased(held) {
                return Some(false);
            }
            seen = seen.checked_add(u32::try_from(chunk).ok()?)?;
        }
        Some(true)
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
        self.fill_pattern_len(self.unit);
    }

    /// Fills the front of the buffer with `len` bytes of the run's pattern.
    fn fill_pattern_len(&mut self, len: usize) {
        for index in 0..len {
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

    /// Programs one unit of the run's pattern at `offset`, which need not be a block start.
    ///
    /// `false` when the adapter refused; the case has already been recorded.
    fn program_unit_at(&mut self, case: CaseId, offset: u32) -> bool {
        self.fill_pattern();
        if self.program_source(offset, self.unit) {
            true
        } else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            false
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
        let case = CaseId::MisalignedProgramIsRefused;
        let unit = self.program_size();
        if unit == 1 {
            self.record(
                case,
                Outcome::NotApplicable(NotApplicable::TheUnitIsOneByte),
            );
            return;
        }
        // A witness first, so a source that clears bits is observable: an all-erased source
        // damages nothing if wrongly accepted, which lets an adapter that validates the
        // length before media and the offset only after programming pass unnoticed.
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        // A witness in the following block too, erased rather than patterned: both illegal
        // probes name a range that reaches half a unit past `base + unit`, which is inside
        // the *next* block on a device whose erase block is a single program unit. An
        // adapter that rounds the misaligned offset up and programs there before refusing
        // corrupts media this case would otherwise never look at.
        if !self.erase_block(case, self.block_b()) {
            return;
        }
        let half = usize::try_from(unit >> 1).unwrap_or(1);
        let base = self.block_a();
        self.fill_source(self.unit + half, 0x00);

        let offset_refused = match self.buffer.get(..self.unit) {
            Some(src) => self.storage.program(base + (unit >> 1), src).is_err(),
            None => false,
        };
        let length_refused = match self.buffer.get(..self.unit + half) {
            Some(src) => self.storage.program(base, src).is_err(),
            None => false,
        };
        if !(offset_refused && length_refused) {
            self.record(case, Outcome::Failed(Failure::IllegalOperationAccepted));
            return;
        }
        let Some(witness_ok) = self.media_matches(base, unit, |position| {
            pattern(usize::try_from(position).unwrap_or(0))
        }) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        // The program unit right after the witnessed one: inside `block_a` on a device
        // whose block holds more than one unit, and `block_b`'s own first unit — already
        // erased by the call above — on one whose block is a single unit. Either way it is
        // erased before the probes run, and it is the unit either illegal range actually
        // reaches.
        let Some(neighbour_ok) = self.media_is_erased(base + unit, unit) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if witness_ok && neighbour_ok {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::RefusedOperationTouchedMedia)
        };
        self.record(case, outcome);
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
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        // A witness in the *following* block too, set up before the target is programmed.
        // When the block holds exactly two program units the target is the block's last
        // unit, so its own suffix is empty and has nothing left to catch a program that
        // spills forward past its own block — only a witness one block over can.
        if !self.program_a_unit(case, self.block_b()) {
            return;
        }
        // The *second* unit of the block, not the first. Every other legal program in this
        // suite is anchored at a block start, so with a first-unit target there is never a
        // preceding unit to watch and an adapter that also clears the unit before the one it
        // was given has nothing to be caught by. Both sides are checked below.
        let base = self.block_a();
        let target = base + unit;
        if !self.program_unit_at(case, target) {
            return;
        }
        let Some(prefix) = self.media_is_erased(base, unit) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let Some(written) = self.media_matches(target, unit, |position| {
            pattern(usize::try_from(position).unwrap_or(0))
        }) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let Some(suffix) = self.media_is_erased(target + unit, block - unit - unit) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let Some(neighbour) = self.block_holds_the_pattern(self.block_b()) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        if !written {
            self.record(case, Outcome::Failed(Failure::ReadBackDiffers));
            return;
        }
        let outcome = if prefix && suffix && neighbour {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
        };
        self.record(case, outcome);
    }

    fn erase_leaves_the_neighbouring_block_alone(&mut self) {
        let case = CaseId::EraseLeavesTheNeighbouringBlockAlone;
        // The *middle* block is the one erased, so both neighbours are watched. Erasing the
        // first block and watching the second sees an over-erase that runs forwards and not
        // one that runs backwards, and an adapter has no obligation to get those wrong in
        // the same direction.
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        if !self.program_a_unit(case, self.block_c()) {
            return;
        }
        if !self.erase_block(case, self.block_b()) {
            return;
        }
        let (before, after) = (self.block_a(), self.block_c());
        let (Some(earlier), Some(later)) = (
            self.block_holds_the_pattern(before),
            self.block_holds_the_pattern(after),
        ) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if earlier && later {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
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
        // A second witnessed offset, aligned and inside the region rather than at the
        // capacity: the capacity lies past the caller's region on anything but a
        // whole-device run, and asking a zero-length operation there invites an adapter that
        // clamps to damage media the caller did not authorise.
        if !self.program_a_unit(case, self.block_d()) {
            return;
        }
        // A witness immediately before `second` too: a broken adapter that answers a
        // zero-length operation there by clamping to the preceding block would otherwise
        // corrupt it with nothing here to notice, on any region — reaching the capacity or
        // not.
        if !self.program_a_unit(case, self.block_c()) {
            return;
        }
        let base = self.block_a();
        let second = self.block_d();

        // The capacity itself is safe to name here too, but only when the region reaches
        // it: a clamp a broken adapter applies then lands inside the region the caller
        // declared expendable, rather than past it — which is exactly the case a
        // whole-device run is for. The block that clamp would land in is the region's own
        // last block, which is `second` only when the region is the required minimum of
        // four blocks; on a wider region it is a block neither existing witness reaches, so
        // it needs one of its own.
        let at_capacity = self.region.end() == self.capacity();
        let edge = self.capacity().checked_sub(self.erase_size());
        if at_capacity {
            let Some(edge) = edge else {
                self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                return;
            };
            if !self.program_a_unit(case, edge) {
                return;
            }
        }

        // A caller with nothing to write is not a caller with a bug, and an adapter that
        // refused would push the empty case into every call site above it.
        let mut legal = self.storage.read(base, &mut []).is_ok()
            && self.storage.program(base, &[]).is_ok()
            && self.storage.erase(base, 0).is_ok()
            && self.storage.read(second, &mut []).is_ok()
            && self.storage.program(second, &[]).is_ok()
            && self.storage.erase(second, 0).is_ok();
        if at_capacity {
            let capacity = self.capacity();
            legal = legal
                && self.storage.read(capacity, &mut []).is_ok()
                && self.storage.program(capacity, &[]).is_ok()
                && self.storage.erase(capacity, 0).is_ok();
        }
        if !legal {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let (Some(first_untouched), Some(second_untouched), Some(before_second_untouched)) = (
            self.block_holds_the_pattern(base),
            self.block_holds_the_pattern(second),
            self.block_holds_the_pattern(self.block_c()),
        ) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let edge_untouched = if at_capacity {
            let Some(result) = edge.and_then(|edge| self.block_holds_the_pattern(edge)) else {
                self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                return;
            };
            result
        } else {
            true
        };
        let outcome =
            if first_untouched && second_untouched && before_second_untouched && edge_untouched {
                Outcome::Passed
            } else {
                Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
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
        // The *whole region*, not the four blocks this run otherwise works in. "Changes no
        // media" is a claim about media, and a check that snapshots four blocks of a
        // sixteen-block region certifies an adapter whose barrier corrupts the fifth. The
        // region is what the caller declared expendable, so it is also what the caller has
        // asked to have checked; a caller who wants a cheaper run passes a smaller region.
        let start = self.region.offset();
        let len = self.region.len();
        if self.storage.erase(start, len).is_err() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        if !self.program_unit_at(case, self.block_a()) {
            return;
        }
        if !self.program_unit_at(case, self.block_b()) {
            return;
        }
        if self.storage.barrier().is_err() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }

        let unit = self.program_size();
        let (first, second) = (self.block_a(), self.block_b());
        let (Some(one), Some(two)) = (
            self.media_matches(first, unit, |position| {
                pattern(usize::try_from(position).unwrap_or(0))
            }),
            self.media_matches(second, unit, |position| {
                pattern(usize::try_from(position).unwrap_or(0))
            }),
        ) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        // Everything else in the region: the tails of the two programmed blocks and every
        // block after them, all of which the erase above left in a state a barrier must not
        // have moved.
        let Some(tails) = self.media_is_erased(first + unit, self.erase_size() - unit) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let rest_start = second + unit;
        let Some(rest) = self.media_is_erased(rest_start, self.region.end() - rest_start) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if one && two && tails && rest {
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

    fn multi_unit_program_is_legal(&mut self) {
        let case = CaseId::MultiUnitProgramIsLegal;
        let unit = self.program_size();
        let block = self.erase_size();
        // Anchored at the *second* designated block, not the region's own first block, so a
        // preceding program unit exists to watch. Every other legal program in this suite is
        // one unit long and starts at a block's own start; neither direction of containment
        // was ever tested for a write spanning more than one unit — and the journal above
        // this contract writes whole frames in one call.
        if !self.erase_block(case, self.block_a()) {
            return;
        }
        let Some(before) = self.block_b().checked_sub(unit) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        if !self.program_unit_at(case, before) {
            return;
        }
        if block <= unit {
            // `validate_program` checks unit alignment and bounds and says nothing about
            // erase-block containment, so when a block holds exactly one program unit a
            // two-unit program is still legal — it just spans two erase blocks. A block-sized
            // exemption here would mean no multi-unit program is ever exercised on such a
            // device.
            self.multi_unit_program_crossing_a_block(case, before);
        } else {
            self.multi_unit_program_within_a_block(case, before);
        }
    }

    /// [`Run::multi_unit_program_is_legal`] on a device where the target fits in one block.
    ///
    /// `before` is the witnessed unit immediately ahead of the target; a fresh witness is
    /// programmed at [`Run::block_c`] as the one immediately behind it.
    fn multi_unit_program_within_a_block(&mut self, case: CaseId, before: u32) {
        let unit = self.program_size();
        let block = self.erase_size();
        if !self.program_a_unit(case, self.block_c()) {
            return;
        }
        if !self.erase_block(case, self.block_b()) {
            return;
        }
        let base = self.block_b();
        let span = unit + unit;
        let Ok(span_len) = usize::try_from(span) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        self.fill_pattern_len(span_len);
        if !self.program_source(base, span_len) {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        // One direct read of the whole span, rather than through `media_matches`'s per-unit
        // chunking: every other successful read in this suite is at most one program unit
        // long, and an adapter that refused a longer one would be certified by a suite that
        // never asked for one.
        if self.read_into(base, span_len, 0).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let written = self
            .bytes(0, span_len)
            .is_some_and(|held| held.iter().copied().eq((0..span_len).map(pattern)));
        let Some(rest) = self.media_is_erased(base + span, block - span) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let Some(preceding) = self.media_matches(before, unit, |position| {
            pattern(usize::try_from(position).unwrap_or(0))
        }) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let Some(following) = self.block_holds_the_pattern(self.block_c()) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if !written {
            Outcome::Failed(Failure::ReadBackDiffers)
        } else if rest && preceding && following {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
        };
        self.record(case, outcome);
    }

    /// [`Run::multi_unit_program_is_legal`] on a device whose block is one program unit, so
    /// the two-unit target spans [`Run::block_b`] and [`Run::block_c`] whole.
    ///
    /// `before` is the witnessed unit immediately ahead of the target; a fresh witness is
    /// programmed at [`Run::block_d`] as the one immediately behind it.
    fn multi_unit_program_crossing_a_block(&mut self, case: CaseId, before: u32) {
        let unit = self.program_size();
        if !self.program_a_unit(case, self.block_d()) {
            return;
        }
        if !self.erase_block(case, self.block_b()) || !self.erase_block(case, self.block_c()) {
            return;
        }
        let base = self.block_b();
        let span = unit + unit;
        let Ok(span_len) = usize::try_from(span) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        self.fill_pattern_len(span_len);
        if !self.program_source(base, span_len) {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        if self.read_into(base, span_len, 0).is_none() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let written = self
            .bytes(0, span_len)
            .is_some_and(|held| held.iter().copied().eq((0..span_len).map(pattern)));
        let Some(preceding) = self.media_matches(before, unit, |position| {
            pattern(usize::try_from(position).unwrap_or(0))
        }) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let Some(following) = self.block_holds_the_pattern(self.block_d()) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if !written {
            Outcome::Failed(Failure::ReadBackDiffers)
        } else if preceding && following {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
        };
        self.record(case, outcome);
    }

    fn multi_block_erase_is_legal(&mut self) {
        let case = CaseId::MultiBlockEraseIsLegal;
        // Anchored at the second and third designated blocks, not the region's own first
        // block, so a block exists on *both* sides of the pair to watch. Every other
        // successful erase elsewhere is one block, so an adapter that refuses a legal
        // two-block erase — or corrupts a neighbour of the pair — has never been asked. The
        // two-bank journal erases a whole bank in one call.
        for block in [
            self.block_a(),
            self.block_b(),
            self.block_c(),
            self.block_d(),
        ] {
            if !self.program_a_unit(case, block) {
                return;
            }
        }
        let size = self.erase_size();
        let base = self.block_b();
        if self.storage.erase(base, size + size).is_err() {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        }
        let Some(cleared) = self.media_is_erased(base, size + size) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        // And it stops where it was told, on both sides: the block before the pair and the
        // block after it still hold what was programmed into them.
        let (Some(before), Some(after)) = (
            self.block_holds_the_pattern(self.block_a()),
            self.block_holds_the_pattern(self.block_d()),
        ) else {
            self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
            return;
        };
        let outcome = if !cleared {
            Outcome::Failed(Failure::EraseDidNotClearTheRegion)
        } else if before && after {
            Outcome::Passed
        } else {
            Outcome::Failed(Failure::MediaOutsideTheOperationChanged)
        };
        self.record(case, outcome);
    }

    fn reading_changes_no_media(&mut self) {
        let case = CaseId::ReadingChangesNoMedia;
        // `read` is the one operation with nothing to check afterwards, which is exactly why
        // an adapter that corrupts the bytes it just handed back can pass every other case:
        // each of them compares what the *first* read returned. Reading twice is what makes
        // the second read a witness for the first — and reading the *whole block* both
        // times, not just the unit, catches a read that corrupts bytes adjacent to what it
        // returned rather than the returned bytes themselves.
        if !self.program_a_unit(case, self.block_a()) {
            return;
        }
        // A witness in the following block too: on a device whose erase block is a single
        // program unit, `block_holds_the_pattern` has no erased tail left to inspect, so a
        // read that corrupts the unit right after the one it returned lands entirely outside
        // this case's own block with nothing here to notice it.
        if !self.program_a_unit(case, self.block_b()) {
            return;
        }
        let base = self.block_a();
        for _ in 0..2 {
            match self.block_holds_the_pattern(base) {
                Some(true) => {}
                Some(false) => {
                    self.record(case, Outcome::Failed(Failure::ReadBackDiffers));
                    return;
                }
                None => {
                    self.record(case, Outcome::Failed(Failure::LegalOperationRefused));
                    return;
                }
            }
        }
        let outcome = match self.block_holds_the_pattern(self.block_b()) {
            Some(true) => Outcome::Passed,
            Some(false) => Outcome::Failed(Failure::MediaOutsideTheOperationChanged),
            None => Outcome::Failed(Failure::LegalOperationRefused),
        };
        self.record(case, outcome);
    }
}

#[cfg(test)]
mod tests {
    use super::{ERASED, Report, Run, pattern, slice_is_erased};
    use crate::region::Region;
    use waymaker_fault::Device;
    use waymaker_flash::storage::{Geometry, StableStorage};

    /// Records every offset an operation names, and otherwise agrees to everything.
    ///
    /// A white-box double rather than a model of a device: it exists to answer "where did
    /// the case look", which no adapter that actually behaves like NOR can be asked either.
    struct OffsetRecorder {
        geometry: Geometry,
        offsets: [u32; 256],
        count: usize,
    }

    impl OffsetRecorder {
        fn note(&mut self, offset: u32) {
            if let Some(slot) = self.offsets.get_mut(self.count) {
                *slot = offset;
            }
            self.count += 1;
        }
    }

    impl StableStorage for OffsetRecorder {
        type Error = core::convert::Infallible;

        fn geometry(&self) -> Geometry {
            self.geometry
        }

        fn read(&mut self, offset: u32, _dst: &mut [u8]) -> Result<(), Self::Error> {
            self.note(offset);
            Ok(())
        }

        fn program(&mut self, offset: u32, _src: &[u8]) -> Result<(), Self::Error> {
            self.note(offset);
            Ok(())
        }

        fn erase(&mut self, offset: u32, _len: u32) -> Result<(), Self::Error> {
            self.note(offset);
            Ok(())
        }

        fn barrier(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn zero_length_probes_stay_inside_a_region_that_ends_short_of_the_capacity() {
        // The capacity is one past the device's own last byte — never a byte the caller's
        // region reaches unless the region is the whole device. A case anchored there names
        // no byte of its own (a zero-length operation names none), but it still tells a
        // broken adapter where to look, and an adapter that clamps and mutates would be
        // reaching past whatever the caller declared expendable.
        let Ok(geometry) = Geometry::new(1024, 64, 4, 2) else {
            unreachable!("1024 is whole 64-byte blocks of whole 4-byte units of 2-byte reads")
        };
        let Ok(region) = Region::new(geometry, 64, 256) else {
            unreachable!("64 and 256 are whole 64-byte blocks inside 1024 bytes")
        };
        let mut storage = OffsetRecorder {
            geometry,
            offsets: [0; 256],
            count: 0,
        };
        let mut buffer = [0_u8; 8];
        let mut run = Run {
            storage: &mut storage,
            region,
            buffer: &mut buffer,
            unit: 4,
            report: Report::new(),
        };
        run.zero_length_operations_are_legal_and_change_nothing();

        assert!(storage.count > 0, "the case issued no operations at all");
        assert!(
            storage.count <= storage.offsets.len(),
            "the case issued {} operations, more than this recorder can hold — widen it \
             rather than silently checking only the first {}",
            storage.count,
            storage.offsets.len()
        );
        for offset in storage.offsets.iter().take(storage.count) {
            assert!(
                (region.offset()..region.end()).contains(offset),
                "a zero-length probe named offset {offset}, outside the region {region:?}"
            );
        }
    }

    const UNIT: u32 = 4;
    const ERASE_SIZE: u32 = 256;

    /// Four whole erase blocks, which is what [`Region::whole_device`] requires and more
    /// than the widest span either test below scans.
    fn geometry() -> Geometry {
        let Ok(geometry) = Geometry::new(4 * ERASE_SIZE, ERASE_SIZE, UNIT, 1) else {
            unreachable!("1024 is four whole 256-byte blocks of 4-byte units")
        };
        geometry
    }

    /// A [`Run`] over `device`, scratching in `buffer` — built directly rather than
    /// through [`run`] because these tests exist to pin the private chunking `media_matches`
    /// does internally, at buffer sizes narrower than, equal to and wider than the span
    /// scanned, which [`run`]'s own `SuiteError::BufferTooSmall` floor does not let a caller
    /// choose freely below two program units.
    fn run_over<'a>(device: &'a mut Device, buffer: &'a mut [u8]) -> Run<'a, Device> {
        let Ok(region) = Region::whole_device(geometry()) else {
            unreachable!("geometry() is four whole erase blocks")
        };
        Run {
            storage: device,
            region,
            buffer,
            unit: UNIT as usize,
            report: Report::new(),
        }
    }

    /// `slice_is_erased` has to agree with "every byte is [`ERASED`]", at every length
    /// around a word boundary and with the one non-erased byte at every position — inside
    /// a whole word-sized chunk and inside whatever a word-at-a-time walk would leave as a
    /// remainder. Pinned before `slice_is_erased` stops being a plain `iter().all(...)` and
    /// starts comparing a word at a time, mirroring
    /// `waymaker_flash::recovery::is_erased`'s own sweep of the same shape.
    #[test]
    fn slice_is_erased_agrees_with_the_byte_at_a_time_definition_at_every_length_and_position() {
        const MAX_LEN: usize = 32;
        let word = core::mem::size_of::<usize>();
        let widest = word * 3 + 1;
        assert!(widest <= MAX_LEN, "word size outgrew this fixture");
        for len in 0..=widest {
            let all_erased = [ERASED; MAX_LEN];
            assert!(
                slice_is_erased(&all_erased[..len]),
                "length {len} of all erased bytes"
            );
            for spoiled in 0..len {
                let mut bytes = all_erased;
                bytes[spoiled] = 0x00;
                assert!(
                    !slice_is_erased(&bytes[..len]),
                    "length {len} spoiled at {spoiled}"
                );
            }
        }
    }

    /// `media_is_erased` has to agree with "every byte is [`ERASED`]" — and detect a lone
    /// programmed unit — at every buffer size this chunking optimization changes the
    /// grouping of: narrower than the span, exactly a unit, exactly the whole span, and
    /// wider than the span. And at every unit position inside the span, including the
    /// first, an interior one, and the last — the one a narrow buffer puts in its own
    /// trailing chunk and a wide one does not.
    #[test]
    fn media_is_erased_agrees_at_every_buffer_size_and_unit_position() {
        const UNITS_IN_SPAN: u32 = 5;
        let span = UNITS_IN_SPAN * UNIT;

        for buffer_len in [
            UNIT as usize,
            (2 * UNIT) as usize,
            span as usize,
            (span * 3) as usize,
        ] {
            for programmed_unit in 0..UNITS_IN_SPAN {
                let mut device = Device::new(geometry());
                // Programming can only clear bits from the erased baseline, so this is what
                // one non-erased unit looks like on real media.
                let programmed = [ERASED & 0xFE, ERASED, ERASED, ERASED];
                device
                    .program(programmed_unit * UNIT, &programmed)
                    .expect("a unit-aligned program inside the span");

                let mut buffer = [0_u8; 4096];
                let mut run = run_over(&mut device, &mut buffer[..buffer_len]);
                assert_eq!(
                    run.media_is_erased(0, span),
                    Some(false),
                    "buffer_len={buffer_len} programmed_unit={programmed_unit}"
                );
            }

            let mut device = Device::new(geometry());
            let mut buffer = [0_u8; 4096];
            let mut run = run_over(&mut device, &mut buffer[..buffer_len]);
            assert_eq!(
                run.media_is_erased(0, span),
                Some(true),
                "buffer_len={buffer_len} freshly erased"
            );
        }
    }

    /// `media_is_erased`'s answer has to agree with an oracle that shares none of its
    /// code: one `StableStorage::read` of the whole span into a single buffer, compared
    /// byte by byte against [`ERASED`] with no chunk loop of its own. Pinned before
    /// `media_is_erased` stops going through the generic, closure-driven `media_matches`
    /// and gets a loop of its own — a chunk-boundary regression in the specialized
    /// version would disagree with an oracle that has no chunk boundaries at all to get
    /// wrong.
    #[test]
    fn media_is_erased_agrees_with_an_unchunked_oracle_read() {
        const SPAN: u32 = 5 * UNIT + 1;

        for buffer_len in [
            UNIT as usize,
            (2 * UNIT) as usize,
            SPAN as usize,
            4096_usize,
        ] {
            for programmed_unit in [None, Some(0_u32), Some(2_u32), Some(4_u32)] {
                let mut device = Device::new(geometry());
                if let Some(unit_index) = programmed_unit {
                    let programmed = [ERASED & 0xFE, ERASED, ERASED, ERASED];
                    device
                        .program(unit_index * UNIT, &programmed)
                        .expect("a unit-aligned program inside the span");
                }

                let mut oracle = [0_u8; SPAN as usize];
                device
                    .read(0, &mut oracle)
                    .expect("the whole span is a legal read");
                let expected = oracle.iter().all(|&byte| byte == ERASED);

                let mut buffer = [0_u8; 4096];
                let mut run = run_over(&mut device, &mut buffer[..buffer_len]);
                assert_eq!(
                    run.media_is_erased(0, SPAN),
                    Some(expected),
                    "buffer_len={buffer_len} programmed_unit={programmed_unit:?}"
                );
            }
        }
    }

    /// A span that is not a whole number of chunks still gets checked completely — the
    /// remainder the internal loop's last iteration leaves over is neither skipped nor
    /// double-counted — at every buffer size.
    #[test]
    fn media_is_erased_covers_a_span_that_is_not_a_whole_number_of_chunks() {
        for buffer_len in [UNIT as usize, (3 * UNIT) as usize, 4096_usize] {
            for span in [1_u32, UNIT - 1, UNIT + 1, UNIT * 3 + 1, ERASE_SIZE - 1] {
                let mut device = Device::new(geometry());
                let mut buffer = [0_u8; 4096];
                let mut run = run_over(&mut device, &mut buffer[..buffer_len]);
                assert_eq!(
                    run.media_is_erased(0, span),
                    Some(true),
                    "buffer_len={buffer_len} span={span}"
                );
            }
        }
    }

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
