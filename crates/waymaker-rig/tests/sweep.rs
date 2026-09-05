//! The rig driven at every crash point the injector lists, and judged by its own oracle.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for supply cuts "at
//! randomised points during schedule, dispatch, and completion writes", and for
//! "watchdog-reset tests at the same three points". A board does that with a MOSFET and a
//! timer. On a host this is stronger and weaker in different places, and both are worth
//! saying:
//!
//! * **stronger**, because [`waymaker_fault::injections`] is exhaustive rather than random —
//!   every byte of every program, every block of every erase, before and after every barrier
//!   — so the "randomised points" a board samples are a subset of what runs here;
//! * **weaker**, because the media is a model. §12's barrier is a no-op in the model, the
//!   part has no analogue behaviour, and a bit that programmed weakly is not a state the
//!   model has. That is precisely why the boards are still owed, and
//!   `xtask::docs::HARDWARE_TARGETS` says so.
//!
//! # How a power cut and a watchdog reset are told apart here
//!
//! Not by relabelling. The two differ in what the flash controller was allowed to finish:
//!
//! * a **brownout** stops a program where it stopped, inside a program unit — which is
//!   [`Progress::Bytes`], a torn write;
//! * a **watchdog reset** leaves the supply up, so the unit already handed to the controller
//!   completes or is abandoned whole — which is [`Progress::None`] and [`Progress::Whole`].
//!
//! So the injector's own enumeration partitions into the two causes, and the census below
//! requires both partitions to be non-empty at all three write points.

use waymaker_fault::{Device, Harness, Injection, Interruption, Progress, Run};
use waymaker_flash::storage::Geometry;
use waymaker_rig::audit::Breach;
use waymaker_rig::census::Coverage;
use waymaker_rig::log::{Entry, Outcome};
use waymaker_rig::phase::{Phase, ResetCause};
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Dispatcher, NeverCut, Rig, Stop};
use waymaker_rig::wear::Metered;
use waymaker_rig::witness::{Progress as Marks, Stage, Witness};

const SEED: u64 = 0x00C0_FFEE_0BAD_CAFE;
const EFFECTS: u16 = 2;

