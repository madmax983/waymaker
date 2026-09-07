//! Design document §14's failure-semantics table, one named test per row.
//!
//! Issue [#31](https://github.com/madmax983/waymaker/issues/31): "each row has a named test,
//! and the test name matches the row so a failure reads as a spec violation". The ten tests
//! below are named after the ten rows, in the table's order, and `xtask`'s `failure-matrix`
//! rule reads their names out of this file. The rig half is
//! `crates/waymaker-rig/tests/matrix.rs`.
//!
//! # How a crash point is put in a row
//!
//! The reference run writes six records, and the two-barrier writer spends four storage
//! operations on each: the frame, the payload barrier, the seal, the commit barrier.
//! [`the_reference_run_is_six_records_of_four_operations_each`] pins that shape, so an
//! injection's operation index says which record was being written and how far it got.
//! That is the *failure point* column. The *recovery result* column is read off the media
//! the crash left, and the two are cross-checked: a class whose media disagrees with its
//! operation is a panic, not a row.
//!
//! One row is not a storage crash point. "During physical activity" is the world being
//! entered and not returning, which the injector cannot produce and
//! [`during_physical_activity_the_effect_is_redelivered_and_the_activity_tolerates_the_duplicate_attempt`]
//! models with a world that performs the effect and then never answers.
//!
//! Rows 2 and 6 are decided by what recovery produced, and the operation is the check. A
//! seal that landed whole with its commit barrier refused is recovered on this model — §15
//! lets recovery include an unacknowledged complete record — so it sits in row 2 or row 6
//! beside the one point that is strictly after the barrier, a watchdog reset at
//! `Progress::Whole`. Row 2 has no power cut after a *returned* barrier: the driver
//! dispatches as soon as the barrier returns, so that world is row 4. Whether a seal with no
//! barrier is durable on a part is §12's contract and `waymaker-conformance`'s.
//!
//! A failed call (`Interruption::Failure`) is not a reset. The driver stops on it, the media
//! are in one of the states above, and the point is classified like any other; the census
//! requires the two reset causes separately.
//!
//! # Where this deviates from §14
//!
//! Row 5 says "redeliver". A torn completion leaves a journal with no append point
//! (ADR 0018), so the driver refuses the bank rather than redelivering into it; the run's
//! continuation is §10's `continue_as_new`, which is a new run. The test asserts what holds
//! — the torn completion is ignored, no partial bytes reach the workflow, nothing is
//! dispatched — and asserts the refusal rather than pretending a redelivery.

use core::cell::RefCell;

use waymaker_core::{
    ActivityKind, DecodeError, EffectId, EffectSeq, KernelError, Outcome, RecordRef, RunId,
};
use waymaker_drive::demo::{BOUNDS, DOWNLOAD, DOWNLOADED, HASHED, Pipeline, World};
use waymaker_drive::{
    Boundary, DriveError, Driver, Identity, Progress, Scratch, Suspended, Workflow,
};
use waymaker_fault::{
    Device, FaultError, Harness, Injection, Interruption, Op, Progress as Landed, Run, Session,
};
use waymaker_flash::bank::{self, Authority, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::capacity::{Refusal, Reserve};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery, RecoveryError};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_flash::swap::{Retired, Swap};
use waymaker_rig::matrix::{Matrix, Row};

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);
const NEXT_RUN: RunId = RunId(0x0BAD_F00D_0000_0031);
const PAGE: usize = 256;

// ---------------------------------------------------------------------------------------
// The reference run, and where a crash point lands in it
// ---------------------------------------------------------------------------------------

/// Two erase blocks: the journal is one, the other is the bank a `continue_as_new` prices.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 512, 4, 1) else {
        unreachable!("1024 is two whole 512-byte blocks of 4-byte units")
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
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 512, align()) else {
        unreachable!("the region is the device's first erase block")
    };
    region
}

fn reserve_for(layout: BankLayout) -> Option<Reserve> {
    Reserve::for_layout(BOUNDS, layout).ok()
}

fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Some(reserve) = reserve_for(layout) else {
        unreachable!("the reference workflow's bounds fit this layout")
    };
    reserve
}

/// One record of a run, by shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Record {
    Started,
    Schedule(u32),
    Outcome(u32),
    Terminal,
}

impl Record {
    const fn of(record: &RecordRef<'_>) -> Self {
        match *record {
            RecordRef::RunStarted { .. } => Self::Started,
            RecordRef::EffectScheduled { seq, .. } => Self::Schedule(seq.0),
            RecordRef::EffectCompleted { seq, .. } | RecordRef::EffectFailed { seq, .. } => {
                Self::Outcome(seq.0)
            }
            RecordRef::RunCompleted { .. } | RecordRef::RunFailed { .. } => Self::Terminal,
        }
    }
}

/// What the fault-free reference run writes, in order.
const REFERENCE: [Record; 6] = [
    Record::Started,
    Record::Schedule(0),
    Record::Outcome(0),
    Record::Schedule(1),
    Record::Outcome(1),
    Record::Terminal,
];

/// The four storage operations one record costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Frame,
    PayloadBarrier,
    Seal,
    CommitBarrier,
}

const STEPS: [Step; 4] = [
    Step::Frame,
    Step::PayloadBarrier,
    Step::Seal,
    Step::CommitBarrier,
];

