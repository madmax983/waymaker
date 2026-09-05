//! Recovery to the last valid committed record, read through a caller-owned page.
//!
//! Issue [#23](https://github.com/madmax983/waymaker/issues/23). `frame::Scan` already
//! walks a journal that is in RAM; design document §04 gives a device 768 B of runtime RAM
//! with a 512 B scratch page, so a journal that is in RAM is a journal on a host. This file
//! is about the other one: a forward scan that reads through §12's storage contract, holds
//! nothing but a position, and says where the next record may be written — or refuses to.
//!
//! The four claims, and where each is:
//!
//! * **Recovery stops at the first frame it cannot accept, and nothing past it is exposed** —
//!   [`recovery_stops_at_a_torn_frame_and_never_reaches_the_valid_one_after_it`], which is
//!   the test issue #23 names, plus the stop-condition cases around it.
//! * **Stale tail bytes are never interpreted** — [`a_hole_in_a_journal_is_not_the_end_of_it`]
//!   and [`a_short_tail_that_is_not_erased_is_a_torn_header`].
//! * **The append offset is a by-product, and only a safe one** —
//!   [`an_append_offset_is_only_ever_erased_media`] and
//!   [`a_journal_that_stopped_at_damage_has_no_append_offset`].
//! * **Cost and RAM do not grow with history** —
//!   [`the_cost_of_a_record_does_not_depend_on_how_many_came_before_it`] and
//!   [`recovery_is_a_position_rather_than_a_buffer`].
//!
//! And one that holds the whole thing together: a recovery over a device and a
//! [`frame::Scan`] over the same bytes must agree, record for record and offset for offset
//! — [`a_recovery_reads_what_a_scan_reads`]. Two readers of one format drift; the way they
//! do not is a test that fails when they start to.

use std::vec::Vec;

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordRef, RunId};
use waymaker_flash::bank::{BankHeader, BankId, BankLayout};
use waymaker_flash::frame::{self, ERASED_BYTE, HEADER_BYTES, ProgramAlign, Scan};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery, RecoveryError, RegionError};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// The run these journals belong to.
const RUN: RunId = RunId(0x0123_4567_89AB_CDEF);

/// The activity the fixtures schedule.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// The scratch page design document §04 states the RAM budget with.
const PAGE: usize = 512;

// ---------------------------------------------------------------------------------------
// A device to recover from.
// ---------------------------------------------------------------------------------------

/// NOR-shaped media that counts what a reader asks of it.
///
/// Not a fault model — `waymaker-fault` is that, and it is one crate up. This one exists to
/// answer two questions no in-RAM scan can be asked: how many reads a recovery issues, and
/// whether any of them left the journal region.
struct Nor {
    geometry: Geometry,
    media: Vec<u8>,
    reads: usize,
    bytes_read: usize,
    /// Every `offset..offset + len` a reader named, in order.
    spans: Vec<(u32, u32)>,
    /// Reads to fail, by ordinal, so an unreadable device can be a test rather than a claim.
    fail_read_at: Option<usize>,
}

impl Nor {
    fn new(geometry: Geometry) -> Self {
        let Ok(capacity) = usize::try_from(geometry.capacity()) else {
            unreachable!("a host holds any capacity this file describes")
        };
        Self {
            geometry,
            media: std::vec![ERASED_BYTE; capacity],
            reads: 0,
            bytes_read: 0,
            spans: Vec::new(),
            fail_read_at: None,
        }
    }

    /// Programs `bytes` at `at` the way flash does: bits only ever clear.
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

    fn forget(&mut self) {
        self.reads = 0;
        self.bytes_read = 0;
        self.spans.clear();
    }
}

impl StableStorage for Nor {
    type Error = GeometryError;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_read(offset, len)?;
        if self.fail_read_at == Some(self.reads) {
            self.reads += 1;
            return Err(GeometryError::OutOfBounds);
        }
        self.reads += 1;
        self.bytes_read += dst.len();
        self.spans.push((offset, len));
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        let end = start
            .checked_add(dst.len())
            .ok_or(GeometryError::OutOfBounds)?;
        let source = self
            .media
            .get(start..end)
            .ok_or(GeometryError::OutOfBounds)?;
        dst.copy_from_slice(source);
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

// ---------------------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------------------

/// A device of two 4 KiB erase blocks, programmed eight bytes at a time, read byte-wise.
fn nor() -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
        unreachable!("8192 is two whole 4096-byte blocks of whole 8-byte units of bytes")
    };
    geometry
}

fn align(bytes: u16) -> ProgramAlign {
    let Some(align) = ProgramAlign::new(bytes) else {
        unreachable!("the alignments in this file are powers of two")
    };
    align
}

/// A device that programs `unit` bytes at a time and reads `read` at a time.
///
/// A journal's granularity must be at least the device's program unit — a region validated
/// only as a read would admit an append offset no driver could program — so a test that
/// varies the granularity has to vary the device with it.
fn device(unit: u32, read: u32) -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, unit, read) else {
        unreachable!("8192 is two whole 4096-byte blocks of nesting power-of-two units")
    };
    geometry
}

