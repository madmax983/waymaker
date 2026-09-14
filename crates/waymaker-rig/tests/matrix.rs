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
//! All ten rows. Issue [#96](https://github.com/madmax983/waymaker/issues/96) closed the
//! four this rig used to owe. Six come from the ordinary crash injector: the schedule,
//! dispatch and completion rows a rig that cuts during those three writes can land in.
//! Two more come from a swap workload: [`rollover_sweep`] runs a run that rolls over
//! mid-way, through the same injector, and classifies each crash point by which bank
//! [`bank::select`] names afterward. The last two are driven rather than swept —
//! [`row_nine`] and [`row_ten`] each build one crash point by hand, because a capacity
//! refusal and a declared-workflow mismatch are not media crashes the injector produces.
//! [`every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned`] pins all ten
//! counts, so a sweep that quietly thinned fails closed.
//!
//! # How a row is read off a run
//!
//! For the six effect rows: from the witness, the recovered count and the journal's
//! ending — all of which a board has after a reset — plus one thing only the harness
//! has: whether the dispatcher was entered, and whether it returned. A `Dispatched` mark
//! is written *before* the effect, so a mark is not evidence of a dispatch; `tests/sweep.rs`
//! says why. Rows 2, 3 and 4 rest on that evidence, and a board cannot supply it.
//!
//! Row 3 is not a crash point of the injector: it is a run whose dispatcher is entered and
//! does not return, which is the supply going during the effect as the rig sees it. It is
//! credited from those runs, not from the injector's cause.
//!
//! Row 6 is decided by what recovery produced, so a completion seal that landed whole with
//! its barrier refused is in it beside the points that are strictly after the barrier; the
//! model half says the same of rows 2 and 6.
//!
//! For the two bank rows: a swap writes no journal record and marks no witness, so there
//! is no mark to read. [`bank::select`]'s own answer is the whole of it — the old bank
//! authoritative is row 7, the new bank authoritative is row 8, and nothing else about a
//! crash point during the swap's own operations needs to be asked.

use std::cell::RefCell;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_fault::{Device, FaultError, Harness, Injection, Interruption, Op, Progress, Run};
use waymaker_flash::append::Journal;
use waymaker_flash::bank;
use waymaker_flash::capacity::{Bounds, Refusal, Reserve};
use waymaker_flash::frame::input_digest;
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_flash::swap::{Retired, Swap};
use waymaker_rig::audit::Breach;
use waymaker_rig::cutter::{Dispatcher, NeverCut, PlannedCut};
use waymaker_rig::log::Outcome;
use waymaker_rig::matrix::{Matrix, Row};
use waymaker_rig::phase::Phase;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Resumed, Rig, RigError, Stop, Verdict};
use waymaker_rig::wear::Metered;
use waymaker_rig::window::Window;
use waymaker_rig::witness::{Mark, Progress as Marks, Stage, Witness, WitnessError};
use waymaker_rig::workload::{Role, Workload};

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
    let mut recovery = Recovery::new(journal, &mut engine);
    while let Some(step) = recovery.next(page) {
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

// ---------------------------------------------------------------------------------------
// Rows 7 and 8: the bank swap
// ---------------------------------------------------------------------------------------

/// How many effects the retiring run completes before it rolls over. Strictly less than
/// `EFFECTS`, so the swap replaces a run that still had an effect left to schedule rather
/// than one that had already reached its own end.
const ROLLOVER_EFFECTS_BEFORE: u16 = EFFECTS - 1;

/// The input for the run a swap installs. The bank header names it, and the installed
/// bank's own `RunStarted` record names it too — the same bytes in both places, as a real
/// writer would write them.
const NEXT_RUN_INPUT: &[u8; 1] = b"n";

/// The header the swap installs: a fresh run id, distinct from the retiring one.
const fn rollover_next_header(rig: &Rig) -> bank::BankHeader<'static> {
    bank::BankHeader {
        run: waymaker_core::RunId(rig.workload(0).run().0 ^ 1),
        align: rig.layout().align(),
        workflow_kind: Workload::WORKFLOW_KIND,
        workflow_version: Workload::WORKFLOW_VERSION,
        input_schema: 0,
        input: NEXT_RUN_INPUT,
    }
}

/// Writes the retiring run's opening records: `RunStarted` and
/// [`ROLLOVER_EFFECTS_BEFORE`] schedule/completion pairs, stopping there.
fn drive_rollover_prefix(session: &mut waymaker_fault::Session) -> Result<(), String> {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut dispatcher = Log::default();
    let mut metered = Metered::new(session);
    rig.prepare(&mut metered, 0, &mut page)
        .map_err(|error| format!("prepare: {error:?}"))?;
    rig.iterate_until_rollover(
        0,
        &mut metered,
        &mut dispatcher,
        &mut page,
        ROLLOVER_EFFECTS_BEFORE,
    )
    .map_err(|error| format!("iterate_until_rollover: {error:?}"))
}

/// §10's seven-step swap from the retiring bank into the other one, driven directly the
/// way [`row_nine`]'s explicit exit is — all seven steps, [`Installed::reclaim`] included.
/// Returns the engine window and the installed bank's journal, positioned after whatever
/// [`Journal::after`] found there. The window still borrows `session`, so the caller can
/// keep writing into the bank it installed.
///
/// Review found this stopping at step 6, because [`Installed::recovery`] and
/// [`Installed::reclaim`] both consume the value `commit` returns and a caller can only
/// take one — so the crash injector never produced a point during the retiring bank's own
/// erase or its barrier, even though row 8 held authoritative throughout it exactly as it
/// does after `commit`. Reclaiming first and then re-deriving the installed bank's journal
/// region by hand — the same read [`iterate_until_rollover_and_iterate_reserved_refuse_a_bank_a_swap_moved_past`]
/// already does — gets both: the erase is under the injector, and the caller still gets a
/// writer for the bank it installed.
///
/// [`Installed::recovery`]: waymaker_flash::swap::Installed::recovery
/// [`Installed::reclaim`]: waymaker_flash::swap::Installed::reclaim
fn drive_rollover_swap(
    session: &mut waymaker_fault::Session,
) -> Result<(Window<'_, waymaker_fault::Session>, Journal), String> {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let layout = rig.layout();
    let booted = bank::Authority::Bank {
        id: Rig::BANK,
        generation: Rig::GENERATION,
    };
    let next = rollover_next_header(&rig);
    let Ok(mut engine) = Window::new(session, 0, layout.geometry().capacity()) else {
        return Err("engine window".to_string());
    };
    let region = try_bank_region(&rig, Rig::BANK, &mut engine, &mut page)?;
    let mut recovery = Recovery::new(region, &mut engine);
    while let Some(step) = recovery.next(&mut page) {
        step.map_err(|error| format!("recovery: {error:?}"))?;
    }
    let swap = Swap::beginning(
        layout,
        booted,
        rig.workload(0).run(),
        Retired::Recovery(recovery),
        next,
    )
    .map_err(|error| format!("swap plan: {error:?}"))?;
    let prepared = swap
        .prepare(&mut engine)
        .map_err(|error| format!("swap prepare: {error:?}"))?;
    let mut header_page = [0_u8; Rig::PAGE_BYTES];
    let staged = prepared
        .stage(&mut header_page)
        .map_err(|error| format!("swap stage: {error:?}"))?;
    let sealable = staged
        .payload_barrier()
        .map_err(|error| format!("swap barrier: {error:?}"))?;
    let installed = sealable
        .commit()
        .map_err(|error| format!("swap commit: {error:?}"))?;
    installed
        .reclaim()
        .map_err(|error| format!("swap reclaim: {error:?}"))?;
    let new_region = try_bank_region(&rig, Rig::BANK.other(), &mut engine, &mut page)?;
    let mut new_recovery = Recovery::new(new_region, &mut engine);
    while let Some(step) = new_recovery.next(&mut page) {
        step.map_err(|error| format!("new recovery: {error:?}"))?;
    }
    let Some(new_journal) = Journal::after(new_recovery) else {
        return Err("new journal not extendable".to_string());
    };
    Ok((engine, new_journal))
}

