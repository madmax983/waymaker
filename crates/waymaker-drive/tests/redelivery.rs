//! Issue [#30](https://github.com/madmax983/waymaker/issues/30): one effect keeps one
//! identity, and a changed input stops the run.
//!
//! Design document §14's fourth guarantee is about the *allocator*, and
//! `waymaker-spec/tests/redelivery.rs` proves it there. This file is the same guarantee one
//! layer up, where a driver can break it: the identity an effect is dispatched under must be
//! the identity its schedule record committed, on the first attempt and on every later one.
//!
//! Two attempts, two causes:
//!
//! * **a reboot** — power went, RAM went, and the run is re-created from its journal.
//!   `tests/drive.rs` and `tests/crash.rs` hold that half.
//! * **an in-boot retry** — an activity was not ready, and the caller drove the run again
//!   with no reset at all. That is this file's, and it is the sharper of the two: the
//!   workflow value, the world and the RAM all survive, so a driver that kept an identity
//!   counter anywhere would still be believed by a reboot test.
//!
//! # Why every case here uses the run's *second* effect
//!
//! `EffectIdAllocator` starts at `EffectSeq(0)`. A run whose outstanding effect is its first
//! cannot tell redelivery from a fresh mint, because the two agree.
//! [`a_fresh_mint_would_not_answer_what_redelivery_answers`] is the tooth that says so.
//!
//! # The digest half
//!
//! §08's fourth row: a workflow that reaches an effect boundary with an input history did
//! not record is a divergence, and the refusal comes *before* anything is dispatched. The
//! sharpest case is the one where history holds a schedule and no outcome — the redelivery
//! row — because there the wrong answer is not a wrong record but a physical effect
//! performed with an input nobody recorded.

use waymaker_core::{
    ActivityKind, EffectId, EffectIdAllocator, EffectSeq, KernelError, Outcome, RecordRef, RunId,
};
use waymaker_drive::demo::{
    BOUNDS, DOWNLOAD, DOWNLOADED, HASH, Pipeline, WORKFLOW_KIND, WORKFLOW_VERSION, World,
};
use waymaker_drive::{
    Activities, Boundary, Conclusion, DriveError, Driver, Identity, Progress, Scratch, Suspended,
    Workflow,
};
use waymaker_fault::{Device, FaultError};
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// The run's second effect. Every case here is about this one.
const SECOND: EffectId = EffectId {
    run: RUN,
    seq: EffectSeq(1),
};

/// The run's first effect, which every case replays rather than redelivers.
const FIRST: EffectId = EffectId {
    run: RUN,
    seq: EffectSeq(0),
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
fn boot<W: Workflow, A: Activities>(
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

/// Every record the journal holds, by shape and sequence.
fn history(device: &mut Device) -> Vec<Shape> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut records = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            break;
        };
        records.push(Shape::of(&record));
    }
    records
}

/// One recovered record, by shape rather than by bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    RunStarted,
    EffectScheduled(u32),
    EffectResolved(u32),
    Terminal,
}

impl Shape {
    const fn of(record: &RecordRef<'_>) -> Self {
        match *record {
            RecordRef::RunStarted { .. } => Self::RunStarted,
            RecordRef::EffectScheduled { seq, .. } => Self::EffectScheduled(seq.0),
            RecordRef::EffectCompleted { seq, .. } | RecordRef::EffectFailed { seq, .. } => {
                Self::EffectResolved(seq.0)
            }
            RecordRef::RunCompleted { .. } | RecordRef::RunFailed { .. } => Self::Terminal,
        }
    }
}

/// A device whose journal holds the run's first effect resolved and its second outstanding.
///
/// The world declines the second effect once, so the schedule record is committed and no
/// outcome follows it. That is §08's redelivery row, on media.
fn a_run_waiting_on_its_second_effect(world: &mut World) -> Device {
    let mut device = Device::new(geometry());
    let mut workflow = Pipeline::new();
    let Ok(progress) = boot(&mut device, world, &mut workflow) else {
        unreachable!("an activity that is not ready is not a failure")
    };
    assert_eq!(progress, Progress::Waiting { id: SECOND });
    assert_eq!(
        history(&mut device),
        [
            Shape::RunStarted,
            Shape::EffectScheduled(0),
            Shape::EffectResolved(0),
            Shape::EffectScheduled(1),
        ]
    );
    device
}