/// A journal region carved out of a device by hand, so a test can name its bounds.
fn region(geometry: Geometry, base: u32, bytes: u32, unit: u16) -> JournalRegion {
    let Ok(region) = JournalRegion::spanning(geometry, base, bytes, align(unit)) else {
        unreachable!("the regions in this file are legal programs")
    };
    region
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

/// Appends `records` to `device` at `region`, and answers the byte after the last frame.
///
/// Written with the encoder a driver would use, so what the recovery reads back is bytes
/// this crate really produces.
fn append(device: &mut Nor, region: JournalRegion, records: &[RecordRef<'_>]) -> u32 {
    let mut staging = [0_u8; PAGE];
    let mut at = region.base();
    for record in records {
        let (Ok(written), ..) = (frame::encode(record, region.align(), &mut staging), ()) else {
            unreachable!("the fixtures in this file fit a page")
        };
        let (Some(frame), Ok(width)) = (staging.get(..written), u32::try_from(written)) else {
            unreachable!("a frame is shorter than a page")
        };
        device.put(at, frame);
        at = at.saturating_add(width);
    }
    at
}

/// Every record a recovery yields, and how it ended.
fn drain<C>(device: &mut Nor, recovery: &mut Recovery<C>) -> (Vec<Vec<u8>>, Option<Ending>)
where
    C: waymaker_flash::integrity::IntegrityCheck,
{
    let mut page = [0_u8; PAGE];
    let mut seen = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        match step {
            Ok(record) => seen.push(describe(&record)),
            Err(_) => break,
        }
        // The page is the caller's, and a caller is free to scribble on it between records.
        // Anything the recovery kept a borrow of would show up here as a wrong record next
        // time round rather than as a compile error, because the borrow is on the page and
        // not on the recovery.
        page.fill(0xA5);
    }
    (seen, recovery.ending())
}

/// A record as bytes, so two readers' answers can be compared without a lifetime between
/// them.
fn describe(record: &RecordRef<'_>) -> Vec<u8> {
    let mut out = std::vec![record.kind().0];
    let mut staging = [0_u8; PAGE];
    let (Ok(written), ..) = (frame::encode(record, ProgramAlign::BYTE, &mut staging), ()) else {
        unreachable!("a record that decoded re-encodes")
    };
    let Some(frame) = staging.get(..written) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    out.extend_from_slice(frame);
    out
}

// ---------------------------------------------------------------------------------------
// The stop conditions.
// ---------------------------------------------------------------------------------------

#[test]
fn recovery_stops_at_a_torn_frame_and_never_reaches_the_valid_one_after_it() {
    // Issue #23's named acceptance test. A journal with three sound records, a fourth whose
    // header was torn by a power loss, and a fifth that is *perfectly good* — which is the
    // whole point. §14: nothing beyond the first unacceptable frame is exposed, even when a
    // later frame happens to be intact, because a prefix with a hole in it is not a prefix.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 512, 8);

    let [schedule, complete] = effect(0, b"in", b"out");
    let end = append(
        &mut device,
        journal,
        &[
            RecordRef::RunStarted {
                workflow_kind: 7,
                workflow_version: 1,
                input: b"run",
            },
            schedule,
            complete,
        ],
    );

    // The torn frame: a header that was being programmed when the power went, so its own
    // checksum cannot hold.
    device.put(end, &[0x57, 0x4D, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00]);
    let torn_at = end - journal.base();

    // And a whole, sound record after it, at the next program boundary.
    let after = end + 8;
    let mut staging = [0_u8; PAGE];
    let written = frame::encode(
        &RecordRef::RunCompleted { result: b"late" },
        journal.align(),
        &mut staging,
    )
    .expect("a page holds this frame");
    device.put(after, &staging[..written]);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);

    assert_eq!(
        seen.len(),
        3,
        "the prefix is the three records before the tear"
    );
    assert_eq!(
        ending,
        Some(Ending::Damaged { at: torn_at }),
        "recovery ends at the torn frame, not past it"
    );
    assert_eq!(
        recovery.append_offset(),
        None,
        "a journal that ends in damage has nowhere safe to append"
    );
}

#[test]
fn a_hole_in_a_journal_is_not_the_end_of_it() {
    // Erased bytes where a header should be, and programmed bytes after them. A reader that
    // called the erased run the end of history would hand back a prefix that is missing
    // records the device still holds, and an append offset pointing at cells a later frame
    // already occupies.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 512, 8);

    let end = append(
        &mut device,
        journal,
        &[RecordRef::RunStarted {
            workflow_kind: 7,
            workflow_version: 1,
            input: b"run",
        }],
    );

    // A whole erase-block's worth of gap would run to the end; this is a hole with history
    // on the far side of it, which is the shape a reader must not accept.
    let mut staging = [0_u8; PAGE];
    let written = frame::encode(
        &RecordRef::RunCompleted { result: b"beyond" },
        journal.align(),
        &mut staging,
    )
    .expect("a page holds this frame");
    device.put(end + 64, &staging[..written]);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);

    assert_eq!(seen.len(), 1);
    assert_eq!(ending, Some(Ending::Damaged { at: end }));
    assert_eq!(recovery.append_offset(), None);
}

#[test]
fn a_short_tail_that_is_not_erased_is_a_torn_header() {
    // Fewer bytes left in the region than a header, and they are not erased. Calling that a
    // clean end would hand back an append offset pointing into cells a program cycle has
    // already cleared — which on NOR cannot be written again without erasing the block.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 32, 8);

    let end = append(
        &mut device,
        journal,
        &[RecordRef::RunCompleted { result: &[] }],
    );
    assert_eq!(
        end, 24,
        "the shortest record is a sixteen-byte frame and an eight-byte commit seal"
    );
    device.put(24, &[0x57, 0x4D, 0x01, 0x07, 0x00, 0x00, 0x00, 0x00]);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);

    assert_eq!(seen.len(), 1);
    assert_eq!(ending, Some(Ending::Damaged { at: 24 }));
}

