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
//! # Which cell a run fills, and what a cell is worth
//!
//! A cell is credited from the injector's own cause, never from a reading of `Progress`. An
//! earlier version of this file partitioned power cuts by how much of an operation completed
//! and called half of them watchdog resets. Codex was right to reject it: that groups power
//! cuts, it does not perform a reset the supply survives.
//!
//! The harness now performs one. [`Interruption::Watchdog`] holds the supply, so the flash
//! controller finishes the unit the core stopped believing in and the writer is never told the
//! operation completed. [`cause_of`] reads the cause off the injection, and
//! [`a_watchdog_cell_is_credited_only_by_a_watchdog_reset`] is what keeps it reading the cause
//! rather than the progress.
//!
//! # What the filled watchdog cells buy, stated exactly
//!
//! Less at a write point than a reader would assume, and the amount is measured rather than
//! described.
//!
//! On media a watchdog reset is *weaker* than a brownout: `waymaker-fault`'s
//! `every_watchdog_image_is_one_a_power_cut_also_produces` proves it. The two causes part
//! company over what the *call* answered, so at a *completed* operation they can only diverge
//! where something other than another storage call follows — which in this rig is the
//! dispatch, and nowhere else.
//!
//! [`the_two_causes_part_company_only_where_an_effect_follows_a_completed_call`] is that,
//! measured over whole operations: identical media at every one, identical dispatch wherever
//! the *engine* was interrupted, and a divergence somewhere. So at a schedule or a completion
//! write the watchdog cell records that the cause was performed and that recovery survived it;
//! it does not record a run its power-cut twin did not also produce. Saying so is the
//! difference between a census and a tally.
//!
//! # How the dispatch cell fills under a watchdog reset
//!
//! At exactly one crash point, and it took two review rounds to find the right one.
//!
//! Not at the dispatch mark's own commit barrier: that barrier does not return under this
//! cause, so the effect never goes out. And not inside the *next* witness program either,
//! which is what an earlier revision credited — there a write is in flight, so the run was cut
//! during that write and not in the window. It read like the window because the torn mark is
//! not whole, leaving `attempted` on the schedule while `effects` had moved.
//!
//! It fills at `(next operation, Progress::None, Watchdog)`: the dispatch mark's barrier
//! returned, the dispatcher ran, and the core reset before the next program began. Nothing on
//! media is half done and the effect is out, which is the window design document §02 decision
//! 3 opens. The power-cut cell fills at the mirror of it — `(barrier, Whole, PowerLoss)`,
//! where the call returns and the world stops before the next one.
//!
//! Both halves of that are Codex's, from the third and fourth rounds:
//! [`in_flight`] is the qualification that stops the wrong runs counting, and the `None`
//! watchdog point before every operation is what makes the right one exist.
//! [`a_write_in_flight_is_not_the_dispatch_window`] requires the wrong runs to exist, so the
//! qualification cannot quietly stop qualifying.
//!
//! # What the boards still owe
//!
//! Everything physical, for both causes. A real part may abort the unit in flight rather than
//! finish it, may leave a bit at neither level, has a reset-cause register no model has, and
//! runs a watchdog off a timer rather than at a call boundary; RAM retention is real there and
//! is modelled here only in `tests/teeth.rs`, as the shortcut it makes available.
//! `xtask::docs::HARDWARE_TARGETS` carries both rows and both stay `Not run`. A complete
//! census here is a complete census *of the model*.

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_fault::{Device, FaultError, Harness, Injection, Interruption, Op, Progress, Run};
use waymaker_flash::bank::BankId;
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::audit::Breach;
use waymaker_rig::census::Coverage;
use waymaker_rig::cutter::{Dispatcher, NeverCut, PlannedCut};
use waymaker_rig::log::{Entry, Outcome};
use waymaker_rig::phase::{Phase, ResetCause};
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Rig, Stop, Verdict};
use waymaker_rig::wear::{Metered, Wear};
use waymaker_rig::witness::{Progress as Marks, Stage, Witness};
use waymaker_rig::workload::Role;

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

/// How many records a run of this rig writes.
fn records() -> u16 {
    let Some(records) = rig().workload(0).records() else {
        unreachable!("a two-effect run has six records")
    };
    records
}

/// The reset cause an injection models.
///
/// Read off the injection's own cause. `Progress` says how much of an operation landed, which
/// is not a reset cause — see the module documentation for the version of this that read one.
const fn cause_of(injection: Injection) -> Option<ResetCause> {
    match injection.interruption {
        // A failed call the writer reacts to is not a reset at all.
        Interruption::Failure => None,
        Interruption::PowerLoss => Some(ResetCause::PowerCut),
        Interruption::Watchdog => Some(ResetCause::Watchdog),
    }
}

/// Whether a storage operation was half done when the run stopped.
///
/// [`Progress::Bytes`] is the only crash point with an operation open: `None` is before it
/// started and `Whole` is after it finished. A run with a write in flight is a run cut during
/// that write, whatever its witness happens to say.
const fn in_flight(run: &Run) -> bool {
    matches!(
        run.injection(),
        Some(Injection {
            progress: Progress::Bytes(_),
            ..
        })
    )
}

/// Whether the interrupted operation was a write to the *engine*, rather than to the rig's
/// own instrument.
///
/// The census below would otherwise credit "a power cut during the schedule write" to a run
/// whose cut actually tore a witness mark. Both are crash points, but only one of them is the
/// write issue #27 names, and a census that could not tell them apart would not be measuring
/// what its own `Gap` message says.
fn interrupted_the_engine(run: &Run, rig: &Rig) -> bool {
    let Some(injection) = run.injection() else {
        return false;
    };
    let Some(op) = run.ops().get(injection.op) else {
        return false;
    };
    let offset = match *op {
        Op::Program { offset, .. } | Op::Erase { offset, .. } => offset,
        // A barrier names no offset. It is ordered against whatever was written before it, so
        // it belongs to neither side and is left out rather than guessed at.
        Op::Barrier => return false,
    };
    offset < rig.instrument_base()
}