/// The record and step the crash landed in, and whether any of the step reached media.
///
/// Normalised in one way: a power cut *after* a commit barrier returned is met at the next
/// record's frame program, which is where the writer learns of it. `None` for a crash before
/// anything was written.
fn written(run: &Run) -> Option<(Record, Step, bool)> {
    let injection = run.injection()?;
    let (record, step) = (injection.op / STEPS.len(), injection.op % STEPS.len());
    let after_a_commit_barrier = injection.progress == Landed::Whole
        && injection.interruption == Interruption::PowerLoss
        && step == STEPS.len() - 1
        && record + 1 < REFERENCE.len();
    let (record, step, landed) = if after_a_commit_barrier {
        (record + 1, 0, false)
    } else {
        (record, step, injection.progress != Landed::None)
    };
    Some((*REFERENCE.get(record)?, *STEPS.get(step)?, landed))
}

/// The committed history an image recovers to, and how the scan ended.
fn history_of(device: &mut Device, region: JournalRegion) -> (Vec<Record>, Option<Ending>) {
    let mut recovery = Recovery::new(region);
    let mut page = [0_u8; PAGE];
    let mut history = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else { break };
        history.push(Record::of(&record));
    }
    (history, recovery.ending())
}

fn history(image: &[u8]) -> (Vec<Record>, Option<Ending>) {
    history_of(&mut restored(image), region())
}

fn restored(image: &[u8]) -> Device {
    let Some(device) = Device::restored(geometry(), image.to_vec()) else {
        unreachable!("the image is device-sized")
    };
    device
}

/// Every outcome payload an image recovers to.
fn payloads(image: &[u8]) -> Vec<Vec<u8>> {
    let mut device = restored(image);
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; PAGE];
    let mut out = Vec::new();
    while let Some(Ok(record)) = recovery.next(&mut device, &mut page) {
        match record {
            RecordRef::EffectCompleted { result, .. } => out.push(result.to_vec()),
            RecordRef::EffectFailed { error, .. } => out.push(error.to_vec()),
            _ => {}
        }
    }
    out
}