#[test]
fn an_unknown_record_kind_stops_recovery() {
    // §09 makes skipping a property of the format version, and version 1 permits none. A
    // reader that skipped one would be asserting that the rest of history means the same
    // thing without it.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);

    let end = append(
        &mut device,
        journal,
        &[RecordRef::RunStarted {
            workflow_kind: 7,
            workflow_version: 1,
            input: b"run",
        }],
    );

    // A structurally sound frame wearing record kind 9, which this firmware reserves and
    // does not decode. Built by re-sealing a real frame, so only the kind is wrong.
    let mut staging = [0_u8; PAGE];
    let written = frame::encode(
        &RecordRef::RunCompleted { result: b"x" },
        journal.align(),
        &mut staging,
    )
    .expect("a page holds this frame");
    let mut bytes = staging[..written].to_vec();
    bytes[3] = 9;
    reseal(&mut bytes);
    device.put(end, &bytes);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);

    assert_eq!(seen.len(), 1);
    assert_eq!(ending, Some(Ending::Damaged { at: end }));
}

#[test]
fn a_frame_the_page_cannot_hold_stops_recovery_without_calling_it_damage() {
    // A record longer than the caller's page is not a damaged record: the journal may be
    // perfectly sound and this device simply cannot stage it. So the scan stops, loudly, and
    // the ending says the prefix is *incomplete* rather than final — a caller that replayed
    // it as if it were complete would be replaying a truncated history.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 512, 8);

    let payload = [0x5A_u8; 200];
    let end = append(
        &mut device,
        journal,
        &[RecordRef::RunCompleted { result: &payload }],
    );
    assert!(end > 200);

    let mut recovery = Recovery::new(journal);
    let mut page = [0_u8; 64];
    let step = recovery
        .next(&mut device, &mut page)
        .expect("a journal with a frame in it has a step");
    // The whole record, seal included: a reader that staged only the frame would have
    // nowhere to check the seal from.
    let record_bytes = frame::encoded_len(&RecordRef::RunCompleted { result: &payload }, align(8))
        .expect("this record encodes");
    assert_eq!(record_bytes, 224);
    assert_eq!(
        step,
        Err(RecoveryError::PageTooSmall {
            needed: record_bytes
        })
    );
    assert_eq!(recovery.ending(), Some(Ending::Incomplete { at: 0 }));
    assert_eq!(recovery.append_offset(), None);
    assert_eq!(
        recovery.next(&mut device, &mut page),
        None,
        "and it is fused"
    );
}

#[test]
fn a_device_that_cannot_be_read_ends_a_recovery_without_a_prefix_or_an_offset() {
    // A read that fails teaches a caller nothing about what is on media. It is neither a
    // clean end nor damage, and the difference matters: a clean end says the prefix is all
    // of history, damage says the prefix is final, and this says neither is known.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);
    append(
        &mut device,
        journal,
        &[RecordRef::RunCompleted { result: b"one" }],
    );
    device.fail_read_at = Some(0);

    let mut recovery = Recovery::new(journal);
    let mut page = [0_u8; PAGE];
    assert_eq!(
        recovery.next(&mut device, &mut page),
        Some(Err(RecoveryError::Storage(GeometryError::OutOfBounds)))
    );
    assert_eq!(recovery.ending(), Some(Ending::Incomplete { at: 0 }));
    assert_eq!(recovery.append_offset(), None);
}

// ---------------------------------------------------------------------------------------
// The append offset.
// ---------------------------------------------------------------------------------------

#[test]
fn a_recovery_refuses_a_device_its_region_was_not_validated_against() {
    // Codex's second finding on pull request #74. Carrying the geometry in the region stops
    // the *reads* being proved against the wrong device; it does not on its own stop the
    // *append offset* being about the wrong one. A region built where the program unit is a
    // single byte reads perfectly on a device that programs eight — every read is one byte
    // aligned — reports a clean end, and hands back an offset that device must refuse.
    //
    // So the two are compared, and the refusal comes before a byte is read.
    let fine = Geometry::new(8192, 4096, 1, 1).expect("programs and reads one");
    let coarse = Geometry::new(8192, 4096, 8, 1).expect("programs eight, reads one");
    let journal = region(fine, 1, 512, 1);
    assert_eq!(
        coarse.validate_program(journal.base(), 8),
        Err(GeometryError::MisalignedOffset),
        "the offset this region would hand back is not programmable on the other device"
    );

    let mut other = Nor::new(coarse);
    let mut recovery = Recovery::new(journal);
    let mut page = [0_u8; PAGE];
    assert_eq!(
        recovery.next(&mut other, &mut page),
        Some(Err(RecoveryError::WrongDevice))
    );
    assert_eq!(recovery.ending(), Some(Ending::Incomplete { at: 0 }));
    assert_eq!(recovery.append_offset(), None);
    assert!(other.spans.is_empty(), "it refused before reading anything");

    // And the device the region *was* built against still works, so this is a refusal rather
    // than a rule nobody can satisfy.
    let mut own = Nor::new(fine);
    let mut sound = Recovery::new(journal);
    let (seen, ending) = drain(&mut own, &mut sound);
    assert!(seen.is_empty());
    assert_eq!(ending, Some(Ending::Clean { append_at: 0 }));
}

#[test]
fn an_append_offset_is_only_ever_erased_media() {
    // The invariant the whole type exists for, stated as a property rather than as a case:
    // whenever an append offset comes back at all, every byte from it to the end of the
    // region is erased. An offset that pointed anywhere else is a firmware that programs
    // over cells a cycle has already cleared, which on NOR is a bank that never boots again.
    let geometry = nor();
    for count in 0..12_usize {
        let mut device = Nor::new(geometry);
        let journal = region(geometry, 0, 512, 8);
        let records: Vec<RecordRef<'static>> = (0..count)
            .map(|index| RecordRef::EffectCompleted {
                seq: EffectSeq(u32::try_from(index).expect("small")),
                result: b"r",
            })
            .collect();
        let end = append(&mut device, journal, &records);

        let mut recovery = Recovery::new(journal);
        let (seen, ending) = drain(&mut device, &mut recovery);
        assert_eq!(seen.len(), count);
        assert_eq!(ending, Some(Ending::Clean { append_at: end }));

        let append_at = recovery
            .append_offset()
            .expect("a journal that ended in erased media has somewhere to append");
        assert_eq!(append_at, end);
        let from = usize::try_from(journal.base() + append_at).expect("host");
        let to = usize::try_from(journal.base() + journal.bytes()).expect("host");
        assert!(
            device.media[from..to]
                .iter()
                .all(|byte| *byte == ERASED_BYTE),
            "an append offset that is not erased media is a bricked bank"
        );
    }
}