/// Which write point the run was in when it stopped.
///
/// Read off the witness, which is the only thing that survives on a board — but *qualified*
/// twice, and both qualifications were found by review rather than written down.
///
/// `effects` is how many effects the run's dispatcher was actually entered for, which the
/// harness can say and a board cannot. It is what earns the dispatch cell. A mark is not
/// evidence of the thing it marks: `Stage::Dispatched` is programmed *before* the effect goes
/// out — deliberately, so it over-claims rather than under-claims — so a power loss that takes
/// the mark's own commit barrier leaves the mark whole on media with `dispatch` never called.
/// Crediting the dispatch cell from the mark alone would report a cut in the instrument's
/// write as a cut in the dispatch window, which is the same relabelling [`cause_of`] refuses.
///
/// The second qualification is that **nothing was in flight**, and it was Codex's, on the
/// fourth review round. [`Phase::Dispatch`] is the interval in which the effect is under way
/// and no storage operation is: a reset that landed *inside* a later witness program is a
/// reset during that write, not during the dispatch. It reads like one because the torn mark
/// is not whole, so `attempted` still names the schedule while `effects` has already moved —
/// which is the same shape as reading a mark as the event it marks.
///
/// [`in_flight`] is that qualification. A crash point at [`Progress::Bytes`] has an operation
/// half done; `None` and `Whole` do not, and those are the two that can be in the window: a
/// power cut at `Whole` returns `Ok(())` and takes the world at the *next* call, and a
/// watchdog reset at the next operation's `None` returns nothing at all. Both leave the run
/// stopped with the effect out and no write open, which is the window §02 decision 3 opens.
fn phase_of(marks: Marks, run: &Run, rig: &Rig, effects: usize) -> Option<Phase> {
    let workload = rig.workload(0);
    let index = marks.attempted()?;
    match workload.role(index)? {
        Role::Schedule(effect) => {
            if marks.dispatched() == Some(index) && effects > usize::from(effect) && !in_flight(run)
            {
                Some(Phase::Dispatch)
            } else if interrupted_the_engine(run, rig) {
                Some(Phase::Schedule)
            } else {
                None
            }
        }
        Role::Completion(_) if interrupted_the_engine(run, rig) => Some(Phase::Completion),
        Role::Completion(_) | Role::Start | Role::Finish => None,
    }
}

/// Runs `prepare` and one whole iteration, so the harness records the write sequence.
fn drive(session: &mut waymaker_fault::Session) -> Result<Stop, String> {
    drive_counting(session).0
}

/// [`drive`], and how many effects the dispatcher was entered for.
///
/// The second half is the census's evidence that execution reached the dispatch window, and it
/// has to be observed rather than inferred — see [`phase_of`]. It is a count rather than the
/// dispatcher itself because that is all the census asks: which effects went out, not what
/// they did.
fn drive_counting(session: &mut waymaker_fault::Session) -> (Result<Stop, String>, usize) {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut dispatcher = Counting::default();
    let mut metered = Metered::new(session);
    let outcome = rig
        .prepare(&mut metered, 0, &mut page)
        .map_err(|error| format!("prepare: {error:?}"))
        .and_then(|()| {
            rig.iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
                .map_err(|error| format!("iterate: {error:?}"))
        });
    (outcome, dispatcher.dispatched.len())
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
    assert_eq!(outcome.outcome(), Outcome::Passed);
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
        u32::from(records()) * 2,
        "every record is a frame program and a seal program"
    );
    assert_eq!(
        after.barriers() - before.barriers(),
        u32::from(records()) * 2,
        "every record is a payload barrier and a commit barrier"
    );
}

#[test]
fn the_engine_is_charged_for_the_bank_it_installs_and_not_for_the_part() {
    // §10 erases a bank. The rig erases a *part* — both banks and, on an odd block count, a
    // spare — and charging that to the engine made the published figure report five erased
    // blocks for a lifecycle that erases two. The number in the wear report is the one this
    // asserts.
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut metered = Metered::new(&mut device);
    rig.prepare(&mut metered, 0, &mut page)
        .expect("a prepared part");

    let bank = rig.layout().bank(Rig::BANK);
    let blocks = bank.bytes() / geometry().erase_size();
    assert_eq!(
        metered.wear().erase_operations(),
        1,
        "§10 installs one bank with one erase"
    );
    assert_eq!(
        metered.wear().erase_blocks(),
        blocks,
        "the engine's erase is the bank's blocks, not the window's"
    );
    assert_eq!(metered.wear().erased_bytes(), bank.bytes());
    // And the rest of the part is the instrument's, so it is measured rather than hidden.
    assert!(
        metered.rig_wear().erase_blocks() > 0,
        "the part outside the bank is erased by somebody"
    );
    assert_eq!(
        metered.total_wear().erase_blocks(),
        geometry().capacity() / geometry().erase_size(),
        "and between them they erase the whole part exactly once"
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
            outcome.outcome(),
            Outcome::Passed,
            "crash point {:?} produced {outcome:?}",
            run.injection()
        );
    }
}

#[test]
fn the_sweep_covers_every_cell_of_the_census() {
    // Issue #27's census, all six cells. A sweep that never reached a dispatch-phase reset has
    // said nothing about that cell, and this is what makes the silence a failure.
    let harness = Harness::new(geometry());
    // What the dispatcher was entered for, one entry per run. `Harness::run` calls the writer
    // once for the fault-free run and then once per crash point, in the order it returns them,
    // so this vector is `runs` — which the two assertions below check rather than assume.
    let dispatched = RefCell::new(Vec::new());
    let runs = harness
        .run(|session| {
            let (outcome, effects) = drive_counting(session);
            dispatched.borrow_mut().push(effects);
            outcome.map(|_| ()).map_err(|_| ())
        })
        .expect("the fault-free run succeeds");
    let dispatched = dispatched.into_inner();
    assert_eq!(
        dispatched.len(),
        runs.len(),
        "the dispatch evidence and the runs it belongs to came apart"
    );
    assert_eq!(
        dispatched.first().copied(),
        Some(usize::from(EFFECTS)),
        "the fault-free run is the first, and it dispatches every effect"
    );

    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut coverage = Coverage::EMPTY;
    for (run, effects) in runs.iter().zip(dispatched) {
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
        if let Some(phase) = phase_of(marks, run, &rig, effects) {
            coverage = coverage.record(phase, cause);
        }
    }
    for phase in Phase::ALL {
        for cause in ResetCause::ALL {
            assert!(
                coverage.iterations(phase, cause) > 0,
                "the sweep never reset the {} write with a {}",
                phase.name(),
                cause.name()
            );
        }
    }
    coverage
        .verdict()
        .expect("every cell of the census was reached");
}