/// One boot of `workflow` over `storage`, with `world`.
fn boot<S: StableStorage, W: Workflow>(
    storage: &mut S,
    world: &mut World,
    workflow: &mut W,
    region: JournalRegion,
    run: RunId,
    reserve: Reserve,
) -> Result<Progress, DriveError<S::Error>> {
    let mut page = [0_u8; PAGE];
    let mut result = [0_u8; 64];
    Driver::new(region, run, reserve).boot(
        storage,
        world,
        workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// The reference run over `session`, and the sequences its world performed.
fn reference(session: &mut Session) -> (Result<(), DriveError<FaultError>>, Vec<u32>) {
    let mut world = World::new();
    let mut workflow = Pipeline::new();
    let ended = boot(session, &mut world, &mut workflow, region(), RUN, reserve());
    let performed = world.dispatched().iter().map(|d| d.id.seq.0).collect();
    (ended.map(|_| ()), performed)
}

/// A reboot of a crash image with a fresh world and workflow.
fn reboot(image: &[u8]) -> (Result<Progress, DriveError<FaultError>>, World, Pipeline) {
    let mut device = restored(image);
    let mut world = World::new();
    let mut workflow = Pipeline::new();
    let ended = boot(
        &mut device,
        &mut world,
        &mut workflow,
        region(),
        RUN,
        reserve(),
    );
    (ended, world, workflow)
}

/// The sequence numbers a world dispatched under `run`, in order.
fn dispatched(world: &World, run: RunId) -> Vec<u32> {
    world
        .dispatched()
        .iter()
        .map(|d| {
            assert_eq!(d.id.run, run, "an effect carried another run's id");
            d.id.seq.0
        })
        .collect()
}

/// One crash point of the reference run, classified.
struct Point {
    injection: Injection,
    image: Vec<u8>,
    /// What the crashed boot's world performed.
    performed: Vec<u32>,
    row: Row,
    /// The effect the row is about.
    effect: u32,
}

/// Which row a crash point of the reference run is an instance of.
///
/// `None` for a crash before any effect's record was begun. Every other answer is
/// cross-checked against the media: the class is read off the operation, and the recovery
/// result is what the table says it must be.
fn classify(run: &Run, performed: &[u32]) -> Option<(Row, u32)> {
    let (record, step, landed) = written(run)?;
    let (history, ending) = history(run.image());
    let clean = matches!(ending, Some(Ending::Clean { .. }));
    let at = format!("{:?}", run.injection());
    assert!(
        REFERENCE.starts_with(&history),
        "history is not a prefix of the reference run, at {at}: {history:?}"
    );
    match record {
        Record::Started => None,
        Record::Schedule(k) => {
            assert!(
                !performed.contains(&k),
                "dispatched during its own schedule, at {at}"
            );
            if history.contains(&Record::Schedule(k)) {
                // Recovered, so the seal landed whole: this is the seal's `Whole` or the
                // commit barrier, and never the frame or the payload barrier.
                assert!(
                    matches!(
                        (step, landed),
                        (Step::Seal, true) | (Step::CommitBarrier, _)
                    ),
                    "a recovered schedule from an unsealed write, at {at}"
                );
                Some((Row::AfterScheduleBarrierBeforeDispatch, k))
            } else {
                Some((Row::DuringScheduleFrameWrite, k))
            }
        }
        Record::Outcome(k) => {
            assert!(
                performed.contains(&k),
                "an outcome written for an effect never performed, at {at}"
            );
            if history.contains(&Record::Outcome(k)) {
                assert!(
                    matches!(
                        (step, landed),
                        (Step::Seal, true) | (Step::CommitBarrier, _)
                    ),
                    "a recovered outcome from an unsealed write, at {at}"
                );
                Some((Row::AfterCompletionBarrier, k))
            } else if step == Step::Frame && !landed {
                assert!(
                    clean,
                    "nothing of the outcome landed and the tail is not erased, at {at}"
                );
                Some((Row::AfterActivityBeforeCompletionBarrier, k))
            } else {
                assert!(
                    !clean,
                    "part of the outcome landed and the tail reads clean, at {at}"
                );
                Some((Row::DuringCompletionWrite, k))
            }
        }
        Record::Terminal => {
            assert_eq!(performed, [0, 1], "at {at}");
            Some((Row::AfterCompletionBarrier, 1))
        }
    }
}

/// The whole sweep of the reference run, every crash point classified.
fn sweep() -> Vec<Point> {
    let harness = Harness::new(geometry());
    let logs: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());
    let Ok(runs) = harness.run(|session| {
        let (ended, performed) = reference(session);
        logs.borrow_mut().push(performed);
        ended
    }) else {
        unreachable!("the fault-free run completes")
    };
    let logs = logs.into_inner();
    assert_eq!(logs.len(), runs.len());
    runs.iter()
        .zip(logs)
        .filter_map(|(run, performed)| {
            let injection = run.injection()?;
            let (row, effect) = classify(run, &performed)?;
            Some(Point {
                injection,
                image: run.image().to_vec(),
                performed,
                row,
                effect,
            })
        })
        .collect()
}

/// The points of one row, required to include both effects and both reset causes.
fn points_of(row: Row) -> Vec<Point> {
    let points: Vec<Point> = sweep().into_iter().filter(|p| p.row == row).collect();
    assert!(!points.is_empty(), "no crash point landed in {}", row.id());
    assert!(
        points.iter().any(|p| p.effect > 0),
        "{} has no instance on the run's second effect, which is the only one that tells \
         redelivery from a fresh mint",
        row.id()
    );
    for cause in [Interruption::PowerLoss, Interruption::Watchdog] {
        assert!(
            points.iter().any(|p| p.injection.interruption == cause),
            "{} was never reached by a {cause:?}",
            row.id()
        );
    }
    points
}

/// A refusal §14 and ADR 0018 permit: the bank cannot be extended.
fn refused_without_dispatch(
    ended: &Result<Progress, DriveError<FaultError>>,
    world: &World,
    at: &str,
) {
    // A torn frame fails its check; a whole frame with no seal is unsealed. Nothing else.
    assert!(
        matches!(
            ended,
            Err(DriveError::Recovery(RecoveryError::Decode(
                DecodeError::IntegrityFailed | DecodeError::Unsealed
            )))
        ),
        "the only legal refusals here, at {at}: {ended:?}"
    );
    assert!(
        world.dispatched().is_empty(),
        "refused after dispatching, at {at}"
    );
}

#[test]
fn the_reference_run_is_six_records_of_four_operations_each() {
    // The map every classification above reads through. A run that grew a record, or a
    // writer that spent five operations on one, would move every row by one.
    let harness = Harness::new(geometry());
    let Ok(runs) = harness.run(|session| reference(session).0) else {
        unreachable!("the fault-free run completes")
    };
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is first")
    };
    assert_eq!(clean.ops().len(), REFERENCE.len() * STEPS.len());
    for (index, op) in clean.ops().iter().enumerate() {
        match STEPS.get(index % STEPS.len()) {
            Some(Step::Frame | Step::Seal) => {
                assert!(matches!(op, Op::Program { .. }), "op {index}");
            }
            Some(Step::PayloadBarrier | Step::CommitBarrier) => {
                assert_eq!(*op, Op::Barrier, "op {index}");
            }
            None => unreachable!(),
        }
    }
    assert_eq!(history(clean.image()).0, REFERENCE);
}

// ---------------------------------------------------------------------------------------
// Rows 1 to 6: the effect protocol
// ---------------------------------------------------------------------------------------

#[test]
fn during_schedule_frame_write_the_frame_is_ignored_and_the_activity_was_not_yet_dispatchable() {
    let mut refused = 0_usize;
    let mut carried_on = 0_usize;
    for point in points_of(Row::DuringScheduleFrameWrite) {
        let at = format!("{:?}", point.injection);
        let k = point.effect;
        // Not dispatchable: the crashed boot never asked the world for it.
        assert!(!point.performed.contains(&k), "at {at}");
        // Frame ignored, previous prefix wins.
        let (history, _) = history(&point.image);
        assert!(!history.contains(&Record::Schedule(k)), "at {at}");
        let (ended, world, workflow) = reboot(&point.image);
        if ended.is_ok() {
            // Nothing of the frame landed, so the reboot schedules it afresh — once.
            let again = dispatched(&world, RUN);
            assert_eq!(again.first(), Some(&k), "at {at}");
            assert_eq!(again.iter().filter(|s| **s == k).count(), 1, "at {at}");
            assert_eq!(workflow.hashed(), HASHED, "at {at}");
            carried_on += 1;
        } else {
            refused_without_dispatch(&ended, &world, &at);
            refused += 1;
        }
    }
    assert!(refused > 0, "no torn schedule frame was refused");
    assert!(
        carried_on > 0,
        "no schedule write was interrupted before it landed"
    );
}

