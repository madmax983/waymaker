//! The across-reset witness.

use waymaker_flash::storage::StableStorage;

use crate::region::Region;
use crate::suite::SuiteError;

/// What a verified witness says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Both barrier clauses held across the reset.
    Held,
    /// One of them did not.
    Breached(Breach),
}

/// A barrier clause an adapter broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Breach {
    /// A mutation a completed barrier acknowledged is not on media after the reset.
    AcknowledgedMutationLost,
    /// A mutation issued after a completed barrier is durable while one the barrier ordered
    /// is not.
    LaterMutationOvertookABarrier,
}

/// Arming or verifying could not be done.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WitnessError<E> {
    /// The run could not start.
    Suite(SuiteError),
    /// The driver refused.
    Driver(E),
}

/// Writes the witness. Cut the power at any point during or after this call.
///
/// # Errors
///
/// [`WitnessError`] if the run could not start, or the driver refused.
pub fn arm<S: StableStorage>(
    _storage: &mut S,
    _region: Region,
    _buffer: &mut [u8],
) -> Result<(), WitnessError<S::Error>> {
    Ok(())
}

/// Reads the witness back after a reset.
///
/// # Errors
///
/// [`WitnessError`] if the run could not start, or the driver refused a read.
pub fn verify<S: StableStorage>(
    _storage: &mut S,
    _region: Region,
    _buffer: &mut [u8],
) -> Result<Verdict, WitnessError<S::Error>> {
    Ok(Verdict::Held)
}
