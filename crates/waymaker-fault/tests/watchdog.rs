//! A watchdog reset is not a brownout, and the model says how it differs.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27): "a watchdog reset is not
//! identical to a brownout and both must be covered". The harness now models both. This file
//! is what "not identical" means when a build can fail over it.
//!
//! # The three differences
//!
//! 1. **The unit completes.** The supply holds, so the flash controller finishes the program
//!    unit the core stopped believing in. Media after a watchdog reset always holds a whole
//!    number of program units. A brownout can stop inside one.
//! 2. **The writer is never told.** The core stops before the call returns. A power loss at
//!    [`Progress::Whole`] returns `Ok(())` first, which is what lets design document §02
//!    decision 3 dispatch an effect; a watchdog reset at the same point puts the same bytes
//!    on media and returns an error.
//! 3. **RAM survives.** Nothing in this crate models RAM. `waymaker-rig` owns that half,
//!    because the rig's durable witness is the thing RAM retention would let a reader skip.
//!
//! # What the first difference costs, and why it is stated as a theorem
//!
//! It makes a watchdog reset *weaker* than a brownout on media. Every image a watchdog reset
//! leaves is an image some power cut also leaves, and
//! [`every_watchdog_image_is_one_a_power_cut_also_produces`] proves it over the real journal
//! writer rather than asserting it. A reader who is told only that the census now has six
//! filled cells would otherwise be entitled to think three of them bought new media states.
//! They buy the two other differences.
//!
//! # What is still owed to a board
//!
//! A real part may abort the unit in flight rather than finish it, may leave a bit at neither
//! level, and has a reset-cause register this model has not. `xtask::docs::HARDWARE_TARGETS`
//! carries both rows, and both stay `Not run`.

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_fault::{
    Device, ERASED, FaultError, Harness, Injection, Interruption, Ledger, Op, Progress, RecordId,
    Run, Session, injections, verify_recovery,
};
use waymaker_flash::append::{AppendError, Journal};
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

/// One erase block of 4-byte program units.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("256 is one 256-byte block of 4-byte units")
    };
    geometry
}

/// The program unit every test here rounds to.
const UNIT: u32 = 4;

/// Eight bytes, none of them erased, so a programmed prefix can be counted.
const PAYLOAD: &[u8] = b"\x01\x02\x03\x04\x05\x06\x07\x08";

/// How many bytes at the start of `run`'s media are programmed.
fn programmed_prefix(run: &Run) -> usize {
    run.image()
        .iter()
        .take_while(|byte| **byte != ERASED)
        .count()
}

/// One run of `writer` with `injection` armed, or a loud failure.
fn run_one<W>(injection: Injection, writer: W) -> Run
where
    W: FnMut(&mut Session) -> Result<(), FaultError>,
{
    match Harness::new(geometry()).run_one(injection, writer) {
        Ok(run) => run,
        Err(error) => unreachable!("{error}"),
    }
}

/// Every run of `writer`, or a loud failure.
fn drive<W>(writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), FaultError>,
{
    match Harness::new(geometry()).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// Programs [`PAYLOAD`] at offset zero and orders it.
fn one_program(session: &mut Session) -> Result<(), FaultError> {
    session.begin_record(RecordId(1));
    session.program(0, PAYLOAD)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

// ---------------------------------------------------------------------------------------
// 1. The unit in flight completes
// ---------------------------------------------------------------------------------------

#[test]
fn a_watchdog_reset_leaves_a_whole_number_of_program_units() {
    // The supply holds, so the controller finishes the unit it was given. A stop after one
    // byte therefore leaves four, and a stop after five leaves eight.
    for (stopped, landed) in [(1, 4), (2, 4), (3, 4), (4, 4), (5, 8), (6, 8), (7, 8)] {
        let run = run_one(
            Injection {
                op: 0,
                progress: Progress::Bytes(stopped),
                interruption: Interruption::Watchdog,
            },
            one_program,
        );
        assert_eq!(
            programmed_prefix(&run),
            landed,
            "a watchdog reset after {stopped} bytes left a partial program unit"
        );
    }
}

#[test]
fn a_brownout_can_stop_inside_a_program_unit() {
    // The tooth for the test above. Without it, a model that rounded *both* causes up would
    // pass, and the difference the census counts would not exist.
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::Bytes(1),
            interruption: Interruption::PowerLoss,
        },
        one_program,
    );
    assert_eq!(programmed_prefix(&run), 1);
}

