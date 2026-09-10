//! The code the tools are pointed at.
//!
//! A profiler measures a process, so a claim about the engine needs a process that is
//! *mostly* the engine and whose remainder is legible. These are that process: each drives
//! real library code over `waymaker-fault`'s model of NOR, and each is small enough that
//! everything else in the image is startup and argument handling.
//!
//! # Why the workloads are here rather than in a crate of their own
//!
//! Because a new workspace member is a row in [`crate::policy`], a bullet in `CLAUDE.md` and
//! a category to argue about, and this is not code anything links — it is a subcommand of
//! the gate, run under a tool, for the length of one measurement. `xtask` already depends on
//! all five crates driven below, for the reason [`crate::wear`] gives: a number that lives in
//! two places is a number that ends up disagreeing with itself.
//!
//! # What a workload owes
//!
//! To *finish*, and to say how many effects it finished. A workload that returned early
//! would leave the tools measuring a prefix of the thing the report names, and the figure
//! would fall rather than rise — which is the one direction a cost measurement must not be
//! wrong in silently. So every step is checked and the count is returned rather than
//! assumed.

use waymaker_core::RunId;
use waymaker_drive::demo::{BOUNDS, DOWNLOAD, HASH, Pipeline, World};
use waymaker_drive::{Conclusion, Driver, Progress, Scratch};
use waymaker_fault::Device;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::JournalRegion;
use waymaker_flash::storage::Geometry;
use waymaker_rig::cutter::{Dispatcher, NeverCut};
use waymaker_rig::log::Outcome;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Rig, Verdict};
use waymaker_rig::wear::Metered;

/// Why a workload could not be run to the end.
///
/// Every one of these is a failure of the measurement rather than a result: a report that
/// carried figures from a workload that did not finish would be a report about a prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkloadError {
    /// What went wrong, in one line.
    pub message: String,
}

impl WorkloadError {
    /// A failure, described.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WorkloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkloadError {}

/// The seed the `journal` workload's payload lengths come from.
///
/// Fixed for [`crate::wear::SEED`]'s reason: a published number that moved with the clock is
/// a number nobody can reproduce. It is deliberately the same seed, so that the instruction
/// figure and the write-amplification figure are two readings of one run rather than of two.
pub const SEED: u64 = crate::wear::SEED;

/// The run the `driver` workload's journal belongs to. On media it lives in the bank header.
const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// A dispatcher that does nothing, because the figures are about the engine.
struct Inert;

impl Dispatcher for Inert {
    type Error = std::convert::Infallible;

    fn dispatch(&mut self, _effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Runs the workload named `name` and answers how many effects it completed.
///
/// # Errors
///
/// [`WorkloadError`] when the name is not one of [`super::WORKLOADS`], or when the workload
/// does not run to its declared end.
pub fn run(name: &str) -> Result<u32, WorkloadError> {
    match name {
        "journal" => journal(),
        "driver" => driver(),
        other => Err(WorkloadError::new(format!(
            "unknown workload `{other}`; the workloads are {}",
            super::WORKLOADS
                .iter()
                .map(|workload| workload.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// `waymaker-flash`'s two-barrier writer, driven by the rig over a modelled part.
///
/// The widest path in the engine that does not need a workflow: §09's frame codec, §09's
/// commit seal, §10's bank layout and capacity reserve, and the recovery scan that positions
/// the writer — plus `waymaker-rig`, which is `#![no_std]` and allocation-free and is
/// therefore held to the same zero.
fn journal() -> Result<u32, WorkloadError> {
    let geometry = Geometry::new(6 * 4096, 4096, 4, 1)
        .map_err(|error| WorkloadError::new(format!("not a geometry ({})", error.message())))?;
    let effects = crate::wear::EFFECTS;
    let rig = Rig::new::<waymaker_fault::FaultError>(geometry, Plan::new(SEED), effects)
        .map_err(|error| WorkloadError::new(format!("cannot be laid out ({error:?})")))?;
    let mut device = Device::new(geometry);
    let mut page = [0_u8; Rig::PAGE_BYTES];
    // The wear meter is not optional on the write path, so it is part of what is measured
    // here — which is right rather than merely unavoidable: `waymaker-rig` is one of the
    // crates whose allocation count this gate holds at zero, and the meter is its code.
    {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .map_err(|error| WorkloadError::new(format!("cannot be prepared ({error:?})")))?;
        rig.iterate(0, &mut metered, &mut Inert, &mut NeverCut, &mut page)
            .map_err(|error| WorkloadError::new(format!("cannot be written ({error:?})")))?;
    }
    // Verified rather than assumed, for [`crate::wear::measure_part`]'s reason: figures from
    // a run that did not recover are figures about a broken write.
    match rig
        .verify(0, &mut device, &mut page)
        .map(Verdict::outcome)
        .map_err(|error| WorkloadError::new(format!("cannot be verified ({error:?})")))?
    {
        Outcome::Passed => Ok(u32::from(effects)),
        Outcome::Breached(breach) => Err(WorkloadError::new(format!(
            "wrote a run its own oracle rejects: {breach}"
        ))),
    }
}

/// §06's boundary and §07's effect protocol, driven to a terminal record.
///
/// The other half of the engine: `waymaker-core`'s replay cursor and transition table, and
/// `waymaker-drive`'s driver — whose own "done when" is a workflow run to completion with
/// "no `Future`, no Embassy, and no allocation". Two of those three are visible in the
/// source; this is the third, measured.
fn driver() -> Result<u32, WorkloadError> {
    let geometry = Geometry::new(4096, 1024, 4, 1)
        .map_err(|error| WorkloadError::new(format!("not a geometry ({})", error.message())))?;
    let align =
        ProgramAlign::new(4).ok_or_else(|| WorkloadError::new("4 is not a program alignment"))?;
    let region = JournalRegion::spanning(geometry, 0, 1024, align)
        .map_err(|error| WorkloadError::new(format!("not a region ({error:?})")))?;
    let layout = BankLayout::new(geometry)
        .map_err(|error| WorkloadError::new(format!("not a bank layout ({error:?})")))?;
    let reserve = Reserve::for_layout(BOUNDS, layout)
        .map_err(|error| WorkloadError::new(format!("not a reserve ({error:?})")))?;

    let mut device = Device::new(geometry);
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region, RUN, reserve)
        .boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .map_err(|error| WorkloadError::new(format!("the run did not complete ({error:?})")))?;

    match progress {
        Progress::Finished {
            conclusion: Conclusion::Completed,
            ..
        } => {}
        other => {
            return Err(WorkloadError::new(format!(
                "the run did not complete: {other:?}"
            )));
        }
    }

    // The effects the world was really asked to perform, rather than the number this
    // function was written expecting. A workflow that stopped dispatching would otherwise
    // publish a cost per effect over a denominator nothing measured.
    //
    // Compared in place rather than collected. A `Vec` here is a harness allocation inside
    // the region both tools are watching, and while it is attributed to `xtask` and gates
    // nothing, a workload whose own noise has to be explained is a workload that will
    // eventually be believed about the wrong thing.
    let dispatched = world.dispatched();
    let expected = [DOWNLOAD, HASH];
    if dispatched.len() != expected.len()
        || dispatched
            .iter()
            .zip(expected)
            .any(|(call, kind)| call.kind != kind)
    {
        return Err(WorkloadError::new(format!(
            "the reference workflow dispatched {} effects rather than its two activities",
            dispatched.len()
        )));
    }
    u32::try_from(dispatched.len())
        .map_err(|_| WorkloadError::new("more effects than a u32 can count"))
}
