//! The bridge from this driver to the async façade.
//!
//! Issue [#35](https://github.com/madmax983/waymaker/issues/35)'s second "done when" is
//! that removing the Embassy crate leaves the protocol fully usable through the synchronous
//! driver. This module is the whole of the edge that would go with it: [`Boundary`],
//! [`Driver`](crate::Driver) and design document §07's typestate name no
//! `waymaker-embassy` type, so deleting this file and [`ota`](crate::ota) leaves a driver
//! that still runs [`Pipeline`](crate::demo::Pipeline).
//!
//! # What the bridge adds
//!
//! Nothing. It renames four calls. The authority is the driver's, and the façade's own
//! documentation says the façade has none.

use waymaker_core::timer::TimerSpec;
use waymaker_core::{ActivityKind, Outcome};
use waymaker_embassy::journal::{Answer, Halted, Journal};

use crate::boundary::{Answered, Boundary, Handoff};

/// A [`Boundary`] seen as the façade's durable half.
///
/// It borrows the boundary rather than owning it, so one boot's `&mut dyn Boundary` can be
/// handed to a workflow future and taken back.
pub struct Bridge<'a> {
    boundary: &'a mut dyn Boundary,
}

impl<'a> Bridge<'a> {
    /// The façade's journal over `boundary`.
    #[must_use]
    pub fn over(boundary: &'a mut dyn Boundary) -> Self {
        Self { boundary }
    }
}

impl Journal for Bridge<'_> {
    fn schedule(
        &mut self,
        kind: ActivityKind,
        input: &[u8],
    ) -> Result<waymaker_embassy::journal::Handoff<'_>, Halted> {
        match self.boundary.schedule(kind, input) {
            Ok(Handoff::Replayed(outcome)) => {
                Ok(waymaker_embassy::journal::Handoff::Replayed(outcome))
            }
            Ok(Handoff::Dispatch(id)) => Ok(waymaker_embassy::journal::Handoff::Dispatch(id)),
            Err(_) => Err(Halted),
        }
    }

    fn resolve(&mut self, answer: Answer<'_>) -> Result<Outcome<'_>, Halted> {
        let answered = match answer {
            Answer::Completed(bytes) => Answered::Completed(bytes),
            Answer::Failed(bytes) => Answered::Failed(bytes),
            Answer::Exhausted => Answered::Exhausted,
        };
        self.boundary.resolve(answered).map_err(|_| Halted)
    }

    fn wait(&mut self, spec: TimerSpec) -> Result<(), Halted> {
        self.boundary.wait(spec).map_err(|_| Halted)
    }

    fn continue_as_new(&mut self, input: &[u8]) -> Halted {
        let _ = self.boundary.continue_as_new(input);
        Halted
    }
}