#[test]
fn a_watchdog_reset_that_takes_a_whole_operation_leaves_all_of_it() {
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::Whole,
            interruption: Interruption::Watchdog,
        },
        one_program,
    );
    assert_eq!(programmed_prefix(&run), PAYLOAD.len());
}

// ---------------------------------------------------------------------------------------
// 2. The writer is never told
// ---------------------------------------------------------------------------------------

#[test]
fn a_watchdog_reset_never_hands_the_writer_an_ok() {
    // The sharpest difference, and the one no media image shows. A power cut at
    // `Progress::Whole` returns `Ok(())` and takes the world at the *next* call, so a writer
    // that does `barrier()?` and then dispatches really dispatches. A watchdog reset stops
    // the core first: the same bytes are on media and the writer learns nothing.
    let seen = RefCell::new(Vec::new());
    let mut writer = |session: &mut Session| {
        let result = session.program(0, PAYLOAD);
        seen.borrow_mut().push(result);
        result
    };

    let power = run_one(
        Injection {
            op: 0,
            progress: Progress::Whole,
            interruption: Interruption::PowerLoss,
        },
        &mut writer,
    );
    let watchdog = run_one(
        Injection {
            op: 0,
            progress: Progress::Whole,
            interruption: Interruption::Watchdog,
        },
        &mut writer,
    );

    // Each `run_one` runs the writer twice: once with nothing armed, once with the crash
    // point. The armed run is the second of each pair.
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 4, "two runs of two");
    assert_eq!(
        seen.get(1),
        Some(&Ok(())),
        "a power cut lets the call return"
    );
    assert_eq!(
        seen.get(3),
        Some(&Err(FaultError::WatchdogReset)),
        "a watchdog reset stops the core before the call returns"
    );
    assert_eq!(
        power.image(),
        watchdog.image(),
        "the two crash points differ in what the writer learned, not in what landed"
    );
}

#[test]
fn a_watchdog_reset_acknowledges_a_record_the_writer_never_saw_ordered() {
    // The consequence of the difference above. The barrier completes on media, so the record
    // is durable and recovery owes it; the writer got an error and knows none of that.
    let run = run_one(
        Injection {
            op: 1,
            progress: Progress::Whole,
            interruption: Interruption::Watchdog,
        },
        one_program,
    );
    assert_eq!(
        run.ledger().state(RecordId(1)),
        Some(waymaker_fault::Durability::Acknowledged)
    );
    assert!(verify_recovery(run.ledger(), &[RecordId(1)]).is_ok());
    assert!(
        verify_recovery(run.ledger(), &[]).is_err(),
        "an acknowledged record recovery lost is a breach whatever reset it survived"
    );
}

// ---------------------------------------------------------------------------------------
// 3. The enumeration
// ---------------------------------------------------------------------------------------

#[test]
fn a_watchdog_reset_is_enumerated_at_whole_operations_and_nowhere_else() {
    // Every other watchdog world is a power-cut world this list already has, and an
    // enumeration that counts one crash point twice is no longer a count of anything. The two
    // tests below are what makes that a measurement rather than an assumption.
    let ops = [Op::Program { offset: 0, len: 8 }, Op::Barrier];
    let watchdog: Vec<_> = injections(&ops, geometry())
        .into_iter()
        .filter(|point| point.interruption == Interruption::Watchdog)
        .collect();

    assert_eq!(
        watchdog,
        vec![
            Injection {
                op: 0,
                progress: Progress::Whole,
                interruption: Interruption::Watchdog,
            },
            // A barrier has no interior and still has this point: it completed, and the core
            // stopped before it returned. Design document §02 decision 3 is about that state.
            Injection {
                op: 1,
                progress: Progress::Whole,
                interruption: Interruption::Watchdog,
            },
        ]
    );
}