#[test]
fn a_watchdog_cell_is_credited_only_by_a_watchdog_reset() {
    // The rule that keeps the census from being a relabelling. A cell is credited from the
    // injector's cause, so a sweep that performed no watchdog reset cannot fill a watchdog
    // cell however its power cuts are grouped.
    //
    // Stated over `cause_of` itself, which is the one function the census reads.
    for progress in [Progress::None, Progress::Bytes(1), Progress::Whole] {
        let power = Injection {
            op: 0,
            progress,
            interruption: Interruption::PowerLoss,
        };
        assert_eq!(cause_of(power), Some(ResetCause::PowerCut));
        let watchdog = Injection {
            op: 0,
            progress,
            interruption: Interruption::Watchdog,
        };
        assert_eq!(cause_of(watchdog), Some(ResetCause::Watchdog));
        let failure = Injection {
            op: 0,
            progress,
            interruption: Interruption::Failure,
        };
        assert_eq!(cause_of(failure), None, "a failed call is not a reset");
    }
}

#[test]
fn a_dispatch_mark_is_not_evidence_that_the_dispatcher_ran() {
    // The tooth for the qualification `phase_of` puts on the dispatch cell. `Stage::Dispatched`
    // is programmed *before* the effect goes out, so a power loss taking the mark's own commit
    // barrier leaves the mark whole on media with `dispatch` never called — and a census that
    // read the mark as the event would credit a cut in the instrument's write as a cut in the
    // dispatch window.
    //
    // Asserting that such runs exist is what keeps the qualification honest: without this, a
    // `phase_of` that had quietly gone back to trusting the mark would still pass every test
    // in this file, because the cell it fills wrongly is a cell that also fills rightly.
    let harness = Harness::new(geometry());
    let dispatched = RefCell::new(Vec::new());
    let runs = harness
        .run(|session| {
            let (outcome, effects) = drive_counting(session);
            dispatched.borrow_mut().push(effects);
            outcome.map(|_| ()).map_err(|_| ())
        })
        .expect("the fault-free run succeeds");
    let dispatched = dispatched.into_inner();
    assert_eq!(dispatched.len(), runs.len());

    let rig = rig();
    let workload = rig.workload(0);
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut claimed = 0_usize;
    let mut earned = 0_usize;
    for (run, effects) in runs.iter().zip(dispatched) {
        if run.injection().is_none() {
            continue;
        }
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
        let Some(index) = marks.attempted() else {
            continue;
        };
        let Some(Role::Schedule(effect)) = workload.role(index) else {
            continue;
        };
        if marks.dispatched() != Some(index) {
            continue;
        }
        claimed += 1;
        if effects > usize::from(effect) {
            earned += 1;
        }
    }
    assert!(earned > 0, "no run reached the dispatch window at all");
    assert!(
        claimed > earned,
        "every run whose witness claimed a dispatch had really dispatched, so the census's \
         evidence is measuring nothing"
    );
}

#[test]
fn a_power_cut_only_census_is_still_incomplete() {
    // The census's own rule, independent of what any sweep reached: three filled cells are not
    // six. A `Coverage::verdict` that stopped saying so would let a sweep whose watchdog half
    // silently stopped running report a pass.
    let mut coverage = Coverage::EMPTY;
    for phase in Phase::ALL {
        coverage = coverage.record(phase, ResetCause::PowerCut);
    }
    let gap = coverage
        .verdict()
        .expect_err("half a census is not a census");
    assert_eq!(gap.cause(), ResetCause::Watchdog);
    assert_eq!(gap.phase(), Phase::Schedule);

    // And the cause is carried end to end, so a board is armed for it: the plan draws it, and
    // the cutter is handed it.
    let rig = rig();
    let armed = (0..64_u32)
        .map(|iteration| rig.cut_at(iteration))
        .filter(|cut| cut.cause() == ResetCause::Watchdog)
        .count();
    assert!(armed > 0, "no iteration in 64 armed a watchdog reset");
}

#[test]
fn the_two_causes_part_company_only_where_an_effect_follows_a_completed_call() {
    // What the two filled watchdog cells buy, measured rather than asserted — and the
    // adversarial reading of this change answered rather than left open.
    //
    // Three claims, over every operation of the run at `Progress::Whole`:
    //
    // 1. the media are identical, whichever cause was armed;
    // 2. wherever the *engine* was the thing interrupted — which is every run the schedule and
    //    completion cells are credited from — the dispatch is identical too, so those cells
    //    record no run their power-cut twins did not also produce;
    // 3. somewhere the dispatch is *not* identical, so the two causes are two.
    //
    // The third is the dispatch mark's commit barrier, and the second is why it is the only
    // one: everywhere else a completed call is followed by another storage call, which fails
    // under either cause.
    let harness = Harness::new(geometry());
    let baseline = harness
        .run(|session| drive(session).map(|_| ()).map_err(|_| ()))
        .expect("the fault-free run succeeds");
    let Some(clean) = baseline.first() else {
        unreachable!("the fault-free run is first")
    };
    let operations = clean.ops().len();
    let rig = rig();

    let at = |op: usize, interruption: Interruption| {
        let dispatched = RefCell::new(0_usize);
        let outcome = harness.run_one(
            Injection {
                op,
                progress: Progress::Whole,
                interruption,
            },
            |session: &mut waymaker_fault::Session| {
                let (result, effects) = drive_counting(session);
                *dispatched.borrow_mut() = effects;
                result.map(|_| ()).map_err(|_| ())
            },
        );
        outcome.ok().map(|run| (dispatched.into_inner(), run))
    };

    let mut divergent = 0_usize;
    let mut engine_points = 0_usize;
    for op in 0..operations {
        let (Some((power_effects, power)), Some((watchdog_effects, watchdog))) = (
            at(op, Interruption::PowerLoss),
            at(op, Interruption::Watchdog),
        ) else {
            continue;
        };
        assert_eq!(
            power.image(),
            watchdog.image(),
            "operation {op} left different media under the two causes"
        );
        if interrupted_the_engine(&power, &rig) {
            engine_points += 1;
            assert_eq!(
                power_effects, watchdog_effects,
                "operation {op} is an engine write and the two causes dispatched differently"
            );
        }
        if power_effects != watchdog_effects {
            divergent += 1;
        }
    }
    assert!(
        engine_points > 0,
        "no operation interrupted the engine, so claim 2 is about nothing"
    );
    assert!(
        divergent > 0,
        "no operation dispatched differently, so the two causes are one run"
    );
}

