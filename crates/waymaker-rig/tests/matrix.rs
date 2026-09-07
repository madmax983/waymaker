//! Design document §14's failure-semantics table, run on the rig.
//!
//! Issue [#31](https://github.com/madmax983/waymaker/issues/31): every row of the table is an
//! executable assertion. The model half is `crates/waymaker-drive/tests/matrix.rs`. This is
//! the rig half: every crash point the injector lists is classified into a [`Row`], the rig
//! is *resumed* from the media the crash left, and the row's required behaviour is checked
//! against what the resume dispatched. One test per row, named after it, and the
//! `failure-matrix` rule reads the names out of this file.
//!
//! # What the rig can reach
//!
//! Six rows: the schedule, dispatch and completion rows a rig that cuts during those three
//! writes can land in. Rows 7 to 10 need a swap workload, a capacity refusal and a divergent
//! replay, and this rig has none of them — issue
//! [#96](https://github.com/madmax983/waymaker/issues/96).
//! [`the_rig_fills_six_rows_and_names_the_seventh_as_its_gap`] pins that: [`Matrix::verdict`]
//! must refuse, naming the first bank row. A census that passed here would be a census of six
//! cells wearing ten names.
//!
//! # How a row is read off a run
//!
//! From the witness, the recovered count and the journal's ending — all of which a board
//! has after a reset — plus one thing only the harness has: whether the dispatcher was
//! entered, and whether it returned. A `Dispatched` mark is written *before* the effect, so
//! a mark is not evidence of a dispatch; `tests/sweep.rs` says why. Rows 2, 3 and 4 rest on
//! that evidence, and a board cannot supply it — issue #96 again.
//!
//! Row 3 is not a crash point of the injector: it is a run whose dispatcher is entered and
//! does not return, which is the supply going during the effect as the rig sees it. It is
//! credited from those runs, not from the injector's cause.
//!
//! Row 6 is decided by what recovery produced, so a completion seal that landed whole with
//! its barrier refused is in it beside the points that are strictly after the barrier; the
//! model half says the same of rows 2 and 6.

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
use waymaker_rig::run::{Resumed, Rig, RigError};
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
    /// Its index in the run.
    index: u16,
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

/// Why a crash point is outside every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skip {
    /// A failed call is not a reset.
    Failure,
    /// The witness has no whole mark: the cut landed before or inside the first one.
    NoWitness,
    /// The rig's own oracle refused the part. Never expected, and counted so it shows.
    Breached,
    /// The opening record was the last begun: no effect is concerned.
    Outside,
}