#[test]
fn an_in_boot_retry_redelivers_the_identity_the_first_attempt_carried() {
    // Declines the second effect once and performs it on the next attempt: an activity that
    // was not ready and then was.
    let mut world = World::pending_once_at_seq(1);
    let mut workflow = Pipeline::new();
    let mut device = Device::new(geometry());

    let Ok(waiting) = boot(&mut device, &mut world, &mut workflow) else {
        unreachable!("an activity that is not ready is not a failure")
    };
    assert_eq!(waiting, Progress::Waiting { id: SECOND });

    // No reset. The same world, the same workflow value, the same power cycle — which is
    // what makes this a retry rather than a reboot.
    let Ok(finished) = boot(&mut device, &mut world, &mut workflow) else {
        unreachable!("the retried activity answers, so the run completes")
    };
    let Progress::Finished { conclusion, .. } = finished else {
        panic!("the retry completes the run: {finished:?}");
    };
    assert_eq!(conclusion, Conclusion::Completed);

    // The whole of the claim. The declined attempt and the retry carry one identity, and the
    // effect the first boot resolved is replayed rather than offered a second time.
    let offered: Vec<EffectId> = world.offered().iter().map(|call| call.id).collect();
    assert_eq!(offered, [FIRST, SECOND, SECOND]);

    // And history holds one schedule record per effect. A driver that minted a fresh
    // identity would have written a second `EffectScheduled` here.
    assert_eq!(
        history(&mut device),
        [
            Shape::RunStarted,
            Shape::EffectScheduled(0),
            Shape::EffectResolved(0),
            Shape::EffectScheduled(1),
            Shape::EffectResolved(1),
            Shape::Terminal,
        ]
    );
}

#[test]
fn any_number_of_retries_carries_one_identity() {
    // The other half of "the original identity": an effect retried five times is one effect
    // five times. A per-boot counter or an attempt number in the identity passes the test
    // above and fails this one.
    let mut world = World::pending_at(1);
    let mut workflow = Pipeline::new();
    let mut device = a_run_waiting_on_its_second_effect(&mut world);

    for _ in 0..5 {
        let Ok(progress) = boot(&mut device, &mut world, &mut workflow) else {
            unreachable!("an activity that is not ready is not a failure")
        };
        assert_eq!(progress, Progress::Waiting { id: SECOND });
    }

    let offered: Vec<EffectId> = world.offered().iter().map(|call| call.id).collect();
    assert_eq!(
        offered,
        [FIRST, SECOND, SECOND, SECOND, SECOND, SECOND, SECOND]
    );
    assert_eq!(
        history(&mut device),
        [
            Shape::RunStarted,
            Shape::EffectScheduled(0),
            Shape::EffectResolved(0),
            Shape::EffectScheduled(1),
        ],
        "a retry writes nothing: the intent is already committed"
    );
}

#[test]
fn a_fresh_mint_would_not_answer_what_redelivery_answers() {
    // The tooth for the two tests above. Their assertions are worth something only because
    // the identity a re-minting driver would have produced is a *different* value — which is
    // true of the run's second effect and false of its first.
    let mut allocator = EffectIdAllocator::for_run(RUN);
    let Ok(minted) = allocator.allocate() else {
        unreachable!("the first allocation of a run is not exhaustion")
    };
    assert_eq!(minted, FIRST);
    assert_ne!(
        minted, SECOND,
        "a run whose outstanding effect is its first cannot tell redelivery from a fresh mint"
    );
}

/// The reference workflow's second call, with an input the recorded run never passed.
///
/// The first call is the reference workflow's exactly, so the divergence this reaches is
/// about the *input* of the second effect and about nothing else.
struct Tampered {
    /// What to pass `HASH` instead of what `DOWNLOAD` answered.
    input: &'static [u8],
}

impl Workflow for Tampered {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: b"seed",
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.call(DOWNLOAD, b"url")?;
        boundary.call(HASH, self.input)?;
        Ok(Outcome::Completed(b"done"))
    }
}

/// The two ways §09's recorded digest can disagree, as static inputs `Tampered` can hold.
///
/// The pair is compared whole, so both halves need a case: a length change with a colliding
/// checksum is exactly what a checksum alone waves through.
const OTHER_BYTES: &[u8] = b"Xontents-of-the-thing";
const OTHER_LENGTH: &[u8] = b"contents-of-the-thin";