#[test]
fn the_sweep_tears_a_write_and_completes_one() {
    // Not a reset cause — see the module documentation — but still the distinction that
    // decides what media a crash leaves, and worth knowing the sweep produces both.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive(session).map(|_| ()).map_err(|_| ()))
        .expect("the fault-free run succeeds");
    let power_cuts = runs
        .iter()
        .filter_map(Run::injection)
        .filter(|injection| cause_of(*injection) == Some(ResetCause::PowerCut));
    let torn = power_cuts
        .clone()
        .filter(|injection| matches!(injection.progress, Progress::Bytes(_)))
        .count();
    let whole = power_cuts
        .filter(|injection| matches!(injection.progress, Progress::None | Progress::Whole))
        .count();
    assert!(torn > 0 && whole > 0, "{torn} torn, {whole} whole");
}

#[test]
fn the_sweep_reaches_every_record_boundary_and_tears_the_witness_itself() {
    // A census, for the reason `waymaker-fault`'s exists: every test above says "at every
    // crash point the oracle accepts", and none of them says the crash points are *anywhere
    // interesting*. A sweep that quietly thinned out — a workload that stopped writing, a
    // `prepare` that started failing early, a witness that stopped landing — would keep every
    // assertion above green while measuring almost nothing.
    //
    // Lower bounds rather than equalities: the numbers move when the workload or the geometry
    // does, and the dangerous direction is a sweep that shrank.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| drive(session).map(|_| ()).map_err(|_| ()))
        .expect("the fault-free run succeeds");
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];

    let mut with_marks = 0_usize;
    let mut torn_witness = 0_usize;
    let mut reached: BTreeSet<u16> = BTreeSet::new();
    for run in &runs {
        let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
            continue;
        };
        let mut instrument = waymaker_rig::window::Window::new(
            &mut device,
            rig.instrument_base(),
            geometry().erase_size(),
        )
        .expect("the instrument window");
        let Ok(marks) = Witness::new(rig.witness_region()).scan(&mut instrument, &mut page) else {
            continue;
        };
        if marks.marks() > 0 {
            with_marks += 1;
        }
        if marks.torn() {
            torn_witness += 1;
        }
        if let Some(high) = marks.acknowledged() {
            reached.insert(high);
        }
    }

    assert!(runs.len() >= 800, "the sweep shrank to {} runs", runs.len());
    assert!(
        with_marks >= 600,
        "only {with_marks} of {} runs got as far as writing a record",
        runs.len()
    );
    // The supply going *during a witness mark* is the one crash point the instrument has of
    // its own, and the one where an under-claiming mark has to be under-claiming rather than
    // absent. A sweep that never produced one would leave `Progress::torn` untested against
    // real media.
    assert!(
        torn_witness >= 100,
        "only {torn_witness} runs tore a witness mark"
    );
    // Every record of the run, from the opening `RunStarted` to the terminal `RunCompleted`,
    // is a boundary some crash point acknowledged. A sweep that stopped short would be one
    // whose later records are never the last thing on media.
    let records = records();
    let expected: BTreeSet<u16> = (0..records).collect();
    assert_eq!(
        reached, expected,
        "the sweep acknowledged {reached:?} of the run's {records} records"
    );
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
        let cut = rig.cut_at(iteration);
        if cut.phase() != Phase::Dispatch {
            continue;
        }
        let mut device = Device::new(geometry());
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, iteration, &mut page)
            .expect("a prepared part");
        let mut dispatcher = Counting::default();
        let mut cutter = waymaker_rig::cutter::PlannedCut::at(cut, EFFECTS);
        let stop = rig
            .iterate(
                iteration,
                &mut metered,
                &mut dispatcher,
                &mut cutter,
                &mut page,
            )
            .expect("an iteration that stops at its cut");
        assert_eq!(
            stop,
            Stop::Cut {
                phase: Phase::Dispatch,
                effect: cutter.effect(),
            }
        );
        // Exactly the effects before the armed one went out, and the armed one did not: the
        // cut lands after its schedule record committed and before its effect. That is the
        // window §02 decision 3 creates, and the one a media-only sweep cannot see, because
        // no storage operation is in flight to interrupt.
        let expected: Vec<u16> = (0..cutter.effect()).collect();
        assert_eq!(dispatcher.dispatched, expected);
        let outcome = rig
            .verify(iteration, &mut device, &mut page)
            .expect("a verdict");
        assert_eq!(outcome.outcome(), Outcome::Passed, "iteration {iteration}");
        reached = true;
        break;
    }
    assert!(reached, "no iteration in 64 armed a dispatch-phase cut");
}

/// A cutter that records every call it was offered, and never cuts.
#[derive(Default)]
struct Offered {
    calls: Vec<(Phase, u16)>,
}

impl waymaker_rig::cutter::Cutter for Offered {
    fn cut(&mut self, phase: Phase, _cause: ResetCause, effect: u16) -> bool {
        self.calls.push((phase, effect));
        false
    }
}

