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
//! * **whole results** (§07 steps 5 to 7, and issue #29). No outcome the image recovers to
//!   holds part of an answer. A torn outcome frame has no commit seal, so the scan stops at
//!   it; the tooth for that is `crates/waymaker-fault/tests/commit_discipline.rs`, whose
//!   seal-before-frame writer reaches the state this asserts is unreachable.
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
    BOUNDS, DOWNLOAD, DOWNLOADED, HASHED, Pipeline, WORKFLOW_KIND, WORKFLOW_VERSION, World,
};
use waymaker_drive::{DriveError, Driver, Scratch};
use waymaker_fault::{Device, FaultError, Harness, Session};
use waymaker_flash::append::Journal;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// Two erase blocks, which is §10's minimum and what §04's reserve is priced against. The
/// journal below is one of them; the other is the bank a `continue_as_new` would install
/// into, which this driver does not perform but the reserve still prices.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 512, 4, 1) else {
        unreachable!("1024 is two whole 512-byte blocks of 4-byte units")
    };
    geometry
}

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 512, align) else {
        unreachable!("the region is the device's first erase block")
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

/// The sequence of the schedule record no outcome follows, if history left one open.
const fn unresolved(history: &[Summary]) -> Option<u32> {
    match history.last() {
        Some(Summary::EffectScheduled(seq)) => Some(*seq),
        Some(Summary::RunStarted | Summary::EffectResolved(_) | Summary::Terminal) | None => None,
    }
}

/// Every effect outcome the image recovers to, with its payload.
fn recovered_outcomes(image: &[u8]) -> Vec<Vec<u8>> {
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
        match record {
            RecordRef::EffectCompleted { result, .. } => out.push(result.to_vec()),
            RecordRef::EffectFailed { error, .. } => out.push(error.to_vec()),
            _ => {}
        }
    }
    out
}

/// One boot of the reference workflow over `session`, and the sequences it dispatched.
fn drive(
    session: &mut Session,
    dispatched: &RefCell<Vec<u32>>,
) -> Result<(), DriveError<FaultError>> {
    drive_world(session, dispatched, World::new())
}

/// The same, for a world that answers differently.
fn drive_world(
    session: &mut Session,
    dispatched: &RefCell<Vec<u32>>,
    mut world: World,
) -> Result<(), DriveError<FaultError>> {
    let mut workflow = Pipeline::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
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
    let mut redelivered = 0_usize;
    let mut redelivered_late = 0_usize;
    for run in &runs {
        let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
            unreachable!("the image is device-sized")
        };
        let mut workflow = Pipeline::new();
        let mut world = World::new();
        let mut page = [0_u8; 256];
        let mut result = [0_u8; 64];
        let ended = Driver::new(region(), RUN, reserve()).boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
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
                // §14's redelivery contract, stated so a fresh mint fails it. When the
                // crash left a schedule with no outcome, the resumed run's *first* dispatch
                // must wear that sequence — a driver that re-minted would answer
                // `EffectSeq(0)`, which is where the allocator starts, and would be caught
                // wherever the outstanding effect is the second one.
                if let Some(outstanding) = unresolved(&recovered(run.image())) {
                    let first = world.dispatched().first().unwrap_or_else(|| {
                        panic!("the resumed run redelivers, at {:?}", run.injection())
                    });
                    assert_eq!(
                        first.id.seq.0,
                        outstanding,
                        "the resumed run redelivered {:?} for an outstanding {outstanding}, at {:?}",
                        first.id.seq,
                        run.injection()
                    );
                    redelivered += 1;
                    if outstanding > 0 {
                        redelivered_late += 1;
                    }
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
        redelivered > 0,
        "some crash images leave an effect outstanding, which is what redelivery is for"
    );
    assert!(
        redelivered_late > 0,
        "and some of those are the run's *second* effect, which is the only case that tells \
         redelivery apart from a fresh identity"
    );
    assert!(
        unextendable > 0,
        "and some do not, which is the case a bank swap exists for"
    );
}

