//! Workflows and worlds that are wrong in one way each, and the refusal each must meet.
//!
//! A driver's bugs are invisible in the way an instrument's are: a run that quietly
//! dispatched an effect twice, or replayed somebody else's history, ends in a passing test.
//! Every case here is a way of being wrong that the driver has to notice, and the assertion
//! is always two-part — the refusal, and that nothing reached the world or the media after
//! it.

use waymaker_core::timer::{ClockCapability, ClockKind};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, KernelError, Outcome, RunId};
use waymaker_drive::demo::{
    BOUNDS, DOWNLOAD, DOWNLOADED, HASH, Pipeline, WORKFLOW_KIND, WORKFLOW_VERSION, World,
};
use waymaker_drive::{
    Activities, Boundary, Clocks, Conclusion, DriveError, Driver, DurableIntent, Identity,
    Performed, Progress, Scratch, Suspended, Workflow,
};
use waymaker_fault::{Device, FaultError};
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::{CapacityError, Reserve};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

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

/// One boot, with a fresh page and result buffer.
fn boot<W: Workflow, A: Activities + Clocks>(
    device: &mut Device,
    world: &mut A,
    workflow: &mut W,
) -> Result<Progress, DriveError<FaultError>> {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        device,
        world,
        workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// How many records the journal holds.
fn records(device: &mut Device) -> usize {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut count = 0;
    while let Some(step) = recovery.next(device, &mut page) {
        if step.is_err() {
            break;
        }
        count += 1;
    }
    count
}

/// The reference workflow, run to a first committed effect and then left waiting.
fn a_run_with_one_effect_committed() -> Device {
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let mut world = World::pending_at(1);
    let Ok(progress) = boot(&mut device, &mut world, &mut workflow) else {
        unreachable!("a pending activity is not a failure")
    };
    assert!(matches!(progress, Progress::Waiting { .. }), "{progress:?}");
    device
}

/// The reference workflow with its two calls the other way round.
struct Reordered;

impl Workflow for Reordered {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.call(HASH, b"url")?;
        Ok(Outcome::Completed(b"done"))
    }
}

#[test]
fn a_workflow_that_asks_for_a_different_effect_is_refused_and_dispatches_nothing() {
    let mut device = a_run_with_one_effect_committed();
    let before = records(&mut device);

    let mut world = World::new();
    let error = boot(&mut device, &mut world, &mut Reordered)
        .expect_err("a different activity kind is design document §08's divergence");
    assert_eq!(
        error,
        DriveError::Kernel(KernelError::NondeterministicWorkflow)
    );
    assert!(
        world.dispatched().is_empty(),
        "a diverging replay dispatches nothing"
    );
    assert_eq!(
        records(&mut device),
        before,
        "and writes nothing: history stands where the divergence found it"
    );
}

/// A workflow whose recorded `RunStarted` describes a different input.
struct Reinput;

impl Workflow for Reinput {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"else",
        }
    }

    fn run(&mut self, _boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Ok(Outcome::Completed(b"done"))
    }
}

#[test]
fn a_run_recorded_for_another_workflow_is_refused_before_it_is_replayed() {
    let mut device = a_run_with_one_effect_committed();
    let before = records(&mut device);

    let mut world = World::new();
    let error = boot(&mut device, &mut world, &mut Reinput)
        .expect_err("the recorded run is a different one");
    assert_eq!(error, DriveError::NotThisWorkflow);
    assert!(world.dispatched().is_empty());
    assert_eq!(records(&mut device), before);
}

/// A workflow that ignores the driver's instruction to stop.
struct Deaf;

impl Workflow for Deaf {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        // Both refusals dropped on purpose: this is the workflow the driver has to survive.
        let _ = boundary.call(DOWNLOAD, b"url");
        let _ = boundary.call(HASH, DOWNLOADED);
        let _ = boundary.call(HASH, DOWNLOADED);
        Ok(Outcome::Completed(b"done"))
    }
}

#[test]
fn a_workflow_that_swallows_a_suspension_stops_anyway_and_dispatches_nothing_more() {
    let mut device = Device::new(geometry());
    let mut world = World::pending_at(1);

    let progress = boot(&mut device, &mut world, &mut Deaf).expect("the run waits");
    let Progress::Waiting { id } = progress else {
        panic!("the driver's own stop outranks the workflow's return: {progress:?}");
    };
    assert_eq!(
        id,
        EffectId {
            run: RUN,
            seq: EffectSeq(1)
        }
    );
    assert_eq!(
        world.dispatched().len(),
        1,
        "the boundaries after the stop reached neither the world nor the media"
    );
    assert_eq!(
        records(&mut device),
        4,
        "run started, effect 0 scheduled and completed, effect 1 scheduled"
    );
}

/// A workflow that stops asking before history has run out.
struct Impatient;

impl Workflow for Impatient {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.call(DOWNLOAD, b"url")?;
        Ok(Outcome::Completed(b"done"))
    }
}