#[test]
fn a_write_cut_is_offered_once_and_for_the_effect_the_plan_named() {
    // The `Cutter` contract says the rig "has reached `phase` for `effect` and is about to do
    // the work of it". A board's implementation arms a supply cut and returns — so being
    // offered the call for *every* effect means arming on effect 0 whatever the seed said,
    // and being offered it more than once means arming twice. `PlannedCut` filters internally
    // and so cannot see either.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    for iteration in 0..48_u32 {
        let cut = rig.cut_at(iteration);
        if cut.phase() == Phase::Dispatch {
            continue;
        }
        let mut device = Device::new(geometry());
        let mut offered = Offered::default();
        {
            let mut metered = Metered::new(&mut device);
            rig.prepare(&mut metered, iteration, &mut page)
                .expect("a prepared part");
            rig.iterate(
                iteration,
                &mut metered,
                &mut Counting::default(),
                &mut offered,
                &mut page,
            )
            .expect("a run the cutter never stops");
        }
        let wanted = cut.effect_index(EFFECTS);
        assert_eq!(
            offered.calls,
            vec![(cut.phase(), wanted)],
            "iteration {iteration} offered {:?}, wanted one call for effect {wanted} in {:?}",
            offered.calls,
            cut.phase()
        );
    }
}

#[test]
fn a_write_cut_is_armed_after_the_attempted_mark_and_not_before_it() {
    // A board's cutter arms a delayed reset and returns, so the very next storage operation is
    // what a short delay tears. Offered before the `Attempted` mark, that operation is the
    // rig's own witness program — the instrument — and the write phase the run reports would
    // never have been under way at all.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut checked = 0_usize;
    for iteration in 0..48_u32 {
        let cut = rig.cut_at(iteration);
        if cut.phase() == Phase::Dispatch {
            continue;
        }
        let mut device = Device::new(geometry());
        {
            let mut metered = Metered::new(&mut device);
            rig.prepare(&mut metered, iteration, &mut page)
                .expect("a prepared part");
            let mut cutter = PlannedCut::at(cut, EFFECTS);
            rig.iterate(
                iteration,
                &mut metered,
                &mut Counting::default(),
                &mut cutter,
                &mut page,
            )
            .expect("an iteration that stops at its cut");
            assert!(cutter.fired());
        }
        // The record the cut was armed in front of is the one the witness says was begun: the
        // `Attempted` mark is down, and the record itself is not committed.
        let marks = {
            let mut instrument = waymaker_rig::window::Window::new(
                &mut device,
                rig.instrument_base(),
                geometry().erase_size(),
            )
            .expect("the instrument window");
            Witness::new(rig.witness_region())
                .scan(&mut instrument, &mut page)
                .expect("a witness the rig wrote")
        };
        let workload = rig.workload(iteration);
        let index = marks.attempted().expect("a record was begun");
        assert_eq!(
            workload.role(index),
            Some(match cut.phase() {
                Phase::Schedule => Role::Schedule(cut.effect_index(EFFECTS)),
                Phase::Completion => Role::Completion(cut.effect_index(EFFECTS)),
                Phase::Dispatch => unreachable!("dispatch iterations are skipped"),
            }),
            "iteration {iteration}: the cut was armed in front of record {index}"
        );
        assert_eq!(
            marks.acknowledged(),
            index.checked_sub(1),
            "iteration {iteration}: the record the cut precedes must not be acknowledged"
        );
        checked += 1;
    }
    assert!(checked > 0, "no write-phase iteration in 48");
}

/// A part whose power goes at the `k`th mutation.
///
/// [`Harness`] starts every run on an erased device, which is the one thing the window below
/// is not about: what matters there is a part that *finished* the previous iteration. So
/// preparation is cut by a fuse rather than by the injector — at every operation boundary,
/// which is where every window in `prepare` opens and closes, a window being the gap between
/// two operations. Reads are not counted: a read changes nothing, so the power taking one is
/// the power taking the mutation after it.
struct Fuse<'a> {
    device: &'a mut Device,
    left: usize,
}

impl Fuse<'_> {
    /// Whether the power has gone, consuming one of the mutations it was given.
    const fn blown(&mut self) -> bool {
        match self.left.checked_sub(1) {
            Some(left) => {
                self.left = left;
                false
            }
            None => true,
        }
    }
}

impl StableStorage for Fuse<'_> {
    type Error = FaultError;

    fn geometry(&self) -> Geometry {
        self.device.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.device.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        if self.blown() {
            return Err(FaultError::PowerLoss);
        }
        self.device.program(offset, src)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        if self.blown() {
            return Err(FaultError::PowerLoss);
        }
        self.device.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        if self.blown() {
            return Err(FaultError::PowerLoss);
        }
        self.device.barrier()
    }
}

/// The image of a part that ran iteration `iteration` to completion.
fn part_after(iteration: u32) -> Vec<u8> {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut device = Device::new(geometry());
    {
        // A helper in an integration test is not a test body, and the workspace denies
        // `expect` outside one. Either refusal here is a broken fixture, not a finding.
        let mut metered = Metered::new(&mut device);
        if rig.prepare(&mut metered, iteration, &mut page).is_err() {
            unreachable!("the fixture geometry prepares")
        }
        let run = rig.iterate(
            iteration,
            &mut metered,
            &mut Counting::default(),
            &mut NeverCut,
            &mut page,
        );
        if run != Ok(Stop::Completed) {
            unreachable!("the fixture run has no cut in it")
        }
    }
    device.into_image()
}

