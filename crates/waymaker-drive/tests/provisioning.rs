#![cfg(not(feature = "without-facade"))]
//! Design document §06's provisioning example, over real media.
//!
//! Issue [#38](https://github.com/madmax983/waymaker/issues/38)'s second example. Where
//! `tests/ota.rs` covers three activities and a clean ending, this covers the boundaries
//! that example does not: a timer, an activity the workflow retries, and a terminal
//! failure carrying a real payload — plus the input-reading gap
//! [ADR 0032](../../../docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md)
//! named: `Driver::begin` checks the recorded run input against `Workflow::identity` on
//! every boot, and here that identity is a field a caller supplies rather than a constant.

use core::cell::RefCell;
use core::task::{Context as Task, Poll};

use waymaker_core::{ActivityKind, EffectId, RecordRef, RunId};
use waymaker_drive::demo::World;
use waymaker_drive::provisioning::{
    BOUNDS, DEVICE_ID, EXHAUSTED, MAX_ATTEMPTS, Provisioning, ProvisioningContext, REGISTER,
    Registrar, TOKEN, TOKEN_BYTES, WINDOW, WORKFLOW_FUTURES, poll_provisioning,
};
use waymaker_drive::{
    Boundary, Conclusion, DriveError, Driver, Identity, Progress, Scratch, Suspended, Workflow,
};
use waymaker_embassy::ActivityDispatcher;
use waymaker_embassy::dispatch::Produced;
use waymaker_fault::{Device, FaultError, Harness, Session};
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
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

fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

/// A world whose persistent clock already reads past [`WINDOW`]'s instant.
fn elapsed_world() -> World {
    let mut world = World::new();
    let TimerInstant(instant) = window_instant();
    world.set_epoch(instant);
    world
}

/// [`WINDOW`]'s instant, pulled out of the spec rather than duplicated as a literal.
struct TimerInstant(u64);

fn window_instant() -> TimerInstant {
    match WINDOW {
        waymaker_core::timer::TimerSpec::AtPersistentTime { instant } => TimerInstant(instant),
        waymaker_core::timer::TimerSpec::AfterBoot { .. } => {
            unreachable!("provisioning's window is a persistent deadline")
        }
    }
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
        });
    }
    out
}

/// Why [`Bureau`] could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unavailable;

/// A registrar a test can make fail, stall, or answer with the wrong bytes.
#[derive(Debug, Default)]
struct Bureau {
    /// One entry per dispatch, in order.
    dispatched: Vec<(EffectId, ActivityKind)>,
    /// How many leading attempts fail.
    fails_first: usize,
    /// How many polls answer [`Poll::Pending`] before the first real answer.
    stalls: usize,
    stalled: usize,
    /// What a successful attempt answers with, once `fails_first` is behind it.
    answer: Vec<u8>,
}

impl Bureau {
    fn new() -> Self {
        Self {
            answer: TOKEN.to_vec(),
            ..Self::default()
        }
    }

    const fn failing_first(mut self, attempts: usize) -> Self {
        self.fails_first = attempts;
        self
    }

    const fn stalling(mut self, polls: usize) -> Self {
        self.stalls = polls;
        self
    }

    fn answering(mut self, bytes: &[u8]) -> Self {
        self.answer = bytes.to_vec();
        self
    }
}

impl ActivityDispatcher for Bureau {
    type Error = Unavailable;

