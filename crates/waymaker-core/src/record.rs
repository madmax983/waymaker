//! Borrowed record views: what a journal record *is*, with no bytes in sight.
//!
//! Design document §09 gives the journal its record vocabulary and §13 gives the borrowed
//! enum a decoder produces. This module owns both, and owns nothing that touches a byte.
//!
//! # What this module owns
//!
//! [`RecordKind`], the number a record occupies on media, including the numbers §09
//! reserves for records this rung does not decode yet; and [`RecordRef`], the borrowed
//! view of one decoded record. A view holds the caller's bytes rather than a copy of them,
//! which is
//! [`numeric-kinds-and-borrowed-bytes`](https://github.com/madmax983/waymaker/blob/main/docs/adr/0003-the-eight-settled-design-decisions.md#numeric-kinds-and-borrowed-bytes)
//! stated as a type: user payload bytes are opaque to the kernel, so the kernel has
//! nowhere to put them and needs nowhere.
//!
//! # What this module must not own
//!
//! The wire format. Not the magic, not the header, not the CRC, not the padding, not one
//! `u16::from_le_bytes`. The kernel's must-not-own cell names CRC explicitly, and a
//! decoder here would be a serialization framework in the crate the layering says has
//! none. `waymaker-flash` owns the encoding and produces these views from bytes; this
//! module says what the views *are*, which is why a renumbering shows up here as a changed
//! constant rather than as a changed function.
//!
//! # The boundary this module exists to defend
//!
//! A record kind is a number on a device in the field. §09's forward-compatibility rule —
//! "unknown record kinds are skippable only when the format version permits" — only means
//! anything if a number, once spent, stays spent. So [`RecordKind`] names the five records
//! this rung cannot decode as well as the six it can: `TimerScheduled` and `TimerFired`
//! are v0.1-required and arrive with the timer issue, and reserving 5 and 6 for them now
//! is what stops that issue from renumbering `RunCompleted` under firmware that has
//! already written it.

use crate::activity::ActivityKind;
use crate::id::EffectSeq;

/// Which record this is, as the byte on media says it.
///
/// `u8` because §09's frame gives `record_kind` one byte, and a newtype rather than an
/// `enum` for the same reason [`ActivityKind`] is one: the number is the wire format, so
/// the encoder reaches the integer directly and the kernel grows no accessor for it. An
/// `enum` would also be unable to hold a number this firmware does not know, and "a kind I
/// cannot decode" is a thing the format has to be able to talk about.
///
/// The associated constants below are the whole vocabulary of §09's record table,
/// including the numbers this rung does not decode. See the module documentation for why
/// the reserved ones are here.
///
/// Not [`Ord`]: the numbers are positions in a table, so one record is not less than
/// another. [`Hash`] stays, because asking whether two kinds are the same is what [`Eq`]
/// already says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct RecordKind(pub u8);

impl RecordKind {
    /// Workflow identity, version, and bounded input. §09: required at v0.1.
    pub const RUN_STARTED: Self = Self(1);
    /// Durable intent and a stable effect sequence. §09: required at v0.1.
    pub const EFFECT_SCHEDULED: Self = Self(2);
    /// Bounded result bytes returned during replay. §09: required at v0.1.
    pub const EFFECT_COMPLETED: Self = Self(3);
    /// A bounded failure payload. §09: required at v0.1.
    pub const EFFECT_FAILED: Self = Self(4);
    /// Clock capability and deadline semantics. §09: required at v0.1, and reserved here.
    ///
    /// Not decodable yet: its clock-kind field belongs to the timer record issue. The
    /// number is spent now so that issue adds a body rather than a renumbering.
    pub const TIMER_SCHEDULED: Self = Self(5);
    /// A recorded timer completion. §09: required at v0.1, and reserved here for the same
    /// reason as [`TIMER_SCHEDULED`](Self::TIMER_SCHEDULED).
    pub const TIMER_FIRED: Self = Self(6);
    /// Terminal success and a bounded result. §09: required at v0.1.
    pub const RUN_COMPLETED: Self = Self(7);
    /// Terminal workflow failure. §09: required at v0.1.
    pub const RUN_FAILED: Self = Self(8);
    /// A recorded upgrade branch. §09: v0.2, and reserved here.
    pub const VERSION_MARKER: Self = Self(9);
    /// Bounded external activation. §09: later, and reserved here.
    pub const SIGNAL_RECEIVED: Self = Self(10);
    /// Child-workflow composition. §09: not v0.x, and reserved here.
    pub const CHILD_STARTED: Self = Self(11);
}

