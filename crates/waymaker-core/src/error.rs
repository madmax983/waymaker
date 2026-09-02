//! The vocabulary the kernel refuses work in.
//!
//! An error here is the kernel declining to do something it cannot do safely, which is
//! part of the contract rather than an implementation detail: design document §07's
//! terminal sequence condition, §08's two replay refusals, §10's "ordinary effect
//! scheduling fails early with `HistoryNearCapacity`; the runtime never overwrites
//! committed history to make room", and §14's bounded decoding.
//!
//! # What this module owns
//!
//! The *vocabulary* — the names of the outcomes, so that both layers and the façade above
//! them speak one set of errors. [`KernelError`] for the engine's refusals and
//! [`DecodeError`] for the ways a record can fail to be read within its bounds.
//!
//! # What this module must not own
//!
//! The decoding, and the CRC. §14 requires that "malformed storage cannot cause
//! out-of-bounds reads or allocation", and the code that checks a seal and walks a frame
//! lives in `waymaker-flash`, which owns the wire encoding — the kernel's own must-not-own
//! cell names CRC explicitly. Core names the outcomes; core computes none of them. So
//! there is no `decode` function here, and a [`DecodeError`] in this crate is only ever a
//! value the layer above constructed.
//!
//! # Neither enum is `#[non_exhaustive]`
//!
//! Deliberately, at 0.x. Every adapter that matches these lives in this workspace, and an
//! exhaustive match is how the compiler tells whoever adds a variant which call sites now
//! have a case to think about. `#[non_exhaustive]` would replace that list with a wildcard
//! arm that silently absorbs it. See
//! [ADR 0006](https://github.com/madmax983/waymaker/blob/main/docs/adr/0006-effect-identity-is-newtypes-and-exhaustion-is-terminal.md).
//!
//! # Why `Display` is so plain
//!
//! Each message is a short static literal written straight through
//! [`core::fmt::Formatter::write_str`], with no interpolation and no context in it: no
//! sequence number in [`KernelError::IdExhausted`], no offset in
//! [`DecodeError::Truncated`]. A single `write!` with an argument pulls `core::fmt::write`
//! and its formatting machinery into an image with an 8 KiB incremental code-flash budget,
//! and it would be paid for a string nobody reads on a device with no console. A caller
//! that wants the values has them in hand at the point it built the error, and
//! [`KernelError::message`] hands over the same text with no formatter involved.

/// A record could not be read within its bounds.
///
/// # Invariants
///
/// This is a vocabulary, not a decoder. Every variant names an outcome that
/// `waymaker-flash` produces; nothing in this crate can construct one from a byte slice,
/// because nothing in this crate reads bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeError {
    /// The input ended before the frame it claimed to hold.
    Truncated,
    /// A length field points past the buffer or the caller-owned output.
    LengthOutOfBounds,
    /// A record kind this kernel does not know.
    UnknownRecordKind,
    /// A format version this firmware cannot replay.
    UnsupportedFormatVersion,
    /// A frame is intact and is still not the record it names.
    ///
    /// Distinct from [`Truncated`](Self::Truncated), which is about the *input* ending
    /// early, and from [`LengthOutOfBounds`](Self::LengthOutOfBounds), which is about a
    /// length reaching outside a buffer. This one is a frame whose checksums hold and
    /// whose body does not fit the kind in its header: a schedule record whose body is not
    /// the fixed size a schedule record has, or a run-scoped record carrying an effect
    /// sequence the encoder never writes. A decoder that read on anyway would be reading
    /// fields that are not there.
    MalformedRecord,
    /// A seal or digest did not match its bytes; the check itself lives in the adapter.
    ///
    /// A frame whose leading magic does not match is reported here too, rather than through
    /// a variant of its own: the magic is the cheapest seal a frame carries, and a reader
    /// that could tell a wrong magic from a failed CRC would learn nothing it could act on.
    /// Either way the bytes are not the record they claim to be, and the adapter stops.
    IntegrityFailed,
}

impl DecodeError {
    /// A short static description of this failure.
    ///
    /// # Postconditions
    ///
    /// Non-empty, ASCII, distinct from every other variant's, and shorter than a firmware
    /// log line — so a refusal is diagnosable on a device with no debugger attached, where
    /// two variants sharing a string would mean a log that cannot say which of two
    /// different failures happened. It is the same text [`Display`](core::fmt::Display)
    /// writes, obtained without a formatter.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Truncated => "the input ended before the frame it claimed",
            Self::LengthOutOfBounds => "a length field points past its buffer",
            Self::UnknownRecordKind => "an unknown record kind",
            Self::UnsupportedFormatVersion => "an unsupported record format version",
            Self::MalformedRecord => "a record body does not fit its kind",
            Self::IntegrityFailed => "a seal did not match the bytes it covers",
        }
    }
}

