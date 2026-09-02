//! The kernel's error vocabulary, tested through the surface adapters see.
//!
//! The errors are the kernel's way of refusing work it cannot do safely — design document
//! §08 and §10 — so they are part of the contract rather than an implementation detail.
//! Neither enum is `#[non_exhaustive]` at 0.x: every adapter in this workspace matches
//! them exhaustively on purpose, so an added variant should break a build rather than fall
//! into a wildcard arm.

use core::error::Error;

use waymaker_core::{DecodeError, KernelError};

/// Every `DecodeError`, listed so that a variant added without a message fails this file.
const DECODE_ERRORS: [DecodeError; 5] = [
    DecodeError::Truncated,
    DecodeError::LengthOutOfBounds,
    DecodeError::UnknownRecordKind,
    DecodeError::UnsupportedFormatVersion,
    DecodeError::IntegrityFailed,
];

/// Every `KernelError`, with one wrapped decode failure standing for the `Decode` arm.
const KERNEL_ERRORS: [KernelError; 5] = [
    KernelError::IdExhausted,
    KernelError::HistoryNearCapacity,
    KernelError::NondeterministicWorkflow,
    KernelError::IncompatibleWorkflow,
    KernelError::Decode(DecodeError::IntegrityFailed),
];

#[test]
fn errors_display_their_message() {
    // `Display` is the message and nothing else: no interpolation, no allocation, no
    // second copy of the wording to drift from the first. `to_string` is available here
    // because integration tests link `std`; the kernel itself never calls it.
    for error in DECODE_ERRORS {
        assert_eq!(error.to_string(), error.message(), "{error:?}");
    }
    for error in KERNEL_ERRORS {
        assert_eq!(error.to_string(), error.message(), "{error:?}");
    }
}

#[test]
fn an_error_is_a_core_error() {
    // `core::error::Error` rather than `std::error::Error`: the kernel is `no_std`, and
    // the point of implementing it is that a host adapter can put these in a `Box<dyn
    // Error>` without the kernel knowing that hosts exist.
    for error in DECODE_ERRORS {
        let erased: &dyn Error = &error;
        assert!(erased.source().is_none(), "{error:?}");
        assert_eq!(erased.to_string(), error.message());
    }
    for error in KERNEL_ERRORS {
        let erased: &dyn Error = &error;
        // `Decode` wraps a `DecodeError` but does not report it as a source: the message
        // already passes through, and a source chain the kernel cannot allocate to walk
        // would be a courtesy to nobody.
        assert!(erased.source().is_none(), "{error:?}");
        assert_eq!(erased.to_string(), error.message());
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
    assert_eq!(core::mem::size_of::<KernelError>(), 1);
}