#[test]
fn a_watchdog_reset_inside_a_unit_is_a_power_cut_at_the_boundary_above_it() {
    // Why the interior points are left out, measured. The controller finishes the unit, so a
    // reset one byte in leaves what a power cut four bytes in leaves, and the writer is dead
    // in both.
    let boundary = run_one(
        Injection {
            op: 0,
            progress: Progress::Bytes(UNIT),
            interruption: Interruption::PowerLoss,
        },
        one_program,
    );
    for inside in 1..UNIT {
        let watchdog = run_one(
            Injection {
                op: 0,
                progress: Progress::Bytes(inside),
                interruption: Interruption::Watchdog,
            },
            one_program,
        );
        assert_eq!(
            watchdog.image(),
            boundary.image(),
            "a watchdog reset {inside} bytes in is not the power cut at the unit above it"
        );
    }
}

#[test]
fn a_watchdog_reset_at_a_whole_operation_is_not_a_power_cut_at_one() {
    // The other half: the point that *is* enumerated is enumerated because it is new. The
    // media agree and the answers do not.
    let seen = RefCell::new(Vec::new());
    let mut writer = |session: &mut Session| {
        let result = session.program(0, PAYLOAD);
        seen.borrow_mut().push(result);
        result
    };
    for interruption in [Interruption::PowerLoss, Interruption::Watchdog] {
        drop(run_one(
            Injection {
                op: 0,
                progress: Progress::Whole,
                interruption,
            },
            &mut writer,
        ));
    }
    let seen = seen.into_inner();
    assert_eq!(seen.get(1), Some(&Ok(())));
    assert_eq!(seen.get(3), Some(&Err(FaultError::WatchdogReset)));
}

#[test]
fn the_enumeration_has_no_duplicates_and_every_cause_in_it() {
    let ops = [
        Op::Erase {
            offset: 0,
            len: 256,
        },
        Op::Program { offset: 0, len: 8 },
        Op::Barrier,
    ];
    let points = injections(&ops, geometry());
    let unique: BTreeSet<_> = points.iter().copied().collect();
    assert_eq!(unique.len(), points.len(), "a crash point is listed once");

    for cause in [
        Interruption::PowerLoss,
        Interruption::Watchdog,
        Interruption::Failure,
    ] {
        assert!(
            points.iter().any(|point| point.interruption == cause),
            "{cause:?} is not enumerated"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 4. The real writer, at every watchdog reset
// ---------------------------------------------------------------------------------------

/// The activity every schedule record names.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// How many records the journal writer appends.
const RECORDS: u32 = 2;

/// The page this device recovers and stages with.
const PAGE: usize = 64;

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a legal program alignment")
    };
    align
}

fn region() -> JournalRegion {
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 256, align()) else {
        unreachable!("the whole device is a legal program")
    };
    region
}

/// The record at `index` of the fixture history.
const fn record(index: u32) -> RecordRef<'static> {
    if index % 2 == 0 {
        RecordRef::EffectScheduled {
            seq: EffectSeq(index),
            kind: DOWNLOAD,
            input_len: 4,
            input_crc: frame::input_digest(b"blob"),
        }
    } else {
        RecordRef::EffectCompleted {
            seq: EffectSeq(index),
            result: b"done",
        }
    }
}

/// What that record is called in the ledger.
const fn id(index: u32) -> RecordId {
    RecordId(index.wrapping_add(1))
}

/// The driver's error out of an append failure.
fn unwind(error: AppendError<FaultError>) -> FaultError {
    match error {
        AppendError::Storage(inner) => inner,
        AppendError::Encode(inner) => unreachable!("this fixture encodes: {inner}"),
        AppendError::WrongDevice => unreachable!("one device"),
        AppendError::NoRoom { needed, available } => {
            unreachable!("{needed} B does not fit {available} B")
        }
        AppendError::Interrupted => unreachable!("this writer stops at its first failure"),
    }
}

/// The real thing: design document §07's two barriers per record.
fn journal_writer(session: &mut Session) -> Result<(), FaultError> {
    let mut page = [0_u8; PAGE];
    let mut recovery = Recovery::new(region());
    while recovery.next(session, &mut page).is_some() {}
    let Some(mut journal) = Journal::after(recovery) else {
        unreachable!("an erased region ends cleanly at its first byte")
    };

    for index in 0..RECORDS {
        let mut staging = [0_u8; PAGE];
        let sealable = journal
            .stage(session, &record(index), &mut staging)
            .and_then(|staged| staged.payload_barrier(session))
            .map_err(unwind)?;
        session.begin_record(id(index));
        sealable.commit(session).map_err(unwind)?;
        session.end_record();
    }
    Ok(())
}

