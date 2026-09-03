//! The across-reset witness: the two clauses no in-process suite can observe.
//!
//! Design document §12 says two things about [`barrier`](waymaker_flash::storage::StableStorage::barrier),
//! and both are statements about what is on media *after the power came back*:
//!
//! * after `barrier` returns, all earlier successful mutations survive reset;
//! * no later mutation may become durable before mutations ordered by a completed barrier.
//!
//! A suite running inside one process never sees that moment, and a suite that claimed to
//! would be lying. So this is two calls with a reset between them: [`arm`] writes a witness,
//! the caller cuts the power wherever it likes, and [`verify`] reads the answer back.
//!
//! # The witness
//!
//! Three erase blocks, and a seal between the two that matter:
//!
//! | Step | Block | What |
//! | --- | --- | --- |
//! | 1–3 | C, B, A | erased, each followed by a barrier |
//! | 4 | A | the *acknowledged* witness, read back, then a barrier |
//! | 5 | B | the *seal*, then a barrier |
//! | 6 | C | the *unacknowledged* witness. No barrier follows it. |
//!
//! Media is `0xFF` when erased, which is [`crate::suite::ERASED`] and is a constant for the
//! reason the in-process suite gives.
//!
//! [`verify`] then asks two questions, and they are the two clauses:
//!
//! * The seal is on media and the acknowledged witness is not. The seal was programmed
//!   *after* the witness's barrier returned, so the witness was promised and then lost:
//!   [`Breach::AcknowledgedMutationLost`].
//! * The unacknowledged witness is on media and the seal is not. That write was issued
//!   after the seal's barrier completed, so it overtook what the barrier ordered:
//!   [`Breach::LaterMutationOvertookABarrier`].
//!
//! Anything else is [`WitnessVerdict::Held`] — including a device that lost everything,
//! which is what a power cut *during* [`arm`] looks like and is not a breach.
//!
//! Unless [`arm`] returned. Then both the acknowledged witness and the seal crossed barriers
//! that returned, so both are owed whatever else is on media, and a device that lost
//! everything has broken the first clause rather than illustrated it. The caller says which
//! of the two histories they produced, through [`Reset`], because a reset destroys the
//! process that would otherwise remember.
//!
//! # Why the erases run backwards
//!
//! C first, then B, then A. The order matters if the region already holds a *previous*
//! witness: erasing C before B means the unacknowledged witness is gone before the seal
//! can be, so a stale C can never be read alongside a fresh, erased seal — which is the one
//! way a correct device could be accused of the second breach.
//!
//! What remains outside what [`verify`] can judge is a region that was armed twice with no
//! `verify` between: a witness is a single arm-reset-verify, and running two on top of each
//! other asks a stateless reader to tell two histories apart.

use waymaker_flash::storage::StableStorage;

use crate::region::Region;
use crate::suite::{REQUIRED_BUFFER_UNITS, SuiteError};

/// When the power went, which is the one thing [`verify`] cannot read off media.
///
/// A reset destroys the process that called [`arm`], so nothing can be carried across it in
/// a value. The caller is the only one who knows which of these two histories they produced,
/// and the answer changes what a device that lost everything means: after a completed arm it
/// is a barrier that did not mean what it said, and during one it is an ordinary power cut
/// before the first barrier returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Reset {
    /// The power went while [`arm`] was running, and it never returned.
    DuringArm,
    /// [`arm`] returned `Ok(())`, and the reset came after it.
    AfterACompletedArm,
}

/// What a verified witness says.
///
/// Named for the witness rather than called `Verdict`, because [`crate::case::Verdict`] is a
/// different type about a different thing and a crate root exporting both would hand an
/// out-of-tree caller the wrong one silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WitnessVerdict {
    /// Both barrier clauses held across the reset.
    Held,
    /// One of them did not.
    Breached(Breach),
}