#[test]
fn after_schedule_barrier_before_dispatch_the_stable_effect_id_is_redelivered() {
    for point in points_of(Row::AfterScheduleBarrierBeforeDispatch) {
        let at = format!("{:?}", point.injection);
        let k = point.effect;
        assert!(!point.performed.contains(&k), "at {at}");
        let (history, _) = history(&point.image);
        assert_eq!(
            history.last(),
            Some(&Record::Schedule(k)),
            "schedule without completion, at {at}"
        );
        let (ended, world, workflow) = reboot(&point.image);
        assert!(
            matches!(ended, Ok(Progress::Finished { .. })),
            "at {at}: {ended:?}"
        );
        assert_eq!(
            world.dispatched().first().map(|d| d.id),
            Some(EffectId {
                run: RUN,
                seq: EffectSeq(k)
            }),
            "the first dispatch of the reboot is the stable id, at {at}"
        );
        assert_eq!(workflow.hashed(), HASHED, "at {at}");
    }
}

/// Row 3, driven: the world performs the effect and never answers, then the run reboots.
///
/// The supply going during the activity, after the world changed. From the media it is a
/// schedule with nothing after it; from the world it is one performance with no answer. RAM
/// goes with the reboot; the world persists. Returns the row it credits.
fn row_three() -> Row {
    for k in 0..2_u32 {
        let mut device = Device::new(geometry());
        let mut world = World::interrupted_once_at_seq(k);
        let mut workflow = Pipeline::new();
        let first = boot(
            &mut device,
            &mut world,
            &mut workflow,
            region(),
            RUN,
            reserve(),
        );
        assert_eq!(
            first,
            Ok(Progress::Waiting {
                id: EffectId {
                    run: RUN,
                    seq: EffectSeq(k)
                }
            })
        );
        let (history, ending) = history_of(&mut device, region());
        assert_eq!(history.last(), Some(&Record::Schedule(k)));
        assert!(matches!(ending, Some(Ending::Clean { .. })));

        let mut again = Pipeline::new();
        let second = boot(
            &mut device,
            &mut world,
            &mut again,
            region(),
            RUN,
            reserve(),
        );
        assert!(
            matches!(second, Ok(Progress::Finished { .. })),
            "{second:?}"
        );
        assert_eq!(again.hashed(), HASHED);
        // Performed twice, under one identity: the duplicate attempt the row requires the
        // activity to tolerate, and it did — the run ended with the right answer.
        let id = EffectId {
            run: RUN,
            seq: EffectSeq(k),
        };
        let attempts: Vec<EffectId> = world
            .dispatched()
            .iter()
            .map(|d| d.id)
            .filter(|got| *got == id)
            .collect();
        assert_eq!(attempts, [id, id], "effect {k}");
        assert_eq!(
            world.offered().iter().filter(|d| d.id == id).count(),
            2,
            "effect {k}"
        );
    }
    Row::DuringPhysicalActivity
}

#[test]
fn during_physical_activity_the_effect_is_redelivered_and_the_activity_tolerates_the_duplicate_attempt()
 {
    assert_eq!(row_three(), Row::DuringPhysicalActivity);
}

#[test]
fn after_physical_activity_before_completion_barrier_the_same_id_is_redelivered() {
    for point in points_of(Row::AfterActivityBeforeCompletionBarrier) {
        let at = format!("{:?}", point.injection);
        let k = point.effect;
        assert!(
            point.performed.contains(&k),
            "the world had changed, at {at}"
        );
        let (history, _) = history(&point.image);
        assert_eq!(
            history.last(),
            Some(&Record::Schedule(k)),
            "the schedule is the last durable record, at {at}"
        );
        let (ended, world, workflow) = reboot(&point.image);
        assert!(
            matches!(ended, Ok(Progress::Finished { .. })),
            "at {at}: {ended:?}"
        );
        assert_eq!(
            world.dispatched().first().map(|d| d.id),
            Some(EffectId {
                run: RUN,
                seq: EffectSeq(k)
            }),
            "the second attempt carries the same id, at {at}"
        );
        assert_eq!(workflow.hashed(), HASHED, "at {at}");
    }
}

#[test]
fn during_completion_write_the_torn_completion_is_ignored_and_no_partial_result_bytes_are_exposed()
{
    for point in points_of(Row::DuringCompletionWrite) {
        let at = format!("{:?}", point.injection);
        let k = point.effect;
        let (history, _) = history(&point.image);
        assert_eq!(
            history.last(),
            Some(&Record::Schedule(k)),
            "torn completion ignored, at {at}"
        );
        // Every recovered outcome belongs to an effect before `k`, and is whole.
        let recovered = payloads(&point.image);
        assert_eq!(recovered.len(), usize::try_from(k).unwrap_or(0), "at {at}");
        assert!(
            recovered.iter().all(|p| p == DOWNLOADED || p == HASHED),
            "a partial answer was recovered, at {at}"
        );
        // See the module documentation: no append point, so the bank is refused rather than
        // redelivered into, and nothing of the answer reaches the workflow.
        let (ended, world, workflow) = reboot(&point.image);
        refused_without_dispatch(&ended, &world, &at);
        let observed = if k == 0 {
            workflow.downloaded()
        } else {
            workflow.hashed()
        };
        assert!(
            observed.is_empty(),
            "the workflow observed part of an answer, at {at}"
        );
    }
}

