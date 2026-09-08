#![cfg(not(feature = "without-facade"))]
//! Issue [#36](https://github.com/madmax983/waymaker/issues/36)'s two "done when"s, over
//! real media.
//!
//! The workflow is an `async fn` over `Ctx`, the world is a
//! [`Table`](waymaker_embassy::wiring::Table) of numeric kinds, the driver is this crate's
//! and the media is `waymaker-fault`'s model of NOR. So what is measured is the protocol
//! rather than a fixture that agrees with it.
//!
//! * **the bound.** The run declares four bytes of effect result and eight of terminal
//!   payload, so the context buffer is wider than an answer may be. A dispatcher that
//!   reports more than four writes a failure with no payload, and the workflow sees no part
//!   of it.
//! * **durable intent.** At every crash point the injector lists, every effect the world
//!   performed has a schedule record in the prefix the crash left behind.
//!
//! The façade's own sequencing is `crates/waymaker-embassy/tests/{ctx,wiring}.rs`.

use core::cell::RefCell;
use core::future::Future;
use core::pin::pin;
use core::task::{Context as Task, Poll, Waker};

use waymaker_core::timer::{ClockCapability, ClockKind};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, Outcome, RecordRef, RunId};
use waymaker_drive::{
    Activities, Boundary, Bridge, Clocks, DriveError, Driver, DurableIntent, Identity, Performed,
    Scratch, Suspended, Workflow,
};
use waymaker_embassy::ctx::{Conclusion, Ctx, Failure};
use waymaker_embassy::dispatch::Produced;
use waymaker_embassy::wiring::{Activity, Table, Unhandled};
use waymaker_embassy::{ActivityDispatcher, Journal};
use waymaker_fault::{Device, FaultError, Harness, Session};
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::{Bounds, Reserve};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);
const DOWNLOAD: ActivityKind = ActivityKind(21);
const WORKFLOW_KIND: u16 = 21;
const WORKFLOW_VERSION: u16 = 1;
const URL: &[u8] = b"fw://a";

/// The name this firmware knows `DOWNLOAD` by. It is never recorded.
const DOWNLOAD_NAME: &str = "download-the-image";

/// The terminal payload of a run that reached a branch this file argues is unreachable.
const UNREACHED: &[u8] = &[254];

/// A four-byte result bound under an eight-byte terminal bound.
///
/// The two differ on purpose: the context buffer is the wider of them, so "wider than the
/// buffer" and "wider than the bound" are two different failures and this file drives the
/// second.
const BOUNDS: Bounds = Bounds {
    run_input_bytes: 6,
    effect_result_bytes: 4,
    terminal_bytes: 8,
};

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
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

/// Why a row could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Offline;

/// What the rows were asked, and what they answer with.
#[derive(Debug, Default)]
struct World {
    /// One entry per dispatch, in order.
    dispatched: Vec<EffectId>,
    /// How wide the buffer was on each dispatch.
    widths: Vec<usize>,
    /// What a download answers with.
    answer: Vec<u8>,
    /// What it reports, when that is not the answer's own length.
    reports: Option<usize>,
}

fn download(
    world: &mut World,
    _task: &mut Task<'_>,
    id: EffectId,
    _input: &[u8],
    out: &mut [u8],
) -> Poll<Result<Produced, Offline>> {
    world.dispatched.push(id);
    world.widths.push(out.len());
    let taken = world.answer.len().min(out.len());
    let (Some(from), Some(into)) = (world.answer.get(..taken), out.get_mut(..taken)) else {
        return Poll::Ready(Err(Offline));
    };
    into.copy_from_slice(from);
    Poll::Ready(Ok(Produced::Completed(
        world.reports.unwrap_or(world.answer.len()),
    )))
}

const ACTIVITIES: &[Activity<World, Offline>] = &[Activity::new(DOWNLOAD, DOWNLOAD_NAME, download)];

