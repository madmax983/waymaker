//! What the suite checks, and how it reports.
//!
//! A case is a named observation with a clause behind it. A [`Report`] is one outcome per
//! case, held in a fixed array so that a `no_std` adapter author on a target with no
//! allocator gets the same report a host does.
//!
//! # Why an outcome starts as [`Outcome::NotRun`]
//!
//! Because a suite that silently skipped a case would report success. Every case starts
//! `NotRun`, the runner overwrites each one as it goes, and [`Report::verdict`] refuses a
//! report that still has one — so a case added to [`CASES`] and forgotten in the runner
//! fails a run rather than shrinking it.

use core::fmt;

/// One named observation the suite makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Case {
    /// Which case this is.
    pub id: CaseId,
    /// The case's name, as a report prints it.
    pub name: &'static str,
    /// The id of the [`crate::clause::Clause`] this case discharges part of.
    pub clause: &'static str,
}

/// Every case the in-process suite runs.
///
/// Two clauses and no more are represented here: `validated-before-media`, which is about
/// what an adapter *refuses*, and `operations-act-on-what-they-name`, which is about what it
/// does when it agrees. The other four clauses of [`crate::clause::CLAUSES`] are discharged
/// somewhere else and say where.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum CaseId {
    /// `geometry()` answers the same thing every time it is asked.
    GeometryIsStable,
    /// A read whose offset or length is not a multiple of the read unit is refused.
    MisalignedReadIsRefused,
    /// A program whose offset or length is not a multiple of the program unit is refused.
    MisalignedProgramIsRefused,
    /// An erase whose offset or length is not a multiple of the erase unit is refused.
    MisalignedEraseIsRefused,
    /// A read that reaches past the capacity — or whose end would overflow — is refused.
    ReadPastCapacityIsRefused,
    /// A program that reaches past the capacity is refused.
    ProgramPastCapacityIsRefused,
    /// An erase that reaches past the capacity is refused.
    ErasePastCapacityIsRefused,
    /// A refused program left the media it named exactly as it found it.
    RefusedProgramTouchesNoMedia,
    /// A refused erase left the media it named exactly as it found it.
    RefusedEraseTouchesNoMedia,
    /// After an erase, every byte of the block reads as one repeated value.
    EraseYieldsOneRepeatedByte,
    /// What a program wrote is what a read returns.
    ProgramRoundTripsThroughRead,
    /// A program of one unit left the rest of its erase block erased.
    ProgramLeavesTheRestOfTheBlockAlone,
    /// An erase of one block left the neighbouring block alone.
    EraseLeavesTheNeighbouringBlockAlone,
    /// Erasing an erased block is legal and leaves it erased.
    EraseIsIdempotent,
    /// A zero-length read, program and erase are legal and change nothing.
    ZeroLengthOperationsAreLegalAndChangeNothing,
    /// Reading a unit in read-sized pieces agrees with reading it whole.
    PartialReadsAgreeWithTheWhole,
    /// `barrier()` succeeds on a device nothing has gone wrong with.
    BarrierSucceeds,
    /// `barrier()` changes no media.
    BarrierChangesNoMedia,
    /// A second `barrier()` with no mutation between is legal.
    RepeatedBarriersAreLegal,
}

/// Every case, in the order the runner runs them.
pub const CASES: &[Case] = &[
    Case {
        id: CaseId::GeometryIsStable,
        name: "geometry is stable",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::MisalignedReadIsRefused,
        name: "a misaligned read is refused",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::MisalignedProgramIsRefused,
        name: "a misaligned program is refused",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::MisalignedEraseIsRefused,
        name: "a misaligned erase is refused",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::ReadPastCapacityIsRefused,
        name: "a read past the capacity is refused",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::ProgramPastCapacityIsRefused,
        name: "a program past the capacity is refused",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::ErasePastCapacityIsRefused,
        name: "an erase past the capacity is refused",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::RefusedProgramTouchesNoMedia,
        name: "a refused program touches no media",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::RefusedEraseTouchesNoMedia,
        name: "a refused erase touches no media",
        clause: "validated-before-media",
    },
    Case {
        id: CaseId::EraseYieldsOneRepeatedByte,
        name: "an erase yields one repeated byte",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::ProgramRoundTripsThroughRead,
        name: "a program round-trips through a read",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::ProgramLeavesTheRestOfTheBlockAlone,
        name: "a program leaves the rest of the block alone",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::EraseLeavesTheNeighbouringBlockAlone,
        name: "an erase leaves the neighbouring block alone",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::EraseIsIdempotent,
        name: "an erase is idempotent",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::ZeroLengthOperationsAreLegalAndChangeNothing,
        name: "zero-length operations are legal and change nothing",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::PartialReadsAgreeWithTheWhole,
        name: "partial reads agree with the whole",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::BarrierSucceeds,
        name: "a barrier succeeds",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::BarrierChangesNoMedia,
        name: "a barrier changes no media",
        clause: "operations-act-on-what-they-name",
    },
    Case {
        id: CaseId::RepeatedBarriersAreLegal,
        name: "repeated barriers are legal",
        clause: "operations-act-on-what-they-name",
    },
];

/// How many cases there are.
pub const CASE_COUNT: usize = CASES.len();

impl CaseId {
    /// This case's position in a [`Report`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// This case's row in [`CASES`].
    ///
    /// `None` only if [`CASES`] and this enum have stopped agreeing, which
    /// `the_case_table_and_the_case_ids_agree` fails a build over.
    #[must_use]
    pub fn spec(self) -> Option<&'static Case> {
        match CASES.get(self.index()) {
            Some(case) if case.id as usize == self.index() => Some(case),
            _ => None,
        }
    }

    /// This case's name, or a placeholder if the table and the enum have drifted.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self.spec() {
            Some(case) => case.name,
            None => "unnamed case",
        }
    }
}