impl WitnessVerdict {
    /// `Ok(())` when the barrier clauses held.
    ///
    /// The shape [`crate::case::Report::verdict`] has, so that `verify(..)?.held()?` reads
    /// the way `report.verdict()?` does. Without it, `verify(..)?` type-checks, discards a
    /// [`Breach`], and reads as a pass — which is the one thing this crate exists to stop.
    ///
    /// # Errors
    ///
    /// The [`Breach`] the witness found.
    pub const fn held(self) -> Result<(), Breach> {
        match self {
            Self::Held => Ok(()),
            Self::Breached(breach) => Err(breach),
        }
    }
}

/// A barrier clause an adapter broke.
///
/// Deliberately *not* `#[non_exhaustive]`, unlike [`crate::case::Failure`]: this is §12's
/// two barrier sentences and there is no third. A variant added here would mean the design
/// document grew a clause, which is a change a caller should be made to look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Breach {
    /// A mutation a completed barrier acknowledged is not on media after the reset.
    ///
    /// Design document §12: "after `barrier` returns, all earlier successful mutations
    /// survive reset".
    AcknowledgedMutationLost,
    /// A mutation issued after a completed barrier is on media while one the barrier
    /// ordered is not.
    ///
    /// Design document §12: "no later mutation may become durable before mutations ordered
    /// by a completed barrier".
    LaterMutationOvertookABarrier,
}

impl Breach {
    /// A short static description of this breach.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::AcknowledgedMutationLost => "a mutation a barrier acknowledged was lost",
            Self::LaterMutationOvertookABarrier => "a later mutation overtook a completed barrier",
        }
    }
}

impl core::fmt::Display for Breach {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for Breach {}

/// Arming or verifying could not be done at all.
///
/// A driver error from [`arm`] is not a failed conformance run: a witness is *meant* to be
/// interrupted, and an interrupted program is one of the ways that happens. It is reported
/// so that a caller who was not expecting one can tell it apart from a completed arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WitnessError<E> {
    /// The run could not start: a region for another device, or too small a buffer.
    Suite(SuiteError),
    /// The driver refused.
    Driver(E),
    /// A witness was programmed and did not read back.
    ///
    /// Not a barrier failure — nothing has been barriered yet — but a device whose erased
    /// state cannot hold the pattern this module programs, which would make every later
    /// answer meaningless. Reported at arm time rather than read as "nothing was durable".
    WitnessDidNotTake,
}

impl<E> WitnessError<E> {
    /// A short static description of this refusal.
    ///
    /// The driver's own error is not rendered here — it is `E` on a generic parameter, and
    /// the `Display` implementation below is what reaches it when `E` can be displayed.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::Suite(error) => error.message(),
            Self::Driver(_) => "the driver refused",
            Self::WitnessDidNotTake => "a programmed witness did not read back",
        }
    }
}

impl<E> From<SuiteError> for WitnessError<E> {
    fn from(error: SuiteError) -> Self {
        Self::Suite(error)
    }
}

impl<E: core::fmt::Display> core::fmt::Display for WitnessError<E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Driver(error) => error.fmt(formatter),
            other => formatter.write_str(other.message()),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for WitnessError<E> {}

/// The three blocks the witness lives in, and the unit each one holds.
struct Layout {
    acknowledged: u32,
    seal: u32,
    unacknowledged: u32,
    unit: usize,
}

impl Layout {
    /// Checks the region against the device and works out where the three witnesses go.
    fn of<S: StableStorage>(
        storage: &S,
        region: Region,
        buffer: &[u8],
    ) -> Result<Self, SuiteError> {
        let geometry = region.geometry();
        if storage.geometry() != geometry {
            return Err(SuiteError::RegionIsNotForThisDevice);
        }
        let unit =
            usize::try_from(geometry.program_size()).map_err(|_| SuiteError::BufferTooSmall)?;
        let needed = unit
            .checked_mul(REQUIRED_BUFFER_UNITS as usize)
            .ok_or(SuiteError::BufferTooSmall)?;
        if buffer.len() < needed {
            return Err(SuiteError::BufferTooSmall);
        }
        // `Region::new` guarantees three erase blocks, so all three are `Some`.
        let (Some(acknowledged), Some(seal), Some(unacknowledged)) =
            (region.block(0), region.block(1), region.block(2))
        else {
            return Err(SuiteError::RegionIsNotForThisDevice);
        };
        Ok(Self {
            acknowledged,
            seal,
            unacknowledged,
            unit,
        })
    }
}

