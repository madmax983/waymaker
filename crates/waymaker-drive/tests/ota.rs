#![cfg(not(feature = "without-facade"))]
//! Design document §06's OTA example, over real media.
//!
//! Issue [#35](https://github.com/madmax983/waymaker/issues/35)'s two "done when"s, and
//! issue [#38](https://github.com/madmax983/waymaker/issues/38)'s third — exercised by the
//! crash rig, not only a happy-path run. The workflow is `waymaker_drive::ota::ota_update`
//! — §06's example as an `async fn` — the façade is `waymaker-embassy`'s, the driver is
//! this crate's, and the media is `waymaker-fault`'s model of NOR. So what is measured
//! below is the protocol and not a fixture that agrees with it.
//!
//! The façade's own sequencing is `crates/waymaker-embassy/tests/ctx.rs`.

use core::cell::RefCell;
use core::task::{Context as Task, Poll};

use waymaker_core::timer::{ClockCapability, ClockKind};
use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, EffectId, EffectSeq, Outcome, RecordRef, RunId};
use waymaker_drive::demo::{BOUNDS as DEMO_BOUNDS, Pipeline, World as SyncWorld};
use waymaker_drive::ota::{
    BOUNDS, DOWNLOAD, Downloader, FLASH_IMAGE, HANDLE, Ota, URL, VERIFY_SIGNATURE, WORKFLOW_KIND,
    WORKFLOW_VERSION, poll_ota,
};
use waymaker_drive::{
    Activities, Boundary, Clocks, Conclusion, DriveError, Driver, Identity, Performed, Progress,
    Scratch, Suspended, Workflow,
};
use waymaker_embassy::ActivityDispatcher;
use waymaker_embassy::ctx::Ctx;
use waymaker_embassy::dispatch::Produced;
use waymaker_fault::{Device, FaultError, Harness, Session};
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::{Bounds, Reserve};
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

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 1024, align) else {
        unreachable!("a 1024-byte region at offset 0 fits this geometry")
    };
    region
}

fn reserve(bounds: Bounds) -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(bounds, layout) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

/// Every record the journal holds, as a kind and its bytes.
fn history(device: &mut Device) -> Vec<(u8, Vec<u8>)> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
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
            RecordRef::VersionMarker { version, .. } => (8, version.to_le_bytes().to_vec()),
        });
    }
    out
}

/// The world the async workflow reaches, and a record of what it was asked.
///
/// Every dispatch is kept with its identity, so a test can say that a replayed effect never
/// happened again and that a redelivered one kept the identity its schedule record
/// committed.
struct Fleet {
    /// One entry per dispatch, in order.
    dispatched: Vec<(EffectId, ActivityKind)>,
    /// How many polls of each activity answer [`Poll::Pending`] before the answer.
    stalls: usize,
    stalled: usize,
    /// Which dispatch fails, if any.
    fails_at: Option<usize>,
    /// What a [`DOWNLOAD`] answers with, when it is not the handle.
    download: Option<&'static [u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NoNetwork;

impl Fleet {
    const fn new() -> Self {
        Self {
            dispatched: Vec::new(),
            stalls: 0,
            stalled: 0,
            fails_at: None,
            download: None,
        }
    }

    const fn answering_download(mut self, bytes: &'static [u8]) -> Self {
        self.download = Some(bytes);
        self
    }

    const fn stalling(mut self, polls: usize) -> Self {
        self.stalls = polls;
        self
    }

    const fn failing_at(mut self, index: usize) -> Self {
        self.fails_at = Some(index);
        self
    }

    fn kinds(&self) -> Vec<ActivityKind> {
        self.dispatched.iter().map(|(_, kind)| *kind).collect()
    }
}

impl ActivityDispatcher for Fleet {
    type Error = NoNetwork;

