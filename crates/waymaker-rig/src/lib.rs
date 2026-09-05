//! The power-cut and watchdog-reset rig of design document §15.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod audit;
pub mod census;
pub mod cutter;
pub mod log;
pub mod phase;
pub mod plan;
pub mod run;
pub mod wear;
pub mod window;
pub mod witness;
pub mod workload;
