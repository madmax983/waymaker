//! The two-barrier write discipline, driven against media that records what it was asked.
//!
//! Design document §07 numbers the steps and issue
//! [#24](https://github.com/madmax983/waymaker/issues/24) makes them a type: write the frame
//! body, wait for a **payload barrier**, program the commit seal, wait for a **commit
//! barrier**. This file is about the order those reach the device in, what the writer
//! refuses, and the write amplification it reports. `tests/commit.rs` is about the seal as
//! bytes, and `waymaker-fault`'s sweep is about what a crash between any two of them leaves
//! behind.

use waymaker_core::{DecodeError, EffectSeq, RecordRef};
use waymaker_flash::append::{AppendError, Journal, WriteAmplification};
use waymaker_flash::frame::{self, ERASED_BYTE, ProgramAlign};
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// What a writer asked of the device, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Program { offset: u32, len: u32 },
    Barrier,
}

/// NOR-shaped media that records the sequence of mutations it was asked for.
struct Nor {
    geometry: Geometry,
    media: Vec<u8>,
    ops: Vec<Op>,
    /// Which barrier to fail, by ordinal, so an unreliable device is a test rather than a
    /// claim.
    fail_barrier_at: Option<usize>,
    barriers: usize,
    /// Whether every program should fail, having changed nothing.
    refuse_programs: bool,
}

impl Nor {
    fn new(geometry: Geometry) -> Self {
        let Ok(capacity) = usize::try_from(geometry.capacity()) else {
            unreachable!("a host holds any capacity this file describes")
        };
        Self {
            geometry,
            media: std::vec![ERASED_BYTE; capacity],
            ops: Vec::new(),
            fail_barrier_at: None,
            barriers: 0,
            refuse_programs: false,
        }
    }

    fn programs(&self) -> Vec<(u32, u32)> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::Program { offset, len } => Some((*offset, *len)),
                Op::Barrier => None,
            })
            .collect()
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
        if self.refuse_programs {
            return Err(GeometryError::OutOfBounds);
        }
        self.ops.push(Op::Program { offset, len });
        let start = usize::try_from(offset).map_err(|_| GeometryError::OutOfBounds)?;
        for (index, wanted) in src.iter().enumerate() {
            let Some(cell) = start
                .checked_add(index)
                .and_then(|at| self.media.get_mut(at))
            else {
                return Err(GeometryError::OutOfBounds);
            };
            // Flash: a program clears bits and never sets them.
            *cell &= *wanted;
        }
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
        let ordinal = self.barriers;
        self.barriers += 1;
        if self.fail_barrier_at == Some(ordinal) {
            return Err(GeometryError::OutOfBounds);
        }
        self.ops.push(Op::Barrier);
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------

const PAGE: usize = 512;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
        unreachable!("8192 is two whole 4096-byte blocks of whole 8-byte units of bytes")
    };
    geometry
}

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(8) else {
        unreachable!("8 is a power of two within the program-size range")
    };
    align
}

fn region(bytes: u32) -> JournalRegion {
    let Ok(region) = JournalRegion::spanning(geometry(), 0, bytes, align()) else {
        unreachable!("the regions in this file are legal programs")
    };
    region
}

const fn record(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectCompleted {
        seq: EffectSeq(seq),
        result: b"payload!",
    }
}

/// A recovery of `region` on `device`, run to its end.
fn recover(device: &mut Nor, region: JournalRegion) -> (Vec<u32>, Option<Ending>) {
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region);
    let mut seen = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        match step {
            Ok(RecordRef::EffectCompleted { seq, .. }) => seen.push(seq.0),
            Ok(_) => seen.push(u32::MAX),
            Err(_) => break,
        }
    }
    (seen, recovery.ending())
}

/// A writer positioned at the start of an erased region.
fn opened(device: &mut Nor, region: JournalRegion) -> Journal {
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region);
    while recovery.next(device, &mut page).is_some() {}
    let Some(journal) = Journal::after(recovery) else {
        unreachable!("an erased region ends cleanly at its first byte")
    };
    journal
}

/// Writes `record` through the whole protocol.
fn commit(
    journal: &mut Journal,
    device: &mut Nor,
    record: &RecordRef<'_>,
) -> Result<WriteAmplification, AppendError<GeometryError>> {
    let mut page = [0_u8; PAGE];
    journal
        .stage(device, record, &mut page)?
        .payload_barrier(device)?
        .commit(device)
}

// ---------------------------------------------------------------------------------------
// The protocol
// ---------------------------------------------------------------------------------------