    fn poll_dispatch(
        &mut self,
        _task: &mut Task<'_>,
        id: EffectId,
        kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, NoNetwork>> {
        if self.stalled < self.stalls {
            self.stalled += 1;
            return Poll::Pending;
        }
        if self.fails_at == Some(self.dispatched.len()) {
            self.dispatched.push((id, kind));
            return Poll::Ready(Err(NoNetwork));
        }
        self.dispatched.push((id, kind));
        let answer: &[u8] = if kind == DOWNLOAD {
            self.download.unwrap_or(HANDLE)
        } else {
            b"ok"
        };
        let taken = answer.len().min(out.len());
        let (Some(from), Some(into)) = (answer.get(..taken), out.get_mut(..taken)) else {
            return Poll::Ready(Err(NoNetwork));
        };
        into.copy_from_slice(from);
        Poll::Ready(Ok(Produced::Completed(answer.len())))
    }
}

/// The synchronous world, which an async workflow never reaches.
///
/// `Driver::boot` needs one because a run may still ask for a clock. What the counter is
/// for is the other half: an async workflow that reached `Activities::perform` would be a
/// workflow going round the façade.
struct Unused {
    performed: usize,
}

impl Activities for Unused {
    fn perform(
        &mut self,
        _intent: waymaker_drive::DurableIntent,
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

/// One boot of the OTA run.
fn boot(
    device: &mut Device,
    workflow: &mut Ota<Fleet>,
    world: &mut Unused,
) -> Result<Progress, DriveError<waymaker_fault::FaultError>> {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve(BOUNDS)).boot(
        device,
        world,
        workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

#[test]
fn the_ota_example_runs_to_completion_against_the_facade() {
    // Issue #35's first "done when". Three activities and a completion, through `Ctx`, over
    // a journal this workspace really writes.
    let mut device = Device::new(geometry());
    let mut workflow = Ota::new(Fleet::new());
    let mut world = Unused { performed: 0 };

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: 0,
        })
    );
    assert_eq!(
        workflow.dispatcher().kinds(),
        vec![DOWNLOAD, VERIFY_SIGNATURE, FLASH_IMAGE]
    );
    // Every effect's schedule record precedes its outcome, and the run has a terminal
    // record. Eight records: the run, three effects, and the end.
    let journal = history(&mut device);
    assert_eq!(journal.len(), 8);
    assert_eq!(journal.first(), Some(&(0, URL.to_vec())));
    assert_eq!(journal.last(), Some(&(6, Vec::new())));
}

#[test]
fn the_synchronous_world_is_never_asked_by_an_async_workflow() {
    // The façade reaches the world through its own dispatcher. A count above zero would
    // mean a workflow that went round `Ctx` to `Activities`, which is the one thing the
    // bridge must not let happen.
    let mut device = Device::new(geometry());
    let mut workflow = Ota::new(Fleet::new());
    let mut world = Unused { performed: 0 };

    let _ignored = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(world.performed, 0);
}

#[test]
fn a_reboot_replays_the_committed_effects_and_dispatches_none_of_them_again() {
    // §06's "the future is disposable". The first boot stalls after the download's outcome
    // is committed; the second creates the workflow and the future again and must not
    // repeat what history holds.
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut first = Ota::new(Fleet::new().stalling(1));

    let stalled = boot(&mut device, &mut first, &mut world);

    assert!(matches!(stalled, Ok(Progress::Waiting { .. })));
    assert!(first.dispatcher().dispatched.is_empty());

    // A reset takes the future, the workflow and the RAM. Only the journal survives.
    let mut second = Ota::new(Fleet::new());
    let resumed = boot(&mut device, &mut second, &mut world);

    assert_eq!(
        resumed,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: 0,
        })
    );
    assert_eq!(
        second.dispatcher().kinds(),
        vec![DOWNLOAD, VERIFY_SIGNATURE, FLASH_IMAGE]
    );
}

