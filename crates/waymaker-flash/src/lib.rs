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
//! Rung 0.1 in progress: [`integrity`] is the seal the codec verifies with — ADR 0010's
//! catalogued, table-free pair, bound behind a trait so the choice stays swappable and the
//! widths do not. [`frame`] is the record codec — §09's handwritten, fixed-endian,
//! self-delimiting, bounds-validated frame, its two checksums, and the append scan that
//! turns a bank into a committed prefix. The commit seal, the barriers and the bank swap
//! arrive with rung 0.2; [`frame`]'s documentation says what that leaves deferred and what
//! the deferral costs.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod crc;
pub mod frame;
pub mod integrity;
pub mod storage;

pub use frame::{Decoded, Frame, ProgramAlign, Scan};
pub use integrity::{Catalogued, IntegrityCheck};
pub use storage::{Geometry, GeometryError, StableStorage};
