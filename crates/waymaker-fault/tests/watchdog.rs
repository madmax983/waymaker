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
//! writer rather than asserting it. So the difference between the two causes is never on
//! media. It is in what the *call* answered — at every crash point, not only at
//! [`Progress::Whole`] — which is why a watchdog reset is enumerated at boundaries of its own
//! rather than folded into the brownout that left the same bytes. For a writer that
//! propagates, the difference then shows only where something other than another storage call
//! follows a *completed* operation, which in `waymaker-rig` is the dispatch and nowhere else.
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

/// Four 64-byte erase blocks, so an erase has an interior.
fn blocky() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 64, 4, 1) else {
        unreachable!("256 is four 64-byte blocks of 4-byte units")
    };
    geometry
}

/// Programs the whole part, then erases it, so an erase can be watched.
fn program_then_erase(session: &mut Session) -> Result<(), FaultError> {
    session.program(0, &[0x00; 256])?;
    session.barrier()?;
    session.erase(0, 256)?;
    session.barrier()
}

/// How many bytes at the start of `run`'s media are erased.
fn erased_prefix(run: &Run) -> usize {
    run.image()
        .iter()
        .take_while(|byte| **byte == ERASED)
        .count()
}

/// [`program_then_erase`] on a [`blocky`] part, interrupted at `injection`.
fn erase_run(progress: Progress, interruption: Interruption) -> Run {
    let injection = Injection {
        // Operation 2 is the erase: program, barrier, erase, barrier.
        op: 2,
        progress,
        interruption,
    };
    match Harness::new(blocky()).run_one(injection, program_then_erase) {
        Ok(run) => run,
        Err(error) => unreachable!("{error}"),
    }
}

#[test]
fn a_watchdog_reset_leaves_a_whole_number_of_erase_blocks() {
    // The erase half of the same rule. The controller finishes the block the core stopped
    // believing in, so a reset one byte into a block leaves the whole block erased.
    //
    // Reachable only through a crash point a caller builds by hand: an erase's reset
    // points are its block boundaries, so every enumerated watchdog point on an erase is
    // already block-aligned and `unit_completed` is the identity there. The round-up
    // branch runs only for a reset that lands inside a block, which takes a hand-built
    // `Progress::Bytes` — which is what this test does.
    for (stopped, erased) in [
        (1, 64),
        (63, 64),
        (64, 64),
        (65, 128),
        (191, 192),
        (192, 192),
    ] {
        let run = erase_run(Progress::Bytes(stopped), Interruption::Watchdog);
        assert_eq!(
            erased_prefix(&run),
            erased,
            "a watchdog reset {stopped} bytes into the erase left a partial block"
        );
    }
}

#[test]
fn a_brownout_leaves_the_erase_block_it_was_inside_unerased() {
    // The tooth. A power cut rounds the other way — no device erases half a block, and the
    // block in flight is the one that did not finish — so a model that rounded both causes up
    // would pass the test above and lose the difference it is about.
    for (stopped, erased) in [(1, 0), (63, 0), (64, 64), (65, 64), (191, 128), (192, 192)] {
        let run = erase_run(Progress::Bytes(stopped), Interruption::PowerLoss);
        assert_eq!(
            erased_prefix(&run),
            erased,
            "a power cut {stopped} bytes into the erase erased a block it was still inside"
        );
    }
}