/// The evidence a device and a dispatcher log give about one run.
fn evidence(
    rig: &Rig,
    device: &mut Device,
    entered: &[u16],
    returned: bool,
) -> Result<Evidence, Skip> {
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let marks = marks_on(rig, device, &mut page).ok_or(Skip::NoWitness)?;
    let index = marks.attempted().ok_or(Skip::NoWitness)?;
    let attempted = rig.workload(0).role(index).ok_or(Skip::Breached)?;
    let verdict = rig
        .verify(0, device, &mut page)
        .map_err(|_| Skip::Breached)?;
    if verdict.outcome() != Outcome::Passed {
        return Err(Skip::Breached);
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
    let clean = matches!(
        ending_on(rig, device, &mut page).ok_or(Skip::Breached)?,
        Ending::Clean { .. }
    );
    Ok(Evidence {
        attempted,
        index,
        recovered_it: verdict.recovered() > index,
        activity,
        clean,
    })
}

/// Resumes the run on `device`, then judges the resumed part.
///
/// Returns what the resume did and what it dispatched. The verdict after the resume is
/// asserted here, for every point: a resume that left a healthy part looking breached would
/// be an instrument failing in the direction it must not.
fn resume(rig: &Rig, device: &mut Device, at: &str) -> (Resumed, Vec<u16>) {
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut dispatcher = Log::default();
    let resumed = {
        let mut metered = Metered::new(&mut *device);
        let Ok(resumed) = rig.resume(0, &mut metered, &mut dispatcher, &mut page) else {
            unreachable!("a run the rig judged healthy is a run it can resume, at {at}")
        };
        resumed
    };
    let Ok(verdict) = rig.verify(0, device, &mut page) else {
        unreachable!("a resumed part is judgeable, at {at}")
    };
    assert_eq!(
        verdict.outcome(),
        Outcome::Passed,
        "the resumed part is healthy, at {at}"
    );
    (resumed, dispatcher.entered)
}

/// The effect a role belongs to.
const fn effect_of(role: Role) -> Option<u16> {
    match role {
        Role::Schedule(effect) | Role::Completion(effect) => Some(effect),
        Role::Start | Role::Finish => None,
    }
}

/// One classified crash point, resumed.
struct Classified {
    row: Row,
    injection: Option<Injection>,
    evidence: Evidence,
    resumed: Resumed,
    resumed_entered: Vec<u16>,
}

impl Classified {
    fn at(&self) -> String {
        format!("{} at {:?}", self.row.id(), self.injection)
    }
}

/// Checks the row's required behaviour against what the resume did.
fn require(point: &Classified) {
    let Classified {
        row,
        evidence,
        resumed,
        resumed_entered,
        ..
    } = point;
    let at = point.at();
    let effect = effect_of(evidence.attempted);
    match row {
        Row::DuringScheduleFrameWrite => {
            assert_eq!(
                evidence.activity,
                Activity::NotEntered,
                "not yet dispatchable: {at}"
            );
            match resumed {
                Resumed::Completed { redelivered, .. } => {
                    assert_eq!(*redelivered, None, "nothing was owed a redelivery: {at}");
                    assert_eq!(resumed_entered.first().copied(), effect, "{at}");
                }
                Resumed::Unextendable { .. } => assert!(resumed_entered.is_empty(), "{at}"),
            }
        }
        Row::AfterScheduleBarrierBeforeDispatch => {
            assert_eq!(evidence.activity, Activity::NotEntered, "{at}");
            // Schedule recovered without completion: the prefix ends at it.
            assert_eq!(
                *resumed,
                Resumed::Completed {
                    recovered: evidence.index + 1,
                    redelivered: effect
                },
                "the stable id is redelivered: {at}"
            );
            assert_eq!(resumed_entered.first().copied(), effect, "{at}");
        }
        Row::DuringPhysicalActivity | Row::AfterActivityBeforeCompletionBarrier => {
            assert_ne!(
                evidence.activity,
                Activity::NotEntered,
                "the activity had begun: {at}"
            );
            assert!(
                matches!(resumed, Resumed::Completed { redelivered, .. } if *redelivered == effect),
                "the same id is redelivered: {at}: {resumed:?}"
            );
            assert_eq!(resumed_entered.first().copied(), effect, "{at}");
        }
        Row::DuringCompletionWrite => {
            assert_eq!(evidence.activity, Activity::Returned, "{at}");
            assert_eq!(
                *resumed,
                Resumed::Unextendable {
                    recovered: evidence.index
                },
                "a torn completion is ignored and leaves no append point: {at}"
            );
            assert!(resumed_entered.is_empty(), "nothing is dispatched: {at}");
        }
        Row::AfterCompletionBarrier => {
            // A torn *terminal* record is in this row too, and it leaves no append point.
            if let Resumed::Completed { redelivered, .. } = resumed {
                assert_eq!(*redelivered, None, "nothing was owed a redelivery: {at}");
            }
            let completed_through = effect.unwrap_or(EFFECTS.saturating_sub(1));
            assert!(
                resumed_entered.iter().all(|ran| *ran > completed_through),
                "a completed activity ran again: {at}: {resumed_entered:?}"
            );
        }
        Row::DuringInactiveBankEraseOrWrite
        | Row::AfterNewBankSealBarrier
        | Row::HistoryCapacityReached
        | Row::ReplayDivergence => unreachable!("this rig cannot reach {}", row.id()),
    }
}

/// The whole sweep: every crash point classified and resumed, or skipped for a named reason.
fn classified() -> (Vec<Classified>, Vec<Skip>) {
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
    let mut points = Vec::new();
    let mut skips = Vec::new();
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            skips.push(Skip::Failure);
            continue;
        }
        let mut device = device_after(run);
        // In the sweep a dispatch that was entered returned: nothing inside it can be cut.
        let evidence = match evidence(&rig, &mut device, entered, true) {
            Ok(evidence) => evidence,
            Err(skip) => {
                skips.push(skip);
                continue;
            }
        };
        let Some(row) = row_of(evidence) else {
            skips.push(Skip::Outside);
            continue;
        };
        let at = format!("{} at {injection:?}", row.id());
        let (resumed, resumed_entered) = resume(&rig, &mut device, &at);
        points.push(Classified {
            row,
            injection: Some(injection),
            evidence,
            resumed,
            resumed_entered,
        });
    }
    (points, skips)
}

