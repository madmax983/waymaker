//! The driver at every crash point, and the two §14 guarantees a driver can break.
//!
//! `waymaker-fault` enumerates every point at which the write sequence can be interrupted —
//! every byte of every program, every block of every erase, and before and after every
//! barrier. The writer under test here is the whole protocol: the recovery scan, the kernel
//! boundary and the two-barrier writer, driven by the reference workflow.
//!
//! Two properties are checked at every one of those points, and both are statements about
//! the *driver* rather than about the codec:
//!
//! * **durable intent** (§14, and §02 decision 3). Every effect the world was asked to
//!   perform has a schedule record in the committed prefix the crash left behind. An effect
//!   whose intent did not survive is an effect the next boot cannot account for.
//! * **prefix safety** (§14). The history the crash image recovers to is a prefix of the
//!   history the fault-free run wrote.
//!
//! # The tooth
//!
//! [`a_driver_that_dispatches_before_it_commits_loses_the_intent`] is the mutant, and it is
//! why the sweep above is worth running: a writer that dispatches first and records the
//! intent afterwards produces exactly the state §02 decision 3 forbids, at a crash point the
//! injector finds. It has to be written by hand against
//! [`waymaker_flash::append::Journal`], because [`Driver`] has no way to express it — which
//! is the guarantee, demonstrated from the outside.

use core::cell::RefCell;

use waymaker_core::{EffectSeq, RecordRef, RunId};
use waymaker_drive::demo::{
    DOWNLOAD, DOWNLOADED, HASHED, Pipeline, WORKFLOW_KIND, WORKFLOW_VERSION, World,
};
use waymaker_drive::{DriveError, Driver};
use waymaker_fault::{Device, FaultError, Harness, Session};
use waymaker_flash::append::Journal;
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// One erase block, which is the whole journal region. Small on purpose: the enumeration is
/// a function of the write sequence, and a bigger region only makes the same sweep slower.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(512, 512, 4, 1) else {
        unreachable!("512 is one whole 512-byte block of 4-byte units")
    };
    geometry
}

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 512, align) else {
        unreachable!("the region is the whole device")
    };
    region
}

/// Every record the image recovers to, as a summary a prefix comparison can use.
fn recovered(image: &[u8]) -> Vec<Summary> {
    let Some(mut device) = Device::restored(geometry(), image.to_vec()) else {
        unreachable!("the image is device-sized")
    };
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut history = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            // §14: the frame is ignored and the previous history prefix wins.
            break;
        };
        history.push(Summary::of(&record));
    }
    history
}

/// One recovered record, compared by shape rather than by bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Summary {
    RunStarted,
    EffectScheduled(u32),
    EffectResolved(u32),
    Terminal,
}

impl Summary {
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

    /// The sequence this record schedules, if it schedules one.
    const fn scheduled(self) -> Option<u32> {
        match self {
            Self::EffectScheduled(seq) => Some(seq),
            Self::RunStarted | Self::EffectResolved(_) | Self::Terminal => None,
        }
    }
}

/// One boot of the reference workflow over `session`, and the sequences it dispatched.
fn drive(
    session: &mut Session,
    dispatched: &RefCell<Vec<u32>>,
) -> Result<(), DriveError<FaultError>> {
    let mut workflow = Pipeline::new();
    let mut world = World::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let ended =
        Driver::new(region(), RUN).boot(session, &mut world, &mut workflow, &mut page, &mut result);
    dispatched
        .borrow_mut()
        .extend(world.dispatched().iter().map(|call| call.id.seq.0));
    ended.map(|_| ())
}

#[test]
fn every_dispatched_effect_has_a_recoverable_schedule_at_every_crash_point() {
    let harness = Harness::new(geometry());
    let logs: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());

    let runs = harness
        .run(|session| {
            logs.borrow_mut().push(Vec::new());
            let mine = RefCell::new(Vec::new());
            let ended = drive(session, &mine);
            if let Some(last) = logs.borrow_mut().last_mut() {
                last.clone_from(&mine.borrow());
            }
            ended
        })
        .expect("the fault-free run completes");

    let logs = logs.into_inner();
    assert_eq!(
        logs.len(),
        runs.len(),
        "one dispatch log per run, in the order the harness ran them"
    );
    assert!(
        runs.len() > 1,
        "the enumeration found crash points to sweep"
    );

    let whole = recovered(runs.first().expect("the fault-free run is first").image());
    assert_eq!(
        whole,
        [
            Summary::RunStarted,
            Summary::EffectScheduled(0),
            Summary::EffectResolved(0),
            Summary::EffectScheduled(1),
            Summary::EffectResolved(1),
            Summary::Terminal,
        ]
    );

    let mut truncated = 0_usize;
    for (run, dispatched) in runs.iter().zip(&logs) {
        let history = recovered(run.image());

        // Prefix safety: a crash never invents history, and never reorders it.
        assert!(
            whole.starts_with(&history),
            "{:?} is not a prefix of the fault-free history: {history:?}",
            run.injection()
        );
        if history.len() < whole.len() {
            truncated += 1;
        }

        // Durable intent: nothing the world performed is missing its committed schedule.
        for seq in dispatched {
            assert!(
                history
                    .iter()
                    .filter_map(|record| record.scheduled())
                    .any(|s| s == *seq),
                "effect {seq} was dispatched with no recoverable schedule record, at {:?}",
                run.injection()
            );
        }
    }
    assert!(
        truncated > 0,
        "a sweep in which no crash ever shortened history is a sweep that measured nothing"
    );
}

