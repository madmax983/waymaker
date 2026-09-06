//! The rows of design document §08's table a completed history reaches, and the refusals
//! around them.
//!
//! Row 5 — "terminal workflow record + further execution → return the recorded outcome and
//! poll no further" — is the one row a driver only meets when a workflow outlives its own
//! history. It is here rather than in `drive.rs` because reaching it takes a workflow that
//! asks for one more effect than the run it is replaying.

use waymaker_core::{ActivityKind, Outcome, RunId};
use waymaker_core::{EffectSeq, KernelError, RecordRef};
use waymaker_drive::demo::{
    BOUNDS, DOWNLOAD, HASH, HASHED, Pipeline, WORKFLOW_KIND, WORKFLOW_VERSION, World,
};
use waymaker_drive::{
    Boundary, Conclusion, DriveError, Driver, Identity, Progress, Scratch, Suspended, Workflow,
};
use waymaker_fault::{Device, FaultError};
use waymaker_flash::append::Journal;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

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

/// A device holding the reference workflow's completed run.
fn a_completed_run() -> Device {
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("the reference run completes on erased media")
    };
    assert!(
        matches!(progress, Progress::Finished { .. }),
        "{progress:?}"
    );
    device
}

/// The reference workflow's two calls, and one more after them.
struct Persistent {
    calls: usize,
}

impl Workflow for Persistent {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.call(DOWNLOAD, b"url")?;
        self.calls += 1;
        boundary.call(HASH, b"contents-of-the-thing")?;
        self.calls += 1;
        // §08 row 5 is met here: history has nothing left but its terminal record.
        boundary.call(DOWNLOAD, b"again")?;
        self.calls += 1;
        Ok(Outcome::Completed(b"unreachable"))
    }
}

#[test]
fn a_workflow_that_outlives_its_history_is_told_the_run_already_ended() {
    let mut device = a_completed_run();
    let mut workflow = Persistent { calls: 0 };
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("a terminal record is an answer, not a failure")
    };

    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: HASHED.len()
        }
    );
    assert_eq!(&result[..HASHED.len()], HASHED);
    assert_eq!(
        workflow.calls, 2,
        "the third boundary was refused, so §08's \"without polling further\" held"
    );
    assert!(
        world.dispatched().is_empty(),
        "and nothing was dispatched past the end of the run"
    );
}

#[test]
fn a_recorded_outcome_longer_than_the_callers_buffer_is_refused() {
    let mut device = a_completed_run();
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    // `DOWNLOAD` recorded twenty-one bytes, and this holds two.
    let mut result = [0_u8; 2];

    let Err(error) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("a replayed result that does not fit cannot be handed back")
    };
    assert_eq!(
        error,
        DriveError::ResultTooLong {
            produced: 21,
            available: 2
        }
    );
}

#[test]
fn a_journal_whose_first_record_is_not_a_run_is_refused_as_malformed() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    {
        let mut scan = Recovery::new(region());
        while scan.next(&mut device, &mut page).is_some() {}
        let Some(mut journal) = Journal::after(scan) else {
            unreachable!("an erased journal has an append point")
        };
        let record = RecordRef::EffectScheduled {
            seq: EffectSeq(0),
            kind: DOWNLOAD,
            input_len: 3,
            input_crc: frame::input_digest(b"url"),
        };
        let Ok(staged) = journal.stage(&mut device, &record, &mut page) else {
            unreachable!("the record fits the region")
        };
        let Ok(sealable) = staged.payload_barrier(&mut device) else {
            unreachable!("the model's barrier cannot fail")
        };
        let Ok(_) = sealable.commit(&mut device) else {
            unreachable!("the model's program cannot fail here")
        };
    }

    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut result = [0_u8; 64];
    let Err(error) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("a run that never started cannot be replayed")
    };
    assert_eq!(error, DriveError::Kernel(KernelError::MalformedHistory));
    assert!(world.dispatched().is_empty());
}

#[test]
fn a_driver_reports_the_region_and_the_run_it_was_built_for() {
    let driver: Driver = Driver::new(region(), RUN, reserve());
    assert_eq!(driver.region(), region());
    assert_eq!(driver.run(), RUN);
}

#[test]
fn the_reference_workflow_and_world_default_to_having_done_nothing() {
    assert_eq!(Pipeline::default(), Pipeline::new());
    assert_eq!(World::default(), World::new());
    assert!(Pipeline::default().downloaded().is_empty());
    assert!(World::default().dispatched().is_empty());
}

/// A workflow whose second call is a kind the world has no answer for.
struct Unknown;

impl Workflow for Unknown {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.call(ActivityKind(99), b"")?;
        Ok(Outcome::Failed(b"no"))
    }
}

#[test]
fn a_run_that_ends_in_failure_records_a_terminal_failure() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress: Result<Progress, DriveError<FaultError>> = Driver::new(region(), RUN, reserve())
        .boot(
            &mut device,
            &mut world,
            &mut Unknown,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        );
    let Ok(progress) = progress else {
        unreachable!("a failing workflow is a recorded outcome, not a driver error")
    };
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: b"no".len()
        }
    );
    assert_eq!(&result[..2], b"no");
    // Read back, because `Conclusion` came from the workflow rather than from media: a
    // `terminal()` that wrote `RunCompleted` while reporting `Failed` would pass without it.
    assert_eq!(
        kinds(&mut device),
        ["started", "scheduled", "completed", "run-failed"]
    );
}