#[test]
fn preparing_over_a_finished_run_never_reports_a_recovery_violation() {
    // The window the initial-device sweep cannot reach. Every run in it starts on an erased
    // part, so "the engine still holds the previous iteration's authoritative journal" is a
    // state it never produces — and that is the state a rig is really in at every iteration
    // after the first.
    //
    // A reset anywhere in `prepare(1)` over such a part leaves the two halves of the device
    // disagreeing about which run owns it: the instrument is erased and the engine is not, or
    // the other way about, depending on the order preparation takes. Neither is a §14
    // violation — nothing has been claimed about run 1 at all — and a rig that reported one
    // would fail a healthy board overnight and blame the firmware.
    let rig = rig();
    let finished = part_after(0);
    let mut page = [0_u8; Rig::PAGE_BYTES];

    for cut in 0..64_usize {
        let Some(mut device) = Device::restored(geometry(), finished.clone()) else {
            unreachable!("the image came from a device of this geometry")
        };
        let outcome = {
            let mut fuse = Fuse {
                device: &mut device,
                left: cut,
            };
            let mut metered = Metered::new(&mut fuse);
            rig.prepare(&mut metered, 1, &mut page)
        };
        let interrupted = outcome.is_err();

        let verdict = rig
            .verify(1, &mut device, &mut page)
            .unwrap_or_else(|error| panic!("verify after {cut} mutations: {error:?}"));
        assert_eq!(
            verdict.outcome(),
            Outcome::Passed,
            "preparation cut after {cut} mutations reported {verdict:?}"
        );
        if !interrupted {
            // Preparation ran to completion, so every window in it has been swept.
            assert!(
                cut >= 8,
                "preparation completed after {cut} mutations, so the sweep cut almost nothing"
            );
            return;
        }
    }
    unreachable!("preparation never completed within the fuse's range");
}

#[test]
fn two_sealed_banks_are_a_breach_whichever_run_owns_them() {
    // §14 `single-authority` is not a question about ownership. No crash makes a second
    // authority out of nothing, so an ambiguous part is a violation before anybody asks which
    // run installed it — and routing it through the uninstalled path would report the one
    // state `Audit::finish` exists to refuse as a pass whenever the witness is empty.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];

    // Two banks at one generation, carrying byte-identical headers and seals. No writer here
    // produces this — `prepare` installs bank A alone — so it is built rather than driven.
    let ambiguous = |image: Vec<u8>| {
        let a = rig.layout().bank(BankId::A);
        let b = rig.layout().bank(BankId::B);
        let mut spliced = image;
        let (Ok(a_base), Ok(b_base), Ok(bytes)) = (
            usize::try_from(a.base()),
            usize::try_from(b.base()),
            usize::try_from(a.bytes()),
        ) else {
            unreachable!("a laid-out bank fits a usize")
        };
        let Some(installed) = spliced.get(a_base..a_base + bytes).map(<[u8]>::to_vec) else {
            unreachable!("bank A is inside the part")
        };
        let Some(other) = spliced.get_mut(b_base..b_base + bytes) else {
            unreachable!("bank B is inside the part")
        };
        other.copy_from_slice(&installed);
        let Some(device) = Device::restored(geometry(), spliced) else {
            unreachable!("the image came from a device of this geometry")
        };
        device
    };

    // With an empty witness — a part prepared and not yet run. This is the silent pass: the
    // run has claimed nothing, so every other question answers "nothing owed".
    let prepared = {
        let mut device = Device::new(geometry());
        {
            let mut metered = Metered::new(&mut device);
            if rig.prepare(&mut metered, 0, &mut page).is_err() {
                unreachable!("the fixture geometry prepares")
            }
        }
        device.into_image()
    };
    let mut device = ambiguous(prepared);
    let verdict = rig.verify(0, &mut device, &mut page).expect("a verdict");
    assert_eq!(
        verdict.outcome(),
        Outcome::Breached(Breach::Authority { banks: 2 }),
        "an ambiguous part with an empty witness must not be a pass"
    );

    // And with a witness, where the count itself is the evidence: reporting zero would name
    // the wrong failure on a part that has two authorities rather than none.
    let mut device = ambiguous(part_after(0));
    let verdict = rig.verify(0, &mut device, &mut page).expect("a verdict");
    assert_eq!(
        verdict.outcome(),
        Outcome::Breached(Breach::Authority { banks: 2 })
    );
}

#[test]
fn a_run_that_marked_a_part_it_was_never_installed_on_is_a_breach() {
    // The other side of the finding above, and the reason "an uninstalled part is not a
    // verdict" is not a hole to pass through. A part carrying *another* run's bank is not this
    // run's authority however sealed it is, so a witness that says this run had begun writing
    // records is §14's `single-authority` failing: the records went into a journal this run
    // does not own.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let Some(mut device) = Device::restored(geometry(), part_after(0)) else {
        unreachable!("the image came from a device of this geometry")
    };

    // A witness that says this run's first record was begun. Hand-built, because no crash
    // produces it — which is the point.
    let begun = Marks::default().raising(Stage::Attempted, 0);
    let verdict = rig
        .judge(1, &mut device, begun, &mut page)
        .expect("a verdict rather than an error");
    assert_eq!(
        verdict.outcome(),
        Outcome::Breached(Breach::Authority { banks: 0 }),
        "a run with marks and no bank of its own has lost its authority"
    );
    assert_eq!(
        verdict.banks(),
        1,
        "and the evidence still records what the part physically had"
    );
}