#[test]
fn after_completion_barrier_the_completion_is_replayed_and_the_activity_never_runs_again() {
    for point in points_of(Row::AfterCompletionBarrier) {
        let at = format!("{:?}", point.injection);
        let k = point.effect;
        let (history, _) = history(&point.image);
        assert!(history.contains(&Record::Outcome(k)), "at {at}");
        let (ended, world, workflow) = reboot(&point.image);
        match ended {
            Ok(Progress::Finished { .. }) => {
                assert_eq!(workflow.hashed(), HASHED, "replayed, at {at}");
            }
            // A torn *terminal* record is in this row too: every completion is replayed and
            // the run cannot be extended.
            _ => refused_without_dispatch(&ended, &world, &at),
        }
        assert!(
            dispatched(&world, RUN).iter().all(|s| *s > k),
            "a completed activity ran again, at {at}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Rows 7 and 8: the bank swap
// ---------------------------------------------------------------------------------------

/// Eight erase blocks, four per bank, so both erases in §10's protocol have interior points.
fn swap_geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(2048, 256, 4, 1) else {
        unreachable!("2048 is eight whole 256-byte blocks")
    };
    geometry
}

fn layout_of(geometry: Geometry) -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry) else {
        unreachable!("an even number of erase blocks is two banks")
    };
    layout
}

const fn header_of(run: RunId, layout: BankLayout) -> BankHeader<'static> {
    BankHeader {
        run,
        align: layout.align(),
        workflow_kind: waymaker_drive::demo::WORKFLOW_KIND,
        workflow_version: waymaker_drive::demo::WORKFLOW_VERSION,
        input_schema: 0,
        input: b"seed",
    }
}

/// §10's steps 3 to 6 by hand: a bank set up from nothing.
fn install<S: StableStorage>(
    storage: &mut S,
    layout: BankLayout,
    id: BankId,
    generation: Generation,
    header: &BankHeader<'_>,
) -> Result<JournalRegion, S::Error> {
    let bank = layout.bank(id);
    let mut page = [0_u8; PAGE];
    let Ok(len) = bank::encode_header(header, &mut page) else {
        unreachable!("a bank holds its own header")
    };
    let Some(frame) = page.get(..len) else {
        unreachable!("the encoder wrote inside the buffer")
    };
    storage.program(bank.base(), frame)?;
    storage.barrier()?;
    let Ok(seal) = bank::seal_for(frame, generation) else {
        unreachable!("a header frame can be sealed")
    };
    let mut sealed = [0_u8; 64];
    let Ok(seal_len) = bank::encode_seal(&seal, layout.align(), &mut sealed) else {
        unreachable!("a seal fits its own region")
    };
    let Some(seal_bytes) = sealed.get(..seal_len) else {
        unreachable!("the encoder wrote inside the buffer")
    };
    storage.program(bank.seal_offset(), seal_bytes)?;
    storage.barrier()?;
    let Ok(region) = JournalRegion::of(layout, id, header) else {
        unreachable!("this bank has room for a journal")
    };
    Ok(region)
}

/// What a cold boot would select, read off the media.
fn authority(device: &mut Device, layout: BankLayout) -> Authority {
    let mut generations = [None, None];
    for (slot, id) in generations.iter_mut().zip(BankId::ALL) {
        let bank = layout.bank(id);
        let mut head = [0_u8; PAGE];
        let mut seal = [0_u8; 64];
        let (Ok(seal_len), Some(head_len)) = (
            usize::try_from(bank.seal_bytes()),
            usize::try_from(bank.payload_bytes())
                .ok()
                .map(|b| b.min(PAGE)),
        ) else {
            unreachable!("a host holds a seal and a page")
        };
        let (Some(h), Some(s)) = (head.get_mut(..head_len), seal.get_mut(..seal_len)) else {
            unreachable!("the buffers are large enough")
        };
        let (Ok(()), Ok(())) = (
            device.read(bank.base(), h),
            device.read(bank.seal_offset(), s),
        ) else {
            unreachable!("both banks are inside the device")
        };
        *slot = bank::sealed_generation(h, s);
    }
    bank::select(generations)
}

/// The storage operations §10's seven steps cost, after the previous life.
const SWAP_OPS: usize = 8;

/// The old run installed and waiting at its second effect, then the whole swap.
/// The run two generations back, left sealed in bank B so §10's step 2 erases something.
const STALE_RUN: RunId = RunId(0x0BAD_F00D_0000_0011);

fn swap_writer(session: &mut Session) -> Result<(), String> {
    let layout = layout_of(swap_geometry());
    // Bank B holds a stale sealed run, so a partial erase leaves a real candidate for
    // `select` to reject rather than cells that were erased already.
    install(
        session,
        layout,
        BankId::B,
        Generation(0),
        &header_of(STALE_RUN, layout),
    )
    .map_err(|e| format!("{e:?}"))?;
    let old = header_of(RUN, layout);
    let region =
        install(session, layout, BankId::A, Generation(1), &old).map_err(|e| format!("{e:?}"))?;
    let Some(reserve) = reserve_for(layout) else {
        return Err("the bounds fit this layout".to_owned());
    };
    let mut world = World::pending_at(1);
    let mut workflow = Pipeline::new();
    let waiting = boot(session, &mut world, &mut workflow, region, RUN, reserve)
        .map_err(|e| format!("{e:?}"))?;
    if !matches!(waiting, Progress::Waiting { .. }) {
        return Err(format!("the old run did not wait: {waiting:?}"));
    }
    let next = header_of(NEXT_RUN, layout);
    let booted = Authority::Bank {
        id: BankId::A,
        generation: Generation(1),
    };
    let swap = Swap::beginning(
        layout,
        booted,
        RUN,
        Retired::Recovery(Recovery::new(region)),
        next,
    )
    .map_err(|e| format!("{e:?}"))?;
    let mut page = [0_u8; PAGE];
    let installed = swap
        .prepare(session)
        .and_then(|prepared| prepared.stage(session, &mut page))
        .and_then(|staged| staged.payload_barrier(session))
        .and_then(|sealable| sealable.commit(session))
        .map_err(|e| format!("{e:?}"))?;
    installed.reclaim(session).map_err(|e| format!("{e:?}"))
}

