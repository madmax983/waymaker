//! Design document §14's failure-semantics table as a vocabulary, and a census over it.
//!
//! Issue [#31](https://github.com/madmax983/waymaker/issues/31) turns each row of that table
//! into a named assertion. The rows live here, in a `no_std` crate a board can link, so the
//! model sweep and the rig sweep classify a crash point in one vocabulary and a failure reads
//! as the row it broke.
//!
//! # Why it fails closed
//!
//! For [`crate::census`]'s reason. A sweep that never reached a row has said nothing about it,
//! and [`Matrix::verdict`] makes that a refusal rather than a silence. This rig reaches six of
//! the ten; `tests/matrix.rs` pins the seventh as the gap rather than shrinking the table.

/// A row of design document §14's failure-semantics table.
///
/// The order is the table's. [`index`](Self::index) is a position in [`ALL`](Self::ALL), so
/// a census cannot credit one row for another's coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Row {
    /// The supply went during the schedule frame write. The frame is ignored; the activity
    /// was not yet dispatchable.
    DuringScheduleFrameWrite,
    /// After the schedule barrier, before dispatch. Redeliver the stable effect id.
    AfterScheduleBarrierBeforeDispatch,
    /// During the physical activity. Redeliver; the activity tolerates a duplicate attempt.
    DuringPhysicalActivity,
    /// After the activity, before the completion barrier. Redeliver with the same id.
    AfterActivityBeforeCompletionBarrier,
    /// During the completion write. The torn completion is ignored and no partial result
    /// bytes are exposed.
    DuringCompletionWrite,
    /// After the completion barrier. The completion is replayed and the activity never runs
    /// again.
    AfterCompletionBarrier,
    /// During the inactive bank's erase or write. The old bank stays authoritative and the
    /// old run continues.
    DuringInactiveBankEraseOrWrite,
    /// After the new bank's seal barrier. The new bank is authoritative and the old run is
    /// never current again.
    AfterNewBankSealBarrier,
    /// History capacity reached. No mutation: a capacity error or an explicit
    /// `continue_as_new`.
    HistoryCapacityReached,
    /// Replay divergence. A deterministic fault, no further execution, history untouched.
    ReplayDivergence,
}

impl Row {
    /// The ten, in the table's order.
    pub const ALL: [Self; 10] = [
        Self::DuringScheduleFrameWrite,
        Self::AfterScheduleBarrierBeforeDispatch,
        Self::DuringPhysicalActivity,
        Self::AfterActivityBeforeCompletionBarrier,
        Self::DuringCompletionWrite,
        Self::AfterCompletionBarrier,
        Self::DuringInactiveBankEraseOrWrite,
        Self::AfterNewBankSealBarrier,
        Self::HistoryCapacityReached,
        Self::ReplayDivergence,
    ];

    /// Where this row falls in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::DuringScheduleFrameWrite => 0,
            Self::AfterScheduleBarrierBeforeDispatch => 1,
            Self::DuringPhysicalActivity => 2,
            Self::AfterActivityBeforeCompletionBarrier => 3,
            Self::DuringCompletionWrite => 4,
            Self::AfterCompletionBarrier => 5,
            Self::DuringInactiveBankEraseOrWrite => 6,
            Self::AfterNewBankSealBarrier => 7,
            Self::HistoryCapacityReached => 8,
            Self::ReplayDivergence => 9,
        }
    }

    /// The row at `index`, or `None` past the end.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::DuringScheduleFrameWrite),
            1 => Some(Self::AfterScheduleBarrierBeforeDispatch),
            2 => Some(Self::DuringPhysicalActivity),
            3 => Some(Self::AfterActivityBeforeCompletionBarrier),
            4 => Some(Self::DuringCompletionWrite),
            5 => Some(Self::AfterCompletionBarrier),
            6 => Some(Self::DuringInactiveBankEraseOrWrite),
            7 => Some(Self::AfterNewBankSealBarrier),
            8 => Some(Self::HistoryCapacityReached),
            9 => Some(Self::ReplayDivergence),
            _ => None,
        }
    }

    /// The stable id, cited in `CLAUDE.md`, the ADR and the gate.
    ///
    /// `xtask`'s `failure-matrix` rule reads these literals, so a row renamed here and not in
    /// `xtask::docs::FAILURE_ROWS` fails a build.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::DuringScheduleFrameWrite => "during-schedule-frame-write",
            Self::AfterScheduleBarrierBeforeDispatch => "after-schedule-barrier-before-dispatch",
            Self::DuringPhysicalActivity => "during-physical-activity",
            Self::AfterActivityBeforeCompletionBarrier => {
                "after-activity-before-completion-barrier"
            }
            Self::DuringCompletionWrite => "during-completion-write",
            Self::AfterCompletionBarrier => "after-completion-barrier",
            Self::DuringInactiveBankEraseOrWrite => "during-inactive-bank-erase-or-write",
            Self::AfterNewBankSealBarrier => "after-new-bank-seal-barrier",
            Self::HistoryCapacityReached => "history-capacity-reached",
            Self::ReplayDivergence => "replay-divergence",
        }
    }
}

/// A row no crash point landed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Gap {
    row: Row,
}

impl Gap {
    /// The row that was never reached.
    #[must_use]
    pub const fn row(self) -> Row {
        self.row
    }
}

impl core::fmt::Display for Gap {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("no crash point landed in row `")?;
        formatter.write_str(self.row.id())?;
        formatter.write_str("`")
    }
}

impl core::error::Error for Gap {}

/// How many crash points landed in each row.
///
/// # Invariants
///
/// [`verdict`](Self::verdict) passes only when every row is non-zero, walking
/// [`Row::ALL`] rather than a hand-written list, so a row added to the table is a row the
/// census demands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Matrix {
    reached: [u32; 10],
}

impl Matrix {
    /// A census nothing has been recorded in.
    pub const EMPTY: Self = Self { reached: [0; 10] };

    /// This census with one more crash point credited to `row`.
    ///
    /// Saturating, for [`crate::census::Coverage::record`]'s reason: a wrapped counter reads
    /// as an unreached row, which is the direction this must not fail in.
    #[must_use]
    pub fn record(mut self, row: Row) -> Self {
        if let Some(cell) = self.reached.get_mut(row.index()) {
            *cell = cell.saturating_add(1);
        }
        self
    }

    /// This census with `row` credited as many times as it can be.
    #[must_use]
    pub fn saturated(mut self, row: Row) -> Self {
        if let Some(cell) = self.reached.get_mut(row.index()) {
            *cell = u32::MAX;
        }
        self
    }

    /// How many crash points landed in `row`.
    #[must_use]
    pub fn iterations(self, row: Row) -> u32 {
        self.reached.get(row.index()).copied().unwrap_or(0)
    }

    /// Every crash point the census counted, saturating.
    #[must_use]
    pub fn total(self) -> u32 {
        self.reached
            .iter()
            .fold(0_u32, |total, count| total.saturating_add(*count))
    }

    /// `Ok` only when every row was reached.
    ///
    /// # Errors
    ///
    /// The first [`Gap`] in [`Row::ALL`] order, so two sweeps with the same hole report the
    /// same hole.
    pub fn verdict(self) -> Result<(), Gap> {
        Row::ALL
            .into_iter()
            .find(|row| self.iterations(*row) == 0)
            .map_or(Ok(()), |row| Err(Gap { row }))
    }
}
