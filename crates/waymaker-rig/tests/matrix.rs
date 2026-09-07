//! Design document §14's failure-semantics table, run on the rig.
//!
//! Issue [#31](https://github.com/madmax983/waymaker/issues/31): every row of the table is an
//! executable assertion. The model half is `crates/waymaker-drive/tests/matrix.rs`. This is
//! the rig half: every crash point the injector lists is classified into a [`Row`], the rig
//! is *resumed* from the media the crash left, and the row's required behaviour is checked
//! against what the resume dispatched.
//!
//! # What the rig can reach
//!
//! Six rows: the schedule, dispatch and completion rows a rig that cuts during those three
//! writes can land in. Rows 7 to 10 need a swap workload, a capacity refusal and a divergent
//! replay, and this rig has none of them. [`the_rig_fills_six_rows_and_names_the_seventh_as_its_gap`]
//! pins that: [`Matrix::verdict`] must refuse, naming the first bank row. A census that
//! passed here would be a census of six cells wearing ten names.
//!
//! # How a row is read off a run
//!
//! From the witness, the recovered count and the journal's ending — all of which a board
//! has after a reset — plus one thing only the harness has: whether the dispatcher was
//! entered, and whether it returned. A `Dispatched` mark is written *before* the effect, so
//! a mark is not evidence of a dispatch; `tests/sweep.rs` says why.

use std::cell::RefCell;

use waymaker_fault::{Device, FaultError, Harness, Injection, Interruption, Run};
use waymaker_flash::bank;
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::audit::Breach;
use waymaker_rig::cutter::{Dispatcher, NeverCut};
use waymaker_rig::log::Outcome;
use waymaker_rig::matrix::{Matrix, Row};
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Resumed, Rig};
use waymaker_rig::wear::Metered;
use waymaker_rig::window::Window;
use waymaker_rig::witness::{Progress as Marks, Witness};
use waymaker_rig::workload::Role;