/// Requires `row` to have been reached by a power cut and by a watchdog reset.
fn both_causes<'a>(injections: impl Iterator<Item = &'a Injection>, row: Row) {
    let seen: Vec<Interruption> = injections.map(|i| i.interruption).collect();
    for cause in [Interruption::PowerLoss, Interruption::Watchdog] {
        assert!(
            seen.contains(&cause),
            "{} was never reached by a {cause:?}",
            row.id()
        );
    }
}

/// One crash point of the swap, classified by what the device then boots.
struct SwapPoint {
    injection: Injection,
    device: Device,
    row: Row,
}

fn swap_sweep() -> Vec<SwapPoint> {
    let geometry = swap_geometry();
    let layout = layout_of(geometry);
    let harness = Harness::new(geometry);
    let Ok(runs) = harness.run(swap_writer) else {
        unreachable!("the fault-free swap completes")
    };
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is first")
    };
    let first_swap_op = clean.ops().len() - SWAP_OPS;
    let expected = [true, false, true, false, true, false, true, false];
    for (offset, is_mutation) in expected.into_iter().enumerate() {
        let op = clean.ops().get(first_swap_op + offset);
        assert_eq!(
            matches!(op, Some(Op::Erase { .. } | Op::Program { .. })),
            is_mutation,
            "swap op {offset}: {op:?}"
        );
    }
    assert!(matches!(
        clean.ops().get(first_swap_op),
        Some(Op::Erase { .. })
    ));
    assert!(matches!(clean.ops().last(), Some(Op::Barrier)));

    let mut points = Vec::new();
    for run in &runs {
        let Some(injection) = run.injection() else {
            continue;
        };
        // From the swap's first operation on, and the power cut after the old run's last
        // barrier returned, which the writer meets at that first operation.
        let before_the_swap = injection.op + 1 == first_swap_op
            && injection.progress == Landed::Whole
            && injection.interruption == Interruption::PowerLoss;
        if injection.op < first_swap_op && !before_the_swap {
            continue;
        }
        let Some(mut device) = Device::restored(geometry, run.image().to_vec()) else {
            unreachable!("the image is device-sized")
        };
        let at = format!("{injection:?}");
        let seal_op = first_swap_op + 4;
        let row = match authority(&mut device, layout) {
            Authority::Bank {
                id: BankId::A,
                generation: Generation(1),
            } => {
                assert!(
                    injection.op <= seal_op,
                    "the old bank stayed authoritative past the seal, at {at}"
                );
                Row::DuringInactiveBankEraseOrWrite
            }
            Authority::Bank {
                id: BankId::B,
                generation: Generation(2),
            } => {
                assert!(
                    injection.op >= seal_op,
                    "the new bank was authoritative before its seal, at {at}"
                );
                Row::AfterNewBankSealBarrier
            }
            other => unreachable!("no single authority at {at}: {other:?}"),
        };
        points.push(SwapPoint {
            injection,
            device,
            row,
        });
    }
    points
}

#[test]
fn during_inactive_bank_erase_or_write_the_old_bank_remains_authoritative_and_the_old_run_continues()
 {
    let layout = layout_of(swap_geometry());
    let Some(reserve) = reserve_for(layout) else {
        unreachable!("the bounds fit this layout")
    };
    let Ok(region) = JournalRegion::of(layout, BankId::A, &header_of(RUN, layout)) else {
        unreachable!("bank A holds a journal")
    };
    let points: Vec<SwapPoint> = swap_sweep()
        .into_iter()
        .filter(|p| p.row == Row::DuringInactiveBankEraseOrWrite)
        .collect();
    both_causes(
        points.iter().map(|p| &p.injection),
        Row::DuringInactiveBankEraseOrWrite,
    );
    let mut seen = 0_usize;
    let mut mid_erase = 0_usize;
    for mut point in points {
        let at = format!("{:?}", point.injection);
        seen += 1;
        if matches!(point.injection.progress, Landed::Bytes(_)) {
            mid_erase += 1;
        }
        let mut world = World::new();
        let mut workflow = Pipeline::new();
        let ended = boot(
            &mut point.device,
            &mut world,
            &mut workflow,
            region,
            RUN,
            reserve,
        );
        assert!(
            matches!(ended, Ok(Progress::Finished { .. })),
            "the old run continues, at {at}: {ended:?}"
        );
        // It continues where it stood: its second effect, redelivered under its own id.
        assert_eq!(dispatched(&world, RUN), [1], "at {at}");
        assert_eq!(workflow.hashed(), HASHED, "at {at}");
    }
    assert!(seen > 0, "no crash point left the old bank authoritative");
    assert!(mid_erase > 0, "no crash point landed inside an erase");
}

