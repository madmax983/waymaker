//! The kernel's error vocabulary, tested through the surface adapters see.
//!
//! The errors are the kernel's way of refusing work it cannot do safely — design document
//! §08 and §10 — so they are part of the contract rather than an implementation detail.
//! Neither enum is `#[non_exhaustive]` at 0.x: every adapter in this workspace matches
//! them exhaustively on purpose, so an added variant should break a build rather than fall
//! into a wildcard arm.

use core::error::Error;

use waymaker_core::{DecodeError, KernelError};

/// Every `DecodeError` beside the exact text it must report.
///
/// The text is pinned here, in a second place, on purpose: `message()` and `Display` agree
/// by construction, so comparing one with the other could not notice two variants whose
/// strings had been swapped — and a swapped message is a firmware log line naming the wrong
/// refusal.
const DECODE_ERRORS: [(DecodeError, &str); 6] = [
    (
        DecodeError::Truncated,
        "the input ended before the frame it claimed",
    ),
    (
        DecodeError::LengthOutOfBounds,
        "a length field points past its buffer",
    ),
    (DecodeError::UnknownRecordKind, "an unknown record kind"),
    (
        DecodeError::UnsupportedFormatVersion,
        "an unsupported record format version",
    ),
    (
        DecodeError::MalformedRecord,
        "a record body does not fit its kind",
    ),
    (
        DecodeError::IntegrityFailed,
        "a seal did not match the bytes it covers",
    ),
];

/// Every `KernelError` beside its exact text, with one wrapped decode failure standing for
/// the `Decode` arm — whose text is the decoder's own, passed through unchanged.
const KERNEL_ERRORS: [(KernelError, &str); 6] = [
    (
        KernelError::IdExhausted,
        "the run's effect sequence space is spent",
    ),
    (
        KernelError::HistoryNearCapacity,
        "history has reached its reserved tail",
    ),
    (
        KernelError::NondeterministicWorkflow,
        "replay diverged from recorded history",
    ),
    (
        KernelError::MalformedHistory,
        "committed history is not a legal record sequence",
    ),
    (
        KernelError::IncompatibleWorkflow,
        "this firmware cannot replay this workflow",
    ),
    (
        KernelError::Decode(DecodeError::IntegrityFailed),
        "a seal did not match the bytes it covers",
    ),
];

#[test]
fn errors_display_their_message() {
    // `Display` is the message and nothing else: no interpolation, no allocation, no
    // second copy of the wording to drift from the first. `to_string` is available here
    // because integration tests link `std`; the kernel itself never calls it.
    for (error, expected) in DECODE_ERRORS {
        assert_eq!(error.message(), expected, "{error:?}");
        assert_eq!(error.to_string(), expected, "{error:?}");
    }
    for (error, expected) in KERNEL_ERRORS {
        assert_eq!(error.message(), expected, "{error:?}");
        assert_eq!(error.to_string(), expected, "{error:?}");
    }
}

#[test]
fn an_error_is_a_core_error() {
    // `core::error::Error` rather than `std::error::Error`: the kernel is `no_std`, and
    // the point of implementing it is that a host adapter can put these in a `Box<dyn
    // Error>` without the kernel knowing that hosts exist.
    for (error, expected) in DECODE_ERRORS {
        let erased: &dyn Error = &error;
        assert!(erased.source().is_none(), "{error:?}");
        assert_eq!(erased.to_string(), expected);
    }
    for (error, expected) in KERNEL_ERRORS {
        let erased: &dyn Error = &error;
        // `Decode` wraps a `DecodeError` but does not report it as a source: the message
        // already passes through, and a source chain the kernel cannot allocate to walk
        // would be a courtesy to nobody.
        assert!(erased.source().is_none(), "{error:?}");
        assert_eq!(erased.to_string(), expected);
    }
}

#[test]
fn the_error_enums_stay_small() {
    // An error travels in every `Result` the kernel returns, so its width is a budget
    // item. Asserted at compile time because that is when a widened enum should fail.
    const {
        assert!(core::mem::size_of::<DecodeError>() == 1);
        assert!(core::mem::size_of::<KernelError>() <= 2);
    }

    // The observed figure on the pinned toolchain is one byte: `KernelError`'s own
    // variants fit in the discriminant values `DecodeError` leaves spare, so `Decode` is
    // free. That is an optimisation rather than a guarantee, which is why the compile-time
    // pin above allows two and only this runtime check insists on one.
    assert_eq!(
        core::mem::size_of::<KernelError>(),
        1,
        "one byte on the pinned toolchain; a layout change here is a toolchain bump to \
         review, not a bug"
    );
}
