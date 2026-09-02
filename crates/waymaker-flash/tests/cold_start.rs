//! Cold-start replay, end to end: real frames on a journal, replayed by the real cursor.
//!
//! Design document §06 Cold-start replay states the sequence as six steps:
//!
//! 1. recover the active bank and its committed record prefix;
//! 2. decode the run input into caller-owned storage;
//! 3. create a fresh workflow future and replay cursor;
//! 4. poll the workflow from its beginning;
//! 5. each effect consumes the matching history records or identifies the first unresolved
//!    effect;
//! 6. stop at pending work or a terminal record.
//!
//! Steps 3 and 4 are the workflow future's, which is rung 0.4's. Every other step is
//! exercised below over bytes this crate encoded, because the two halves of the mechanism —
//! the append scan that produces a committed prefix and the cursor that consumes it — are
//! written in different crates and could each be perfectly right while disagreeing about
//! the seam. `waymaker-core` cannot test this: it reads no bytes, by design.

use waymaker_core::replay::{PendingEffect, Position, ReplayCursor, Step};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, KernelError, RecordRef, RunId};
use waymaker_flash::frame::{self, ERASED_BYTE, ProgramAlign, Scan};

/// The run these journals belong to. On media it lives once, in the bank header.
const RUN: RunId = RunId(0xABCD_1234_5678_9012);

/// The activity the workflow schedules.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// A journal region large enough for the histories below, erased the way flash is.
const JOURNAL_BYTES: usize = 512;

/// The program granularity the journals are written at.
fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two and within the program-size range")
    };
    align
}

