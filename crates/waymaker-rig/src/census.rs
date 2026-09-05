//! The census over the six cells the three write points and the two reset causes make.
//!
//! # Why it fails closed
//!
//! Because "the rig ran for an hour" is not "the rig covered the six cells". A run that never
//! reached a dispatch-phase watchdog reset has said nothing about that cell, and
//! [`Coverage::verdict`] is what makes that a refusal rather than a silence.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for cuts at three write
//! points and, separately, for "watchdog-reset tests at the same three points". Six cells,
//! and a run that missed one is not a run that passed.

use crate::phase::{Phase, ResetCause};

/// A cell of the census that no iteration reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Gap {
    phase: Phase,
    cause: ResetCause,
}

impl Gap {
    /// The write point that was never cut into.
    #[must_use]
    pub const fn phase(self) -> Phase {
        self.phase
    }

    /// The reset cause that was never applied there.
    #[must_use]
    pub const fn cause(self) -> ResetCause {
        self.cause
    }
}

impl core::fmt::Display for Gap {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("no iteration cut the ")?;
        formatter.write_str(self.phase.name())?;
        formatter.write_str(" write with a ")?;
        formatter.write_str(self.cause.name())?;
        formatter.write_str(" reset")
    }
}

impl core::error::Error for Gap {}

/// How many iterations reached each cause, for one phase.
///
/// A struct rather than an array because `indexing_slicing` is denied workspace-wide and a
/// census that reached for a cell by number is a census that can reach for the wrong one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Causes {
    power_cut: u32,
    watchdog: u32,
}

impl Causes {
    const EMPTY: Self = Self {
        power_cut: 0,
        watchdog: 0,
    };

    const fn count(self, cause: ResetCause) -> u32 {
        match cause {
            ResetCause::PowerCut => self.power_cut,
            ResetCause::Watchdog => self.watchdog,
        }
    }

    const fn with(self, cause: ResetCause, count: u32) -> Self {
        match cause {
            ResetCause::PowerCut => Self {
                power_cut: count,
                ..self
            },
            ResetCause::Watchdog => Self {
                watchdog: count,
                ..self
            },
        }
    }
}

/// How many iterations reached each of the six cells.
///
/// # Invariants
///
/// [`verdict`](Self::verdict) passes only when every cell is non-zero, and it walks
/// [`Phase::ALL`] x [`ResetCause::ALL`] rather than a hand-written list of six, so a fourth
/// write point added to [`Phase`] is a cell the census demands rather than one it forgets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Coverage {
    schedule: Causes,
    dispatch: Causes,
    completion: Causes,
}

impl Coverage {
    /// A census nothing has been recorded in.
    pub const EMPTY: Self = Self {
        schedule: Causes::EMPTY,
        dispatch: Causes::EMPTY,
        completion: Causes::EMPTY,
    };

    const fn row(self, phase: Phase) -> Causes {
        match phase {
            Phase::Schedule => self.schedule,
            Phase::Dispatch => self.dispatch,
            Phase::Completion => self.completion,
        }
    }

    const fn with_row(self, phase: Phase, row: Causes) -> Self {
        match phase {
            Phase::Schedule => Self {
                schedule: row,
                ..self
            },
            Phase::Dispatch => Self {
                dispatch: row,
                ..self
            },
            Phase::Completion => Self {
                completion: row,
                ..self
            },
        }
    }

    /// This census with one more iteration credited to `(phase, cause)`.
    ///
    /// Saturating. A wrapped counter reads as an uncovered cell, which is the one direction
    /// this must not fail in: the census exists to refuse a run with a hole, and a run long
    /// enough to wrap has the opposite problem.
    #[must_use]
    pub const fn record(self, phase: Phase, cause: ResetCause) -> Self {
        let row = self.row(phase);
        let raised = row.count(cause).saturating_add(1);
        self.with_row(phase, row.with(cause, raised))
    }

    /// This census with `(phase, cause)` credited as many times as it can be.
    ///
    /// The census's own saturation, reachable without a four-billion-iteration loop.
    #[must_use]
    pub const fn saturated(self, phase: Phase, cause: ResetCause) -> Self {
        let row = self.row(phase).with(cause, u32::MAX);
        self.with_row(phase, row)
    }

    /// How many iterations reached `(phase, cause)`.
    #[must_use]
    pub const fn iterations(self, phase: Phase, cause: ResetCause) -> u32 {
        self.row(phase).count(cause)
    }

    /// Every cell, in the order [`verdict`](Self::verdict) reads them.
    fn cells(self) -> impl Iterator<Item = (Phase, ResetCause, u32)> {
        Phase::ALL.into_iter().flat_map(move |phase| {
            ResetCause::ALL
                .into_iter()
                .map(move |cause| (phase, cause, self.iterations(phase, cause)))
        })
    }

    /// Every iteration the census counted, saturating.
    #[must_use]
    pub fn total(self) -> u32 {
        self.cells()
            .fold(0_u32, |total, (_, _, count)| total.saturating_add(count))
    }

    /// `Ok` only when all six cells were reached.
    ///
    /// # Errors
    ///
    /// The first [`Gap`] in [`Phase::ALL`] x [`ResetCause::ALL`] order. Fixed, so two runs
    /// with the same hole report the same hole.
    pub fn verdict(self) -> Result<(), Gap> {
        self.cells()
            .find(|(_, _, count)| *count == 0)
            .map_or(Ok(()), |(phase, cause, _)| Err(Gap { phase, cause }))
    }
}
