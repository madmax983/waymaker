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

use waymaker_fault::{Device, FaultError, Harness, Injection, Interruption, Op, Progress, Run};
use waymaker_flash::bank;
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::audit::Breach;
use waymaker_rig::cutter::{Dispatcher, NeverCut};
use waymaker_rig::log::Outcome;
use waymaker_rig::matrix::{Matrix, Row};
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Resumed, Rig, RigError, Verdict};
use waymaker_rig::wear::Metered;
use waymaker_rig::window::Window;
use waymaker_rig::witness::{Progress as Marks, Witness, WitnessError};
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

/// The resume of a crashed part, as a writer the injector can cut.
///
/// The crashed image is put back with one program, op 0, and the rig resumes over it. The
/// resume's own operations are the ops after it, so every crash point the injector lists for
/// them is a reset the resume itself takes: every byte of every mark and every record, and
/// every barrier. A resume that fails with nothing armed is a fault of the test.
fn restore_and_resume(
    rig: &Rig,
    image: &[u8],
    session: &mut waymaker_fault::Session,
) -> Result<(), String> {
    let mut page = [0_u8; Rig::PAGE_BYTES];
    session
        .program(0, image)
        .map_err(|error| format!("restore: {error:?}"))?;
    let mut metered = Metered::new(session);
    rig.resume(0, &mut metered, &mut Log::default(), &mut page)
        .map(|_| ())
        .map_err(|error| format!("resume: {error:?}"))
}

/// Whether `op` is a program of a witness slot.
const fn programs_the_instrument(rig: &Rig, op: Op) -> bool {
    matches!(op, Op::Program { offset, .. } if offset >= rig.instrument_base())
}

#[test]
fn a_reset_at_any_point_of_a_resume_leaves_a_part_the_rig_judges_healthy() {
    // Codex found the first version of `resume` erasing the instrument: a reset after that
    // erase left a journal with records and a witness that claimed none, and `verify`
    // accused a healthy part. The resume now continues the witness. Round 3 found the test
    // for that cutting only between calls, so no mark was ever torn by a resume. This runs
    // every resume of the sweep through the injector, so a reset lands inside every byte
    // of every mark and record the resume writes, and judges what is left. Each distinct
    // image once: two crash points that left the same media are one resume.
    let harness = Harness::new(geometry());
    let Ok(runs) = harness.run(|session| drive(session).0.map_err(|_| ())) else {
        unreachable!("the fault-free run succeeds")
    };
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut images = std::collections::HashSet::new();
    let mut cuts = 0_usize;
    let mut torn_marks = 0_usize;
    let mut uninstalled = 0_usize;
    for run in runs.iter().skip(1) {
        let Some(landed) = run.injection() else {
            continue;
        };
        if landed.interruption == Interruption::Failure || !images.insert(run.image().to_vec()) {
            continue;
        }
        {
            let mut probe = device_after(run);
            if rig.verify(0, &mut probe, &mut page).map(Verdict::outcome) != Ok(Outcome::Passed) {
                continue;
            }
            match rig.resume(
                0,
                &mut Metered::new(&mut probe),
                &mut Log::default(),
                &mut page,
            ) {
                Ok(_) => {}
                // A cut before the bank seal landed: no run to resume, and nothing to cut.
                Err(RigError::Authority { banks: 0 }) => {
                    uninstalled += 1;
                    continue;
                }
                Err(error) => unreachable!("a healthy part resumes, after {landed:?}: {error:?}"),
            }
        }
        let image = run.image();
        // The control: the resume with nothing armed. `run_one` refuses a crash point
        // that never fires, so a past-the-end `op` can no longer stand in for "no
        // crash" — the fault-free run says it plainly.
        let Ok(clean) = harness.run_fault_free(|session| restore_and_resume(&rig, image, session))
        else {
            unreachable!("a resume nothing cuts completes, after {landed:?}")
        };
        let resume_ops = clean.ops().get(1..).unwrap_or_default();
        // A resume that writes nothing has no point to cut. The empty sequence's
        // sentinels are "before the first resume op", but there is no first resume
        // op — and `run_one` refuses a crash point the writer never reaches, so
        // they cannot be run. Skipping is honest: a reset at any point of a resume
        // that does nothing is a reset of the restored image, already judged above.
        if resume_ops.is_empty() {
            continue;
        }
        for point in waymaker_fault::injections(resume_ops, geometry()) {
            if point.interruption == Interruption::Failure {
                continue;
            }
            let injection = Injection {
                op: point.op + 1,
                ..point
            };
            let Ok(cut) = harness.run_one(injection, |session| {
                restore_and_resume(&rig, image, session)
            }) else {
                unreachable!("a deterministic resume, at {injection:?} after {landed:?}")
            };
            let mut device = device_after(&cut);
            let Ok(verdict) = rig.verify(0, &mut device, &mut page) else {
                unreachable!(
                    "a cut resume leaves a judgeable part, at {injection:?} after {landed:?}"
                )
            };
            assert_eq!(
                verdict.outcome(),
                Outcome::Passed,
                "a resume cut at {injection:?}, after {landed:?}"
            );
            cuts += 1;
            if matches!(point.progress, Progress::Bytes(_))
                && resume_ops
                    .get(point.op)
                    .is_some_and(|op| programs_the_instrument(&rig, *op))
            {
                torn_marks += 1;
            }
        }
    }
    // Pinned, so a sweep that thinned fails closed: the parts cut before the bank was
    // sealed, the distinct images resumed, the resets taken, and those inside a mark.
    // 180 of the 389 images resume to a part that needs no writes, so they contribute
    // no cuts: a resume that writes nothing has no point to cut.
    assert_eq!(
        (uninstalled, images.len(), cuts, torn_marks),
        (47, 389, 46_797, 15_990),
        "the resume sweep changed size"
    );
}