const SEED: u64 = 0x0031_0031_0031_0031;
const EFFECTS: u16 = 2;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(6 * 256, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

fn rig() -> Rig {
    let Ok(rig) = Rig::new::<FaultError>(geometry(), Plan::new(SEED), EFFECTS) else {
        unreachable!("the geometry above holds two banks and a witness")
    };
    rig
}

/// A dispatcher that records every effect it was entered for, and may refuse one.
///
/// `entered` is what a board cannot know after a reset and the harness can. `refuse` models
/// the supply going *during* the physical activity: the dispatcher is entered, the world may
/// have changed, and the call never returns `Ok`.
#[derive(Default)]
struct Log {
    entered: Vec<u16>,
    refuse: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Interrupted;

impl Dispatcher for Log {
    type Error = Interrupted;

    fn dispatch(&mut self, effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        self.entered.push(effect);
        if self.refuse == Some(effect) {
            return Err(Interrupted);
        }
        Ok(())
    }
}

/// One rig run through the harness: prepare, iterate, and what the dispatcher saw.
fn drive(session: &mut waymaker_fault::Session) -> (Result<(), String>, Vec<u16>) {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut dispatcher = Log::default();
    let mut metered = Metered::new(session);
    let outcome = rig
        .prepare(&mut metered, 0, &mut page)
        .map_err(|error| format!("prepare: {error:?}"))
        .and_then(|()| {
            rig.iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
                .map(|_| ())
                .map_err(|error| format!("iterate: {error:?}"))
        });
    (outcome, dispatcher.entered)
}

fn device_after(run: &Run) -> Device {
    let Some(device) = Device::restored(geometry(), run.image().to_vec()) else {
        unreachable!("the image came from a device of this geometry")
    };
    device
}

fn marks_on(rig: &Rig, device: &mut Device, page: &mut [u8]) -> Option<Marks> {
    let mut instrument =
        Window::new(device, rig.instrument_base(), geometry().erase_size()).ok()?;
    Witness::new(rig.witness_region())
        .scan(&mut instrument, page)
        .ok()
}

/// How the journal of the rig's bank ends, read the way a boot reads it.
fn ending_on(rig: &Rig, device: &mut Device, page: &mut [u8]) -> Option<Ending> {
    let layout = rig.layout();
    let region = layout.bank(Rig::BANK);
    let mut engine = Window::new(device, 0, layout.geometry().capacity()).ok()?;
    let want = usize::try_from(region.payload_bytes())
        .ok()?
        .min(page.len());
    engine.read(region.base(), page.get_mut(..want)?).ok()?;
    let header = bank::decode_header(page.get(..want)?).ok()?;
    let journal = JournalRegion::of(layout, Rig::BANK, &header).ok()?;
    let mut recovery = Recovery::new(journal);
    while let Some(step) = recovery.next(&mut engine, page) {
        if step.is_err() {
            break;
        }
    }
    recovery.ending()
}

/// How far the dispatcher got with the effect a record belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activity {
    /// Never entered.
    NotEntered,
    /// Entered and did not return: the supply went during the effect.
    Entered,
    /// Entered and returned.
    Returned,
}

/// What one crash point left, in the terms the rows are stated in.
#[derive(Clone, Copy, Debug)]
struct Evidence {
    /// The record whose write was begun last.
    attempted: Role,
    /// Whether that record was recovered.
    recovered_it: bool,
    /// What the dispatcher did with the record's effect.
    activity: Activity,
    /// Whether the journal ends in erased media.
    clean: bool,
}

/// Which row `evidence` is an instance of, or `None` for a crash outside every effect row.
const fn row_of(evidence: Evidence) -> Option<Row> {
    match (evidence.attempted, evidence.activity) {
        (Role::Schedule(_), _) if !evidence.recovered_it => Some(Row::DuringScheduleFrameWrite),
        (Role::Schedule(_), Activity::Returned) => Some(Row::AfterActivityBeforeCompletionBarrier),
        (Role::Schedule(_), Activity::Entered) => Some(Row::DuringPhysicalActivity),
        (Role::Schedule(_), Activity::NotEntered) => Some(Row::AfterScheduleBarrierBeforeDispatch),
        (Role::Completion(_), _) if evidence.recovered_it => Some(Row::AfterCompletionBarrier),
        (Role::Completion(_), _) if evidence.clean => {
            Some(Row::AfterActivityBeforeCompletionBarrier)
        }
        (Role::Completion(_), _) => Some(Row::DuringCompletionWrite),
        (Role::Finish, _) => Some(Row::AfterCompletionBarrier),
        (Role::Start, _) => None,
    }
}

/// The evidence a device and a dispatcher log give about one run.
fn evidence(rig: &Rig, device: &mut Device, entered: &[u16], returned: bool) -> Option<Evidence> {
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let marks = marks_on(rig, device, &mut page)?;
    let index = marks.attempted()?;
    let attempted = rig.workload(0).role(index)?;
    let verdict = rig.verify(0, device, &mut page).ok()?;
    if verdict.outcome() != Outcome::Passed {
        return None;
    }
    let effect = match attempted {
        Role::Schedule(effect) | Role::Completion(effect) => Some(effect),
        Role::Start | Role::Finish => None,
    };
    let activity = match effect.is_some_and(|effect| entered.contains(&effect)) {
        false => Activity::NotEntered,
        true if returned => Activity::Returned,
        true => Activity::Entered,
    };
    let clean = matches!(ending_on(rig, device, &mut page)?, Ending::Clean { .. });
    Some(Evidence {
        attempted,
        recovered_it: verdict.recovered() > index,
        activity,
        clean,
    })
}

/// Resumes the run on `device` and returns what it did and what it dispatched.
fn resume(rig: &Rig, device: &mut Device) -> (Resumed, Vec<u16>) {
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut dispatcher = Log::default();
    let mut metered = Metered::new(device);
    let Ok(resumed) = rig.resume(0, &mut metered, &mut dispatcher, &mut page) else {
        unreachable!("a run the rig judged healthy is a run it can resume")
    };
    (resumed, dispatcher.entered)
}

/// The effect a role belongs to.
const fn effect_of(role: Role) -> Option<u16> {
    match role {
        Role::Schedule(effect) | Role::Completion(effect) => Some(effect),
        Role::Start | Role::Finish => None,
    }
}

/// Checks the row's required behaviour against what the resume did.
fn require(row: Row, evidence: Evidence, resumed: Resumed, resumed_entered: &[u16], at: &str) {
    let effect = effect_of(evidence.attempted);
    match row {
        Row::DuringScheduleFrameWrite => {
            assert_eq!(
                evidence.activity,
                Activity::NotEntered,
                "not yet dispatchable, at {at}"
            );
            match resumed {
                Resumed::Completed { redelivered, .. } => {
                    assert_eq!(redelivered, None, "nothing was owed a redelivery, at {at}");
                    assert_eq!(resumed_entered.first().copied(), effect, "at {at}");
                }
                Resumed::Unextendable { .. } => assert!(resumed_entered.is_empty(), "at {at}"),
            }
        }
        Row::AfterScheduleBarrierBeforeDispatch => {
            assert_eq!(evidence.activity, Activity::NotEntered, "at {at}");
            assert_eq!(
                resumed,
                Resumed::Completed {
                    recovered: recovered_of(resumed),
                    redelivered: effect
                },
                "the stable id is redelivered, at {at}"
            );
            assert_eq!(resumed_entered.first().copied(), effect, "at {at}");
        }
        Row::DuringPhysicalActivity | Row::AfterActivityBeforeCompletionBarrier => {
            assert_ne!(
                evidence.activity,
                Activity::NotEntered,
                "the activity had begun, at {at}"
            );
            assert!(
                matches!(resumed, Resumed::Completed { redelivered, .. } if redelivered == effect),
                "the same id is redelivered, at {at}: {resumed:?}"
            );
            assert_eq!(resumed_entered.first().copied(), effect, "at {at}");
        }
        Row::DuringCompletionWrite => {
            assert_eq!(evidence.activity, Activity::Returned, "at {at}");
            assert!(
                matches!(resumed, Resumed::Unextendable { .. }),
                "a torn completion leaves no append point, at {at}: {resumed:?}"
            );
            assert!(resumed_entered.is_empty(), "nothing is dispatched, at {at}");
        }
        Row::AfterCompletionBarrier => {
            // A torn *terminal* record is in this row too, and it leaves no append point.
            if let Resumed::Completed { redelivered, .. } = resumed {
                assert_eq!(redelivered, None, "nothing was owed a redelivery, at {at}");
            }
            let completed_through = effect.unwrap_or(EFFECTS.saturating_sub(1));
            assert!(
                resumed_entered.iter().all(|ran| *ran > completed_through),
                "a completed activity ran again, at {at}: {resumed_entered:?}"
            );
        }
        Row::DuringInactiveBankEraseOrWrite
        | Row::AfterNewBankSealBarrier
        | Row::HistoryCapacityReached
        | Row::ReplayDivergence => unreachable!("this rig cannot reach {}", row.id()),
    }
}

const fn recovered_of(resumed: Resumed) -> u16 {
    match resumed {
        Resumed::Completed { recovered, .. } | Resumed::Unextendable { recovered } => recovered,
    }
}

/// The whole sweep, classified: one `(row, evidence, resumed, dispatched)` per crash point.
fn classified() -> (Matrix, Vec<(Row, Injection)>) {
    let harness = Harness::new(geometry());
    let logs: RefCell<Vec<Vec<u16>>> = RefCell::new(Vec::new());
    let Ok(runs) = harness.run(|session| {
        let (outcome, entered) = drive(session);
        logs.borrow_mut().push(entered);
        outcome.map_err(|_| ())
    }) else {
        unreachable!("the fault-free run succeeds")
    };
    let logs = logs.into_inner();
    assert_eq!(logs.len(), runs.len());

    let rig = rig();
    let mut matrix = Matrix::EMPTY;
    let mut rows = Vec::new();
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut device = device_after(run);
        // In the sweep a dispatch that was entered returned: nothing inside it can be cut.
        let Some(evidence) = evidence(&rig, &mut device, entered, true) else {
            continue;
        };
        let Some(row) = row_of(evidence) else {
            continue;
        };
        let (resumed, resumed_entered) = resume(&rig, &mut device);
        require(
            row,
            evidence,
            resumed,
            &resumed_entered,
            &format!("{injection:?}"),
        );
        matrix = matrix.record(row);
        rows.push((row, injection));
    }
    (matrix, rows)
}

