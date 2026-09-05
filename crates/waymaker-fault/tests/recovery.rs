//! Storage-backed recovery, at every point a power loss can land.
//!
//! Issue [#23](https://github.com/madmax983/waymaker/issues/23) asks for a forward scan that
//! reads through §12's storage contract with one caller-owned page, stops at the first frame
//! it cannot accept, and establishes where the next record may be written. The first two are
//! properties of a reader and are tested against fixtures in `waymaker-flash`. The third is
//! a property of a *device*, and a fixture cannot reach the states that make it dangerous —
//! a header torn halfway through a program unit, an erase that came back for one block and
//! not the next, a frame whose seal landed and whose payload did not.
//!
//! So this file drives the real reader over media a real crash left behind, at every crash
//! point [`waymaker_fault::injections`] enumerates, and asserts three things at each of them:
//!
//! * **Prefix safety.** What the recovery produced is a legal recovery of the run the ledger
//!   describes — §15's oracle, unchanged, over the same writer `tests/committed_prefix.rs`
//!   drives through `Scan`.
//! * **The append offset is erased media.** Whenever
//!   [`Recovery::append_offset`](waymaker_flash::recovery::Recovery::append_offset) answers
//!   at all, every byte from it to the end of the region is `0xFF`. An offset that pointed
//!   anywhere else is a firmware that programs over cells a cycle has already cleared, which
//!   on NOR is a bank that never boots again.
//! * **The two readers agree.** The storage-backed recovery and `frame::Scan` over the same
//!   bytes produce the same records and stop at the same offset. `waymaker-flash` checks that
//!   over journals it builds; this checks it over journals a crash built.
//!
//! # The teeth
//!
//! A suite that only ever sees a correct reader is a suite that would pass with the reader
//! deleted. [`an_append_offset_taken_from_the_stopping_point_lands_on_programmed_media`] is
//! the mutant: a reader that reports its stopping offset whatever the ending — which is the
//! obvious implementation, and the one `frame::Scan`'s documentation warns about in as many
//! words — and the sweep finds a crash point at which it hands back an offset pointing at
//! bytes a program cycle has already cleared.

use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordRef};
use waymaker_fault::{Device, FaultError, Harness, RecordId, Run, Session, verify_recovery};
use waymaker_flash::frame::{self, ERASED_BYTE, ProgramAlign, Scan};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

/// One erase block, which is the whole journal region.
///
/// The same shape `tests/committed_prefix.rs` uses, so the two files hold the same writer to
/// two different readers and a disagreement between them is visible as one failing and the
/// other passing.
fn geometry() -> Geometry {
    device(1)
}