#[test]
fn a_journal_that_stopped_at_damage_has_no_append_offset() {
    // Damage is the case an offset must not survive. `Ending::Damaged` carries no
    // `append_at` field, so this is checked by the compiler as much as by the assertion —
    // there is no way to write down the unsafe answer.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 128, 8);
    device.put(0, &[0x57, 0x4D, 0x01, 0x07, 0x00, 0x00, 0x00, 0x00]);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);
    assert!(seen.is_empty());
    assert_eq!(ending, Some(Ending::Damaged { at: 0 }));
    assert_eq!(recovery.append_offset(), None);
}

#[test]
fn a_full_journal_appends_at_its_end_and_a_caller_finds_no_room() {
    // The boundary: a region whose last byte is the last byte of a frame. The prefix is
    // whole, so the ending is clean, and the offset it carries is the end of the region —
    // which is the honest answer. Whether a record fits there is the caller's arithmetic.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 48, 8);
    let end = append(
        &mut device,
        journal,
        &[
            RecordRef::RunCompleted { result: &[] },
            RecordRef::RunFailed { error: &[] },
        ],
    );
    assert_eq!(end, 48, "two empty records, each sixteen bytes and a seal");

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);
    assert_eq!(seen.len(), 2);
    assert_eq!(ending, Some(Ending::Clean { append_at: 48 }));
    assert_eq!(recovery.append_offset(), Some(48));
}

#[test]
fn an_erased_journal_recovers_nothing_and_appends_at_its_first_byte() {
    // A device that has never been written. Reporting this as damage would make every first
    // boot look like a corrupted one.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);
    assert!(seen.is_empty());
    assert_eq!(ending, Some(Ending::Clean { append_at: 0 }));
    assert_eq!(recovery.append_offset(), Some(0));
    assert_eq!(recovery.offset(), 0);
}

// ---------------------------------------------------------------------------------------
// Agreement with the in-RAM reader.
// ---------------------------------------------------------------------------------------

#[test]
fn a_recovery_reads_what_a_scan_reads() {
    // Two readers of one format drift apart; this is what fails when they start to. Every
    // journal below is walked twice — once through §12's storage contract with a page, once
    // as a slice by `frame::Scan` — and the two must agree record for record, on where the
    // prefix ends, and on whether it ended cleanly.
    let mut rng = Rng::new(0x5EED_2317);

    for case in 0..256_u32 {
        // Four devices, not one: the granularity a journal was written at is the program
        // unit of the device that wrote it, and the read unit varies with it so the
        // page-and-chunk arithmetic is exercised at more than one read width.
        let (unit, read) =
            [(1_u16, 1_u32), (2, 2), (4, 1), (8, 4)][usize::try_from(case % 4).expect("host")];
        let geometry = device(u32::from(unit), read);
        let mut device = Nor::new(geometry);
        let journal = region(geometry, 0, 512, unit);
        let count = usize::try_from(rng.below(6)).expect("host");
        let payloads: Vec<Vec<u8>> = (0..count)
            .map(|_| std::vec![rng.byte(); usize::try_from(rng.below(9)).expect("host")])
            .collect();
        let records: Vec<RecordRef<'_>> = payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| RecordRef::EffectCompleted {
                seq: EffectSeq(u32::try_from(index).expect("host")),
                result: payload,
            })
            .collect();
        let end = append(&mut device, journal, &records);

        // Then damage one byte somewhere in or after the journal, half the time.
        if case % 2 == 0 && end > 0 {
            let at = rng.below(end.max(1));
            let target = usize::try_from(at).expect("host");
            device.media[target] &= rng.byte();
        }

        let image: Vec<u8> = device.media[..512].to_vec();
        let mut scan = Scan::new(&image, align(unit));
        let mut expected: Vec<Vec<u8>> = Vec::new();
        let mut scan_failed = None;
        for step in &mut scan {
            let record = match step {
                Ok(record) => record,
                Err(error) => {
                    scan_failed = Some(error);
                    break;
                }
            };
            expected.push(describe(&record));
        }

        let mut recovery = Recovery::new(journal);
        let (seen, ending) = drain(&mut device, &mut recovery);

        assert_eq!(seen, expected, "case {case}: the two readers disagree");
        assert_eq!(
            recovery.offset(),
            u32::try_from(scan.offset()).expect("host"),
            "case {case}: the two readers stopped in different places"
        );
        // Compared by *which* ending rather than by "did either stop", because that is the
        // distinction issue #24 added: a recovery that called every unsealed frame damaged
        // would pass a union and would be exactly the confusion the fourth variant exists to
        // end.
        let expected_ending = match scan_failed {
            None => "clean",
            Some(DecodeError::Unsealed) => "unsealed",
            Some(_) => "damaged",
        };
        let reported = match ending {
            Some(Ending::Clean { .. }) => "clean",
            Some(Ending::Unsealed { .. }) => "unsealed",
            Some(Ending::Damaged { .. }) => "damaged",
            Some(Ending::Incomplete { .. }) => "incomplete",
            None => "unfinished",
        };
        assert_eq!(
            reported, expected_ending,
            "case {case}: the two readers classified the same journal differently"
        );
    }
}

