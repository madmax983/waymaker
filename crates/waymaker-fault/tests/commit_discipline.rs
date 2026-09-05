//! Issue #24's exit criterion: never "sealed but incomplete", at every crash point.
//!
//! Issue [#24](https://github.com/madmax983/waymaker/issues/24): "Every crash point in the
//! write sequence recovers to either *frame absent* or *frame committed* — never *sealed but
//! incomplete*." That is a statement about media rather than about a reader, so it is
//! checked on media: for every image a crash left behind, at every record slot, **if the
//! commit seal landed then the frame it seals landed whole**.
//!
//! The writer under test is the real one —
//! [`waymaker_flash::append::Journal`] — driven through the fault
//! harness with nothing modelled. The frame bodies, the seals, the two barriers and the
//! crash points between them are all the real thing.
//!
//! # The teeth
//!
//! [`a_writer_that_seals_before_it_writes_leaves_a_sealed_incomplete_frame`] is the mutant,
//! and it is the reason this file exists rather than an assertion in `tests/recovery.rs`: a
//! writer that programs the seal before the frame body produces exactly the state the issue
//! forbids, at a crash point the injector finds. It has to reach around
//! [`waymaker_flash::append`] to the session to do it, because the writer API has no way to
//! express it — which is the guarantee, demonstrated from the outside.
//!
//! # What this file cannot falsify
//!
//! That the *payload barrier* is necessary. §12 requires that "no later mutation may become
//! durable before mutations ordered by a completed barrier", and a writer that programmed
//! the body and the seal back to back with no barrier between them would be relying on the
//! device not to reorder them. [`waymaker_fault::Device`] does not reorder: it applies
//! programs in the order they are issued, so a sweep against it cannot tell that writer from
//! the correct one. Modelling store reordering is a different harness, and stating the gap
//! is what stops this file from being read as proof of something it does not check. What it
//! *does* check is the other half — that the seal never lands over an incomplete frame —
//! and §07's barrier is what a conforming device needs to keep that true.

use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_fault::{Device, FaultError, Harness, RecordId, Run, Session, verify_recovery};
use waymaker_flash::append::{AppendError, Journal};
use waymaker_flash::frame::{self, ERASED_BYTE, ProgramAlign};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

/// One erase block, which is the whole journal region.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("256 is one whole 256-byte block of 4-byte units")
    };
    geometry
}

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

fn region() -> JournalRegion {
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 256, align()) else {
        unreachable!("the whole device is a legal program")
    };
    region
}

/// The activity every schedule record names.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// How many records the writer appends.
const RECORDS: u32 = 2;

/// The page a device this size would recover and stage with.
const PAGE: usize = 64;

/// The record at index `index` of the fixture history.
const fn record(index: u32) -> RecordRef<'static> {
    if index % 2 == 0 {
        RecordRef::EffectScheduled {
            seq: EffectSeq(index),
            kind: DOWNLOAD,
            input_len: 4,
            input_crc: frame::input_digest(b"blob"),
        }
    } else {
        RecordRef::EffectCompleted {
            seq: EffectSeq(index),
            result: b"done",
        }
    }
}

/// What that record is called in the ledger.
const fn id(index: u32) -> RecordId {
    RecordId(index.wrapping_add(1))
}

/// The record at `index`, encoded whole, and where its commit seal starts inside that.
fn encoded(index: u32) -> (Vec<u8>, usize) {
    let mut page = [0_u8; PAGE];
    let staged = record(index);
    let (Ok(written), Ok(body)) = (
        frame::encode(&staged, align(), &mut page),
        frame::body_len(&staged, align()),
    ) else {
        unreachable!("a page is more than any record this file writes")
    };
    let Some(bytes) = page.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    (bytes.to_vec(), body)
}

/// Where record `index` starts in the journal.
fn slot(index: u32) -> usize {
    (0..index).map(|earlier| encoded(earlier).0.len()).sum()
}

// ---------------------------------------------------------------------------------------
// The writers
// ---------------------------------------------------------------------------------------

/// The real thing: §07's two barriers per record, through the writer that enforces them.
fn two_barrier_writer(session: &mut Session) -> Result<(), FaultError> {
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region());
    while recovery.next(session, &mut page).is_some() {}
    let Some(mut journal) = Journal::after(&recovery) else {
        unreachable!("an erased region ends cleanly at its first byte")
    };

    for index in 0..RECORDS {
        let mut staging = [0_u8; PAGE];
        let sealable = journal
            .stage(session, &record(index), &mut staging)
            .and_then(|staged| staged.payload_barrier(session))
            .map_err(unwind)?;

        // The record is declared *here*, between the payload barrier and the seal, and that
        // placement is the protocol rather than bookkeeping. §15's `Acknowledged` means "a
        // barrier completed after every one of this record's writes", and the payload
        // barrier completes after the frame body — so a record declared before it would be
        // acknowledged by it, and the oracle would then require recovery to produce a
        // record §07 says was never committed. The frame body is not a record for the same
        // reason `tests/banks.rs` does not declare its bank header as one: a crash before
        // the seal recovers the run without it, which is history the device is right to
        // have no trace of.
        session.begin_record(id(index));
        sealable.commit(session).map_err(unwind)?;
        session.end_record();
    }
    Ok(())
}

