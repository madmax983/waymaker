//! A workflow driven to completion with no `Future`, no Embassy and no allocation.
//!
//! Issue [#28](https://github.com/madmax983/waymaker/issues/28)'s first "done when". The
//! driver under test is the library's, the workflow is the library's reference one, and the
//! media is `waymaker-fault`'s model of NOR — so the bytes below are bytes this workspace
//! really writes rather than a fixture that agrees with it.

use waymaker_core::{EffectId, EffectSeq, RecordKind, RecordRef, RunId};
use waymaker_drive::demo::{BOUNDS, DOWNLOAD, DOWNLOADED, HASH, HASHED, Pipeline, World};
use waymaker_drive::{Conclusion, DriveError, Driver, Progress, Scratch};
use waymaker_fault::Device;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

/// The run these journals belong to. On media it lives in the bank header.
const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// A device big enough for the histories here, at a granularity a real part has.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(4096, 1024, 4, 1) else {
        unreachable!("4096/1024/4/1 is a legal geometry")
    };
    geometry
}

/// The journal every test writes into: one erase block, at the device's program unit.
fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 1024, align) else {
        unreachable!("a 1024-byte region at offset 0 fits this geometry")
    };
    region
}

/// §10's reserve the driver gates every append with.
fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("the reference workflow's bounds fit this layout")
    };
    reserve
}

/// Every record the journal holds, decoded.
fn history(device: &mut Device) -> Vec<RecordKindAndBytes> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
        };
        out.push(RecordKindAndBytes::of(&record));
    }
    out
}

/// One record, summarised so a test can compare a whole history in one assertion.
#[derive(Debug, PartialEq, Eq)]
enum RecordKindAndBytes {
    RunStarted(u16, u16, Vec<u8>),
    EffectScheduled(u32),
    EffectCompleted(u32, Vec<u8>),
    EffectFailed(u32, Vec<u8>),
    TimerScheduled(u32),
    TimerFired(u32),
    VersionMarker(u32, u16, u16),
    RunCompleted(Vec<u8>),
    RunFailed(Vec<u8>),
}

impl RecordKindAndBytes {
    fn of(record: &RecordRef<'_>) -> Self {
        match *record {
            RecordRef::RunStarted {
                workflow_kind,
                workflow_version,
                input,
            } => Self::RunStarted(workflow_kind, workflow_version, input.to_vec()),
            RecordRef::EffectScheduled { seq, .. } => Self::EffectScheduled(seq.0),
            RecordRef::EffectCompleted { seq, result } => {
                Self::EffectCompleted(seq.0, result.to_vec())
            }
            RecordRef::EffectFailed { seq, error } => Self::EffectFailed(seq.0, error.to_vec()),
            RecordRef::TimerScheduled { seq, .. } => Self::TimerScheduled(seq.0),
            RecordRef::TimerFired { seq } => Self::TimerFired(seq.0),
            RecordRef::VersionMarker { seq, gate, version } => {
                Self::VersionMarker(seq.0, gate.0, version)
            }
            RecordRef::RunCompleted { result } => Self::RunCompleted(result.to_vec()),
            RecordRef::RunFailed { error } => Self::RunFailed(error.to_vec()),
        }
    }
}

#[test]
fn a_workflow_runs_to_completion_on_an_erased_journal() {
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve())
        .boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .expect("the run completes");

    let Progress::Finished {
        conclusion,
        result_len,
    } = progress
    else {
        panic!("the run finished: {progress:?}");
    };
    assert_eq!(conclusion, Conclusion::Completed);
    assert_eq!(&result[..result_len], HASHED);

    // Both effects reached the world, in order, under the identities the run minted.
    let dispatched: Vec<_> = world.dispatched().iter().map(|call| call.kind).collect();
    assert_eq!(dispatched, [DOWNLOAD, HASH]);
    let seqs: Vec<_> = world
        .dispatched()
        .iter()
        .map(|call| call.id.seq)
        .collect::<Vec<_>>();
    assert_eq!(seqs, [EffectSeq(0), EffectSeq(1)]);

    // And §07's order is on media: every intent precedes the outcome that resolves it.
    assert_eq!(
        history(&mut device),
        [
            RecordKindAndBytes::RunStarted(7, 1, b"seed".to_vec()),
            RecordKindAndBytes::EffectScheduled(0),
            RecordKindAndBytes::EffectCompleted(0, DOWNLOADED.to_vec()),
            RecordKindAndBytes::EffectScheduled(1),
            RecordKindAndBytes::EffectCompleted(1, HASHED.to_vec()),
            RecordKindAndBytes::RunCompleted(HASHED.to_vec()),
        ]
    );
}