/// One decoded journal record, borrowing the bytes it was read out of.
///
/// Design document §13. Every payload field is a `&'a [u8]` into the caller's buffer: the
/// kernel does not copy user bytes, because it has no allocator to copy them into and
/// because their meaning is the workflow's rather than the engine's.
///
/// # Invariants
///
/// * A view is what a *valid* record decoded to. Producing one is `waymaker-flash`'s job
///   and its bounds checks are what make the borrowed slices in scope; nothing in this
///   crate can build one out of bytes, because nothing in this crate reads bytes.
/// * [`kind`](Self::kind) is total and agrees with §09's numbering, so the encoder never
///   has to decide which byte a record wears.
/// * [`EffectScheduled`](Self::EffectScheduled) carries a *digest* of the activity input
///   rather than the input itself. §07 orders a durable intent before the effect and §08
///   compares what replay asks for against what history recorded; a length and a CRC are
///   enough to catch a divergent call, and storing the input twice would spend the
///   journal's scarcest resource on bytes the workflow reconstructs anyway. *Which* four
///   fields it carries is design document §16's third deferred question, settled by
///   [ADR 0011](https://github.com/madmax983/waymaker/blob/main/docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md):
///   a sequence, a kind, a length and a digest, and nothing else. A fifth field is 17% more
///   journal on every effect for the life of the format, so the `effect-scheduled-fields`
///   gate rule fails a build that adds one — and one that removes one, because that is a
///   wire-format change on a record firmware in the field has already written.
///
/// # Why the derives stop where they do
///
/// [`PartialEq`] and [`Eq`] are structural, which is what makes a round-trip test one
/// layer up an actual test: `decode(encode(record)) == record` compares the bytes as well
/// as the shape. [`Copy`] because a view is a fat pointer and two numbers, and a cursor
/// that had to clone one would be a cursor with a lifetime problem it does not need.
/// Not [`Hash`], not [`Ord`]: a record is neither keyed nor sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordRef<'a> {
    /// A run began: which workflow, at which version, on which input.
    RunStarted {
        /// The workflow this run executes, as the bank header pins it.
        workflow_kind: u16,
        /// The version of that workflow, which a firmware image may refuse to replay.
        workflow_version: u16,
        /// The run's input, opaque to the kernel.
        input: &'a [u8],
    },
    /// An effect was scheduled: durable intent, before the physical effect.
    EffectScheduled {
        /// Where this effect falls in the run's history.
        seq: EffectSeq,
        /// Which activity the dispatcher is to run.
        kind: ActivityKind,
        /// How many bytes of input the call passed.
        input_len: u16,
        /// A digest of those bytes, for the divergence check in §08.
        input_crc: u32,
    },
    /// An effect completed, with the bytes replay will hand back.
    EffectCompleted {
        /// The effect this outcome belongs to.
        seq: EffectSeq,
        /// The activity's result, opaque to the kernel.
        result: &'a [u8],
    },
    /// An effect failed, with a bounded failure payload.
    EffectFailed {
        /// The effect this outcome belongs to.
        seq: EffectSeq,
        /// The failure payload, opaque to the kernel.
        error: &'a [u8],
    },
    /// The run finished successfully. Terminal.
    RunCompleted {
        /// The workflow's result, opaque to the kernel.
        result: &'a [u8],
    },
    /// The run failed. Terminal.
    RunFailed {
        /// The failure payload, opaque to the kernel.
        error: &'a [u8],
    },
}

impl RecordRef<'_> {
    /// The number this record occupies on media.
    ///
    /// # Postconditions
    ///
    /// Total, `const`, and one of the six decodable constants on [`RecordKind`] — never a
    /// reserved one, because no reserved kind has a variant to be reached from. The
    /// encoder writes what this returns, so the mapping lives once: a completion cannot
    /// go to media wearing a failure's byte.
    #[must_use]
    pub const fn kind(&self) -> RecordKind {
        match self {
            Self::RunStarted { .. } => RecordKind::RUN_STARTED,
            Self::EffectScheduled { .. } => RecordKind::EFFECT_SCHEDULED,
            Self::EffectCompleted { .. } => RecordKind::EFFECT_COMPLETED,
            Self::EffectFailed { .. } => RecordKind::EFFECT_FAILED,
            Self::RunCompleted { .. } => RecordKind::RUN_COMPLETED,
            Self::RunFailed { .. } => RecordKind::RUN_FAILED,
        }
    }
}