/// A part whose supply goes inside the first witness mark programmed after `base`.
///
/// One program unit of the mark lands, so the slot is neither erased nor a mark: the torn
/// slot a resume reads past. Every call after it is refused, as after a power cut.
struct TornMark<'a> {
    device: &'a mut Device,
    base: u32,
    torn: bool,
}

impl StableStorage for TornMark<'_> {
    type Error = FaultError;

    fn geometry(&self) -> Geometry {
        self.device.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        if self.torn {
            return Err(FaultError::PowerLoss);
        }
        self.device.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        if self.torn {
            return Err(FaultError::PowerLoss);
        }
        if offset < self.base {
            return self.device.program(offset, src);
        }
        self.torn = true;
        let unit =
            usize::try_from(self.geometry().program_size()).map_err(|_| FaultError::PowerLoss)?;
        let head = src.get(..unit).ok_or(FaultError::PowerLoss)?;
        self.device.program(offset, head)?;
        Err(FaultError::PowerLoss)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        if self.torn {
            return Err(FaultError::PowerLoss);
        }
        self.device.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        if self.torn {
            return Err(FaultError::PowerLoss);
        }
        self.device.barrier()
    }
}

#[test]
fn a_resume_survives_its_reset_budget_and_reports_the_reset_past_it() {
    // Codex, round 3: a torn slot is never reclaimed, so a witness sized for a clean run
    // alone is full at the first reset inside a mark. `Rig::new` reserves `TORN_SLOTS` past
    // the marks and `reset_budget` says how many the part holds. This spends the budget one
    // torn mark at a time and requires the run to finish, then spends one more and requires
    // the refusal to be `Full` with the part still judged healthy.
    let harness = Harness::new(geometry());
    let Ok(runs) = harness.run(|session| drive(session).0.map_err(|_| ())) else {
        unreachable!("the fault-free run succeeds")
    };
    let rig = rig();
    let budget = rig.reset_budget();
    assert!(budget >= Rig::TORN_SLOTS);
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let records = 2 * EFFECTS + 2;
    // A power cut that left a whole witness and a run with records still to write.
    let Some(run) = runs.iter().skip(1).find(|run| {
        let mut device = device_after(run);
        run.injection()
            .is_some_and(|i| i.interruption == Interruption::PowerLoss)
            && marks_on(&rig, &mut device, &mut page).is_some_and(|marks| !marks.torn())
            && rig.verify(0, &mut device, &mut page).map(Verdict::outcome) == Ok(Outcome::Passed)
            && matches!(
                rig.resume(0, &mut Metered::new(&mut device), &mut Log::default(), &mut page),
                Ok(Resumed::Completed { recovered, .. }) if recovered + 2 < records
            )
    }) else {
        unreachable!("the sweep has a resumable crash point")
    };
    for tears in 0..=budget {
        let mut device = device_after(run);
        for _ in 0..tears {
            let stopped = {
                let mut torn = TornMark {
                    device: &mut device,
                    base: rig.instrument_base(),
                    torn: false,
                };
                rig.resume(
                    0,
                    &mut Metered::new(&mut torn),
                    &mut Log::default(),
                    &mut page,
                )
            };
            assert!(stopped.is_err(), "the tear was not taken");
            assert!(
                marks_on(&rig, &mut device, &mut page).is_some_and(Marks::torn),
                "the witness does not read as torn"
            );
        }
        let resumed = rig.resume(
            0,
            &mut Metered::new(&mut device),
            &mut Log::default(),
            &mut page,
        );
        assert!(
            matches!(resumed, Ok(Resumed::Completed { .. })),
            "{tears} torn marks against a budget of {budget}: {resumed:?}"
        );
        let Ok(verdict) = rig.verify(0, &mut device, &mut page) else {
            unreachable!("a resumed part is judgeable")
        };
        assert_eq!(
            verdict.outcome(),
            Outcome::Passed,
            "after {tears} torn marks"
        );
    }
    let mut device = device_after(run);
    for _ in 0..=budget {
        let mut torn = TornMark {
            device: &mut device,
            base: rig.instrument_base(),
            torn: false,
        };
        let _ = rig.resume(
            0,
            &mut Metered::new(&mut torn),
            &mut Log::default(),
            &mut page,
        );
    }
    let resumed = rig.resume(
        0,
        &mut Metered::new(&mut device),
        &mut Log::default(),
        &mut page,
    );
    assert!(
        matches!(resumed, Err(RigError::Witness(WitnessError::Full))),
        "one reset past the budget: {resumed:?}"
    );
    let Ok(verdict) = rig.verify(0, &mut device, &mut page) else {
        unreachable!("a part whose instrument is full is still judgeable")
    };
    assert_eq!(verdict.outcome(), Outcome::Passed);
}