/// The whole combined run: the retiring prefix, the swap, and a small complete run
/// written into the bank it installs.
fn drive_rollover(session: &mut waymaker_fault::Session) -> Result<(), String> {
    drive_rollover_prefix(session)?;
    let (mut engine, mut new_journal) = drive_rollover_swap(session)?;
    try_write_tiny_run(&mut engine, &mut new_journal)?;
    Ok(())
}

/// The tiny run [`write_tiny_run`] writes, as records, in order.
const fn tiny_run_records(input: &[u8; 1]) -> [RecordRef<'_>; 4] {
    [
        RecordRef::RunStarted {
            workflow_kind: Workload::WORKFLOW_KIND,
            workflow_version: Workload::WORKFLOW_VERSION,
            input,
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq(0),
            kind: ActivityKind(1),
            input_len: 1,
            input_crc: input_digest(input),
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"ok",
        },
        RecordRef::RunCompleted { result: b"done" },
    ]
}

/// Continues [`write_tiny_run`]'s run from whatever prefix `recovered` already covers.
/// Returns the effects dispatched by this call.
fn continue_tiny_run<S: StableStorage>(
    engine: &mut Window<'_, S>,
    journal: &mut Journal,
    recovered: u16,
) -> Vec<u16>
where
    S::Error: core::fmt::Debug,
{
    let input = NEXT_RUN_INPUT;
    let records = tiny_run_records(input);
    let mut dispatcher = Log::default();

    // The schedule (index 1) is durable and its completion (index 2) is not: the effect
    // is outstanding, and it is redelivered under the same index before anything else is
    // written — the redelivery `resume_as` requires of a real resume. Without this, the
    // loop below would skip both the write *and* the dispatch at index 1 and go straight
    // to writing the completion at index 2, reporting an effect as done that this call
    // never ran.
    if recovered == 2 {
        let Ok(()) = dispatcher.dispatch(0, input) else {
            unreachable!("the new run's own dispatcher accepts effect 0")
        };
    }

    for (index, record) in records.iter().enumerate() {
        let Ok(index) = u16::try_from(index) else {
            unreachable!("four records index in a u16")
        };
        if index < recovered {
            continue;
        }
        write_record(engine, journal, record);
        if index == 1 {
            let Ok(()) = dispatcher.dispatch(0, input) else {
                unreachable!("the new run's own dispatcher accepts effect 0")
            };
        }
    }
    dispatcher.entered
}

/// Which bank is authoritative, read the way [`Rig::verify`] reads it.
fn authority_of(rig: &Rig, device: &mut Device, page: &mut [u8]) -> bank::Authority {
    let layout = rig.layout();
    let Ok(mut engine) = Window::new(device, 0, layout.geometry().capacity()) else {
        unreachable!("the engine window")
    };
    let mut generations = [None, None];
    for (slot, id) in generations
        .iter_mut()
        .zip([bank::BankId::A, bank::BankId::B])
    {
        let region = layout.bank(id);
        let Some(want) = usize::try_from(region.payload_bytes())
            .ok()
            .map(|want| want.min(page.len()))
        else {
            unreachable!("a header fits a page")
        };
        let Some(header_slot) = page.get_mut(..want) else {
            unreachable!("a header fits a page")
        };
        let Ok(()) = engine.read(region.base(), header_slot) else {
            unreachable!("a readable bank")
        };
        let mut seal = [0_u8; Rig::MAX_PROGRAM_BYTES as usize];
        let Ok(seal_len) = usize::try_from(region.seal_bytes()) else {
            unreachable!("a seal fits its own width")
        };
        let Some(seal_slot) = seal.get_mut(..seal_len) else {
            unreachable!("a seal fits its own width")
        };
        let Ok(()) = engine.read(region.seal_offset(), seal_slot) else {
            unreachable!("a readable seal")
        };
        let Some(header_bytes) = page.get(..want) else {
            unreachable!("a header fits a page")
        };
        *slot = bank::sealed_generation(header_bytes, seal_slot);
    }
    bank::select(generations)
}

/// Which row a crash point of the combined sequence is an instance of, or `None` for a
/// point before the swap's own operations began — rows 1 to 6 already cover those.
fn classify_rollover(
    injection: Injection,
    swap_start: usize,
    rig: &Rig,
    device: &mut Device,
    page: &mut [u8],
) -> Option<Row> {
    if injection.op < swap_start {
        return None;
    }
    match authority_of(rig, device, page) {
        bank::Authority::Bank { id, .. } if id == Rig::BANK => {
            Some(Row::DuringInactiveBankEraseOrWrite)
        }
        bank::Authority::Bank { .. } => Some(Row::AfterNewBankSealBarrier),
        bank::Authority::Unsealed | bank::Authority::Ambiguous { .. } => {
            unreachable!(
                "the installed bank's own seal is untouched by reclaiming the retiring one, \
                 so exactly one bank is always a candidate even while that erase is under way"
            )
        }
    }
}

/// Every crash point of the combined run, classified into row 7 or row 8.
fn rollover_sweep() -> Vec<(Injection, Row)> {
    let rig = rig();
    let geometry = geometry();
    let harness = Harness::new(geometry);

    let Ok(prefix_only) = harness.run_fault_free(drive_rollover_prefix) else {
        unreachable!("the rollover prefix completes fault-free")
    };
    let swap_start = prefix_only.ops().len();

    let Ok(runs) = harness.run(|session| drive_rollover(session).map_err(|_| ())) else {
        unreachable!("the fault-free rollover completes")
    };
    let mut points = Vec::new();
    for run in &runs {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let mut device = device_after(run);
        if let Some(row) = classify_rollover(injection, swap_start, &rig, &mut device, &mut page) {
            points.push((injection, row));
        }
    }
    points
}

