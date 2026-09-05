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
use waymaker_core::{
    ActivityKind, DecodeError, EffectId, EffectSeq, KernelError, RecordRef, RunId,
};
use waymaker_flash::frame::{self, ERASED_BYTE, HEADER_BYTES, ProgramAlign, Scan};

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
        cursor.next_effect_id(),
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
        cursor.next_effect_id(),
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
        cursor.next_effect_id(),
        Ok(EffectId {
            run: RUN,
            seq: EffectSeq::FIRST
        })
    );
}

/// A journal holding `RunStarted`, one resolved effect, and one more schedule frame — with
/// the offset the sound prefix ends at, and the offset the last frame starts at.
fn journal_with_a_trailing_schedule() -> ([u8; JOURNAL_BYTES], usize, usize) {
    let [schedule_zero, complete_zero] = effect(0, b"in", b"out");
    let (prefix, prefix_end) = journal(&[
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"seed",
        },
        schedule_zero,
        complete_zero,
    ]);
    let (media, end) = journal(&[
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
    // The two encodings must agree byte for byte about where the prefix ends, or the
    // offsets the tests below assert against would be measuring two different journals.
    assert_eq!(prefix.get(..prefix_end), media.get(..prefix_end));
    (media, prefix_end, end)
}

/// Replays `media` and reports the scan's stopping offset, its failure if any, and the
/// cursor it left behind.
fn replay(media: &[u8]) -> (usize, Option<DecodeError>, ReplayCursor) {
    let mut cursor = ReplayCursor::new(RUN);
    let mut scan = Scan::new(media, align());
    let mut failure = None;
    for step in &mut scan {
        match step {
            Ok(record) => {
                let Ok(_) = cursor.advance(record) else {
                    unreachable!("the sound prefix of these journals is legal history")
                };
            }
            Err(error) => failure = Some(error),
        }
    }
    (scan.offset(), failure, cursor)
}

#[test]
fn a_frame_damaged_in_its_payload_ends_the_prefix_where_the_damage_starts() {
    // §09: "CRC detects accidental corruption and torn writes." The byte flipped here is in
    // the *payload* — the schedule record's activity kind — rather than in the trailing
    // checksum, which is what `end - 1` would have hit and which is the easiest possible
    // tear to catch. The payload is what a reader would go on to believe.
    let (mut media, prefix_end, _) = journal_with_a_trailing_schedule();
    let payload_byte = prefix_end + HEADER_BYTES;
    media[payload_byte] ^= 0xFF;

    let (offset, failure, cursor) = replay(&media);

    assert_eq!(failure, Some(DecodeError::IntegrityFailed));
    // §14: "frame ignored; previous history prefix wins" — and the offset is where that
    // prefix ends, which is the assertion that says so.
    assert_eq!(offset, prefix_end);
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
    assert_eq!(cursor.next_seq(), Some(EffectSeq(1)));
    assert_eq!(
        cursor.next_effect_id(),
        Ok(EffectId {
            run: RUN,
            seq: EffectSeq(1)
        })
    );
}

#[test]
fn a_schedule_frame_that_never_landed_leaves_the_prefix_untouched_and_reports_nothing() {
    // The failure §14 actually names — "During schedule frame write: frame ignored;
    // previous history prefix wins" — which is a write that did not land, not a bit flip.
    // Erased bytes after a sound prefix are the ordinary end of a journal, so the right
    // outcome is *no error at all*: a test that insisted on one would be asserting that a
    // clean power cut looks like corruption.
    let (media, prefix_end, end) = journal_with_a_trailing_schedule();
    let mut erased = media;
    for byte in erased.get_mut(prefix_end..end).unwrap_or(&mut []) {
        *byte = ERASED_BYTE;
    }

    let (offset, failure, cursor) = replay(&erased);

    assert_eq!(failure, None);
    assert_eq!(offset, prefix_end);
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.next_seq(), Some(EffectSeq(1)));
}