/// A workflow with no effects and a terminal payload larger than a small result buffer.
struct Verbose {
    payload: [u8; 32],
}

impl Workflow for Verbose {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, _boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Ok(Outcome::Completed(&self.payload))
    }
}

/// Every record the journal holds, by kind.
fn kinds(device: &mut Device) -> Vec<&'static str> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            break;
        };
        out.push(match record {
            RecordRef::RunStarted { .. } => "started",
            RecordRef::EffectScheduled { .. } => "scheduled",
            RecordRef::EffectCompleted { .. } => "completed",
            RecordRef::EffectFailed { .. } => "failed",
            RecordRef::RunCompleted { .. } => "run-completed",
            RecordRef::RunFailed { .. } => "run-failed",
        });
    }
    out
}

#[test]
fn a_terminal_payload_that_does_not_fit_is_refused_before_the_record_is_written() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut workflow = Verbose { payload: [7; 32] };

    {
        let mut small = [0_u8; 8];
        let Err(error) = Driver::new(region(), RUN, reserve()).boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut small,
            },
        ) else {
            unreachable!("a terminal payload that does not fit cannot be handed back")
        };
        assert_eq!(
            error,
            DriveError::ResultTooLong {
                produced: 32,
                available: 8
            }
        );
    }

    // The refusal came before the record. Committing it first would leave a run that
    // completed on media and reported `ResultTooLong` on this boot and on every boot after
    // it, whatever buffer the caller brought.
    assert_eq!(kinds(&mut device), ["started"]);

    let mut roomy = [0_u8; 64];
    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut roomy,
        },
    ) else {
        unreachable!("a caller with room finishes the run")
    };
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: 32
        }
    );
    assert_eq!(kinds(&mut device), ["started", "run-completed"]);
}

#[test]
fn an_activity_answer_that_does_not_fit_is_refused_rather_than_recorded_short() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    let mut workflow = Pipeline::new();
    let mut page = [0_u8; 256];
    // `DOWNLOAD` answers with twenty-one bytes, and this holds two.
    let mut result = [0_u8; 2];

    let Err(error) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("a truncated result would be recorded as history and replayed for ever")
    };
    assert_eq!(
        error,
        DriveError::ResultTooLong {
            produced: 21,
            available: 2
        }
    );
    assert_eq!(
        kinds(&mut device),
        ["started", "scheduled"],
        "the intent is committed and the short answer is not"
    );
}

/// Appends `record` to the journal as it stands, whatever follows it.
fn append_past_the_end(device: &mut Device, record: &RecordRef<'_>) {
    let mut page = [0_u8; 256];
    let mut scan = Recovery::new(region());
    while scan.next(device, &mut page).is_some() {}
    let Some(mut journal) = Journal::after(scan) else {
        unreachable!("the fixtures here end in erased media")
    };
    let Ok(staged) = journal.stage(device, record, &mut page) else {
        unreachable!("the record fits the region")
    };
    let Ok(sealable) = staged.payload_barrier(device) else {
        unreachable!("the model's barrier cannot fail")
    };
    let Ok(_) = sealable.commit(device) else {
        unreachable!("the model's program cannot fail here")
    };
}

/// A committed record after a terminal one, which no execution could have written.
fn a_run_with_a_record_past_its_end() -> Device {
    let mut device = a_completed_run();
    append_past_the_end(
        &mut device,
        &RecordRef::EffectScheduled {
            seq: EffectSeq(2),
            kind: DOWNLOAD,
            input_len: 3,
            input_crc: frame::input_digest(b"url"),
        },
    );
    device
}

#[test]
fn a_record_committed_after_the_run_ended_is_refused_when_the_workflow_ends_too() {
    let mut device = a_run_with_a_record_past_its_end();
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    // The workflow makes its two calls, both replayed, and returns; `conclude` consumes the
    // terminal record and then finds one after it. `ReplayCursor` refuses a record after a
    // terminal one, and a driver that stopped without asking would report a clean finish.
    let Err(error) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("history that could not have been written is refused")
    };
    assert_eq!(error, DriveError::HistoryContinues);
    assert!(world.dispatched().is_empty());
}

#[test]
fn a_record_committed_after_the_run_ended_is_refused_at_an_effect_boundary_too() {
    let mut device = a_run_with_a_record_past_its_end();
    let mut workflow = Persistent { calls: 0 };
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    // The other route to §08 row 5: the workflow asks for a third effect and the machine
    // answers with the terminal record. The check belongs on both paths, so it is on both.
    let Err(error) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("history that could not have been written is refused")
    };
    assert_eq!(error, DriveError::HistoryContinues);
    assert_eq!(workflow.calls, 2);
}

#[test]
fn a_damaged_frame_after_the_run_ended_is_ignored_rather_than_refused() {
    // §14: a frame that fails to decode is ignored and the previous history prefix wins. For
    // a finished run that prefix is the whole run, so this is a completed boot rather than a
    // refusal — which is the line between the two cases above and this one.
    let device = a_run_with_a_record_past_its_end();
    let mut image = device.into_image();
    let last = image
        .iter()
        .rposition(|byte| *byte != 0xFF)
        .unwrap_or_default();
    image[last] &= 0xF0;
    let Some(mut device) = Device::restored(geometry(), image) else {
        unreachable!("the image is device-sized")
    };

    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("a damaged tail after a terminal record is not a refusal")
    };
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: HASHED.len()
        }
    );
}