#[test]
fn after_new_bank_seal_barrier_the_new_bank_is_authoritative_and_the_old_run_is_never_current() {
    let layout = layout_of(swap_geometry());
    let Some(reserve) = reserve_for(layout) else {
        unreachable!("the bounds fit this layout")
    };
    let next = header_of(NEXT_RUN, layout);
    let Ok(region) = JournalRegion::of(layout, BankId::B, &next) else {
        unreachable!("bank B holds a journal")
    };
    let points: Vec<SwapPoint> = swap_sweep()
        .into_iter()
        .filter(|p| p.row == Row::AfterNewBankSealBarrier)
        .collect();
    both_causes(
        points.iter().map(|p| &p.injection),
        Row::AfterNewBankSealBarrier,
    );
    let mut seen = 0_usize;
    let mut old_journal_intact = 0_usize;
    for mut point in points {
        let at = format!("{:?}", point.injection);
        seen += 1;
        // The authoritative header names the new run, not the old one.
        let mut head = [0_u8; PAGE];
        let bank_b = layout.bank(BankId::B);
        let Ok(()) = point.device.read(bank_b.base(), &mut head) else {
            unreachable!("bank B is inside the device")
        };
        let Ok(header) = bank::decode_header(&head) else {
            unreachable!("the authoritative bank decodes, at {at}")
        };
        assert_eq!(header.run, NEXT_RUN, "at {at}");
        // The old run's journal may still be whole on media; it is never current.
        let Ok(old_region) = JournalRegion::of(layout, BankId::A, &header_of(RUN, layout)) else {
            unreachable!("bank A holds a journal")
        };
        if history_of(&mut point.device, old_region).0.len() == 4 {
            old_journal_intact += 1;
        }
        let mut world = World::new();
        let mut workflow = Pipeline::new();
        let ended = boot(
            &mut point.device,
            &mut world,
            &mut workflow,
            region,
            NEXT_RUN,
            reserve,
        );
        assert!(
            matches!(ended, Ok(Progress::Finished { .. })),
            "the new run boots, at {at}: {ended:?}"
        );
        assert_eq!(
            dispatched(&world, NEXT_RUN),
            [0, 1],
            "a fresh run, under its own id, at {at}"
        );
    }
    assert!(seen > 0, "no crash point made the new bank authoritative");
    assert!(
        old_journal_intact > 0,
        "no crash point left the old run whole and unselected"
    );
}

// ---------------------------------------------------------------------------------------
// Row 9: capacity
// ---------------------------------------------------------------------------------------

/// A storage that counts what the device beneath it was asked to change.
struct Counted<'a> {
    device: &'a mut Device,
    mutations: u32,
    barriers: u32,
}

impl StableStorage for Counted<'_> {
    type Error = FaultError;

    fn geometry(&self) -> Geometry {
        self.device.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.device.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        self.mutations += 1;
        self.device.program(offset, src)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.mutations += 1;
        self.device.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.barriers += 1;
        self.device.barrier()
    }
}

/// A part whose bank fills after one effect: the reference run's second schedule is refused.
///
/// Searched for rather than written down, so the number comes from the reserve's own
/// arithmetic over real records.
fn near_capacity() -> (Geometry, BankLayout, Reserve, JournalRegion, Device) {
    for erase in [64_u32, 128, 256, 512] {
        let Ok(geometry) = Geometry::new(erase * 2, erase, 4, 1) else {
            continue;
        };
        let Ok(layout) = BankLayout::new(geometry) else {
            continue;
        };
        let Some(reserve) = reserve_for(layout) else {
            continue;
        };
        let mut device = Device::new(geometry);
        let Ok(region) = install(
            &mut device,
            layout,
            BankId::A,
            Generation(1),
            &header_of(RUN, layout),
        ) else {
            unreachable!("a fault-free device installs")
        };
        let mut world = World::new();
        let mut workflow = Pipeline::new();
        let ended = boot(&mut device, &mut world, &mut workflow, region, RUN, reserve);
        if ended == Err(DriveError::Capacity(Refusal::NearCapacity))
            && dispatched(&world, RUN) == [0]
        {
            return (geometry, layout, reserve, region, device);
        }
    }
    unreachable!("no bank in the search fills after exactly one effect")
}

/// Row 9, driven. Returns the row it credits.
fn row_nine() -> Row {
    let (_, layout, reserve, region, mut device) = near_capacity();
    assert_eq!(
        history_of(&mut device, region).0,
        [Record::Started, Record::Schedule(0), Record::Outcome(0)]
    );

    // A capacity error, and no mutation: the reboot replays, asks for the second schedule,
    // and is refused before the device is asked for anything.
    let before = device.image().to_vec();
    let mut counted = Counted {
        device: &mut device,
        mutations: 0,
        barriers: 0,
    };
    let mut world = World::new();
    let mut workflow = Pipeline::new();
    let ended = boot(
        &mut counted,
        &mut world,
        &mut workflow,
        region,
        RUN,
        reserve,
    );
    assert_eq!(ended, Err(DriveError::Capacity(Refusal::NearCapacity)));
    assert_eq!(
        (counted.mutations, counted.barriers),
        (0, 0),
        "the refusal touched the device"
    );
    assert!(
        world.dispatched().is_empty(),
        "the refused effect was dispatched"
    );
    assert_eq!(device.image(), before.as_slice());

    // Or an explicit continue-as-new: §10's swap from exactly this state installs the next
    // run in the other bank, and that run can start and do work.
    let next = header_of(NEXT_RUN, layout);
    let booted = Authority::Bank {
        id: BankId::A,
        generation: Generation(1),
    };
    let mut page = [0_u8; PAGE];
    let Ok(swap) = Swap::beginning(
        layout,
        booted,
        RUN,
        Retired::Recovery(Recovery::new(region)),
        next,
    ) else {
        unreachable!("a swap can be planned from the near-capacity state")
    };
    let Ok(installed) = swap
        .prepare(&mut device)
        .and_then(|prepared| prepared.stage(&mut device, &mut page))
        .and_then(|staged| staged.payload_barrier(&mut device))
        .and_then(|sealable| sealable.commit(&mut device))
    else {
        unreachable!("the seven steps complete on a fault-free device")
    };
    assert_eq!(authority(&mut device, layout), installed.authority());
    let mut new_world = World::new();
    let mut new_workflow = Pipeline::new();
    let ended = boot(
        &mut device,
        &mut new_world,
        &mut new_workflow,
        installed.region(),
        NEXT_RUN,
        reserve,
    );
    assert_eq!(
        dispatched(&new_world, NEXT_RUN),
        [0],
        "the new run starts and does work"
    );
    assert_eq!(
        history_of(&mut device, installed.region()).0,
        [Record::Started, Record::Schedule(0), Record::Outcome(0)]
    );
    assert!(
        matches!(ended, Err(DriveError::Capacity(Refusal::NearCapacity))),
        "{ended:?}"
    );
    Row::HistoryCapacityReached
}