// `RecordKind` travels in every record, so its width is pinned the same way
// `ActivityKind`'s is. `RecordRef` holds a fat pointer, so its size is target-dependent
// and is budgeted through `kernel_state_types!` rather than pinned to a literal here.
const _: () = assert!(core::mem::size_of::<RecordKind>() == 1);
const _: () = assert!(core::mem::align_of::<RecordKind>() == 1);

#[cfg(test)]
mod tests {
    use super::*;

    /// Every decodable kind, in §09's table order, by exhaustive `match`.
    ///
    /// A variant added to `RecordRef` without a number is a compile error here rather than
    /// a record that encodes as whatever the last arm happened to return.
    const fn number_of(record: &RecordRef<'_>) -> u8 {
        match record {
            RecordRef::RunStarted { .. } => 1,
            RecordRef::EffectScheduled { .. } => 2,
            RecordRef::EffectCompleted { .. } => 3,
            RecordRef::EffectFailed { .. } => 4,
            RecordRef::RunCompleted { .. } => 5,
            RecordRef::RunFailed { .. } => 6,
        }
    }

    /// One of each variant, for a test that wants to walk them all.
    const EVERY_VARIANT: [RecordRef<'static>; 6] = [
        RecordRef::RunStarted {
            workflow_kind: 0x1234,
            workflow_version: 2,
            input: b"in",
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq(11),
            kind: ActivityKind(3),
            input_len: 2,
            input_crc: 0x0BAD_F00D,
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(11),
            result: b"out",
        },
        RecordRef::EffectFailed {
            seq: EffectSeq(11),
            error: b"err",
        },
        RecordRef::RunCompleted { result: b"done" },
        RecordRef::RunFailed { error: b"bad" },
    ];

    #[test]
    fn the_variant_list_is_complete() {
        // Every position in `0..len` appears once, in order, which is only possible when
        // the array holds each variant the `match` knows exactly once.
        assert!(
            EVERY_VARIANT
                .iter()
                .map(number_of)
                .eq(1..=u8::try_from(EVERY_VARIANT.len()).unwrap())
        );
    }

    #[test]
    fn each_variant_reports_its_own_kind() {
        // Six distinct kinds over six variants: one arm returning another's constant
        // shows up as a duplicate rather than as a value nobody looked at.
        let kinds: [RecordKind; 6] = [
            EVERY_VARIANT[0].kind(),
            EVERY_VARIANT[1].kind(),
            EVERY_VARIANT[2].kind(),
            EVERY_VARIANT[3].kind(),
            EVERY_VARIANT[4].kind(),
            EVERY_VARIANT[5].kind(),
        ];

        for (left_index, left) in kinds.iter().enumerate() {
            for (right_index, right) in kinds.iter().enumerate() {
                assert_eq!(
                    left_index == right_index,
                    left == right,
                    "two variants report kind {left:?}"
                );
            }
        }
        assert_eq!(kinds[0], RecordKind::RUN_STARTED);
        assert_eq!(kinds[5], RecordKind::RUN_FAILED);
    }

    #[test]
    fn kind_is_available_in_a_const_context() {
        // `const` so the encoder can be, and so a table of expected kinds costs nothing at
        // runtime.
        const KIND: RecordKind = RecordRef::RunCompleted { result: &[] }.kind();
        assert_eq!(KIND, RecordKind::RUN_COMPLETED);
    }

    #[test]
    fn an_empty_payload_is_a_payload() {
        // Zero-length is the boundary the encoder and the scanner both have to survive: a
        // run can complete with no result at all, and that record still has to be a
        // record.
        let empty = RecordRef::RunCompleted { result: &[] };
        assert_eq!(empty.kind(), RecordKind::RUN_COMPLETED);
        assert_ne!(empty, RecordRef::RunCompleted { result: b"x" });
    }
}