/// Download once, then end with what the workflow was allowed to see.
///
/// The terminal payload is the whole point: a run that saw part of an over-bound answer
/// would end with a length above zero.
async fn work<D, J>(ctx: &mut Ctx<'_, D, J>) -> Result<(), ()>
where
    D: ActivityDispatcher,
    J: Journal,
{
    let seen: u8 = match ctx.activity::<()>(DOWNLOAD, URL).await {
        Ok(()) => 255,
        Err(Failure::Activity { len }) => u8::try_from(len).unwrap_or(254),
        // `Infallible` has no value, so this arm names one that cannot be built.
        Err(Failure::Decode(never)) => match never {},
    };
    let ended = [seen];
    ctx.complete(&ended).await
}

/// [`work`] as the synchronous driver runs it.
struct Wired {
    table: Table<'static, World, Offline>,
    out: [u8; 8],
}

impl Wired {
    const fn new(world: World) -> Self {
        Self {
            table: Table::over(world, ACTIVITIES),
            out: [0; 8],
        }
    }

    const fn world(&self) -> &World {
        self.table.world()
    }
}

impl Workflow for Wired {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: URL,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        let ended = {
            let mut bridge = Bridge::over(boundary);
            let mut ctx = Ctx::new(&mut bridge, &mut self.table, &mut self.out);
            let polled = {
                let mut future = pin!(work(&mut ctx));
                future.as_mut().poll(&mut Task::from_waker(Waker::noop()))
            };
            match (ctx.conclusion(), polled) {
                (Some(Conclusion::Ended(Outcome::Completed(bytes))), _) => Some(bytes.len()),
                (Some(Conclusion::Ended(Outcome::Failed(_)) | Conclusion::Refused), _)
                | (None, Poll::Pending) => None,
                (None, Poll::Ready(_)) => Some(0),
            }
        };
        let Some(len) = ended else {
            // Unreachable, and observable rather than silent. `work` always ends the run,
            // its one-byte terminal payload always fits the eight-byte buffer, and this
            // file's world always answers — so a poll that ends without a conclusion is a
            // boundary the driver already stopped, and `Context::conclude` reads its own
            // `stop` before it reads this value. `UNREACHED` is a byte no test expects.
            return Ok(Outcome::Failed(UNREACHED));
        };
        Ok(Outcome::Completed(self.out.get(..len).unwrap_or_default()))
    }
}

/// The synchronous world, which an async workflow never reaches.
struct Unused {
    performed: usize,
}

impl Activities for Unused {
    fn perform(
        &mut self,
        _intent: DurableIntent,
        _kind: ActivityKind,
        _input: &[u8],
        _out: &mut [u8],
    ) -> Performed {
        self.performed += 1;
        Performed::Pending
    }
}

impl Clocks for Unused {
    fn capability(&self) -> ClockCapability {
        ClockCapability::BootOnly
    }

    fn now(&mut self, _kind: ClockKind) -> Option<u64> {
        Some(0)
    }
}

/// One boot over `device`.
fn boot(
    device: &mut Device,
    workflow: &mut Wired,
) -> Result<waymaker_drive::Progress, DriveError<FaultError>> {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let mut world = Unused { performed: 0 };
    Driver::new(region(), RUN, reserve()).boot(
        device,
        &mut world,
        workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// Every record the journal holds, as a kind and its bytes.
fn history(device: &mut Device) -> Vec<(u8, Vec<u8>)> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            break;
        };
        out.push(match record {
            RecordRef::RunStarted { input, .. } => (0, input.to_vec()),
            RecordRef::EffectScheduled { seq, .. } => (1, seq.0.to_le_bytes().to_vec()),
            RecordRef::EffectCompleted { result, .. } => (2, result.to_vec()),
            RecordRef::EffectFailed { error, .. } => (3, error.to_vec()),
            RecordRef::TimerScheduled { .. } => (4, Vec::new()),
            RecordRef::TimerFired { .. } => (5, Vec::new()),
            RecordRef::RunCompleted { result } => (6, result.to_vec()),
            RecordRef::RunFailed { error } => (7, error.to_vec()),
        });
    }
    out
}

#[test]
fn the_world_is_handed_the_runs_declared_result_bound_and_not_the_whole_buffer() {
    let mut device = Device::new(geometry());
    let mut workflow = Wired::new(World {
        answer: b"ok".to_vec(),
        ..World::default()
    });

    let _progress = boot(&mut device, &mut workflow);

    assert_eq!(
        workflow.world().widths,
        vec![usize::from(BOUNDS.effect_result_bytes)],
        "the bound the bridge hands out is the run's, and the buffer is wider than it"
    );
}

#[test]
fn an_answer_over_the_bound_records_a_failure_with_no_payload() {
    // Issue #36's first "done when". Four bytes of bound, five reported, eight bytes of
    // buffer: the buffer would have taken it and the run's declared bound would not.
    let mut device = Device::new(geometry());
    let mut workflow = Wired::new(World {
        answer: b"abcd".to_vec(),
        reports: Some(5),
        ..World::default()
    });

    let progress = boot(&mut device, &mut workflow);

    assert!(progress.is_ok(), "the run makes progress: {progress:?}");
    let journal = history(&mut device);
    assert_eq!(
        journal.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
        vec![0, 1, 3, 6],
        "the run, the schedule, a failure, and the end — no completion"
    );
    assert_eq!(
        journal.get(2),
        Some(&(3, Vec::new())),
        "a clean failure with no payload, rather than a truncated result"
    );
}