/// The mutant: the commit seal programmed before the frame it seals.
///
/// Not expressible through [`waymaker_flash::append`] — the value that can program a seal is
/// one only the payload barrier produces — so it reaches around it to the session, which is
/// what a tooth is for.
///
/// One barrier rather than two, because with the order inverted a second barrier would buy
/// nothing: what makes a seal safe is the frame being durable *before* it, and this writer
/// has already given that up.
fn seals_before_it_writes(session: &mut Session) -> Result<(), FaultError> {
    let mut at = 0_u32;
    for index in 0..RECORDS {
        let (bytes, body) = encoded(index);
        let (Some(frame_bytes), Some(seal_bytes)) = (bytes.get(..body), bytes.get(body..)) else {
            unreachable!("`encode` reports what it wrote")
        };
        session.begin_record(id(index));
        session.program(at.wrapping_add(width(body)), seal_bytes)?;
        session.program(at, frame_bytes)?;
        session.barrier()?;
        session.end_record();
        at = at.wrapping_add(width(bytes.len()));
    }
    Ok(())
}

/// A length as a device offset.
fn width(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// The driver's error out of an append failure.
///
/// Every other variant is a refusal this writer cannot provoke: the region holds both
/// records, the page holds each of them, and the device is the one the region was built
/// against. Reaching one would be a bug in this fixture rather than a crash point.
fn unwind(error: AppendError<FaultError>) -> FaultError {
    match error {
        AppendError::Storage(inner) => inner,
        AppendError::Encode(inner) => unreachable!("this fixture encodes: {inner}"),
        AppendError::WrongDevice => unreachable!("one device"),
        AppendError::NoRoom { needed, available } => {
            unreachable!("{needed} B does not fit {available} B")
        }
    }
}

/// Every run of `writer`, or a loud failure.
fn drive(writer: fn(&mut Session) -> Result<(), FaultError>) -> Vec<Run> {
    match Harness::new(geometry()).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

// ---------------------------------------------------------------------------------------
// What a record slot looks like after a crash
// ---------------------------------------------------------------------------------------

/// What one record's bytes on media turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    /// Nothing of it reached media.
    Absent,
    /// Some of the frame body is there and the seal is not.
    BodyOnly,
    /// The whole frame body is there and the seal is not.
    Unsealed,
    /// The frame body and its seal are both whole. This one is history.
    Committed,
    /// A valid seal over a frame body that is not what the writer meant to write.
    ///
    /// The state issue #24 forbids. It has a name so that a failure says which state it
    /// found rather than only that an assertion failed.
    SealedButIncomplete,
}

/// How record `index` reads in `image`.
fn slot_state(image: &[u8], index: u32) -> Slot {
    let (bytes, body) = encoded(index);
    let start = slot(index);
    let Some(landed) = image.get(start..start + bytes.len()) else {
        unreachable!("the fixture fits the device")
    };
    let (Some(frame_landed), Some(seal_landed)) = (landed.get(..body), landed.get(body..)) else {
        unreachable!("a record is longer than its frame body")
    };
    let (Some(frame_wanted), Some(seal_wanted)) = (bytes.get(..body), bytes.get(body..)) else {
        unreachable!("a record is longer than its frame body")
    };

    let sealed = seal_landed == seal_wanted;
    let whole = frame_landed == frame_wanted;
    match (sealed, whole) {
        (true, true) => Slot::Committed,
        (true, false) => Slot::SealedButIncomplete,
        (false, true) => Slot::Unsealed,
        (false, false) if frame_landed.iter().all(|byte| *byte == ERASED_BYTE) => Slot::Absent,
        (false, false) => Slot::BodyOnly,
    }
}

/// Every record slot of `image`, in order.
fn slots(image: &[u8]) -> Vec<Slot> {
    (0..RECORDS).map(|index| slot_state(image, index)).collect()
}

/// Everything the storage-backed recovery finds in `image`, and how it ended.
fn recover(image: &[u8]) -> (Vec<RecordId>, Option<Ending>) {
    let Some(mut device) = Device::restored(geometry(), image.to_vec()) else {
        unreachable!("an image of the device's own capacity restores")
    };
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; PAGE];
    let mut found = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        match step {
            Ok(RecordRef::EffectScheduled { seq, .. } | RecordRef::EffectCompleted { seq, .. }) => {
                found.push(id(seq.0));
            }
            Ok(_) => unreachable!("this fixture writes no other record"),
            Err(_) => break,
        }
    }
    (found, recovery.ending())
}

// ---------------------------------------------------------------------------------------
// The exit criterion
// ---------------------------------------------------------------------------------------