#[test]
fn a_recovery_with_the_wrong_check_stops_at_the_first_frame() {
    // `Recovery<C>` promises a journal is verified with the algorithm that sealed it, and
    // until this test nothing used the parameter: review of this change replaced both
    // `::<C>` calls in the reader with their non-generic siblings, and the whole workspace
    // passed. A pin now refuses that mutation; this is the behaviour behind the pin.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);
    append(
        &mut device,
        journal,
        &[
            RecordRef::RunCompleted { result: b"one" },
            RecordRef::RunFailed { error: b"two" },
        ],
    );

    // The shipped check reads it.
    let mut shipped = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut shipped);
    assert_eq!(seen.len(), 2);
    assert!(matches!(ending, Some(Ending::Clean { .. })));

    // Another one does not, and stops at the very first frame rather than walking it wrong.
    let mut other = Recovery::<Other>::with_integrity(journal);
    let mut page = [0_u8; PAGE];
    assert_eq!(
        other.next(&mut device, &mut page),
        Some(Err(RecoveryError::Decode(DecodeError::IntegrityFailed)))
    );
    assert_eq!(other.ending(), Some(Ending::Damaged { at: 0 }));
    assert_eq!(other.append_offset(), None);
}

#[test]
fn a_read_that_fails_mid_scan_ends_a_recovery_after_a_short_prefix() {
    // The other side of `Incomplete`: not the first read, but one after a record has already
    // been yielded. The prefix is *short* rather than final, which is the whole distinction
    // between this ending and `Damaged` — and the offset it carries is where the scan was,
    // not where it started.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);
    append(
        &mut device,
        journal,
        &[
            RecordRef::RunCompleted { result: b"one" },
            RecordRef::RunFailed { error: b"two" },
        ],
    );
    // Reads 0 and 1 are the first record's header and frame; read 2 is the second's header.
    device.fail_read_at = Some(2);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);
    assert_eq!(seen.len(), 1);
    assert_eq!(ending, Some(Ending::Incomplete { at: 32 }));
    assert_ne!(ending, Some(Ending::Damaged { at: 32 }));
    assert_eq!(recovery.append_offset(), None);
}

#[test]
fn a_frame_exactly_the_size_of_the_page_is_read_rather_than_refused() {
    // The `need > capacity` boundary from the near side. Only the far side was tested — a
    // 212-byte frame against a 64-byte page — so an off-by-one that refused a frame the page
    // could hold would have passed.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);
    let payload = [0x5A_u8; 52];
    let frame_len = payload.len() + waymaker_flash::frame::FRAME_OVERHEAD_BYTES;
    assert_eq!(frame_len, 68);
    // Padded to eight and followed by an eight-byte seal: the page has to hold the record,
    // not the frame.
    let record_bytes = frame::encoded_len(&RecordRef::RunCompleted { result: &payload }, align(8))
        .expect("this record encodes");
    assert_eq!(record_bytes, 80);
    append(
        &mut device,
        journal,
        &[RecordRef::RunCompleted { result: &payload }],
    );

    let mut exact = Recovery::new(journal);
    let mut page = [0_u8; 80];
    assert!(matches!(exact.next(&mut device, &mut page), Some(Ok(_))));

    let mut short = Recovery::new(journal);
    let mut one_less = [0_u8; 79];
    assert_eq!(
        short.next(&mut device, &mut one_less),
        Some(Err(RecoveryError::PageTooSmall { needed: 80 }))
    );
}

#[test]
fn a_page_that_is_not_whole_read_units_is_used_down_to_the_unit() {
    // `capacity = page_bytes & !(read_unit - 1)` is the identity at a one-byte read unit, so
    // every other test in this file leaves the mask untested. A device that reads four at a
    // time and a page of thirty-four bytes is what exercises it: thirty-two bytes are
    // usable, and a record that fits those — a twenty-eight-byte padded frame and a
    // four-byte seal — is read rather than refused.
    let geometry = Geometry::new(8192, 4096, 4, 4).expect("reads and programs four");
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 4);
    append(
        &mut device,
        journal,
        &[RecordRef::RunCompleted {
            result: b"twelve bytes",
        }],
    );

    let mut recovery = Recovery::new(journal);
    let mut ragged = [0_u8; 34];
    assert!(matches!(
        recovery.next(&mut device, &mut ragged),
        Some(Ok(_))
    ));
    for (offset, len) in &device.spans {
        assert_eq!(
            len % 4,
            0,
            "a read of {len} bytes at {offset} is not whole units"
        );
        assert_eq!(offset % 4, 0, "a read at {offset} is not whole units");
    }
}

#[test]
fn a_recovery_at_a_wider_read_unit_still_stops_where_a_scan_does() {
    // The whole of `waymaker-fault`'s sweep and every stop-condition test above runs at a
    // one-byte read unit, where `round_up(HEADER_BYTES, read_unit)` is the identity. This is
    // the same journal at four.
    let geometry = Geometry::new(8192, 4096, 8, 4).expect("reads four, programs eight");
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 256, 8);
    let end = append(
        &mut device,
        journal,
        &[RecordRef::RunCompleted { result: b"one" }],
    );
    device.put(end, &[0x57, 0x4D, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00]);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);
    assert_eq!(seen.len(), 1);
    assert_eq!(ending, Some(Ending::Damaged { at: end }));
}

// ---------------------------------------------------------------------------------------
// Cost and RAM.
// ---------------------------------------------------------------------------------------