/// An erased journal with `records` appended to it, and the byte after the last frame.
///
/// Written with the same encoder a driver would use, so the bytes the cursor sees below are
/// bytes this crate really produces rather than a fixture that agrees with it.
fn journal(records: &[RecordRef<'_>]) -> ([u8; JOURNAL_BYTES], usize) {
    let mut media = [ERASED_BYTE; JOURNAL_BYTES];
    let mut offset = 0_usize;
    for record in records {
        let Some(rest) = media.get_mut(offset..) else {
            unreachable!("the fixtures in this file fit the journal region")
        };
        let Ok(written) = frame::encode(record, align(), rest) else {
            unreachable!("the fixtures in this file fit the journal region")
        };
        offset += written;
    }
    (media, offset)
}

/// One committed effect: its schedule, and the completion that resolved it.
fn effect(seq: u32, input: &[u8], result: &'static [u8]) -> [RecordRef<'static>; 2] {
    [
        RecordRef::EffectScheduled {
            seq: EffectSeq(seq),
            kind: DOWNLOAD,
            input_len: u16::try_from(input.len()).unwrap_or(u16::MAX),
            input_crc: frame::input_digest(input),
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result,
        },
    ]
}

#[test]
fn a_recovered_prefix_replays_to_the_first_unresolved_effect() {
    // A run that got two effects done and had a third scheduled when the power went. §14:
    // "Schedule recovered without completion. Redeliver the stable effect ID."
    let [schedule_zero, complete_zero] = effect(0, b"first", b"one");
    let [schedule_one, complete_one] = effect(1, b"second", b"two");
    let (media, end) = journal(&[
        RecordRef::RunStarted {
            workflow_kind: 0x0042,
            workflow_version: 3,
            input: b"run-input",
        },
        schedule_zero,
        complete_zero,
        schedule_one,
        complete_one,
        RecordRef::EffectScheduled {
            seq: EffectSeq(2),
            kind: DOWNLOAD,
            input_len: 5,
            input_crc: frame::input_digest(b"third"),
        },
    ]);

    // Step 1: the committed record prefix. Step 2: the run input, decoded into storage the
    // caller owns rather than borrowed past the record it came from.
    let mut run_input = [0_u8; 32];
    let mut run_input_len = 0_usize;
    let mut cursor = ReplayCursor::new(RUN);
    let mut scan = Scan::new(&media, align());
    let mut results: Vec<Vec<u8>> = Vec::new();

    for step in &mut scan {
        let record = step.expect("every frame in this journal is sound");
        match cursor.advance(record).expect("this history is legal") {
            Step::RunStarted {
                workflow_kind,
                workflow_version,
                input,
            } => {
                assert_eq!(workflow_kind, 0x0042);
                assert_eq!(workflow_version, 3);
                run_input_len = input.len();
                run_input[..run_input_len].copy_from_slice(input);
            }
            // Step 5: a completion consumes the effect's records and hands the workflow the
            // recorded result.
            Step::EffectCompleted { id, result } => {
                assert_eq!(id.run, RUN);
                results.push(result.to_vec());
            }
            Step::EffectScheduled(_) => {}
            other => panic!("this history holds no {other:?}"),
        }
    }

    assert_eq!(&run_input[..run_input_len], b"run-input");
    assert_eq!(results, [b"one".to_vec(), b"two".to_vec()]);
    assert_eq!(scan.offset(), end, "the scan stopped short of the prefix");

    // Step 6: stopped at pending work. The identity is the one the dispatcher already had.
    assert_eq!(cursor.position(), Position::AwaitingOutcome);
    assert_eq!(
        cursor.pending(),
        Some(PendingEffect {
            id: EffectId {
                run: RUN,
                seq: EffectSeq(2)
            },
            kind: DOWNLOAD,
            input_len: 5,
            input_crc: frame::input_digest(b"third"),
        })
    );
    // And the run does not get to start a fourth effect while the third is in the air.
    assert_eq!(
        cursor.allocate(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn a_recovered_prefix_stops_at_a_terminal_record() {
    let [schedule_zero, complete_zero] = effect(0, b"only", b"done");
    let (media, _) = journal(&[
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"",
        },
        schedule_zero,
        complete_zero,
        RecordRef::RunCompleted { result: b"final" },
    ]);

    let mut cursor = ReplayCursor::new(RUN);
    let mut terminal: Option<Vec<u8>> = None;
    for step in Scan::new(&media, align()) {
        let record = step.expect("every frame in this journal is sound");
        if let Step::RunCompleted { result } = cursor.advance(record).expect("legal history") {
            terminal = Some(result.to_vec());
        }
    }

    assert_eq!(terminal, Some(b"final".to_vec()));
    assert_eq!(cursor.position(), Position::RunCompleted);
    assert!(cursor.position().is_terminal());
    assert_eq!(cursor.pending(), None);
}

#[test]
fn an_erased_journal_replays_as_a_run_that_has_not_started() {
    let media = [ERASED_BYTE; JOURNAL_BYTES];
    let mut cursor = ReplayCursor::new(RUN);
    let mut scan = Scan::new(&media, align());
    assert!(scan.next().is_none());
    assert_eq!(scan.offset(), 0);
    assert_eq!(cursor.position(), Position::BeforeRun);
    // First boot and cold start are the same code path: the driver writes `RunStarted` and
    // advances over it.
    assert_eq!(
        cursor.allocate(),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert!(
        cursor
            .advance(RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"",
            })
            .is_ok()
    );
    assert_eq!(
        cursor.allocate(),
        Ok(EffectId {
            run: RUN,
            seq: EffectSeq::FIRST
        })
    );
}

#[test]
fn a_torn_frame_ends_the_prefix_and_the_cursor_keeps_what_came_before() {
    // §14: "During schedule frame write — frame ignored; previous history prefix wins."
    let [schedule_zero, complete_zero] = effect(0, b"in", b"out");
    let (mut media, end) = journal(&[
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"seed",
        },
        schedule_zero,
        complete_zero,
        RecordRef::EffectScheduled {
            seq: EffectSeq(1),
            kind: DOWNLOAD,
            input_len: 2,
            input_crc: 7,
        },
    ]);
    // Tear the last frame by flipping a byte inside it. Its checksums stop holding, so the
    // scan refuses it and the prefix is what came before.
    let torn = end - 1;
    media[torn] ^= 0xFF;

    let mut cursor = ReplayCursor::new(RUN);
    let mut scan = Scan::new(&media, align());
    let mut failure = None;
    for step in &mut scan {
        match step {
            Ok(record) => {
                cursor.advance(record).expect("the sound prefix is legal");
            }
            Err(error) => failure = Some(error),
        }
    }

    assert!(failure.is_some(), "the torn frame was accepted");
    // Effect zero is committed and resolved; the torn schedule never happened.
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
    assert_eq!(cursor.next_seq(), Some(EffectSeq(1)));
    assert_eq!(
        cursor.allocate(),
        Ok(EffectId {
            run: RUN,
            seq: EffectSeq(1)
        })
    );
}

#[test]
fn history_that_could_not_have_been_written_is_refused() {
    // The scan cannot catch this: every frame below is individually perfect, and §09 hands
    // "out-of-sequence" to the cursor precisely because ordering is a fact about the run
    // rather than about the bytes.
    let (media, _) = journal(&[
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"",
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"from nowhere",
        },
    ]);

    let mut cursor = ReplayCursor::new(RUN);
    let mut outcome = Ok(());
    for step in Scan::new(&media, align()) {
        let record = step.expect("every frame in this journal is sound");
        if let Err(error) = cursor.advance(record) {
            outcome = Err(error);
        }
    }

    assert_eq!(outcome, Err(KernelError::MalformedHistory));
    assert_eq!(
        cursor.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
}

#[test]
fn the_digest_a_schedule_records_is_the_one_replay_will_compare() {
    // The two crates have to agree on what a digest *is* or §08's divergence check compares
    // two numbers nobody computed. The cursor carries the recorded digest through
    // untouched — it computes none, because the kernel owns no CRC — so this is where the
    // two halves are pinned together.
    let input = b"the activity input";
    let (media, _) = journal(&[
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"",
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq(0),
            kind: DOWNLOAD,
            input_len: 18,
            input_crc: frame::input_digest(input),
        },
    ]);

    let mut cursor = ReplayCursor::new(RUN);
    for step in Scan::new(&media, align()) {
        let record = step.expect("every frame in this journal is sound");
        cursor.advance(record).expect("this history is legal");
    }

    let pending = cursor.pending().expect("an effect is unresolved");
    assert_eq!(pending.input_crc, frame::input_digest(input));
    assert_eq!(usize::from(pending.input_len), input.len());
}