    fn poll_dispatch(
        &mut self,
        _task: &mut Task<'_>,
        id: EffectId,
        kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, Unavailable>> {
        if self.stalled < self.stalls {
            self.stalled += 1;
            return Poll::Pending;
        }
        self.dispatched.push((id, kind));
        if self.dispatched.len() <= self.fails_first {
            return Poll::Ready(Err(Unavailable));
        }
        let taken = self.answer.len().min(out.len());
        let (Some(from), Some(into)) = (self.answer.get(..taken), out.get_mut(..taken)) else {
            return Poll::Ready(Err(Unavailable));
        };
        into.copy_from_slice(from);
        Poll::Ready(Ok(Produced::Completed(self.answer.len())))
    }
}

/// One boot of a provisioning run.
fn boot(
    device: &mut Device,
    workflow: &mut Provisioning<Bureau>,
    world: &mut World,
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

#[test]
fn the_provisioning_example_runs_to_completion_against_the_facade() {
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let mut workflow = Provisioning::new(Bureau::new(), DEVICE_ID);

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: TOKEN_BYTES,
        })
    );
    assert_eq!(workflow.dispatcher().dispatched.len(), 1);
    assert_eq!(workflow.dispatcher().dispatched[0].1, REGISTER);
    let journal = history(&mut device);
    // The run, the timer's schedule and firing, the registration's two records, and the end.
    assert_eq!(journal.len(), 6);
    assert_eq!(journal.first(), Some(&(0, DEVICE_ID.to_vec())));
    assert_eq!(journal.last(), Some(&(6, TOKEN.to_vec())));
}

#[test]
fn a_run_whose_window_has_not_opened_suspends_rather_than_registering() {
    let TimerInstant(instant) = window_instant();
    let mut device = Device::new(geometry());
    let mut world = World::new();
    world.set_epoch(instant - 1);
    let mut workflow = Provisioning::new(Bureau::new(), DEVICE_ID);

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert!(matches!(progress, Ok(Progress::WaitingUntil { .. })));
    assert!(
        workflow.dispatcher().dispatched.is_empty(),
        "the window has not opened, so the workflow never reaches the activity"
    );
}

#[test]
fn a_reboot_after_the_window_opens_carries_the_run_on() {
    let TimerInstant(instant) = window_instant();
    let mut device = Device::new(geometry());
    let mut before = World::new();
    before.set_epoch(instant - 1);
    let mut first = Provisioning::new(Bureau::new(), DEVICE_ID);
    let waiting = boot(&mut device, &mut first, &mut before);
    assert!(matches!(waiting, Ok(Progress::WaitingUntil { .. })));

    // A fresh boot, and a persistent clock that has since moved past the instant — the way
    // a real board's backed clock would read on the next reset.
    let mut after = elapsed_world();
    let mut second = Provisioning::new(Bureau::new(), DEVICE_ID);
    let progress = boot(&mut device, &mut second, &mut after);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: TOKEN_BYTES,
        })
    );
}

#[test]
fn a_failed_attempt_is_retried_under_a_fresh_identity_and_the_run_completes() {
    // Retry is workflow code, not the engine's: §16 leaves `retry-policy-placement` open,
    // and each attempt here is a new effect rather than a redelivery of one the kernel
    // already committed.
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let mut workflow = Provisioning::new(Bureau::new().failing_first(2), DEVICE_ID);

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: TOKEN_BYTES,
        })
    );
    let dispatched = &workflow.dispatcher().dispatched;
    assert_eq!(
        dispatched.len(),
        3,
        "two failed attempts, then the one that succeeded"
    );
    let seqs: Vec<u32> = dispatched.iter().map(|(id, _)| id.seq.0).collect();
    assert_eq!(seqs[1], seqs[0] + 1, "each retry carries its own identity");
    assert_eq!(seqs[2], seqs[0] + 2);
}

#[test]
fn attempts_exhausted_ends_the_run_with_the_real_failure_payload() {
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let attempts = usize::try_from(MAX_ATTEMPTS).unwrap_or(usize::MAX);
    let mut workflow = Provisioning::new(Bureau::new().failing_first(attempts), DEVICE_ID);

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: EXHAUSTED.len(),
        })
    );
    assert_eq!(workflow.dispatcher().dispatched.len(), attempts);
    let journal = history(&mut device);
    assert_eq!(
        journal.last(),
        Some(&(7, EXHAUSTED.to_vec())),
        "the terminal failure record carries the real reason, not an empty payload"
    );
}