/// Reads the last committed record of the rig's bank back to erased media, as a part that
/// lost a committed suffix would present it.
fn blank_last_record(rig: &Rig, device: &mut Device, page: &mut [u8]) {
    let layout = rig.layout();
    let region = layout.bank(Rig::BANK);
    let (start, end) = {
        let Ok(mut engine) = Window::new(&mut *device, 0, layout.geometry().capacity()) else {
            unreachable!("the engine window")
        };
        let Some(want) = usize::try_from(region.payload_bytes())
            .ok()
            .map(|want| want.min(page.len()))
        else {
            unreachable!("a header fits a page")
        };
        let Some(head) = page.get_mut(..want) else {
            unreachable!("a header fits a page")
        };
        let Ok(()) = engine.read(region.base(), head) else {
            unreachable!("a readable bank")
        };
        let Ok(header) = bank::decode_header(head) else {
            unreachable!("an installed bank")
        };
        let Ok(journal) = JournalRegion::of(layout, Rig::BANK, &header) else {
            unreachable!("a journal region")
        };
        let mut recovery = Recovery::new(journal);
        let mut start = None;
        loop {
            let at = recovery.offset();
            match recovery.next(&mut engine, page) {
                Some(Ok(_)) => start = Some(at),
                Some(Err(_)) | None => break,
            }
        }
        let Some(start) = start else {
            unreachable!("a journal with a record in it")
        };
        (start, recovery.offset())
    };
    let mut image = device.image().to_vec();
    let (Ok(from), Ok(to)) = (usize::try_from(start), usize::try_from(end)) else {
        unreachable!("offsets index the image")
    };
    let Some(record) = image.get_mut(from..to) else {
        unreachable!("the record is inside the image")
    };
    record.fill(0xFF);
    let Some(blanked) = Device::restored(geometry(), image) else {
        unreachable!("the image is this geometry's")
    };
    *device = blanked;
}