#[test]
fn the_cost_of_a_record_does_not_depend_on_how_many_came_before_it() {
    // Issue #23's second "done when". The contract is that the *per-record* cost is constant
    // in history — not that a record costs any particular number of reads, which is an
    // implementation choice a later rung is free to improve.
    //
    // So three points rather than two, with the region's tail held constant so the one cost
    // that is not per record cancels between them, and the assertion is on the slope: the
    // marginal cost of a record between 8 and 100 must be exactly the marginal cost between
    // 100 and 200. That fails on a reader that rescans, and on any term in `n²`; it passes a
    // reader that got cheaper.
    // Every record here is a twelve-byte header, an eight-byte payload, a four-byte frame
    // check — twenty-four bytes, already a whole eight-byte program unit — and an eight-byte
    // commit seal. The tail is the same in every run, so the one cost that is not per record
    // cancels between them.
    const FRAME: u32 = 32;
    const TAIL: u32 = 4_096;

    let geometry = Geometry::new(65_536, 4096, 8, 1).expect("sixteen whole blocks");
    let mut cost = Vec::new();
    for count in [8_u32, 100, 200] {
        let mut device = Nor::new(geometry);
        let journal = region(geometry, 0, count * FRAME + TAIL, 8);
        let records: Vec<RecordRef<'static>> = (0..count)
            .map(|index| RecordRef::EffectCompleted {
                seq: EffectSeq(index),
                result: b"payload!",
            })
            .collect();
        let end = append(&mut device, journal, &records);
        assert_eq!(end, count * FRAME);
        device.forget();

        let mut recovery = Recovery::new(journal);
        let (seen, ending) = drain(&mut device, &mut recovery);
        assert_eq!(seen.len(), usize::try_from(count).expect("host"));
        assert_eq!(ending, Some(Ending::Clean { append_at: end }));
        cost.push((count, device.reads, device.bytes_read));
    }

    let slope = |from: (u32, usize, usize), to: (u32, usize, usize)| {
        let records = usize::try_from(to.0 - from.0).expect("host");
        ((to.1 - from.1) / records, (to.2 - from.2) / records)
    };
    let (reads_lower, bytes_lower) = slope(cost[0], cost[1]);
    let (reads_upper, bytes_upper) = slope(cost[1], cost[2]);
    assert_eq!(
        (reads_lower, bytes_lower),
        (reads_upper, bytes_upper),
        "a record costs more the later it is, so something in this reader grows with history"
    );
    // And the marginal cost is bounded, so "constant" is not being satisfied by a constant
    // that happens to be enormous. Two reads is what the implementation spends today; the
    // bound is what the contract needs.
    assert!(reads_upper <= 2, "a record cost {reads_upper} reads");
    assert!(
        bytes_upper <= usize::try_from(2 * FRAME).expect("host"),
        "a record cost {bytes_upper} bytes"
    );

    // RAM is the caller's page, and it is the same page at every size above — the loop uses
    // one `PAGE`-sized buffer per run and `Recovery` retains none of it.
    assert!(cost[2].1 > cost[0].1, "the sweep did not grow at all");
}

#[test]
fn recovery_is_a_position_rather_than_a_buffer() {
    // A recovery that grew an inline page would be a type whose size tracked the caller's
    // buffer, and the 768 B runtime budget of §04 has no room for a second one. The size is
    // asserted in the crate; this is the half a test can see — that the page is borrowed
    // per call and never retained, so scribbling on it between records changes nothing.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 0, 512, 8);
    let [schedule, complete] = effect(3, b"in", b"out");
    append(&mut device, journal, &[schedule, complete]);

    let mut recovery = Recovery::new(journal);
    let mut page = [0_u8; PAGE];
    let mut seen = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let record = step.expect("this journal is sound");
        seen.push(describe(&record));
        page.fill(0xA5);
    }
    assert_eq!(seen.len(), 2);
    // The size itself is a `const` assertion in the crate, which is where a `<=` would be a
    // place to hide a page. What a test adds is the *shape*: a recovery is its region, an
    // offset and a verdict, and nothing else — so a field that appeared would move this
    // equality rather than merely fit under a ceiling.
    assert_eq!(
        std::mem::size_of::<Recovery>(),
        std::mem::size_of::<JournalRegion>() + std::mem::size_of::<u32>() + 8
    );
}

#[test]
fn a_recovery_never_reads_outside_the_region_it_was_given() {
    // The bound. A read that overshot would reach the generation seal, or the other bank,
    // and a reader that walked into either would be reading a neighbour's bytes as history.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 4096, 512, 8);
    append(
        &mut device,
        journal,
        &[
            RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"in",
            },
            RecordRef::RunCompleted { result: b"done" },
        ],
    );
    device.forget();

    let mut recovery = Recovery::new(journal);
    let (seen, _) = drain(&mut device, &mut recovery);
    assert_eq!(seen.len(), 2);
    assert!(!device.spans.is_empty());
    for (offset, len) in &device.spans {
        assert!(
            *offset >= journal.base() && offset + len <= journal.base() + journal.bytes(),
            "a read of {len} bytes at {offset} left the region"
        );
    }
}

// ---------------------------------------------------------------------------------------
// The region.
// ---------------------------------------------------------------------------------------

#[test]
fn every_offset_a_recovery_reports_is_relative_to_its_region() {
    // Every stop-condition test above builds a region at base 0, where a region-relative
    // offset and a device-absolute one are the same number. Review of this change
    // demonstrated what that costs: changing `Ending::Damaged { at }` to report
    // `region.base() + offset` passed the entire workspace suite. So one case is run at a
    // base that is not zero, for both the ending and the offset.
    let geometry = nor();
    let mut device = Nor::new(geometry);
    let journal = region(geometry, 4096, 512, 8);
    let end = append(
        &mut device,
        journal,
        &[
            RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"in",
            },
            RecordRef::RunCompleted { result: b"done" },
        ],
    );
    let torn_at = end - journal.base();
    device.put(end, &[0x57, 0x4D, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00]);

    let mut recovery = Recovery::new(journal);
    let (seen, ending) = drain(&mut device, &mut recovery);
    assert_eq!(seen.len(), 2);
    assert!(
        torn_at > 0 && torn_at < journal.base(),
        "the two numbers differ"
    );
    assert_eq!(ending, Some(Ending::Damaged { at: torn_at }));
    assert_eq!(recovery.offset(), torn_at);
    assert_eq!(recovery.append_offset(), None);
}