#[test]
fn a_half_programmed_frame_ends_the_prefix_rather_than_being_read() {
    // The other half of the same power cut: the header landed and the payload did not, so
    // the frame is neither sound nor erased. It must stop the scan rather than be
    // interpreted, or a reader believes a record the writer never finished.
    let (media, prefix_end, end) = journal_with_a_trailing_schedule();
    let mut torn = media;
    for byte in torn
        .get_mut(prefix_end + HEADER_BYTES..end)
        .unwrap_or(&mut [])
    {
        *byte = ERASED_BYTE;
    }

    let (offset, failure, cursor) = replay(&torn);

    assert!(failure.is_some(), "a half-programmed frame was accepted");
    assert_eq!(offset, prefix_end);
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
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
    // The two crates have to agree on what a digest *is*, or §08's divergence check compares
    // two numbers nobody computed. What is checked here is that the cursor carries the
    // recorded digest across the journal round trip *unchanged* — it computes none, because
    // the kernel owns no CRC — and that the value discriminates: a different input recovers
    // a different number, so a digest function that collapsed to a constant would be caught
    // here rather than only in `tests/frame.rs`, where the value itself is pinned.
    let recovered = |input: &[u8]| {
        let (media, _) = journal(&[
            RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"",
            },
            RecordRef::EffectScheduled {
                seq: EffectSeq(0),
                kind: DOWNLOAD,
                input_len: u16::try_from(input.len()).unwrap_or(u16::MAX),
                input_crc: frame::input_digest(input),
            },
        ]);
        let mut cursor = ReplayCursor::new(RUN);
        for step in Scan::new(&media, align()) {
            let record = step.expect("every frame in this journal is sound");
            cursor.advance(record).expect("this history is legal");
        }
        cursor.pending().expect("an effect is unresolved")
    };

    let first = recovered(b"the activity input");
    assert_eq!(first.input_crc, frame::input_digest(b"the activity input"));
    assert_eq!(usize::from(first.input_len), 18);

    let second = recovered(b"a different input!");
    assert_eq!(second.input_len, first.input_len, "same length, on purpose");
    assert_ne!(
        second.input_crc, first.input_crc,
        "two different inputs of the same length recovered the same digest"
    );
}

#[test]
fn an_effect_appended_past_the_prefix_recovers_under_the_identity_it_was_dispatched_with() {
    // §14's stable redelivery, driven the way a real adapter drives it: recover, mint an
    // identity for the effect the workflow asks for next, commit its schedule (§07 steps
    // 1-3), and lose power before the outcome. The identity a fresh cursor recovers must be
    // the one the dispatcher was handed, not the next one along.
    //
    // This is the path the first version of this branch got wrong: minting used to *spend*
    // the sequence, so advancing over the record just written was refused as out-of-order
    // history and the live path could not record a single effect.
    let (media, end) = journal(&[RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 1,
        input: b"seed",
    }]);

    let (_, _, mut live) = replay(&media);
    let dispatched = live
        .next_effect_id()
        .expect("history ended, so the next effect is a new one");
    assert_eq!(dispatched.seq, EffectSeq::FIRST);

    // Step 1-3: the schedule frame for that identity reaches media, then the cursor is
    // advanced over the record that is now committed.
    let input = b"payload";
    let schedule = RecordRef::EffectScheduled {
        seq: dispatched.seq,
        kind: DOWNLOAD,
        input_len: 7,
        input_crc: frame::input_digest(input),
    };
    let mut appended = media;
    let Some(rest) = appended.get_mut(end..) else {
        unreachable!("the journal region has room for one more frame")
    };
    let Ok(written) = frame::encode(&schedule, align(), rest) else {
        unreachable!("the journal region has room for one more frame")
    };
    assert!(written > 0);
    assert_eq!(
        live.advance(schedule),
        Ok(Step::EffectScheduled(PendingEffect {
            id: dispatched,
            kind: DOWNLOAD,
            input_len: 7,
            input_crc: frame::input_digest(input),
        }))
    );

    // Step 4 dispatched, and the power went. A fresh cursor over the same bytes recovers the
    // same effect, under the same id.
    let (_, failure, recovered) = replay(&appended);
    assert_eq!(failure, None);
    assert_eq!(recovered.position(), Position::AwaitingOutcome);
    assert_eq!(
        recovered.pending().map(|effect| effect.id),
        Some(dispatched)
    );
    assert_eq!(recovered.pending(), live.pending());
}

// ---------------------------------------------------------------------------------------
// The same six steps, read off a device rather than out of a slice.
//
// Everything above this line replays a journal that is in RAM, which is what a host can do
// and what §04's 768 B runtime budget says a device cannot. Issue #23's recovery reads
// through §12's storage contract with one caller-owned page, and the seam it makes — a bank
// selected, a header decoded, its journal walked, the records handed to the kernel's cursor
// — is the whole of design document §06 step 1, so it is tested end to end here rather than
// implied by two halves that each pass on their own.
// ---------------------------------------------------------------------------------------

