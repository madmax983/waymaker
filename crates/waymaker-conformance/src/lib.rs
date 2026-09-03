//! The conformance suite for design document §12's storage contract.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod case;
pub mod clause;
pub mod durability;
pub mod nor;
pub mod region;
pub mod suite;

pub use case::{CASES, Case, CaseId, Failure, NotApplicable, Outcome, Report, Verdict};
pub use clause::{CLAUSES, Clause, Discharge};
pub use nor::{NorFlashStorage, PortError, PortGeometryError};
pub use region::{REQUIRED_ERASE_BLOCKS, Region, RegionError};
pub use suite::{SuiteError, run};