#[test]
fn a_redelivered_effect_carries_the_identity_its_schedule_record_committed() {
    // §14's fourth guarantee, through the façade. The first boot commits the download's
    // schedule record and stops before its outcome; the second dispatches the same
    // `(RunId, EffectSeq)` rather than a fresh one.
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut first = Ota::new(Fleet::new().stalling(1));
    let _ignored = boot(&mut device, &mut first, &mut world);

    let mut second = Ota::new(Fleet::new());
    let _resumed = boot(&mut device, &mut second, &mut world);

    let first_dispatch = second.dispatcher().dispatched.first().copied();
    assert_eq!(
        first_dispatch,
        Some((
            EffectId {
                run: RUN,
                seq: EffectSeq(0)
            },
            DOWNLOAD
        ))
    );
}

#[test]
fn a_replayed_run_reaches_the_world_zero_times() {
    // The run is over. A second boot must answer from history alone.
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut first = Ota::new(Fleet::new());
    let _ignored = boot(&mut device, &mut first, &mut world);

    let mut second = Ota::new(Fleet::new());
    let replayed = boot(&mut device, &mut second, &mut world);

    assert_eq!(
        replayed,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: 0,
        })
    );
    assert!(second.dispatcher().dispatched.is_empty());
}

#[test]
fn a_dispatcher_that_fails_records_a_failed_effect_and_the_run_ends() {
    // A failed activity has to reach media, or §08 strands the run. The workflow's `?` then
    // ends the run, and the terminal record is a `RunFailed`.
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut workflow = Ota::new(Fleet::new().failing_at(1));

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: 0,
        })
    );
    let journal = history(&mut device);
    // The run, the download's two records, the verify's schedule and its failure, and the
    // terminal record.
    assert_eq!(journal.len(), 6);
    assert_eq!(journal.get(4), Some(&(3, Vec::new())));
    assert_eq!(journal.last(), Some(&(7, Vec::new())));
}

#[test]
fn continue_as_new_is_refused_by_a_driver_that_cannot_name_a_bank() {
    // §10's swap is `waymaker-flash`'s and works on a bank. This driver is pointed at a
    // journal region, so it refuses rather than swapping a bank it cannot name. Issue #36's
    // dispatcher is where the two are joined.
    struct Restarting;

    impl Workflow for Restarting {
        fn identity(&self) -> Identity<'_> {
            Identity {
                kind: WORKFLOW_KIND,
                versions: VersionRange::exact(WORKFLOW_VERSION),
                input: URL,
            }
        }

        fn run(
            &mut self,
            boundary: &mut dyn Boundary,
        ) -> Result<waymaker_core::Outcome<'_>, Suspended> {
            Err(boundary.continue_as_new(b"next"))
        }
    }

    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve(BOUNDS)).boot(
        &mut device,
        &mut world,
        &mut Restarting,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(progress, Err(DriveError::ContinueUnsupported));
}

#[test]
fn the_synchronous_driver_still_runs_a_workflow_that_names_no_facade_type() {
    // Issue #35's second "done when", as far as a test can put it: the reference workflow
    // of issue #28 reaches the same end with the façade in the workspace. The structural
    // half is not a test — it is the `drive-facadeless` pipeline stage, which builds this
    // crate with `without-facade` and so compiles the driver, §06's boundary and §07's
    // typestate with the façade edge deleted. The `ctx-facade` gate rule is the fast,
    // local half of the same claim.
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let mut world = SyncWorld::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve(DEMO_BOUNDS)).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert!(matches!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            ..
        })
    ));
}

/// [`Ota<Downloader>`] driven through [`poll_ota`], which is the concrete path.
///
/// `Ota` and `ota_update` are generic, and a generic body no caller names is type-checked
/// rather than compiled. `poll_ota` names one, so the firmware build monomorphises this
/// workflow's future and the four façade futures — and this drives the same call the
/// firmware build compiles.
struct Concrete(Ota<Downloader>);

impl Workflow for Concrete {
    fn identity(&self) -> Identity<'_> {
        self.0.identity()
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        poll_ota(&mut self.0, boundary)
    }
}