/// The three patterns. Distinct, so that a block holding one of the others is not mistaken
/// for the one being looked for.
const ACKNOWLEDGED_SALT: u8 = 0xA5;
const SEAL_SALT: u8 = 0x5E;
const UNACKNOWLEDGED_SALT: u8 = 0xC3;

/// One byte of a witness pattern.
///
/// Not a function of the erased state, unlike the in-process suite's pattern: [`verify`]
/// runs after a reset and has nothing to learn the erased byte from that a broken device
/// could not have invented. On NOR — erased is all ones — programming these bytes stores
/// them exactly, and [`arm`] reads its own witness back rather than assuming so.
const fn witness_byte(salt: u8, index: u8) -> u8 {
    salt ^ index
}

/// The low byte of an index, without a cast a lint has to be told to ignore.
///
/// A witness longer than 256 bytes repeats, which is what a pattern is for: the point is
/// that a block holding one salt is not mistaken for a block holding another.
fn low_byte(index: usize) -> u8 {
    u8::try_from(index & 0xFF).unwrap_or(0)
}

/// Fills `slot` with the witness for `salt`.
fn fill_witness(slot: &mut [u8], salt: u8) {
    for (index, cell) in slot.iter_mut().enumerate() {
        *cell = witness_byte(salt, low_byte(index));
    }
}

/// Whether `bytes` is the witness for `salt`.
fn is_witness(bytes: &[u8], salt: u8) -> bool {
    bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == witness_byte(salt, low_byte(index)))
}

/// Writes the witness. Cut the power at any point during or after this call.
///
/// On return with `Ok(())`, the acknowledged witness and the seal have both crossed a
/// barrier and the unacknowledged one has not. On return with
/// [`WitnessError::Driver`] the run was interrupted, which is a legitimate outcome and
/// still leaves a region [`verify`] can judge.
///
/// # Errors
///
/// [`WitnessError::Suite`] if the region or the buffer is wrong for this device,
/// [`WitnessError::Driver`] if the driver refused, and [`WitnessError::WitnessDidNotTake`]
/// if a programmed witness did not read back.
pub fn arm<S: StableStorage>(
    storage: &mut S,
    region: Region,
    buffer: &mut [u8],
) -> Result<(), WitnessError<S::Error>> {
    let layout = Layout::of(storage, region, buffer)?;
    let block = region.geometry().erase_size();

    for offset in [layout.unacknowledged, layout.seal, layout.acknowledged] {
        storage.erase(offset, block).map_err(WitnessError::Driver)?;
        storage.barrier().map_err(WitnessError::Driver)?;
    }

    program_witness(
        storage,
        buffer,
        layout.acknowledged,
        ACKNOWLEDGED_SALT,
        layout.unit,
    )?;
    storage.barrier().map_err(WitnessError::Driver)?;

    program_witness(storage, buffer, layout.seal, SEAL_SALT, layout.unit)?;
    storage.barrier().map_err(WitnessError::Driver)?;

    program_witness(
        storage,
        buffer,
        layout.unacknowledged,
        UNACKNOWLEDGED_SALT,
        layout.unit,
    )
}