#[test]
fn a_committed_record_is_a_body_a_barrier_a_seal_and_a_barrier() {
    // §07 steps 1 to 3, as the device sees them. The seal is its own program call, and the
    // barrier between the two is the whole of the discipline: a seal in the same call as its
    // frame is a seal that can reach media first.
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);
    device.ops.clear();

    let written = commit(&mut journal, &mut device, &record(1)).expect("a legal append");

    let body = frame::body_len(&record(1), align()).expect("this record encodes");
    let seal = frame::seal_bytes(align());
    let body_len = u32::try_from(body).expect("host");
    let seal_len = u32::try_from(seal).expect("host");
    assert_eq!(
        device.ops,
        std::vec![
            Op::Program {
                offset: 0,
                len: body_len
            },
            Op::Barrier,
            Op::Program {
                offset: body_len,
                len: seal_len
            },
            Op::Barrier,
        ]
    );
    assert_eq!(written.program_operations(), 2);
    assert_eq!(written.barriers(), 2);
    assert_eq!(written.programmed_bytes(), body_len + seal_len);
    assert_eq!(written.payload_bytes(), 8);
    assert_eq!(written.overhead_bytes(), body_len + seal_len - 8);
}

#[test]
fn the_seal_is_one_program_unit_wide_at_every_granularity() {
    // "Seal sized to the device's program unit, derived from `Geometry::program_size`."
    // `BankLayout::align` is where that derivation lives and `JournalRegion` refuses a
    // granularity below the device's program unit, so the seal below is that unit at each of
    // them.
    for unit in [1_u32, 2, 4, 8, 16] {
        let Ok(geometry) = Geometry::new(8192, 4096, unit, 1) else {
            unreachable!("a legal geometry")
        };
        let Some(align) = ProgramAlign::new(u16::try_from(unit).expect("host")) else {
            unreachable!("a power of two")
        };
        let Ok(region) = JournalRegion::spanning(geometry, 0, 256, align) else {
            unreachable!("a legal program region")
        };
        let mut device = Nor::new(geometry);
        let mut page = [0_u8; PAGE];
        let mut recovery = Recovery::new(region);
        while recovery.next(&mut device, &mut page).is_some() {}
        let Some(mut journal) = Journal::after(recovery) else {
            unreachable!("an erased region ends cleanly")
        };
        device.ops.clear();

        let mut staging = [0_u8; PAGE];
        journal
            .stage(&mut device, &record(0), &mut staging)
            .and_then(|staged| staged.payload_barrier(&mut device))
            .and_then(|sealable| sealable.commit(&mut device))
            .expect("a legal append");

        let programs = device.programs();
        let Some((_, seal_len)) = programs.get(1) else {
            unreachable!("a commit programs twice")
        };
        assert_eq!(
            *seal_len, unit,
            "the seal at a {unit}-byte program unit is not one unit"
        );
    }
}

#[test]
fn a_committed_record_reads_back_through_recovery() {
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);
    for seq in 0..3 {
        commit(&mut journal, &mut device, &record(seq)).expect("a legal append");
    }

    let (seen, ending) = recover(&mut device, region);
    assert_eq!(seen, [0, 1, 2]);
    assert_eq!(
        ending,
        Some(Ending::Clean {
            append_at: journal.offset()
        }),
        "the writer and the reader must agree about where history ends"
    );
}

#[test]
fn a_writer_resumes_where_the_last_boot_left_off() {
    // The round trip that matters on a device: write, lose power, recover, keep appending.
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut first = opened(&mut device, region);
    commit(&mut first, &mut device, &record(0)).expect("a legal append");
    let ended_at = first.offset();

    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region);
    while recovery.next(&mut device, &mut page).is_some() {}
    let Some(mut resumed) = Journal::after(recovery) else {
        unreachable!("a journal of committed records ends cleanly")
    };
    assert_eq!(resumed.offset(), ended_at);
    assert_eq!(
        resumed.amplification(),
        WriteAmplification::NONE,
        "a resumed writer has not written anything yet"
    );

    commit(&mut resumed, &mut device, &record(1)).expect("a legal append");
    assert_eq!(recover(&mut device, region).0, [0, 1]);
}

#[test]
fn amplification_accumulates_over_the_records_a_writer_committed() {
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);

    let one = commit(&mut journal, &mut device, &record(0)).expect("a legal append");
    let two = commit(&mut journal, &mut device, &record(1)).expect("a legal append");

    assert_eq!(journal.amplification(), one.plus(two));
    assert_eq!(journal.amplification().program_operations(), 4);
    assert_eq!(journal.amplification().barriers(), 4);
    assert_eq!(journal.amplification().payload_bytes(), 16);
}

// ---------------------------------------------------------------------------------------
// What the writer refuses
// ---------------------------------------------------------------------------------------