/// Row 3, driven: one run per effect whose dispatcher is entered and does not return.
fn row_three() -> Vec<Classified> {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut points = Vec::new();
    for effect in 0..EFFECTS {
        let mut device = Device::new(geometry());
        let mut log = Log {
            refuse: Some(effect),
            ..Log::default()
        };
        {
            let mut metered = Metered::new(&mut device);
            if rig.prepare(&mut metered, 0, &mut page).is_err() {
                unreachable!("the fixture geometry prepares")
            }
            let stopped = rig.iterate(0, &mut metered, &mut log, &mut NeverCut, &mut page);
            assert!(stopped.is_err(), "the interrupted activity stops the run");
        }
        assert_eq!(log.entered.last().copied(), Some(effect));
        let Ok(evidence) = evidence(&rig, &mut device, &log.entered, false) else {
            unreachable!("a healthy device with a dispatch begun")
        };
        let at = format!("effect {effect}");
        let (resumed, resumed_entered) = resume(&rig, &mut device, &at);
        // The duplicate attempt carries the same effect index.
        let attempts = log
            .entered
            .iter()
            .chain(&resumed_entered)
            .filter(|e| **e == effect)
            .count();
        assert_eq!(attempts, 2, "effect {effect} was attempted twice");
        points.push(Classified {
            row: Row::DuringPhysicalActivity,
            injection: None,
            evidence,
            resumed,
            resumed_entered,
        });
    }
    points
}

/// The classified points of `row`, required to exist and to be reached by both causes.
fn points_of(row: Row) -> Vec<Classified> {
    let (points, _) = classified();
    let points: Vec<Classified> = points.into_iter().filter(|p| p.row == row).collect();
    assert!(!points.is_empty(), "no crash point landed in {}", row.id());
    for cause in [Interruption::PowerLoss, Interruption::Watchdog] {
        assert!(
            points
                .iter()
                .any(|p| p.injection.is_some_and(|i| i.interruption == cause)),
            "{} was never reached by a {cause:?}",
            row.id()
        );
    }
    assert!(
        points
            .iter()
            .any(|p| effect_of(p.evidence.attempted).is_some_and(|e| e > 0)),
        "{} has no instance on the run's second effect",
        row.id()
    );
    points
}

#[test]
fn during_schedule_frame_write_the_frame_is_ignored_and_the_activity_was_not_yet_dispatchable_on_the_rig()
 {
    let points = points_of(Row::DuringScheduleFrameWrite);
    for point in &points {
        require(point);
    }
    // Both shapes: a torn frame the resume refuses, and a frame never begun it writes afresh.
    assert!(
        points
            .iter()
            .any(|p| matches!(p.resumed, Resumed::Unextendable { .. }))
    );
    assert!(
        points
            .iter()
            .any(|p| matches!(p.resumed, Resumed::Completed { .. }))
    );
}

#[test]
fn after_schedule_barrier_before_dispatch_the_stable_effect_id_is_redelivered_on_the_rig() {
    for point in points_of(Row::AfterScheduleBarrierBeforeDispatch) {
        require(&point);
    }
}

#[test]
fn during_physical_activity_the_effect_is_redelivered_and_the_activity_tolerates_the_duplicate_attempt_on_the_rig()
 {
    let points = row_three();
    assert_eq!(points.len(), usize::from(EFFECTS));
    for point in &points {
        assert_eq!(point.row, Row::DuringPhysicalActivity);
        require(point);
    }
}

#[test]
fn after_physical_activity_before_completion_barrier_the_same_id_is_redelivered_on_the_rig() {
    for point in points_of(Row::AfterActivityBeforeCompletionBarrier) {
        require(&point);
    }
}

#[test]
fn during_completion_write_the_torn_completion_is_ignored_and_no_partial_result_bytes_are_exposed_on_the_rig()
 {
    for point in points_of(Row::DuringCompletionWrite) {
        require(&point);
    }
}