#[test]
fn a_witness_left_by_the_previous_iteration_is_not_read_as_this_one_s() {
    // A rig's loop is `prepare(n)` → `iterate(n)` → reset → `verify(n)`, so a reset that lands
    // before `prepare(n)` erased the instrument leaves iteration `n - 1`'s marks in front of
    // iteration `n`'s declarations. Pairing the two is unsound in both directions — it invents
    // a breach on a healthy device, and it excuses a genuine loss when the stale marks happen
    // to claim less than this run did.
    //
    // *Which* of the two is right depends on something outside the witness, and getting that
    // wrong is what round five of review found. A stale witness on a part this run was never
    // installed on says nothing and is owed nothing; a stale witness on a part this run *is*
    // installed on is an instrument that failed, because `prepare` erases the instrument
    // before it installs the bank. Both halves are below.
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut device = Device::new(geometry());

    // Iteration 0, run to completion and left on the part.
    {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("a prepared part");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting::default(),
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
    }
    assert_eq!(
        rig.verify(0, &mut device, &mut page).map(Verdict::outcome),
        Ok(Outcome::Passed),
        "iteration 0 judged as itself"
    );
    let stale = device.image().to_vec();

    // Half one: the reset landed before iteration 1 touched anything. Both halves of the part
    // are iteration 0's — a perfectly healthy device, mid-way through a rig's own bookkeeping
    // — and iteration 1 has claimed nothing that recovery could have lost.
    let outcome = rig
        .verify(1, &mut device, &mut page)
        .expect("a verdict rather than an error");
    assert_eq!(
        outcome.outcome(),
        Outcome::Passed,
        "a part iteration 1 was never installed on is not a verdict about iteration 1"
    );

    // Half two: a part iteration 1 *is* installed on, carrying iteration 0's marks. No crash
    // produces this — `prepare` erases the instrument first — so reaching it means the
    // instrument failed, and the rig must say so rather than audit run 1 against run 0's
    // claims.
    let mut installed = Device::new(geometry());
    {
        let mut metered = Metered::new(&mut installed);
        rig.prepare(&mut metered, 1, &mut page)
            .expect("a part prepared for iteration 1");
    }
    let mut spliced = installed.into_image();
    let base = usize::try_from(rig.instrument_base()).expect("an instrument base");
    let Some(instrument) = spliced.get_mut(base..) else {
        unreachable!("the instrument is inside the part")
    };
    let Some(previous) = stale.get(base..) else {
        unreachable!("the two images are the same geometry")
    };
    instrument.copy_from_slice(previous);
    let Some(mut device) = Device::restored(geometry(), spliced) else {
        unreachable!("the image came from a device of this geometry")
    };
    let outcome = rig
        .verify(1, &mut device, &mut page)
        .expect("a verdict rather than an error");
    assert_eq!(
        outcome.outcome(),
        Outcome::Breached(Breach::WitnessUnreadable),
        "an installed part carrying the previous iteration's marks is a broken instrument"
    );
}

#[test]
fn a_witness_caused_violation_is_reproduced_from_the_log_line_alone() {
    // The half of issue #27's third "done when" that *is* a replay: the obligations §14 puts
    // on a recovery come from the witness, the witness travels in the line, and the run comes
    // from the seed — so a reader with nothing but the bytes reaches the identical verdict.
    // Nothing below the `Entry::parse` may name a variable from above it.
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let wear = {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("a prepared part");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting::default(),
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
        metered.wear()
    };

    // A witness that claims more than happened: the writer began and acknowledged record 99.
    let lying = Marks::EMPTY
        .raising(Stage::Attempted, 99)
        .raising(Stage::Acknowledged, 99);
    let verdict = rig
        .judge(0, &mut device, lying, &mut page)
        .expect("a verdict");
    assert_eq!(
        verdict.outcome(),
        Outcome::Breached(Breach::LostAcknowledgedRecord { index: 99 })
    );

    let mut line = [0_u8; Entry::LINE_BYTES];
    let rendered = rig
        .entry(0, verdict, wear, lying)
        .render(&mut line)
        .expect("a line")
        .to_vec();

    // ---- Everything below this line knows only `rendered`. ----
    let read = Entry::parse(&rendered).expect("a line the rig wrote");
    let again = replay_from(&read);
    assert_eq!(
        again.outcome(),
        read.outcome(),
        "the log line did not reproduce the verdict it recorded"
    );
    assert_eq!(again.recovered(), read.recovered());
    assert_eq!(usize::from(read.banks()), again.banks());
}

#[test]
fn a_media_caused_violation_is_characterised_by_the_log_line_it_cannot_replay() {
    // The other half, and the one worth being exact about. A `RecordDiffers` is caused by the
    // *bytes on the part*, and a log line cannot carry a bank. Rebuilding a part from the seed
    // writes the correct history, so replaying it reaches `Passed` — the breach is gone with
    // the media that caused it.
    //
    // What the line can do, and now does, is carry the evidence: how many records recovery
    // accepted before it diverged, and how many banks claimed authority. Without those a line
    // reading `record-differs` says history diverged and not where, and the only way to find
    // out is to still have the device.
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let wear = {
        let mut metered = Metered::new(&mut device);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("a prepared part");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting::default(),
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
        metered.wear()
    };
    let honest = {
        let mut instrument = waymaker_rig::window::Window::new(
            &mut device,
            rig.instrument_base(),
            geometry().erase_size(),
        )
        .expect("the instrument window");
        Witness::new(rig.witness_region())
            .scan(&mut instrument, &mut page)
            .expect("a witness the rig wrote")
    };

    // Media that diverges from what the run declared, without touching the witness: a second
    // rig's history, written into the first's *own* bank. This is the state a real bug
    // produces — recovery handing back records that are not the ones this run declared — and
    // the bank is installed by `rig` rather than by `other` on purpose. A part another run
    // installed is not this run's part at all; it is judged by `Rig::installed_journal` and
    // reported as no verdict, which is a different finding and belongs to a different test.
    let other = Rig::new::<waymaker_fault::FaultError>(geometry(), Plan::new(SEED ^ 0xFF), EFFECTS)
        .expect("the same layout under another seed");
    let mut divergent = Device::new(geometry());
    {
        let mut metered = Metered::new(&mut divergent);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("a part this run installed");
        other
            .iterate(
                0,
                &mut metered,
                &mut Counting::default(),
                &mut NeverCut,
                &mut page,
            )
            .expect("a clean run of another workload");
    }
    let verdict = rig
        .judge(0, &mut divergent, honest, &mut page)
        .expect("a verdict");
    let Outcome::Breached(Breach::RecordDiffers { index }) = verdict.outcome() else {
        panic!(
            "another run's history must differ; got {:?}",
            verdict.outcome()
        );
    };
    assert_eq!(
        verdict.recovered(),
        index,
        "the divergence is where it says"
    );

    let mut line = [0_u8; Entry::LINE_BYTES];
    let rendered = rig
        .entry(0, verdict, wear, honest)
        .render(&mut line)
        .expect("a line")
        .to_vec();

    // ---- Everything below this line knows only `rendered`. ----
    let read = Entry::parse(&rendered).expect("a line the rig wrote");
    assert_eq!(read.outcome(), verdict.outcome());
    assert_eq!(read.recovered(), index);
    assert_eq!(read.banks(), 1);
    // And the honest limit, asserted rather than left for a reader to discover: the part is
    // gone, so replaying the run from the seed reaches a pass. That is why the evidence above
    // is in the line, and it is what ADR 0021 records as what a log line cannot carry.
    let again = replay_from(&read);
    assert_eq!(
        again.outcome(),
        Outcome::Passed,
        "a rebuilt part carries correct history, so the breach is not in it"
    );
    assert_ne!(
        again.recovered(),
        read.recovered(),
        "and the evidence is what tells the two runs apart"
    );
}

