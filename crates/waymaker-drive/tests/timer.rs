//! A durable deadline driven end to end: recorded, re-armed across a reset, and replayed.
//!
//! Issue [#33](https://github.com/madmax983/waymaker/issues/33)'s two "done when" clauses,
//! against the real driver, the real codec and `waymaker-fault`'s model of NOR.
//!
//! * **Replay of a fired timer re-arms no hardware.** The only thing this driver does to
//!   hardware is read a clock, so [`Clocks::now`] is counted and required to be untouched
//!   on the boot that replays a firing.
//! * **A flipped clock kind in a recorded frame is refused.** The byte is changed *on
//!   media* and the frame re-sealed with the real codec, so what recovery meets is a frame
//!   a writer could have written.

use waymaker_core::timer::{ClockCapability, ClockKind, TimerSpec};
use waymaker_core::{KernelError, RecordKind, RecordRef, RunId};
use waymaker_drive::demo::{DELAYED_BOUNDS, Delayed, World};
use waymaker_drive::{Conclusion, DriveError, Driver, Progress, Scratch};
use waymaker_fault::Device;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

/// The run these journals belong to.
const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// The persistent instant [`Delayed`] waits for.
const DEADLINE: u64 = 1_700_000_000;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(4096, 1024, 4, 1) else {
        unreachable!("4096/1024/4/1 is a legal geometry")
    };
    geometry
}

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 1024, align) else {
        unreachable!("a 1024-byte region at offset 0 fits this geometry")
    };
    region
}

fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(DELAYED_BOUNDS, layout) else {
        unreachable!("the delayed workflow's bounds fit this layout")
    };
    reserve
}

/// One boot of [`Delayed`] against `device` and `world`.
fn boot(
    device: &mut Device,
    world: &mut World,
) -> Result<Progress, DriveError<<Device as StableStorage>::Error>> {
    let mut workflow = Delayed::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        device,
        world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// The kind byte of every record the journal holds.
fn kinds(device: &mut Device) -> Vec<RecordKind> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
        };
        out.push(record.kind());
    }
    out
}

/// A world whose persistent clock reads `epoch`.
const fn world_at(epoch: u64) -> World {
    let mut world = World::new();
    world.set_epoch(epoch);
    world
}

#[test]
fn a_deadline_in_the_future_records_its_intent_and_suspends_the_run() {
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);

    let progress = boot(&mut device, &mut world);

    assert!(
        matches!(progress, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 500),
        "{progress:?}"
    );
    // §07's order: the deadline is on media before the run can observe it as passed.
    assert_eq!(
        kinds(&mut device),
        vec![RecordKind::RUN_STARTED, RecordKind::TIMER_SCHEDULED]
    );
}

#[test]
fn a_deadline_already_past_fires_in_the_same_boot() {
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE + 1);

    let progress = boot(&mut device, &mut world);

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
    assert!(kinds(&mut device).contains(&RecordKind::TIMER_FIRED));
}

#[test]
fn a_deadline_is_re_armed_across_a_reset_from_the_reading_history_recorded() {
    // The reset takes the RAM the armed timer lived in. What comes back is the record.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    // A second boot with a fresh world: nothing but media survives.
    let mut rebooted = world_at(DEADLINE - 400);
    let progress = boot(&mut device, &mut rebooted);
    assert!(
        matches!(progress, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 400),
        "{progress:?}"
    );
    // The intent is committed once. A second boot must not schedule the timer again.
    assert_eq!(
        kinds(&mut device)
            .iter()
            .filter(|kind| **kind == RecordKind::TIMER_SCHEDULED)
            .count(),
        1
    );

    // And once the epoch passes the instant, the same identity fires.
    let mut later = world_at(DEADLINE);
    assert!(boot(&mut device, &mut later).is_ok());
    assert!(kinds(&mut device).contains(&RecordKind::TIMER_FIRED));
}

#[test]
fn replaying_a_fired_timer_reads_no_clock_at_all() {
    // Issue #33's first "done when". Reading the clock is the only thing this driver does
    // to hardware, so a boot that replays a firing and touches it is a boot that re-armed.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE + 1);
    assert!(boot(&mut device, &mut world).is_ok());
    assert!(world.clock_reads() > 0, "the first boot must arm the timer");

    let mut replaying = world_at(DEADLINE + 1);
    let progress = boot(&mut device, &mut replaying);

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
    assert_eq!(
        replaying.clock_reads(),
        0,
        "a timer whose firing is in history must be answered from history"
    );
}

#[test]
fn a_clock_kind_flipped_on_media_is_refused_rather_than_reinterpreted() {
    // Issue #33's second "done when". The byte is changed on the device and the frame is
    // re-sealed with the real codec, so the refusal is about the *policy* rather than about
    // a checksum. The firmware here has the persistent clock, so nothing stops it reading
    // the record — only the fact that the record now names another deadline.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    flip_the_clock_kind(&mut device);

    let mut rebooted = world_at(DEADLINE - 400);
    assert_eq!(
        boot(&mut device, &mut rebooted),
        Err(DriveError::Kernel(KernelError::NondeterministicWorkflow))
    );
}