#[test]
fn a_workflow_that_ends_while_history_continues_is_refused() {
    let mut device = a_run_with_one_effect_committed();
    let before = records(&mut device);

    let mut world = World::new();
    let error = boot(&mut device, &mut world, &mut Impatient)
        .expect_err("history holds a schedule the workflow never asked for");
    assert_eq!(error, DriveError::HistoryContinues);
    assert_eq!(
        records(&mut device),
        before,
        "no terminal record is written over history that is still going"
    );
}

/// A world that claims to have written more than the buffer holds.
///
/// A broken activity: an answer that does not fit is [`Performed::Exhausted`], and this one
/// says `Completed` with a length it cannot have written. On media the two are the same
/// statement — the answer does not fit — so the driver records the exhaustion rather than
/// refusing. Refusing would strand the run: the schedule record is committed, §08 has no
/// edge from an unresolved effect to a terminal record, and every later boot meets the same
/// answer.
struct Greedy;

impl Clocks for Greedy {
    fn capability(&self) -> ClockCapability {
        ClockCapability::BootOnly
    }

    fn now(&mut self, _kind: ClockKind) -> Option<u64> {
        // The workflow this tooth drives waits for nothing, so nothing reads this. A value
        // is answered rather than `None` so that a future test which does wait meets a
        // clock rather than a refusal it did not ask about.
        Some(0)
    }
}

impl Activities for Greedy {
    fn perform(
        &mut self,
        _intent: DurableIntent,
        _kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Performed {
        Performed::Completed(out.len().saturating_add(1))
    }
}

#[test]
fn a_result_longer_than_the_bound_is_recorded_as_exhausted_rather_than_stranding_the_run() {
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let progress = boot(&mut device, &mut Greedy, &mut workflow)
        .expect("an answer that does not fit is a recorded failure, not a stuck run");
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: b"download".len()
        }
    );

    // And the run is over, so a second boot replays it rather than performing the effect
    // again. That is the whole difference: a refusal here repeats for ever.
    let mut again = Pipeline::new();
    let progress =
        boot(&mut device, &mut Greedy, &mut again).expect("the terminal record is replayed");
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: b"download".len()
        }
    );
}

/// A workflow that passes an activity input longer than a schedule record can describe.
struct Verbose {
    input: Vec<u8>,
}

impl Workflow for Verbose {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.call(DOWNLOAD, &self.input)?;
        Ok(Outcome::Completed(b"done"))
    }
}

#[test]
fn an_activity_input_a_schedule_record_cannot_describe_is_refused() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    let mut workflow = Verbose {
        input: vec![0; usize::from(u16::MAX) + 1],
    };
    let error = boot(&mut device, &mut world, &mut workflow)
        .expect_err("a schedule record records a u16 length");
    assert_eq!(error, DriveError::InputTooLong { bytes: 65536 });
    assert!(world.dispatched().is_empty());
}

#[test]
fn a_failed_activity_is_recorded_as_a_failure_and_replayed_as_one() {
    let mut device = Device::new(geometry());
    let mut world = World::failing_at(0);
    let mut workflow = Pipeline::new();
    let progress =
        boot(&mut device, &mut world, &mut workflow).expect("a failed effect is history");
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: b"download".len()
        }
    );

    // And the recorded failure is what a cold start replays.
    let mut replayed = Pipeline::new();
    let mut quiet = World::new();
    let progress = boot(&mut device, &mut quiet, &mut replayed).expect("the run replays");
    assert_eq!(
        progress,
        Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: b"download".len()
        }
    );
    assert!(quiet.dispatched().is_empty());
}

/// A journal too small for the run's records, but large enough for its opening ones.
///
/// The device is the one the reserve was priced on; only the region is narrowed, which is
/// the case §10's gate exists for.
fn a_cramped_region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 64, align) else {
        unreachable!("a 64-byte region at offset 0 fits this geometry")
    };
    region
}

#[test]
fn a_journal_that_cannot_hold_the_reserve_is_refused_before_the_run_starts() {
    let mut device = Device::new(geometry());
    let mut world = World::new();
    let mut workflow = Pipeline::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let Err(error) = Driver::new(a_cramped_region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("a journal this small can never hold the run's exits")
    };

    // §10's gate is taken when the writer is opened, so the refusal comes before the run's
    // own record. Without it the driver commits `RunStarted` and a schedule record, tells
    // the world to perform the effect, and only then finds the outcome record does not fit
    // — which strands the run for ever, because §08 has no edge from an unresolved effect
    // to a terminal record, and re-performs the effect on every boot after it.
    assert_eq!(
        error,
        DriveError::Reserve(CapacityError::RegionTooSmall),
        "{error:?}"
    );
    assert!(
        world.dispatched().is_empty(),
        "nothing is dispatched into a journal that cannot record the outcome"
    );
    assert_eq!(
        records(&mut device),
        0,
        "and nothing is written into it either"
    );

    // The same device, the same reserve, and a region that can hold the run: it completes.
    let mut roomy = World::new();
    let mut again = Pipeline::new();
    let Ok(progress) = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut roomy,
        &mut again,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    ) else {
        unreachable!("the run fits the region the reserve was accepted against")
    };
    assert!(
        matches!(progress, Progress::Finished { .. }),
        "{progress:?}"
    );
}
