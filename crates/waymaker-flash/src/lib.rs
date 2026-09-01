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
//! Rung 0.0: the crate exists so that the layering is enforceable. The record seals,
//! barriers, and bank swap arrive with rung 0.2.

#![no_std]
#![forbid(unsafe_code)]