#[test]
fn a_reboot_after_a_crash_either_carries_the_run_on_or_refuses_before_dispatching() {
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| {
            let mine = RefCell::new(Vec::new());
            drive(session, &mine)
        })
        .expect("the fault-free run completes");

    let mut resumed = 0_usize;
    let mut unextendable = 0_usize;
    for run in &runs {
        let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
            unreachable!("the image is device-sized")
        };
        let mut workflow = Pipeline::new();
        let mut world = World::new();
        let mut page = [0_u8; 256];
        let mut result = [0_u8; 64];
        let ended = Driver::new(region(), RUN).boot(
            &mut device,
            &mut world,
            &mut workflow,
            &mut page,
            &mut result,
        );

        match ended {
            Ok(_) => {
                resumed += 1;
                assert_eq!(
                    workflow.hashed(),
                    HASHED,
                    "the resumed run reaches the same answer, at {:?}",
                    run.injection()
                );
                // §14's redelivery contract: a redelivered effect wears the sequence its
                // schedule record already committed, so nothing after the crash uses an
                // identity the run had not already spent.
                for call in world.dispatched() {
                    assert!(
                        call.id.seq.0 <= 1,
                        "a resumed run minted {:?} beyond the run's two effects, at {:?}",
                        call.id.seq,
                        run.injection()
                    );
                }
            }
            Err(error) => {
                // §14 and ADR 0018: a scan that stopped at damage or at an unsealed frame
                // has no append point, and a bank that cannot be extended is `swap`'s to
                // recycle rather than this driver's to repair.
                assert!(
                    matches!(error, DriveError::Recovery(_) | DriveError::NoAppendPoint),
                    "the only legal refusals here, at {:?}: {error:?}",
                    run.injection()
                );
                // And the refusal came before the world heard anything. That is structural
                // rather than lucky: an effect is dispatched only once the scan has become
                // a writer, and a writer never refuses for either of these two reasons.
                assert!(
                    world.dispatched().is_empty(),
                    "a bank that cannot record an intent dispatches none, at {:?}",
                    run.injection()
                );
                unextendable += 1;
            }
        }
    }
    assert!(resumed > 0, "some crash images carry the run on");
    assert!(
        unextendable > 0,
        "and some do not, which is the case a bank swap exists for"
    );
}

#[test]
fn a_driver_that_dispatches_before_it_commits_loses_the_intent() {
    let harness = Harness::new(geometry());
    let dispatched: RefCell<Vec<u32>> = RefCell::new(Vec::new());
    let logs: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());

    // The mutant: §07's steps, with the effect performed before its schedule record is
    // committed rather than after. Written by hand because `Driver` cannot express it.
    let runs = harness
        .run(|session| {
            logs.borrow_mut().push(Vec::new());
            dispatched.borrow_mut().clear();
            let mut page = [0_u8; 256];
            let recovery = Recovery::new(region());
            let mut scan = recovery;
            let mut probe = [0_u8; 256];
            while scan.next(session, &mut probe).is_some() {}
            let Some(mut journal) = Journal::after(scan) else {
                return Ok(());
            };
            let started = RecordRef::RunStarted {
                workflow_kind: WORKFLOW_KIND,
                workflow_version: WORKFLOW_VERSION,
                input: b"seed",
            };
            append(&mut journal, session, &started, &mut page)?;

            // Dispatched first. This is the whole mutation.
            dispatched.borrow_mut().push(0);
            if let Some(last) = logs.borrow_mut().last_mut() {
                last.push(0);
            }

            let schedule = RecordRef::EffectScheduled {
                seq: EffectSeq(0),
                kind: DOWNLOAD,
                input_len: 3,
                input_crc: frame::input_digest(b"url"),
            };
            append(&mut journal, session, &schedule, &mut page)?;
            let done = RecordRef::EffectCompleted {
                seq: EffectSeq(0),
                result: DOWNLOADED,
            };
            append(&mut journal, session, &done, &mut page)
        })
        .expect("the fault-free run completes");

    let logs = logs.into_inner();
    let lost = runs.iter().zip(&logs).any(|(run, performed)| {
        let history = recovered(run.image());
        performed.iter().any(|seq| {
            !history
                .iter()
                .filter_map(|record| record.scheduled())
                .any(|scheduled| scheduled == *seq)
        })
    });
    assert!(
        lost,
        "a driver that dispatches before it commits must lose an intent at some crash point"
    );
}

/// Design document §07's three steps for one record, for the mutant above.
fn append(
    journal: &mut Journal,
    session: &mut Session,
    record: &RecordRef<'_>,
    page: &mut [u8],
) -> Result<(), waymaker_flash::append::AppendError<FaultError>> {
    journal
        .stage(session, record, page)?
        .payload_barrier(session)?
        .commit(session)
        .map(|_| ())
}