#[test]
fn history_capacity_reached_is_a_capacity_error_with_no_mutation_or_an_explicit_continue_as_new() {
    assert_eq!(row_nine(), Row::HistoryCapacityReached);
}

// ---------------------------------------------------------------------------------------
// Row 10: divergence
// ---------------------------------------------------------------------------------------

/// The reference workflow with its second effect changed.
struct Divergent(Pipeline);

impl Workflow for Divergent {
    fn identity(&self) -> Identity<'_> {
        self.0.identity()
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        match boundary.call(DOWNLOAD, b"url")? {
            Outcome::Completed(_) => {}
            Outcome::Failed(_) => return Ok(Outcome::Failed(b"download")),
        }
        // Not `HASH`: a different kind at the second boundary.
        let _ = boundary.call(ActivityKind(9), b"other")?;
        Ok(Outcome::Completed(b""))
    }
}

/// Row 10, driven over every crash image whose history reaches the divergent effect.
/// Returns the row it credits.
fn row_ten() -> Row {
    let harness = Harness::new(geometry());
    let Ok(runs) = harness.run(|session| reference(session).0) else {
        unreachable!("the fault-free run completes")
    };
    let mut diverged = 0_usize;
    let mut diverged_with_the_effect_outstanding = 0_usize;
    for run in &runs {
        let (history, _) = history(run.image());
        if !history.contains(&Record::Schedule(1)) {
            continue;
        }
        let at = format!("{:?}", run.injection());
        let mut device = restored(run.image());
        for boot_number in 0..2 {
            let before = device.image().to_vec();
            let mut world = World::new();
            let mut workflow = Divergent(Pipeline::new());
            let ended = boot(
                &mut device,
                &mut world,
                &mut workflow,
                region(),
                RUN,
                reserve(),
            );
            assert_eq!(
                ended,
                Err(DriveError::Kernel(KernelError::NondeterministicWorkflow)),
                "boot {boot_number} at {at}"
            );
            assert!(
                world.dispatched().is_empty(),
                "executed past a divergence, at {at}"
            );
            assert_eq!(
                device.image(),
                before.as_slice(),
                "history was reinterpreted, at {at}"
            );
        }
        diverged += 1;
        if history.last() == Some(&Record::Schedule(1)) {
            diverged_with_the_effect_outstanding += 1;
        }
    }
    assert!(diverged > 0);
    assert!(
        diverged_with_the_effect_outstanding > 0,
        "the sharpest case — a changed request for an outstanding effect — was never met"
    );
    Row::ReplayDivergence
}

#[test]
fn replay_divergence_is_a_deterministic_fault_with_no_further_execution_and_history_untouched() {
    assert_eq!(row_ten(), Row::ReplayDivergence);
}

// ---------------------------------------------------------------------------------------
// The census
// ---------------------------------------------------------------------------------------

#[test]
fn every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned() {
    let mut matrix = Matrix::EMPTY;
    for point in sweep() {
        matrix = matrix.record(point.row);
    }
    for point in swap_sweep() {
        matrix = matrix.record(point.row);
    }
    // The three rows that are driven rather than swept, credited by what each returns after
    // its own assertions and not by fiat.
    for row in [row_three(), row_nine(), row_ten()] {
        matrix = matrix.record(row);
    }
    matrix
        .verdict()
        .expect("every row of §14's table is reached on the model");
    // Pinned exactly, for `waymaker-spec`'s census reason: the numbers move when the run or
    // the geometry does, and the dangerous direction is a sweep that quietly shrank.
    let counts: Vec<(Row, u32)> = Row::ALL
        .into_iter()
        .map(|row| (row, matrix.iterations(row)))
        .collect();
    assert_eq!(
        counts,
        [
            (Row::DuringScheduleFrameWrite, 138),
            (Row::AfterScheduleBarrierBeforeDispatch, 12),
            (Row::DuringPhysicalActivity, 1),
            (Row::AfterActivityBeforeCompletionBarrier, 6),
            (Row::DuringCompletionWrite, 159),
            (Row::AfterCompletionBarrier, 79),
            (Row::DuringInactiveBankEraseOrWrite, 123),
            (Row::AfterNewBankSealBarrier, 25),
            (Row::HistoryCapacityReached, 1),
            (Row::ReplayDivergence, 1),
        ]
    );
}
