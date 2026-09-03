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
//!   buffer it may use, and returns one outcome per case. Twenty of them.
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
//! times, and two program units of scratch. **No case names a byte outside that region** —
//! not even in an operation it expects to be refused, which is the part that matters: an
//! adapter that wrongly *accepted* one could then only damage media the caller declared
//! expendable. Where no such operation exists, the case is [`Outcome::NotApplicable`] rather
//! than issued somewhere unsafe, and
//! `tests/teeth.rs::no_wrong_adapter_lets_the_suite_damage_media_outside_the_region` holds
//! it against every wrong adapter in the catalogue rather than against a correct one.
//!
//! What that cannot promise is containment of an adapter whose *legal* operations run wild.
//! A driver asked to erase one block of the region and erasing the whole chip, or a `barrier`
//! that scribbles somewhere it was not asked to, is outside what any suite can prevent — the
//! operation was authorised — and both are caught rather than contained.
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

pub use case::{CASE_COUNT, CASES, Case, CaseId, Failure, NotApplicable, Outcome, Report, Verdict};
pub use clause::{CLAUSES, Clause, Discharge};
pub use durability::{Breach, Reset, WitnessError, WitnessVerdict};
pub use nor::{NorFlashStorage, PortError, PortGeometryError};
pub use region::{REQUIRED_ERASE_BLOCKS, Region, RegionError};
pub use suite::{ERASED, REQUIRED_BUFFER_UNITS, SuiteError, run};