#[test]
fn every_crash_point_the_rig_reaches_resumes_as_its_row_requires() {
    let (matrix, rows) = classified();
    assert!(
        rows.len() > 200,
        "only {} classified crash points",
        rows.len()
    );
    for row in [
        Row::DuringScheduleFrameWrite,
        Row::AfterScheduleBarrierBeforeDispatch,
        Row::AfterActivityBeforeCompletionBarrier,
        Row::DuringCompletionWrite,
        Row::AfterCompletionBarrier,
    ] {
        assert!(
            matrix.iterations(row) > 0,
            "no crash point landed in {}",
            row.id()
        );
    }
    // A watchdog reset reaches the schedule row and the completion row too, or the census
    // is a power-cut census wearing both names.
    for row in [Row::DuringScheduleFrameWrite, Row::AfterCompletionBarrier] {
        assert!(
            rows.iter().any(|(r, injection)| {
                *r == row && injection.interruption == Interruption::Watchdog
            }),
            "no watchdog reset landed in {}",
            row.id()
        );
    }
}

#[test]
fn a_reset_during_the_physical_activity_redelivers_the_same_effect_on_the_rig() {
    // Row 3. Not a storage crash point: the dispatcher is entered and does not return, which
    // is what the supply going during the effect looks like to the rig.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut matrix = Matrix::EMPTY;
    for effect in 0..EFFECTS {
        let mut device = Device::new(geometry());
        let mut log = Log {
            refuse: Some(effect),
            ..Log::default()
        };
        {
            let mut metered = Metered::new(&mut device);
            rig.prepare(&mut metered, 0, &mut page)
                .expect("a prepared part");
            let stopped = rig.iterate(0, &mut metered, &mut log, &mut NeverCut, &mut page);
            assert!(stopped.is_err(), "the interrupted activity stops the run");
        }
        assert_eq!(log.entered.last().copied(), Some(effect));
        let evidence = evidence(&rig, &mut device, &log.entered, false)
            .expect("a healthy device with a dispatch begun");
        assert_eq!(row_of(evidence), Some(Row::DuringPhysicalActivity));
        let (resumed, resumed_entered) = resume(&rig, &mut device);
        require(
            Row::DuringPhysicalActivity,
            evidence,
            resumed,
            &resumed_entered,
            &format!("effect {effect}"),
        );
        // The duplicate attempt carries the same effect index, and the run completes.
        let attempts = log
            .entered
            .iter()
            .chain(&resumed_entered)
            .filter(|e| **e == effect);
        assert_eq!(attempts.count(), 2, "effect {effect} was attempted twice");
        matrix = matrix.record(Row::DuringPhysicalActivity);
    }
    assert_eq!(
        matrix.iterations(Row::DuringPhysicalActivity),
        u32::from(EFFECTS)
    );
}

