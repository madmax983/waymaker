//! Two-bank NOR flash adapter for Waymaker.
//!
//! This crate owns the stable wire encoding, CRC and record seals, bank selection, append
//! scanning, and the compaction transition. It turns the kernel's effect boundaries into
//! bytes that survive power loss, and turns bytes back into a legal committed prefix.
//!
//! # What this crate must not own
//!
//! Activities, workflow types, timers, or Embassy. The async façade lives one layer up.
//! `cargo xtask check-layering` fails a build in which this crate reaches Embassy, whether
//! directly or through another crate.
//!
//! # Status
//!
//! [`integrity`] is the seal the codec verifies with — ADR 0010's catalogued, table-free
//! pair, bound behind a trait so the choice stays swappable and the widths do not.
//! [`frame`] is the record codec — §09's handwritten, fixed-endian, self-delimiting,
//! bounds-validated frame, its two checksums, and the append scan that turns a bank into a
//! committed prefix. [`storage`] is §12's contract: the geometry that decides what an
//! operation may name, and the four operations and one barrier every port implements.
//!
//! [`bank`] is rung 0.2's first arrival: §10's two-bank layout derived from a geometry, the
//! bank header, the generation seal that names it, and the selection rule that makes exactly
//! one bank authoritative. What is still owed at 0.2 is the per-record commit seal, the
//! barrier protocol that writes it, the capacity reserve and `continue_as_new`; [`frame`]'s
//! documentation says what the first of those leaves deferred and what the deferral costs.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bank;
mod crc;
pub mod frame;
pub mod integrity;
pub mod recovery;
pub mod storage;

pub use bank::{
    Authority, BankHeader, BankId, BankLayout, BankRegion, Generation, LayoutError, Seal,
};
pub use frame::{Decoded, Frame, ProgramAlign, Scan};
pub use integrity::{Catalogued, IntegrityCheck};
pub use recovery::{Ending, JournalRegion, Recovery, RecoveryError, RegionError};
pub use storage::{Geometry, GeometryError, StableStorage};