#[test]
fn a_completed_run_replays_from_history_and_dispatches_nothing() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    {
        let mut workflow = Pipeline::new();
        let mut world = World::new();
        Driver::new(region(), RUN, reserve())
            .boot(
                &mut device,
                &mut world,
                &mut workflow,
                Scratch {
                    page: &mut page,
                    result: &mut result,
                },
            )
            .expect("the run completes");
    }

    // A cold start over the same media, with a workflow that has never run.
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let progress = Driver::new(region(), RUN, reserve())
        .boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .expect("the recovered run replays");

    let Progress::Finished {
        conclusion,
        result_len,
    } = progress
    else {
        panic!("history holds a terminal record: {progress:?}");
    };
    assert_eq!(conclusion, Conclusion::Completed);
    assert_eq!(&result[..result_len], HASHED);
    assert!(
        world.dispatched().is_empty(),
        "a replayed effect is answered by history, never by the world"
    );
    assert_eq!(
        workflow.hashed(),
        HASHED,
        "the workflow observed the recorded results, not fresh ones"
    );
}

#[test]
fn an_activity_that_is_not_ready_suspends_the_run_under_a_committed_identity() {
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let mut world = World::pending_at(0);
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve())
        .boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .expect("a pending activity is not a failure");

    let Progress::Waiting { id } = progress else {
        panic!("the run waits: {progress:?}");
    };
    assert_eq!(id.run, RUN);
    assert_eq!(id.seq, EffectSeq(0));

    // §02 decision 3: the intent crossed a durability barrier before anything was
    // dispatched, so the journal holds the schedule and nothing after it.
    assert_eq!(
        history(&mut device),
        [
            RecordKindAndBytes::RunStarted(7, 1, b"seed".to_vec()),
            RecordKindAndBytes::EffectScheduled(0),
        ]
    );
}

#[test]
fn a_reboot_redelivers_the_effect_under_the_identity_it_was_scheduled_with() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    {
        // The *second* effect is the one left outstanding, on purpose. A driver that minted
        // a fresh identity on redelivery would answer `EffectSeq(0)` below, because that is
        // where `EffectIdAllocator` starts — so a run whose outstanding effect is the first
        // one cannot tell redelivery from re-minting at all.
        let mut workflow = Pipeline::new();
        let mut world = World::pending_at(1);
        let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        ) else {
            unreachable!("a pending activity is not a failure")
        };
        assert_eq!(
            progress,
            Progress::Waiting {
                id: EffectId {
                    run: RUN,
                    seq: EffectSeq(1)
                }
            }
        );
    }

    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("the resumed run completes")
    };
    let Progress::Finished { conclusion, .. } = progress else {
        panic!("the resumed run completes: {progress:?}");
    };
    assert_eq!(conclusion, Conclusion::Completed);

    // §14's redelivery contract: the resumed run dispatches the outstanding effect and
    // nothing else, under the sequence its schedule record already committed.
    let dispatched: Vec<_> = world
        .dispatched()
        .iter()
        .map(|call| (call.id.seq, call.kind))
        .collect();
    assert_eq!(dispatched, [(EffectSeq(1), HASH)]);

    // And the journal holds one schedule per effect. A driver that minted a fresh identity
    // would have written a second `EffectScheduled(0)` here.
    assert_eq!(
        history(&mut device),
        [
            RecordKindAndBytes::RunStarted(7, 1, b"seed".to_vec()),
            RecordKindAndBytes::EffectScheduled(0),
            RecordKindAndBytes::EffectCompleted(0, DOWNLOADED.to_vec()),
            RecordKindAndBytes::EffectScheduled(1),
            RecordKindAndBytes::EffectCompleted(1, HASHED.to_vec()),
            RecordKindAndBytes::RunCompleted(HASHED.to_vec()),
        ]
    );
}

#[test]
fn a_record_kind_from_the_future_stops_the_driver_rather_than_being_skipped() {
    // Issue #41's third reader obligation, at the layer that has to obey it. §09 permits
    // skipping an unknown record kind at no format version, so a reader that meets one must
    // stop, expose the prefix before it as the whole of history, and offer no append point.
    // The first two are `waymaker-flash`'s and are tested there; this is the third, and it
    // is the one a *driver* can get wrong, because a driver that appended after the frame it
    // could not read would overwrite a record a shipped device committed.
    //
    // It is also what makes downgrade a data-loss operation rather than a refusal: the run
    // is not resumable by this image, and ADR 0037 says so.
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    {
        let mut workflow = Pipeline::new();
        let mut world = World::pending_at(0);
        Driver::new(region(), RUN, reserve())
            .boot(
                &mut device,
                &mut world,
                &mut workflow,
                Scratch {
                    page: &mut page,
                    result: &mut result,
                },
            )
            .expect("a pending activity is not a failure");
    }

    // A frame this firmware cannot read, wearing `UNNUMBERED_KIND`.
    let mut image = device.into_image();
    let end = image
        .iter()
        .rposition(|byte| *byte != 0xFF)
        .expect("the journal holds records")
        + 1;
    let at = ProgramAlign::new(4)
        .and_then(|align| align.round_up(end))
        .expect("the journal is written at a four-byte granularity");
    let frame = future_frame(UNNUMBERED_KIND);
    image
        .get_mut(at..at + frame.len())
        .expect("the region holds one more frame")
        .copy_from_slice(&frame);
    let mut device = Device::restored(geometry(), image).expect("the image is device-sized");

    // The cause is asserted rather than assumed. Without this the test would pass on a
    // frame that was merely damaged, which is a different refusal reached the same way —
    // and the version of this test in `waymaker-flash` was exactly that for two rungs.
    {
        let mut recovery = Recovery::new(region());
        let mut seen = 0_usize;
        let mut refusal = None;
        while let Some(step) = recovery.next(&mut device, &mut page) {
            match step {
                Ok(_) => seen += 1,
                Err(error) => {
                    refusal = Some(error);
                    break;
                }
            }
        }
        assert!(seen > 0, "the prefix before the unknown frame is history");
        assert!(
            matches!(
                refusal,
                Some(waymaker_flash::recovery::RecoveryError::Decode(
                    waymaker_core::DecodeError::UnknownRecordKind
                ))
            ),
            "recovery stopped for some other reason than the kind it could not read: {refusal:?}"
        );
        assert_eq!(recovery.append_offset(), None);
    }

    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let error = Driver::new(region(), RUN, reserve())
        .boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .expect_err("a record this firmware cannot read has no append point after it");
    assert!(
        matches!(error, DriveError::NoAppendPoint | DriveError::Recovery(_)),
        "{error:?}"
    );
    assert!(
        world.dispatched().is_empty(),
        "nothing is dispatched from a journal this firmware cannot finish reading"
    );
}