#[test]
fn the_rig_fills_six_rows_and_names_the_seventh_as_its_gap() {
    // The honest shape of this rig: six effect rows reached, and a verdict that refuses
    // rather than a census that stops at six. Rows 7 to 10 are owed — see
    // `xtask::docs::FAILURE_ROWS`.
    let (mut matrix, _) = classified();
    matrix = matrix.record(Row::DuringPhysicalActivity);
    for row in Row::ALL.iter().take(6) {
        assert!(matrix.iterations(*row) > 0, "{} is empty", row.id());
    }
    let gap = matrix.verdict().expect_err("four rows are owed");
    assert_eq!(gap.row(), Row::DuringInactiveBankEraseOrWrite);
    assert_eq!(
        gap.to_string(),
        "no crash point landed in row `during-inactive-bank-erase-or-write`"
    );
}

#[test]
fn the_rows_are_ten_and_each_is_its_own_index_and_id() {
    assert_eq!(Row::ALL.len(), 10);
    let mut ids = Vec::new();
    for (index, row) in Row::ALL.iter().enumerate() {
        assert_eq!(row.index(), index);
        assert_eq!(Row::from_index(index), Some(*row));
        assert!(
            row.id()
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-'),
            "{}",
            row.id()
        );
        ids.push(row.id());
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 10, "ids are distinct");
    assert_eq!(Row::from_index(10), None);
    assert_eq!(
        Row::ALL.first().map(|r| r.id()),
        Some("during-schedule-frame-write")
    );
    assert_eq!(Row::ALL.last().map(|r| r.id()), Some("replay-divergence"));
}