#[test]
fn no_crash_point_leaves_a_seal_over_an_incomplete_frame() {
    // Issue #24's first "done when", stated over media. A seal that landed is a promise that
    // the frame before it landed first, and the payload barrier is what makes the promise
    // keepable.
    let runs = drive(two_barrier_writer);
    assert!(runs.len() > 50, "only {} crash points", runs.len());

    for run in &runs {
        for (index, state) in slots(run.image()).into_iter().enumerate() {
            assert_ne!(
                state,
                Slot::SealedButIncomplete,
                "record {index} is sealed over an incomplete frame at {:?}",
                run.injection()
            );
        }
    }
}

#[test]
fn every_crash_point_recovers_a_record_as_absent_or_as_committed() {
    // The same criterion from the reader's side: what recovery hands back is exactly the
    // records whose slots are `Committed`, and never a prefix of one.
    for run in drive(two_barrier_writer) {
        let states = slots(run.image());
        let committed: Vec<RecordId> = states
            .iter()
            .enumerate()
            .take_while(|(_, state)| **state == Slot::Committed)
            .map(|(index, _)| id(u32::try_from(index).unwrap_or(u32::MAX)))
            .collect();
        let (recovered, _) = recover(run.image());
        assert_eq!(
            recovered,
            committed,
            "recovery disagreed with the media at {:?}: {states:?}",
            run.injection()
        );
    }
}

#[test]
fn recovery_is_a_legal_recovery_at_every_crash_point() {
    // §15's oracle over the two-barrier writer: the prefix is legal, everything acknowledged
    // survives, and nothing torn is produced.
    for run in drive(two_barrier_writer) {
        let (history, _) = recover(run.image());
        if let Err(breach) = verify_recovery(run.ledger(), &history) {
            unreachable!("{breach} at {:?}", run.injection());
        }
    }
}

#[test]
fn the_sweep_reaches_every_state_a_record_slot_can_be_in() {
    // A census. Without it the three tests above would pass over a sweep in which every
    // crash point left the same thing on media, and "never sealed but incomplete" would be a
    // statement about a state nothing reached.
    let seen: BTreeSet<Slot> = drive(two_barrier_writer)
        .iter()
        .flat_map(|run| slots(run.image()))
        .collect();
    assert_eq!(
        seen,
        BTreeSet::from([
            Slot::Absent,
            Slot::BodyOnly,
            Slot::Unsealed,
            Slot::Committed
        ]),
        "the sweep did not reach every legal slot state, or reached an illegal one"
    );
}

#[test]
fn the_writer_reports_the_same_amplification_at_every_record() {
    // Issue #24's third work item, measured rather than asserted: two programs, two
    // barriers, and the bytes of a padded frame and a seal.
    let mut device = Device::new(geometry());
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region());
    while recovery.next(&mut device, &mut page).is_some() {}
    let Some(mut journal) = Journal::after(&recovery) else {
        unreachable!("an erased region ends cleanly")
    };

    for index in 0..RECORDS {
        let (bytes, _) = encoded(index);
        let mut staging = [0_u8; PAGE];
        let Ok(written) = journal
            .stage(&mut device, &record(index), &mut staging)
            .and_then(|staged| staged.payload_barrier(&mut device))
            .and_then(|sealable| sealable.commit(&mut device))
        else {
            unreachable!("a legal append")
        };
        assert_eq!(written.program_operations(), 2);
        assert_eq!(written.barriers(), 2);
        assert_eq!(written.programmed_bytes(), width(bytes.len()));
        // A schedule record is eight payload bytes and a completion here is four, so the
        // amplification differs between them — which is the point of reporting it.
        assert_eq!(written.payload_bytes(), if index % 2 == 0 { 8 } else { 4 });
    }
    assert_eq!(journal.amplification().program_operations(), 4);
    assert_eq!(journal.amplification().barriers(), 4);
}

// ---------------------------------------------------------------------------------------
// The teeth
// ---------------------------------------------------------------------------------------

#[test]
fn a_writer_that_seals_before_it_writes_leaves_a_sealed_incomplete_frame() {
    // The control first: the honest writer never reaches the state, which is the test above.
    // Then the mutant, which is the same protocol with steps 1 and 3 swapped — and the sweep
    // has to find the crash point that makes it visible, or every assertion above is about a
    // state nothing can produce.
    let caught = drive(seals_before_it_writes)
        .iter()
        .flat_map(|run| slots(run.image()))
        .any(|state| state == Slot::SealedButIncomplete);
    assert!(
        caught,
        "a writer that seals ahead of its frame was not caught, so the sweep has no teeth"
    );
}

#[test]
fn the_reader_still_refuses_what_the_mutant_writer_left() {
    // And the second line of defence, stated rather than assumed: a seal over a torn frame
    // is a state the *writer* must not produce, and the reader refuses it anyway because the
    // frame's own checksum does not hold. Both have to be true — the reader alone would let
    // a seal-first writer pass the oracle, and the writer alone would leave a device with no
    // defence against a codec bug.
    for run in drive(seals_before_it_writes) {
        let (history, _) = recover(run.image());
        if let Err(breach) = verify_recovery(run.ledger(), &history) {
            unreachable!("{breach} at {:?}", run.injection());
        }
    }
}