#[test]
fn a_resume_refuses_a_part_that_lost_an_acknowledged_record_rather_than_rewriting_it() {
    // Codex, round 4: the prefix audit ran against a synthetic witness that claimed only that
    // every record was attempted, so a part that lost a committed, acknowledged record read
    // as resumable. The resume rewrote the record and `verify` then passed, masking the one
    // loss the rig exists to catch. The audit runs against the real witness now, and the
    // resume refuses before any mutation or dispatch.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut device = Device::new(geometry());
    {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("a prepared part");
        rig.iterate(
            0,
            &mut metered,
            &mut Log::default(),
            &mut NeverCut,
            &mut page,
        )
        .expect("a completed run");
    }
    blank_last_record(&rig, &mut device, &mut page);
    let last = 2 * EFFECTS + 1;
    let breached = Outcome::Breached(Breach::LostAcknowledgedRecord { index: last });
    assert_eq!(
        rig.verify(0, &mut device, &mut page).map(Verdict::outcome),
        Ok(breached),
        "the loss is what verify reports before the resume"
    );

    let mut dispatcher = Log::default();
    let refused = {
        let mut metered = Metered::new(&mut device);
        let before = (metered.wear(), metered.rig_wear());
        let refused = rig.resume(0, &mut metered, &mut dispatcher, &mut page);
        assert_eq!(
            (metered.wear(), metered.rig_wear()),
            before,
            "a refused resume writes nothing"
        );
        refused
    };
    assert!(
        matches!(
            refused,
            Err(RigError::Breach(Breach::LostAcknowledgedRecord { index })) if index == last
        ),
        "{refused:?}"
    );
    assert!(
        dispatcher.entered.is_empty(),
        "a refused resume dispatches nothing"
    );
    assert_eq!(
        rig.verify(0, &mut device, &mut page).map(Verdict::outcome),
        Ok(breached),
        "the loss is still there to be reported"
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
    // Every id, in the table's order: a swapped pair is a row wearing another's name.
    assert_eq!(
        Row::ALL.map(Row::id),
        [
            "during-schedule-frame-write",
            "after-schedule-barrier-before-dispatch",
            "during-physical-activity",
            "after-activity-before-completion-barrier",
            "during-completion-write",
            "after-completion-barrier",
            "during-inactive-bank-erase-or-write",
            "after-new-bank-seal-barrier",
            "history-capacity-reached",
            "replay-divergence",
        ]
    );
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

    // A bank installed for iteration 0 and written by iteration 1: the witness and the
    // prefix are another run's, and the resume refuses before writing, with the breach
    // `verify` reports.
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
        matches!(refused, Err(RigError::Breach(Breach::WitnessUnreadable))),
        "{refused:?}"
    );
    assert_eq!(
        (metered.wear(), metered.rig_wear()),
        before,
        "a refused resume writes nothing"
    );
    assert_eq!(
        rig.verify(0, &mut device, &mut page).map(Verdict::outcome),
        Ok(Outcome::Breached(Breach::WitnessUnreadable))
    );
}