#[test]
fn a_persistent_deadline_a_firmware_cannot_measure_is_an_incompatible_workflow() {
    // The same flip, met by a firmware built without the clock: the record is intact and
    // this image cannot honour it. §02 decision 8 — a refusal, never a substitution.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    let mut boot_only = world_at(DEADLINE - 400);
    boot_only.set_capability(ClockCapability::BootOnly);
    assert_eq!(
        boot(&mut device, &mut boot_only),
        Err(DriveError::Kernel(KernelError::IncompatibleWorkflow))
    );
}

/// Rewrites the journal's `TimerScheduled` record with the other clock kind.
///
/// The whole record is re-encoded through the real codec, so what lands on media is a frame
/// whose header checksum, frame checksum and commit seal all hold. A test that changed one
/// byte and left the checksums alone would be testing the checksum.
fn flip_the_clock_kind(device: &mut Device) {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two")
    };
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let at;
    let flipped = loop {
        let offset = recovery.offset();
        let Some(step) = recovery.next(device, &mut page) else {
            unreachable!("the journal holds a scheduled timer")
        };
        let Ok(record) = step else {
            unreachable!("the journal this test wrote is legal")
        };
        if let RecordRef::TimerScheduled {
            seq,
            deadline,
            armed_at,
            ..
        } = record
        {
            at = offset;
            break RecordRef::TimerScheduled {
                seq,
                clock_kind: ClockKind::AFTER_BOOT,
                deadline,
                armed_at,
            };
        }
    };

    let mut encoded = [0_u8; 128];
    let Ok(written) = frame::encode(&flipped, align, &mut encoded) else {
        unreachable!("128 bytes hold a scheduled timer")
    };
    // The model only clears bits, so the block is erased before the frame is put back.
    let Ok(()) = device.erase(0, 1024) else {
        unreachable!("the region is one whole erase block")
    };
    let Ok(()) = replay_prefix(device, at as usize) else {
        unreachable!("the prefix was read from this device")
    };
    let Some(bytes) = encoded.get(..written) else {
        unreachable!("the frame is `written` bytes long")
    };
    let Ok(()) = device.program(at, bytes) else {
        unreachable!("the record fits where it came from")
    };
    let Ok(()) = device.barrier() else {
        unreachable!("the model's barrier cannot fail")
    };
}

/// Re-programs the `RunStarted` record the erase above removed.
fn replay_prefix(device: &mut Device, upto: usize) -> Result<(), ()> {
    let Some(align) = ProgramAlign::new(4) else {
        return Err(());
    };
    let mut encoded = [0_u8; 128];
    let record = RecordRef::RunStarted {
        workflow_kind: waymaker_drive::demo::DELAYED_KIND,
        workflow_version: waymaker_drive::demo::DELAYED_VERSION,
        input: b"seed",
    };
    let Ok(written) = frame::encode(&record, align, &mut encoded) else {
        return Err(());
    };
    if written != upto {
        return Err(());
    }
    let Some(bytes) = encoded.get(..written) else {
        return Err(());
    };
    device.program(0, bytes).map_err(|_| ())?;
    device.barrier().map_err(|_| ())
}

#[test]
fn a_timer_and_an_activity_share_one_sequence_space() {
    // Issue #33: one ordered history. The timer takes sequence 0 and the download that
    // follows it takes sequence 1, so a downstream system deduplicating on
    // `(RunId, EffectSeq)` sees two boundaries and not one.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE + 1);
    assert!(boot(&mut device, &mut world).is_ok());

    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut sequences = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journal this test wrote is legal")
        };
        match record {
            RecordRef::TimerScheduled { seq, .. } => sequences.push(("timer", seq.0)),
            RecordRef::EffectScheduled { seq, .. } => sequences.push(("effect", seq.0)),
            _ => {}
        }
    }
    assert_eq!(sequences, vec![("timer", 0), ("effect", 1)]);
}

#[test]
fn a_spec_the_firmware_cannot_service_is_refused_before_a_record_is_written() {
    // A fresh run on a firmware with no persistent clock. Refused at the boundary, so the
    // journal holds the run's own record and nothing else: a committed `TimerScheduled` the
    // firmware could never arm strands the run for ever.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    world.set_capability(ClockCapability::BootOnly);

    assert_eq!(
        boot(&mut device, &mut world),
        Err(DriveError::Kernel(KernelError::NoPersistentClock))
    );
    assert_eq!(kinds(&mut device), vec![RecordKind::RUN_STARTED]);
}

#[test]
fn the_delayed_workflow_waits_for_the_deadline_this_test_file_names() {
    // The fixture the tests above rest on: if `Delayed` stopped waiting for `DEADLINE`,
    // every assertion about remaining ticks would be about another number.
    assert_eq!(
        Delayed::SPEC,
        TimerSpec::AtPersistentTime { instant: DEADLINE }
    );
}