/// The kernel declining to do something it cannot do safely.
///
/// # Invariants
///
/// Every variant is a refusal rather than a retry hint. [`IdExhausted`](Self::IdExhausted)
/// and the two replay refusals are terminal for the run; §08 is explicit that a replay
/// mismatch stops rather than guesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KernelError {
    /// The run's 32-bit effect sequence space is spent. Terminal for the run: the way out
    /// is `continue_as_new`, and wraparound is not an option — see
    /// [`EffectIdAllocator`](crate::EffectIdAllocator).
    IdExhausted,
    /// History has reached the reserved tail: only a terminal record or `continue_as_new`
    /// still fits. §10 — the runtime never overwrites committed history to make room.
    HistoryNearCapacity,
    /// Replay met a different kind, digest or sequence than history recorded. §08 — stop,
    /// never guess.
    NondeterministicWorkflow,
    /// This firmware cannot replay the workflow kind and version the bank header pins.
    IncompatibleWorkflow,
    /// The committed records are not a legal history of any run.
    ///
    /// §09 stops recovery at "the first unsealed, malformed, out-of-sequence, or
    /// integrity-failed frame", and this is the third of those four. It is a fact about
    /// how records sit *beside each other* rather than about any one frame, so it cannot
    /// be a [`DecodeError`]: every frame involved decoded perfectly. An outcome with no
    /// schedule before it, a schedule while an earlier effect is unresolved, an effect
    /// sequence that skips or repeats, a second `RunStarted`, a record after a terminal
    /// one — each is history that no execution could have produced.
    ///
    /// Distinct from [`NondeterministicWorkflow`](Self::NondeterministicWorkflow), which
    /// is the *workflow* disagreeing with history that is itself sound. Two different
    /// faults with two different causes: one is a damaged or forged journal, the other is
    /// changed code. A firmware log line that could not tell them apart would send an
    /// engineer to the wrong place.
    MalformedHistory,
    /// A record could not be decoded within its bounds.
    Decode(DecodeError),
}

impl KernelError {
    /// A short static description of this failure.
    ///
    /// # Postconditions
    ///
    /// Non-empty, ASCII, distinct from every other variant's, and shorter than a firmware
    /// log line. [`Decode`](Self::Decode) passes the wrapped
    /// [`DecodeError::message`] straight through: wrapping must not blur the diagnosis, so
    /// a `Decode` says exactly what the decoder said and the wrapper costs the reader
    /// nothing.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::IdExhausted => "the run's effect sequence space is spent",
            Self::HistoryNearCapacity => "history has reached its reserved tail",
            Self::NondeterministicWorkflow => "replay diverged from recorded history",
            Self::IncompatibleWorkflow => "this firmware cannot replay this workflow",
            Self::MalformedHistory => "committed history is not a legal record sequence",
            Self::Decode(error) => error.message(),
        }
    }
}

impl core::fmt::Display for DecodeError {
    /// Writes [`DecodeError::message`] and nothing else.
    ///
    /// [`core::fmt::Formatter::write_str`] rather than `write!`: an argument would pull
    /// `core::fmt::write` into an image with an 8 KiB incremental budget.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::fmt::Display for KernelError {
    /// Writes [`KernelError::message`] and nothing else, for the same reason as
    /// [`DecodeError`]'s.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

// `core::error::Error`, not `std::error::Error`: the kernel is `no_std`, and the point of
// implementing it is that a host adapter can put these in a `Box<dyn Error>` without the
// kernel knowing that hosts exist. Neither impl overrides `source`. `KernelError::Decode`
// wraps a `DecodeError` but does not report it as a source: the message already passes
// through, and a source chain the kernel cannot allocate to walk would be a courtesy to
// nobody.
impl core::error::Error for DecodeError {}

impl core::error::Error for KernelError {}

impl From<DecodeError> for KernelError {
    /// Wraps a decode failure as [`KernelError::Decode`].
    ///
    /// # Postconditions
    ///
    /// `KernelError::from(error) == KernelError::Decode(error)`, and the message passes
    /// through unchanged. This exists so that `?` carries a failure across the seam where
    /// the adapter decodes and the kernel returns, rather than a hand-written match at
    /// every call site.
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

// An error travels in every `Result` the kernel returns, so its width is a budget item,
// and a widened enum should fail at compile time rather than in a report. `KernelError` is
// allowed two bytes here and observed at one — its own variants fit in the discriminant
// values `DecodeError` leaves spare, so `Decode` is free. That is an optimisation rather
// than a guarantee, which is why only the integration test insists on one.
const _: () = assert!(core::mem::size_of::<DecodeError>() == 1);
const _: () = assert!(core::mem::size_of::<KernelError>() <= 2);

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `DecodeError`, in declaration order.
    ///
    /// A fixed-length array cannot notice a variant that was never put in it, so the list
    /// is not trusted on its own: `the_variant_lists_are_complete` maps each entry through
    /// an exhaustive `match` and fails the moment the enum has an arm this array does not.
    const DECODE_ERRORS: [DecodeError; 6] = [
        DecodeError::Truncated,
        DecodeError::LengthOutOfBounds,
        DecodeError::UnknownRecordKind,
        DecodeError::UnsupportedFormatVersion,
        DecodeError::MalformedRecord,
        DecodeError::IntegrityFailed,
    ];