/// The same device, reading `read` bytes at a time.
///
/// The sweep ran only at a one-byte read unit until review of this change pointed out that
/// `round_up(HEADER_BYTES, read_unit)` is then the identity and the page-and-chunk arithmetic
/// is dead code under test. Four is the other width the crash images are walked at.
fn device(read: u32) -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, read) else {
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

fn region_of(geometry: Geometry) -> JournalRegion {
    let Ok(region) = JournalRegion::spanning(geometry, 0, 256, align()) else {
        unreachable!("the whole device is a legal program")
    };
    region
}

fn region() -> JournalRegion {
    region_of(geometry())
}

/// The activity every schedule record below names.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// How many records the writer appends.
const RECORDS: u32 = 3;

/// The page a device recovers with. §04 states the RAM budget with 512 B; this device is
/// smaller than that, so 64 is what a page for it would be.
const PAGE: usize = 64;

/// Appends one record, padded to the program granularity.
fn append(session: &mut Session, at: &mut u32, record: &RecordRef<'_>) -> Result<(), FaultError> {
    let mut buffer = [0_u8; PAGE];
    let Ok(written) = frame::encode(record, align(), &mut buffer) else {
        unreachable!("a page is more than any record this file writes")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    session.program(*at, bytes)?;
    *at = at.wrapping_add(u32::try_from(written).unwrap_or(u32::MAX));
    Ok(())
}

/// The writer under test: three schedule records, each its own durable unit.
fn journal_writer(session: &mut Session) -> Result<(), FaultError> {
    let mut at = 0;
    for seq in 1..=RECORDS {
        session.begin_record(RecordId(seq.wrapping_mul(2).wrapping_sub(1)));
        append(
            session,
            &mut at,
            &RecordRef::EffectScheduled {
                seq: EffectSeq(seq),
                kind: DOWNLOAD,
                input_len: 4,
                input_crc: frame::input_digest(b"blob"),
            },
        )?;
        session.barrier()?;
        session.end_record();
    }
    Ok(())
}

/// Every run of [`journal_writer`], or a loud failure.
fn drive() -> Vec<Run> {
    match Harness::new(geometry()).run(journal_writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// A device holding `image`, the way a reset would find it.
fn restored(image: &[u8]) -> Device {
    restored_on(geometry(), image)
}

/// The same, on a device of a stated geometry.
fn restored_on(geometry: Geometry, image: &[u8]) -> Device {
    let Some(device) = Device::restored(geometry, image.to_vec()) else {
        unreachable!("an image of the device's own capacity restores")
    };
    device
}

/// What the record a run declared is called, from the bytes on media.
const fn id_of(record: &RecordRef<'_>) -> Option<RecordId> {
    match record {
        RecordRef::EffectScheduled { seq, .. } => {
            Some(RecordId(seq.0.wrapping_mul(2).wrapping_sub(1)))
        }
        _ => None,
    }
}

/// Everything the storage-backed recovery finds in `image`, and how it ended.
fn recover(image: &[u8]) -> (Vec<RecordId>, Option<Ending>, Option<u32>) {
    recover_with(geometry(), image, PAGE)
}

/// The same, on a stated device and with a stated page.
fn recover_with(
    geometry: Geometry,
    image: &[u8],
    page_bytes: usize,
) -> (Vec<RecordId>, Option<Ending>, Option<u32>) {
    let mut device = restored_on(geometry, image);
    let mut recovery = Recovery::new(region_of(geometry));
    let mut page = [0_u8; PAGE];
    let Some(page) = page.get_mut(..page_bytes) else {
        unreachable!("no test in this file asks for more than a page")
    };
    let mut found = Vec::new();
    while let Some(step) = recovery.next(&mut device, page) {
        let Ok(record) = step else { break };
        if let Some(id) = id_of(&record) {
            found.push(id);
        }
        // The page is the caller's between records, and a caller is entitled to use it.
        page.fill(0xA5);
    }
    (found, recovery.ending(), recovery.append_offset())
}

/// The same journal read the in-RAM way, for the agreement check.
///
/// The third element is *which* verdict the scan reached rather than whether it reached one:
/// a recovery that called every unsealed frame damaged would agree with a boolean and would
/// be exactly the confusion issue #24's fourth ending exists to end.
fn scanned(image: &[u8]) -> (Vec<RecordId>, usize, &'static str) {
    let mut scan = Scan::new(image, align());
    let mut found = Vec::new();
    let mut verdict = "clean";
    for step in &mut scan {
        let record = match step {
            Ok(record) => record,
            Err(DecodeError::Unsealed) => {
                verdict = "unsealed";
                break;
            }
            Err(_) => {
                verdict = "damaged";
                break;
            }
        };
        if let Some(id) = id_of(&record) {
            found.push(id);
        }
    }
    (found, scan.offset(), verdict)
}

#[test]
fn recovery_produces_a_committed_prefix_at_every_crash_point() {
    let runs = drive();
    // The enumeration is doing real work: a three-record journal is hundreds of crash
    // points, not a handful.
    assert!(runs.len() > 100, "only {} runs", runs.len());

    for run in &runs {
        let (history, _, _) = recover(run.image());
        if let Err(breach) = verify_recovery(run.ledger(), &history) {
            unreachable!("{breach} at {:?}", run.injection());
        }
    }

    // And the fault-free run really did write all three, so the loop above is not vacuously
    // true over an empty journal.
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is always first")
    };
    let (history, ending, append_at) = recover(clean.image());
    assert_eq!(history, [RecordId(1), RecordId(3), RecordId(5)]);
    assert!(matches!(ending, Some(Ending::Clean { .. })));
    assert_eq!(
        append_at,
        Some(84),
        "three records of a twenty-four-byte frame and a four-byte commit seal"
    );

    // Every prefix length actually occurs. Without this the loop above could be passing
    // because every crash point happened to recover the same thing.
    let lengths: BTreeSet<usize> = runs
        .iter()
        .map(|run| recover(run.image()).0.len())
        .collect();
    assert_eq!(lengths, BTreeSet::from([0, 1, 2, 3]));
}

#[test]
fn an_append_offset_is_erased_media_at_every_crash_point() {
    // The anti-bricking invariant, swept. Every offset this reader hands back has to be the
    // start of a run of erased cells reaching the end of the region — otherwise a writer
    // that trusted it would program bits a cycle has already cleared, and on NOR that frame
    // fails its own header checksum on every boot for ever.
    let runs = drive();
    let mut offered = 0_usize;
    let mut withheld = 0_usize;

    for run in &runs {
        let (_, _, append_at) = recover(run.image());
        let Some(append_at) = append_at else {
            withheld += 1;
            continue;
        };
        offered += 1;
        let Ok(from) = usize::try_from(region().base().wrapping_add(append_at)) else {
            unreachable!("a host holds this offset")
        };
        let Some(tail) = run.image().get(from..) else {
            unreachable!("an append offset is inside the region")
        };
        assert!(
            tail.iter().all(|byte| *byte == ERASED_BYTE),
            "the append offset {append_at} is not erased media, at {:?}",
            run.injection()
        );
    }

    // Both answers have to occur, or the property above is about an empty set. A crash in
    // the middle of a program leaves a torn frame, and a torn frame has nowhere to append.
    assert!(offered > 0, "no crash point produced an append offset");
    assert!(withheld > 0, "no crash point withheld one");
}

#[test]
fn the_two_readers_agree_at_every_crash_point() {
    // `waymaker-flash` checks this over journals it builds; this checks it over journals a
    // crash built, which is the half a fixture cannot reach. Two readers of one format
    // drift, and this is what fails when they start to.
    for run in drive() {
        let (recovered, ending, _) = recover(run.image());
        let (scanned, offset, verdict) = scanned(run.image());
        assert_eq!(
            recovered,
            scanned,
            "the two readers disagree at {:?}",
            run.injection()
        );
        let reported = match ending {
            Some(Ending::Clean { append_at }) if usize::try_from(append_at) == Ok(offset) => {
                "clean"
            }
            Some(Ending::Unsealed { at }) if usize::try_from(at) == Ok(offset) => "unsealed",
            Some(Ending::Damaged { at }) if usize::try_from(at) == Ok(offset) => "damaged",
            _ => "elsewhere",
        };
        assert_eq!(
            reported,
            verdict,
            "the two readers stopped differently at {:?}",
            run.injection()
        );
    }
}

#[test]
fn the_sweep_reaches_every_ending_a_recovery_has() {
    // A census, for the reason `tests/committed_prefix.rs` has one: a sweep in which every
    // crash point produced the same verdict would pass every assertion above while checking
    // one branch. Review of this change measured the sweep as it stood — 17 clean, 138
    // damaged, **0 incomplete** — because a 64-byte page never meets a frame it cannot hold
    // and a restored device never fails a read. So `Incomplete` is swept here, at a real
    // crash point, by running the same images through a page too small for a record.
    let runs = drive();
    let mut clean = 0_usize;
    let mut damaged = 0_usize;
    let mut incomplete = 0_usize;
    let mut unsealed = 0_usize;

    for run in &runs {
        match recover(run.image()).1 {
            Some(Ending::Clean { .. }) => clean += 1,
            Some(Ending::Damaged { .. }) => damaged += 1,
            Some(Ending::Incomplete { .. }) => incomplete += 1,
            Some(Ending::Unsealed { .. }) => unsealed += 1,
            None => unreachable!("a drained recovery has ended"),
        }
        // Sixteen bytes is shorter than the twenty-four-byte frames this writer appends, so
        // any run whose first frame is whole ends `Incomplete` rather than `Damaged`.
        if let Some(Ending::Incomplete { .. }) = recover_with(geometry(), run.image(), 16).1 {
            incomplete += 1;
        }
    }

    assert!(clean > 0, "no crash point recovered a clean journal");
    assert!(damaged > 0, "no crash point recovered a damaged one");
    assert!(
        incomplete > 0,
        "no crash point produced an incomplete recovery, so that ending is untested here"
    );
    assert!(
        unsealed > 0,
        "no crash point left a frame body with no commit seal over it, so issue #24's \
         ending is untested here"
    );
}

#[test]
fn the_same_crash_images_recover_the_same_way_at_a_wider_read_unit() {
    // `capacity = page_bytes & !(read_unit - 1)` and `round_up(HEADER_BYTES, read_unit)` are
    // both the identity at a one-byte read unit, which is what every other sweep in this
    // workspace runs at. Four is where they do arithmetic, and the answer must not change:
    // the read width is a property of the reader, and history is a property of the media.
    for run in drive() {
        let (narrow, narrow_end, narrow_append) = recover(run.image());
        let (wide, wide_end, wide_append) = recover_with(device(4), run.image(), PAGE);
        assert_eq!(
            (narrow, narrow_end, narrow_append),
            (wide, wide_end, wide_append),
            "the read width changed what was recovered at {:?}",
            run.injection()
        );
    }
}

#[test]
fn an_append_offset_taken_from_the_stopping_point_lands_on_programmed_media() {
    // The tooth. A reader that reported its stopping offset whatever the ending is the
    // obvious implementation and is wrong; this finds a crash point at which it is wrong,
    // rather than arguing that it would be.
    //
    // If this ever stops finding one, the sweep has stopped reaching torn frames and every
    // assertion in this file is weaker than it reads.
    let mut caught = None;
    for run in drive() {
        let mut device = restored(run.image());
        let mut recovery = Recovery::new(region());
        let mut page = [0_u8; PAGE];
        while let Some(step) = recovery.next(&mut device, &mut page) {
            if step.is_err() {
                break;
            }
        }
        if !matches!(recovery.ending(), Some(Ending::Damaged { .. })) {
            continue;
        }
        // What the mutant would have said, and what is really there.
        let Ok(from) = usize::try_from(recovery.offset()) else {
            unreachable!("a host holds this offset")
        };
        let Some(tail) = run.image().get(from..) else {
            unreachable!("the stopping offset is inside the region")
        };
        if tail.iter().any(|byte| *byte != ERASED_BYTE) {
            caught = Some(run.injection());
            break;
        }
    }
    assert!(
        caught.is_some(),
        "no crash point left a stopping offset on programmed media, so the append rule is \
         untested"
    );
}
