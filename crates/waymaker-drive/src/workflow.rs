//! The workflow a driver runs.
//!
//! A plain value with a method, re-run from its beginning after every reset. There is no
//! `Future`: a workflow that must wait returns [`Suspended`], and the state it keeps
//! between boundaries is its own fields, which the replay of committed history refills.

use waymaker_core::Outcome;

use crate::boundary::{Boundary, Suspended};

/// Which workflow this is, at which version, on which input.
///
/// Design document §06 step 2: the run input is decoded into caller-owned storage. Here it
/// is the workflow's own, which is why the driver compares this against the `RunStarted`
/// record rather than handing the record's bytes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity<'a> {
    /// The workflow, as the `RunStarted` record records it.
    pub kind: u16,
    /// Its version. A firmware image may refuse to replay a version it does not know.
    pub version: u16,
    /// The run's input, opaque to the engine.
    pub input: &'a [u8],
}

/// A workflow the driver can run to completion.
pub trait Workflow {
    /// What this run is, so that history can be checked against it.
    ///
    /// # Postconditions
    ///
    /// The same value for the life of the run. A workflow whose identity changed between
    /// two boots is a workflow whose recorded `RunStarted` no longer describes it, and the
    /// driver refuses that rather than replaying somebody else's history.
    fn identity(&self) -> Identity<'_>;

    /// Run from the beginning, asking `boundary` at every effect.
    ///
    /// Called once per boot. Every call the workflow makes must be the same call, in the
    /// same order, with the same input, as the run that wrote the history — that is design
    /// document §08's determinism requirement, and the kernel refuses a run that breaks it.
    ///
    /// # Errors
    ///
    /// [`Suspended`], which a workflow gets from `boundary` and must propagate. It must not
    /// be swallowed: the driver has already decided the run stops here, and a workflow that
    /// carries on only stops saying so.
    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended>;
}