    /// Every `KernelError`, with one wrapped decode failure standing for the `Decode` arm,
    /// kept complete the same way as [`DECODE_ERRORS`].
    const KERNEL_ERRORS: [KernelError; 6] = [
        KernelError::IdExhausted,
        KernelError::HistoryNearCapacity,
        KernelError::NondeterministicWorkflow,
        KernelError::IncompatibleWorkflow,
        KernelError::MalformedHistory,
        KernelError::Decode(DecodeError::IntegrityFailed),
    ];

    /// The longest a message may be, so that it fits a firmware log line whole.
    const MESSAGE_LIMIT: usize = 60;

    /// The position of each `DecodeError` in [`DECODE_ERRORS`], by exhaustive `match`.
    ///
    /// Adding a variant without extending the array forces a new arm here, and the only
    /// index it can be given is one the array does not have.
    const fn decode_index(error: DecodeError) -> usize {
        match error {
            DecodeError::Truncated => 0,
            DecodeError::LengthOutOfBounds => 1,
            DecodeError::UnknownRecordKind => 2,
            DecodeError::UnsupportedFormatVersion => 3,
            DecodeError::MalformedRecord => 4,
            DecodeError::IntegrityFailed => 5,
        }
    }

    /// The position of each `KernelError` in [`KERNEL_ERRORS`], by exhaustive `match`.
    const fn kernel_index(error: KernelError) -> usize {
        match error {
            KernelError::IdExhausted => 0,
            KernelError::HistoryNearCapacity => 1,
            KernelError::NondeterministicWorkflow => 2,
            KernelError::IncompatibleWorkflow => 3,
            KernelError::MalformedHistory => 4,
            KernelError::Decode(_) => 5,
        }
    }

    #[test]
    fn the_variant_lists_are_complete() {
        // Every index in `0..len` is produced exactly once, in order, which is only
        // possible when the array lists each variant the `match` knows once. A variant the
        // enum gains is a compile error in the `match`; an entry the array loses is a
        // failure here.
        assert!(
            DECODE_ERRORS
                .iter()
                .copied()
                .map(decode_index)
                .eq(0..DECODE_ERRORS.len())
        );
        assert!(
            KERNEL_ERRORS
                .iter()
                .copied()
                .map(kernel_index)
                .eq(0..KERNEL_ERRORS.len())
        );
    }

    #[test]
    fn every_message_is_non_empty_and_distinct() {
        // Distinct messages are what makes a refusal diagnosable on a device with no
        // debugger attached: two variants sharing a string means a log line that cannot
        // say which of two different failures happened.
        for (left_index, left) in DECODE_ERRORS.iter().enumerate() {
            let message = left.message();
            assert!(!message.is_empty(), "{left:?} has no message");
            assert!(message.len() < MESSAGE_LIMIT, "{left:?}: {message}");

            for (right_index, right) in DECODE_ERRORS.iter().enumerate() {
                assert!(
                    (left_index == right_index) == (message == right.message()),
                    "{left:?} and {right:?} share a message"
                );
            }
        }

        for (left_index, left) in KERNEL_ERRORS.iter().enumerate() {
            let message = left.message();
            assert!(!message.is_empty(), "{left:?} has no message");
            assert!(message.len() < MESSAGE_LIMIT, "{left:?}: {message}");

            for (right_index, right) in KERNEL_ERRORS.iter().enumerate() {
                assert!(
                    (left_index == right_index) == (message == right.message()),
                    "{left:?} and {right:?} share a message"
                );
            }
        }
    }

    #[test]
    fn decode_converts_into_kernel_error() {
        // `From` rather than a hand-written match at every call site: the adapter decodes
        // and the kernel returns, and `?` is what carries the failure across that seam.
        for error in DECODE_ERRORS {
            assert_eq!(KernelError::from(error), KernelError::Decode(error));

            let converted: KernelError = error.into();
            assert_eq!(converted, KernelError::Decode(error));
        }
    }

    #[test]
    fn a_decode_error_message_passes_through() {
        // Wrapping must not blur the diagnosis: a `Decode` says exactly what the decoder
        // said, so the wrapper costs the reader nothing.
        for error in DECODE_ERRORS {
            assert_eq!(KernelError::Decode(error).message(), error.message());
        }
    }
}