#[test]
fn the_tampered_inputs_differ_from_the_recorded_one_in_one_way_each() {
    // Without this the two cases below could both be length changes, and the checksum half
    // of §09's digest would be tested by nothing.
    assert_eq!(OTHER_BYTES.len(), DOWNLOADED.len());
    assert_ne!(OTHER_BYTES, DOWNLOADED);
    assert_ne!(OTHER_LENGTH.len(), DOWNLOADED.len());
    assert!(
        DOWNLOADED.starts_with(OTHER_LENGTH),
        "one byte shorter, and nothing else"
    );
}

#[test]
fn a_changed_input_on_a_resolved_effect_stops_the_run_rather_than_replaying_it() {
    for tampered in [OTHER_BYTES, OTHER_LENGTH] {
        let mut world = World::new();
        let mut workflow = Pipeline::new();
        let mut device = Device::new(geometry());
        let Ok(_) = boot(&mut device, &mut world, &mut workflow) else {
            unreachable!("the reference run completes")
        };
        let before = history(&mut device);

        // The control: the workflow that wrote this history replays it and dispatches
        // nothing. Without it a refusal below could be a device that refuses everything.
        let mut replaying = World::new();
        let Ok(_) = boot(&mut device, &mut replaying, &mut Pipeline::new()) else {
            unreachable!("the recorded workflow replays its own history")
        };
        assert!(
            replaying.offered().is_empty(),
            "a replay dispatches nothing"
        );

        let mut world = World::new();
        let error = boot(&mut device, &mut world, &mut Tampered { input: tampered })
            .expect_err("§08's fourth row: a different input is a divergence");
        assert_eq!(
            error,
            DriveError::Kernel(KernelError::NondeterministicWorkflow)
        );
        assert!(
            world.offered().is_empty(),
            "a diverging replay dispatches nothing, for input {tampered:?}"
        );
        assert_eq!(
            history(&mut device),
            before,
            "and writes nothing: history stands where the divergence found it"
        );
    }
}

#[test]
fn a_changed_input_on_an_outstanding_effect_stops_the_run_rather_than_redelivering_it() {
    // The sharpest case in this file. History holds a schedule record and no outcome, so the
    // engine action §08 prescribes is a *dispatch* — and the input the workflow now offers is
    // not the one that schedule record recorded. Issue #30: a mismatch is
    // `NondeterministicWorkflow`, not a silent re-dispatch.
    for tampered in [OTHER_BYTES, OTHER_LENGTH] {
        let mut waiting = World::pending_at(1);
        let mut device = a_run_waiting_on_its_second_effect(&mut waiting);
        let before = history(&mut device);

        let mut world = World::new();
        let error = boot(&mut device, &mut world, &mut Tampered { input: tampered })
            .expect_err("a redelivery with a changed input is a divergence");
        assert_eq!(
            error,
            DriveError::Kernel(KernelError::NondeterministicWorkflow)
        );
        assert!(
            world.offered().is_empty(),
            "the effect is not redelivered under an input nobody recorded, for {tampered:?}"
        );
        assert_eq!(history(&mut device), before, "and nothing is written");
    }
}

#[test]
fn a_changed_activity_kind_on_an_outstanding_effect_stops_the_run_too() {
    // The digest is one of §08's three; the kind is another, and on the redelivery row it
    // has the same consequence — an effect performed against a schedule record that
    // describes something else.
    struct OtherKind;

    impl Workflow for OtherKind {
        fn identity(&self) -> Identity<'_> {
            Identity {
                kind: WORKFLOW_KIND,
                version: WORKFLOW_VERSION,
                input: b"seed",
            }
        }

        fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
            boundary.call(DOWNLOAD, b"url")?;
            boundary.call(ActivityKind(9), DOWNLOADED)?;
            Ok(Outcome::Completed(b"done"))
        }
    }

    let mut waiting = World::pending_at(1);
    let mut device = a_run_waiting_on_its_second_effect(&mut waiting);
    let before = history(&mut device);

    let mut world = World::new();
    let error = boot(&mut device, &mut world, &mut OtherKind)
        .expect_err("a redelivery of another activity is a divergence");
    assert_eq!(
        error,
        DriveError::Kernel(KernelError::NondeterministicWorkflow)
    );
    assert!(world.offered().is_empty());
    assert_eq!(history(&mut device), before);
}