#[test]
fn a_journal_opens_only_where_a_recovery_said_it_is_safe() {
    // §10's anti-bricking rule as a constructor. Every ending but a clean one leaves
    // programmed cells at the offset, and there is no other way to build a writer.
    let mut device = Nor::new(geometry());
    let region = region(256);

    // A scan that has not finished has no append point either.
    let unfinished = Recovery::new(region);
    assert!(Journal::after(unfinished).is_none());

    // A journal whose tail is a frame nobody sealed.
    let mut journal = opened(&mut device, region);
    let mut page = [0_u8; PAGE];
    let staged = journal
        .stage(&mut device, &record(0), &mut page)
        .expect("a legal stage");
    drop(staged);
    // ... and the power went. The body is on media and the seal is not.
    let mut recovery = Recovery::new(region);
    while recovery.next(&mut device, &mut page).is_some() {}
    assert_eq!(recovery.ending(), Some(Ending::Unsealed { at: 0 }));
    assert!(
        Journal::after(recovery).is_none(),
        "an unsealed tail is not an append point"
    );
}

#[test]
fn every_step_refuses_a_device_the_region_was_not_validated_against() {
    // `stage` is not the only step that touches media. A payload barrier taken on some other
    // device orders nothing on this one, so the frame would be sealed without ever having
    // been made durable; and a commit taken elsewhere programs a seal at an offset that
    // device never validated. Review of this change found both.
    let Ok(elsewhere) = Geometry::new(8192, 4096, 16, 1) else {
        unreachable!("a legal geometry")
    };

    for step in 0..2_usize {
        let mut device = Nor::new(geometry());
        let region = region(256);
        let mut journal = opened(&mut device, region);
        let mut other = Nor::new(elsewhere);
        let mut page = [0_u8; PAGE];

        let staged = journal
            .stage(&mut device, &record(0), &mut page)
            .expect("a legal stage");
        if step == 0 {
            assert_eq!(
                staged.payload_barrier(&mut other).err(),
                Some(AppendError::WrongDevice)
            );
        } else {
            let sealable = staged
                .payload_barrier(&mut device)
                .expect("the payload barrier holds");
            assert_eq!(
                sealable.commit(&mut other).err(),
                Some(AppendError::WrongDevice)
            );
        }
        assert!(other.ops.is_empty(), "a refusal must not touch media");
        assert_eq!(journal.offset(), 0);
    }
}

#[test]
fn a_record_that_does_not_fit_is_refused_before_a_byte_moves() {
    // §10 reserves tail space and decides "does this fit" before anything is programmed:
    // a half-appended record at the end of a bank is a bank that cannot be booted or grown.
    let mut device = Nor::new(geometry());
    let region = region(32);
    let mut journal = opened(&mut device, region);
    device.ops.clear();

    let long = RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: &[0x5A; 64],
    };
    let mut page = [0_u8; PAGE];
    let needed = u32::try_from(frame::encoded_len(&long, align()).expect("encodes")).expect("host");
    assert_eq!(
        journal.stage(&mut device, &long, &mut page).err(),
        Some(AppendError::NoRoom {
            needed,
            available: 32
        })
    );
    assert!(device.ops.is_empty(), "a refusal must not touch media");
    assert_eq!(journal.offset(), 0);
    assert_eq!(journal.room(), 32);
}

#[test]
fn a_page_too_small_for_the_record_is_refused_before_a_byte_moves() {
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);
    device.ops.clear();

    let mut crumb = [0_u8; 8];
    assert_eq!(
        journal.stage(&mut device, &record(0), &mut crumb).err(),
        Some(AppendError::Encode(DecodeError::LengthOutOfBounds))
    );
    assert!(device.ops.is_empty(), "a refusal must not touch media");
}

#[test]
fn a_writer_handed_another_device_is_refused_before_a_byte_moves() {
    // The same failure `Recovery` closes, in the direction that programs rather than reads:
    // every bound the region proved was proved against one geometry.
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);

    let Ok(elsewhere) = Geometry::new(8192, 4096, 16, 1) else {
        unreachable!("a legal geometry")
    };
    let mut other = Nor::new(elsewhere);
    let mut page = [0_u8; PAGE];
    assert_eq!(
        journal.stage(&mut other, &record(0), &mut page).err(),
        Some(AppendError::WrongDevice)
    );
    assert!(other.ops.is_empty(), "a refusal must not touch media");
}

#[test]
fn a_payload_barrier_that_fails_leaves_nothing_that_can_be_sealed() {
    // §12: a caller that met an error at a barrier has learned nothing about what is on
    // media. So the staged record is consumed and there is no value left that can program a
    // seal — the failure closes in the direction that cannot commit half a protocol.
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);
    device.fail_barrier_at = Some(device.barriers);

    let mut page = [0_u8; PAGE];
    let staged = journal
        .stage(&mut device, &record(0), &mut page)
        .expect("a legal stage");
    assert_eq!(
        staged.payload_barrier(&mut device).err(),
        Some(AppendError::Storage(GeometryError::OutOfBounds))
    );

    // The frame body is on media and its seal is not, which is exactly what recovery is
    // required to refuse.
    assert_eq!(journal.offset(), 0, "an uncommitted record is not history");
    let (seen, ending) = recover(&mut device, region);
    assert!(seen.is_empty());
    assert_eq!(ending, Some(Ending::Unsealed { at: 0 }));
}