#[test]
fn during_inactive_bank_erase_or_write_the_old_bank_remains_authoritative_and_the_old_run_continues_on_the_rig()
 {
    let retiring: Vec<Injection> = rollover_sweep()
        .into_iter()
        .filter_map(|(injection, row)| {
            (row == Row::DuringInactiveBankEraseOrWrite).then_some(injection)
        })
        .collect();
    assert!(
        !retiring.is_empty(),
        "no crash point left the retiring bank authoritative"
    );
    let rig = rig();
    for injection in retiring {
        let Ok(cut) = Harness::new(geometry())
            .run_one(injection, |session| drive_rollover(session).map_err(|_| ()))
        else {
            unreachable!("a deterministic crash point, at {injection:?}")
        };
        let mut device = device_after(&cut);
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let Ok(verdict) = rig.verify(0, &mut device, &mut page) else {
            unreachable!("a judgeable part, at {injection:?}")
        };
        assert_eq!(
            verdict.outcome(),
            Outcome::Passed,
            "the retiring bank's own history is intact, at {injection:?}"
        );
        let mut metered = Metered::new(&mut device);
        let mut dispatcher = Log::default();
        let resumed = rig.resume(0, &mut metered, &mut dispatcher, &mut page);
        assert!(
            matches!(resumed, Ok(Resumed::Completed { .. })),
            "the old run continues, at {injection:?}: {resumed:?}"
        );
    }
}

