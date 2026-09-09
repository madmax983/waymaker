//! Workflow versioning: which recorded versions this image may replay.
//!
//! Design document §08. Every run pins a `workflow_kind` and a `workflow_version` in its
//! `RunStarted` record, and §08 states four rules about what a later image may do with
//! them. Two are here.
//!
//! # What this module owns
//!
//! [`VersionRange`], the recorded versions a firmware image declares it can replay, and
//! [`GateId`], the number that names one decision point inside a workflow. That is the
//! whole vocabulary; the record that carries a gate's answer is
//! [`RecordRef::VersionMarker`](crate::RecordRef::VersionMarker) and the boundary that
//! decides it is [`ReplayMachine::version_intent`](crate::ReplayMachine::version_intent).
//!
//! # What this module must not own
//!
//! Source locations. §08's fourth rule is that "source-location hashes are not stable
//! identity; call-order sequencing remains authoritative", so nothing here reads
//! `file!`, `line!`, `column!` or `module_path!` — a gate is a number a workflow author
//! chooses and a position in the run's one ordered history, and it is those two together
//! that identify it. A hash of a source location changes when a comment above the call
//! moves, which would turn every reformatting into a divergence. The `version-gate` rule
//! fails a build over any of the four.
//!
//! # The absence this module defends
//!
//! A version this image cannot replay is never replayed anyway. [`VersionRange::admits`]
//! has two answers — yes, and [`KernelError::IncompatibleWorkflow`] — and no third that
//! means "close enough". §08: a firmware image that cannot replay the recorded version
//! *returns* `IncompatibleWorkflow`; it does not attempt a best-effort replay.

use crate::error::KernelError;

/// Which decision point in a workflow a recorded branch belongs to.
///
/// `u16` and a newtype for [`ActivityKind`](crate::ActivityKind)'s reasons: the number is
/// the wire format, so the encoder reaches the integer directly.
///
/// The number is the author's, chosen once and never reused for a different decision. It
/// is half of a gate's identity; the other half is the sequence the marker occupies, which
/// is what makes §08's "call-order sequencing remains authoritative" true of gates as well
/// as of effects. A gate that moved to another position in the run is caught by the
/// sequence, and two gates that swapped places are caught by this.
///
/// Not [`Ord`]: the numbers are names, so one gate is not less than another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct GateId(pub u16);

/// The recorded workflow versions a firmware image can replay.
///
/// §08's first two rules as one value: `oldest` is the earliest recorded version whose
/// branches this binary still holds, and `current` is the version it writes into a new
/// run's `RunStarted` record.
///
/// # Invariants
///
/// * `oldest <= current`. The fields are private and [`new`](Self::new) is the only way
///   past that check, because an inverted range admits nothing and a run declared under
///   one could never start — a refusal a workflow author would meet at the first boot on a
///   device rather than at the call that built it.
///
/// # Why it is a range rather than a number
///
/// A firmware upgrade that changes what a workflow does has to keep replaying the runs
/// already on media. An image that compared the recorded version for equality could not:
/// every run in flight would be refused the moment the binary changed, which is the
/// opposite of §08's first rule. A range says what an author actually knows — "I still
/// hold the branches back to here" — and [`admits`](Self::admits) is that knowledge as a
/// decision.
///
/// Not [`Ord`]: a range is not a magnitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VersionRange {
    oldest: u16,
    current: u16,
}

impl VersionRange {
    /// A range from `oldest` through `current`, or [`None`] when they are inverted.
    ///
    /// # Postconditions
    ///
    /// `Some` exactly when `oldest <= current`, and then `oldest() == oldest` and
    /// `current() == current`.
    #[must_use]
    pub const fn new(oldest: u16, current: u16) -> Option<Self> {
        if oldest > current {
            return None;
        }
        Some(Self { oldest, current })
    }

    /// A range that admits one version and no other.
    ///
    /// What a workflow that has never been upgraded declares. It is `const` and takes no
    /// [`Option`], because `version <= version` cannot fail and a caller should not have
    /// to unwrap a refusal that does not exist.
    #[must_use]
    pub const fn exact(version: u16) -> Self {
        Self {
            oldest: version,
            current: version,
        }
    }

    /// The earliest recorded version this image can replay.
    #[must_use]
    pub const fn oldest(self) -> u16 {
        self.oldest
    }

    /// The version this image writes into a new run.
    #[must_use]
    pub const fn current(self) -> u16 {
        self.current
    }

    /// Whether this image may replay a run recorded at `recorded`.
    ///
    /// §08's second rule, as a total function. Two causes reach one refusal, and both are
    /// the same fault from the run's point of view — this binary does not hold the code
    /// that run was written against:
    ///
    /// * `recorded > current` is a rollback. The run was written by a newer image, and
    ///   replaying it means executing branches this binary never had.
    /// * `recorded < oldest` is a retirement. The branch was removed on purpose, and
    ///   replaying it means executing whatever replaced it.
    ///
    /// # Errors
    ///
    /// [`KernelError::IncompatibleWorkflow`], whose message is "this firmware cannot
    /// replay this workflow". Never [`NondeterministicWorkflow`](KernelError::NondeterministicWorkflow):
    /// the workflow did not change, this image did.
    ///
    /// # Postconditions
    ///
    /// Total and `const`. It never rewrites `recorded`: the only two answers are "yes" and
    /// a named refusal.
    pub const fn admits(self, recorded: u16) -> Result<(), KernelError> {
        if recorded < self.oldest || recorded > self.current {
            return Err(KernelError::IncompatibleWorkflow);
        }
        Ok(())
    }
}

// `GateId` travels in every marker record, so its width is pinned the way `ActivityKind`'s
// is. `VersionRange` never reaches media — it is what an image declares about itself — so
// only its two ends are pinned, by the record's own golden bytes one layer up.
const _: () = assert!(core::mem::size_of::<GateId>() == 2);
const _: () = assert!(core::mem::align_of::<GateId>() == 2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_its_two_ends() {
        let range = VersionRange::new(1, 4).expect("1 <= 4");
        assert_eq!((range.oldest(), range.current()), (1, 4));
    }

    #[test]
    fn the_ends_are_both_inclusive() {
        // The boundary a comparison written with `<` instead of `<=` gets wrong, in both
        // directions at once.
        let range = VersionRange::new(1, 4).expect("1 <= 4");
        assert_eq!(range.admits(1), Ok(()));
        assert_eq!(range.admits(4), Ok(()));
    }

    #[test]
    fn admits_is_available_in_a_const_context() {
        // `const` so a workflow can declare its range as an associated constant and a
        // firmware can decide compatibility without reaching runtime.
        const RANGE: VersionRange = VersionRange::exact(2);
        const ADMITTED: bool = RANGE.admits(2).is_ok();
        const REFUSED: bool = RANGE.admits(3).is_ok();
        assert_eq!((ADMITTED, REFUSED), (true, false));
    }
}