/// Programs one witness and reads it back, before any barrier orders it.
fn program_witness<S: StableStorage>(
    storage: &mut S,
    buffer: &mut [u8],
    offset: u32,
    salt: u8,
    unit: usize,
) -> Result<(), WitnessError<S::Error>> {
    let Some(slot) = buffer.get_mut(..unit) else {
        return Err(WitnessError::Suite(SuiteError::BufferTooSmall));
    };
    fill_witness(slot, salt);
    let Some(source) = buffer.get(..unit) else {
        return Err(WitnessError::Suite(SuiteError::BufferTooSmall));
    };
    // The source has to be copied out of the buffer to read back into it, and a `no_std`
    // crate with no allocator cannot; the read therefore lands in the second unit.
    storage
        .program(offset, source)
        .map_err(WitnessError::Driver)?;
    let Some(back) = buffer.get_mut(unit..unit + unit) else {
        return Err(WitnessError::Suite(SuiteError::BufferTooSmall));
    };
    storage.read(offset, back).map_err(WitnessError::Driver)?;
    let Some(back) = buffer.get(unit..unit + unit) else {
        return Err(WitnessError::Suite(SuiteError::BufferTooSmall));
    };
    if is_witness(back, salt) {
        Ok(())
    } else {
        Err(WitnessError::WitnessDidNotTake)
    }
}

/// Reads the witness back after a reset.
///
/// `reset` says which history the caller produced, because a reset destroys the process that
/// ran [`arm`] and no value survives it. Passing [`Reset::AfterACompletedArm`] when `arm`
/// did not in fact return is how a caller manufactures a breach that never happened; passing
/// [`Reset::DuringArm`] when it did is how a caller hides the only barrier bug that loses
/// everything.
///
/// # Errors
///
/// [`WitnessError::Suite`] if the region or the buffer is wrong for this device, and
/// [`WitnessError::Driver`] if a read the geometry permits was refused.
pub fn verify<S: StableStorage>(
    storage: &mut S,
    region: Region,
    buffer: &mut [u8],
    reset: Reset,
) -> Result<WitnessVerdict, WitnessError<S::Error>> {
    let layout = Layout::of(storage, region, buffer)?;

    let acknowledged = present(
        storage,
        buffer,
        layout.acknowledged,
        ACKNOWLEDGED_SALT,
        layout.unit,
    )?;
    let seal = present(storage, buffer, layout.seal, SEAL_SALT, layout.unit)?;
    let unacknowledged = present(
        storage,
        buffer,
        layout.unacknowledged,
        UNACKNOWLEDGED_SALT,
        layout.unit,
    )?;

    // After a completed arm there is nothing to be relative to: both the witness and the
    // seal crossed a barrier that returned, so both are owed whatever else is on media. A
    // seal-relative rule alone calls a device that lost *everything* `Held`, and a write-back
    // cache behind a `barrier` that returns early loses exactly everything — the most likely
    // barrier bug there is, and the one this witness exists to find.
    if reset == Reset::AfterACompletedArm && !(acknowledged && seal) {
        return Ok(WitnessVerdict::Breached(Breach::AcknowledgedMutationLost));
    }
    if seal && !acknowledged {
        return Ok(WitnessVerdict::Breached(Breach::AcknowledgedMutationLost));
    }
    if unacknowledged && !seal {
        return Ok(WitnessVerdict::Breached(
            Breach::LaterMutationOvertookABarrier,
        ));
    }
    Ok(WitnessVerdict::Held)
}

/// Whether the witness for `salt` is on media at `offset`.
fn present<S: StableStorage>(
    storage: &mut S,
    buffer: &mut [u8],
    offset: u32,
    salt: u8,
    unit: usize,
) -> Result<bool, WitnessError<S::Error>> {
    let Some(slot) = buffer.get_mut(..unit) else {
        return Err(WitnessError::Suite(SuiteError::BufferTooSmall));
    };
    storage.read(offset, slot).map_err(WitnessError::Driver)?;
    let Some(slot) = buffer.get(..unit) else {
        return Err(WitnessError::Suite(SuiteError::BufferTooSmall));
    };
    Ok(is_witness(slot, salt))
}