/// A record kind no version of this format numbers.
///
/// Past the last number the format names, so it cannot rot: a kind added later takes the
/// next unused number, not this one. The assertion is what says so if that stops being true.
const UNNUMBERED_KIND: u8 = 0xFE;
const _: () = assert!(RecordKind::CHILD_STARTED.0 < UNNUMBERED_KIND);

/// A structurally sound frame wearing `kind`, at the four-byte granularity these tests use.
///
/// Built by encoding a real record and re-sealing it, so every check but the kind holds —
/// which is what makes the driver's refusal a statement about the kind rather than about
/// damage.
fn future_frame(kind: u8) -> Vec<u8> {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let mut page = [0_u8; 64];
    let Ok(written) =
        waymaker_flash::frame::encode(&RecordRef::RunFailed { error: b"x" }, align, &mut page)
    else {
        unreachable!("a 64-byte page holds a three-byte record")
    };
    let mut bytes = page.get(..written).unwrap_or_default().to_vec();
    if let Some(slot) = bytes.get_mut(3) {
        *slot = kind;
    }

    let header = crc16(bytes.get(..10).unwrap_or_default());
    if let Some(seal) = bytes.get_mut(10..12) {
        seal.copy_from_slice(&header.to_le_bytes());
    }
    let payload_len = bytes
        .get(8..10)
        .and_then(|slice| <[u8; 2]>::try_from(slice).ok())
        .map_or(0, |declared| usize::from(u16::from_le_bytes(declared)));
    let covered = 12 + payload_len;
    let frame = crc32(bytes.get(..covered).unwrap_or_default());
    if let Some(trailer) = bytes.get_mut(covered..covered + 4) {
        trailer.copy_from_slice(&frame.to_le_bytes());
    }
    let Some(body) = align.round_up(covered + 4) else {
        unreachable!("a record that encoded has a body length")
    };
    let pattern = waymaker_flash::frame::commit_seal(frame);
    for (index, slot) in bytes.iter_mut().skip(body).enumerate() {
        if let Some(byte) = pattern.get(index % pattern.len()) {
            *slot = *byte;
        }
    }
    bytes
}

/// CRC-16/CCITT-FALSE, bit by bit, written here rather than borrowed from the crate.
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
    crc ^ 0xFFFF_FFFF
}

#[test]
fn a_journal_that_cannot_be_extended_is_refused_rather_than_appended_to() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    {
        let mut workflow = Pipeline::new();
        let mut world = World::pending_at(0);
        Driver::new(region(), RUN, reserve())
            .boot(
                &mut device,
                &mut world,
                &mut workflow,
                Scratch {
                    page: &mut page,
                    result: &mut result,
                },
            )
            .expect("a pending activity is not a failure");
    }

    // A byte cleared inside the last frame: the scan stops there, so there is no append
    // point and §14's "previous history prefix wins" is all a driver may have.
    let mut image = device.into_image();
    let last = image
        .iter()
        .rposition(|byte| *byte != 0xFF)
        .expect("the journal holds records");
    image[last] &= 0xF0;
    let mut device = Device::restored(geometry(), image).expect("the image is device-sized");

    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let error = Driver::new(region(), RUN, reserve())
        .boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .expect_err("a damaged tail has no append point");
    assert!(
        matches!(
            error,
            DriveError::NoAppendPoint | DriveError::Recovery(_) | DriveError::Kernel(_)
        ),
        "{error:?}"
    );
    assert!(
        world.dispatched().is_empty(),
        "nothing is dispatched from a journal that cannot record the intent"
    );
}