#[test]
fn a_whole_erase_is_the_same_under_either_reset() {
    // Which is why the enumeration lists one of them. The two causes part company over what
    // the *call* answers, and `a_watchdog_reset_at_a_whole_operation_is_not_a_power_cut_at_one`
    // is where that is measured.
    let power = erase_run(Progress::Whole, Interruption::PowerLoss);
    let watchdog = erase_run(Progress::Whole, Interruption::Watchdog);
    assert_eq!(erased_prefix(&watchdog), 256);
    assert_eq!(power.image(), watchdog.image());
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
fn a_barrier_that_did_not_return_orders_a_record_without_acknowledging_it() {
    // The consequence of the difference above, and Codex's second review round. The barrier
    // completes on media, so the record really is durable — every byte of it is there, and a
    // forward scan will produce it. What did not happen is the promise: the call never
    // returned, so nothing was told to anyone.
    //
    // §15's guarantee is about the promise, so recovery is *permitted* to produce this record
    // and not *required* to. Requiring it would make the oracle reject a reader that stopped
    // one record short of a record nobody was ever told about, which is the direction an
    // instrument must not fail in.
    let watchdog = run_one(
        Injection {
            op: 1,
            progress: Progress::Whole,
            interruption: Interruption::Watchdog,
        },
        one_program,
    );
    assert_eq!(
        watchdog.ledger().state(RecordId(1)),
        Some(waymaker_fault::Durability::PossiblyDurable)
    );
    assert_eq!(watchdog.ledger().torn(RecordId(1)), Some(false));
    // Both answers are legal, which is what "permitted and not required" means.
    assert!(verify_recovery(watchdog.ledger(), &[RecordId(1)]).is_ok());
    assert!(verify_recovery(watchdog.ledger(), &[]).is_ok());

    // The tooth: a power cut at the same point *does* acknowledge, because the barrier
    // returned before the supply went. The two crash points leave identical media and
    // different obligations, and that is the whole of the difference.
    let power = run_one(
        Injection {
            op: 1,
            progress: Progress::Whole,
            interruption: Interruption::PowerLoss,
        },
        one_program,
    );
    assert_eq!(power.image(), watchdog.image());
    assert_eq!(
        power.ledger().state(RecordId(1)),
        Some(waymaker_fault::Durability::Acknowledged)
    );
    assert!(
        verify_recovery(power.ledger(), &[]).is_err(),
        "an acknowledged record recovery lost is a breach"
    );
}

// ---------------------------------------------------------------------------------------
// 3. The enumeration
// ---------------------------------------------------------------------------------------

#[test]
fn a_watchdog_reset_is_enumerated_at_unit_boundaries_and_nowhere_else() {
    // A reset before *each* operation, an interior point per unit, and a whole operation. A
    // point inside a unit would be the boundary above it, and an enumeration that counts one
    // crash point twice is no longer a count of anything.
    //
    // `None` is per operation rather than once, unlike the power-cut half, and that is Codex's
    // fourth-round finding: a power cut at `Whole` returns `Ok(())` so "before the next
    // operation" is the same world, while a watchdog reset at `Whole` returns an error and the
    // writer never reaches whatever lies between the two.
    let ops = [Op::Program { offset: 0, len: 8 }, Op::Barrier];
    let watchdog: Vec<_> = injections(&ops, geometry())
        .into_iter()
        .filter(|point| point.interruption == Interruption::Watchdog)
        .collect();

    assert_eq!(
        watchdog,
        vec![
            // The core resets before the sequence begins.
            Injection {
                op: 0,
                progress: Progress::None,
                interruption: Interruption::Watchdog,
            },
            // One unit landed and the core stopped. Eight bytes of 4-byte units has one
            // interior boundary.
            Injection {
                op: 0,
                progress: Progress::Bytes(UNIT),
                interruption: Interruption::Watchdog,
            },
            Injection {
                op: 0,
                progress: Progress::Whole,
                interruption: Interruption::Watchdog,
            },
            // A barrier has no interior and still has both: the core stopped before it ran,
            // and the core stopped after it changed media and before it returned. §02
            // decision 3 is about the second.
            Injection {
                op: 1,
                progress: Progress::None,
                interruption: Interruption::Watchdog,
            },
            Injection {
                op: 1,
                progress: Progress::Whole,
                interruption: Interruption::Watchdog,
            },
        ]
    );
}

#[test]
fn a_watchdog_reset_inside_a_unit_is_the_watchdog_reset_at_the_boundary_above_it() {
    // Why the interior of a unit is not enumerated, measured. The controller finishes the
    // unit, so the media are the same — and the cause is the same, so the caller is answered
    // the same. One crash point.
    let boundary = run_one(
        Injection {
            op: 0,
            progress: Progress::Bytes(UNIT),
            interruption: Interruption::Watchdog,
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
            "a watchdog reset {inside} bytes in is not the one at the unit above it"
        );
    }
}

#[test]
fn a_watchdog_reset_at_a_unit_boundary_is_not_the_power_cut_beside_it() {
    // Why an interior watchdog point is *not* folded into the brownout that left the same
    // bytes. Codex found this on the first review round, and it is a defect in an argument
    // rather than in the media: the two causes leave the same image and hand the writer
    // different errors, and this crate's writer is any `FnMut` over a `Session`. Nothing
    // obliges it to propagate. One that catches the error and does something which is not a
    // storage call — dispatch an effect, take a branch — behaves differently under the two,
    // so a sweep that listed only one of them would never run the other path.
    let seen = RefCell::new(Vec::new());
    let mut writer = |session: &mut Session| {
        // A writer that reacts rather than propagates, which is what makes the point.
        let result = session.program(0, PAYLOAD);
        seen.borrow_mut().push(result);
        Ok(())
    };
    for interruption in [Interruption::PowerLoss, Interruption::Watchdog] {
        drop(run_one(
            Injection {
                op: 0,
                progress: Progress::Bytes(UNIT),
                interruption,
            },
            &mut writer,
        ));
    }
    let seen = seen.into_inner();
    assert_eq!(
        seen.get(1),
        Some(&Err(FaultError::PowerLoss)),
        "a power cut at a unit boundary tells the writer the power went"
    );
    assert_eq!(
        seen.get(3),
        Some(&Err(FaultError::WatchdogReset)),
        "a watchdog reset at the same boundary tells the writer the core went"
    );
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
    // The inclusion is proper, and that is a remark rather than a check: a power cut is
    // enumerated at every interior byte and a watchdog reset only at whole operations, so the
    // inequality holds however the two causes behave. What rules out a relabelling is
    // `a_watchdog_reset_at_a_whole_operation_is_not_a_power_cut_at_one`, where the two answer
    // the caller differently, and the rig's
    // `the_two_causes_part_company_only_where_an_effect_follows_a_completed_call`, where
    // that difference changes what a run did.
    assert!(watchdog.len() < power.len());
}

#[test]
fn the_oracle_accepts_the_real_writer_at_every_watchdog_reset() {
    // Design document §15's core property, over the third cause. The engine has to survive a
    // watchdog reset as it survives a brownout, and this is the sweep that says so.
    //
    // What it is not is new oracle reach, and the theorem above is why: every watchdog image
    // is a power-cut image, so this cannot go red without a power-cut run in the same sweep
    // going red first. It confirms the writer under the new cause rather than reaching a
    // state the old sweep could not.
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
    //
    // What it does not do is catch a rounding regression. Every point it sees is a whole
    // operation, where the rounding is a no-op; the direction of the rounding is held by the
    // hand-built crash points above. It is here so that an enumeration which later broadens
    // `Interruption::Watchdog` is held to the rule from its first run.
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
        let landed = programmed_prefix(run);
        assert_eq!(
            u32::try_from(landed).unwrap_or(u32::MAX) % UNIT,
            0,
            "{point:?} left a partial program unit"
        );
    }
    assert!(swept > 0, "no watchdog reset was swept");
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
