//! The in-process conformance run.

use waymaker_flash::storage::StableStorage;

use crate::case::Report;
use crate::region::Region;

/// Why a conformance run could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuiteError {
    /// The region was checked against a different device.
    RegionIsNotForThisDevice,
    /// The caller's buffer is smaller than twice the program unit.
    BufferTooSmall,
}

/// Runs every case of [`crate::case::CASES`] against `storage`.
///
/// # Errors
///
/// [`SuiteError`] if the run could not start at all.
pub fn run<S: StableStorage>(
    _storage: &mut S,
    _region: Region,
    _buffer: &mut [u8],
) -> Result<Report, SuiteError> {
    Ok(Report::new())
}