/// Why a case did not apply to this device.
///
/// Every reason is a fact about the geometry, so a report that skips a case says which
/// property of the device made the case unaskable — never "it did not seem to matter".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NotApplicable {
    /// The unit this case misaligns against is one byte, so no misalignment exists.
    TheUnitIsOneByte,
    /// The erase block is one program unit, so a program has no rest-of-block to leave
    /// alone.
    TheBlockIsOneProgramUnit,
    /// Erasing yields a byte with no set bits, so no program can change anything and a
    /// round-trip would be vacuous.
    TheErasedStateHasNoProgrammableBits,
}

impl NotApplicable {
    /// A short static description of this exemption.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::TheUnitIsOneByte => "the unit is one byte, so no misaligned operation exists",
            Self::TheBlockIsOneProgramUnit => "the erase block is a single program unit",
            Self::TheErasedStateHasNoProgrammableBits => "the erased state has no bits to clear",
        }
    }
}

/// How an adapter broke a case.
///
/// The driver's own error is deliberately not carried: it is `S::Error` on a generic
/// parameter, and a report that had to name it could not be a plain array of `Copy` values
/// on a target with no allocator. The case id names the operation, and a driver author who
/// needs the error re-runs the one call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Failure {
    /// An operation the geometry permits was refused.
    LegalOperationRefused,
    /// An operation the geometry forbids was accepted.
    IllegalOperationAccepted,
    /// A refused operation changed media before refusing.
    RefusedOperationTouchedMedia,
    /// A read did not return what a program put there.
    ReadBackDiffers,
    /// An erase left the region in a state a program cannot start from.
    EraseDidNotClearTheRegion,
    /// Media the operation did not name changed anyway.
    MediaOutsideTheOperationChanged,
    /// `geometry()` gave two different answers.
    GeometryIsNotStable,
}

impl Failure {
    /// A short static description of this failure.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::LegalOperationRefused => "a legal operation was refused",
            Self::IllegalOperationAccepted => "an illegal operation was accepted",
            Self::RefusedOperationTouchedMedia => "a refused operation changed media",
            Self::ReadBackDiffers => "a read did not return what was programmed",
            Self::EraseDidNotClearTheRegion => "an erase did not clear the region",
            Self::MediaOutsideTheOperationChanged => "media outside the operation changed",
            Self::GeometryIsNotStable => "geometry() gave two different answers",
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for Failure {}

/// What became of one case.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// The runner never reached this case. Fails [`Report::verdict`], on purpose.
    #[default]
    NotRun,
    /// The adapter held.
    Passed,
    /// The geometry made the question unaskable.
    NotApplicable(NotApplicable),
    /// The adapter broke.
    Failed(Failure),
}

/// One outcome per case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    outcomes: [Outcome; CASE_COUNT],
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}

impl Report {
    /// A report in which nothing has been run yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            outcomes: [Outcome::NotRun; CASE_COUNT],
        }
    }

    /// Records what became of `case`.
    pub fn record(&mut self, case: CaseId, outcome: Outcome) {
        if let Some(slot) = self.outcomes.get_mut(case.index()) {
            *slot = outcome;
        }
    }

    /// What became of `case`.
    #[must_use]
    pub fn outcome(&self, case: CaseId) -> Outcome {
        self.outcomes
            .get(case.index())
            .copied()
            .unwrap_or(Outcome::NotRun)
    }

    /// Every case and its outcome, in [`CASES`] order.
    pub fn entries(&self) -> impl Iterator<Item = (&'static Case, Outcome)> + '_ {
        CASES
            .iter()
            .zip(self.outcomes.iter().copied())
            .map(|(case, outcome)| (case, outcome))
    }

    /// The first case that did not pass, if there is one.
    #[must_use]
    pub fn first_failure(&self) -> Option<(CaseId, Failure)> {
        self.entries().find_map(|(case, outcome)| match outcome {
            Outcome::Failed(failure) => Some((case.id, failure)),
            _ => None,
        })
    }

    /// Whether the adapter conforms.
    ///
    /// # Errors
    ///
    /// [`Verdict::Failed`] naming the first broken case, and [`Verdict::NotRun`] naming the
    /// first case the runner never reached — a suite that skipped a case has measured
    /// nothing about it, and reporting that as conformance is the failure mode this whole
    /// crate exists to avoid.
    pub fn verdict(&self) -> Result<(), Verdict> {
        for (case, outcome) in self.entries() {
            match outcome {
                Outcome::NotRun => return Err(Verdict::NotRun(case.id)),
                Outcome::Failed(failure) => return Err(Verdict::Failed(case.id, failure)),
                Outcome::Passed | Outcome::NotApplicable(_) => {}
            }
        }
        Ok(())
    }

    /// Every case the geometry made unaskable.
    pub fn exemptions(&self) -> impl Iterator<Item = (CaseId, NotApplicable)> + '_ {
        self.entries().filter_map(|(case, outcome)| match outcome {
            Outcome::NotApplicable(reason) => Some((case.id, reason)),
            _ => None,
        })
    }
}

/// Why a report is not a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// This case never ran.
    NotRun(CaseId),
    /// This case ran and the adapter broke it.
    Failed(CaseId, Failure),
}

impl fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRun(case) => {
                formatter.write_str("case never ran: ")?;
                formatter.write_str(case.name())
            }
            Self::Failed(case, failure) => {
                formatter.write_str(case.name())?;
                formatter.write_str(": ")?;
                formatter.write_str(failure.message())
            }
        }
    }
}

impl core::error::Error for Verdict {}