fn geometry() -> Geometry {
    // Six erase blocks: two banks of two blocks each in the engine area, one block for the
    // instrument, and one spare so the engine area is a whole number of banks.
    let Ok(geometry) = Geometry::new(6 * 256, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

fn rig() -> Rig {
    let Ok(rig) = Rig::new::<waymaker_fault::FaultError>(geometry(), Plan::new(SEED), EFFECTS)
    else {
        unreachable!("the geometry above holds two banks and a witness")
    };
    rig
}

/// A dispatcher that records which effects it was asked to perform.
#[derive(Default)]
struct Counting {
    dispatched: Vec<u16>,
}

impl Dispatcher for Counting {
    type Error = core::convert::Infallible;

    fn dispatch(&mut self, effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        self.dispatched.push(effect);
        Ok(())
    }
}

/// The reset cause an injection models. See the module documentation.
const fn cause_of(injection: Injection) -> Option<ResetCause> {
    match (injection.progress, injection.interruption) {
        (_, Interruption::Failure) => None,
        (Progress::Bytes(_), Interruption::PowerLoss) => Some(ResetCause::PowerCut),
        (Progress::None | Progress::Whole, Interruption::PowerLoss) => Some(ResetCause::Watchdog),
    }
}

/// Which write point the run had reached when it stopped, read off the witness.
fn phase_of(marks: Marks, rig: &Rig) -> Option<Phase> {
    let workload = rig.workload(0);
    let index = marks.attempted()?;
    match workload.role(index)? {
        waymaker_rig::workload::Role::Schedule(_) => {
            // A schedule record that was acknowledged and dispatched puts the run in the
            // dispatch window; one that was not is still in the schedule write.
            if marks.dispatched() == Some(index) {
                Some(Phase::Dispatch)
            } else {
                Some(Phase::Schedule)
            }
        }
        waymaker_rig::workload::Role::Completion(_) => Some(Phase::Completion),
        waymaker_rig::workload::Role::Start | waymaker_rig::workload::Role::Finish => None,
    }
}

/// Runs `prepare` and one whole iteration, so the harness records the write sequence.
fn drive(session: &mut waymaker_fault::Session) -> Result<Stop, String> {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut metered = Metered::new(session);
    rig.prepare(&mut metered, 0, &mut page)
        .map_err(|error| format!("prepare: {error:?}"))?;
    let mut dispatcher = Counting::default();
    rig.iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
        .map_err(|error| format!("iterate: {error:?}"))
}

/// The media a run left behind, as a device the rig can be pointed at again.
fn device_after(run: &Run) -> Device {
    let Some(device) = Device::restored(geometry(), run.image().to_vec()) else {
        unreachable!("the image came from a device of this geometry")
    };
    device
}

#[test]
fn a_clean_run_writes_the_whole_workload_and_recovers_it() {
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut metered = Metered::new(&mut device);
    rig.prepare(&mut metered, 0, &mut page)
        .expect("a prepared part");
    let mut dispatcher = Counting::default();
    let stop = rig
        .iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
        .expect("a run with no cut in it");
    assert_eq!(stop, Stop::Completed);
    assert_eq!(dispatcher.dispatched, vec![0, 1]);

    let wear = metered.wear();
    assert!(wear.erase_operations() > 0, "the bank install erases");
    assert_eq!(wear.effects(), u32::from(EFFECTS));
    assert!(
        wear.programmed_bytes_per_effect().is_some(),
        "a run with effects in it has a per-effect figure"
    );

    let outcome = rig.verify(0, &mut device, &mut page).expect("a verdict");
    assert_eq!(outcome, Outcome::Passed);
}

#[test]
fn the_rig_and_the_journal_agree_about_what_the_engine_was_asked_for() {
    // Two independent counters of the same traffic. The meter sees the device; the journal
    // sees itself. They must agree, or one of them has stopped counting.
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut metered = Metered::new(&mut device);
    rig.prepare(&mut metered, 0, &mut page)
        .expect("a prepared part");
    let before = metered.wear();
    let mut dispatcher = Counting::default();
    rig.iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
        .expect("a clean run");
    let after = metered.wear();

    // The install's programs and barriers are the engine's too, so the journal's own figure
    // is the difference rather than the total.
    assert_eq!(
        after.program_operations() - before.program_operations(),
        u32::from(rig.workload(0).records()) * 2,
        "every record is a frame program and a seal program"
    );
    assert_eq!(
        after.barriers() - before.barriers(),
        u32::from(rig.workload(0).records()) * 2,
        "every record is a payload barrier and a commit barrier"
    );
}

#[test]
fn the_rigs_own_marks_are_not_charged_to_the_engine() {
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut metered = Metered::new(&mut device);
    rig.prepare(&mut metered, 0, &mut page)
        .expect("a prepared part");
    let mut dispatcher = Counting::default();
    rig.iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
        .expect("a clean run");
    assert!(
        metered.rig_wear().program_operations() > 0,
        "the witness was written"
    );
    assert!(
        metered.total_wear().program_operations() > metered.wear().program_operations(),
        "the instrument's cost is outside the engine's figure"
    );
}

#[test]
fn every_crash_point_leaves_media_the_oracle_accepts() {
    // The exit criterion, on a model: at every point the injector can interrupt the rig, the
    // rig's own verifier must find no violation. A failure here is either a bug in the
    // firmware or a bug in the rig, and both are worth stopping for.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive(session).map(|_| ()).map_err(|_| ()))
        .expect("the fault-free run succeeds");
    assert!(runs.len() > 100, "only {} crash points", runs.len());

    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    for run in &runs {
        let mut device = device_after(run);
        let outcome = rig
            .verify(0, &mut device, &mut page)
            .unwrap_or_else(|error| panic!("verify at {:?}: {error:?}", run.injection()));
        assert_eq!(
            outcome,
            Outcome::Passed,
            "crash point {:?} produced {outcome:?}",
            run.injection()
        );
    }
}

#[test]
fn the_sweep_covers_all_three_write_points_under_both_reset_causes() {
    // Issue #27's census. A sweep that never reached a dispatch-phase watchdog reset has said
    // nothing about that cell, and this is what makes the silence a failure.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive(session).map(|_| ()).map_err(|_| ()))
        .expect("the fault-free run succeeds");

    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut coverage = Coverage::EMPTY;
    for run in &runs {
        let Some(injection) = run.injection() else {
            continue;
        };
        let Some(cause) = cause_of(injection) else {
            continue;
        };
        let mut device = device_after(run);
        let marks = {
            let mut instrument = waymaker_rig::window::Window::new(
                &mut device,
                rig.instrument_base(),
                geometry().erase_size(),
            )
            .expect("the instrument window");
            match Witness::new(rig.witness_region()).scan(&mut instrument, &mut page) {
                Ok(marks) => marks,
                Err(_) => continue,
            }
        };
        if let Some(phase) = phase_of(marks, &rig) {
            coverage = coverage.record(phase, cause);
        }
    }
    coverage
        .verdict()
        .unwrap_or_else(|gap| panic!("the sweep left a hole: {gap}"));
}

#[test]
fn the_sweep_tears_a_write_and_completes_one_at_every_write_point() {
    // The census above counts cells; this checks the two partitions are what they claim to
    // be, so a mapping that quietly put every injection in one bucket cannot pass.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive(session).map(|_| ()).map_err(|_| ()))
        .expect("the fault-free run succeeds");
    let torn = runs
        .iter()
        .filter_map(Run::injection)
        .filter(|injection| cause_of(*injection) == Some(ResetCause::PowerCut))
        .count();
    let whole = runs
        .iter()
        .filter_map(Run::injection)
        .filter(|injection| cause_of(*injection) == Some(ResetCause::Watchdog))
        .count();
    assert!(torn > 0 && whole > 0, "{torn} torn, {whole} whole");
}

#[test]
fn a_run_cut_after_a_dispatch_still_accounts_for_the_effect() {
    // §14 `durable-intent` at the one crash point that matters most: the effect happened and
    // the completion record never did. The schedule record must still be there.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];

    // The plan-driven cutter, on the first iteration whose phase is a dispatch — so the
    // window this test is about is the one actually exercised.
    let mut reached = false;
    for iteration in 0..64_u32 {
        let cut = rig.cut(iteration);
        if cut.phase() != Phase::Dispatch {
            continue;
        }
        let mut device = Device::new(geometry());
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, iteration, &mut page)
            .expect("a prepared part");
        let mut dispatcher = Counting::default();
        let mut cutter = waymaker_rig::run::PlannedCut::new(cut, EFFECTS);
        let stop = rig
            .iterate(
                iteration,
                &mut metered,
                &mut dispatcher,
                &mut cutter,
                &mut page,
            )
            .expect("an iteration that stops at its cut");
        assert!(matches!(
            stop,
            Stop::Cut {
                phase: Phase::Dispatch,
                ..
            }
        ));
        assert!(
            !dispatcher.dispatched.is_empty() || cutter.effect() == 0,
            "the cut lands after at least the first dispatch mark"
        );
        let outcome = rig
            .verify(iteration, &mut device, &mut page)
            .expect("a verdict");
        assert_eq!(outcome, Outcome::Passed, "iteration {iteration}");
        reached = true;
        break;
    }
    assert!(reached, "no iteration in 64 armed a dispatch-phase cut");
}

#[test]
fn a_violation_is_reproducible_from_the_log_line_alone() {
    // Issue #27's third "done when". A breach is produced, logged, the log line is encoded,
    // and a reader given nothing but those bytes reaches the identical breach.
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut metered = Metered::new(&mut device);
    rig.prepare(&mut metered, 0, &mut page)
        .expect("a prepared part");
    let mut dispatcher = Counting::default();
    rig.iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
        .expect("a clean run");
    let wear = metered.wear();

    // A witness that claims more than happened: the writer began and acknowledged record 99.
    // Nothing recovered it, because the run has no record 99 — so `acknowledged-durability`
    // is the guarantee that breaks, and it breaks at `finish` rather than at a record.
    let lying = Marks::default()
        .raising(Stage::Attempted, 99)
        .raising(Stage::Acknowledged, 99);
    let outcome = rig
        .judge(0, &mut device, lying, &mut page)
        .expect("a verdict");
    let Outcome::Breached(breach) = outcome else {
        panic!("a witness claiming record 99 must break something");
    };
    assert_eq!(breach, Breach::LostAcknowledgedRecord { index: 99 });

    let entry = rig.entry(0, outcome, wear);
    let mut line = [0_u8; Entry::LINE_BYTES];
    let rendered = entry.render(&mut line).expect("a line").to_vec();

    // Everything from here on knows only the bytes.
    let read = Entry::parse(&rendered).expect("a line the rig wrote");
    assert_eq!(read.outcome(), outcome);
    let replayed = Rig::new::<waymaker_fault::FaultError>(
        read.geometry().expect("a geometry"),
        Plan::new(read.seed()),
        read.effects(),
    )
    .expect("the same layout");
    let mut fresh = Device::new(read.geometry().expect("a geometry"));
    let mut metered = Metered::new(&mut fresh);
    replayed
        .prepare(&mut metered, read.iteration(), &mut page)
        .expect("a prepared part");
    let mut dispatcher = Counting::default();
    replayed
        .iterate(
            read.iteration(),
            &mut metered,
            &mut dispatcher,
            &mut NeverCut,
            &mut page,
        )
        .expect("the same clean run");
    let again = replayed
        .judge(read.iteration(), &mut fresh, lying, &mut page)
        .expect("a verdict");
    assert_eq!(again, outcome, "the log line did not reproduce the breach");
}
