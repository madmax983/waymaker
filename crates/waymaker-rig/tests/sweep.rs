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
//! # Why every cell this file fills is a power cut
//!
//! An earlier version of this file partitioned the injector's enumeration into the two reset
//! causes — a torn program for a brownout, a whole or unstarted one for a watchdog — and
//! reported a complete census. Codex was right to reject it, and the reason is worth keeping
//! here rather than in a commit message.
//!
//! Every injection the harness performs is an [`Interruption::PowerLoss`], whose documented
//! contract is "the world stops here": nothing runs afterwards, the session is dead, and the
//! image is what media held at that instant. What separates a watchdog reset from a brownout
//! is that the supply *holds* — the flash controller may finish a unit the core has stopped
//! believing in, RAM is not cleared, and the reset-cause register says which happened. The
//! model has none of those. Splitting `Progress` two ways groups power cuts by how much of an
//! operation completed; it does not perform a watchdog reset, and calling the result watchdog
//! coverage is the relabelling this crate exists to avoid.
//!
//! So this file fills the three [`ResetCause::PowerCut`] cells and says so. The three
//! [`ResetCause::Watchdog`] cells are a **hardware** obligation, and they are inside the rows
//! `xtask::docs::HARDWARE_TARGETS` already carries — both of which name "power-cut *and
//! watchdog-reset* loops". What this crate does supply for them is everything but the
//! evidence: the plan arms the cause, [`PlannedCut`] hands it to the cutter, the log line
//! records it, and the census refuses a run that never reached them. That refusal is a tested
//! property below rather than a claim.

use std::collections::BTreeSet;

use waymaker_fault::{Device, Harness, Injection, Interruption, Op, Progress, Run};
use waymaker_flash::storage::Geometry;
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
/// One answer, because the harness has one: `Interruption::PowerLoss` is a power cut whatever
/// `Progress` it stopped at. See the module documentation for why this used to have two.
const fn cause_of(injection: Injection) -> Option<ResetCause> {
    match injection.interruption {
        // A failed call the writer reacts to is not a reset at all.
        Interruption::Failure => None,
        Interruption::PowerLoss => Some(ResetCause::PowerCut),
    }
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
/// by what was interrupted, which is the part the first version of this file left out.
fn phase_of(marks: Marks, run: &Run, rig: &Rig) -> Option<Phase> {
    let workload = rig.workload(0);
    let index = marks.attempted()?;
    match workload.role(index)? {
        Role::Schedule(_) => {
            if marks.dispatched() == Some(index) {
                // The schedule record committed and its effect was marked as going out, and
                // no later record has been begun. That is the dispatch window exactly, and it
                // is the one phase with *no* storage operation in flight — §02 decision 3
                // creates it precisely so that nothing is being written there — so it is
                // classified by the witness alone, on purpose.
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
fn the_sweep_covers_all_three_write_points_under_a_power_cut() {
    // Issue #27's census, over the half a host can supply. A sweep that never reached a
    // dispatch-phase cut has said nothing about that cell, and this is what makes the silence
    // a failure.
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
        if let Some(phase) = phase_of(marks, run, &rig) {
            coverage = coverage.record(phase, cause);
        }
    }
    for phase in Phase::ALL {
        assert!(
            coverage.iterations(phase, ResetCause::PowerCut) > 0,
            "the sweep never cut the {} write",
            phase.name()
        );
    }

    // And the half it cannot: the census must still refuse this run, because a watchdog reset
    // has not happened. A host sweep that reported a complete census would be reporting
    // coverage nothing produced — which is what this file used to do.
    let gap = coverage
        .verdict()
        .expect_err("a host sweep performs no watchdog reset, so the census is not complete");
    assert_eq!(
        gap.cause(),
        ResetCause::Watchdog,
        "the only cells a host cannot fill are the watchdog ones"
    );
}

#[test]
fn the_watchdog_cells_are_owed_by_hardware_and_the_census_says_so() {
    // The rig supplies everything for a watchdog reset but the reset. This is that stated as a
    // test rather than as a comment: a census with every power-cut cell filled is still
    // incomplete, and the gap it names is a watchdog cell every time.
    let mut coverage = Coverage::EMPTY;
    for phase in Phase::ALL {
        coverage = coverage.record(phase, ResetCause::PowerCut);
    }
    let gap = coverage
        .verdict()
        .expect_err("three of the six cells are hardware's");
    assert_eq!(gap.cause(), ResetCause::Watchdog);
    assert_eq!(gap.phase(), Phase::Schedule);

    // And the cause is carried end to end, so a board that *can* perform one is armed for it:
    // the plan draws it, and the cutter is handed it.
    let rig = rig();
    let armed = (0..64_u32)
        .map(|iteration| rig.cut_at(iteration))
        .filter(|cut| cut.cause() == ResetCause::Watchdog)
        .count();
    assert!(armed > 0, "no iteration in 64 armed a watchdog reset");
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

#[test]
fn a_witness_left_by_the_previous_iteration_is_not_read_as_this_one_s() {
    // The shape a board actually runs in, and the one every other test here misses: iteration
    // N on media iteration N-1 left behind. A rig that paired iteration N's declarations with
    // N-1's marks is unsound in both directions — it invents `RecordDiffers` on a healthy
    // device, and it excuses a genuine loss when the stale marks happen to claim less.
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

    // Now the reset lands before iteration 1 got as far as erasing the instrument. The
    // witness still holds iteration 0's marks, and the journal still holds iteration 0's
    // records — a perfectly healthy device, mid-way through a rig's own bookkeeping.
    let outcome = rig
        .verify(1, &mut device, &mut page)
        .expect("a verdict rather than an error");
    assert_eq!(
        outcome.outcome(),
        Outcome::Breached(Breach::WitnessUnreadable),
        "iteration 1 must refuse iteration 0's witness rather than audit against it"
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
    // rig's history, written over the first's journal. This is the state a real bug produces.
    let other = Rig::new::<waymaker_fault::FaultError>(geometry(), Plan::new(SEED ^ 0xFF), EFFECTS)
        .expect("the same layout under another seed");
    let mut divergent = Device::new(geometry());
    {
        let mut metered = Metered::new(&mut divergent);
        other
            .prepare(&mut metered, 0, &mut page)
            .expect("a prepared part");
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