#[test]
fn after_new_bank_seal_barrier_the_new_bank_is_authoritative_and_the_old_run_is_never_current_again_on_the_rig()
 {
    let installed: Vec<Injection> = rollover_sweep()
        .into_iter()
        .filter_map(|(injection, row)| (row == Row::AfterNewBankSealBarrier).then_some(injection))
        .collect();
    assert!(
        !installed.is_empty(),
        "no crash point left the new bank authoritative"
    );
    let rig = rig();
    for injection in installed {
        let Ok(cut) = Harness::new(geometry())
            .run_one(injection, |session| drive_rollover(session).map_err(|_| ()))
        else {
            unreachable!("a deterministic crash point, at {injection:?}")
        };
        let mut device = device_after(&cut);
        let mut page = [0_u8; Rig::PAGE_BYTES];

        // The old run is never current again: it is not this rig's authoritative bank.
        {
            let mut metered = Metered::new(&mut device);
            let mut dispatcher = Log::default();
            let resumed = rig.resume(0, &mut metered, &mut dispatcher, &mut page);
            assert!(
                matches!(resumed, Err(RigError::Bank)),
                "the retiring run answered as current, at {injection:?}: {resumed:?}"
            );
        }

        // Retired is not lost: the old bank's own witness-claimed history is still exactly
        // what it was before the swap, and `verify` must say so rather than reading "not
        // current any more" as "records went missing".
        {
            let Ok(verdict) = rig.verify(0, &mut device, &mut page) else {
                unreachable!("a judgeable part, at {injection:?}")
            };
            assert_eq!(
                verdict.outcome(),
                Outcome::Passed,
                "the retired bank's own history was reported lost, at {injection:?}"
            );
        }

        // The new bank is authoritative. Where its own journal still has an append point
        // — every case but a torn first frame, which ADR 0018 refuses rather than
        // repairs, the same as any other bank — it starts and does work.
        let layout = rig.layout();
        let Ok(mut engine) = Window::new(&mut device, 0, layout.geometry().capacity()) else {
            unreachable!("the engine window")
        };
        let region = bank_region(&rig, Rig::BANK.other(), &mut engine, &mut page);
        let mut recovery = Recovery::new(region, &mut engine);
        let mut recovered = 0_u16;
        while let Some(step) = recovery.next(&mut page) {
            match step {
                Ok(_) => recovered = recovered.saturating_add(1),
                Err(_) => break,
            }
        }
        if let Some(mut journal) = Journal::after(recovery) {
            let ran = continue_tiny_run(&mut engine, &mut journal, recovered);
            // Below `recovered == 3` the effect's completion is not yet durable, so this
            // call owes it a dispatch — either fresh or redelivered. At `recovered >= 3`
            // the completion already landed in an earlier attempt, and this call owes it
            // nothing: an empty `ran` there is correct, not vacuous.
            assert!(
                recovered >= 3 || !ran.is_empty(),
                "the new run's effect was never dispatched, at {injection:?} \
                 (recovered={recovered})"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// Row 9: history capacity reached
// ---------------------------------------------------------------------------------------

/// §10's exits, priced for a run whose records are at most
/// [`Workload::MAX_PAYLOAD_BYTES`] wide. `tail` covers the outcome and terminal record.
/// This bound is wider than any real record. The reserve never spends the room on a real
/// payload — only on its own floor.
// Guards the literal `bounds` uses below: a `usize as u16` cast cannot prove at compile
// time that it fits, so the width is asserted here instead of cast there.
const _: () = assert!(Workload::MAX_PAYLOAD_BYTES == 16);

const fn bounds(tail: u16) -> Bounds {
    Bounds {
        run_input_bytes: 16,
        effect_result_bytes: tail,
        terminal_bytes: tail,
    }
}

/// A declared tail wide enough that the reserve refuses the second effect's schedule once
/// the first effect has completed, on the rig's own standard fixture.
///
/// This searches for the value rather than hard-coding one. The number comes from the
/// reserve's own arithmetic on real records, not from a figure copied out of a passing run.
fn near_capacity() -> (Rig, Reserve, Device) {
    for tail in [
        32_u16, 48, 64, 96, 128, 160, 192, 224, 256, 300, 350, 400, 450, 500,
    ] {
        let rig = rig();
        let Ok(reserve) = Reserve::for_layout(bounds(tail), rig.layout()) else {
            continue;
        };
        let mut device = Device::new(geometry());
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let entered = {
            let mut metered = Metered::new(&mut device);
            if rig.prepare(&mut metered, 0, &mut page).is_err() {
                continue;
            }
            let mut dispatcher = Log::default();
            let outcome =
                rig.iterate_reserved(0, &mut metered, &mut dispatcher, reserve, &mut page);
            if !matches!(outcome, Err(RigError::Capacity(Refusal::NearCapacity))) {
                continue;
            }
            dispatcher.entered
        };
        if entered == [0] {
            return (rig, reserve, device);
        }
    }
    unreachable!("no declared tail in the search fills after exactly one effect")
}

/// The refusal [`near_capacity`] finds writes no witness mark of its own for the record
/// it refuses.
///
/// [`assert_replay_refuses_without_mutation`] cannot see this: it checks a *replay*, and a
/// replay's witness continuation skips any mark the first attempt already wrote, which is
/// exactly what would hide one written just before that attempt's own refusal. This
/// compares the first attempt's rig-only wear against a device that legitimately stopped
/// after the same one-effect prefix and never met a refusal at all — equal wear is the
/// only way the refused record's mark was never programmed.
#[test]
fn the_first_capacity_refusal_writes_no_mark_of_its_own() {
    let mut found = None;
    for tail in [
        32_u16, 48, 64, 96, 128, 160, 192, 224, 256, 300, 350, 400, 450, 500,
    ] {
        let rig = rig();
        let Ok(reserve) = Reserve::for_layout(bounds(tail), rig.layout()) else {
            continue;
        };
        let mut device = Device::new(geometry());
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let refused_rig_wear = {
            let mut metered = Metered::new(&mut device);
            if rig.prepare(&mut metered, 0, &mut page).is_err() {
                continue;
            }
            let mut dispatcher = Log::default();
            let outcome =
                rig.iterate_reserved(0, &mut metered, &mut dispatcher, reserve, &mut page);
            if !matches!(outcome, Err(RigError::Capacity(Refusal::NearCapacity))) {
                continue;
            }
            if dispatcher.entered != [0] {
                continue;
            }
            metered.rig_wear()
        };
        found = Some((rig, refused_rig_wear));
        break;
    }
    let Some((rig, refused_rig_wear)) = found else {
        unreachable!("no declared tail in the search fills after exactly one effect")
    };

    let mut clean = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let clean_rig_wear = {
        let mut metered = Metered::new(&mut clean);
        let Ok(()) = rig.prepare(&mut metered, 0, &mut page) else {
            unreachable!("the same fixture prepares")
        };
        let mut dispatcher = Log::default();
        let Ok(()) = rig.iterate_until_rollover(0, &mut metered, &mut dispatcher, &mut page, 1)
        else {
            unreachable!("one full effect, driven without a reserve, does not refuse")
        };
        metered.rig_wear()
    };

    assert_eq!(
        refused_rig_wear, clean_rig_wear,
        "the refused record's own witness mark was written before the refusal"
    );
}

/// `bank`'s journal region, read the way a boot reads it. Fails rather than panicking, for
/// a caller driven by the crash injector — a read this close to a fault point can fail
/// like any other storage call.
fn try_bank_region<S: StableStorage>(
    rig: &Rig,
    bank: bank::BankId,
    engine: &mut Window<'_, S>,
    page: &mut [u8],
) -> Result<JournalRegion, String> {
    let layout = rig.layout();
    let region = layout.bank(bank);
    let Some(want) = usize::try_from(region.payload_bytes())
        .ok()
        .map(|want| want.min(page.len()))
    else {
        return Err("a header does not fit a page".to_string());
    };
    let Some(slot) = page.get_mut(..want) else {
        return Err("a header does not fit a page".to_string());
    };
    engine
        .read(region.base(), slot)
        .map_err(|_| "bank read".to_string())?;
    let Some(bytes) = page.get(..want) else {
        return Err("a header does not fit a page".to_string());
    };
    let header = bank::decode_header(bytes).map_err(|_| "an unreadable bank header".to_string())?;
    JournalRegion::of(layout, bank, &header).map_err(|error| format!("journal region: {error:?}"))
}

/// `bank`'s journal region, read the way a boot reads it.
///
/// For a caller inspecting a part the injector has already stopped: every read here is
/// against media that is no longer changing, so a failure is a bug rather than a crash
/// point. See [`try_bank_region`] for the fallible twin a live writer needs.
fn bank_region<S: StableStorage>(
    rig: &Rig,
    bank: bank::BankId,
    engine: &mut Window<'_, S>,
    page: &mut [u8],
) -> JournalRegion {
    let Ok(region) = try_bank_region(rig, bank, engine, page) else {
        unreachable!("a readable, already-crashed bank")
    };
    region
}

/// Bank A's journal region, read the way a boot reads it.
fn bank_a_region(rig: &Rig, device: &mut Device, page: &mut [u8]) -> JournalRegion {
    let Ok(mut engine) = Window::new(device, 0, rig.layout().geometry().capacity()) else {
        unreachable!("the engine window")
    };
    bank_region(rig, Rig::BANK, &mut engine, page)
}

/// Writes one record with §07's two-barrier protocol, the way [`Rig`] itself does. Fails
/// rather than panicking, for a caller driven by the crash injector.
fn try_write_record<S: StableStorage>(
    engine: &mut Window<'_, S>,
    journal: &mut Journal,
    record: &RecordRef<'_>,
) -> Result<(), String>
where
    S::Error: core::fmt::Debug,
{
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let staged = journal
        .stage(engine, record, &mut page)
        .map_err(|error| format!("stage: {error:?}"))?;
    let sealable = staged
        .payload_barrier()
        .map_err(|error| format!("payload barrier: {error:?}"))?;
    sealable
        .commit()
        .map_err(|error| format!("commit: {error:?}"))?;
    Ok(())
}

/// Writes one record with §07's two-barrier protocol, the way [`Rig`] itself does.
///
/// For a caller writing into an already-crashed, no-longer-injected device: every step
/// here is fault-free, so a failure is a bug rather than a crash point. See
/// [`try_write_record`] for the fallible twin a live writer needs.
fn write_record<S: StableStorage>(
    engine: &mut Window<'_, S>,
    journal: &mut Journal,
    record: &RecordRef<'_>,
) where
    S::Error: core::fmt::Debug,
{
    let Ok(()) = try_write_record(engine, journal, record) else {
        unreachable!("a fault-free record write")
    };
}

/// Replays the near-capacity run and requires the same refusal, having read, programmed
/// and barriered nothing for it.
fn assert_replay_refuses_without_mutation(
    rig: &Rig,
    reserve: Reserve,
    device: &mut Device,
    page: &mut [u8],
) {
    let mut metered = Metered::new(device);
    let before = (metered.wear(), metered.rig_wear());
    let mut dispatcher = Log::default();
    let resumed = rig.resume_reserved(0, &mut metered, &mut dispatcher, reserve, page);
    assert_eq!(
        before,
        (metered.wear(), metered.rig_wear()),
        "the refusal touched the device"
    );
    assert!(
        matches!(resumed, Err(RigError::Capacity(Refusal::NearCapacity))),
        "{resumed:?}"
    );
    assert!(
        dispatcher.entered.is_empty(),
        "the refused effect was dispatched again"
    );
}

/// Writes one complete, one-effect run into `journal`: `RunStarted`, a schedule and
/// completion for effect 0, and `RunCompleted`. Fails rather than panicking, for a caller
/// driven by the crash injector. Returns the effects it dispatched.
fn try_write_tiny_run<S: StableStorage>(
    engine: &mut Window<'_, S>,
    journal: &mut Journal,
) -> Result<Vec<u16>, String>
where
    S::Error: core::fmt::Debug,
{
    let input = NEXT_RUN_INPUT;
    let records = tiny_run_records(input);
    let mut dispatcher = Log::default();
    for (index, record) in records.iter().enumerate() {
        try_write_record(engine, journal, record)?;
        if index == 1 {
            dispatcher
                .dispatch(0, input)
                .map_err(|error| format!("dispatch: {error:?}"))?;
        }
    }
    Ok(dispatcher.entered)
}

/// Writes one complete, one-effect run into `journal`, as [`try_write_tiny_run`] does.
///
/// For a caller writing into an already-crashed, no-longer-injected device. See
/// [`try_write_tiny_run`] for the fallible twin a live writer needs.
fn write_tiny_run<S: StableStorage>(engine: &mut Window<'_, S>, journal: &mut Journal) -> Vec<u16>
where
    S::Error: core::fmt::Debug,
{
    let Ok(entered) = try_write_tiny_run(engine, journal) else {
        unreachable!("a fault-free tiny run")
    };
    entered
}

/// §10's explicit exit from the near-capacity state: a swap into the other bank, and a
/// small complete run written into it. Returns the effects the new run dispatched.
fn explicit_rollover(rig: &Rig, device: &mut Device, page: &mut [u8]) -> Vec<u16> {
    let layout = rig.layout();
    let booted = bank::Authority::Bank {
        id: Rig::BANK,
        generation: Rig::GENERATION,
    };
    let next = rollover_next_header(rig);
    let region = bank_a_region(rig, device, page);
    let Ok(mut engine) = Window::new(device, 0, layout.geometry().capacity()) else {
        unreachable!("the engine window")
    };
    let mut recovery = Recovery::new(region, &mut engine);
    while let Some(step) = recovery.next(page) {
        if step.is_err() {
            unreachable!("bank A's journal is whole up to its last completed effect")
        }
    }
    let Ok(swap) = Swap::beginning(
        layout,
        booted,
        rig.workload(0).run(),
        Retired::Recovery(recovery),
        next,
    ) else {
        unreachable!("a swap can be planned from the near-capacity state")
    };
    let Ok(prepared) = swap.prepare(&mut engine) else {
        unreachable!("a fault-free erase and barrier")
    };
    let mut header_page = [0_u8; Rig::PAGE_BYTES];
    let Ok(staged) = prepared.stage(&mut header_page) else {
        unreachable!("the header and its seal fit a page")
    };
    let Ok(sealable) = staged.payload_barrier() else {
        unreachable!("a fault-free barrier")
    };
    let Ok(installed) = sealable.commit() else {
        unreachable!("a fault-free commit")
    };
    assert_eq!(
        installed.authority(),
        bank::Authority::Bank {
            id: Rig::BANK.other(),
            generation: bank::Generation(2),
        },
        "the new bank is authoritative, at the next generation, and the old run is retired"
    );

    let mut new_recovery = installed.recovery();
    while let Some(step) = new_recovery.next(page) {
        if step.is_err() {
            unreachable!("a freshly installed bank scans clean")
        }
    }
    let Some(mut new_journal) = Journal::after(new_recovery) else {
        unreachable!("a freshly installed bank is a clean, extendable journal")
    };
    write_tiny_run(&mut engine, &mut new_journal)
}

/// Row 9, driven: no mutation on a replayed refusal, and an explicit swap past it that
/// starts a new run and dispatches its first effect. Returns the row it credits.
fn row_nine() -> Row {
    let (rig, reserve, mut device) = near_capacity();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    assert_replay_refuses_without_mutation(&rig, reserve, &mut device, &mut page);
    let dispatched = explicit_rollover(&rig, &mut device, &mut page);
    assert_eq!(dispatched, [0], "the new run starts and does work");
    Row::HistoryCapacityReached
}

#[test]
fn history_capacity_reached_is_a_capacity_error_with_no_mutation_or_an_explicit_continue_as_new() {
    assert_eq!(row_nine(), Row::HistoryCapacityReached);
}

// ---------------------------------------------------------------------------------------
// Row 10: replay divergence
// ---------------------------------------------------------------------------------------

/// Row 10, driven. Take every crash point where the second effect's schedule is
/// recoverable and its completion is not. A declared workload naming a different activity
/// for that schedule is refused there, twice in a row. Nothing is dispatched and nothing
/// is rewritten. Returns the row it credits.
///
/// Not swept through [`classified`]. That function resumes each point as it classifies
/// it, so a resumed device is no longer the crash image this test needs. This runs its own
/// pass over the harness instead, and stops at [`evidence`], which only reads the device —
/// so every device checked here stays untouched.
fn row_ten() -> Row {
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
    let rig = rig();
    let Some(diverging_at) = rig.workload(0).schedule_index(1) else {
        unreachable!("a run of two effects schedules a second one")
    };
    let declared = rig.workload(0).diverging(diverging_at);

    let mut checked = 0_usize;
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut device = device_after(run);
        let Ok(evidence) = evidence(&rig, &mut device, entered, true) else {
            continue;
        };
        // The sharpest case row 10's model half names: the second effect's schedule is
        // recovered, its completion is not, so the effect is outstanding when the
        // declared workflow stops agreeing with history.
        if !matches!(
            (evidence.attempted, evidence.activity, evidence.recovered_it),
            (Role::Schedule(1), Activity::NotEntered, true)
        ) {
            continue;
        }
        let at = format!("{injection:?}");
        for attempt in 0_u8..2 {
            let before = device.image().to_vec();
            let mut page = [0_u8; Rig::PAGE_BYTES];
            let mut dispatcher = Log::default();
            let outcome = {
                let mut metered = Metered::new(&mut device);
                rig.resume_declaring(0, declared, &mut metered, &mut dispatcher, &mut page)
            };
            assert!(
                matches!(
                    outcome,
                    Err(RigError::Breach(Breach::RecordDiffers { index }))
                        if index == diverging_at
                ),
                "attempt {attempt} at {at}: {outcome:?}"
            );
            assert!(
                dispatcher.entered.is_empty(),
                "executed past a divergence, attempt {attempt} at {at}"
            );
            assert_eq!(
                device.image(),
                before.as_slice(),
                "history was reinterpreted, attempt {attempt} at {at}"
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "no crash point left the second effect's schedule outstanding"
    );
    Row::ReplayDivergence
}

#[test]
fn replay_divergence_is_a_deterministic_fault_with_no_further_execution_and_history_untouched() {
    assert_eq!(row_ten(), Row::ReplayDivergence);
}

#[test]
fn every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned() {
    // All ten rows, filled: the six effect rows swept through the ordinary crash
    // injector, the two bank rows swept through the swap workload, and the two driven
    // rows — history capacity and replay divergence — credited once each by their own
    // assertions. Issue #96 closes the gap `the_rig_fills_six_rows_and_names_the_seventh_as_its_gap`
    // used to pin.
    let (points, _) = classified();
    let mut matrix = Matrix::EMPTY;
    for point in points.iter().chain(&row_three()) {
        matrix = matrix.record(point.row);
    }
    for (_, row) in rollover_sweep() {
        matrix = matrix.record(row);
    }
    for row in [row_nine(), row_ten()] {
        matrix = matrix.record(row);
    }
    matrix
        .verdict()
        .expect("every row of §14's table is reached on the rig");
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
            (Row::DuringInactiveBankEraseOrWrite, 61),
            (Row::AfterNewBankSealBarrier, 175),
            (Row::HistoryCapacityReached, 1),
            (Row::ReplayDivergence, 1),
        ]
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
        let mut recovery = Recovery::new(journal, &mut engine);
        let mut start = None;
        loop {
            let at = recovery.offset();
            match recovery.next(page) {
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

    // A bank installed for iteration 0, but a witness mark that belongs to iteration 1:
    // the resume refuses before writing, with the breach `verify` reports. `Rig::iterate`
    // no longer lets a mismatched iteration reach the witness at all — see
    // `iterate_and_its_siblings_refuse_a_bank_installed_for_another_iteration` — so this
    // plants the mark directly, through the same low-level API `Rig::iterate` itself uses,
    // to exercise `resume`'s own protection against a witness that answers a different run.
    let mut device = Device::new(geometry());
    {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("a prepared part");
    }
    {
        let Ok(mut instrument) =
            Window::new(&mut device, rig.instrument_base(), geometry().erase_size())
        else {
            unreachable!("the instrument window")
        };
        let mut witness = Witness::new(rig.witness_region());
        witness
            .mark(
                &mut instrument,
                Mark::new(1, 0, Stage::Attempted),
                &mut page,
            )
            .expect("a fault-free mark");
    }
    let mut metered = Metered::new(&mut device);
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

/// `resume_reserved` refuses an outstanding effect's own completion without redelivering the
/// effect first.
///
/// A reserve is passed in at resume time and need not be the one the prefix was written
/// under — the prefix here is written ungated, by the plain [`Rig::iterate`], cut at effect
/// 1's own dispatch so its schedule is durable and its completion is not. The reserve handed
/// to `resume_reserved` then declares a zero-width effect-result bound, which refuses *any*
/// real completion (never empty; see [`Workload::MAX_PAYLOAD_BYTES`]) regardless of how much
/// room the journal has left — deterministic, and independent of the shared fixture's small
/// geometry, unlike a room-based refusal.
#[test]
fn resume_reserved_refuses_before_redelivering_an_effect_its_own_completion_cannot_hold() {
    let mut seed = None;
    for candidate in 0_u64..200_000 {
        let cut = Plan::new(candidate).cut(0);
        if cut.phase() == Phase::Dispatch && cut.effect_index(EFFECTS) == 1 {
            seed = Some(candidate);
            break;
        }
    }
    let Some(seed) = seed else {
        unreachable!("some seed cuts at dispatch for effect 1")
    };
    let Ok(custom_rig) = Rig::new::<FaultError>(geometry(), Plan::new(seed), EFFECTS) else {
        unreachable!("the shared geometry holds two banks and a witness")
    };

    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    {
        let mut metered = Metered::new(&mut device);
        let Ok(()) = custom_rig.prepare(&mut metered, 0, &mut page) else {
            unreachable!("prepare")
        };
        let mut dispatcher = Log::default();
        let mut cutter = PlannedCut::at(custom_rig.cut_at(0), EFFECTS);
        let outcome = custom_rig.iterate(0, &mut metered, &mut dispatcher, &mut cutter, &mut page);
        assert!(
            matches!(
                outcome,
                Ok(Stop::Cut {
                    phase: Phase::Dispatch,
                    effect: 1
                })
            ),
            "{outcome:?}"
        );
        assert_eq!(
            dispatcher.entered,
            [0],
            "effect 0 completed and effect 1 was not yet dispatched"
        );
    }

    let Ok(reserve) = Reserve::for_layout(
        Bounds {
            run_input_bytes: 16,
            effect_result_bytes: 0,
            terminal_bytes: 16,
        },
        custom_rig.layout(),
    ) else {
        unreachable!("a zero-width bound is never harder to satisfy than a real one")
    };
    let mut metered = Metered::new(&mut device);
    let mut dispatcher = Log::default();
    let resumed = custom_rig.resume_reserved(0, &mut metered, &mut dispatcher, reserve, &mut page);
    assert!(
        matches!(resumed, Err(RigError::Capacity(Refusal::OverDeclaredBound))),
        "{resumed:?}"
    );
    assert!(
        dispatcher.entered.is_empty(),
        "the outstanding effect was redelivered despite the refusal: {:?}",
        dispatcher.entered
    );
}

/// `verify` does not report a reclaimed, superseded bank as one that lost its acknowledged
/// records.
///
/// Distinct from [`after_new_bank_seal_barrier_the_new_bank_is_authoritative_and_the_old_run_is_never_current_again_on_the_rig`]'s
/// own `verify` check: that one crashes mid-swap and leaves the retiring bank intact, which
/// `own_bank_journal` reads directly regardless of authority. This one drives the swap to
/// completion and past it — §10 step 7, [`Installed::reclaim`] — so the retiring bank is
/// erased media and `own_bank_journal` has nothing to read at all. `judge` still has to tell
/// that apart from a part this run was never installed on.
///
/// [`Installed::reclaim`]: waymaker_flash::swap::Installed::reclaim
#[test]
fn verify_does_not_report_a_reclaimed_bank_as_having_lost_its_acknowledged_records() {
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    {
        let mut metered = Metered::new(&mut device);
        let Ok(()) = rig.prepare(&mut metered, 0, &mut page) else {
            unreachable!("prepare")
        };
        let mut dispatcher = Log::default();
        let Ok(()) = rig.iterate_until_rollover(
            0,
            &mut metered,
            &mut dispatcher,
            &mut page,
            ROLLOVER_EFFECTS_BEFORE,
        ) else {
            unreachable!("the fault-free prefix writes cleanly")
        };
    }

    let layout = rig.layout();
    let booted = bank::Authority::Bank {
        id: Rig::BANK,
        generation: Rig::GENERATION,
    };
    let next = rollover_next_header(&rig);
    let region = bank_a_region(&rig, &mut device, &mut page);
    let Ok(mut engine) = Window::new(&mut device, 0, layout.geometry().capacity()) else {
        unreachable!("the engine window")
    };
    let mut recovery = Recovery::new(region, &mut engine);
    while let Some(step) = recovery.next(&mut page) {
        if step.is_err() {
            unreachable!("bank A's journal is whole up to its last completed effect")
        }
    }
    let Ok(swap) = Swap::beginning(
        layout,
        booted,
        rig.workload(0).run(),
        Retired::Recovery(recovery),
        next,
    ) else {
        unreachable!("a swap can be planned from the rolled-over prefix")
    };
    let Ok(prepared) = swap.prepare(&mut engine) else {
        unreachable!("a fault-free erase and barrier")
    };
    let mut header_page = [0_u8; Rig::PAGE_BYTES];
    let Ok(staged) = prepared.stage(&mut header_page) else {
        unreachable!("the header and its seal fit a page")
    };
    let Ok(sealable) = staged.payload_barrier() else {
        unreachable!("a fault-free barrier")
    };
    let Ok(installed) = sealable.commit() else {
        unreachable!("a fault-free commit")
    };
    let Ok(()) = installed.reclaim() else {
        unreachable!("a fault-free erase and barrier")
    };

    let Ok(verdict) = rig.verify(0, &mut device, &mut page) else {
        unreachable!("a judgeable part")
    };
    assert_eq!(
        verdict.outcome(),
        Outcome::Passed,
        "the reclaimed bank's superseded history was reported lost: {verdict:?}"
    );
}

/// `iterate_until_rollover` and `iterate_reserved` refuse rather than append into a bank a
/// swap has already moved authority away from.
///
/// Before this, both checked only that *some one* bank was authoritative, so calling either
/// again on a device a swap had already moved past would read and append into the retired
/// bank instead of refusing it.
#[test]
fn iterate_until_rollover_and_iterate_reserved_refuse_a_bank_a_swap_moved_past() {
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    {
        let mut metered = Metered::new(&mut device);
        let Ok(()) = rig.prepare(&mut metered, 0, &mut page) else {
            unreachable!("prepare")
        };
        let mut dispatcher = Log::default();
        let Ok(()) = rig.iterate_until_rollover(
            0,
            &mut metered,
            &mut dispatcher,
            &mut page,
            ROLLOVER_EFFECTS_BEFORE,
        ) else {
            unreachable!("the fault-free prefix writes cleanly")
        };
    }

    let layout = rig.layout();
    let booted = bank::Authority::Bank {
        id: Rig::BANK,
        generation: Rig::GENERATION,
    };
    let next = rollover_next_header(&rig);
    let region = bank_a_region(&rig, &mut device, &mut page);
    let Ok(mut engine) = Window::new(&mut device, 0, layout.geometry().capacity()) else {
        unreachable!("the engine window")
    };
    let mut recovery = Recovery::new(region, &mut engine);
    while let Some(step) = recovery.next(&mut page) {
        if step.is_err() {
            unreachable!("bank A's journal is whole up to its last completed effect")
        }
    }
    let Ok(swap) = Swap::beginning(
        layout,
        booted,
        rig.workload(0).run(),
        Retired::Recovery(recovery),
        next,
    ) else {
        unreachable!("a swap can be planned from the rolled-over prefix")
    };
    let Ok(prepared) = swap.prepare(&mut engine) else {
        unreachable!("a fault-free erase and barrier")
    };
    let mut header_page = [0_u8; Rig::PAGE_BYTES];
    let Ok(staged) = prepared.stage(&mut header_page) else {
        unreachable!("the header and its seal fit a page")
    };
    let Ok(sealable) = staged.payload_barrier() else {
        unreachable!("a fault-free barrier")
    };
    let Ok(_installed) = sealable.commit() else {
        unreachable!("a fault-free commit")
    };

    // Bank B is now the sole authority. Calling either write path again must refuse rather
    // than blindly reading and appending into bank A, which a boot will never choose again.
    let mut metered = Metered::new(&mut device);
    let mut dispatcher = Log::default();
    let outcome = rig.iterate_until_rollover(
        0,
        &mut metered,
        &mut dispatcher,
        &mut page,
        ROLLOVER_EFFECTS_BEFORE,
    );
    assert!(matches!(outcome, Err(RigError::Bank)), "{outcome:?}");

    let Ok(reserve) = Reserve::for_layout(
        Bounds {
            run_input_bytes: 16,
            effect_result_bytes: 16,
            terminal_bytes: 16,
        },
        rig.layout(),
    ) else {
        unreachable!("the shared fixture's own bounds price a valid reserve")
    };
    let outcome = rig.iterate_reserved(0, &mut metered, &mut dispatcher, reserve, &mut page);
    assert!(matches!(outcome, Err(RigError::Bank)), "{outcome:?}");
}

/// Round 7: `require_own_authority` named the bank but never checked whose run its header
/// holds, so `iterate`, `iterate_until_rollover` and `iterate_reserved` would all happily
/// write one iteration's records and witness marks into the authoritative bank's journal
/// even when that bank was installed for a *different* iteration entirely.
///
/// `resume`/`recover_prefix` already refuse this shape, through `installed_journal`'s own
/// run-id check; `journal_region` — the write path's twin — now takes the workload it means
/// to write and refuses the same way before it hands back a region to append into.
#[test]
fn iterate_and_its_siblings_refuse_a_bank_installed_for_another_iteration() {
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    {
        let mut metered = Metered::new(&mut device);
        let Ok(()) = rig.prepare(&mut metered, 0, &mut page) else {
            unreachable!("prepare")
        };
    }
    let before = device.image().to_vec();

    // The bank is authoritative and readable; it simply names iteration 0, not the
    // iteration 1 each call below is asked to continue.
    let mut metered = Metered::new(&mut device);
    let mut dispatcher = Log::default();
    let outcome = rig.iterate(1, &mut metered, &mut dispatcher, &mut NeverCut, &mut page);
    assert!(matches!(outcome, Err(RigError::Bank)), "{outcome:?}");
    assert!(
        dispatcher.entered.is_empty(),
        "a mismatched iteration was dispatched"
    );

    let mut dispatcher = Log::default();
    let outcome = rig.iterate_until_rollover(
        1,
        &mut metered,
        &mut dispatcher,
        &mut page,
        ROLLOVER_EFFECTS_BEFORE,
    );
    assert!(matches!(outcome, Err(RigError::Bank)), "{outcome:?}");
    assert!(
        dispatcher.entered.is_empty(),
        "a mismatched iteration was dispatched"
    );

    let Ok(reserve) = Reserve::for_layout(bounds(16), rig.layout()) else {
        unreachable!("the shared fixture's own bounds price a valid reserve")
    };
    let mut dispatcher = Log::default();
    let outcome = rig.iterate_reserved(1, &mut metered, &mut dispatcher, reserve, &mut page);
    assert!(matches!(outcome, Err(RigError::Bank)), "{outcome:?}");
    assert!(
        dispatcher.entered.is_empty(),
        "a mismatched iteration was dispatched"
    );

    assert_eq!(
        device.image(),
        before.as_slice(),
        "a mismatched iteration mutated the device"
    );
}

/// Round 5, review finding B: `perform` used to rebuild its own workload from the iteration
/// number instead of using the one `resume_as` was auditing against, so `resume_declaring`
/// could durably append a schedule record its own narrower workload could not answer for.
/// `perform` now takes the caller's `workload` directly, which is what makes the refusal
/// below possible at all: without it, the extra effect's schedule record would land durably
/// before the narrower `self.workload(iteration)` refused to dispatch it.
///
/// Round 6, review finding C: a `declared` workload wider than [`Rig::effects`](Rig) has the
/// same run id as this rig's own workload whenever it shares its seed and iteration, so it
/// can match the recovered prefix exactly — but the bank and witness were only ever
/// provisioned for `Rig::effects`. Left ungated, review found this would run past that
/// provisioning until an unrelated capacity error (`AppendError::NoRoom`,
/// `WitnessError::Full`) stopped it, which is not the failure `resume`'s own postcondition
/// promises: a refusal *before* any write or dispatch. `resume_as` now refuses any workload
/// wider than `self.effects` before touching the device at all.
///
/// This finds a crash point that leaves both of the rig's two effects durably completed and
/// `RunCompleted` not yet begun, then resumes it declaring a workload with one more effect
/// than the rig was provisioned for. Reverting the capacity check reproduces the review
/// finding directly: the extra effect's schedule record lands durably and is dispatched
/// (finding B's own fix serves it, since the declared workload's extra effect matches the
/// recovered prefix) — exactly the mutation-before-refusal finding C flags.
#[test]
fn resume_declaring_refuses_a_workload_wider_than_the_rig_was_provisioned_for() {
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
    let rig = rig();
    let declared = Workload::new(SEED, 0, EFFECTS + 1);

    let mut checked = 0_usize;
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut device = device_after(run);
        let Ok(evidence) = evidence(&rig, &mut device, entered, true) else {
            continue;
        };
        // Both of the rig's own effects durably completed, and `RunCompleted` not yet
        // begun: the sharpest case, because the recovered prefix agrees with the declared
        // workload exactly and the only disagreement is what comes after it — which is
        // exactly the state from which the unfixed code would proceed to mutate.
        if !matches!(
            (evidence.attempted, evidence.activity, evidence.recovered_it),
            (Role::Completion(1), Activity::Returned, true)
        ) {
            continue;
        }
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let mut dispatcher = Log::default();
        let before = device.image().to_vec();
        let outcome = {
            let mut metered = Metered::new(&mut device);
            rig.resume_declaring(0, declared, &mut metered, &mut dispatcher, &mut page)
        };
        assert!(matches!(outcome, Err(RigError::Workload)), "{outcome:?}");
        assert!(
            dispatcher.entered.is_empty(),
            "an over-wide declaration was dispatched before being refused"
        );
        assert_eq!(
            device.image(),
            before.as_slice(),
            "an over-wide declaration mutated the device before being refused"
        );
        checked += 1;
        break;
    }
    assert!(
        checked > 0,
        "no crash point left both effects durably completed with RunCompleted not yet begun"
    );
}

/// Round 8: `resume_declaring` could durably write and dispatch a divergent record that was
/// never actually on media, if the crash it resumed from landed *before*
/// [`Workload::diverging`]'s own changed index — `recover_prefix`'s audit only compares
/// `declared` against what recovery actually found, so with nothing recorded yet at that
/// index there was nothing there to disagree with, and the loop that writes what the run
/// still owes wrote the declared, diverged record fresh and dispatched it.
///
/// The fix widens what a fresh write is checked against: not only must a *recovered* record
/// agree with `declared`, a record `resume_as` is about to write for the first time must
/// agree with this rig's own undiverged truth — `self.workload(iteration)` — before it is
/// marked, appended, or dispatched. For an ordinary `resume` the two are the same workload
/// and the check never fires; `resume_declaring`'s `declared` is where it can differ.
#[test]
fn resume_declaring_refuses_a_divergent_record_it_would_write_fresh() {
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
    let rig = rig();
    let Some(diverging_at) = rig.workload(0).schedule_index(1) else {
        unreachable!("a run of two effects schedules a second one")
    };
    let declared = rig.workload(0).diverging(diverging_at);

    let mut checked = 0_usize;
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut device = device_after(run);
        let Ok(evidence) = evidence(&rig, &mut device, entered, true) else {
            continue;
        };
        // Effect 0 durably completed, and effect 1's schedule — the record `declared`
        // changes — not yet recovered at all: the gap, since nothing on media there could
        // have disagreed with `declared` during `recover_prefix`'s own audit.
        if !matches!(
            (evidence.attempted, evidence.activity, evidence.recovered_it),
            (Role::Completion(0), Activity::Returned, true)
        ) {
            continue;
        }
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let mut dispatcher = Log::default();
        let before = device.image().to_vec();
        let outcome = {
            let mut metered = Metered::new(&mut device);
            rig.resume_declaring(0, declared, &mut metered, &mut dispatcher, &mut page)
        };
        assert!(
            matches!(
                outcome,
                Err(RigError::Breach(Breach::RecordDiffers { index })) if index == diverging_at
            ),
            "{outcome:?}"
        );
        assert!(
            dispatcher.entered.is_empty(),
            "a divergent record that was never on media was dispatched anyway"
        );
        assert_eq!(
            device.image(),
            before.as_slice(),
            "a divergent record that was never on media was written anyway"
        );
        checked += 1;
        break;
    }
    assert!(
        checked > 0,
        "no crash point left effect 0 durable with effect 1's schedule not yet recovered"
    );
}

/// Round 9: the capacity preflight only refused a `declared` *wider* than `self.effects`, so
/// a *narrower* one still reached the write loop. Its early records agree with this rig's
/// own truth — a shorter run's opening effects are a prefix of a longer one's, byte for
/// byte — so the per-record check round 8 added does not fire until the declaration's own
/// early `RunCompleted` collides with an effect index the real run still has open,
/// mutating and dispatching every record in between first.
///
/// The fix widens the preflight from "not wider" to "the same shape": `resume_declaring`
/// only ever means to audit history against a workload that agrees with this rig's own run
/// everywhere but the one record [`Workload::diverging`] names, and a workload of a
/// different length is not that.
#[test]
fn resume_declaring_refuses_a_narrower_workload_before_dispatching_anything() {
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
    let rig = rig();
    let declared = Workload::new(SEED, 0, EFFECTS - 1);

    let mut checked = 0_usize;
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut device = device_after(run);
        let Ok(evidence) = evidence(&rig, &mut device, entered, true) else {
            continue;
        };
        // Only `RunStarted` durable: every record the narrower declaration would write is
        // fresh, so nothing but the per-record shape check can catch it.
        if !matches!(
            (evidence.attempted, evidence.recovered_it),
            (Role::Start, true)
        ) {
            continue;
        }
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let mut dispatcher = Log::default();
        let before = device.image().to_vec();
        let outcome = {
            let mut metered = Metered::new(&mut device);
            rig.resume_declaring(0, declared, &mut metered, &mut dispatcher, &mut page)
        };
        assert!(matches!(outcome, Err(RigError::Workload)), "{outcome:?}");
        assert!(
            dispatcher.entered.is_empty(),
            "a narrower declaration dispatched an effect before being refused"
        );
        assert_eq!(
            device.image(),
            before.as_slice(),
            "a narrower declaration mutated the device before being refused"
        );
        checked += 1;
        break;
    }
    assert!(checked > 0, "no crash point left only RunStarted durable");
}

/// Round 10: round 8's per-record check runs inside the write loop, so a record that
/// *agrees* with this rig's own truth is still written and, if it schedules an effect,
/// dispatched — before the loop ever reaches the index where `declared` disagrees. Row 10's
/// own promise is "no further execution and history untouched", for the whole declaration,
/// not only for the one record that turns out to disagree.
///
/// The fix moves the check ahead of everything: every record `resume_as` would still need
/// to write is compared against this rig's own truth in one pass, before the outstanding-
/// effect redelivery and the write loop both run, so a disagreement anywhere in what is
/// left of the run refuses before the first agreeing record in front of it is touched.
#[test]
fn resume_declaring_refuses_before_running_any_record_that_agrees_ahead_of_the_divergence() {
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
    let rig = rig();
    let Some(diverging_at) = rig.workload(0).schedule_index(1) else {
        unreachable!("a run of two effects schedules a second one")
    };
    let declared = rig.workload(0).diverging(diverging_at);

    let mut checked = 0_usize;
    for (run, entered) in runs.iter().zip(&logs) {
        let Some(injection) = run.injection() else {
            continue;
        };
        if injection.interruption == Interruption::Failure {
            continue;
        }
        let mut device = device_after(run);
        let Ok(evidence) = evidence(&rig, &mut device, entered, true) else {
            continue;
        };
        // Only `RunStarted` durable: effect 0's schedule and completion — both of which
        // agree with `declared`, since divergence is only at effect 1's schedule — are
        // still ahead of the loop, in front of the record that actually disagrees.
        if !matches!(
            (evidence.attempted, evidence.recovered_it),
            (Role::Start, true)
        ) {
            continue;
        }
        let mut page = [0_u8; Rig::PAGE_BYTES];
        let mut dispatcher = Log::default();
        let before = device.image().to_vec();
        let outcome = {
            let mut metered = Metered::new(&mut device);
            rig.resume_declaring(0, declared, &mut metered, &mut dispatcher, &mut page)
        };
        assert!(
            matches!(
                outcome,
                Err(RigError::Breach(Breach::RecordDiffers { index })) if index == diverging_at
            ),
            "{outcome:?}"
        );
        assert!(
            dispatcher.entered.is_empty(),
            "effect 0 was dispatched before the later divergence was caught"
        );
        assert_eq!(
            device.image(),
            before.as_slice(),
            "effect 0's records were written before the later divergence was caught"
        );
        checked += 1;
        break;
    }
    assert!(checked > 0, "no crash point left only RunStarted durable");
}