#[test]
fn a_journal_region_is_the_bytes_between_a_banks_header_and_its_seal() {
    // The chain §10 states: a layout says where a bank is, a decoded header says where its
    // journal starts, and the seal is at the far end. This is that chain as one call, so a
    // caller cannot get the arithmetic wrong on its own.
    let geometry = nor();
    let layout = BankLayout::new(geometry).expect("two blocks are two banks");
    let header = BankHeader {
        run: RUN,
        align: layout.align(),
        workflow_kind: 7,
        workflow_version: 1,
        input_schema: 2,
        input: b"run-input",
    };
    let bank = layout.bank(BankId::B);
    let journal =
        JournalRegion::of(layout, BankId::B, &header).expect("this bank has room for a journal");

    let offset = header
        .journal_offset()
        .expect("a header this size has an offset");
    assert_eq!(
        journal.base(),
        bank.base() + u32::try_from(offset).expect("host")
    );
    assert_eq!(
        journal.bytes(),
        bank.payload_bytes() - u32::try_from(offset).expect("host")
    );
    assert_eq!(journal.align(), header.align);
}

#[test]
fn a_region_refuses_a_granularity_the_device_cannot_program_at() {
    // A journal written at a granularity finer than this device's program unit has frame
    // boundaries this device cannot write at — and the offset a recovery hands back is one
    // of them. Refusing at construction is a cold boot that stops; discovering it at the
    // first append would be a driver told to program a misaligned offset, and discovering it
    // mid-scan would be a recovery that stopped somewhere arbitrary and called it the end of
    // history.
    let geometry = Geometry::new(8192, 4096, 8, 4).expect("a device that programs eight");
    for finer in [ProgramAlign::BYTE, align(2), align(4)] {
        assert_eq!(
            JournalRegion::spanning(geometry, 0, 512, finer),
            Err(RegionError::AlignBelowProgramUnit),
            "granularity {} is below the program unit",
            finer.get()
        );
    }
    assert!(JournalRegion::spanning(geometry, 0, 512, align(8)).is_ok());
    assert!(JournalRegion::spanning(geometry, 0, 512, align(16)).is_ok());
}

#[test]
fn a_region_a_driver_could_read_but_never_program_is_refused() {
    // Codex's finding on pull request #74, as a test. A device that reads single bytes and
    // programs eight accepts a base of 1 as a *read*; a recovery of an erased region there
    // reports a clean end at offset 0, and the absolute offset a caller would then program
    // — 1 — is one every conforming `StableStorage` must refuse. So the region is validated
    // as a program, which is strictly stronger: a geometry nests, so whatever is legal to
    // program is legal to read.
    let geometry = Geometry::new(8192, 4096, 8, 1).expect("reads one, programs eight");
    assert_eq!(
        geometry.validate_read(1, 512),
        Ok(()),
        "readable, and that is the trap"
    );
    assert_eq!(
        geometry.validate_program(1, 512),
        Err(GeometryError::MisalignedOffset)
    );
    assert_eq!(
        JournalRegion::spanning(geometry, 1, 512, align(8)),
        Err(RegionError::Geometry(GeometryError::MisalignedOffset))
    );
    // And a length that is whole read units but not whole program units, for the same
    // reason: it puts the end of the region — and so a full journal's append offset —
    // somewhere no program can reach.
    assert_eq!(
        JournalRegion::spanning(geometry, 0, 500, align(8)),
        Err(RegionError::Geometry(GeometryError::MisalignedLength))
    );
    assert!(JournalRegion::spanning(geometry, 8, 512, align(8)).is_ok());
}

#[test]
fn a_region_refuses_what_the_device_would_refuse() {
    // §12 puts the obligation to validate on the adapter, and a region is validated once at
    // construction rather than on every read — so a region that exists is one every read of
    // it is legal for.
    let geometry = nor();
    assert_eq!(
        JournalRegion::spanning(geometry, 8192, 8, align(8)),
        Err(RegionError::Geometry(GeometryError::OutOfBounds))
    );
    assert_eq!(
        JournalRegion::spanning(geometry, 0, 8_200, align(8)),
        Err(RegionError::Geometry(GeometryError::OutOfBounds))
    );
    assert_eq!(
        JournalRegion::spanning(geometry, 0, 0, align(8)),
        Err(RegionError::EmptyRegion)
    );
}

#[test]
fn a_bank_written_at_another_devices_granularity_is_refused() {
    // The one call that welds the writer's granularity to the reader's, refusing rather than
    // guessing. Review of this change demonstrated the alternative: taking the offset from
    // the layout instead of from the header passed the whole suite, because every fixture set
    // the two to the same value and nothing made them differ. Now they cannot.
    let geometry = nor();
    let layout = BankLayout::new(geometry).expect("two blocks are two banks");
    for foreign in [align(4), align(16)] {
        assert_ne!(foreign, layout.align());
        let header = BankHeader {
            run: RUN,
            align: foreign,
            workflow_kind: 7,
            workflow_version: 1,
            input_schema: 2,
            input: b"run-input",
        };
        assert_eq!(
            JournalRegion::of(layout, BankId::A, &header),
            Err(RegionError::AlignDisagreesWithBank),
            "granularity {} is not this layout's",
            foreign.get()
        );
    }
}