#[test]
fn a_matrix_counts_saturates_and_refuses_the_first_empty_row() {
    let mut matrix = Matrix::EMPTY;
    assert_eq!(matrix.total(), 0);
    assert_eq!(
        matrix.verdict().expect_err("empty").row(),
        Row::DuringScheduleFrameWrite
    );
    for row in Row::ALL {
        matrix = matrix.record(row);
    }
    assert_eq!(matrix.total(), 10);
    matrix.verdict().expect("every row reached");
    let saturated = matrix.saturated(Row::ReplayDivergence);
    assert_eq!(saturated.iterations(Row::ReplayDivergence), u32::MAX);
    assert_eq!(
        saturated
            .saturated(Row::ReplayDivergence)
            .record(Row::ReplayDivergence),
        saturated
    );
    assert_eq!(saturated.total(), u32::MAX);
}

#[test]
fn a_progress_reading_is_not_a_row_reading() {
    // The classification never reads `Progress`: a torn witness mark and a whole one are
    // the same row when the media and the dispatcher say the same thing.
    let base = Evidence {
        attempted: Role::Schedule(1),
        recovered_it: true,
        activity: Activity::NotEntered,
        clean: true,
    };
    assert_eq!(row_of(base), Some(Row::AfterScheduleBarrierBeforeDispatch));
    assert_eq!(
        row_of(Evidence {
            activity: Activity::Entered,
            ..base
        }),
        Some(Row::DuringPhysicalActivity)
    );
    assert_eq!(
        row_of(Evidence {
            activity: Activity::Returned,
            ..base
        }),
        Some(Row::AfterActivityBeforeCompletionBarrier)
    );
    assert_eq!(
        row_of(Evidence {
            recovered_it: false,
            ..base
        }),
        Some(Row::DuringScheduleFrameWrite)
    );
    let completion = Evidence {
        attempted: Role::Completion(1),
        recovered_it: false,
        activity: Activity::Returned,
        clean: false,
    };
    assert_eq!(row_of(completion), Some(Row::DuringCompletionWrite));
    assert_eq!(
        row_of(Evidence {
            clean: true,
            ..completion
        }),
        Some(Row::AfterActivityBeforeCompletionBarrier)
    );
    assert_eq!(
        row_of(Evidence {
            recovered_it: true,
            ..completion
        }),
        Some(Row::AfterCompletionBarrier)
    );
    assert_eq!(
        row_of(Evidence {
            attempted: Role::Finish,
            ..completion
        }),
        Some(Row::AfterCompletionBarrier)
    );
    assert_eq!(
        row_of(Evidence {
            attempted: Role::Start,
            ..completion
        }),
        None
    );
}

#[test]
fn a_resume_refuses_a_short_page_an_uninstalled_part_and_another_runs_prefix() {
    use waymaker_rig::run::RigError;

    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut short = [0_u8; Rig::PAGE_BYTES / 2];

    // A short page, before anything is read.
    let mut erased = Device::new(geometry());
    let mut metered = Metered::new(&mut erased);
    let refused = rig.resume(0, &mut metered, &mut Log::default(), &mut short);
    assert!(matches!(refused, Err(RigError::ShortPage)), "{refused:?}");

    // A part nothing was installed on: no bank names this run.
    let refused = rig.resume(0, &mut metered, &mut Log::default(), &mut page);
    assert!(matches!(refused, Err(RigError::Bank)), "{refused:?}");

    // A bank installed for iteration 0 and written by iteration 1: the prefix is not this
    // run's, and the resume refuses before writing.
    let mut device = Device::new(geometry());
    let mut metered = Metered::new(&mut device);
    rig.prepare(&mut metered, 0, &mut page)
        .expect("a prepared part");
    rig.iterate(
        1,
        &mut metered,
        &mut Log::default(),
        &mut NeverCut,
        &mut page,
    )
    .expect("another iteration writes into the bank");
    let before = metered.wear();
    let refused = rig.resume(0, &mut metered, &mut Log::default(), &mut page);
    assert!(
        matches!(
            refused,
            Err(RigError::Breach(Breach::RecordDiffers { index: 0 }))
        ),
        "{refused:?}"
    );
    assert_eq!(metered.wear(), before, "a refused resume writes nothing");
}
