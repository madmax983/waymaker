//! The conformance suite for design document §12's storage contract, and the
//! `embedded-storage` port that shows the contract can be adapted.
//!
//! Issue [#21](https://github.com/madmax983/waymaker/issues/21) asks for two things:
//! "a conformance test suite exists that any adapter can be run against", and "an
//! `embedded-storage` implementation can be adapted without `embedded-storage` becoming a
//! kernel dependency". Both live here, and here is above the layers — `waymaker-flash` owns
//! §12's [`StableStorage`](waymaker_flash::storage::StableStorage) and
//! [`Geometry`](waymaker_flash::storage::Geometry), and it is 8 KiB of code flash that has
//! no business carrying a test suite or a third-party dependency. See
//! [ADR 0016](https://github.com/madmax983/waymaker/blob/main/docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md).
//!
//! # What this crate owns
//!
//! * [`clause`] — §12's contract as a table, every row saying what discharges it. Two of
//!   the six rows say "not this crate", which is the point of the table.
//! * [`case`] and [`suite`] — the nineteen in-process observations and the runner that
//!   makes them. [`suite::run`] takes an adapter, a [`region::Region`] it may destroy and a
//!   buffer it may use, and returns one outcome per case.
//! * [`durability`] — the two clauses no in-process suite can observe, as a witness written
//!   before a reset and read after one.
//! * [`nor`] — [`NorFlashStorage`], an `embedded_storage::nor_flash::NorFlash` presented as
//!   a `StableStorage`.
//!
//! # What this crate must not own
//!
//! Records, frames, journals, banks. The suite is written against the trait and nothing
//! else, which is what makes "any adapter" mean any adapter — including one for a part
//! Waymaker has never seen, written by somebody who has never read `waymaker-flash`.
//!
//! # Why it is `#![no_std]` when the other test-support crates are not
//!
//! Because the adapter it exists for may only be runnable on the target the driver is for.
//! `waymaker-fault` and `waymaker-spec` model and enumerate on a host, and a host is where
//! they belong; a conformance suite is something a driver author runs *against the part*,
//! over a debug probe, on a device with no allocator. Nothing here allocates, and the one
//! buffer the suite needs is the caller's.
//!
//! # What a run costs
//!
//! Three erase blocks of the region the caller names, erased and reprogrammed several
//! times, and four program units of scratch. The suite never mutates a byte outside that
//! region: the illegal operations it issues are chosen so that a conformant adapter refuses
//! them, and an adapter that wrongly accepted one could only damage bytes the region already
//! covers.
//!
//! ```
//! use waymaker_conformance::{Region, run};
//! use waymaker_flash::storage::Geometry;
//!
//! # fn main() -> Result<(), Box<dyn core::error::Error>> {
//! let geometry = Geometry::new(1024, 64, 4, 2)?;
//! let region = Region::whole_device(geometry)?;
//! let mut buffer = [0_u8; 16];
//! let mut adapter = waymaker_fault::Device::new(geometry);
//!
//! let report = run(&mut adapter, region, &mut buffer)?;
//! report.verdict()?;
//! # Ok(())
//! # }
//! ```

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
pub use durability::{Breach, WitnessError, arm, verify};
pub use nor::{NorFlashStorage, PortError, PortGeometryError};
pub use region::{REQUIRED_ERASE_BLOCKS, Region, RegionError};
pub use suite::{REQUIRED_BUFFER_UNITS, SuiteError, run};