#[test]
fn a_bank_whose_header_leaves_exactly_no_journal_is_refused_too() {
    // The `bytes == 0` arm, which is a different arm from "the header is longer than the
    // bank": a zero-length region would be one every caller reads as an empty history it may
    // append to.
    let geometry = nor();
    let layout = BankLayout::new(geometry).expect("two blocks are two banks");
    let bank = layout.bank(BankId::A);
    let payload = usize::try_from(bank.payload_bytes()).expect("host");
    let overhead = waymaker_flash::bank::HEADER_OVERHEAD_BYTES;
    // An input whose padded header frame is exactly the payload region, and not a byte more.
    let input = std::vec![0_u8; payload - overhead];
    let header = BankHeader {
        run: RUN,
        align: layout.align(),
        workflow_kind: 7,
        workflow_version: 1,
        input_schema: 2,
        input: &input,
    };
    assert_eq!(
        header.journal_offset(),
        Some(payload),
        "this header fills the payload region exactly"
    );
    assert_eq!(
        JournalRegion::of(layout, BankId::A, &header),
        Err(RegionError::NoJournalRoom)
    );
}

#[test]
fn a_bank_whose_header_fills_it_has_no_journal() {
    // A header long enough to leave nothing behind it. The refusal is the point: the
    // alternative is a zero-byte journal reported as a legal region, which every caller
    // would then treat as an empty history it may append to.
    let geometry = nor();
    let layout = BankLayout::new(geometry).expect("two blocks are two banks");
    let bank = layout.bank(BankId::A);
    let input = std::vec![0_u8; usize::try_from(bank.payload_bytes()).expect("host")];
    let header = BankHeader {
        run: RUN,
        align: layout.align(),
        workflow_kind: 7,
        workflow_version: 1,
        input_schema: 2,
        input: &input,
    };
    assert_eq!(
        JournalRegion::of(layout, BankId::A, &header),
        Err(RegionError::NoJournalRoom)
    );
}

#[test]
fn every_recovery_error_is_a_distinct_shape() {
    // `RecoveryError` carries no `message` of its own — see its documentation — so what a
    // test can hold is that the variants are distinguishable, which is what a caller's
    // `match` rests on.
    let errors: [RecoveryError<GeometryError>; 4] = [
        RecoveryError::Storage(GeometryError::OutOfBounds),
        RecoveryError::Decode(DecodeError::Truncated),
        RecoveryError::PageTooSmall { needed: 16 },
        RecoveryError::WrongDevice,
    ];
    for (left, one) in errors.iter().enumerate() {
        for (right, other) in errors.iter().enumerate() {
            assert_eq!(
                left == right,
                one == other,
                "two errors compare equal: {one:?}"
            );
        }
    }
}

#[test]
fn every_region_error_says_something_different() {
    // The postcondition says "distinct from every other variant's — including from every
    // `GeometryError` it wraps", so every one of them is compared rather than one of them.
    let messages = [
        RegionError::NoJournalRoom.message(),
        RegionError::AlignDisagreesWithBank.message(),
        RegionError::AlignBelowProgramUnit.message(),
        RegionError::EmptyRegion.message(),
        RegionError::Geometry(GeometryError::ZeroUnit).message(),
        RegionError::Geometry(GeometryError::UnitIsNotAPowerOfTwo).message(),
        RegionError::Geometry(GeometryError::UnitsDoNotNest).message(),
        RegionError::Geometry(GeometryError::MisalignedOffset).message(),
        RegionError::Geometry(GeometryError::MisalignedLength).message(),
        RegionError::Geometry(GeometryError::OutOfBounds).message(),
    ];
    for (left, one) in messages.iter().enumerate() {
        assert!(!one.is_empty());
        for (right, other) in messages.iter().enumerate() {
            assert_eq!(left == right, one == other, "two errors share `{one}`");
        }
    }
}

// ---------------------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------------------

/// Re-computes both of a frame's checksums in place, with an implementation of §09's
/// polynomials written for these tests rather than borrowed from the crate.
fn reseal(bytes: &mut [u8]) {
    let sealed = HEADER_BYTES - 2;
    let Some(end) = bytes.len().checked_sub(4) else {
        unreachable!("a frame is at least sixteen bytes long")
    };
    let Some(header) = bytes.get(..sealed).map(crc16) else {
        unreachable!("a frame is at least sixteen bytes long")
    };
    let Some(header_seal) = bytes.get_mut(sealed..HEADER_BYTES) else {
        unreachable!("a frame is at least sixteen bytes long")
    };
    header_seal.copy_from_slice(&header.to_le_bytes());

    let Some(frame) = bytes.get(..end).map(crc32) else {
        unreachable!("a frame is at least sixteen bytes long")
    };
    let Some(trailer) = bytes.get_mut(end..) else {
        unreachable!("a frame is at least sixteen bytes long")
    };
    trailer.copy_from_slice(&frame.to_le_bytes());
}

/// CRC-16/CCITT-FALSE, bit by bit.
fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
        }
    }
    crc
}

/// CRC-32/ISO-HDLC, bit by bit.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xEDB8_8320
            };
        }
    }
    !crc
}

/// An integrity check that is not the shipped one, so `Recovery<C>`'s parameter has
/// something to select.
///
/// Both seals are the shipped ones complemented, which is enough: a journal sealed by one and
/// read by the other fails at the header, which is the first thing a reader checks.
struct Other;

impl waymaker_flash::integrity::IntegrityCheck for Other {
    fn header_check(bytes: &[u8]) -> u16 {
        !crc16(bytes)
    }

    fn frame_check(bytes: &[u8]) -> u32 {
        !crc32(bytes)
    }
}

/// A deterministic generator, so a failure can be reproduced from the seed it names.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        u32::try_from(self.next() % u64::from(bound)).unwrap_or(0)
    }

    fn byte(&mut self) -> u8 {
        u8::try_from(self.next() & 0xFF).unwrap_or(0)
    }
}