#[test]
fn no_part_of_an_over_bound_answer_reaches_the_workflow() {
    let mut device = Device::new(geometry());
    let mut workflow = Wired::new(World {
        answer: b"abcd".to_vec(),
        reports: Some(5),
        ..World::default()
    });

    let _progress = boot(&mut device, &mut workflow);

    let journal = history(&mut device);
    assert_eq!(
        journal.last(),
        Some(&(6, vec![0])),
        "the workflow ended having observed zero bytes of the answer"
    );
}

#[test]
fn an_answer_within_the_bound_is_recorded_whole() {
    // The control. Without it the test above would pass against a façade that refused
    // every answer.
    let mut device = Device::new(geometry());
    let mut workflow = Wired::new(World {
        answer: b"abcd".to_vec(),
        ..World::default()
    });

    let _progress = boot(&mut device, &mut workflow);

    let journal = history(&mut device);
    assert_eq!(
        journal.get(2),
        Some(&(2, b"abcd".to_vec())),
        "four bytes is the bound, so four bytes is recorded"
    );
    assert_eq!(journal.last(), Some(&(6, vec![255])));
}

#[test]
fn an_activity_name_never_reaches_media() {
    // Issue #36's third work item. The name is compile-time metadata for a log; §09's
    // `EffectScheduled` carries a number, a length and a digest, and `effect-scheduled-fields`
    // is what keeps it that way. This is the same statement read off the device.
    let mut device = Device::new(geometry());
    let mut workflow = Wired::new(World {
        answer: b"ok".to_vec(),
        ..World::default()
    });

    let _progress = boot(&mut device, &mut workflow);

    assert_eq!(
        workflow.table.name_of(DOWNLOAD),
        Some(DOWNLOAD_NAME),
        "the name is there for a log"
    );
    let image = device.image().to_vec();
    assert!(
        !image
            .windows(DOWNLOAD_NAME.len())
            .any(|window| window == DOWNLOAD_NAME.as_bytes()),
        "no byte of the name is on media"
    );
}

#[test]
fn a_kind_no_row_declares_stops_the_run_without_a_panic() {
    let mut table = Table::over(World::default(), ACTIVITIES);
    let mut out = [0_u8; 8];
    let mut task = Task::from_waker(Waker::noop());

    let answered = table.poll_dispatch(
        &mut task,
        EffectId {
            run: RUN,
            seq: EffectSeq(0),
        },
        ActivityKind(99),
        URL,
        &mut out,
    );

    assert_eq!(
        answered,
        Poll::Ready(Err(Unhandled::NoSuchActivity(ActivityKind(99))))
    );
}

/// One boot over `session`, and the sequences the world was asked to perform.
fn sweep_boot(
    session: &mut Session,
    dispatched: &RefCell<Vec<u32>>,
) -> Result<(), DriveError<FaultError>> {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let mut world = Unused { performed: 0 };
    let mut workflow = Wired::new(World {
        answer: b"ok".to_vec(),
        ..World::default()
    });
    let ended = Driver::new(region(), RUN, reserve()).boot(
        session,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );
    dispatched
        .borrow_mut()
        .extend(workflow.world().dispatched.iter().map(|id| id.seq.0));
    ended.map(|_progress| ())
}

/// Every schedule sequence the image recovers to.
fn scheduled(image: &[u8]) -> Vec<u32> {
    let Some(mut device) = Device::restored(geometry(), image.to_vec()) else {
        unreachable!("the image is device-sized")
    };
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            break;
        };
        if let RecordRef::EffectScheduled { seq, .. } = record {
            out.push(seq.0);
        }
    }
    out
}

#[test]
fn every_effect_the_facade_dispatched_has_a_recoverable_schedule_at_every_crash_point() {
    // Issue #36's second "done when", end to end. The typestate that makes it true is rung
    // 0.3's; what is measured here is that the façade path does not go round it.
    let harness = Harness::new(geometry());
    let logs: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());

    let runs = harness
        .run(|session| {
            logs.borrow_mut().push(Vec::new());
            let mine = RefCell::new(Vec::new());
            let ended = sweep_boot(session, &mine);
            if let Some(last) = logs.borrow_mut().last_mut() {
                last.clone_from(&mine.borrow());
            }
            ended
        })
        .expect("the fault-free run completes");

    let logs = logs.into_inner();
    assert_eq!(logs.len(), runs.len());
    assert!(
        runs.len() > 1,
        "the enumeration found crash points to sweep"
    );

    let mut shortened = 0_usize;
    let whole = scheduled(runs.first().expect("the fault-free run is first").image());
    for (run, dispatched) in runs.iter().zip(&logs) {
        let recovered = scheduled(run.image());
        if recovered.len() < whole.len() {
            shortened += 1;
        }
        for seq in dispatched {
            assert!(
                recovered.contains(seq),
                "effect {seq} was performed with no recoverable schedule record, at {:?}",
                run.injection()
            );
        }
    }
    assert!(
        shortened > 0,
        "a sweep in which no crash ever shortened history measured nothing"
    );
}