#[test]
fn after_completion_barrier_the_completion_is_replayed_and_the_activity_never_runs_again_on_the_rig()
 {
    for point in points_of(Row::AfterCompletionBarrier) {
        require(&point);
    }
}

#[test]
fn the_rig_fills_six_rows_and_names_the_seventh_as_its_gap() {
    // The honest shape of this rig: six effect rows reached, and a verdict that refuses
    // rather than a census that stops at six. Rows 7 to 10 are owed — see
    // `xtask::docs::FAILURE_ROWS` and issue #96.
    let (points, _) = classified();
    let mut matrix = Matrix::EMPTY;
    for point in points.iter().chain(&row_three()) {
        matrix = matrix.record(point.row);
    }
    let counts: Vec<(Row, u32)> = Row::ALL
        .into_iter()
        .map(|row| (row, matrix.iterations(row)))
        .collect();
    // Pinned exactly, so a sweep that quietly thinned fails closed.
    assert_eq!(
        counts,
        [
            (Row::DuringScheduleFrameWrite, 86),
            (Row::AfterScheduleBarrierBeforeDispatch, 84),
            (Row::DuringPhysicalActivity, 2),
            (Row::AfterActivityBeforeCompletionBarrier, 42),
            (Row::DuringCompletionWrite, 84),
            (Row::AfterCompletionBarrier, 138),
            (Row::DuringInactiveBankEraseOrWrite, 0),
            (Row::AfterNewBankSealBarrier, 0),
            (Row::HistoryCapacityReached, 0),
            (Row::ReplayDivergence, 0),
        ]
    );
    let gap = matrix.verdict().expect_err("four rows are owed");
    assert_eq!(gap.row(), Row::DuringInactiveBankEraseOrWrite);
    assert_eq!(
        gap.to_string(),
        "no crash point landed in row `during-inactive-bank-erase-or-write`"
    );
}

#[test]
fn the_sweep_skips_only_for_a_named_reason_and_never_because_the_oracle_refused() {
    // A silent `continue` is how a thinning classification hides. Every crash point is a
    // row or a counted skip, the oracle refuses none, and each expected reason occurs.
    let (points, skips) = classified();
    let count = |skip: Skip| skips.iter().filter(|s| **s == skip).count();
    assert_eq!(
        count(Skip::Breached),
        0,
        "the oracle refused a healthy part"
    );
    assert!(count(Skip::Failure) > 0, "no failed call was skipped");
    assert!(
        count(Skip::NoWitness) > 0,
        "no cut landed before the first mark"
    );
    assert!(
        count(Skip::Outside) > 0,
        "no cut landed in the opening record"
    );
    // Every injected run; the fault-free run has no injection and is neither.
    assert_eq!(points.len() + skips.len(), 1096, "the sweep changed size");
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
fn a_row_is_read_from_the_dispatcher_and_the_media_and_never_from_a_mark() {
    let base = Evidence {
        attempted: Role::Schedule(1),
        index: 3,
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
        index: 4,
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
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut short = [0_u8; Rig::PAGE_BYTES / 2];

    // A short page, before anything is read.
    let mut erased = Device::new(geometry());
    let mut metered = Metered::new(&mut erased);
    let refused = rig.resume(0, &mut metered, &mut Log::default(), &mut short);
    assert!(matches!(refused, Err(RigError::ShortPage)), "{refused:?}");

    // A part nothing was installed on: no bank is authoritative.
    let refused = rig.resume(0, &mut metered, &mut Log::default(), &mut page);
    assert!(
        matches!(refused, Err(RigError::Authority { banks: 0 })),
        "{refused:?}"
    );

    // A bank installed for iteration 1: the authority names another run.
    let mut other = Device::new(geometry());
    let mut metered = Metered::new(&mut other);
    rig.prepare(&mut metered, 1, &mut page)
        .expect("a prepared part");
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
    let before = (metered.wear(), metered.rig_wear());
    let refused = rig.resume(0, &mut metered, &mut Log::default(), &mut page);
    assert!(
        matches!(
            refused,
            Err(RigError::Breach(Breach::RecordDiffers { index: 0 }))
        ),
        "{refused:?}"
    );
    assert_eq!(
        (metered.wear(), metered.rig_wear()),
        before,
        "a refused resume writes nothing"
    );
}