#[test]
fn a_commit_barrier_that_fails_does_not_advance_the_writer() {
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);
    device.fail_barrier_at = Some(device.barriers + 1);

    let mut page = [0_u8; PAGE];
    let sealable = journal
        .stage(&mut device, &record(0), &mut page)
        .expect("a legal stage")
        .payload_barrier(&mut device)
        .expect("the payload barrier holds");
    assert_eq!(
        sealable.commit(&mut device).err(),
        Some(AppendError::Storage(GeometryError::OutOfBounds))
    );
    assert_eq!(
        journal.offset(),
        0,
        "a record whose commit barrier failed is not history"
    );
}

#[test]
fn a_program_that_fails_does_not_advance_the_writer() {
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);
    device.refuse_programs = true;

    let mut page = [0_u8; PAGE];
    assert_eq!(
        journal.stage(&mut device, &record(0), &mut page).err(),
        Some(AppendError::Storage(GeometryError::OutOfBounds))
    );
    assert_eq!(journal.offset(), 0);
}

#[test]
fn a_dropped_stage_ends_the_journal_rather_than_letting_the_next_record_overwrite_it() {
    // The append point is erased media, and a frame body on it is not. A writer that carried
    // on would program a second header over the first — and on NOR a programmed bit cannot be
    // returned to one, so the bank would fail its own header checksum on every boot for ever.
    let mut device = Nor::new(geometry());
    let region = region(256);
    let mut journal = opened(&mut device, region);

    let mut page = [0_u8; PAGE];
    let staged = journal
        .stage(&mut device, &record(0), &mut page)
        .expect("a legal stage");
    drop(staged);

    let mut second = [0_u8; PAGE];
    assert_eq!(
        journal.stage(&mut device, &record(1), &mut second).err(),
        Some(AppendError::Interrupted)
    );
    assert_eq!(journal.offset(), 0);
}

#[test]
fn a_failed_step_ends_the_journal_whichever_step_it_was() {
    // Every one of the three ways a record can stop short leaves programmed cells at the
    // offset, so all three have to give the next caller the same answer.
    for failing in 0..3_usize {
        let mut device = Nor::new(geometry());
        let region = region(256);
        let mut journal = opened(&mut device, region);
        let mut page = [0_u8; PAGE];

        match failing {
            0 => {
                device.refuse_programs = true;
                let outcome = journal.stage(&mut device, &record(0), &mut page);
                assert!(outcome.is_err(), "the program was refused");
                device.refuse_programs = false;
            }
            1 => {
                device.fail_barrier_at = Some(device.barriers);
                let outcome = journal
                    .stage(&mut device, &record(0), &mut page)
                    .expect("a legal stage")
                    .payload_barrier(&mut device);
                assert!(outcome.is_err(), "the payload barrier was refused");
                device.fail_barrier_at = None;
            }
            _ => {
                device.fail_barrier_at = Some(device.barriers + 1);
                let outcome = journal
                    .stage(&mut device, &record(0), &mut page)
                    .expect("a legal stage")
                    .payload_barrier(&mut device)
                    .expect("the payload barrier holds")
                    .commit(&mut device);
                assert!(outcome.is_err(), "the commit barrier was refused");
                device.fail_barrier_at = None;
            }
        }

        let mut second = [0_u8; PAGE];
        assert_eq!(
            journal.stage(&mut device, &record(1), &mut second).err(),
            Some(AppendError::Interrupted),
            "step {failing} left a journal that still accepts records"
        );
    }
}

#[test]
fn a_full_journal_refuses_the_next_record_rather_than_wrapping() {
    let mut device = Nor::new(geometry());
    let region = region(64);
    let mut journal = opened(&mut device, region);
    let each =
        u32::try_from(frame::encoded_len(&record(0), align()).expect("encodes")).expect("host");
    assert_eq!(each, 32, "a padded frame and a seal");

    commit(&mut journal, &mut device, &record(0)).expect("a legal append");
    commit(&mut journal, &mut device, &record(1)).expect("a legal append");
    assert_eq!(journal.room(), 0);

    let mut page = [0_u8; PAGE];
    assert_eq!(
        journal.stage(&mut device, &record(2), &mut page).err(),
        Some(AppendError::NoRoom {
            needed: each,
            available: 0
        })
    );
}
