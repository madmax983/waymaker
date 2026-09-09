//! Design document §08's workflow-versioning rules, as the kernel states them.
//!
//! Two of §08's four rules live here. "Existing runs must continue under compatible code
//! for their recorded version" is [`VersionRange`], and "a firmware image that cannot
//! replay the recorded version returns `IncompatibleWorkflow`" is
//! [`VersionRange::admits`]. The other two — a recorded gate, and call-order sequencing —
//! are the `VersionMarker` record, in `record.rs`, `replay.rs` and `transition.rs`.

use waymaker_core::KernelError;
use waymaker_core::version::{GateId, VersionRange};

#[test]
fn a_range_admits_every_version_between_its_ends() {
    let range = VersionRange::new(2, 5).expect("2 <= 5");

    for recorded in 2..=5 {
        assert_eq!(
            range.admits(recorded),
            Ok(()),
            "version {recorded} is inside the range"
        );
    }
}

#[test]
fn a_recorded_version_newer_than_this_image_is_incompatible() {
    // The rollback case. This image writes version 5; the run on media was written by an
    // image that knew version 6. Replaying it means executing code this binary does not
    // have, so it refuses rather than taking whichever branch it happens to hold.
    let range = VersionRange::new(2, 5).expect("2 <= 5");

    assert_eq!(range.admits(6), Err(KernelError::IncompatibleWorkflow));
    assert_eq!(
        range.admits(u16::MAX),
        Err(KernelError::IncompatibleWorkflow)
    );
}

#[test]
fn a_recorded_version_older_than_this_image_supports_is_incompatible() {
    // The retirement case. The run was written at version 1 and this image dropped that
    // branch. §08 says the refusal is `IncompatibleWorkflow`, never a best-effort replay
    // under whatever code is left.
    let range = VersionRange::new(2, 5).expect("2 <= 5");

    assert_eq!(range.admits(1), Err(KernelError::IncompatibleWorkflow));
    assert_eq!(range.admits(0), Err(KernelError::IncompatibleWorkflow));
}

#[test]
fn an_inverted_range_cannot_be_built() {
    // The invariant is the type's, not the caller's: a range whose oldest is above its
    // current admits nothing at all, and a run under it could never start.
    assert_eq!(VersionRange::new(6, 5), None);
    assert_eq!(VersionRange::new(1, 0), None);
}

#[test]
fn an_exact_range_admits_one_version() {
    // What a workflow that has never been upgraded declares.
    let range = VersionRange::exact(3);

    assert_eq!(range.oldest(), 3);
    assert_eq!(range.current(), 3);
    assert_eq!(range.admits(3), Ok(()));
    assert_eq!(range.admits(2), Err(KernelError::IncompatibleWorkflow));
    assert_eq!(range.admits(4), Err(KernelError::IncompatibleWorkflow));
}

#[test]
fn a_range_reports_the_ends_it_was_built_from() {
    let range = VersionRange::new(0, u16::MAX).expect("0 <= u16::MAX");

    assert_eq!(range.oldest(), 0);
    assert_eq!(range.current(), u16::MAX);
    // The widest range any image can declare still refuses nothing, which is what makes
    // "admits" a decision a workflow author takes rather than a default.
    assert_eq!(range.admits(0), Ok(()));
    assert_eq!(range.admits(u16::MAX), Ok(()));
}

#[test]
fn a_gate_id_is_the_number_it_was_built_from() {
    // A newtype for `RecordKind`'s reason: the number is the wire format, so the encoder
    // reaches the integer directly rather than through an accessor.
    assert_eq!(GateId(7).0, 7);
    assert_ne!(GateId(7), GateId(8));
}