/// Rebuilds the run `entry` names and judges it against the witness `entry` carries.
///
/// Everything it uses comes from the entry, which is the discipline the two tests above are
/// about: a replay that reached for anything else would be reproducing the test rather than
/// the log line.
fn replay_from(entry: &Entry) -> Verdict {
    // A helper in an integration test is not a test body, and the workspace denies `expect`
    // outside one. Every refusal here would be a broken fixture rather than a finding.
    let Ok(geometry) = entry.geometry() else {
        unreachable!("the rig wrote this line, so its four units are a geometry")
    };
    let Ok(replayed) =
        Rig::new::<waymaker_fault::FaultError>(geometry, Plan::new(entry.seed()), entry.effects())
    else {
        unreachable!("the layout the line names is the one the rig was built on")
    };
    let mut fresh = Device::new(geometry);
    let mut page = [0_u8; Rig::PAGE_BYTES];
    {
        let mut metered = Metered::new(&mut fresh);
        let Ok(()) = replayed.prepare(&mut metered, entry.iteration(), &mut page) else {
            unreachable!("a legal layout prepares")
        };
        let Ok(_) = replayed.iterate(
            entry.iteration(),
            &mut metered,
            &mut Counting::default(),
            &mut NeverCut,
            &mut page,
        ) else {
            unreachable!("the run rebuilt from the seed is the run that already ran")
        };
    }
    let Ok(verdict) = replayed.judge(entry.iteration(), &mut fresh, entry.progress(), &mut page)
    else {
        unreachable!("a healthy part has a verdict")
    };
    verdict
}

#[test]
fn a_log_line_carries_what_the_rig_knew_and_not_only_what_it_ran() {
    // The witness field. A line whose witness did not survive the round trip would reproduce a
    // *different* verdict — most likely `Passed`, because an empty witness demands nothing,
    // which is the direction that turns a recorded violation into a clean run.
    let rig = rig();
    let knew = Marks::EMPTY
        .raising(Stage::Attempted, 4)
        .raising(Stage::Acknowledged, 3)
        .raising(Stage::Dispatched, 1);
    let mut line = [0_u8; Entry::LINE_BYTES];
    let entry = rig
        .entry(7, passing_verdict(), Wear::NONE, knew)
        .with_evidence(6, 1);
    let rendered = entry.render(&mut line).expect("a line");
    let read = Entry::parse(rendered).expect("a line the rig wrote");
    assert_eq!(read.progress(), knew);
    assert_eq!(read.progress().attempted(), Some(4));
    assert_eq!(read.progress().acknowledged(), Some(3));
    assert_eq!(read.progress().dispatched(), Some(1));
    assert_eq!(read.recovered(), 6);
    assert_eq!(read.banks(), 1);
    assert_eq!(read, entry);
}

/// A verdict from a clean run, for the cases that are about the log rather than the run.
fn passing_verdict() -> waymaker_rig::run::Verdict {
    let rig = rig();
    let mut device = Device::new(geometry());
    let mut page = [0_u8; Rig::PAGE_BYTES];
    {
        let mut metered = Metered::new(&mut device);
        let Ok(()) = rig.prepare(&mut metered, 7, &mut page) else {
            unreachable!("a legal layout prepares")
        };
        let Ok(_) = rig.iterate(
            7,
            &mut metered,
            &mut Counting::default(),
            &mut NeverCut,
            &mut page,
        ) else {
            unreachable!("a clean run succeeds")
        };
    }
    let Ok(verdict) = rig.verify(7, &mut device, &mut page) else {
        unreachable!("a clean run has a verdict")
    };
    verdict
}

#[test]
fn a_write_in_flight_is_not_the_dispatch_window() {
    // The tooth for [`in_flight`], and Codex's fourth-round finding kept as a test.
    //
    // A reset inside a later witness program leaves that mark torn, so `attempted` still names
    // the schedule and `effects` has already moved: the run reads like a dispatch-window reset
    // and is a reset during a write. Without the qualification the census credited those, and
    // the cell it credited wrongly is a cell that also fills rightly — so nothing failed.
    //
    // This asserts that such runs exist. If they stop existing, the qualification is guarding
    // nothing and this says so rather than passing quietly.
    let harness = Harness::new(geometry());
    let dispatched = RefCell::new(Vec::new());
    let runs = harness
        .run(|session| {
            let (outcome, effects) = drive_counting(session);
            dispatched.borrow_mut().push(effects);
            outcome.map(|_| ()).map_err(|_| ())
        })
        .expect("the fault-free run succeeds");
    let dispatched = dispatched.into_inner();
    let rig = rig();
    let workload = rig.workload(0);
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let (mut looked_like, mut credited) = (0_usize, 0_usize);

    for (run, effects) in runs.iter().zip(dispatched) {
        if run.injection().is_none() {
            continue;
        }
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
        let Some(index) = marks.attempted() else {
            continue;
        };
        let Some(Role::Schedule(effect)) = workload.role(index) else {
            continue;
        };
        if marks.dispatched() != Some(index) || effects <= usize::from(effect) {
            continue;
        }
        looked_like += 1;
        if phase_of(marks, run, &rig, effects) == Some(Phase::Dispatch) {
            credited += 1;
        }
    }
    assert!(credited > 0, "no run reached the dispatch window at all");
    assert!(
        looked_like > credited,
        "every run whose witness and dispatcher agreed was really in the window, so the \
         in-flight qualification is measuring nothing"
    );
}