use waymaker_flash::bank::{self, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// NOR-shaped media: erased is `0xFF`, and programming only ever clears bits.
struct Device {
    geometry: Geometry,
    media: Vec<u8>,
}

impl Device {
    fn new(geometry: Geometry) -> Self {
        let Ok(capacity) = usize::try_from(geometry.capacity()) else {
            unreachable!("a host holds any capacity this file describes")
        };
        Self {
            geometry,
            media: std::vec![ERASED_BYTE; capacity],
        }
    }

    fn put(&mut self, at: u32, bytes: &[u8]) {
        let Ok(start) = usize::try_from(at) else {
            unreachable!("a host holds any offset this file describes")
        };
        for (index, wanted) in bytes.iter().enumerate() {
            let Some(cell) = start
                .checked_add(index)
                .and_then(|at| self.media.get_mut(at))
            else {
                unreachable!("the fixtures in this file fit the device")
            };
            *cell &= *wanted;
        }
    }
}

impl StableStorage for Device {
    type Error = GeometryError;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_read(offset, len)?;
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        let end = start
            .checked_add(dst.len())
            .ok_or(GeometryError::OutOfBounds)?;
        dst.copy_from_slice(
            self.media
                .get(start..end)
                .ok_or(GeometryError::OutOfBounds)?,
        );
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_program(offset, len)?;
        self.put(offset, src);
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry.validate_erase(offset, len)?;
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        let end = start
            .checked_add(usize::try_from(len).map_err(|_| GeometryError::OutOfBounds)?)
            .ok_or(GeometryError::OutOfBounds)?;
        self.media
            .get_mut(start..end)
            .ok_or(GeometryError::OutOfBounds)?
            .fill(ERASED_BYTE);
        Ok(())
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A device with one bank written, sealed and filled with `records`.
///
/// The whole of §10's write side, done by hand because the writer that will do it is issue
/// #24's. What matters here is that every byte the recovery reads back was put there through
/// the same encoders a driver would use.
fn boot_disk(records: &[RecordRef<'_>]) -> (Device, BankLayout, BankId, u32) {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
        unreachable!("8192 is two whole 4096-byte blocks")
    };
    let Ok(layout) = BankLayout::new(geometry) else {
        unreachable!("two erase blocks are two banks")
    };
    let mut device = Device::new(geometry);
    let id = BankId::B;
    let region = layout.bank(id);

    let header = BankHeader {
        run: RUN,
        align: layout.align(),
        workflow_kind: 0x0042,
        workflow_version: 3,
        input_schema: 1,
        input: b"run-input",
    };
    let mut staging = [0_u8; 256];
    let Ok(header_len) = bank::encode_header(&header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Some(header_frame) = staging.get(..header_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    device.put(region.base(), header_frame);

    let Ok(seal) = bank::seal_for(header_frame, Generation(7)) else {
        unreachable!("a header frame can be sealed")
    };
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = bank::encode_seal(&seal, layout.align(), &mut seal_bytes) else {
        unreachable!("a seal fits its own region")
    };
    let Some(sealed) = seal_bytes.get(..seal_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    device.put(region.seal_offset(), sealed);

    let Ok(journal) = JournalRegion::of(layout, id, &header) else {
        unreachable!("this bank has room for a journal")
    };
    let mut at = journal.base();
    for record in records {
        let Ok(written) = frame::encode(record, journal.align(), &mut staging) else {
            unreachable!("the fixtures in this file fit the staging buffer")
        };
        let (Some(bytes), Ok(width)) = (staging.get(..written), u32::try_from(written)) else {
            unreachable!("a frame is shorter than a bank")
        };
        device.put(at, bytes);
        at = at.saturating_add(width);
    }
    (device, layout, id, at - journal.base())
}

/// Reads a bank's header back off media, the way a cold boot has to.
fn read_header(device: &mut Device, layout: BankLayout, id: BankId, page: &mut [u8]) -> usize {
    let region = layout.bank(id);
    let Some(window) = page.get_mut(..64) else {
        unreachable!("the page in this file is larger than a bank header")
    };
    let Ok(()) = device.read(region.base(), window) else {
        unreachable!("a bank header is inside the device")
    };
    64
}

#[test]
fn a_cold_boot_selects_a_bank_recovers_its_prefix_and_replays_it() {
    // §06's six steps against a device, with a 512 B page as the only buffer in sight. Steps
    // 3 and 4 are the workflow future's, which is rung 0.4's; everything else is here.
    let [schedule_zero, complete_zero] = effect(0, b"first", b"one");
    let (mut device, layout, id, end) = boot_disk(&[
        RecordRef::RunStarted {
            workflow_kind: 0x0042,
            workflow_version: 3,
            input: b"run-input",
        },
        schedule_zero,
        complete_zero,
        RecordRef::EffectScheduled {
            seq: EffectSeq(1),
            kind: DOWNLOAD,
            input_len: 6,
            input_crc: frame::input_digest(b"second"),
        },
    ]);

    // The one page a device has. Everything below borrows it and gives it back.
    let mut page = [0_u8; 512];

    // Which bank is authoritative — §10's generation seal, read off media.
    let mut seal_bytes = [0_u8; 64];
    let region = layout.bank(id);
    let seal_len = usize::try_from(region.seal_bytes()).expect("a host holds a seal");
    device
        .read(region.seal_offset(), &mut seal_bytes[..seal_len])
        .expect("a seal is inside the device");
    let header_len = read_header(&mut device, layout, id, &mut page);
    let generation = bank::sealed_generation(&page[..header_len], &seal_bytes[..seal_len]);
    assert_eq!(generation, Some(Generation(7)));

    // Step 1: the committed record prefix, through §12's contract and this page.
    let header = bank::decode_header(&page[..header_len]).expect("this bank has a header");
    assert_eq!(header.run, RUN);
    let journal = JournalRegion::of(layout, id, &header).expect("this bank has a journal");
    // Step 2: the run input, copied into caller-owned storage before the page moves on.
    let mut run_input = [0_u8; 32];
    let run_input_len = header.input.len();
    run_input[..run_input_len].copy_from_slice(header.input);
    assert_eq!(&run_input[..run_input_len], b"run-input");

    let mut cursor = ReplayCursor::new(header.run);
    let mut recovery = Recovery::new(journal);
    let mut results: Vec<Vec<u8>> = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let record = step.expect("every frame in this journal is sound");
        // Step 5: each record either resolves an effect or identifies the first unresolved
        // one. The cursor is the only thing that knows which.
        if let Step::EffectCompleted { id, result } = cursor.advance(record).expect("legal") {
            assert_eq!(id.run, RUN);
            results.push(result.to_vec());
        }
    }

    assert_eq!(results, [b"one".to_vec()]);
    assert_eq!(recovery.offset(), end);
    assert_eq!(recovery.ending(), Some(Ending::Clean { append_at: end }));
    // Step 6: stopped at pending work, under the identity the dispatcher already had.
    assert_eq!(cursor.position(), Position::AwaitingOutcome);
    assert_eq!(
        cursor.pending().map(|effect| effect.id),
        Some(EffectId {
            run: RUN,
            seq: EffectSeq(1)
        })
    );
    // And the append point for the outcome that resolves it.
    assert_eq!(recovery.append_offset(), Some(end));
}

#[test]
fn an_out_of_sequence_frame_stops_a_cold_boot_and_withholds_the_append_point() {
    // §09 lists out-of-sequence beside malformed and integrity-failed, and it is the one of
    // the four the recovery cannot see for itself: ordering is a fact about the run rather
    // than about the bytes, so `waymaker_core::ReplayCursor` owns it (ADR 0008). This is the
    // composition, and the property that makes it sound — a caller that stops pumping gets
    // no append offset, because an unfinished scan has none.
    let (mut device, layout, id, _) = boot_disk(&[
        RecordRef::RunStarted {
            workflow_kind: 0x0042,
            workflow_version: 3,
            input: b"run-input",
        },
        // A completion for an effect that was never scheduled: every byte of it is sound and
        // no reader of bytes can say it is wrong.
        RecordRef::EffectCompleted {
            seq: EffectSeq(4),
            result: b"nope",
        },
        RecordRef::RunCompleted { result: b"late" },
    ]);

    let mut page = [0_u8; 512];
    let header_len = read_header(&mut device, layout, id, &mut page);
    let header = bank::decode_header(&page[..header_len]).expect("this bank has a header");
    let journal = JournalRegion::of(layout, id, &header).expect("this bank has a journal");

    let mut cursor = ReplayCursor::new(header.run);
    let mut recovery = Recovery::new(journal);
    let mut accepted = 0_usize;
    let mut refusal = None;
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let record = step.expect("every frame in this journal is sound");
        match cursor.advance(record) {
            Ok(_) => accepted += 1,
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }

    assert_eq!(accepted, 1, "only `RunStarted` was legal history");
    assert_eq!(refusal, Some(KernelError::MalformedHistory));
    // The bytes were sound, so the recovery itself never failed — and it never finished
    // either, which is why there is nothing to append with.
    assert_eq!(recovery.ending(), None);
    assert_eq!(recovery.append_offset(), None);
    // And the refusal is terminal: there is no way back into replaying this history.
    assert_eq!(
        cursor.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
}