/// What a reader recovers from the media `run` left behind.
///
/// A scan that meets damage stops. The prefix before it is history, which is design document
/// §14's "frame ignored; previous history prefix wins".
fn recovered(run: &Run) -> Vec<RecordId> {
    let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
        unreachable!("the image came from a device of this geometry")
    };
    let mut page = [0_u8; PAGE];
    let mut reader = Recovery::new(region());
    let mut history = Vec::new();
    while let Some(step) = reader.next(&mut device, &mut page) {
        if step.is_err() {
            break;
        }
        history.push(id(u32::try_from(history.len()).unwrap_or(u32::MAX)));
    }
    history
}

#[test]
fn every_watchdog_image_is_one_a_power_cut_also_produces() {
    // The theorem the module documentation states. A watchdog reset finishes the unit in
    // flight, and a power cut is enumerated at every byte, so every watchdog image sits in
    // the power-cut set. Proved over the real journal writer, not argued.
    let runs = drive(journal_writer);
    let power: BTreeSet<Vec<u8>> = runs
        .iter()
        .filter(|run| {
            run.injection()
                .is_some_and(|point| point.interruption == Interruption::PowerLoss)
        })
        .map(|run| run.image().to_vec())
        .collect();
    let watchdog: BTreeSet<Vec<u8>> = runs
        .iter()
        .filter(|run| {
            run.injection()
                .is_some_and(|point| point.interruption == Interruption::Watchdog)
        })
        .map(|run| run.image().to_vec())
        .collect();

    assert!(
        !watchdog.is_empty(),
        "the sweep performed no watchdog reset"
    );
    assert!(
        watchdog.is_subset(&power),
        "a watchdog reset left media no power cut can leave"
    );
    // And it is a *proper* subset, so the two causes are not the same model wearing two
    // names. Without this the theorem above would also hold of a relabelling.
    assert!(
        watchdog.len() < power.len(),
        "the two causes produce the same images, so one of them is a label"
    );
}

#[test]
fn the_oracle_accepts_the_real_writer_at_every_watchdog_reset() {
    // Design document §15's core property, over the third cause. The engine has to survive a
    // watchdog reset as it survives a brownout, and this is the sweep that says so.
    let runs = drive(journal_writer);
    let mut swept = 0_usize;
    for run in &runs {
        let Some(point) = run.injection() else {
            continue;
        };
        if point.interruption != Interruption::Watchdog {
            continue;
        }
        swept += 1;
        if let Err(breach) = verify_recovery(run.ledger(), &recovered(run)) {
            unreachable!("{point:?} broke {breach}");
        }
    }
    assert!(swept > 0, "no watchdog reset was swept");
}

#[test]
fn a_watchdog_reset_leaves_no_partial_unit_in_the_journal_either() {
    // The targeted test at the top, restated over the real writer: at every watchdog reset,
    // the programmed prefix of the journal ends on a program-unit boundary.
    let runs = drive(journal_writer);
    for run in &runs {
        let Some(point) = run.injection() else {
            continue;
        };
        if point.interruption != Interruption::Watchdog {
            continue;
        }
        let landed = programmed_prefix(run);
        assert_eq!(
            u32::try_from(landed).unwrap_or(u32::MAX) % UNIT,
            0,
            "{point:?} left a partial program unit"
        );
    }
}

/// The ledger of the fault-free run, for the assertion below.
fn clean_ledger(runs: &[Run]) -> &Ledger {
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is first")
    };
    clean.ledger()
}

#[test]
fn a_clean_run_is_unchanged_by_the_third_cause() {
    // The regression guard. Adding a reset cause must not move the fault-free run, which is
    // where every crash point is enumerated from.
    let runs = drive(journal_writer);
    for index in 0..RECORDS {
        assert_eq!(
            clean_ledger(&runs).state(id(index)),
            Some(waymaker_fault::Durability::Acknowledged)
        );
    }
}