#[test]
fn the_concrete_workflow_runs_to_completion_through_poll_ota() {
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut workflow = Concrete(Ota::new(Downloader));
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve(BOUNDS)).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: 0,
        })
    );
    let journal = history(&mut device);
    assert_eq!(journal.len(), 8);
    // The download's outcome is the handle and not the image, which is the lesson §06's
    // example exists for.
    assert_eq!(journal.get(2), Some(&(2, HANDLE.to_vec())));
}

#[test]
fn an_answer_that_is_not_a_handle_fails_the_run_rather_than_replaying_for_ever() {
    // A decode failure is a workflow fault, not an effect failure: the outcome is committed
    // and replay hands back the same bytes, so the same call fails the same way on every
    // boot. The run must therefore *end*, and it ends failed.
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut workflow = Ota::new(Fleet::new().answering_download(b"no"));

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: 0,
        })
    );
    // The download happened once and nothing after it did.
    assert_eq!(workflow.dispatcher().kinds(), vec![DOWNLOAD]);
    let journal = history(&mut device);
    assert_eq!(
        journal.len(),
        4,
        "the run, the download's two records, the end"
    );
    assert_eq!(journal.last(), Some(&(7, Vec::new())));
}

#[test]
fn a_download_that_fails_ends_the_run_through_the_other_conversion() {
    // `Failure<NotAnImageSlot>`'s activity arm, where the test above takes its decode arm.
    let mut device = Device::new(geometry());
    let mut world = Unused { performed: 0 };
    let mut workflow = Ota::new(Fleet::new().failing_at(0));

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: 0,
        })
    );
    assert_eq!(workflow.dispatcher().kinds(), vec![DOWNLOAD]);
}

/// Design document §04 says the workflow future is user memory and is reported separately.
/// These two hold the halves of that sentence: what the context costs, which is budgeted,
/// and what the generated future costs, which is not.
#[test]
fn the_context_measured_is_the_context_the_workflow_uses() {
    // The ceiling is `assert_context_size!`'s, in the source, at compile time. What that
    // cannot say is that the *constant* names the right type: it constrains `OtaContext` and
    // `xtask` gates whatever `CONTEXT_BYTES` holds. So this reads the size back through the
    // type the workflow is actually driven with, which no substitution survives.
    assert_eq!(
        waymaker_drive::ota::CONTEXT_BYTES,
        size_of::<Ctx<'static, Downloader, waymaker_drive::Bridge<'static>>>(),
    );
}

#[test]
fn the_generated_workflow_future_is_named_and_is_not_the_context() {
    let futures = waymaker_drive::ota::WORKFLOW_FUTURES;
    assert_eq!(futures.len(), 1);
    let (name, bytes) = futures[0];
    assert_eq!(name, "ota_update");
    // §04's point is that the two are separate numbers, not that either is larger: a
    // workflow with one boundary could be narrower than the context it borrows. What is
    // asserted is that this is a state machine and not a scalar the trick picked up by
    // mistake — it holds a `&mut Ctx` across three boundaries, so it is at least a pointer.
    assert!(bytes >= size_of::<usize>(), "the future measured {bytes} B");
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

/// One boot over `session`, and the sequences the world was asked to perform.
fn sweep_boot(
    session: &mut Session,
    dispatched: &RefCell<Vec<u32>>,
) -> Result<(), DriveError<FaultError>> {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let mut world = Unused { performed: 0 };
    let mut workflow = Ota::new(Fleet::new());
    let ended = Driver::new(region(), RUN, reserve(BOUNDS)).boot(
        session,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );
    dispatched.borrow_mut().extend(
        workflow
            .dispatcher()
            .dispatched
            .iter()
            .map(|(id, _)| id.seq.0),
    );
    ended.map(|_progress| ())
}

#[test]
fn every_effect_the_ota_example_dispatched_has_a_recoverable_schedule_at_every_crash_point() {
    // Issue #38's third "done when": exercised by the crash rig, not only a happy-path run.
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