#[test]
fn an_answer_that_is_not_a_token_fails_the_run_rather_than_retrying_it() {
    // A decode failure replays identically on every boot, so it ends the run instead of
    // spending an attempt on it.
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let mut workflow = Provisioning::new(Bureau::new().answering(b"ab"), DEVICE_ID);

    let progress = boot(&mut device, &mut workflow, &mut world);

    assert_eq!(
        progress,
        Ok(Progress::Finished {
            conclusion: Conclusion::Failed,
            result_len: 0,
        })
    );
    assert_eq!(workflow.dispatcher().dispatched.len(), 1);
}

#[test]
fn a_reboot_replays_the_committed_registration_and_dispatches_nothing_again() {
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let mut first = Provisioning::new(Bureau::new().stalling(1), DEVICE_ID);
    let stalled = boot(&mut device, &mut first, &mut world);
    assert!(matches!(stalled, Ok(Progress::Waiting { .. })));
    assert!(first.dispatcher().dispatched.is_empty());

    let mut second = Provisioning::new(Bureau::new(), DEVICE_ID);
    let resumed = boot(&mut device, &mut second, &mut world);

    assert_eq!(
        resumed,
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            result_len: TOKEN_BYTES,
        })
    );
    assert_eq!(second.dispatcher().dispatched.len(), 1);
}

#[test]
fn a_reboot_with_a_different_input_is_refused_rather_than_silently_replayed() {
    // ADR 0032 named this gap: an example whose input is a module constant has nothing
    // tying `Workflow::identity` to the bytes it registers, so nothing here could ever
    // catch a caller supplying different ones across boots. `Provisioning` carries its
    // input as a field, and `Driver::begin` checks it on every boot.
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let mut first = Provisioning::new(Bureau::new().stalling(1), DEVICE_ID);
    let stalled = boot(&mut device, &mut first, &mut world);
    assert!(matches!(stalled, Ok(Progress::Waiting { .. })));

    let mut other = DEVICE_ID;
    let Some(first_byte) = other.first_mut() else {
        unreachable!("DEVICE_ID_BYTES is not zero")
    };
    *first_byte = b'X';
    let mut second = Provisioning::new(Bureau::new(), other);

    let result = boot(&mut device, &mut second, &mut world);

    assert_eq!(result, Err(DriveError::NotThisWorkflow));
}

/// [`Provisioning<Registrar>`] driven through [`poll_provisioning`], the concrete path the
/// firmware build monomorphises.
struct Concrete(Provisioning<Registrar>);

impl Workflow for Concrete {
    fn identity(&self) -> Identity<'_> {
        self.0.identity()
    }

    fn run(
        &mut self,
        boundary: &mut dyn Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, Suspended> {
        poll_provisioning(&mut self.0, boundary)
    }
}

#[test]
fn the_concrete_workflow_runs_to_completion_through_poll_provisioning() {
    let mut device = Device::new(geometry());
    let mut world = elapsed_world();
    let mut workflow = Concrete(Provisioning::new(Registrar, DEVICE_ID));
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve()).boot(
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
            result_len: TOKEN_BYTES,
        })
    );
}

#[test]
fn the_context_is_the_same_size_for_every_concrete_ctx() {
    // `Ctx` borrows everything it uses, so its size does not depend on `D` or `J`. This is
    // why `provisioning` shares `ota`'s gate rather than needing a second one: the claim is
    // checked here rather than assumed.
    assert_eq!(
        size_of::<ProvisioningContext<'static>>(),
        waymaker_drive::ota::CONTEXT_BYTES,
    );
}

#[test]
fn the_generated_workflow_future_is_named_and_reported_beside_otas() {
    assert_eq!(WORKFLOW_FUTURES.len(), 1);
    let (name, bytes) = WORKFLOW_FUTURES[0];
    assert_eq!(name, "provision");
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
    let mut world = elapsed_world();
    let mut workflow = Provisioning::new(Bureau::new(), DEVICE_ID);
    let ended = Driver::new(region(), RUN, reserve()).boot(
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
fn every_registration_the_example_dispatched_has_a_recoverable_schedule_at_every_crash_point() {
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
