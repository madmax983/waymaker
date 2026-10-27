//! [`Bridge`], the one type in this crate. See the crate documentation.

use waymaker_core::timer::{ClockKind, TimerSpec};
use waymaker_core::{ActivityKind, Outcome};
use waymaker_drive::{Answered, Boundary, Handoff, Suspended};
use waymaker_embassy::journal::{Answer, Halted, Journal};

/// A [`Boundary`] seen as the façade's durable half.
///
/// It borrows the boundary rather than owning it, so one boot's `&mut dyn Boundary` can be
/// handed to a workflow future and taken back.
pub struct Bridge<'a> {
    boundary: &'a mut dyn Boundary,
    /// The last real [`Suspended`] the boundary returned, if any.
    suspended: Option<Suspended>,
}

impl<'a> Bridge<'a> {
    /// The façade's journal over `boundary`.
    #[must_use]
    pub fn over(boundary: &'a mut dyn Boundary) -> Self {
        Self {
            boundary,
            suspended: None,
        }
    }

    /// The boundary's own [`Suspended`], if the last call returned one.
    ///
    /// For a caller driving an opaque future: a poll that ends `Pending` with no recorded
    /// conclusion suspended for a real reason, and this is that reason, carried across the
    /// `.await` rather than invented on this side of it.
    #[must_use]
    pub const fn take_suspended(&mut self) -> Option<Suspended> {
        self.suspended.take()
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
            Ok(Handoff::Dispatch { id, result_bytes }) => {
                Ok(waymaker_embassy::journal::Handoff::Dispatch { id, result_bytes })
            }
            Err(suspended) => {
                self.suspended = Some(suspended);
                Err(Halted)
            }
        }
    }

    fn resolve(&mut self, answer: Answer<'_>) -> Result<Outcome<'_>, Halted> {
        let answered = match answer {
            Answer::Completed(bytes) => Answered::Completed(bytes),
            Answer::Failed(bytes) => Answered::Failed(bytes),
            Answer::Exhausted => Answered::Exhausted,
        };
        match self.boundary.resolve(answered) {
            Ok(outcome) => Ok(outcome),
            Err(suspended) => {
                self.suspended = Some(suspended);
                Err(Halted)
            }
        }
    }

    fn wait(&mut self, spec: TimerSpec) -> Result<(), Halted> {
        match self.boundary.wait(spec) {
            Ok(()) => Ok(()),
            Err(suspended) => {
                self.suspended = Some(suspended);
                Err(Halted)
            }
        }
    }

    fn continue_as_new(&mut self, input: &[u8]) -> Halted {
        self.suspended = Some(self.boundary.continue_as_new(input));
        Halted
    }

    fn deadline_remaining(&self) -> Option<(ClockKind, u64)> {
        self.boundary.deadline_remaining()
    }
}
