//! The borrowed record view, tested through the surface an adapter sees.
//!
//! Design document §09 gives the journal's record vocabulary and §13 gives the borrowed
//! enum a decoder produces. The kernel owns the *view* and the numbers; it owns no bytes,
//! so nothing here encodes or decodes anything — that is `waymaker-flash`, one layer up.
//!
//! What these tests are for is the half of the wire format that is frozen by being
//! *named*: a record kind's number is on media for the life of a device, so a renumbering
//! is a format break dressed up as a refactor. Every number below is therefore written out
//! as a literal rather than derived from the constant it checks.

use waymaker_core::{ActivityKind, EffectSeq, RecordKind, RecordRef};

/// Every record kind this firmware can decode, beside the number it occupies on media.
///
/// The literals are the point. Comparing `RecordKind::RUN_STARTED` with itself would pass
/// under any renumbering; comparing it with `1` fails the moment the wire format moves.
const DECODABLE_KINDS: [(RecordKind, u8); 6] = [
    (RecordKind::RUN_STARTED, 1),
    (RecordKind::EFFECT_SCHEDULED, 2),
    (RecordKind::EFFECT_COMPLETED, 3),
    (RecordKind::EFFECT_FAILED, 4),
    (RecordKind::RUN_COMPLETED, 7),
    (RecordKind::RUN_FAILED, 8),
];

/// Numbers §09's record table claims that this rung does not yet decode.
///
/// Reserved rather than free: a later issue fills the body in behind the same number, so
/// the format never has to renumber a record that firmware in the field has already
/// written.
const RESERVED_KINDS: [(RecordKind, u8); 5] = [
    (RecordKind::TIMER_SCHEDULED, 5),
    (RecordKind::TIMER_FIRED, 6),
    (RecordKind::VERSION_MARKER, 9),
    (RecordKind::SIGNAL_RECEIVED, 10),
    (RecordKind::CHILD_STARTED, 11),
];

#[test]
fn every_record_kind_holds_the_number_the_wire_format_pins() {
    for (kind, number) in DECODABLE_KINDS {
        assert_eq!(kind.0, number, "{kind:?}");
    }
    for (kind, number) in RESERVED_KINDS {
        assert_eq!(kind.0, number, "{kind:?}");
    }
}

#[test]
fn zero_is_not_a_record_kind() {
    // An erased NOR page reads back as `0xFF` and a zeroed one as `0x00`. Neither is a
    // record, so neither number is spent on one: leaving zero unassigned means a run of
    // zeroed bytes cannot name a record even before any checksum is consulted.
    for (kind, _) in DECODABLE_KINDS {
        assert_ne!(kind.0, 0, "{kind:?}");
        assert_ne!(kind.0, 0xFF, "{kind:?}");
    }
    for (kind, _) in RESERVED_KINDS {
        assert_ne!(kind.0, 0, "{kind:?}");
        assert_ne!(kind.0, 0xFF, "{kind:?}");
    }
}

#[test]
fn no_two_record_kinds_share_a_number() {
    // Two records on one number is a journal that cannot be replayed: the decoder would
    // take the first and read the second's body under the wrong rules.
    let all: [(RecordKind, u8); 11] = [
        DECODABLE_KINDS[0],
        DECODABLE_KINDS[1],
        DECODABLE_KINDS[2],
        DECODABLE_KINDS[3],
        DECODABLE_KINDS[4],
        DECODABLE_KINDS[5],
        RESERVED_KINDS[0],
        RESERVED_KINDS[1],
        RESERVED_KINDS[2],
        RESERVED_KINDS[3],
        RESERVED_KINDS[4],
    ];

    for (left_index, (left, _)) in all.iter().enumerate() {
        for (right_index, (right, _)) in all.iter().enumerate() {
            assert_eq!(
                left_index == right_index,
                left.0 == right.0,
                "{left:?} and {right:?} share number {}",
                left.0
            );
        }
    }
}

#[test]
fn a_record_reports_the_kind_it_is() {
    // The encoder asks a record which number to write, and a mismatch here would put a
    // completion on media wearing a failure's kind byte — decodable, self-consistent, and
    // wrong.
    let cases: [(RecordRef<'_>, RecordKind); 6] = [
        (
            RecordRef::RunStarted {
                workflow_kind: 7,
                workflow_version: 1,
                input: b"input",
            },
            RecordKind::RUN_STARTED,
        ),
        (
            RecordRef::EffectScheduled {
                seq: EffectSeq(3),
                kind: ActivityKind(9),
                input_len: 5,
                input_crc: 0xDEAD_BEEF,
            },
            RecordKind::EFFECT_SCHEDULED,
        ),
        (
            RecordRef::EffectCompleted {
                seq: EffectSeq(3),
                result: b"ok",
            },
            RecordKind::EFFECT_COMPLETED,
        ),
        (
            RecordRef::EffectFailed {
                seq: EffectSeq(3),
                error: b"no",
            },
            RecordKind::EFFECT_FAILED,
        ),
        (
            RecordRef::RunCompleted { result: b"done" },
            RecordKind::RUN_COMPLETED,
        ),
        (
            RecordRef::RunFailed { error: b"bad" },
            RecordKind::RUN_FAILED,
        ),
    ];

    for (record, expected) in cases {
        assert_eq!(record.kind(), expected, "{record:?}");
    }
}

#[test]
fn a_record_borrows_its_payload_rather_than_owning_it() {
    // The whole point of the borrowed view: user payload bytes stay where the reader put
    // them. A view that copied would need somewhere to copy to, and the kernel has no
    // allocator and no buffer of its own.
    let page = [1_u8, 2, 3, 4, 5, 6, 7, 8];

    let RecordRef::EffectCompleted { result, .. } = (RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: &page,
    }) else {
        unreachable!("the record was built as a completion")
    };

    assert_eq!(result.as_ptr(), page.as_ptr());
    assert_eq!(result.len(), page.len());
}

#[test]
fn the_record_view_fits_the_kernel_state_budget() {
    // The view is live while the cursor resolves one record, so it is charged against the
    // 128 B in design document §04 rather than assumed free. Asserted at compile time,
    // because the size of a type is a compile-time fact.
    const {
        assert!(size_of::<RecordRef<'static>>() <= waymaker_core::budget::KERNEL_STATE_BYTES);
    }

    assert!(
        waymaker_core::budget::KERNEL_STATE_TYPES
            .iter()
            .any(|entry| entry.name.contains("RecordRef")),
        "the record view is live kernel state, so it belongs in the registry the size \
         report reads"
    );
}

#[test]
fn records_compare_by_value() {
    // Round-trip tests one layer up assert `decode(encode(record)) == record`, which is
    // only a test if equality is structural. Two completions with the same sequence and
    // different bytes must not be equal.
    let left = RecordRef::EffectCompleted {
        seq: EffectSeq(1),
        result: b"a",
    };
    let same = RecordRef::EffectCompleted {
        seq: EffectSeq(1),
        result: b"a",
    };
    let other_bytes = RecordRef::EffectCompleted {
        seq: EffectSeq(1),
        result: b"b",
    };
    let other_seq = RecordRef::EffectCompleted {
        seq: EffectSeq(2),
        result: b"a",
    };
    let other_kind = RecordRef::EffectFailed {
        seq: EffectSeq(1),
        error: b"a",
    };

    assert_eq!(left, same);
    assert_ne!(left, other_bytes);
    assert_ne!(left, other_seq);
    assert_ne!(left, other_kind);
}