#[test]
fn a_driver_that_dispatches_before_it_commits_loses_the_intent() {
    let harness = Harness::new(geometry());
    let logs: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());

    // The mutant: §07's steps, with the effect performed before its schedule record is
    // committed rather than after. Written by hand because `Driver` cannot express it.
    let runs = harness
        .run(|session| {
            logs.borrow_mut().push(Vec::new());
            let mut page = [0_u8; 256];
            let mut scan = Recovery::new(region());
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

#[test]
fn no_recovered_outcome_holds_part_of_an_answer_at_any_crash_point() {
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive(session, &RefCell::new(Vec::new())))
        .expect("the fault-free run completes");

    assert!(
        runs.len() > 1,
        "the enumeration found crash points to sweep"
    );
    let mut seen = 0_usize;
    for run in &runs {
        for payload in recovered_outcomes(run.image()) {
            seen = seen.saturating_add(1);
            assert!(
                payload == DOWNLOADED || payload == HASHED,
                "a committed outcome holds {} bytes, which is part of an answer, at {:?}",
                payload.len(),
                run.injection()
            );
        }
    }
    assert!(
        seen > 0,
        "a sweep that recovered no outcome at all measured nothing"
    );
}

#[test]
fn an_exhausted_answer_stays_empty_and_keeps_its_intent_at_every_crash_point() {
    let harness = Harness::new(geometry());
    let logs: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());

    let runs = harness
        .run(|session| {
            logs.borrow_mut().push(Vec::new());
            let mine = RefCell::new(Vec::new());
            // The first effect's answer is wider than the bound the world is handed.
            let ended = drive_world(session, &mine, World::exhausting_at(0));
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
            Summary::Terminal,
        ],
        "an exhausted effect resolves, and the workflow's failure branch ends the run"
    );

    let mut truncated = 0_usize;
    for (run, dispatched) in runs.iter().zip(&logs) {
        let history = recovered(run.image());
        assert!(
            whole.starts_with(&history),
            "{:?} is not a prefix of the fault-free history: {history:?}",
            run.injection()
        );
        if history.len() < whole.len() {
            truncated = truncated.saturating_add(1);
        }
        for payload in recovered_outcomes(run.image()) {
            assert!(
                payload.is_empty(),
                "an exhausted effect committed {} bytes at {:?}",
                payload.len(),
                run.injection()
            );
        }
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
fn a_reboot_after_an_exhausted_effect_carries_the_run_on_or_refuses_before_dispatching() {
    // The window `an_exhausted_answer_…` leaves: what the *next* boot does with a crash
    // image an exhausted effect left behind. The world is keyed on the effect sequence
    // rather than on a per-boot dispatch counter, so the same effect exhausts on every boot.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive_world(session, &RefCell::new(Vec::new()), World::exhausting_seq(0)))
        .expect("the fault-free run completes");

    let mut resumed = 0_usize;
    let mut refused = 0_usize;
    for run in &runs {
        let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
            unreachable!("the image is device-sized")
        };
        let mut workflow = Pipeline::new();
        let mut world = World::exhausting_seq(0);
        let mut page = [0_u8; 256];
        let mut result = [0_u8; 64];
        let ended = Driver::new(region(), RUN, reserve()).boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        );
        if ended.is_ok() {
            {
                resumed = resumed.saturating_add(1);
                assert_eq!(
                    recovered(device.image()),
                    [
                        Summary::RunStarted,
                        Summary::EffectScheduled(0),
                        Summary::EffectResolved(0),
                        Summary::Terminal,
                    ],
                    "a resumed run reaches the same history, at {:?}",
                    run.injection()
                );
                for payload in recovered_outcomes(device.image()) {
                    assert!(
                        payload.is_empty(),
                        "a resumed run committed {} bytes for an exhausted effect, at {:?}",
                        payload.len(),
                        run.injection()
                    );
                }
            }
        } else {
            // §14: a torn or unsealed tail has no append point, and this driver does not
            // swap banks — so it refuses rather than repairing. Nothing was dispatched.
            refused = refused.saturating_add(1);
            assert_eq!(world.performed(), 0, "at {:?}", run.injection());
        }
    }
    assert!(
        resumed > 0,
        "no crash image was one the run could carry on from"
    );
    assert!(refused > 0, "no crash image was one the run had to refuse");
}
